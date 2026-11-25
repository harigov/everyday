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
- **Words first, then the pulse** (Nov 23 2026). Hari found the pulse alone
  mechanical: a person says "let me see" or "let me check your calendar". So
  the session says a few words of its own (`voice::phrases`), and the pulse is
  for a wait that goes on after them:
  - a turn that has said nothing 1.2 s after it began says a thinking phrase
    ("Hmm, let me think."), and the pulse holds off until then;
  - a tool says a phrase for what it touches, mail, calendar, tasks, the web,
    the vault's own words, or a change, which promises nothing ("Okay, one
    sec."), since the call may still be asked about or fail;
  - the first words of a turn come at once; later ones only for a tool
    starting 5 s or more after the last thing played, and at most three a
    turn.

  The words come from the app, not the model. Asked to say "let me check"
  itself, a model does so only sometimes (reasoning models least), its words
  can only come when its stream does, they are kept in the reply, and nothing
  covers the wait before its first word. So each errand has several phrases,
  dealt in a fresh shuffle each round so none comes twice running, and the next
  one is made into a voice while the session is only listening
  (`voice::rehearse`), so it plays the moment it is wanted. The note to a
  spoken turn tells the model the voice fills waits and not to say it is
  checking. When it does anyway, its words count as the turn's first, and a
  tool call ends the text before it, so they are said before the call rather
  than held back to come out in front of the answer.
- **"Type that" from the talk key** (Nov 23 2026). A session the shell opens
  for the talk key types into the field the person was in (`type_text`, see
  `panel.md`): at once for their own clear words with no stranger's in the
  turn, otherwise shown on the card first.
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
- **Sentences behind the first are said together** (Nov 23 2026). A voice
  handed one sentence at a time starts every one afresh -- the same pitch at
  the start of each, the same fall at the end -- which is what made a reply
  sound read out. The narrator (`speech::narrate`, `Narrator::joining`) makes
  the reply's sentences that are waiting behind a clip already playing into
  one clip, and so for something read aloud; the first sentence, and anything
  said into silence, is still made alone, so a reply starts as soon as it
  did. A clip is at most twice as long as the one ahead of it (it is made
  while that one plays: Phonon makes speech about nine times as fast as it
  is said on this computer, Kokoro four) and at most 240 characters, and
  never goes past a line's end, whose longer pause only a clip's end gets.
  (Both the first sentence alone and twice the clip ahead changed on Nov 24:
  see the next entry.) Tool phrases and questions are never joined: a phrase is rehearsed alone,
  and a question is waited on by its id. What was heard of a cut reply is
  counted in clips: one talked over half way counts as not heard. A clip the
  voice cannot make is made again a sentence at a time. A rendered article or
  podcast joins as much as fits in a line. Listen does not join: the reader
  shows and skips by sentence.
