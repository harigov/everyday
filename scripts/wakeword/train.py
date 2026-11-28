"""Train the head that turns 16 speech embeddings into "the name, just now".

    python train.py --features FEATURES --out head.pt

Reads what `features.py` wrote (`clips-{train,test}.npz`, `long-{train,test}.npz`).
Every batch is a quarter windows where the name has just ended, a quarter
hard negatives (sound-alikes, other assistants' names, the name cut short),
and the rest synthetic and real speech. A tenth of the real training audio
is held back to choose the threshold: the lowest score that keeps false
wakes on it under `--max-false-per-hour`. Then everything is measured on
voices and recordings the head never saw: how many names it catches, how
often it wakes for nothing in hours of podcasts and audiobooks, and how
often for a sound-alike.
"""

from __future__ import annotations

import argparse
import json
from pathlib import Path

import numpy as np
import torch
import torch.nn as nn

WINDOW = 16
EMBEDDING = 96
STEP_SECONDS = 0.08
REFRACTORY = 20  # steps after a hit when nothing counts, as in the app


# The name with one sound changed -- "chuh-TAA-yoo", "guh-TAA-yoo" -- is how
# some people say it, not something else: never trained on as "not the name",
# and reported apart from the sound-alikes that are other words.
NEAR_NAME = ("/tʃʌtˈɑːjuː/", "/ɡʌtˈɑːjuː/", "/dʒʌkˈɑːjuː/", "/dʒʌtˈeɪjuː/", "dhataayu")


def near_name(meta: dict) -> bool:
    return any(n in meta.get("said", "") for n in NEAR_NAME)


class Head(nn.Module):
    def __init__(self, hidden: int = 128, dropout: float = 0.2):
        super().__init__()
        self.l1 = nn.Linear(WINDOW * EMBEDDING, hidden)
        self.n1 = nn.LayerNorm(hidden)
        self.l2 = nn.Linear(hidden, hidden)
        self.n2 = nn.LayerNorm(hidden)
        self.out = nn.Linear(hidden, 1)
        self.drop = nn.Dropout(dropout)

    def logits(self, x: torch.Tensor) -> torch.Tensor:
        x = self.drop(x.flatten(1))
        x = torch.relu(self.n1(self.l1(x)))
        x = self.drop(x)
        x = torch.relu(self.n2(self.l2(x)))
        return self.out(x).squeeze(-1)

    def forward(self, x: torch.Tensor) -> torch.Tensor:
        return torch.sigmoid(self.logits(x))

    # What export.py writes, in order: the app's `speech::spotter` reads it.
    SPEC = {
        "layers": [
            {"op": "dense", "weight": "l1.weight", "bias": "l1.bias"},
            {"op": "layernorm", "weight": "n1.weight", "bias": "n1.bias", "eps": 1e-5},
            {"op": "relu"},
            {"op": "dense", "weight": "l2.weight", "bias": "l2.bias"},
            {"op": "layernorm", "weight": "n2.weight", "bias": "n2.bias", "eps": 1e-5},
            {"op": "relu"},
            {"op": "dense", "weight": "out.weight", "bias": "out.bias"},
            {"op": "sigmoid"},
        ]
    }


def load_head(path: str) -> Head:
    saved = torch.load(path, map_location="cpu")
    head = Head(hidden=saved["meta"]["hidden"])
    head.load_state_dict(saved["state"])
    return head.eval()


class Split:
    """One features file: embeddings, labels and segments."""

    def __init__(self, path: Path):
        data = np.load(path)
        self.emb = torch.from_numpy(data["emb"].astype(np.float32))
        self.lab = data["lab"]
        self.start, self.length, self.kind = data["start"], data["length"], data["kind"]
        self.meta = [json.loads(line) for line in open(str(path) + ".jsonl")]

    def window_ends(self, label: int, kinds: tuple[int, ...] | None = None, segments=None, keep=None) -> np.ndarray:
        """Global step indices that end a judged window with `label`."""
        picked = []
        segs = range(len(self.start)) if segments is None else segments
        for s in segs:
            if kinds is not None and self.kind[s] not in kinds:
                continue
            if keep is not None and not keep(self.meta[s]):
                continue
            a, n = self.start[s], self.length[s]
            if n < WINDOW:
                continue
            ks = np.arange(WINDOW - 1, n)
            ks = ks[self.lab[a + ks] == label]
            picked.append(a + ks)
        return np.concatenate(picked) if picked else np.zeros(0, np.int64)

    def windows(self, ends: np.ndarray, device) -> torch.Tensor:
        idx = torch.from_numpy(ends)[:, None] + torch.arange(-WINDOW + 1, 1)[None, :]
        return self.emb[idx].to(device)


