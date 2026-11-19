# Calling the assistant

Call the assistant's Matrix account from FluffyChat on a phone, as you would
call a person, and talk to it. It answers, listens, does what it is asked and
talks back, and what was said lands in the same thread as what is typed in that
chat. Whatever runs the chat link answers: the desktop app, or `karmayoga
serve` on a machine with no screen and no sound card.

## Decisions

- **Legacy one-to-one Matrix VoIP (`m.call.*`, version 1).** It is what
  FluffyChat speaks: WebRTC between the two devices, with the session
  descriptions and candidates carried as room events. MatrixRTC (Element Call,
  LiveKit) is what Element X speaks instead; it needs an SFU, a token service
  and homeserver features Conduit 0.10 lacks, so it waits until somebody needs
  it.
- **A call is a voice session with a different transport.** `voice::session`
  already takes 16 kHz mono and gives back audio and events; the desktop's
  microphone and speakers were its first transport and a call is its second.
  Nothing about listening, interrupting, spoken approval or what is kept in the
  thread is new.
- **The phone cancels its own echo.** Every phone does on every call, so the
  call transport needs nothing like the desktop's `sonora`.
- **Direct connections only: no STUN, no TURN.** The phone reaches the
  homeserver over the tailnet, so it can reach the machine answering too. This
  side offers every address of every interface that is up (the tailnet's
  `100.x` among them) as host candidates, one UDP socket each (str0m accepts
  a packet only at the exact address of a candidate), and that is all. A phone that is not
  on the tailnet cannot reach Conduit either.
- **Opus only, at 48 kHz, decoded straight to 16 kHz.** libopus decodes to
  16 kHz itself; only the voice's own rate is resampled, to 48 kHz on the way
  out. Every Matrix client offers Opus.
- **A video call is answered with voice.** The video section of the offer is
  declined.
- **Only the owner, only in the private room**, exactly as for messages: an
  invite from anyone else, or in a room anyone else is in or invited to, is
  never heard. Invites that expired before the bot heard them are dropped, and
  the backlog rule for messages (a fresh sign-in does not answer history)
  holds for calls too.
- **One call at a time, and never beside a typed turn.** A call is a work item
  on its chat's worker, like a message: it waits for a turn that is running,
  and a message typed during a call waits for the call. A second call while one
  is running is refused at once, with a sentence in the chat.
- **Approvals the voice cannot take are asked in the chat.** Spoken approval is
  as narrow as at the desktop (see `voice.md`). Anything else is asked as the
  usual question in the chat, with the reactions, and the session says "That
  needs your go-ahead in our chat." A tap or a typed yes answers it; if the
  voice or the turn settles it first, the question is withdrawn.
- **Failure is said in the chat.** A call that cannot be taken (no speech model
  or voice chosen, the vault locking) is rejected and the owner told why in a
  sentence; a call that never connects is hung up and the owner told the phone
  must reach this computer directly.
- **No renegotiation yet.** `m.call.negotiate` (hold, or adding video) is
  ignored.

## Shape

```text
  FluffyChat ──m.call.invite / candidates / hangup──▶ matrix::call ──Incoming::Called…──▶ relay
       ▲                                                                                    │
       │                                           Connector::answer_call / end_call ◀──────┤
       │                                                                                    ▼
       └──────────── UDP: ICE, DTLS-SRTP, Opus ───────────▶ call::media ◀── VoiceLink ── calling
                                                                              ▲
                                                     Service::voice_open_via ─┘
```

| Piece | What it does |
|---|---|
| `connector.rs` | `Incoming::{Called, CallCandidates, CallEnded}`, `Connector::{answer_call, end_call}`, `HangUp`. Service-agnostic. |
| `matrix/call.rs` | Hears the owner's `m.call.*` events and sends the bot's: answer (with the stream metadata matrix-dart-sdk reads), reject, hangup. |
| `relay.rs`, `calling.rs` | Routes a call to its chat's worker, buffers candidates until it is answered, picks the conversation as for a message, opens the voice session, answers, carries approvals into the chat, and hangs up with the right reason. |
| `call/` | The WebRTC peer (str0m, sans-IO, one task per call), Opus in and out, and the paced outbound queue that honours `Play`/`Pause`/`Resume`/`Clear` and reports `Played`. |
| `Service::voice_open_via` | A voice session whose turns run as turns from the chat (`via`), spoken. |
| `speech::resample` | Moved down from the desktop app so a server without devices converts rates the same way. |

## Setting it up

- In Settings → Models, choose a speech model and a voice, as for voice mode.
  Without them a call is rejected with a sentence saying so.
- The phone must be on the tailnet while calling.
- Conduit's `CONDUIT_TURN_URIS` on the home server points at `turn:localhost`
  with a placeholder secret, and nothing listens there. Clients ask for TURN
  servers and try that one; it does no harm, since the direct path is the one
  that connects, but it is worth removing (or replacing with a real coturn)
  so a failed call is not blamed on it.

## Testing

- Unit tests beside each piece; the relay's call handling against a fake
  connector and a fake call backend in `tests/relay.rs`.
- The media over loopback against a second str0m peer playing the phone.
- End to end, `tests/call_live.rs`: a throwaway Conduit, the real runner,
  Parakeet and Phonon, and a str0m peer playing the phone over this
  machine's own address. The phone asks a question in Phonon's voice;
  Parakeet must hear the scripted answer in what comes back, and the thread
  must hold both. `#[ignore]` and built only with `--features speech`:

  ```text
  KARMAYOGA_TEST_MATRIX_URL=http://127.0.0.1:16167 KARMAYOGA_TEST_SPEECH=1 \
    cargo test -p karmayoga-chat --features speech --test call_live -- --ignored --nocapture
  ```

  Not yet tried against FluffyChat on a real phone.