- **Clips as long as the voice can keep up with, the same rule for every
  voice** (Nov 24 2026). Hari still heard a reply as a list read out. Three
  things kept clips short, and each is now one rule for all voices rather
  than a setting per voice:
  - *Twice the clip ahead* was a guess at the slowest voice. Every voice now
    times itself in `Delivery`, the wrapper all of them are built in
    (`Synthesiser::rate`: characters made a second, the middle of the last
    five clips of 30 characters or more, so one slow clip -- the model loaded
    again after a rest, a wait behind other speech -- moves nothing, while a
    busy computer shows within three). The rate is kept per engine for the
    life of the process, so a new voice session, Listen at its own speed or
    a podcast's second host starts knowing it; a phrase handed back from
    rehearsal never reaches `Delivery`, so it cannot make the voice look
    fast. The narrator sizes the next clip to what can be made in half the
    time the audio ahead has left to play (playback paused for somebody
    talking is not counted down), and in no more than two seconds, since a
    clip talked over is still finished by the voice and holds it from the
    next reply. A fast voice reaches the 240-character cap by the second
    clip; a slow voice, or a busy computer, keeps them short enough that the
    speech never stops for one. A voice not yet timed is taken to be
    Kokoro's speed, which is the old rule.
  - *The first sentence was always alone.* It may now wait for a second, as
    long as the audio starts no more than half a second later than the first
    alone would have (`FIRST_WAIT`): the wait for the second sentence's words
    and the time to make both count. It waits only while a sentence of the
    usual length (60 characters) could still be made in that time. The first
    version waited while even the shortest could, and a real run showed it
    only ever delayed: the second sentence (77 characters) never fitted. On
    this computer's processor neither Phonon (about 140 characters a second)
    nor Kokoro (60 to 85) can make 60 characters in half a second, so the
    wait is for faster voices; the gain for these is behind the first clip.
    The end of the reply or of the turn, a tool call, the end of something
    read aloud, a second sentence that does not fit or one of another kind
    ends the wait at once (`Narrator::flush`). The session's loop wakes the
    narrator when the wait is up (`Narrator::wake_at`).
  - *Phonon cut joined clips apart again.* `ptts` groups what it is handed
    into pieces of about one long sentence (50 tokens) and makes each afresh
    from the speaker's voice, knowing nothing of the piece before. Every
    voice is now told to say what it is handed in one go, up to a clip's 240
    characters (the `Synthesiser` contract): `ptts` with its largest piece
    (`MAX_FIT_TOKENS`, 200), Kokoro already (400 phonemes). Where speech is
    cut is the narrator's to decide. Measured on a three-sentence clip in two
    speakers: the cut left a 0.72-0.74 s silence where `ptts` restarted, and
    one piece leaves 0.44-0.48 s, like the sentence before it; Parakeet heard
    every word both ways, and one piece is made a fifth faster.
  - *Measured end to end* with both voices, a reply streamed at a fast
    model's pace and at a slow one's, playback simulated in real time: no
    gaps on a quiet computer; clips of 39, 77, then 119 to 184 characters
    behind a fast model. Behind a slow model (40 characters a second) most
    clips stay one or two sentences, since each is started as soon as its
    sentence is whole; holding a clip behind audio for more words is not
    done yet.
- **Phonon's own temperature, and a seed per sentence** (Nov 23 2026).
  `ptts` used temperature 0.3 and one fixed seed for every sentence; Phonon's
  config asks for 0.8. It is now 0.7, and each sentence is seeded from its
  own words. Measured, neither changes much -- not how well Parakeet hears it,
  how far the pitch moves, or the length of a clip -- so the joining above is
  what to listen for.
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
| `expect` | The words a recogniser is told to expect, gathered from the vault for what the listening is for |
| `hotwords` | How Parakeet is told them: sherpa-onnx's hotwords and the vocabulary it cuts them with |
| `synth` | Words into audio, through any configured voice, Phonon and Kokoro included |
| `tempo` | Speech sped up or slowed down without changing its pitch, for the local voice |
| `text` | Writing made fit to hear, cut into sentences |
| `script` | The marks in a sentence that say how it is said -- a word and its respelling, and a line's end -- the pause after it, and what of it is shown |
| `narrate` | Sentences into audio in order, a limited way ahead of playback |
| `pcm`, `wav` | Sample and file conversions everything shares |

### Words to expect

A recogniser hears a name it does not know as the nearest everyday word:
"Carma" for an assistant called Karma, "Priya Sha" for Priya Shah. So every
recogniser that can be told is told the words it is likely to hear.
`karmayoga_core::speech::words` decides which words, how many and in what form,
and `speech::expect` gathers them from the vault.

- **What the listening is for decides the words** (`Occasion`). Talking to the
  assistant (voice mode, the talk key, a conversation the wake word began): its
  names, its own and the wake word's; the person's name; the people on the next
  day and a half of events, then their circles, closest first; those events'
  places and titles; active projects, goals and areas; routines; the apps they
  use most this week and the connected services; and the app's own words
  (Karma Yoga, Siddhis). Listening for the wake word: only the names it answers
  to and the person's, since any other boosted name ("Jamie", a contact) could
  be written in place of the assistant's at the start of an utterance, the one
  place the wake word looks. Writing by voice (voice typing, dictation): the
  people, places and work they write about, and the assistant's names last. A
  meeting: the person's own words first, then its invite's names, companies,
  place and agenda words (`words::for_meeting`). A podcast: its show and title.
  The person's own list from Settings → Meetings goes everywhere.
