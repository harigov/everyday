"""Recordings for the app's own test of the built-in model
(`speech::spotter`'s `the_name_is_heard_in_recordings`): held-out voices
saying the name, sound-alikes and other speech, set in rooms with the test
backgrounds, each with what training's code made of it.

    python eval_wavs.py --clips DATA/clips --models MODELS --head head.pt \\
        --test-bg ... --out DIR
"""

from __future__ import annotations

import argparse
import json
import random
import wave
from pathlib import Path

import numpy as np
import torch

import features
from train import near_name, load_head, hits


def quiet_room(clip: np.ndarray, meta: dict, rng: random.Random):
    """The clip at an ordinary level, a second and a half of near silence
    before it and two after, so that a detector closes the utterance."""
    pre, post = int(1.5 * 16_000), 2 * 16_000
    speech = clip.astype(np.float32) / 32768.0
    speech *= 10 ** ((-24 - features.rms_db(speech)) / 20)
    audio = np.concatenate([np.zeros(pre, np.float32), speech, np.zeros(post, np.float32)])
    audio += np.random.default_rng(rng.getrandbits(32)).normal(size=len(audio)).astype(np.float32) * 10 ** (-70 / 20)
    return (np.clip(audio, -1, 1) * 32767).astype(np.int16), pre + meta["name_end"]


def main():
    p = argparse.ArgumentParser()
    p.add_argument("--clips", required=True)
    p.add_argument("--models", required=True)
    p.add_argument("--head", required=True)
    p.add_argument("--threshold", type=float)
    p.add_argument("--test-bg", nargs="+", required=True)
    p.add_argument("--out", required=True)
    p.add_argument("--count", type=int, default=600)
    p.add_argument(
        "--clean",
        action="store_true",
        help="held-out names with and without a request, in a quiet room: for the wake word's own test",
    )
    args = p.parse_args()
    out = Path(args.out)
    out.mkdir(parents=True, exist_ok=True)
    rng = random.Random(5)
    saved = torch.load(args.head, map_location="cpu")
    threshold = args.threshold if args.threshold is not None else saved["meta"]["threshold"]
    head = load_head(args.head)

    chosen = {"positive": [], "adversarial": [], "speech": []}
    for shard in sorted(Path(args.clips).glob("clips-*.npz")):
        data = np.load(shard)
        metas = [json.loads(line) for line in open(shard.with_suffix(".jsonl"))]
        for key, meta in zip(data.files, metas):
            if meta["split"] == "test" and not near_name(meta):
                chosen[meta["kind"]].append((data[key], meta))
    share = {"positive": 0.5, "adversarial": 0.33, "speech": 0.17}
    if args.clean:
        share = {"positive": 1.0, "adversarial": 0.0, "speech": 0.0}
    picked = []
    for kind, items in chosen.items():
        rng.shuffle(items)
        picked += items[: int(args.count * share[kind])]

    raws = [f for f in args.test_bg if f.endswith(".raw")]
    flacs = [f for f in args.test_bg if not f.endswith(".raw")]
    rirs = features.make_rirs(60, 99)
    backgrounds = features.Backgrounds(raws, flacs, 99)
    frontend = features.Frontend(args.models)
    with open(out / "manifest.jsonl", "w") as manifest:
        for n, (clip, meta) in enumerate(picked):
            if args.clean:
                audio, name_end = quiet_room(clip, meta, rng)
            else:
                audio, name_end = features.augment(clip, meta, rirs, backgrounds, rng)
            emb = frontend.embed([audio])[0]
            windows = np.stack([emb[k - 15 : k + 1] for k in range(15, len(emb))])
            with torch.no_grad():
                scores = np.full(len(emb), np.nan, np.float32)
                scores[15:] = head(torch.from_numpy(windows)).numpy()
            labels = features.labels(len(emb), name_end)
            fired = hits(scores, labels, threshold)
            name = f"{n:04d}-{meta['kind']}.wav"
            with wave.open(str(out / name), "wb") as w:
                w.setnchannels(1)
                w.setsampwidth(2)
                w.setframerate(16_000)
                w.writeframes(audio.astype("<i2").tobytes())
            judged = scores[features.WARMUP :]
            manifest.write(
                json.dumps(
                    {
                        "file": name,
                        "kind": meta["kind"],
                        "said": meta.get("said", meta.get("reading")),
                        "name_end": name_end,
                        "peak": float(np.nanmax(judged)) if len(judged) else 0.0,
                        "fired": bool(fired),
                        "request": meta.get("request"),
                    }
                )
                + "\n"
            )
    print(f"{len(picked)} recordings in {out}, threshold {threshold}")


if __name__ == "__main__":
    main()
