# Voice assistant reliability: waking, ending, hearing, typing

Hari's four complaints about the voice assistant (Nov 23 2026), what was
behind each, and the fix. The sessions are voice mode's
(`karmayoga_service::voice::session`), opened by the window, the talk key or
the wake word (`karmayoga-app` `talk/`, `wake.rs`).

## 1. Calling it by name gives no sign it is listening

"Jimmy" alone opens a conversation with no first turn. The card goes up in
the corner, but nothing is heard, and the person, who is not looking at the
corner, cannot tell whether they were heard or when to speak. Words said
before the session has opened are lost.

**Fix.** A short listening sound, played once the session is open and
listening: `VoiceInput::Chime` → the session → `VoiceOutput::Chime` → the
transport plays `cue::listening_chime` (two soft rising notes, a quarter of a
second, mixed over whatever plays and heard by the echo canceller). Asked for
by `talk::open` only when the wake word opened it with the name alone. Words
said straight after it are a clear turn like any other: a tone has no words
for an echo check to find, and treating them as over audio would have made
"type I'm running late" stop to ask.

When the wake word came with words after it, that first turn waits up to
400 ms for the main window to say what it shows (`talk::screen_shared`, from
`voice_set_screen`), so "this article" is the one open there.

## 2. "I don't have anything else" does not end it

Ending was wholly the model's call (`end_voice_mode`), and then the end could
be taken back by anything heard: `said_goodbye` waits for nobody to be
talking, and any words that were not an echo began a turn, which clears
`ending`. So "bye" or "thanks" said back to the goodbye, a keypress the
recogniser wrote as a word, or a two-word echo of the goodbye itself
started the conversation again, and the model answered it.

**Fix.**

- **The session hears a plain goodbye itself.** A whole utterance that is
  only a way of saying "I'm done" (`phrases::is_closing`: "that's all",
  "nothing else", "I don't have anything else", "I'm done", "bye", "talk to
  you later", with "no", "okay", "thanks" and the like around it) ends the
  conversation without a turn: whatever is running is stopped, the session
  says a short goodbye of its own, and ends once it has played. Bare
  "that's it" and "no thanks" are left to the model: they are as often an
  answer.
- **Once it is ending, courtesy does not undo it.** While `ending`, an
  utterance that is only thanks, goodbye, a backchannel or another closer is
  heard and let go; only real words carry the conversation on.
- **The model is told more plainly** when to call `end_voice_mode`, with the
  phrasings people use, and not to end every answer with "anything else?".

## 3. Typing, the room and its own voice are taken for speech

Echo needed three words in a row to count, so a one- or two-word fragment of
the assistant's own reply coming back through the microphone ("added it")
interrupted the reply as if the person had spoken, and began a turn on it. A
murmured "mm-hmm" did the same, and a recogniser's "Hmm." for a breath or a
chair was a turn of its own. And Silero's speech probability alone decided
what counted as somebody talking.

What was measured: the real detector (Silero, conversation tuning) over seven
recorded keyboards (Wikimedia Commons), as recorded and 12 and 24 dB louder,
raised no start at all. So typing on its own is not what Silero mistakes;
what reaches the session as "words" while somebody types is far more likely
their own voice or the assistant's coming back, a murmur, or a filler the
recogniser wrote for a breath -- the session's rules below.

**Fix.**

- **Short echoes and murmurs.** Over the assistant's audio, an utterance
  of one or two words that are all in what was said in the last few seconds
  is an echo (unless it is a stop word). Said while a reply is playing, a
  backchannel ("mm-hmm", "uh-huh", "right", "got it") lets the reply go on.
  An utterance of nothing but fillers ("um", "hmm") is nothing.
- **A floor of voice under Silero** (`speech::voicing`). Every 32 ms window
  Silero calls speech is also checked for a pitch: low-passed under 900 Hz,
  loud enough, mostly low-frequency, and periodic at a pitch a human voice
  has (80 to 500 Hz, normalised autocorrelation of 0.6 or more at a local
  peak); a window counts only beside another. `Turns` reports a start only
  after such a pair, and drops an utterance without one
  (`min_voiced_seconds`, 0.064 s talking and dictating, 0 for podcasts),
  closing it with an empty `Ended` if it had started. Deliberately low: at
  0.75 and 0.12 s it dropped a recorded "yes" and "enemy"; at these values
  every recorded word got through and the keyboards still gave nothing. It
  is for a sound with no pitch in it at all.

The owner's voiceprint (`Voiceprint::is_owner`) as a gate is the next step for
other people's voices and the television, not this one.

## 4. "Type …" is misunderstood, and a document cannot be dictated

The model was never told it could type: `type_text` was one tool among
a hundred and fifty, and nothing in the turn said the person was talking
from another application's text field. A dictation longer than one
breath was cut by the end-of-turn pause into separate turns that talked over
each other. And on Wayland a conversation the wake word began has no
activation token, so the helper window that sets the clipboard was likely
refused the keyboard, and nothing was typed.

**Fix.**

- **A typing note** with every spoken turn that has a typist: they are
  talking from wherever they are on the desktop; `type_text` types into the
  field that had the keyboard; "type", "write", "put", "reply saying …" mean
  it; "here" is that field; find first what must be found (an article, a
  mail), then type what was written. The `type_text` description says the
  same.
- **Dictation inside a conversation.** A new tool, `start_dictation`
  (`voice::DICTATE_TOOL`, offered beside `type_text`), hands the microphone
  over: once the reply ("Go ahead.") has been said, each utterance is typed
  as it is heard (`VoiceAssistant::type_text`, in order, one at a time), with
  fillers out and "new line" / "new paragraph" as line breaks, until the
  person says "stop dictating" (or "that's all", "I'm done", "stop"). The
  session says so (`VoiceEvent::Dictation`, `VoiceEvent::Typed`) and the card
  shows it. A conversation the wake word began types no line breaks, as
  `type_text` types none there without a card.
- **Typing without a window of ours** (`portal::paste_text`). The
  RemoteDesktop session that presses Ctrl+V also asks for the clipboard
  (`org.freedesktop.portal.Clipboard`), offers the text as its contents and
  hands it over when the focused application asks: no helper window, no
  focus, no activation token. The helper-window paste stays as the fallback
  where the desktop has no clipboard portal.
- **What is on Karma Yoga's screen** goes to a conversation from the talk
  key or the wake word too: the window, if it is showing, sends its
  `screenNow()` when told the conversation began (`karmayoga://talk`), so
  "that article" can be the one open in the Feed.

## 5. Writing by voice (Nov 24 2026)

Hari's follow-up: select text anywhere and say "improve this", "simplify
this", "make this concise", "correct the grammar" or any other change; and
"write that I'm not feeling well and will reach out tomorrow" for a proper
message in his voice. "Type" is his exact words; "write" is a message made
properly, shaped by whether he says email, Slack or message.

- **Reading the selection without focus** (`portal::copy_selection`): the
  RemoteDesktop session with the clipboard presses **Ctrl+Insert** -- copy in
  GTK, Qt, browsers, Electron and terminals, where Ctrl+C would interrupt
  what runs -- waits for the clipboard to change hands and reads it with
  `SelectionRead`. The field keeps its selection, so the paste after it
  replaces it. Elsewhere, writing help's own selection read. Behind
  `Typist::selected_text`.
- **The model's tools** (`agent.rs`): `rewrite_selection(how, instruction)`
  (improve, simplify, concise, grammar, or other in their words) and
  `write_message(what, kind, to)` (email, chat, text, other), offered beside
  `type_text`. The typing note says which is which. Neither hands the
  selection to the model.
- **The writing** (`deskwrite`): a rewrite is writing help's own editor
  prompt and layout check on the medium model (new `WritingAction::Simplify`
  and `Custom` with `WritingAssist::instruction`); a message is written in
  his voice from mail's voice machinery (style guide, learned notes, his own
  "how I sound") where an account allows writing, plainly warm otherwise,
  with kind rules: an email has a greeting and sign-off, a chat message and a
  text have none. Both kept as `WritingAssist` records like the popup's.
