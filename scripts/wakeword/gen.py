"""Generate the spoken clips a wake-word head is trained and tested on.

    python gen.py --out DATA --piper MODELS --kokoro KOKORO --text LIBRISPEECH

Three kinds of clip, each from many synthetic voices, every voice in either
the training or the test split and never both (`split`):

- `positive`: the name, alone or after "hey"/"okay"/..., sometimes followed
  by a request. `name_end` is where the name ends, in samples: the request is
  synthesised on its own and joined after a short pause, so the end is known
  exactly rather than guessed.
- `adversarial`: what sounds like part of the name or like it ("Jacuzzi",
  "got to you", "juh-TAA" cut short), and other assistants' names after
  "hey", so neither a fragment nor "hey" alone wakes it.
- `speech`: ordinary sentences in the same voices, so the head cannot learn
  that a synthetic voice is the wake word.

Clips go to shards of `.npz` (int16 at 16 kHz) with a `.jsonl` beside each.
"""

from __future__ import annotations

import argparse
import hashlib
import json
import multiprocessing as mp
import os
import random
from pathlib import Path

import numpy as np

import synth
from synth import RATE, Voice

# The name, as IPA, and how often each reading is chosen. eSpeak's own reading
# of the spelling is wrong, so every voice is handed these.
NAME = {
    "juh-TAA-yoo": ("dʒʌtˈɑːjuː", 0.30),
    "juh-TAH-yoo": ("dʒətˈɑːjuː", 0.25),
    "jaa-TAA-yoo": ("dʒɑːtˈɑːjuː", 0.20),
    "JUH-taa-yoo": ("dʒˈʌtɑːjuː", 0.25),
}
# Retroflex, as a Hindi speaker says it: Kokoro's Hindi voices only, since
# Piper's English voices turn the retroflex t into "Jack, ah, you".
NAME_INDIAN = {"retroflex": ("dʒʌʈˈɑːju", 1.0)}

# The longest a name may run, in seconds, to fit the head's 1.28 s window with
# a little of what came before.
MAX_NAME = 1.15

PREFIXES = [("", 0.45), ("hey", 0.30), ("okay", 0.10), ("hi", 0.05), ("um", 0.05), ("so", 0.05)]

REQUESTS = [
    "what's the time",
    "what's on my calendar today",
    "type I'm running a few minutes late",
    "set a timer for ten minutes",
    "remind me to call mom tonight",
    "read my new mail",
    "what's the weather like",
    "add milk to my shopping list",
    "take a note",
    "play the next episode",
    "how long is my commute",
    "stop",
    "never mind",
    "open my notes",
    "what did I miss",
]

# Close to the name, or to a piece of it. As text where eSpeak's reading is
# the point, as IPA where the point is a fragment of the name itself.
SOUNDALIKE_TEXT = [
    "Jacuzzi", "got to you", "the tire", "Gatorade", "jet ride you", "Jamal",
    "chateau", "satay for you", "Jayden", "data", "today", "tattoo", "Jatin",
    "Jaya", "Jumanji", "Jupiter", "Jeeves", "Jarvis", "attire", "Jerry",
    "jaguar", "a tire for you", "what are you", "that you", "gotcha",
    "Tata", "ta-da", "chutney", "jet lag", "just a moment", "Jason",
    "Java", "a toy for you", "Gotta go", "I'll get to you", "Utah",
    "Satyu", "Jacob", "jalapeno", "Jodhpur", "Jaipur",
]
SOUNDALIKE_IPA = [
    "dʒʌtˈɑː",  # the first two syllables
    "tˈɑːjuː",  # the last two
    "dʒʌjˈuː",  # the middle gone
    "dʒʌtˈɑːlə",
    "dʒʌtˈæ",
    "dʒətˈuː",
]
# Not here: the name with one sound changed ("chuh-TAA-yoo", "guh-TAA-yoo",
# "juh-KAA-yoo", "juh-TAY-yoo", "dhataayu"). That is how some people say it,
# and training it as not the name taught the head to doubt the name itself;
# train.py leaves those out of clips made before this list changed.
OTHER_WAKE = ["Siri", "Jimmy", "Alexa", "Google", "Jarvis", "computer", "John", "Jake", "Jade", "Tara"]


