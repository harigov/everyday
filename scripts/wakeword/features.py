"""Speech embeddings for wake-word training, computed exactly as the app does.

The front end is openWakeWord's: a mel spectrogram and Google's speech
embedding (Apache-2.0), frozen. What the app computes, chunk by chunk, is
defined here and nowhere else (`crates/jatayu-service/src/speech/spotter.rs`
copies it):

- Audio is 16 kHz, as int16 values held in float32 (not scaled to -1..1).
- Every step takes 1280 new samples (80 ms) with the 480 before them: 1760
  samples, eight mel frames of 32 bands, `x / 10 + 2`. Before the first step
  there are 480 zeros.
- The mel frames follow 76 rows of ones; each step embeds the newest 76 rows
  (one 96-value embedding per 80 ms).
- The head reads the newest 16 embeddings (1.28 s).

The mel model floors each call's decibels at its own maximum less 80 dB, a
maximum taken over the whole batch. The app makes one call per step, so here
the floor is taken per window, never across a batch.

    python features.py clips DATA/clips OUT --models MODELS --backgrounds ...
    python features.py long OUT --models MODELS FILES...
"""

from __future__ import annotations

import argparse
import glob
import json
import multiprocessing as mp
import os
import random
from pathlib import Path

import numpy as np
import torch
import torch.nn.functional as F

RATE = 16_000
STEP = 1280
CONTEXT = 480
MEL_FRAMES = 76
WINDOW = 16
# Steps at the start of a segment whose embeddings still read the ones the
# mel history starts with: never trained on.
WARMUP = MEL_FRAMES // 8 + WINDOW


class Frontend:
    """The mel spectrogram and the embedding, on the GPU, a batch at a time."""

    def __init__(self, models: str, device: str = "cuda"):
        import onnx
        import onnx2torch
        from onnx import numpy_helper

        # Plain float32: TF32 on an RTX card moved embeddings by 0.03.
        torch.backends.cuda.matmul.allow_tf32 = False
        torch.backends.cudnn.allow_tf32 = False
        mel = onnx.load(os.path.join(models, "melspectrogram.onnx"))
        weights = {i.name: numpy_helper.to_array(i) for i in mel.graph.initializer}
        t = lambda name: torch.from_numpy(weights[name].copy()).to(device)
        self.real = t("0.stft.conv_real.weight")
        self.imag = t("0.stft.conv_imag.weight")
        self.mel_w = t("1.melW")
        self.low = float(weights["onnx::Clip_39"])
        self.high = float(weights["onnx::Clip_40"])
        self.top = float(weights["onnx::Clip_41"])
        embedding = onnx.load(os.path.join(models, "embedding_model.onnx"))
        self.embed_net = onnx2torch.convert(embedding).to(device).eval()
        self.device = device

    @torch.no_grad()
    def mel(self, windows: torch.Tensor) -> torch.Tensor:
        """(N, 1760) -> (N, 8, 32), each window floored on its own."""
        x = windows[:, None, :]
        re = F.conv1d(x, self.real, stride=160)
        im = F.conv1d(x, self.imag, stride=160)
        power = (re * re + im * im).transpose(1, 2)
        mel = torch.clamp(power @ self.mel_w, self.low, self.high)
        db = 10.0 * torch.log(mel) / 2.3025851
        floor = db.amax(dim=(1, 2), keepdim=True) - 80.0
        db = torch.minimum(torch.maximum(db, floor), torch.tensor(self.top, device=db.device))
        return db / 10.0 + 2.0

    @torch.no_grad()
    def embed(self, clips: list[np.ndarray], batch: int = 8192, net_batch: int = 1024) -> list[np.ndarray]:
        """Each clip (int16) to its embeddings, one per whole step."""
        windows, counts = [], []
        for clip in clips:
            steps = len(clip) // STEP
            counts.append(steps)
            padded = np.concatenate([np.zeros(CONTEXT, np.float32), clip.astype(np.float32)])
            if steps:
                idx = np.arange(steps)[:, None] * STEP + np.arange(STEP + CONTEXT)[None, :]
                windows.append(padded[idx])
        if not windows:
            return [np.zeros((0, 96), np.float32) for _ in clips]
        all_windows = torch.from_numpy(np.concatenate(windows)).to(self.device)
        mels = torch.cat([self.mel(all_windows[i : i + batch]) for i in range(0, len(all_windows), batch)])
        # Each clip's history: 76 rows of ones, then its frames.
        stacks, at = [], 0
        ones = torch.ones((MEL_FRAMES, 32), device=self.device)
        for steps in counts:
            frames = mels[at : at + steps].reshape(-1, 32)
            at += steps
            history = torch.cat([ones, frames])
            if steps:
                idx = (torch.arange(steps, device=self.device)[:, None] + 1) * 8 + torch.arange(
                    MEL_FRAMES, device=self.device
                )[None, :]
                stacks.append(history[idx])
        stacked = torch.cat(stacks)[..., None]
        out = torch.cat(
            # A smaller batch through the network: its first layer alone is
            # 7 KB a window, and the card is shared with the running app.
            [self.embed_net(stacked[i : i + net_batch]).reshape(-1, 96) for i in range(0, len(stacked), net_batch)]
        ).cpu().numpy()
        result, at = [], 0
        for steps in counts:
            result.append(out[at : at + steps])
            at += steps
        return result