def concat(splits: list[Split]) -> Split:
    """Several features files read as one."""
    if len(splits) == 1:
        return splits[0]
    out = Split.__new__(Split)
    out.emb = torch.cat([s.emb for s in splits])
    out.lab = np.concatenate([s.lab for s in splits])
    offsets = np.cumsum([0] + [len(s.lab) for s in splits[:-1]])
    out.start = np.concatenate([s.start + o for s, o in zip(splits, offsets)])
    out.length = np.concatenate([s.length for s in splits])
    out.kind = np.concatenate([s.kind for s in splits])
    out.meta = [m for s in splits for m in s.meta]
    return out


@torch.no_grad()
def scores_of(head: Head, split: Split, segments, device, batch: int = 65536) -> list[np.ndarray]:
    """Every step's score, segment by segment (NaN before a full window)."""
    out = []
    for s in segments:
        a, n = int(split.start[s]), int(split.length[s])
        sc = np.full(n, np.nan, np.float32)
        if n >= WINDOW:
            ends = np.arange(a + WINDOW - 1, a + n)
            vals = []
            for i in range(0, len(ends), batch):
                vals.append(head(split.windows(ends[i : i + batch], device)).cpu().numpy())
            sc[WINDOW - 1 :] = np.concatenate(vals)
        out.append(sc)
    return out


def hits(scores: np.ndarray, labels: np.ndarray, threshold: float) -> list[int]:
    """Steps where the app would report the name: over the threshold, past
    the warm-up, outside the quiet after the last hit."""
    found, quiet = [], -1
    for k in np.flatnonzero(scores >= threshold):
        if labels[k] == -1 and k < 25:  # warm-up
            continue
        if k < quiet:
            continue
        found.append(int(k))
        quiet = k + REFRACTORY
    return found


def evaluate(head, clips: Split, long: Split | None, threshold: float, device, long_segments=None) -> dict:
    head.eval()
    report = {"threshold": threshold}
    pos = [s for s in range(len(clips.start)) if clips.kind[s] == 0]
    sc = scores_of(head, clips, pos, device)
    caught, early = 0, 0
    for s, score in zip(pos, sc):
        lab = clips.lab[clips.start[s] : clips.start[s] + clips.length[s]]
        k = hits(score, lab, threshold)
        ones = np.flatnonzero(lab == 1)
        if not len(ones):
            continue
        # Caught: a hit from just before the name ends -- its last sound
        # still dying away -- to 1.5 s after it. Early: before that, with the
        # name not yet said.
        end_step = ones[0]
        if any(end_step - 4 <= x <= end_step + 19 for x in k):
            caught += 1
        if any(x < end_step - 4 for x in k):
            early += 1
    report["recall"] = caught / max(1, len(pos))
    report["fired_before_the_name_ended"] = early / max(1, len(pos))
    groups = (
        ("adversarial", lambda s: clips.kind[s] == 1 and not near_name(clips.meta[s])),
        ("near_name", lambda s: clips.kind[s] == 1 and near_name(clips.meta[s])),
        ("tts_speech", lambda s: clips.kind[s] == 2),
    )
    for name, member in groups:
        segs = [s for s in range(len(clips.start)) if member(s)]
        sc = scores_of(head, clips, segs, device)
        fired = sum(
            bool(hits(score, clips.lab[clips.start[s] : clips.start[s] + clips.length[s]], threshold))
            for s, score in zip(segs, sc)
        )
        report[f"{name}_woke"] = fired / max(1, len(segs))
    if long is not None:
        segs = list(range(len(long.start))) if long_segments is None else long_segments
        sc = scores_of(head, long, segs, device)
        total, hours = 0, 0.0
        for s, score in zip(segs, sc):
            lab = long.lab[long.start[s] : long.start[s] + long.length[s]]
            total += len(hits(score, lab, threshold))
            hours += long.length[s] * STEP_SECONDS / 3600
        report["false_per_hour"] = total / max(hours, 1e-9)
        report["hours"] = hours
    return report