def split(voice: Voice, test_share: float) -> str:
    """`train` or `test`, fixed for a voice."""
    if voice.backend == "kokoro":
        return "test" if voice.speaker in (5, 13, 22, 26, 32) and voice.blend is None else "train"
    digest = hashlib.sha1(voice.key.encode()).digest()
    return "test" if digest[0] / 255 < test_share else "train"


def pick(rng: random.Random, weighted):
    items = list(weighted.items()) if isinstance(weighted, dict) else weighted
    names = [i[0] for i in items]
    weights = [i[1][1] if isinstance(i[1], tuple) else i[1] for i in items]
    return rng.choices(names, weights)[0]


def trim(audio: np.ndarray, floor: float = 0.02) -> np.ndarray:
    """Without the quiet either side."""
    loud = np.flatnonzero(np.abs(audio) > floor)
    if loud.size == 0:
        return audio[:0]
    return audio[max(0, loud[0] - 160) : loud[-1] + 160]


class Speaker:
    """Says things in one voice, with that call's speed and variation."""

    def __init__(self, args, voice: Voice, rng: random.Random):
        self.args, self.voice, self.rng = args, voice, rng

    def phonemes(self, text: str) -> list[str]:
        model = self.voice.model if self.voice.backend == "piper" else "en_US-libritts_r-medium"
        return synth.piper_phonemes(self.args.piper, model, text)

    def say(self, phonemes: list[str], pace: float) -> np.ndarray:
        if self.voice.backend == "piper":
            r = self.rng
            return synth.piper_say(
                self.args.piper, self.voice, phonemes, pace, r.uniform(0.45, 0.9), r.uniform(0.5, 1.0)
            )
        return synth.kokoro_say(self.args.kokoro, self.voice, synth.to_kokoro(phonemes), 1.0 / pace)

    def pace(self) -> float:
        # LibriTTS's voices are slow already: the bare name runs a second at
        # the median there against 0.76 s elsewhere.
        if self.voice.model.startswith("en_US-libritts"):
            return self.rng.uniform(0.75, 1.1)
        return self.rng.uniform(0.8, 1.3)