- **Not everything is a word to boost.** A first name that is an everyday word
  too ("Will", "Grace") is boosted only as part of the whole name; times and
  ordinals ("10am", "2nd") and the words of a title written in capitals are not
  unusual; a video call ("Microsoft Teams Meeting", "Zoom") is not a place.
- **Two forms.** Qwen3-ASR reads a short prompt in its system turn, and so do
  OpenAI's and Google's servers: a few labelled sentences, at most 500 bytes
  ("Hari talks to Karma. People: Priya Shah. Words: OKR."). Parakeet boosts
  words as it decodes (sherpa-onnx hotwords, which need its beam search): names
  and unusual words only, at most 40, since an everyday word is one it already
  hears well and every boosted word is a chance to hear it where it was not
  said. Whisper on this computer takes neither.
- **Not overwhelming it.** Each kind has a quota, most relevant first; too long
  a prompt loses the least relevant words first and the assistant's names and
  the person's own list last. Titles are context for a prompt, but only their
  unusual words (capitals mid-sentence, acronyms, codes) are boosted.
- **Fresh.** A listener's words are gathered when it starts and again every ten
  minutes while it runs, so the all-day wake word moves on with the day. The
  refresh runs on a thread of its own, and the utterance that finds the words
  stale is told the old ones rather than waiting. A listener holds the vault
  weakly: one still running cannot keep a closed vault, and its lock, alive.
- **Read back only where read.** An utterance is cleaned against its prompt only
  when the recogniser read it (Qwen or a server) and the audio was faint. Said
  clearly, the prompt's own words are somebody talking ("Cancel the design
  review with Acme"), and Parakeet, told words to boost, has no prompt to read
  back.

Parakeet's download has no vocabulary for sherpa-onnx to cut the words into its
pieces with, so `hotwords::vocabulary` writes `bpe.vocab` beside `tokens.txt`
at every load, scored from each piece's place in the BPE order. sherpa-onnx's
cutter is not BPE; it agrees with Parakeet's own cut for about three names in
four, and a word cut differently is boosted only as far as the cuts agree.

