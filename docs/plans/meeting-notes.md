# Meeting notes that take themselves

The app already knows when a call is about to start, who was invited, and
how to write a note, and it already has a model it trusts to write one. What
it cannot do is hear. This plan gives it ears for the length of a call: it
offers to take notes when a call starts, records both sides, turns the
recording into a transcript that says who said what, writes a note in the
shape the person asked for, keeps the transcript, and then throws the audio
away.

## What this is

- **An offer, not a habit.** When a calendar event that looks like an online
  call starts, the app asks *"Take notes for Design sync?"*. Nothing is
  recorded until somebody says yes. A call that is not on the calendar can
  be recorded from the notes app with the same pipeline.
- **Two tracks.** The microphone is you. What the computer plays is
  everybody else. Keeping them apart from the first sample is the cheapest
  speaker identification there is, and for a one-to-one call it is the only
  identification needed.
- **Transcription on this machine, or somewhere the person chose.** A
  built-in local model, OpenAI, Google, or any server that speaks OpenAI's
  transcription API (whisper.cpp's server, Speaches) through a base URL, in
  the same way the assistant already reaches Ollama.
- **Names, not "Speaker 2".** Voices are matched against the event's
  attendees using voiceprints held in the vault. A voice nobody has named
  yet is asked about once in the note, and after that it is recognised.
- **The summary is written by the assistant's model**, the one in
  `AgentSettings.assistant_model`, following a template the person edits in
  Settings.
- **The note keeps the transcript and never the audio.** The spool is
  deleted the moment the note is written.

## Decisions already made

- **Online calls only.** In-room meetings on one microphone are a different
  problem: no channel split and far worse diarisation. They are out, and
  the design does not bend to accommodate them.
- **No consent tooling.** No announcement, no chat message, no
  per-jurisdiction logic. The person running the app answers for that.
- **The audio is not kept.** The transcript text is kept, with timings and
  speakers. The raw audio lives in a sealed spool only until the note
  exists, or until a failed run is retried or discarded.
- **Capture uses one cross-platform library: `cpal` 0.18.** As of 0.18 it
  captures system output on all three desktops. On Windows that is WASAPI
  loopback. On macOS 14.6+ it is a Core Audio process tap. On Linux, the
  native PipeWire host exposes the default sink as a device whose input
  stream is the sink's monitor. The same call opens both tracks everywhere:
  `build_input_stream` on the default input device, and on the default
  output device. No Swift sidecar, no per-OS module of our own.
- **Summaries use the assistant's model, not the quick model.** It is the
  model the person chose to reason with, and a summary of an hour's call is
  reasoning. The quick model's twelve-second, no-retry policy is also the
  wrong one for this job.
- **Identity is always worked out locally**, whichever backend transcribes.
  A voiceprint is biometric data. It does not leave the machine, and no
  provider is asked to recognise anybody. This also makes attribution
  consistent across chunks, which remote diarisation is not (see *Risks*).
- **Recording is native, in the Tauri shell.** The webview gets no
  microphone permission; `capabilities/default.json` does not change. The
  capture code sends PCM to the service through a command, so in-process
  and remote-client mode are the same code.
- **The pipeline runs where the vault is open for writing**, as a
  supervisor task, for the same reason mail sync does.

## The libraries

| Need | Crate | Licence | Why this one |
|---|---|---|---|
| Capture, both tracks, three OSes | `cpal` 0.18 | Apache-2.0 | Loopback on all three desktops as of 0.18; already the Rust default for audio I/O |
| Resampling to 16 kHz mono | `rubato` | MIT | Pure Rust, and every speech model wants 16 kHz |
| Chunk files | `hound` | Apache-2.0 | WAV in and out, nothing else. We send WAV rather than encode Opus: a ten-minute 16 kHz mono chunk is 19 MB, under OpenAI's 25 MB request limit, and it avoids an encoder dependency |
| VAD, local ASR, diarisation, speaker embeddings | `sherpa-onnx` 1.13 | Apache-2.0 | One dependency for four jobs: Silero VAD, Whisper and Parakeet recognisers, pyannote segmentation with clustering, and an embedding extractor with a speaker manager. Links statically; ONNX, no Python |
| OpenAI and OpenAI-compatible transcription | `reqwest` 0.13 (add `multipart`) | — | Already in the tree. `rig-core`'s `TranscriptionModel` returns text only and has nowhere for `diarized_json` segments or `chunking_strategy` to go, so a client of about two hundred lines is simpler than working round it |
| Gemini transcription | `reqwest`, `generateContent` with a response schema | — | Same argument; rig's Gemini transcriber is a fixed "transcribe this" preamble |

Rejected:
- **`audio-recorder-rs`:** a 0.1 crate with one author, doing what cpal now does.
- **`whisper-rs`:** only a recogniser. We would still need VAD and diarisation from elsewhere.
- **pyannote and WhisperX:** the best diarisation available, but they need a Python sidecar.
- **Meetily and Hyprnote:** worth reading, since they are Tauri and Rust notetakers, but their capture code predates cpal's loopback support and is not something to vendor.

`THIRD-PARTY-NOTICES.md` gains cpal, rubato, hound and sherpa-onnx, plus the
model licences below.

### Models

Models are never bundled with the app. They are downloaded on first use into
the app's data directory, not the vault: they are neither secret nor
per-vault. Each download is pinned by URL and SHA-256 in code.

- **Speech kit** (about 40 MB, needed from phase 2 on): Silero VAD,
  pyannote `segmentation-3.0`, and a 3D-Speaker embedding model.
- **Local recogniser** (phase 4, the person picks one):
  - *Parakeet TDT 0.6B v3*, int8: English and the major European languages, several times faster than real time on a laptop CPU.
  - *Whisper large-v3-turbo*: any language, slower.

## The shape of it

```
 ┌─ everyday-app (desktop only) ───────────────────────────────┐
 │ capture.rs   two cpal streams → 16 kHz mono i16 → 30 s       │
 │              frames, level meters, device-change handling    │
 │ commands     meeting_start / meeting_stop / meeting_levels   │
 └───────────────┬─────────────────────────────────────────────┘
                 │ meeting.append(recording, track, seq, pcm)   (in-process or remote)
 ┌─ everyday-service ▼──────────────────────────────────────────┐
 │ meeting/watch.rs      the offer: events starting now          │
 │ meeting/spool.rs      sealed chunk files, crash recovery      │
 │ meeting/pipeline.rs   supervisor task, one per recording:     │
 │     vad → transcribe → identify → merge → summarise → note    │
 │ meeting/transcribe/   {local, openai, gemini}.rs  one trait   │
 │ meeting/speech.rs     sherpa-onnx: vad, diarise, embed        │
 │ meeting/models.rs     download + checksum                     │
 └───────────────┬──────────────────────────────────────────────┘
 ┌─ everyday-core ▼─────────────────────────────────────────────┐
 │ meeting.rs   records; is_online_call(); the template filler;  │
 │              prompt builder; chunk plan for map-reduce;       │
 │              merge of two tracks, echo removal; speaker       │
 │              matching given embeddings — all pure, all tested │
 │ store/meetings.rs   the store trait + conformance tests       │
 └──────────────────────────────────────────────────────────────┘
```

This is the house split again: core decides and cannot open a socket or a
device; the service owns the sockets and the models; the shell owns the
hardware.

### The transcription trait

```rust
/// One backend. Handed one track's speech, in chunks it may assume are
/// under its own size limit; returns what it heard, with times relative
/// to the chunk.
#[async_trait]
pub trait Transcriber: Send + Sync {
    fn limits(&self) -> Limits; // max bytes and seconds per request
    async fn transcribe(&self, chunk: &SpeechChunk, hints: &Hints) -> Result<Vec<RawSegment>>;
}

pub struct RawSegment {
    pub start_ms: u32,
    pub end_ms: u32,
    pub text: String,
    /// The backend's own label ("A", "speaker_1"), if it diarises. Only
    /// ever a hint: identity is settled locally.
    pub speaker_hint: Option<String>,
}
```

`Hints` carries the language and a prompt built from the event title and
attendee names. Every one of these recognisers spells names better when
told them.

| Backend | Request | Segments | Speaker hint |
|---|---|---|---|
| Local (sherpa) | none: VAD turn → recogniser | exact | from local diarisation |
| OpenAI `gpt-4o-transcribe-diarize` | multipart, `response_format=diarized_json`, `chunking_strategy=auto` | yes | yes, per request |
| OpenAI `gpt-4o-transcribe` | multipart, `json` | one per VAD turn, since we send turns | no |
| Compatible (`whisper-1`-style) | multipart, `verbose_json`, segment timestamps | yes | no |
| Gemini | `generateContent`, inline audio (Files API above 20 MB), response schema `[{start, end, speaker, text}]` | approximate | yes |

Whatever the backend, speech is cut locally with VAD first, and silence
never crosses the wire. On a call your microphone is silent most of the time
anyway, so this roughly halves what is sent.

### Who said what

The identity stage runs after transcription and in the same way for every
backend:

1. **Mic track → the vault owner.** No inference.
2. **System track → turns.** If the backend gave speaker hints, its segments
   are the turns. If not, local diarisation of the system track provides
   them, and each transcript segment takes the speaker it overlaps most.
3. **Every turn → an embedding** (sherpa, locally). Turns are clustered
   across the *whole* meeting, not per chunk, so "speaker A" in chunk one
   and "speaker B" in chunk two can be the same person.
4. **Every cluster → a person**, by cosine similarity against stored
   voiceprints. Candidates are limited to the event's attendees plus
   anybody explicitly added. The threshold is conservative: a wrong name is
   worse than "Unknown 1".
5. **The special case:** if exactly one attendee besides you is unmatched
   and exactly one cluster is unknown, that cluster is that attendee,
   marked *inferred*.
6. **Whatever is left** is labelled "Unknown 1", "Unknown 2", and the note
   shows a chip on each: *"Who is this?"*, offering the attendees. Choosing
   one relabels the transcript, updates or creates that person's
   voiceprint, and offers to rewrite the summary.

The model is never asked to guess names. It may *propose* one ("Unknown 1
was addressed as Priya three times") as a chip, and a proposal is never
applied without a press.

### Echo

Without headphones, the microphone hears the speakers and your track
contains everybody. `core::meeting::merge` drops a mic segment when a
system segment overlaps it in time and the text is near-identical
(normalised token overlap above a threshold). This is a pure function with
a table of real transcripts as tests. The recording pill also says *"Using
speakers? Headphones give cleaner notes"* the first time it detects echo.

### Summarising

Placeholders are filled before the model sees anything:
- `{{title}}`, `{{date}}`, `{{start}}`, `{{end}}`, `{{duration}}`
- `{{organizer}}`, `{{attendees}}`, `{{present}}` (who actually spoke), `{{calendar}}`

After that, **a heading is kept verbatim and the text under it is an
instruction**, which the model replaces with content.

```markdown
# {{title}}
{{date}}, {{start}}–{{end}} · {{present}}

## Summary
Three sentences: what the call was for and where it landed.

## Decisions
Bullets. Only what was actually agreed, with who agreed it.

## Action items
- [ ] Owner — what — by when, only if a date was said.

## What the open questions were settled as

- **Models:** no defaults, locally or remotely. The person picks and
  configures the transcriber, and the switch is disabled until they have.
  The OpenAI model field offers `gpt-4o-transcribe-diarize` and
  `gpt-4o-transcribe` as suggestions, not as a preselection.
- **"Always" mode:** offered. Detected calls on the chosen calendars are
  recorded without asking, and the pill is always visible.
- **Links between notes and events:** not in this change. Meeting details
  are written into the note body, and the recording history row is the
  only thing that knows which event a note came from.
- **Purpose:** a meeting note is filed under its calendar's role
  automatically.
- **Transcript in search:** indexed as part of its note.