def positive(s: Speaker, rng: random.Random):
    readings = dict(NAME)
    if s.voice.backend == "kokoro" and s.voice.speaker in synth.KOKORO_HINDI:
        readings = {**NAME, **{k: (v[0], 0.3) for k, v in NAME_INDIAN.items()}}
    reading = pick(rng, readings)
    prefix = pick(rng, PREFIXES)
    pace = s.pace()
    head = (s.phonemes(prefix) + [" "] if prefix else []) + list(readings[reading][0])
    named = trim(s.say(head + ["."] if rng.random() < 0.5 else head, pace))
    # Where the name starts: after the prefix said on its own at this pace,
    # which is near enough for a window of 1.28 s to be judged by.
    start = len(trim(s.say(s.phonemes(prefix), pace))) if prefix else 0
    start = min(start, max(0, len(named) - RATE // 3))
    if len(named) - start > MAX_NAME * RATE:
        raise ValueError(f"the name ran {(len(named) - start) / RATE:.2f} s")
    meta = {
        "kind": "positive",
        "reading": reading,
        "prefix": prefix,
        "name_start": int(start),
        "name_end": int(len(named)),
    }
    if rng.random() < 0.5:
        request = rng.choice(REQUESTS if rng.random() < 0.7 else s.args.sentences)
        tail = trim(s.say(s.phonemes(request), pace))
        gap = np.zeros(int(rng.uniform(0.05, 0.45) * RATE), np.float32)
        meta["request"] = request
        return np.concatenate([named, gap, tail]), meta
    return named, meta


def adversarial(s: Speaker, rng: random.Random):
    roll = rng.random()
    if roll < 0.45:
        text = rng.choice(SOUNDALIKE_TEXT)
        if rng.random() < 0.3:
            text = f"{rng.choice(['hey', 'okay', 'so'])} {text}"
        phonemes, what = s.phonemes(text), text
    elif roll < 0.75:
        ipa = rng.choice(SOUNDALIKE_IPA)
        lead = rng.choice(["", "", "hey", "the"])
        phonemes = (s.phonemes(lead) + [" "] if lead else []) + list(ipa)
        what = f"{lead} /{ipa}/".strip()
    else:
        text = f"{rng.choice(['hey', 'okay', 'hi'])} {rng.choice(OTHER_WAKE)}"
        if rng.random() < 0.5:
            text += ", " + rng.choice(REQUESTS)
        phonemes, what = s.phonemes(text), text
    return trim(s.say(phonemes, s.pace())), {"kind": "adversarial", "said": what}


def speech(s: Speaker, rng: random.Random):
    text = rng.choice(s.args.sentences)
    return trim(s.say(s.phonemes(text), s.pace())), {"kind": "speech", "said": text}


MAKERS = {"positive": positive, "adversarial": adversarial, "speech": speech}


def work(job):
    args, voice, plan, seed = job
    rng = random.Random(seed)
    s = Speaker(args, voice, rng)
    out, skipped = [], {}
    for kind, count in plan.items():
        for _ in range(count):
            try:
                audio, meta = MAKERS[kind](s, rng)
            except Exception as e:  # one bad voice or phrase is not the run's end
                skipped[type(e).__name__] = skipped.get(type(e).__name__, 0) + 1
                continue
            if len(audio) < RATE // 10:
                continue
            meta.update(voice=voice.key, split=split(voice, args.test_share))
            out.append(((np.clip(audio, -1, 1) * 32767).astype(np.int16), meta))
    if skipped:
        print(f"{voice.key}: skipped {skipped}", flush=True)
    return out


def sentences(librispeech: Path, limit: int, rng: random.Random) -> list[str]:
    """Short stretches of LibriSpeech's own transcripts, lower-cased."""
    lines = []
    for path in sorted(librispeech.rglob("*.trans.txt")):
        for line in path.read_text().splitlines():
            words = line.split(" ", 1)[1].lower().split()
            if len(words) >= 3:
                n = rng.randint(3, min(12, len(words)))
                at = rng.randint(0, len(words) - n)
                lines.append(" ".join(words[at : at + n]))
    rng.shuffle(lines)
    return lines[:limit]


def main():
    p = argparse.ArgumentParser()
    p.add_argument("--out", required=True)
    p.add_argument("--piper", required=True)
    p.add_argument("--kokoro", required=True)
    p.add_argument("--text", required=True, help="a LibriSpeech folder, for sentences")
    p.add_argument("--test-share", type=float, default=0.12)
    p.add_argument("--blends", type=int, default=150)
    p.add_argument("--workers", type=int, default=max(1, os.cpu_count() - 4))
    p.add_argument("--scale", type=float, default=1.0, help="multiplies every count")
    args = p.parse_args()
    rng = random.Random(1)
    args.sentences = sentences(Path(args.text), 20000, rng)

    voices = synth.piper_speakers(args.piper) + synth.kokoro_speakers()
    train_kokoro = [v.speaker for v in synth.kokoro_speakers() if split(v, args.test_share) == "train"]
    for _ in range(args.blends):
        a, b = rng.sample(train_kokoro, 2)
        voices.append(Voice("kokoro", "kokoro", a, b, round(rng.uniform(0.2, 0.8), 2)))

    def plan_for(v: Voice):
        k = args.scale
        if v.backend == "kokoro" and v.blend is None:
            base = {"positive": 40, "adversarial": 24, "speech": 16}
        elif v.backend == "kokoro":
            base = {"positive": 10, "adversarial": 6, "speech": 4}
        else:
            base = {"positive": 12, "adversarial": 7, "speech": 5}
        return {kind: max(1, round(n * k)) for kind, n in base.items()}

    jobs = [(args, v, plan_for(v), rng.getrandbits(32)) for v in voices]
    rng.shuffle(jobs)
    out = Path(args.out)
    out.mkdir(parents=True, exist_ok=True)
    print(f"{len(voices)} voices, {sum(sum(j[2].values()) for j in jobs)} clips", flush=True)

    shard, buffer, done = 0, [], 0
    with mp.get_context("spawn").Pool(args.workers) as pool:
        for clips in pool.imap_unordered(work, jobs, chunksize=2):
            buffer += clips
            done += 1
            if len(buffer) >= 2000 or done == len(jobs):
                np.savez(out / f"clips-{shard:03d}.npz", *[c[0] for c in buffer])
                with open(out / f"clips-{shard:03d}.jsonl", "w") as f:
                    for _, meta in buffer:
                        f.write(json.dumps(meta) + "\n")
                print(f"shard {shard}: {len(buffer)} clips, {done}/{len(jobs)} voices", flush=True)
                shard, buffer = shard + 1, []


if __name__ == "__main__":
    main()