`karmayoga-app`'s `audio` module is the same idea for the devices: `input` and
`resample` serve meeting capture and the engine alike, and `AudioEngine` runs a
microphone, a speaker or both, with echo cancellation when it has both. Every
microphone, the engine's and meeting capture's, is cleaned of rumble, mains hum
and steady hiss by `echo::Cleaning::Noise` even with nothing playing.

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
6. **While it works.** `ToolPreparing` or `ToolStarted` first ends the text
   before the call (the splitter's held tail is a sentence), then says a phrase
   for `Errand::of(name)` (never for `end_voice_mode`). A turn that has queued
   nothing to say `FILLER_AFTER` (1.2 s) after it began says an
   `Errand::Thinking` phrase. Either only while the turn is working, nothing is
   queued or playing, nobody is talking or being understood and no question
   waits; the turn's first words at once, later ones `AGAIN_AFTER` (5 s) after
   the last thing played, at most `MAX_PHRASES` (3). `Phrasebook` deals them;
   the next for the same errand, and at start a thinking one, is rehearsed once
   the session is `Listening`.
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
    session stops as for `Stop`. Words said over the goodbye that are not an echo, a stop
    word or courtesy (15) start a turn, which takes the ending back. Only the talking ends: the
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
    transport to play (`Narrator::playing`), nothing held, and no turn waiting
    for its moment to think aloud (6); `false` as soon
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

14. **The listening chime.** `VoiceInput::Chime`, asked for by the shell when
    the wake word opened the conversation with the name alone, becomes
    `VoiceOutput::Chime` for the transport to play (`cue::listening_chime`),
    unless the session is stopping or ending. Nothing is made of the moment
    after it: the chime has no words for an echo check to match, and words that
    follow it are the person's own and clear to approve what they ask for ("type
    I'm running late"), so it is not counted as audio in the air.
15. **A plain goodbye ends it.** Whole utterances only, looked at after the
    echo checks (so the session's own goodbye coming back is not another
    goodbye) and before a stop word (`phrases::is_closing`: "that's all",
    "nothing else", "I don't have anything else", "I'm done", "bye", "talk
    to you later", with "no", "okay", "thanks" and the like round them;
    never "that's it", "no" or "no thanks", which are as often an answer).
    The session stops whatever is running as a stop word does (`interrupt(None)`:
    the turn cancelled, audio and any question cleared, the stored reply cut
    to what was heard), says one of a few goodbyes of its own
    (`Errand::Goodbye`, dealt like the others), and sets `ending`, so
    10 ends the session once it has played. No turn begins and nothing is
    announced as `Heard`. While it is ending, an utterance that is only
    courtesy (`phrases::is_courtesy`: thanks, bye, "you too", "okay",
    "got it", a murmur, or another closer) is heard and let go: it neither
    takes the goodbye back nor says another. Real words still do. What follows
    the wake word (`VoiceInput::Spoken`) is looked at the same way: a filler
    is nothing, a plain goodbye ends it without a turn.
16. **Fillers, fragments and murmurs.** Looked at in `on_words`, in this
    order: nothing, or nothing but fillers ("um", "hmm", "oh":
    `is_filler`), is nothing, and is not an unclear answer to a question
    either; an utterance that began while audio was actually playing
    (`over_playing`, not its tail: a word a moment after the reply is an
    answer, "Friday" to "Thursday or Friday?") and is one or two words that are
    all in what was said in the last three seconds, in order
    (`is_echo_fragment`), is an echo and is let go -- never a stop word, which a
    reply may contain, nor a word that answers (yes, no, yeah, yep, nope, sure,
    okay); a backchannel
    ("mm-hmm", "uh-huh", "right", "got it": `is_backchannel`) said over
    audio that was actually playing when it began (`Utterance::over_playing`,
    not merely its tail in the air) lets the reply go on. "Yes", "yeah" and
    "okay" are not murmurs: they answer.
17. **Dictation.** A turn whose outcome says `dictate` (`start_dictation`,
    for a session that can type) and that was not talked over begins one:
    `VoiceEvent::Dictation { active: true }`. While it lasts no turn begins,
    no update is said and there is no working sound; each utterance is
    echo-checked as above, and then is its own end ("stop dictating",
    "that's all", "I'm done", a stop word: "Done." or the like is said), a
    goodbye (`is_closing` and not an end of dictation, or an end of dictation
    with a goodbye in it, "that's all, bye": it is typed up to, then ends as in
    15), or text. Text is made by `phrases::dictated`
    (fillers out, "new line" and "new paragraph" as breaks, a capital after
    a break, a space in front when it runs on from the last) at the moment
    it is heard, and typed through `VoiceAssistant::type_text`, strictly in
    order and one at a time, each told as `VoiceEvent::Typed`. Typing that
    fails is `Trouble`, a short sorry, and the end of the dictation, with
    what waited behind it dropped. What was said before "stop dictating" is
    typed before it is over, and a goodbye said after it waits for that
    (`said_goodbye` does not end the session while anything is being typed or
    queued). `start_dictation`, like `end_voice_mode`, is given no tool phrase:
    "Okay, one sec." in front of "Go ahead." says nothing. A session that stops in the middle of one
    emits `Dictation { active: false }` first. The real assistant types
    through the conversation's typist, with no line break when the wake word
    opened it.

## The desktop transport (`karmayoga-app/src/voice`)

Streams are opened and dropped on one audio thread, as in `capture.rs`. The
microphone is resampled to 16 kHz, cleaned by `sonora` in 10 ms frames with what
was actually played as the reference, and sent as `VoiceInput::Audio`. `Play`
audio is resampled to the output device's rate and queued. Voice and meeting
recording refuse each other: there is one microphone.
