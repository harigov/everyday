# Talking to the assistant

A conversation you can have out loud: press the microphone in the assistant's
composer, talk, hear the answer, and talk over it to change course. What is said
goes into the same thread as what is typed.

## Decisions

- **One session, any transport.** `karmayoga_service::voice` takes 16 kHz mono
  audio and gives back audio and events. The desktop app's microphone and
  speakers are the first transport; a Matrix call is the second (see
  `calls.md`, through `Service::voice_open_via`). WebRTC in a browser and XMPP
  are later callers.
- **Echo cancellation belongs to the transport**, which alone knows what was
  really played. The desktop uses `sonora`, a pure Rust port of WebRTC's audio
  processing. A browser brings its own.
- **Voices are a setting**, the sixth card in Settings → Models: OpenAI, any
  server that speaks OpenAI's `/v1/audio/speech` (how open-source voices such as
  Kokoro run), or Supertonic 3 on this computer through sherpa-onnx.
- **Supertonic rather than Kokoro or Piper for the built-in voice:** it needs no
  espeak-ng data. Its code is MIT licensed; its weights are OpenRAIL-M.
- **Spoken approval is narrow.** Web searches and page fetches can be answered
  by voice, when the question can say all of what would be sent: a long search,
  or an address with a query, login, port or escapes, waits for the card. So does
  everything else.
- **Audio is not kept.** The thread holds words only.
- **This computer only, for now.** A window connected to a remote vault, and the
  web interface, have no voice mode. A call through a chat link is answered by
  whatever runs the link, a server included.

## The layers underneath

Listening and talking are platform services. The assistant's voice mode and
meeting notes use them today. Dictation, controlling the app by voice, live
meeting transcripts, reading an article or email aloud and voice messages are
meant to be built on them without new plumbing.

`karmayoga_service::speech`:

| Module | What it is for |
|---|---|
| `access` | `Service::utterance_transcriber`, `speech_synthesiser`, `live_transcription`: what is chosen in Settings → Models, ready to use, or a sentence saying what is missing |
| `detect` | Where speech starts and ends in a stream, with a `Tuning` per use |
| `utterance` | One stretch of speech into words |
| `live` | A stream of audio into ordered, timed words as they are said |
| `transcribe` | Chunks of a recording into timed segments, the meeting pipeline's backends |
| `local` | The sherpa-onnx models on this computer |
| `synth` | Words into audio, through any configured voice |
| `text` | Writing made fit to hear, cut into sentences |
| `narrate` | Sentences into audio in order, a limited way ahead of playback |
| `pcm`, `wav` | Sample and file conversions everything shares |

`karmayoga-app`'s `audio` module is the same idea for the devices: `input` and
`resample` serve meeting capture and the engine alike, and `AudioEngine` runs a
microphone, a speaker or both, with echo cancellation when it has both.

**Where the next features attach.** Dictation and voice control: an `AudioEngine`
with only the microphone into `Service::live_transcription`, acting on
`LiveEvent::Transcribed`. Live meeting transcripts: one `LiveTranscription` per
track, using the timings in its events. Read aloud: `text` and a `Narrator` into
an `AudioEngine` with only the speaker. Voice messages: `speech_synthesiser` and
`wav`.

## The session (`voice/session.rs`)

A Tokio task. Everything slow runs elsewhere and reports back on a channel: the
detector on its own thread, transcription, synthesis and the turn on their own
tasks. Nothing the loop awaits may block it from hearing `Stop`.

1. **Listening.** Audio goes to the detector unless muted. `Detected::Started`
   moves to `Hearing`; `Detected::Ended` moves to `Understanding` and transcribes.
2. **A turn.** Non-empty words emit `Heard`, then run `VoiceAssistant::turn`. Every
   `AgentEvent` is forwarded as `VoiceEvent::Agent`. The state is `Thinking` until
   audio is queued, then `Speaking`.
3. **Speaking.** `Delta` goes through `SentenceSplitter`; each sentence through
   `speakable`, then the synthesiser, strictly in order, at most two sentences
   ahead of what is playing. `Retrying { text }` calls `replace`, so nothing is said
   twice. `Finished` flushes `finish()`. When the last `Played` arrives and the turn
   is over, back to `Listening`.
4. **Talking over it.** The detector is `Guarded` whenever audio is outstanding.
   `Started` while audio is outstanding sends `Pause`. When the words arrive:
   - empty, or `sounds_like_echo` against the last 30 seconds said: `Resume`, as if
     nothing happened;
   - `is_stop_word`: `Clear`, cancel the turn, cut the reply, `Listening`;
   - anything else: `Clear`, cancel the turn, wait for it to end, cut the reply,
     then run a new turn on the words.
5. **Cutting a reply.** If the person did not hear all of it, the stored reply
   becomes the sentences whose `Played` arrived: `keep_heard`, then `ReplyCut`. The
   model's memory of the conversation then matches what was heard. Tool phrases are
   not part of it.
6. **While a tool runs.** The first `ToolPreparing` or `ToolStarted` of a turn with
   nothing queued says `tool_phrase(name)`, once per turn.
7. **Confirmations.** On `ConfirmationRequired`:
   - `may_answer_aloud(kind)`: queue `question(kind, subject)`, state `Asking` once
     it has played. Only speech whose `Started` came after that `Played` counts, so
     the question cannot approve itself through the speakers. `spoken_answer`: `Yes`
     confirms, `No` declines, `Unclear` asks once more; a second `Unclear` falls
     through to the click.
   - otherwise: queue `NEEDS_A_CLICK`, state `WaitingForClick`.
   The card stays live on screen either way. A `ToolStarted`, `ToolFinished` or
   `Finished` naming that call means it was answered there, and the session moves
   on. Talking during `WaitingForClick` is an ordinary interruption.
8. **Failures.** Hearing or synthesis errors emit `Trouble` and carry on; a failed
   sentence is skipped. `Failed` from a turn emits `Trouble` and says "Sorry, that
   didn't work."
9. **Stop.** Cancel any turn, `Clear`, cut the reply, `Ended { error: None }`. A
   closed input channel is a `Stop`.

## The desktop transport (`karmayoga-app/src/voice`)

Streams are opened and dropped on one audio thread, as in `capture.rs`. The
microphone is resampled to 16 kHz, cleaned by `sonora` in 10 ms frames with what
was actually played as the reference, and sent as `VoiceInput::Audio`. `Play`
audio is resampled to the output device's rate and queued. Voice and meeting
recording refuse each other: there is one microphone.
