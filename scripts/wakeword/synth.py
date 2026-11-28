"""Voices for wake-word training: Piper's multi-speaker models and Kokoro.

Every voice is driven by phonemes, not text, so the wake word is said the way
it is meant to be said whatever a voice's own reading of the spelling would
be (eSpeak reads "Jatayu" as "JAD-ay-oo"). The words around it -- "hey",
"what's the time" -- are phonemized by eSpeak through Piper and, for Kokoro,
mapped onto its symbol set.

Audio comes back as float32 at 16 kHz, peak-normalised to 0.9.
"""

from __future__ import annotations

import dataclasses
import json
import os
from functools import lru_cache
from pathlib import Path

import numpy as np
from scipy.signal import resample_poly

RATE = 16_000


@dataclasses.dataclass(frozen=True)
class Voice:
    """One speaker of one model."""

    backend: str  # "piper" or "kokoro"
    model: str  # file stem
    speaker: int
    # Kokoro only: a second speaker blended into the first, and how much.
    blend: int | None = None
    mix: float = 0.0

    @property
    def key(self) -> str:
        if self.blend is None:
            return f"{self.model}:{self.speaker}"
        return f"{self.model}:{self.speaker}+{self.blend}@{self.mix:.2f}"


# ---- Piper --------------------------------------------------------------------


@lru_cache(maxsize=None)
def _piper(model_dir: str, stem: str):
    import onnxruntime as ort
    from piper import PiperVoice

    path = os.path.join(model_dir, stem + ".onnx")
    voice = PiperVoice.load(path)
    # One thread: generation runs a process per core, and a session that
    # takes every core in each of them spends its time waiting on the others.
    options = ort.SessionOptions()
    options.intra_op_num_threads = 1
    options.inter_op_num_threads = 1
    voice.session = ort.InferenceSession(path, options, providers=["CPUExecutionProvider"])
    return voice


def piper_speakers(model_dir: str) -> list[Voice]:
    voices = []
    for config in sorted(Path(model_dir).glob("en_*-medium.onnx.json")):
        stem = config.name.removesuffix(".onnx.json")
        count = json.loads(config.read_text()).get("num_speakers", 1)
        voices += [Voice("piper", stem, s) for s in range(count)]
    return voices


def piper_phonemes(model_dir: str, stem: str, text: str) -> list[str]:
    """eSpeak's phonemes for `text`, as one flat list with spaces between
    sentences."""
    out: list[str] = []
    for sentence in _piper(model_dir, stem).phonemize(text):
        if out:
            out.append(" ")
        out += sentence
    return out


def piper_say(
    model_dir: str,
    voice: Voice,
    phonemes: list[str],
    length_scale: float,
    noise_scale: float,
    noise_w: float,
) -> np.ndarray:
    from piper import SynthesisConfig

    engine = _piper(model_dir, voice.model)
    known = engine.config.phoneme_id_map
    phonemes = [p for p in phonemes if p in known]
    ids = engine.phonemes_to_ids(phonemes)
    config = SynthesisConfig(
        speaker_id=voice.speaker,
        length_scale=length_scale,
        noise_scale=noise_scale,
        noise_w_scale=noise_w,
    )
    audio = engine.phoneme_ids_to_audio(ids, config)
    audio = np.asarray(audio, dtype=np.float32)
    if audio.dtype.kind == "i" or np.abs(audio).max() > 2.0:
        audio = audio / 32768.0
    return _normalise(resample_poly(audio, 320, 441).astype(np.float32))


# ---- Kokoro -------------------------------------------------------------------

KOKORO_ENGLISH = list(range(0, 28))  # af_*, am_*, bf_*, bm_*
KOKORO_HINDI = [31, 32, 33, 34]  # hf_alpha, hf_beta, hm_omega, hm_psi


@lru_cache(maxsize=None)
def _kokoro(kokoro_dir: str):
    import onnxruntime as ort

    options = ort.SessionOptions()
    options.intra_op_num_threads = 1
    options.inter_op_num_threads = 1
    session = ort.InferenceSession(
        os.path.join(kokoro_dir, "model.onnx"), options, providers=["CPUExecutionProvider"]
    )
    voices = np.fromfile(os.path.join(kokoro_dir, "voices.bin"), dtype=np.float32)
    voices = voices.reshape(-1, 510, 256)
    tokens = {}
    for line in Path(kokoro_dir, "tokens.txt").read_text(encoding="utf-8").splitlines():
        if not line:
            continue
        symbol, _, index = line.rpartition(" ")
        tokens[symbol if symbol else " "] = int(index)
    return session, voices, tokens


def kokoro_speakers() -> list[Voice]:
    return [Voice("kokoro", "kokoro", s) for s in KOKORO_ENGLISH + KOKORO_HINDI]


# eSpeak's IPA as Piper gives it, onto the symbols Kokoro v1.0 was trained on
# (misaki's): affricates and diphthongs are single letters there.
_ESPEAK_TO_MISAKI = [
    ("dʒ", "ʤ"),
    ("tʃ", "ʧ"),
    ("eɪ", "A"),
    ("oʊ", "O"),
    ("aɪ", "I"),
    ("aʊ", "W"),
    ("ɔɪ", "Y"),
    ("ɝ", "ɜɹ"),
    ("ɐ", "ə"),
    ("ɡ", "ɡ"),
    ("r", "ɹ"),
]


def to_kokoro(phonemes: list[str] | str) -> str:
    text = phonemes if isinstance(phonemes, str) else "".join(phonemes)
    for a, b in _ESPEAK_TO_MISAKI:
        text = text.replace(a, b)
    return text


def kokoro_say(kokoro_dir: str, voice: Voice, phonemes: str, speed: float) -> np.ndarray:
    session, voices, tokens = _kokoro(kokoro_dir)
    ids = [tokens[c] for c in phonemes if c in tokens][:510]
    style = voices[voice.speaker, len(ids)]
    if voice.blend is not None:
        style = (1 - voice.mix) * style + voice.mix * voices[voice.blend, len(ids)]
    audio = session.run(
        None,
        {
            "tokens": np.array([[0, *ids, 0]], dtype=np.int64),
            "style": style[None, :].astype(np.float32),
            "speed": np.array([speed], dtype=np.float32),
        },
    )[0]
    return _normalise(resample_poly(np.asarray(audio, np.float32), 2, 3).astype(np.float32))


def _normalise(audio: np.ndarray) -> np.ndarray:
    peak = float(np.abs(audio).max()) if audio.size else 0.0
    return audio * (0.9 / peak) if peak > 1e-6 else audio
