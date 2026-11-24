# The wake word: calling the assistant by name

Asked for on Nov 23 2026: "I want to configure a wakeup word for the assistant
so that I can speak to it and make it do work without having to press the
shortcut … I could say 'Siri, type …'. I will continue adding more voice
actions like this."

Switched on in Settings → AI features → On this computer → Wake word, the
microphone is listened to whenever nothing else needs it. Somebody who says
the name and then what they want -- "Siri, type I won't be available
tomorrow" -- gets the talk key's conversation (`panel.md`, "The talk key"),
with what came after the name as its first turn: the card in the corner, the
answer spoken, `type_text` typing where they were typing. The name alone
opens the conversation and listens. A conversation the name began ends after
15 s with nothing said (`talk::conversation::WOKEN_IDLE_AFTER`, the key's is
two minutes), without the "nothing was said" note, and the listening starts
again.

## Where it lives

| Part | Where | What |
| --- | --- | --- |
| Hearing the name | `karmayoga_service::speech::wake` | `Service::wake_open(names)`: voice mode's detector and recogniser (`live_transcription(Tuning::CONVERSATION)`) over the microphone; `heard(transcript, names)` looks for a name at the start of each utterance and answers what came after it (`Woken { rest }`). Anything said without the name is matched and dropped there: never sent, shown, logged or kept. Refused unless the chosen speech model is on this computer. A fresh detector every hour of audio (`FRESH_EVERY`), between utterances: Sherpa counts samples in an `i32`, which runs out after about 37 hours. |
| When to listen | `karmayoga-app/src/wake.rs` | A loop every 500 ms: `reason()` says why not to listen -- off, a remote vault, locked, something of ours has the microphone or is reading aloud (voice, capture, dictation and voice typing, Listen slots), a conversation going on (`talk::is_talking`), or another application on the microphone (`MicWatch::others_using`, the sound server's list as it is, not the call tracker's settled answer) unless "Stop listening during calls" is off. It never claims a slot, so it never refuses anyone the microphone. A refusal (no speech model, or one that is not on this computer, no name, the microphone failing) is shown in Settings and tried again a minute later. The setting is read from its file once and kept in memory; switched off, nothing else is looked at. Listening for the assistant's own name, it looks at the name every 15 s and starts again when it was renamed. |
| The conversation | `talk::wake(app, said)` | `Begin::Wake`: toggle posture, `Live::woke`, and once the session is open `voice::shell_heard` hands it `VoiceInput::Spoken(said)`. |
| First turn | `voice::session`, `VoiceInput::Spoken` | A turn on the words, `Said { clear: true }`: heard while nothing of ours was playing. Ignored while a turn runs or anything is being said. |
| What it may do | `agent::Turn::woken`, `ConfirmGate` | The whole conversation is `woken` (`Shell::woken` → `voice_open_typing(.., woken)`). Its words approve no change, so a delete or a calendar change is asked about; `type_text` types one line without asking and shows anything with a line break first, since a line break in a terminal or a chat is Enter. |
| Setting | `~/.config/karmayoga/wake.json` | `enabled` (off), `names` (comma-separated; empty = the assistant's own name from Settings → Assistant), `pauseInCalls` (on). Commands `wake_settings`, `set_wake_settings`. |

## Decisions

- **No wake-word model.** sherpa-onnx has keyword spotting, but its keywords
  are BPE tokens that need the model's tokenizer to write, and a fixed
  keyword set. The recogniser already loaded for voice mode hears any name,
  spelled any way, and gives the request in the same breath for nothing more.
  It costs a transcription per utterance heard near the microphone (Parakeet
  does a short one in under a third of a second on the CPU) and nothing while
  it is quiet: only the detector runs then.
- **The name must start the utterance**, after at most one greeting ("hey",
  "OK"). "I asked Siri yesterday" is talking about the assistant. A one-word
  name of five letters or more may be one letter off ("Carma" for "Karma");
  shorter ones must be heard exactly ("Sir" is not "Siri"). A name the
  recogniser spells differently can be added as a second name.
- **It steps aside rather than claiming.** Everything else that uses the
  microphone predates it and has its own exclusion; making each of them ask
  the wake word to stop would touch all of them. Looking every half second
  means they can share the microphone for that long, and the check is made
  again when the name is heard, so a name said in the first moment of voice
  typing is not acted on.
- **Calls.** While another application has the microphone it stops, by
  default, at once rather than after the four seconds the call tracker waits:
  what is said on a call is for the call.
- **Who it answers.** Anybody who says the name -- or a video, a podcast, a
  visitor -- gets what the talk key would give, less what only being at the
  computer earns: spoken approval of changes to tasks, events and notes is
  off, and typing more than one line shows the text first (Nov 23 2026, from
  review). A single line is still typed straight away, which is the point of
  "Siri, type I won't be available tomorrow". Answering only the owner's
  voice, from the voiceprint meeting notes already keep
  (`Voiceprint::is_owner`), is the next step, before more voice actions are
  added.
- **Only a recogniser on this computer.** Everything said near the
  microphone goes to the recogniser to find the few utterances with the name
  in them; a remote one would upload all of it, and bill for it.

- **A segment keeps 200 ms of what followed it** (`speech::detect`,
  `SileroVad`, `TAIL_SAMPLES`). Silero ended Kokoro's "Siri." before its soft
  last syllable and Parakeet heard "Si"; the quiet that ends a turn always
  follows a segment, so its first 200 ms are safe to keep. Voice mode,
  dictation and voice typing get the same, and the real-model voice tests
  still pass.

## Checked

A scratch probe (not committed) spoke six lines in Phonon and in Kokoro into
the real listener (`Service::wake_open`, the speech kit's detector and
Parakeet): "Siri, type I won't be available tomorrow. I can attend the meeting
on Friday." woke it with the whole request; "Hey Siri, what's on my calendar
this afternoon?" and "Karma, remind me to call Josh at five." woke it; "Siri."
woke it with nothing after; "I asked Siri about it yesterday" and "That was
really good karma, I think" did not. 12 of 12, after the tail above (11 before).

## Not done

- Not tried on a desktop with a real microphone and a real model behind the
  conversation.
- No sound when it wakes; the card is the only sign. A short chime would tell
  somebody who said only the name that it is listening.
- On Wayland, typing needs an activation token for the moment the paste
  window takes the keyboard, and the name brings none (an earlier key press's
  is cleared, not reused). GNOME gives a newly
  mapped window the keyboard anyway, which is what it would rely on;
  unverified.
- Media the webview plays (a podcast, a video) is not a reason to stop, as
  Listen's reading aloud is: it could be heard, and a name in it would wake
  the assistant.
- Not on macOS or Windows beyond what the shared code does; `MicWatch` is
  Linux only, so elsewhere calls do not stop it.
