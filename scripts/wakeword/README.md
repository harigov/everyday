# Training the wake word's own model

How `crates/jatayu-service/assets/wakeword/jatayu.jww` was made, and how to
make it again or for another name. The app side is `speech::spotter` and
`speech::wake::spawn_spotted`; the design and the numbers are in
`docs/plans/wake-word.md`, "Jatayu's own model".

Everything below ran on one desktop (20 cores, an 8 GB RTX 3070 shared with
the running app) in about three hours, most of it generating voices and
downloading. The data stays outside the repo (about 30 GB).

## Setting up

```sh
W=~/.cache/wakeword                       # anywhere with room
uv venv --python 3.12 $W/venv && source $W/venv/bin/activate
uv pip install torch --index-url https://download.pytorch.org/whl/cu128
uv pip install onnxruntime onnx onnx2torch numpy scipy soundfile piper-tts==1.7.0 \
    sherpa-onnx pyroomacoustics pyarrow tqdm
```

Models, into `$W/models`:

- openWakeWord v0.5.1's `melspectrogram.onnx` and `embedding_model.onnx`
  (GitHub release assets).
- Piper's multi-speaker English voices, `.onnx` and `.onnx.json`:
  `en_US-libritts_r-medium`, `en_GB-vctk-medium`, `en_US-arctic-medium`,
  `en_US-l2arctic-medium` (Hugging Face `rhasspy/piper-voices`).
- Kokoro is the app's own download (`~/.local/share/jatayu/models/kokoro`).

Real speech, as what is not the name: LibriSpeech dev and test from
openslr.org (slow), LibriSpeech train-clean-100 and People's Speech `dirty`
shards from Hugging Face's parquet conversions (`parquet_audio.py` turns a
shard into raw 16 kHz), and any podcasts, decoded with
`ffmpeg -ac 1 -ar 16000 -f s16le`. Keep some of each apart to test with.

## Making it

```sh
cd scripts/wakeword
# 1. Clips: ~29k from ~1,240 voices, in about 40 minutes on 16 processes.
python gen.py --out $W/clips --piper $W/models --kokoro ~/.local/share/jatayu/models/kokoro \
    --text $W/LibriSpeech/dev-clean
# 2. Features, exactly as the app computes them.
python features.py check --out $W/feat --models $W/models        # GPU against onnxruntime
python features.py clips --clips $W/clips --out $W/feat --models $W/models \
    --train-bg <train backgrounds...> --test-bg <test backgrounds...>
python features.py long --out $W/feat --models $W/models --train-bg ... --test-bg ...
python features.py long --tag more --out $W/feat --models $W/models --train-bg ... --test-bg ...
# 3. Heads: a first one, then twice more on what the earlier ones got wrong.
python train.py --features $W/feat --out $W/head-a.pt --steps 50000
python train.py --features $W/feat --out $W/head-b.pt --steps 50000 --mine $W/head-a.pt
python train.py --features $W/feat --out $W/head-c.pt --steps 60000 \
    --mine $W/head-a.pt $W/head-b.pt --mine-above 0.3 0.1
# 4. Into the app, with the fixtures its tests compare against.
python export.py --models $W/models --head $W/head-c.pt --threshold 0.9 \
    --out ../../crates/jatayu-service/assets/wakeword/jatayu.jww
python features.py reference --models $W/models --head $W/head-c.pt \
    --out ../../crates/jatayu-service/tests/fixtures/wakeword
```

`train.py` prints, for held-out voices and recordings, how many names were
caught and how many false wakes an hour at a range of thresholds; pick one
and pass it to `export.py`.

## Checking it in the app

```sh
python eval_wavs.py --threshold 0.9 --clips $W/clips --models $W/models --head $W/head-c.pt \
    --test-bg <test backgrounds...> --out $W/evalwavs
JATAYU_TEST_WAKEWORD_DIR=$W/evalwavs cargo test --release -p jatayu-service --lib \
    speech::spotter::tests::the_name_is_heard_in_recordings -- --ignored --nocapture

python eval_wavs.py --clean --count 120 --threshold 0.9 --clips $W/clips --models $W/models \
    --head $W/head-c.pt --test-bg <any> --out $W/cleanwavs
JATAYU_TEST_SPEECH=1 JATAYU_TEST_WAKEWORD_DIR=$W/cleanwavs XDG_DATA_HOME=<scratch with models> \
    cargo test --release -p jatayu-service --features speech --test wake_spotted_real \
    -- --ignored --nocapture
```

The first says whether the app hears what training's code heard (it should
agree on every recording); the second runs the whole wake word, detector and
recogniser included.

## Another name

`gen.py`'s `NAME` (IPA readings, since eSpeak guesses names wrong),
`SOUNDALIKE_TEXT`/`SOUNDALIKE_IPA`, and `export.py --name`. Check the
readings first by synthesising a few and having a recogniser write them
down. Leave the name with one sound changed out of the sound-alikes: that is
how people say it.

## Licences

See THIRD-PARTY-NOTICES entry 69. For a build released widely, drop
`en_US-l2arctic-medium` (trained on CC BY-NC data) and any podcasts from the
inputs and train again.
