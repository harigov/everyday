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
opens the conversation and listens, and says so with a short rising chime. A
conversation the name began ends after 15 s with nothing said
(`talk::conversation::WOKEN_IDLE_AFTER`, the key's is two minutes), without
the "nothing was said" note, and the listening starts again -- unless a
dictation is going on, which has the long wait. Saying "that's all" ends it
sooner, and the card says to.

## Where it lives

| Part | Where | What |
| --- | --- | --- |
| Jatayu's own model | `jatayu_service::speech::spotter`, `assets/wakeword/jatayu.jww` | When every name listened for is "Jatayu", `wake_open` listens with a trained spotter instead: see "Jatayu's own model" below. |
| Hearing the name | `jatayu_service::speech::wake` | `Service::wake_open(names)`: voice mode's detector and recogniser (`live_transcription(Tuning::CONVERSATION)`) over the microphone; `heard(transcript, names)` looks for a name at the start of each utterance and answers what came after it (`Woken { rest }`). Anything said without the name is matched and dropped there: never sent, shown, logged or kept. Refused unless the chosen speech model is on this computer. A fresh detector every hour of audio (`FRESH_EVERY`), between utterances: Sherpa counts samples in an `i32`, which runs out after about 37 hours. |
| When to listen | `jatayu-app/src/wake.rs` | A loop every 500 ms: `reason()` says why not to listen -- off, a remote vault, locked, something of ours has the microphone or is reading aloud (voice, capture, dictation and voice typing, Listen slots), a conversation going on (`talk::is_talking`), or another application on the microphone (`MicWatch::others_using`, the sound server's list as it is, not the call tracker's settled answer) unless "Stop listening during calls" is off. It never claims a slot, so it never refuses anyone the microphone. A refusal (no speech model, or one that is not on this computer, no name, the microphone failing) is shown in Settings and tried again a minute later. The setting is read from its file once and kept in memory; switched off, nothing else is looked at. Listening for the assistant's own name, it looks at the name every 15 s and starts again when it was renamed. |
| The conversation | `talk::wake(app, said)` | `Begin::Wake`: toggle posture, `Live::woke`, and once the session is open (`talk::opening`) `voice::shell_heard` hands it `VoiceInput::Spoken(said)`, or, for the name alone, `voice::shell_chime` asks it for `VoiceInput::Chime`. |
| The sound | `voice::cue::listening_chime`, `audio::engine` | The session turns `VoiceInput::Chime` into `VoiceOutput::Chime` once it is listening; the transport plays the sound on the speaker (`AudioEngine::chime`, `PlaybackQueue::play_chime`). |
| First turn | `voice::session`, `VoiceInput::Spoken` | A turn on the words, `Said { clear: true }`: heard while nothing of ours was playing. Ignored while a turn runs or anything is being said. |
| What it may do | `agent::Turn::woken`, `ConfirmGate` | The whole conversation is `woken` (`Shell::woken` → `voice_open_typing(.., woken)`). Its words approve no change, so a delete or a calendar change is asked about; `type_text` types one line without asking and shows anything with a line break first, since a line break in a terminal or a chat is Enter. `write_message` and `rewrite_selection` keep to the same rule (`TypingRule`, one `typing_held` with the gate): what they would type with a line break is handed back for `type_text`, whose card shows it; a dictation types no line breaks at all. |
| Setting | `~/.config/jatayu/wake.json` | `enabled` (off), `names` (comma-separated; empty = the assistant's own name from Settings → Assistant), `pauseInCalls` (on). Commands `wake_settings`, `set_wake_settings`. |

## Jatayu's own model (Nov 28 2026)

Asked for on Nov 28: "train a wake word identifying model for jatayu as the
agent name. It should work efficiently and cross platform. Generate maybe a
few hundreds of sample voices." For a name it was trained for, the wake word
no longer writes down everything the microphone hears.

- **What listens.** `speech::spotter::Spotter`: openWakeWord's frozen front
  end (a mel spectrogram and Google's speech embedding, Apache-2.0) and a
  small head trained for "Jatayu" (16 embeddings, 1.28 s, through two layers
  of 128). One `.jww` file holds all of it, built into the binary (3.3 MB).
  Pure Rust with `matrixmultiply`: the embedding network is convolutional in
  time with three pools of two, so the window moving by one 80 ms step moves
  each layer by whole rows, and each layer keeps the rows it still needs and
  works out only the new ones -- the same numbers as embedding all 76 frames
  afresh, at 0.49 ms a step (0.6% of one core, release build, i7-12700F).
  tract ran the same network at 9.4 ms a step and was dropped.
- **How it wakes.** `wake::spawn_spotted` (`speech/wake/spotted.rs`, a
  build with `speech`): every block goes to the voice detector and, inline,
  to the spotter. An utterance the detector closes is looked at only if the
  spotter heard the name in its first 2 s (`take_hit`; a hit up to 0.5 s
  after a short utterance's end belongs to it, and one just after an
  utterance cut at its longest is left for the next); nothing else is
  written down. The recogniser is loaded as listening starts, as the
  recogniser's way loads it, and again on a hit if it was let go idle.
  `after_name`: with less than 0.4 s of utterance after the hit and no
  syllable begun in it (`syllable_after`: loudness falling under a fifth of
  the loudest and rising past a third, for "Jatayu, stop" heard late), the
  name was said alone and nothing is written down at all. Otherwise the
  whole utterance is heard: the name at its start, after a greeting or one
  of the fillers training used ("um", "so"), gives the request by the
  recogniser's own split (it is told to expect the name); written later on,
  it was talked about; written another way (`opens_like_name`: near it run
  together, "Jet a you", within a letter in three, or with the name's end
  among the first four words, the "you" Parakeet kept when it wrote the rest
  as other words: "Got our, you", "Jot are you?", "Hi Jetter, you"), what
  came after the hit is heard on its own, a leftover "you" dropped
  (`without_name_tail`). Written as nothing like it -- Parakeet left the
  name out of about a fifth of the recordings that called it ("Take a
  note.") -- it wakes only if the spotter grew sure of the name (`SURE`: a
  step of 0.99 or more through the name, `Spotter::peak`; on the held-out
  speech such steps came 0.04 times an hour, against 0.31 at the 0.9 a hit
  needs), and what was written is the request. So a spotter's near thing
  over somebody else's "Jacuzzi, set the timer" sends nothing. Asking the
  recogniser to agree instead woke for 35, then 53, of 60 recordings. With
  the owner's voice check on, the name alone is written down too. The
  hourly fresh detector leaves the spotter alone (it has no counter to run
  out, and a fresh one hears nothing for 2 s): hits are counted from the
  start, the detector's utterances from its last reset. A spotter made
  afresh -- each `wake_open` -- hears nothing for its first 2 s.
  `wake_open` uses it when every name is one the built-in model hears; any
  other name, or a list mixing one in, listens the recogniser's way as before.
- **Training** (`scripts/wakeword`, data in `~/.cache`, not the repo).
  `gen.py`: 29,343 clips from 1,237 synthetic voices -- Piper's LibriTTS-R
  (904 speakers), VCTK (109), ARCTIC (18) and L2-ARCTIC (24, Hindi-accented
  ones among them), Kokoro's 28 English and 4 Hindi voices and 150 blends of
  them -- 1,109 voices to train on and 128 kept apart to test with. Every
  voice is handed the name as IPA, since eSpeak reads "Jatayu" as
  "JAD-ay-oo": juh-TAA-yoo, juh-TAH-yoo, jaa-TAA-yoo, JUH-taa-yoo, and for the
  Hindi voices a retroflex t. Alone, after hey/okay/hi/um/so, or followed by
  a request; sound-alikes ("Jacuzzi", "got to you", the name cut short,
  "hey Siri") and ordinary sentences in the same voices. `features.py`:
  each clip in a simulated room (pyroomacoustics), under speech, podcasts,
  babble or noise at 0-25 dB, at -42 to -14 dBFS, sometimes high- or
  low-passed; the streaming features exactly as the app computes them
  (checked: GPU against onnxruntime step by step 8e-5, Rust against Python
  2e-3). 209 h of real speech as what is not the name: LibriSpeech,
  People's Speech and podcasts. `train.py`: hard-negative mining twice over
  (what two earlier heads wrongly liked).
- **The name with one sound changed is not a sound-alike.** "chuh-TAA-yoo",
  "guh-TAA-yoo", "juh-KAA-yoo" and "juh-TAY-yoo" were trained as negatives at
  first and woke it 66-94% of the time anyway; they are how people say the
  name, and are left out of training both ways.
- **Measured** on the 128 held-out voices, in noisy rooms, and 22.6 h of
  held-out real speech (LibriSpeech test, People's Speech, three podcasts),
  at the threshold shipped (0.9): 81.5% of names caught, 0.31 false wakes an
  hour, 2.0% of real-word sound-alikes woke it. In a quiet room, 120 of 120.
  Through `wake_open` with the real detector and Parakeet
  (`tests/wake_spotted_real.rs`): woke for 120 of 120, the name alone with
  nothing after it 57 of 57, the request after it right 57 of 63 (the misses
  Parakeet's mishearings: "Open my nose.", "Yeah."). The app's spotter and training's code agreed on
  600 of 600 noisy recordings (`spotter::tests::the_name_is_heard_in_recordings`).
- **Found on the way:** Parakeet wrote nothing at all for some utterances
  cut exactly at their speech (the detector's segment of a clean recording).
  An utterance said to the app (`local::Recogniser::transcribe_utterance`:
  voice mode, voice typing, the wake word) that Parakeet writes nothing for
  is heard again with a quarter second of silence after it. Only then, and
  only Parakeet: silence after every utterance changed words it had heard
  right ("Friday" became "Frida"), and meeting notes and the speed test in
  Settings → Models, which expect silence to come back empty, are heard
  once.
- **Not tried:** Hari's own voice and room, a real microphone, Qwen behind
  it (the end-to-end test runs Parakeet), macOS, Windows and phones (the code
  is pure Rust and shared). Licences of what it was trained on: THIRD-PARTY-
  NOTICES entry 69.

## Decisions

- **No wake-word model** (Nov 23; for Jatayu there is one now, above).
  sherpa-onnx has keyword spotting, but its keywords
  are BPE tokens that need the model's tokenizer to write, and a fixed
  keyword set. The recogniser already loaded for voice mode hears any name,
  spelled any way, and gives the request in the same breath for nothing more.
  It costs a transcription per utterance heard near the microphone (Parakeet
  does a short one in under a third of a second on the CPU) and nothing while
  it is quiet: only the detector runs then.
- **The name must start the utterance**, after at most one greeting ("hey",
  "OK"). "I asked Siri yesterday" is talking about the assistant.
- **Matched as it sounds, not as it is spelled.** A recogniser that does not
  know a name writes it the way it sounds, so both sides are compared as
  sounds (`wake::sound_of`): doubled letters once, a "c" as "k" before a, o
  or u and as "s" before e, i or y, a last "ie", "ey" or "i" as "y".
  "Jimmie" and "Jimi" are "Jimmy"; "Carma" is "Karma". A soft "g" stays: no
  recogniser wrote "Gimmy" for "Jimmy", and "Gimmie" is "gimme". A word of
  five letters or more may also be one letter off ("Karmas"), though never
  the first letter. Measured on 616 sentences of sound-alikes, this woke on 1
  where "any one letter off" woke on 108: every "Timmy…" and "Kimmy…" for an
  assistant called Jimmy. Shorter words must sound the same ("Sir" is not
  "Siri"). A name the recogniser spells some other way can be added as a
  second name.
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

- **A chime when only the name was said.** The card in the corner is easy to
  miss, and somebody who has said the name and nothing else is looking at
  neither it nor the screen: with nothing heard, they cannot tell whether
  they were heard or when to speak, and anything said before the session had
  opened was lost. So the session, once it is open and listening, plays two
  soft rising notes (`cue::listening_chime`: E5 then A5, 90 ms each, a
  quarter of a second in all, about -15 dBFS), and the 15 s is counted from
  then. Not for the key (the press is the sign), and not
  when there are words after the name (the answer is the sign). The chime is
  mixed over whatever is playing in the speaker callback rather than queued,
  so it is in the reference the echo canceller hears and is taken out of the
  microphone like speech, and it is not an utterance, so nothing is reported
  when it has played; `Clear` ends it. The session treats the moments after it
  as audio in the air, so it is never taken for the person.
- **The card says how to end it by voice.** The key's card says "Press
  Ctrl+Alt+A to end"; nobody who called the assistant by name has a key in
  their hand, so a conversation the name began says "Say “that's all” when
  you're done" (`card::Hint::Say`), which the session hears for itself
  (`phrases::is_closing`, `docs/plans/voice-reliability.md`). Muted, where
  nothing would hear it, it gives the key's line again.
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
- The chime has not been heard on a real speaker: its tests are of the
  sound's shape and of the queue it is mixed into.
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
