# Listening and dictating

Three features on the speech layers in `docs/plans/voice.md`:

- **Listen to an email.** A button beside Summarise: the model writes a spoken
  briefing of the thread and the voice reads it.
- **Write an email by voice.** A microphone beside "Write with AI": what is said
  becomes the instruction the model writes the email from, as Superhuman does.
- **Listen to an article.** A button in the Feed's story view: the model makes
  the article fit to hear, changing as little as it can, and the voice reads it.

## Decisions

- **One player for everything read aloud.** `speech::listen` is a session like
  the voice conversation's, without a microphone: text in, audio and events out,
  with pause, back and forward a sentence, speed and stop. The desktop's speakers
  are its first transport. The player sits in the top bar, so it keeps playing
  while you move between apps. One thing plays at a time.
- **Speech starts before the writing ends.** The model's reply is streamed
  (`textstream::stream_text`) and cut into sentences as it arrives, so the first
  sentence plays while the rest is being written.
- **Every sentence is kept** for the session, so going back, going forward and
  changing speed never ask the model again. A change of speed builds the voice
  again (`Service::speech_synthesiser_at`) and starts from the sentence playing.
- **A mail briefing scales with the thread**, from about 12 seconds for one short
  message to under a minute at most (it was 20 to 90 seconds, which ran longer
  than people needed). Your own messages are headed "you wrote", so the briefing
  never tells you somebody else asked what you asked. It is gated as summaries are
  (`MailAiFeature::Summaries`): the same mail goes to the same model. A newsletter
  in the Feed is mail too, and is gated the same way.
- **An article is made fit to hear, not summarised.** The author's sentences stay
  word for word. Only what does not work aloud changes: tables, lists, links,
  footnote markers, figures, code, symbols and abbreviations. Paragraphs go to the
  model in chunks, and the next chunk is written while the one before is read.
  "Read as written" skips the model and uses only `speech::text::speakable`. It is
  also what happens when no model may be used.
- **Dictation ends on Stop, after 15 seconds of quiet, or after three minutes.**
  Both limits are counted in audio, not wall-clock time.
- **A spoken instruction is not a typed one.** `draft_with_ai` takes
  `spoken: true` and tells the model the words were transcribed by machine:
  ignore filler and false starts, let a self-correction win, prefer the spelling
  of names in the thread, and keep the person's own wording when they dictated the
  message itself.
- **One microphone, one speaker.** Dictation, voice mode and meeting recording
  refuse each other. Listening refuses to start during voice mode, and voice mode
  and dictation stop any listening first.
- **This computer only.** Like voice mode, a window on a remote vault and the web
  interface have neither listening nor dictation.
- **Nothing is kept.** Audio is played or transcribed and dropped. The briefing
  text is cached in memory per thread, as summaries are.
- **Later:** highlighting the paragraph being read in the article, and clicking a
  paragraph to start there. Every sentence already carries its paragraph (`part`).

## The layers

| Where | What |
|---|---|
| `karmayoga_service::speech::listen` | The session: `Piece` stream → `SentenceSplitter` → `speakable` → `Narrator` → `ListenOutput::Play` |
| `karmayoga_service::speech::dictate` | The session: microphone → `LiveTranscription` (`Tuning::DICTATION`) → words so far, with the silence and time limits |
| `karmayoga_service::textstream` | A model's plain-text reply, streamed |
| `karmayoga_service::listen` | `ListenSource` and `Service::listen_open`: picks the text, builds the voice, starts the session |
| `karmayoga_service::mailai::listen_pieces` | The spoken briefing of a thread |
| `karmayoga_service::news::listen::open` | An article made fit to hear, or as written |
| `karmayoga_service::mailwrite::draft_with_ai` | `spoken` instructions |
| `karmayoga-app` `listen`, `dictation` | The transports: an `AudioEngine` with only the speaker, or only the microphone |
| `ui` `listen.svelte.ts`, `dictation.svelte.ts` | The stores; `ListenPlayer.svelte` in the top bar |

## The desktop commands

By name, like `voice_start`: they are the speakers' and the microphone's, not the
vault's.

- `listen_start { source, speed, channel }`, then `listen_pause`, `listen_resume`,
  `listen_skip { by }`, `listen_speed { speed }` and `listen_stop`.
- `dictation_start { channel }`, then `dictation_stop` (keep the words) and
  `dictation_cancel` (throw them away).
