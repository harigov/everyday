# Hearing only the person: other voices and noise out of the microphone

Hari (Nov 24 2026): speech recognition gets confused by background noise,
people talking in the room; filter it so only his voice is heard. Before
this, every app microphone was cleaned by `sonora` (WebRTC's processing):
a high-pass filter, steady-noise suppression and, in voice mode, echo
cancellation (`karmayoga-app` `audio/echo.rs`). None of that tells one voice
from another, and Silero and the voicing check after it only ask whether a
sound is a voice, not whose.

Three things, each a switch in Settings → AI features → On this computer →
Microphone, each this computer's (`microphone.json`, the app's
`microphone.rs`), all off until chosen. They apply where the microphone
listens for the person: voice mode and the talk key, the wake word, voice
typing and dictation. A meeting is recorded as it was.

## 1. Only my voice: the owner's voice check

`karmayoga_service::speech::owner`. Every utterance the detector cuts from
this computer's microphone is embedded with the speech kit's speaker model
(eres2net, already used to tell meeting voices apart) and compared with the
owner's voiceprint before a recogniser hears it. Wrapped round the
transcriber in `Service::live_transcription` when the source is
`HeardFrom::ThisComputer`; a call through Matrix (`HeardFrom::Call`) is the
person's phone and never checked. The app tells the service the setting
(`Service::set_voice_check`) at start-up and on change.

The decision (`owner::judge`), by the whole utterance's similarity:

- too short to embed (about a second), or plainly the owner (`sure`): heard;
- holds the owner (`utterance`): sliced, 1.5 s every 0.5 s, and whatever
  comes before the first stretch of them and after the last is cut, by the
  plain slice bar so their own edges stay -- never anything between, so no
  word of theirs is cut from the middle;
- not the owner: sliced, and only their stretches heard, joined by a short
  silence; none, nothing heard.

A slice counts only with a neighbour that agrees (`MIN_RUN`), over a bar
(`window`), and, when the whole is not theirs, 0.06 clear of the whole
utterance's score (`ABOVE_WHOLE`). The words are worked out while the voice
is checked, and thrown away when it is not the owner's, so the check adds no
wait to an answer. The setting is read for every utterance: switching it, or
its strictness, needs no microphone opened again.

| strictness | sure | utterance | slice |
|---|---|---|---|
| Lenient | 0.35 | 0.20 | 0.17 |
| Balanced | 0.45 | 0.28 | 0.24 |
| Strict | 0.50 | 0.36 | 0.30 |

Measured on Hari's own 97 s recording (enrolled from its first 20 s, tested
on the rest) and four podcasts:

- His utterances 0.32 to 0.57 from his voiceprint; podcasts -0.07 to 0.24;
  one long utterance of him over a podcast, 0.33.
- 1.5 s slices: 90% of his at least 0.22, 95% of the podcasts' at most 0.21.
- Through the detector, Parakeet and the check itself: on Balanced, 127 of
  his 127 words heard, and 1 of about 2,260 from three minutes of each
  podcast alone. Lenient let 169 words of the podcast whose voice is
  nearest his through; Strict lost up to 10 of his.
- A lone "Jimmy" is too short to judge and is always heard.

The real-model test is `tests/mic_cleanup_real.rs` (two Phonon voices).

## 2. Remove other voices: VoxLock

`karmayoga_service::speech::cleanup`, run by the app's audio engine. VoxLock
(arbazsiddiqui/voxlock, Apache-2.0, 27M causal band-split RNN, int8 from
sherpa-onnx's `speech-enhancement-models` release, 72.5 MB) is the only
thing here that takes out somebody talking *over* the person: once a
detector has cut an utterance, two voices in it are one sound. It needs a
sample of the person's voice, so recording a voiceprint now keeps the most
voiced 12 s of the recording too (`Voiceprint::voice_sample`, base64 PCM,
sealed with the voiceprint and gone with it; `owner::best_stretch`).

Measured, Hari's recording with podcast talk mixed under it, through the
detector and Parakeet, word error rate against the clean transcript:

| | raw | isolated |
|---|---|---|
| clean | -- | 0.12 |
| talk 12 dB under | 0.73 | 0.12 |
| talk 6 dB under | 0.76 | 0.39 |

What testing found about using it, all of which shaped the code:

- **The sample.** It enrols on the loudest 3 s of what it is given. Three
  seconds cut from a recording made it keep the podcast and drop Hari (4
  words of 127); twelve to fifteen found him every time. Hence 12 s kept.
- **Starting cold** it muted the person's first several seconds. Fed 2 s of
  their own voice first (`PRIME_MS`) it does not; more bought nothing on
  their speech.
- **What it holds on to** is the voice it has lately heard that matches.
  After the person talks, somebody else alone is cut by 30 dB or more for
  about 45 s, then let back in. Before the person has said anything, a
  stranger alone is not cut at all. So a television while they are quiet is
  the voice check's to leave out, and Settings says to use both. Priming
  with the whole sample held on longer but cost 5 s before the microphone
  answered.
- **Speed**: one thread, 30 ms at a time, was slower than real time (1.05).
  Two threads, 80 ms chunks (`CHUNK`): 0.43 on the i7-12700F. Four: 0.33.
- **Synthetic voices**: it held on to neither of two Phonon voices, so the
  real-model test uses the recorded call its author publishes.
- sherpa-onnx 2.0.1 takes the sample only as a WAV path, read once when the
  denoiser is made (its C++ has `SetVoiceSample`, not in the C API yet). So
  it is written to a private folder, read and removed at once
  (`HeldSample`), and leftovers are removed on the next write.

## 3. Stronger noise filter: DPDFNet

DPDFNet2, 16 kHz (Ceva, Apache-2.0, a DeepFilterNet2 with dual-path RNN
blocks, 10 MB), for noise that comes and goes, which `sonora`'s suppressor,
learning a steady floor, leaves in. 0.11 of real time on one thread. On a
noisy test clip Parakeet wrote nothing raw and the sentence filtered;
babble under Hari, 0.84 to 0.58. It changed 15% of the words of his clean
speech, so it is for noisy rooms. When it is on, `sonora`'s own noise
suppression is off (`Canceller::with_noise_suppression`), so audio is not
thinned twice. `dpdfnet4` was no better on that clip at twice the cost.

## In the engine

`AudioEngine::start(options, cleanup, sink)`. The models run on a thread of
their own, "mic-cleanup", after the canceller, so a slow model never holds
up echo cancellation. Batches wait in a bounded channel (`CLEANUP_BACKLOG`,
about 7 s) while the models load and prime, so the first words are late,
not lost. If they cannot keep up (`Pace`: more than 0.9 of real time over
10 s) they are turned off for the rest of that microphone and Settings says
why (`cleanup_trouble`). The order is noise filter, then VoxLock.

The noise filter loads on the engine thread (a tenth of a second) before
`sonora`'s suppression stands aside for it, and the suppression comes back
if the models give up; each model loads on its own, so one failing leaves
the other. `AudioEngine::finish` waits for the backlog and what the models
hold back (flushed) before voice typing and dictation finish; a plain stop
tells the models' thread to stop at once. The wake word's microphone opens
again after a recording of the voice, and when a filter's model arrives or
goes (`microphone::filters`, looked at every 15 s).

## Not done

- No per-session reuse of a primed VoxLock: each microphone pays its load
  and priming, about two seconds.
- Meetings are left alone; the stronger noise filter could serve their mic
  track, at a model's cost for an hour's call.
- Untried on Hari's microphone and room. The thresholds came from one
  recording on one microphone; Strictness is there for the rest.