- **The same typing rule**: what `type_text` would put on a card -- a
  stranger's words in the turn, or a line break in a woken conversation --
  these two hand back for `type_text` instead of typing (`TypingRule`,
  sharing `typing_held` with the gate). With no clear spoken request (maybe
  the assistant's own voice), `rewrite_selection` reads nothing at all; and
  it reads the selection again before typing, typing nothing if it moved.
  Every voice rewrite and message is kept and settled (`deskwrite::settle`):
  typed, handed back (pending) or abandoned.

Checked in the headless GNOME 50 shell: "their going home tomorow" selected
in gnome-text-editor read back exactly through the portal right after the
one dialog, a rewrite pasted over it replaced exactly the selection, nothing
selected read as empty, and Ctrl+Insert into gnome-terminal left a running
`sleep` alive.

## Ownership (Nov 23 2026, five Sonnet agents)

| Agent | Owns |
| --- | --- |
| A detector | `speech/voicing.rs`, `speech/detect.rs` |
| B session | `voice/session.rs`, `voice/phrases.rs`, `voice/assistant.rs`, `docs/plans/voice.md` |
| C tools | `agent.rs` (service), `karmayoga-core/src/agent.rs` (`SPOKEN_NOTE`), new `tests/voice_dictate.rs` |
| D shell | `voice/cue.rs` (`listening_chime`), `audio/engine.rs`, `audio/playback.rs`, `talk/*.rs`, `wake.rs`, `docs/plans/wake-word.md`, `docs/plans/panel.md` |
| E clipboard | `portal.rs`, `voicetyping/paste.rs`, `docs/plans/voice-typing.md` |
| supervisor | the contract (`voice/mod.rs`, `Tuning`, stubs), the UI, integration |