@torch.no_grad()
def mine(head: Head, split: Split, ends: np.ndarray, above: float, device, batch: int = 65536) -> np.ndarray:
    """The window ends among `ends` that `head` scores above `above`."""
    head.eval()
    keep = []
    for i in range(0, len(ends), batch):
        chunk = ends[i : i + batch]
        keep.append(chunk[head(split.windows(chunk, device)).cpu().numpy() > above])
    return np.concatenate(keep) if keep else np.zeros(0, np.int64)


def false_per_hour_at(head, long: Split, segments, thresholds, device) -> dict:
    sc = scores_of(head, long, segments, device)
    hours = sum(long.length[s] for s in segments) * STEP_SECONDS / 3600
    out = {}
    for t in thresholds:
        total = sum(
            len(hits(score, long.lab[long.start[s] : long.start[s] + long.length[s]], t))
            for s, score in zip(segments, sc)
        )
        out[float(t)] = total / hours
    return out


def main():
    p = argparse.ArgumentParser()
    p.add_argument("--features", required=True)
    p.add_argument("--out", required=True)
    p.add_argument("--steps", type=int, default=30000)
    p.add_argument("--batch", type=int, default=1024)
    p.add_argument("--hidden", type=int, default=128)
    p.add_argument("--lr", type=float, default=1e-3)
    p.add_argument("--max-false-per-hour", type=float, default=0.3)
    p.add_argument("--seed", type=int, default=0)
    p.add_argument("--mine", nargs="*", default=[], help="heads whose false wakes in the training data are trained on harder")
    p.add_argument("--mine-above", nargs="*", type=float, default=[], help="the score each head's false wakes start at")
    args = p.parse_args()
    torch.manual_seed(args.seed)
    rng = np.random.default_rng(args.seed)
    device = "cuda" if torch.cuda.is_available() else "cpu"
    f = Path(args.features)
    clips = Split(f / "clips-train.npz")
    long = concat([Split(p) for p in sorted(f.glob("long-*train.npz"))])
    test_clips = Split(f / "clips-test.npz")
    test_long = concat([Split(p) for p in sorted(f.glob("long-*test.npz"))])
    print(f"real audio: {len(long.lab) * STEP_SECONDS / 3600:.1f} h to train on, "
          f"{len(test_long.lab) * STEP_SECONDS / 3600:.1f} h to test on", flush=True)

    # A tenth of the real training audio, by segment, held back.
    segs = rng.permutation(len(long.start))
    held = sorted(segs[: len(segs) // 10].tolist())
    used = sorted(segs[len(segs) // 10 :].tolist())

    pools = {
        "pos": clips.window_ends(1, (0,)),
        # The name cut short, in the positive clips themselves.
        "partial": clips.window_ends(0, (0,)),
        "soundalike": clips.window_ends(0, (1,), keep=lambda m: not near_name(m)),
        "tts": clips.window_ends(0, (2,)),
        "real": long.window_ends(0, None, used),
    }
    share = {"pos": 0.25, "partial": 0.10, "soundalike": 0.15, "tts": 0.10, "real": 0.40}
    weight = {"pos": 1.0, "partial": 2.0, "soundalike": 3.0, "tts": 2.0, "real": 2.0}
    sources = {"pos": clips, "partial": clips, "soundalike": clips, "tts": clips, "real": long}
    if args.mine:
        # What an earlier head wrongly liked, in the training data alone:
        # real speech, synthetic speech and sound-alikes it scored highly.
        mined_real, mined_clips = [], []
        negatives = np.concatenate([pools["partial"], pools["soundalike"], pools["tts"]])
        for i, path in enumerate(args.mine):
            above = args.mine_above[i] if i < len(args.mine_above) else 0.3
            earlier = load_head(path).to(device)
            mined_real.append(mine(earlier, long, pools["real"], above, device))
            mined_clips.append(mine(earlier, clips, negatives, above, device))
        mined_real = np.unique(np.concatenate(mined_real))
        mined_clips = np.unique(np.concatenate(mined_clips))
        pools["mined_real"], pools["mined_clips"] = mined_real, mined_clips
        sources["mined_real"], sources["mined_clips"] = long, clips
        share = {"pos": 0.25, "partial": 0.08, "soundalike": 0.10, "tts": 0.07, "real": 0.30,
                 "mined_real": 0.12, "mined_clips": 0.08}
        weight.update(mined_real=3.0, mined_clips=3.0)
        print(f"mined {len(mined_real)} real and {len(mined_clips)} synthetic windows", flush=True)
    print({k: len(v) for k, v in pools.items()}, flush=True)

    head = Head(args.hidden).to(device)
    opt = torch.optim.AdamW(head.parameters(), lr=args.lr, weight_decay=1e-2)
    sched = torch.optim.lr_scheduler.OneCycleLR(opt, max_lr=args.lr, total_steps=args.steps, pct_start=0.05)
    for step in range(1, args.steps + 1):
        head.train()
        xs, ys, ws = [], [], []
        for name, frac in share.items():
            n = int(args.batch * frac)
            ends = pools[name][rng.integers(0, len(pools[name]), n)]
            x = sources[name].windows(ends, device)
            xs.append(x)
            ys.append(torch.full((n,), 1.0 if name == "pos" else 0.0, device=device))
            # A false wake costs more than a missed one: the room says the
            # name again, a television does not stop.
            ws.append(torch.full((n,), weight[name], device=device))
        x, y, w = torch.cat(xs), torch.cat(ys), torch.cat(ws)
        # A little noise on the embeddings: no two rooms are alike.
        x = x + torch.randn_like(x) * 0.05 * x.std()
        loss = (nn.functional.binary_cross_entropy_with_logits(head.logits(x), y, reduction="none") * w).mean()
        opt.zero_grad()
        loss.backward()
        opt.step()
        sched.step()
        if step % 2000 == 0 or step == args.steps:
            r = evaluate(head, test_clips, None, 0.5, device)
            print(f"step {step} loss {loss.item():.4f} test@0.5 {json.dumps(r)}", flush=True)

    head.eval()
    thresholds = np.round(np.arange(0.30, 0.995, 0.01), 3)
    fph = false_per_hour_at(head, long, held, thresholds, device)
    ok = [t for t in thresholds if fph[float(t)] <= args.max_false_per_hour]
    threshold = float(min(ok)) if ok else 0.99
    print(f"threshold {threshold} (held-back false wakes per hour: {fph[threshold]:.2f})", flush=True)
    report = evaluate(head, test_clips, test_long, threshold, device)
    sweep = {}
    for t in (0.5, 0.6, 0.7, 0.8, 0.9, threshold):
        sweep[t] = evaluate(head, test_clips, test_long, float(t), device)
    print("test:", json.dumps(report, indent=1), flush=True)
    for t, r in sorted(sweep.items()):
        print(
            f"  at {t:.2f}: recall {r['recall']:.3f}  early {r['fired_before_the_name_ended']:.3f}  "
            f"false/h {r['false_per_hour']:.2f}  sound-alikes {r['adversarial_woke']:.3f}  "
            f"near-name {r['near_name_woke']:.3f}  tts speech {r['tts_speech_woke']:.3f}",
            flush=True,
        )
    meta = {
        "hidden": args.hidden,
        "threshold": threshold,
        "test": report,
        "pools": {k: int(len(v)) for k, v in pools.items()},
        "steps": args.steps,
    }
    # Plain JSON values, so the file loads with `weights_only`.
    meta = json.loads(json.dumps(meta, default=float))
    torch.save({"state": head.state_dict(), "spec": Head.SPEC, "meta": meta}, args.out)
    print(f"saved {args.out}")


if __name__ == "__main__":
    main()
