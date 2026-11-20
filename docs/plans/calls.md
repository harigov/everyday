# Calling

Call the assistant's Matrix account from FluffyChat on a phone, as you would
call a person, and talk to it. It answers, listens, does what it is asked and
talks back, and what was said lands in the same thread as what is typed in that
chat. It can also call you: a routine set to "Call me with it" rings the phone
with the run's result, says it, and stays on the line for whatever you ask
about it. If you don't pick up, the result is sent as a message instead.
Whatever runs the chat link answers: the desktop app, or `karmayoga serve` on a
machine with no screen and no sound card.

**Changed Nov 19 2026: MatrixRTC, not legacy calls.** The first version
(9d19ea0e) answered legacy one-to-one calls (`m.call.*`), on the belief that
FluffyChat made them. FluffyChat 2.10.0 (October 2 2026) had already removed
them for MatrixRTC, so its call button never appeared and nothing ever rang the
bot. The test that passed used a fake phone. What follows is the MatrixRTC
design; the wire format is what FluffyChat 2.11.5's own source sends.

## Decisions

- **MatrixRTC with LiveKit, as FluffyChat 2.10+ calls.** A call is a room on a
  LiveKit media server (an SFU): every participant sends its audio there and
  receives everybody else's from there. Joining is three things on Matrix:
  - an `org.matrix.msc3401.call.member` state event (state key
    `_@user:server_DEVICE_m.call`) saying this device is in the room's call,
    emptied to `{}` on leaving;
  - a LiveKit token from the homeserver's token service (`lk-jwt-service`),
    which checks a Matrix OpenID token: `POST {service}/sfu/get`;
  - this side's media key, sent to the caller's device as an Olm-encrypted
    `io.element.call.encryption_keys` to-device message.
- **Every frame is end-to-end encrypted**, always, as FluffyChat does
  whatever the room's encryption: AES-128-GCM, keys per participant derived
  with HKDF, the Opus TOC byte in the clear. The SFU never sees a key. Keys
  are accepted only Olm-encrypted from the owner (FluffyChat also takes
  plaintext ones; this side does not).
- **The token service is found where FluffyChat finds it**: the caller's
  membership names it (`foci_preferred`), and the bot joins the same one. For
  a call the bot places, its homeserver's MSC4143 `rtc/transports`, then
  `.well-known/matrix/client` `org.matrix.msc4143.rtc_foci`.
- **A call is a voice session with a different transport.** `voice::session`
  already takes 16 kHz mono and gives back audio and events; the desktop's
  microphone and speakers were its first transport and a call is its second.
  Nothing about listening, interrupting, spoken approval or what is kept in the
  thread is new.
- **The phone cancels its own echo.** Every phone does on every call, so the
  call transport needs nothing like the desktop's `sonora`.
- **Two connections to the SFU, as every LiveKit client keeps**: the
  *publisher*, which this side offers and sends on, and the *subscriber*,
  which the SFU offers and sends the caller's audio on. LiveKit signalling
  protocol 15, protobuf over a WebSocket, only the messages a voice call uses
  declared by hand (`call::signal`). str0m for both connections, as before.
- **No STUN, no TURN.** The SFU runs beside the homeserver on the tailnet and
  advertises its tailnet address; this side offers every address of every
  interface that is up as host candidates.
- **Opus only, at 48 kHz, decoded straight to 16 kHz.** No RED (FluffyChat
  turns it off when it encrypts). A video track is declined; this side asks
  for voice (`"m.call.intent": "voice"`), so FluffyChat answering one of the
  bot's calls would not start its camera.
- **Only the owner, only in the private room**, exactly as for messages. State
  events cannot be end-to-end encrypted, so a membership is trusted by its
  sender and room; the media then listens only to the owner's device's
  participant in the SFU room (whose identity the token service vouches for)
  and opens only frames that the owner's own keys open.
- **One call at a time, and never beside a typed turn.** A call is a work item
  on its chat's worker, like a message: it waits for a turn that is running,
  and a message typed during a call waits for the call. A second call while one
  is running is refused at once, with a sentence in the chat. A routine that
  would call while the line is busy is written instead, and says so.
- **A routine calls by ringing, then talking.** `Connector::ring` joins the
  room's call and sends FluffyChat's ring. Picking up is the owner's device
  joining, the same `Incoming::Called` as a call they placed. The voice
  session opens in the run's own conversation, says the result
  (`VoiceInput::Say`, no turn of its own), and then listens, so a follow-up
  is an ordinary spoken turn in that conversation. Nobody picking up within
  about 50 seconds stops the ring and posts the result. A run that failed is
  written, not rung through.