# ---- augmentation ---------------------------------------------------------------


def make_rirs(count: int, seed: int) -> list[np.ndarray]:
    """Room responses from simulated rooms: a desk in a study to a kitchen."""
    import pyroomacoustics as pra

    rng = np.random.default_rng(seed)
    rirs = []
    while len(rirs) < count:
        dims = rng.uniform([2.5, 2.5, 2.3], [9.0, 7.0, 4.0])
        absorption = rng.uniform(0.08, 0.6)
        room = pra.ShoeBox(dims, fs=RATE, materials=pra.Material(absorption), max_order=int(rng.integers(3, 18)))
        mic = rng.uniform([0.3, 0.3, 0.7], dims - [0.3, 0.3, 0.5])
        distance = rng.uniform(0.3, 3.5)
        direction = rng.normal(size=3)
        source = np.clip(mic + direction / np.linalg.norm(direction) * distance, 0.2, dims - 0.2)
        room.add_source(source)
        room.add_microphone(mic)
        room.compute_rir()
        rir = np.asarray(room.rir[0][0], np.float32)[: RATE]
        rirs.append(rir / (np.abs(rir).max() + 1e-9))
    return rirs


class Backgrounds:
    """Stretches of real audio to mix under a clip: speech, podcasts, noise."""

    def __init__(self, raw_files: list[str], flac_dirs: list[str], seed: int):
        self.rng = np.random.default_rng(seed)
        self.raws = [np.memmap(f, dtype=np.int16, mode="r") for f in raw_files]
        self.flacs = sorted(f for d in flac_dirs for f in glob.glob(os.path.join(d, "**", "*.flac"), recursive=True))

    def take(self, n: int) -> np.ndarray:
        roll = self.rng.random()
        if roll < 0.35 and self.raws:
            raw = self.raws[self.rng.integers(len(self.raws))]
            at = int(self.rng.integers(0, len(raw) - n - 1))
            return raw[at : at + n].astype(np.float32) / 32768.0
        if roll < 0.70 and self.flacs:
            import soundfile as sf

            parts, have = [], 0
            while have < n:
                audio, _ = sf.read(self.flacs[self.rng.integers(len(self.flacs))], dtype="float32")
                parts.append(audio)
                have += len(audio)
            joined = np.concatenate(parts)
            if self.rng.random() < 0.4:  # a second talker: babble
                other = np.roll(joined, int(self.rng.integers(len(joined))))
                joined = joined + 0.7 * other
            return joined[:n]
        return coloured_noise(n, self.rng)


def coloured_noise(n: int, rng) -> np.ndarray:
    """White to brown noise: fans, hum-free hiss, traffic."""
    exponent = rng.uniform(0.0, 2.0)
    spectrum = np.fft.rfft(rng.normal(size=n))
    f = np.arange(len(spectrum)) + 1.0
    noise = np.fft.irfft(spectrum / f ** (exponent / 2), n)
    return (noise / (np.std(noise) + 1e-9)).astype(np.float32) * 0.1


