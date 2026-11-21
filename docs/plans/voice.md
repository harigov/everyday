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
  Kokoro run), or Phonon 31M or Kokoro 82M on this computer.
- **Phonon 31M for the built-in voice:** the best small open voice in blind tests
  (Artificial Analysis Elo 1110, tied with OpenAI's tts-1 HD), Apache-2.0 weights,
  and a pure Rust runtime, `ptts`, with no native library and no phonemiser. About
  ten times faster than real time on four x86 threads, with first audio in about
  70 ms. English only. It replaces Supertonic 3, which Supertone stopped
  maintaining; saved `supertonic3` settings read as Phonon. A speed set for it is
  applied afterwards by `speech::tempo`, which keeps the pitch.
- **Kokoro 82M as the second voice on this computer** (Nov 20 2026), for its
  different speakers: 28 English ones, American and British, hexgrad's v1.0 as
  sherpa-onnx packages it. Run by sherpa-onnx, already linked for listening.
  Since 2.0 sherpa-onnx has no espeak-ng and skips any word its lexicon lacks,
  so `speech::synth::kokoro` makes the phonemes itself with `loqui-g2p`
  (misaki's pipeline in Rust, with a small neural model for unknown words) and
  hands them over in runs of at most 400. The full-precision model, about 4×
  faster than real time on four threads; the int8 one is less than half as
  quick. Kokoro sets its own speed, so nothing is stretched afterwards. It
  scores below Phonon in blind tests (AA Elo 1060).
- **What is asked for out loud is approved by asking** (Nov 19 2026). At this
  computer, adding, finishing and moving tasks, scheduling, moving and
  cancelling events, changing what is on a shelf and writing notes
  (`voice::confirm::CHANGED_BY_VOICE`) run in a spoken turn without a card,
  invitations and cancellations to guests included. Held back:
  - **Words that may be an echo.** Only what the person began saying while
    nothing of the assistant's own was audible approves anything
    (`Turn::voice_approves`): a reply, or an article being read aloud, coming
    back through the microphone must never be the yes.
  - **After a stranger's words** (a mail, a page, an invitation's description;
    listing events counts): only a change the person's own words this turn
    asked for (`request_asks_for`: "cancel", "remove", "reschedule", "remind
    me", ... -- whole words, not in a question, not after a "don't"), and for
    the calendar only a new time or a cancellation; an invitation to somebody
    new, or a title, place or description sent to the guests, asks.
  - **On a call through a chat.** A destructive call from a phone always asks
    (`confirms_destructive`), spoken or typed.

  What is held back is asked about: a delete aloud, by the name of what goes
  ("Should I delete Book the dentist?"), and a spoken yes does it; anything
  else on the card, or in the chat on a call. The model is told, in the note
  to a spoken turn, not to ask "are you sure?" itself.
- **Other spoken approval is narrow.** Web searches and page fetches can be
  answered by voice, when the question can say all of what would be sent: a
  long search, or an address with a query, login, port or escapes, waits for
  the card. So does everything else: a mail sent, a memory kept, a project
  deleted.
- **Silence while it works is filled.** From the moment the person stops
  talking until the first of the answer plays -- understanding, thinking, a
  tool running, the first sentence being made into a voice -- the transport
  plays a soft pulse (`voice::cue`, `VoiceOutput::Working`): a pause of about
  half a second, then a low swell, every 1.6 s, faded out when it stops. Never
  while the person talks, while anything plays or is held, or while the
  assistant waits on them (a question, a card).
- **Mail and articles, the way the apps tell them.** `summarize_thread`
  summarises a thread or a newsletter with the Mail app's own Summarise prompt,
  cache and per-account switch -- in a spoken turn, the Listen button's spoken
  briefing instead. `read_aloud`, offered to spoken turns, reads a newsletter,
  an email or a Feed article out in full after the reply, made fit to hear as
  the Listen player makes it; the text never reaches the model.
- **It talks like a person, because the writing does.** None of the voices
  has a switch for warmth or emphasis (Phonon has none at all, and Kokoro's
  stress marks only lift a word that was unstressed), so the delivery is in
  the text: the core's note to a spoken turn asks for contractions, a natural
  reaction when it fits, feeling carried by words and by `!`, `?`, `…` and
  dashes, stress by where a word falls rather than capitals, and a list as a
  count and then one short sentence on its own line per item. OpenAI's
  `gpt-…-tts` voices, the only ones that take instructions, are also asked to
  sound warm and to pause between items.
- **Pauses are the sentence's, whichever voice says it.** Each model leaves its
  own silence round a clip -- measured, Phonon nothing before and 0.5 to 0.75 s
  after, Kokoro 0.3 s before and about 0.45 s after -- so every clip is cut
  back to the speech in it, faded at both edges, and given the pause its
  ending asks for (`speech::synth::pace`, `speech::script::pause_after`):
  about 0.3 s after a full stop or an exclamation, a little more after a
  question, half a second after an ellipsis, 0.55 s after a line (an item of a
  list, a paragraph), a breath after a sentence cut at a comma. The splitter
  marks a sentence that ended a line with `¶`, since the line break is gone by
  then. A faster speed shortens the pauses with the words. Phonon reads a dash
  as a space, so a dash becomes a comma for it; Kokoro's phonemiser drops `…`
  and a hyphen standing alone, so they become `...` and a dash for it.
- **Kokoro keeps its pauses.** sherpa-onnx's default `silence_scale` of 0.2
  cut every silence of 0.2 s or more to a fifth, splicing straight to the next
  sound with no fade: commas came out at 60 ms, sentences said together ran
  into each other, and the voice was heard stopping short and jumping on.
  The scale is 1.
- **Words it would say wrong are said the person's way.** A spoken reply
  writes a word the voice is likely to get wrong as `{word|respelling}` --
  plain letters, hyphens between syllables, the stressed one in capitals:
  `{Siobhan|shiv-AWN}`. The person's own list (`AgentSettings::pronunciations`,
  edited in Settings → Models → Voice, and by `remember_pronunciation` when
  they correct the assistant) is marked into every sentence any voice says,
  Listen's included. Every voice says the respelling its own way: Kokoro as
  phonemes made a syllable at a time, with the capital syllable stressed
  (the respelling run through misaki whole read its capitals as letters);
  Phonon and OpenAI-style servers as lower-case letters. Only the word is
  shown and kept: the session streams the reply to the screen through
  `script::Shown`, and the thread, what was heard of a cut reply and the echo
  check all have the marks taken out (`script::shown`). Only the assistant's
  own spoken reply is said with its marks: a question about a call, something
  read aloud, an update and Listen are made inert first (`script::inert`) and
  said as written, so a stranger's text cannot make a question say one thing
  while another is sent. A respelling holds only letters, spaces, hyphens and
  apostrophes, and no more words than the word it is for and one more (of at
  most three), and the tool asks first once a stranger's words are in the
  turn. The list is shared by every voice the service builds
  (`Service::pronunciations`), so a word corrected mid-conversation is said
  the new way from the next sentence.
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
| `synth` | Words into audio, through any configured voice, Phonon and Kokoro included |
| `tempo` | Speech sped up or slowed down without changing its pitch, for the local voice |
| `text` | Writing made fit to hear, cut into sentences |
| `script` | The marks in a sentence that say how it is said -- a word and its respelling, and a line's end -- the pause after it, and what of it is shown |
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
10. **Ending by voice.** A spoken turn, and only a spoken one, is offered
    `end_voice_mode` (`voice::END_TOOL`). The model calls it when the person makes
    clear they are done ("that's it", "that's all I have") and then says a short
    goodbye; the model decides, not a phrase list, so "that's it!" said of a right
    answer is not taken for goodbye. The turn reports `TurnOutcome::ended_voice`
    (read from the calls that ran, never for a stopped turn), and once its reply has
    played -- nothing outstanding, nobody talking or being transcribed -- and 1.5 s
    more have passed for the last word to leave the speaker or the phone, the
    session stops as for `Stop`. Words said over the goodbye that are not an echo or a stop
    word start a turn, which takes the ending back. Only the talking ends: the
    thread stays. On a call through a chat, ending the session hangs up.
11. **Reading aloud.** A turn whose `read_aloud` ran reports what it chose,
    each call in order (`TurnOutcome::read_aloud`, never for a stopped turn),
    and they are read one after another. The session opens its
    text on a task of its own (`VoiceAssistant::open_reading`, which is
    `Service::read_aloud_text`: the Feed's or a newsletter's Listen
    preparation) while the reply plays, then reads it sentence by sentence
    after the reply, as `Kind::Reading`: said and echo-checked like anything
    else, but not part of the reply, so nothing of it is kept. Its text is
    asked for a piece at a time, only while fewer than four of its sentences
    wait to be played, so stopping early does not pay for the rest. Talking
    over it is talking over a reply: an echo resumes it; anything else, a stop
    word included, ends it, and whatever was still to be read. A text that
    cannot be opened says why aloud when the reason is a sentence for a person
    ("There is nothing in this email to read."), and "Sorry, I couldn't read
    that" when it is not. The reply's own end is announced
    (`VoiceEvent::ReplyHeard`) once its last sentence has played, so a call's
    chat can write it before the reading is over.
12. **The sound of working.** `VoiceOutput::Working(true)` while the state is
    `Understanding`, `Thinking` or `Speaking` with nothing yet handed to the
    transport to play (`Narrator::playing`), and nothing held; `false` as soon
    as anything plays, the person talks, or the session stops. Sent before the
    state it goes with. The detector stays `Open` for it: most of it is
    silence, a desktop without a speaker plays none of it, and a short "stop"
    said while a slow tool runs must be heard. The desktop mixes `cue::Bed` into the speaker queue
    when it has no speech (so the echo canceller hears it as it hears speech);
    a call puts it in the 20 ms frames.
13. **Updates.** `VoiceInput::Update` carries something a background job has
    to say: a watch seeing CI finish, a project reaching a milestone. See
    `docs/plans/watches.md`. It is queued, never said at once. It is said at
    the session's next gap:
    - nobody is talking or being understood;
    - no turn is running, or the last turn's reply has played;
    - no question is waiting;
    - nothing is being read aloud or is still to be;
    - nothing of the session's own is playing or queued;
    - the echo tail since the last sound has passed.

    Muting does not hold updates back: muting stops listening, not speaking.
    Updates are not said while the conversation is ending; they wait, and go
    if the person talks again. Several waiting are said together after one
    lead ("A couple of updates:"), at most five, then "And N more." They are
    the session's own speech, not part of any reply. In quiet hours the router
    holds updates rather than handing them over, then gives them to the
    sessions that were open when they were held.

## The desktop transport (`karmayoga-app/src/voice`)

Streams are opened and dropped on one audio thread, as in `capture.rs`. The
microphone is resampled to 16 kHz, cleaned by `sonora` in 10 ms frames with what
was actually played as the reference, and sent as `VoiceInput::Audio`. `Play`
audio is resampled to the output device's rate and queued. Voice and meeting
recording refuse each other: there is one microphone.