- **Approvals the voice cannot take are asked in the chat.** Spoken approval is
  as narrow as at the desktop (see `voice.md`). Anything else is asked as the
  usual question in the chat, with the reactions, and the session says "That
  needs your go-ahead in our chat." A tap or a typed yes answers it; if the
  voice or the turn settles it first, the question is withdrawn.
- **Failure is said in the chat**: a call that cannot be taken (no speech model
  or voice chosen, the vault locking), a token service or SFU that cannot be
  reached, a call whose media never connects.
- **No delayed events.** Conduit 0.10 has none (MSC4140), and FluffyChat does
  not use them, so a device that drops off without leaving keeps its
  membership until it expires (4 hours). A ringing call from a membership more
  than a minute old is never answered.

## Shape

```text
  FluffyChat ──call.member state / Olm keys──▶ matrix::rtc ──Incoming::{Called, CallKeys, CallEnded}──▶ relay
       │                                           ▲                                                  │
       │                                           └── Connector::join_call / end_call ◀──────────────┤
       │                                               (OpenID → /sfu/get, own membership, own key)   ▼
       └──▶ LiveKit SFU ◀── WebSocket + 2 × UDP (ICE, DTLS-SRTP, encrypted Opus) ── call::media ◀── VoiceLink ── calling
                                                                                                  ▲
                                                                          Service::voice_open_via ┘
```

| Piece | What it does |
|---|---|
| `connector.rs` | `Incoming::{Called, CallKeys, CallEnded}`, `Connector::{ring, join_call, end_call}`, `CallJoin`, `CallKey`, `HangUp`. Service-agnostic. |
| `matrix/rtc.rs` | Hears the owner's call memberships and keys; rings (own membership, then FluffyChat's notification); joins (OpenID token, `/sfu/get`, its own membership, its key to the owner's device) and leaves. |
| `relay.rs`, `calling.rs` | Routes a call to its chat's worker, buffers keys until the media runs, picks the conversation as for a message, opens the voice session, joins, carries approvals into the chat, and hangs up with the right reason. A routine's call rings, and on pickup opens by saying the result in the run's conversation. |
| `call/` | The SFU client: WebSocket and protobuf (`sfu`, `signal`), the two str0m connections in one task (`media`), frame encryption (`frames`), Opus in and out, and the paced outbound queue that honours `Play`/`Pause`/`Resume`/`Clear` and reports `Played`. |
| `Service::voice_open_via` | A voice session whose turns run as turns from the chat (`via`), spoken. |

## Setting it up

- In Settings → Models, choose a speech model and a voice, as for voice mode.
  Without them a call is refused with a sentence saying so.
- The homeserver needs a LiveKit SFU and `lk-jwt-service`, and has to say
  where the token service is. On Hari's server (runtipi's Conduit app) they
  are extra services in `user-config/migrated/matrix-conduit/docker-compose.yml`:
  - LiveKit with signalling through Traefik at `/livekit/sfu` and media on
    7891/tcp and 7892/udp at the tailnet address (`NODE_IP`);
  - the token service at `/livekit/jwt`, with the tailnet name pointed at the
    host so it can check OpenID tokens through Conduit's federation API —
    hence `ALLOW_FEDERATION=true`;
  - nginx answering `/.well-known/matrix/client` (with `rtc_foci`) and MSC4143
    `rtc/transports` in Conduit's place. FluffyChat caches the `.well-known`
    for three days but asks `rtc/transports` every time.
- The phone must be on the tailnet while calling.
- In the room, the owner needs the power to send
  `org.matrix.msc3401.call.member` for FluffyChat to show the call button. The
  bot needs the same, plus `org.matrix.msc4075.rtc.notification`, to ring.
- A routine calls when its delivery has "Call me with it" on (the routine
  editor, or `call_me` when the assistant sets the routine up). Both need a
  chat link.

## Testing

- Unit tests beside each piece; the relay's call handling against a fake
  connector and a fake call backend in `tests/relay.rs`; the SFU client
  against a WebSocket server that only lets it in (`call::media`'s tests).
- `make test-livekit` (`tests/sfu_live.rs`): two of this crate's clients in
  one room on a throwaway LiveKit, hearing each other's tones through it,
  encrypted; and a wrong key hearing nothing.
- The frame key derivation against an independent HKDF (`call::frames`).
- A call from FluffyChat on the iPhone to the real bot.