def rms_db(x: np.ndarray) -> float:
    return 10 * np.log10(np.mean(x.astype(np.float64) ** 2) + 1e-12)


def augment(clip: np.ndarray, meta: dict, rirs, backgrounds: Backgrounds, rng: random.Random):
    """A clip (int16) set in a room, with something going on behind it.
    Returns the audio (int16) and where the name ends in it, if it has one."""
    from scipy.signal import butter, fftconvolve, sosfilt

    speech = clip.astype(np.float32) / 32768.0
    if rirs and rng.random() < 0.6:
        rir = rirs[rng.randrange(len(rirs))]
        wet = fftconvolve(speech, rir)[: len(speech) + RATE // 4]
        speech = wet / (np.abs(wet).max() + 1e-9) * 0.9
    pre = int(rng.uniform(2.1, 3.0) * RATE)
    post = int(rng.uniform(0.6, 1.6) * RATE)
    total = pre + len(speech) + post
    audio = np.zeros(total, np.float32)
    audio[pre : pre + len(speech)] = speech
    level = rng.uniform(-42, -14)
    audio *= 10 ** ((level - rms_db(speech)) / 20)
    if rng.random() < 0.85:
        noise = backgrounds.take(total)
        snr = rng.uniform(0, 25)
        noise *= 10 ** ((level - snr - rms_db(noise)) / 20)
        audio += noise
    else:
        audio += np.random.default_rng(rng.getrandbits(32)).normal(size=total).astype(np.float32) * 10 ** (
            rng.uniform(-80, -60) / 20
        )
    if rng.random() < 0.5:  # the rumble filter in front of a webcam mic
        audio = sosfilt(butter(4, rng.uniform(60, 120), "highpass", fs=RATE, output="sos"), audio)
    if rng.random() < 0.3:  # a phone or a cheap mic
        audio = sosfilt(butter(4, rng.uniform(3400, 7000), "lowpass", fs=RATE, output="sos"), audio)
    audio = np.clip(audio, -1, 1)
    name_end = pre + meta["name_end"] if meta.get("kind") == "positive" else None
    return (audio * 32767).astype(np.int16), name_end


def labels(steps: int, name_end: int | None) -> np.ndarray:
    """Per step: 1 the name has just ended, 0 it has not been said (or not
    whole), -1 not judged -- warm-up, or the stretch after the name where
    firing late is neither right nor wrong."""
    out = np.zeros(steps, np.int8)
    out[:WARMUP] = -1
    if name_end is None:
        return out
    ends = (np.arange(steps) + 1) * STEP
    out[(ends >= name_end - 0.2 * RATE) & (ends < name_end)] = -1
    out[(ends >= name_end) & (ends <= name_end + 0.4 * RATE)] = 1
    out[(ends > name_end + 0.4 * RATE) & (ends <= name_end + 1.5 * RATE)] = -1
    out[:WARMUP] = -1
    return out


# ---- the two jobs -----------------------------------------------------------------

_WORKER = {}


def _init(rir_seed, raw_files, flac_dirs, seed):
    _WORKER["rirs"] = make_rirs(150, rir_seed)
    _WORKER["bg"] = Backgrounds(raw_files, flac_dirs, seed + os.getpid())


def _augment_job(job):
    clip, meta, copies, seed = job
    rng = random.Random(seed)
    return [augment(clip, meta, _WORKER["rirs"], _WORKER["bg"], rng) for _ in range(copies)], meta


KIND = {"positive": 0, "adversarial": 1, "speech": 2, "long": 3}


def save(path: Path, embeddings: list[np.ndarray], labs: list[np.ndarray], segs_meta: list[dict]):
    emb = np.concatenate(embeddings).astype(np.float16)
    lab = np.concatenate(labs)
    lengths = np.array([len(e) for e in embeddings], np.int64)
    starts = np.concatenate([[0], np.cumsum(lengths)[:-1]])
    kinds = np.array([KIND[m["kind"]] for m in segs_meta], np.int8)
    np.savez(path, emb=emb, lab=lab, start=starts, length=lengths, kind=kinds)
    with open(str(path) + ".jsonl", "w") as f:
        for m in segs_meta:
            f.write(json.dumps(m) + "\n")
    print(f"{path}: {len(embeddings)} segments, {len(emb)} steps, {int((lab == 1).sum())} positive", flush=True)


def clips_job(args):
    frontend = Frontend(args.models)
    copies = {"positive": args.positive_copies, "adversarial": 2, "speech": 1}
    for split in ("train", "test"):
        backgrounds = args.train_bg if split == "train" else args.test_bg
        raws = [f for f in backgrounds if f.endswith(".raw")]
        flacs = [f for f in backgrounds if not f.endswith(".raw")]
        jobs, rng = [], random.Random(7 if split == "train" else 8)
        for shard in sorted(Path(args.clips).glob("clips-*.npz")):
            data = np.load(shard)
            metas = [json.loads(line) for line in open(shard.with_suffix(".jsonl"))]
            for key, meta in zip(data.files, metas):
                if meta["split"] == split:
                    jobs.append((data[key], meta, copies[meta["kind"]], rng.getrandbits(32)))
        print(f"{split}: {len(jobs)} clips", flush=True)
        embeddings, labs, metas = [], [], []
        with mp.get_context("spawn").Pool(
            args.workers, _init, (11 if split == "train" else 12, raws, flacs, 3)
        ) as pool:
            batch = []
            for made, meta in pool.imap_unordered(_augment_job, jobs, chunksize=16):
                batch += [(audio, end, meta) for audio, end in made]
                if len(batch) >= 512:
                    flush(frontend, batch, embeddings, labs, metas)
                    batch = []
            flush(frontend, batch, embeddings, labs, metas)
        save(Path(args.out) / f"clips-{split}.npz", embeddings, labs, metas)


def flush(frontend, batch, embeddings, labs, metas):
    if not batch:
        return
    out = frontend.embed([b[0] for b in batch])
    for (audio, end, meta), emb in zip(batch, out):
        embeddings.append(emb)
        labs.append(labels(len(emb), end))
        metas.append(meta)


def long_job(args):
    """Real audio, all of it not the name: a segment per minute."""
    import soundfile as sf

    frontend = Frontend(args.models)
    minute = 60 * RATE
    for name, files in (("train", args.train_bg), ("test", args.test_bg)):
        embeddings, labs, metas = [], [], []
        pieces = []
        for f in files:
            if f.endswith(".raw"):
                pieces.append((f, np.memmap(f, dtype=np.int16, mode="r")))
            else:
                parts = [sf.read(x, dtype="int16")[0] for x in sorted(glob.glob(os.path.join(f, "**", "*.flac"), recursive=True))]
                pieces.append((f, np.concatenate(parts)))
        for source, audio in pieces:
            segments = [np.asarray(audio[i : i + minute]) for i in range(0, len(audio) - RATE, minute)]
            for i in range(0, len(segments), 64):
                chunk = segments[i : i + 64]
                for seg, emb in zip(chunk, frontend.embed(chunk)):
                    embeddings.append(emb)
                    labs.append(labels(len(emb), None))
                    metas.append({"kind": "long", "source": os.path.basename(source)})
        if embeddings:
            tag = f"{args.tag}-" if args.tag else ""
            save(Path(args.out) / f"long-{tag}{name}.npz", embeddings, labs, metas)


def main():
    p = argparse.ArgumentParser()
    p.add_argument("job", choices=["clips", "long", "check", "reference"])
    p.add_argument("--head")
    p.add_argument("--clips")
    p.add_argument("--out", required=True)
    p.add_argument("--models", required=True)
    p.add_argument("--train-bg", nargs="*", default=[])
    p.add_argument("--test-bg", nargs="*", default=[])
    p.add_argument("--positive-copies", type=int, default=3)
    p.add_argument("--workers", type=int, default=12)
    p.add_argument("--tag", default="", help="long: named long-TAG-{train,test}.npz")
    args = p.parse_args()
    Path(args.out).mkdir(parents=True, exist_ok=True)
    if args.job == "clips":
        clips_job(args)
    elif args.job == "long":
        long_job(args)
    elif args.job == "reference":
        reference(args)
    else:
        check(args)


def check(args):
    """The GPU front end against onnxruntime run one step at a time, as the
    app runs it."""
    import onnxruntime as ort

    mel = ort.InferenceSession(os.path.join(args.models, "melspectrogram.onnx"), providers=["CPUExecutionProvider"])
    emb = ort.InferenceSession(os.path.join(args.models, "embedding_model.onnx"), providers=["CPUExecutionProvider"])
    rng = np.random.default_rng(0)
    clip = (rng.normal(size=RATE * 3) * 3000).astype(np.int16)
    clip[RATE : RATE + 8000] //= 300  # a quiet stretch, where a shared floor would show
    ours = Frontend(args.models).embed([clip, clip[: RATE * 2]])[0]
    padded = np.concatenate([np.zeros(CONTEXT, np.float32), clip.astype(np.float32)])
    history = np.ones((MEL_FRAMES, 32), np.float32)
    theirs = []
    for k in range(len(clip) // STEP):
        frames = mel.run(None, {"input": padded[k * STEP : k * STEP + STEP + CONTEXT][None]})[0].squeeze() / 10 + 2
        history = np.concatenate([history, frames])
        theirs.append(emb.run(None, {"input_1": history[-MEL_FRAMES:][None, :, :, None]})[0].reshape(96))
    theirs = np.stack(theirs)
    print("steps", len(theirs), "max abs diff", float(np.abs(ours - theirs).max()), "scale", float(np.abs(theirs).max()))
    np.save(os.path.join(args.out, "check-clip.npy"), clip)
    np.save(os.path.join(args.out, "check-embeddings.npy"), theirs)


def reference_clip() -> np.ndarray:
    """The Rust tests' clip (`speech::spotter`), in whole numbers so both
    make the same samples."""
    seed, out = 12345, []
    for n in range(48_000):
        seed = (seed * 1_664_525 + 1_013_904_223) & 0xFFFFFFFF
        noise = ((seed >> 16) % 2001) - 1000
        v = ((n // 18) % 2 * 2 - 1) * 3000 + ((n // 6) % 2 * 2 - 1) * 1500 + noise
        if 16_000 <= n < 24_000:
            v = -((-v) // 100) if v < 0 else v // 100
        out.append(v)
    return np.array(out, np.int16)


def reference(args):
    """The fixtures the Rust tests compare with: each step's embedding of
    `reference_clip`, worked out one step at a time with onnxruntime as the
    app works it, and with `--head`, the head's score at every step that has
    16 embeddings behind it."""
    import onnxruntime as ort

    mel = ort.InferenceSession(os.path.join(args.models, "melspectrogram.onnx"), providers=["CPUExecutionProvider"])
    emb = ort.InferenceSession(os.path.join(args.models, "embedding_model.onnx"), providers=["CPUExecutionProvider"])
    clip = reference_clip()
    padded = np.concatenate([np.zeros(CONTEXT, np.float32), clip.astype(np.float32)])
    history = np.ones((MEL_FRAMES, 32), np.float32)
    steps = []
    for k in range(len(clip) // STEP):
        frames = mel.run(None, {"input": padded[k * STEP : k * STEP + STEP + CONTEXT][None]})[0].squeeze() / 10 + 2
        history = np.concatenate([history, frames])
        steps.append(emb.run(None, {"input_1": history[-MEL_FRAMES:][None, :, :, None]})[0].reshape(96))
    steps = np.stack(steps).astype("<f4")
    out = Path(args.out)
    steps.tofile(out / "embeddings.f32")
    print("embeddings", steps.shape)
    if args.head:
        from train import load_head

        head = load_head(args.head)
        windows = np.stack([steps[k - WINDOW + 1 : k + 1] for k in range(WINDOW - 1, len(steps))])
        with torch.no_grad():
            scores = head(torch.from_numpy(windows)).numpy().astype("<f4")
        scores.tofile(out / "scores.f32")
        print("scores", scores.shape, scores.min(), scores.max())


if __name__ == "__main__":
    main()
