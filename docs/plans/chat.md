# The assistant, from any chat app

> **Delivered: phase 1, Matrix.** Read this for the reasoning, and the
> commits for what was done. The build went differently from the plan below,
> or settled things the plan was silent on, in these places. Each was for a
> reason worth keeping.
>
> **Signing in**
>
> - **A newly typed password means "sign in afresh".** `save_chat_link`
>   stores a password without dropping the saved session. When the connector
>   finds a password, it ignores the session and empties the link's store.
>   It then logs in as a new device and recovers the encryption identity from
>   the saved recovery key. That is the only way out of a refused token. The
>   cost is that each fresh sign-in leaves the old "Every Day assistant"
>   device listed on the account; nothing logs it out yet.
>
> **The Matrix connector**
>
> - **A room is private only if nobody else is invited either.** An
>   outstanding invitation is a reader in waiting, since a room's history is
>   usually visible to whoever joins it later.
> - **Strangers get silence, not a refusal.** A reply would confirm to anyone
>   who found the bot that there is an assistant behind it.
> - **The owner gets one notice per room per run** when they write somewhere
>   that is not private.
> - **What is dropped or simplified:**
>   - A plaintext message in an encrypted room is dropped, because no client
>     of the owner's could have sent one.
>   - Edits are skipped.
>   - Replies lose their quoted fallback.
> - **The greeting waits until the bot knows the owner is in the room.**
>   Otherwise a freshly created room's key is shared with nobody, and the
>   owner sees an undecryptable first message.
> - **The connector retries sync failures itself:** five times, from 1 s up
>   to 16 s, before giving up with `Transient`. The SDK does not retry plain
>   network failures.
> - **Long replies are split at about 16 KiB.**
> - **`.well-known` lookups are switched off.** The homeserver address is
>   used exactly as typed.
>
> **The relay**
>
> - **Only `!stop` jumps the queue.** `!new` and `!help` wait their turn like
>   anything else.
> - **The stale rule applies to commands too.** A twelve-hour-old `!stop` is
>   not obeyed.
> - **After `!stop`, "Stopped." is said once,** by the dispatcher straight
>   away, not again when the turn unwinds.
> - **"Later" is read as "No"** on a question that could not be parked.
> - **The bot stops showing it is typing while a question waits.**
> - **The relay stops when the connector stops listening,** not only when the
>   supervisor says so. A connector's event handlers can keep the inbox open
>   after their sync has ended.
>
> **The service**
>
> - **`reconcile` leaves alone a task that is already up.** Saving always
>   restarts that link, through `restart_link`.
> - **The status shown is the connector's report, corrected by the
>   supervisor:** `Backoff` reads as `failing`, with its error.
> - **A Postgres-backed vault keeps a link's store under
>   `<config dir>/chat/<blake3 of the vault path>/<link>`.** BLAKE3 rather
>   than mail's `DefaultHasher`, because a hash that changed would orphan the
>   bot's device keys, whereas mail's index can be rebuilt.
>
> **The Rust version**
>
> - **Raising `rust-version` to 1.96 turned on the clippy lints for newer
>   language features.** These are let-chains, `is_multiple_of` and
>   `as_chunks`. About forty places across the workspace were rewritten to
>   match (by `clippy --fix`).
>
> **Still open**
>
> - **Saving a link that is backing off can take up to five seconds.** The
>   supervisor's backoff sleep does not watch the stop signal, so `stop`
>   waits out its grace period.
> - **CI does not run `make test-matrix`.** It needs Conduit in Docker. Run
>   it by hand before changing the connector.

## Why

The assistant lives on the machine that holds the vault. A phone should be
able to ask it things — "what's on tomorrow", "reply to Sam and say yes",
"add milk to the list" — without this application having to be built,
signed and reviewed for an app store. A chat service already has excellent
apps on every platform, and push notifications that work. So the assistant
joins one as a bot.

The person behind this runs their own Matrix homeserver (Conduit) on the same
machine, reachable from their devices over Tailscale. Matrix is first.

## What was decided, and why

- **A bot on a chat service, not a chat server inside this app.** The
  service's apps solve reaching a laptop behind a home router, and they solve
  iPhone push, which only an app's own maker can send. The bot connects out
  to the homeserver; nothing listens on a new port.
- **One small trait of our own, not a library.** Nothing in Rust abstracts
  several chat services and is mature — the candidates are 0.1 releases with
  no users, and ZeroClaw's good `Channel` trait is welded to its own runtime.
  [`Connector`](../../crates/everyday-chat/src/connector.rs) is modelled on
  ZeroClaw's and holds only what the relay uses. Matrix itself is also a way
  to reach other networks later (mautrix bridges bring Telegram, Signal and
  WhatsApp into Matrix rooms), with one caveat: bridged senders cannot be
  checked the way a Matrix user can.
- **It answers one Matrix ID, and nobody else.** The person chose this over
  also requiring verified devices. Encrypted rooms are supported, not required.
- **One complete reply per turn.** While the assistant works, the bot shows
  that it is typing. Then it sends one Markdown message. Editing a message
  as text arrives would put only the first fragment in the phone's
  notification.
- **The idle timer does not stop the bot.** While any chat link is switched
  on, the vault keeps its key past `forget_key_seconds`, the way it already
  does when the key is in the keychain. The window's own screen lock still
  hides the window. Locking by hand (Ctrl/Cmd+L, the tray) stops the bot like
  everything else, and it starts again on unlock.
- **Chat turns always confirm destructive tools**, whatever
  `confirm_destructive` says on the desktop: a phone is a likelier thing to
  lose than a desk.
- **Rust 1.96.** `matrix-sdk` 0.19 is the first release on `rusqlite` 0.40,
  the version the rest of the workspace links, and the two cannot differ
  (`links = "sqlite3"`). 0.19 needs Rust 1.96; the workspace's
  `rust-version` says so.

## The pieces

```text
 phone / Mac (Element)                         this machine
 ┌──────────────┐     ┌──────────┐     ┌───────────────────────────────────────┐
 │ owner writes │ ──▶ │ Conduit  │ ◀── │ everyday-chat                         │
 └──────────────┘     └──────────┘     │  matrix::MatrixConnector  (one link)  │
                                       │        │ Incoming      ▲ send/ask/typing│
                                       │        ▼               │               │
                                       │  relay  ── chat_turn ──▶ everyday-service
                                       └───────────────────────────────────────┘
```

| Where | What |
|---|---|
| `everyday-core/src/chat.rs` | `ChatSettings`, `ChatLink`, `ChatService::Matrix(MatrixLink)`, `ChatSecret`, `Via`. |
| `everyday-core/src/vault/chat.rs` | `chat_settings`, `save_chat_settings`, `chat_secret`, `save_chat_secret`, `delete_chat_secret`. |
| `everyday-core` `AgentStore` + `everyday-store-sql` | `chat_settings` / `put_chat_settings`: one sealed singleton row in a new `chat_settings` table (schema v13), shaped like `meeting_settings`. |
| `everyday-core` `Conversation.via` | Which chat a conversation arrived through. Absent for every other conversation. |
| `everyday-core` `Vault::set_kept_for_chat` | The idle-timer rule above. |
| `everyday-service/src/chat.rs` | `ChatRunner` (installed by the shell), `ChatHub`, `ChatState`/`ChatStatus`, `reconcile`, `state_dir`, `store_passphrase`, `Service::chat_turn`. |
| `everyday-service/src/domains/chat.rs` | The commands below. |
| `everyday-service/src/agent.rs` | `Turn.via`: the context sentence, and always-confirm. |
| `everyday-chat/src/connector.rs` | The trait every service implements, and its vocabulary. |
| `everyday-chat/src/relay.rs` | Everything between a chat and the assistant that is the same on every service. |
| `everyday-chat/src/lib.rs` | `Runner` and `run_link`: one link's life, from sign-in to stop. |
| `everyday-chat/src/matrix/` | The Matrix connector, on `matrix-sdk` 0.19. |
| `ui/` | Settings → Chat; "via Matrix" in the assistant's history. |
| `everyday-app`, `everyday-cli serve` | `service.set_chat_runner(Arc::new(everyday_chat::Runner))`. |

## Storage

- **`ChatSettings`**: every link, as one sealed record. It changes only when
  somebody saves Settings. The chat-to-conversation map lives on the
  conversations themselves (`Conversation.via`), not here.
- **`ChatSecret`**: in the secret store, under owner kind `"chat"` and the
  link's id. It holds the password (dropped once a session exists), the
  service's session as opaque JSON, and Matrix's recovery key.
- **A link's local state**: under `<vault>/store/chat/<link id>/` (see
  `state_dir`). For Matrix this is matrix-sdk's SQLite stores, encrypted with
  a passphrase derived from the vault's key
  (`vault.derive_subkey("chat-store:<link id>")`, hex). It is never written
  down anywhere.

## The service

`reconcile(service, vault)` is called on unlock (when the vault is
writable), after a save or a removal, and when the runner is installed. It:

- starts each switched-on link under the supervisor key `chat:<id>`;
- stops every other link that has a task;
- calls `vault.set_kept_for_chat(settings.any_enabled())`.

A save always restarts that link's task, so new settings take effect at once.

**Commands** (scope `Agent`, the scope of the assistant's own settings):

| Command | Effect | Arguments → returns |
|---|---|---|
| `chat_links` | Read | `{}` → `ChatLinkView[]` |
| `new_chat_link` | Read | `{}` → `ChatLink` (a fresh id, switched on, empty Matrix fields) |
| `save_chat_link` | Write, `Settings/Updated` | `{ link: ChatLink, password?: string \| null }` → `ChatLinkView` |
| `remove_chat_link` | Destructive, `Settings/Updated` | `{ id: ChatLinkId }` → `void` |
| `answer_chat_verification` | Write | `{ id, flow: string, answer: 'match' \| 'mismatch' \| 'cancel' }` → `void` |
| `start_chat_verification` | Write | `{ id: ChatLinkId }` → `void` |

`ChatLinkView` is `{ link, status: ChatStatus, signedIn: boolean, hasPassword: boolean, ownerVerified: boolean | null, verification: ChatVerification | null }`.
`ChatStatus` is `{ state: 'off' | 'connecting' | 'online' | 'needsSignIn' | 'failing', detail, since }`.

**`save_chat_link`**:
- Upserts by id, keeping the stored `createdAt` and stamping `updatedAt`.
- If the account changed (`ChatLink::same_account` is false), it drops the
  session, the recovery key and the link's state directory: the bot is now a
  different account.
- A password, when given, is stored in the secret.
- It then reconciles.

**`remove_chat_link`** stops the task, then deletes the link, its secret and
its state directory.

**Status.** A link's `ChatStatus` comes from what the connector last reported
(`Service::report_chat_status`), overridden by the supervisor's view:
- `Backoff` reads as `failing`, with its `last_error`.
- A link switched off, or with no task, reads as `off`.

Each report also raises `Change { kind: BackgroundTask, op: Updated, id: "chat:<id>" }`.

**`Service::chat_turn(via, conversation, prompt, sink)`** runs
`agent::run_turn` with `Turn { via: Some(via), on_screen: None,
unattended: None, drafting: None }` and the service's own `Pending`. It then
raises the same change events `send_message` does (the tools' writes, then
`Conversation/Updated`), stamped with origin `"chat"`. Inside `run_turn`, a
`via` does three things:
1. A missing conversation is created with `via` set.
2. The context says: "They are writing to you from {service} on another
   device, not in this app: they cannot see its screen, and links into the
   app will not open there. Keep replies short; Markdown is fine."
3. The confirm gate's `enabled` is forced on.

## The relay

`relay::run(service, vault, link, connector, inbox, stop)` runs until stop or
until the inbox closes.

**One turn at a time per chat.** Each chat has a worker task fed by a queue.
The dispatcher routes what arrives:

- **`!stop`** is handled at once, not queued. It cancels the chat's running
  turn (`service.pending().cancel(conversation)`) and declines a pending
  question.
- **While a question is waiting** for this chat:
  - `Chose { question }` on that question answers it.
  - A `Said` that `Choice::from_text` recognises answers it.
  - Any other `Said` declines the question, then is queued as a new message.
- **Everything else** is queued.

**Commands** (`!` rather than `/`, which Element keeps for its own):

| Command | What it does |
|---|---|
| `!new` | Forget this chat's current conversation; reply "Starting a new conversation." |
| `!stop` | As above; reply "Stopped." if anything was running, else "Nothing is running." |
| `!help` | List the commands, and say what the bot is. |

Unknown `!word`s are treated as ordinary text.

**Which conversation.** The current conversation for a chat is the most
recently updated conversation whose `via` names this link and chat, found by
scanning `vault.conversations(&ConversationQuery::default())` once per chat
and cached after. It is replaced by a fresh `ConversationId::new()` when there
is none, after `!new`, or when the last one was updated more than
`link.fresh_after_minutes` ago (0 = never). `chat_turn` creates it, marked
with `Via { link, service: link.service.label(), chat }`.

**A turn**:
1. `connector.typing(chat, true)`.
2. Run `service.chat_turn(...)` with a sink that forwards `AgentEvent`s to the
   worker over an unbounded channel. The sink is a synchronous `Fn`.
3. `ConfirmationRequired { call_id, subject, kind, can_park, .. }` becomes
   `connector.ask(chat, Question { markdown, choices })`.
   - The Markdown says what the assistant wants to do (`subject`), in words
     that suit `kind`: "outward" sends something to someone, "destructive"
     deletes, "search" / "fetch" leave the machine after mail was read.
   - The choices are Yes and No, plus Later when `can_park`.
   - The answer goes to `service.pending().answer(call_id, ConfirmAnswer::…)`.
   - After **30 minutes** with no answer, it is Later if parkable, else
     Decline, and the bot says which.
4. When the turn ends: typing off, then one message.
   - It is `Turned.text`; when that is empty, the concatenated `Delta`s.
   - Empty and stopped: "Stopped."
   - Empty otherwise: "(no reply)".
   - On `Err`: "Something went wrong: {message}". This also covers errors
     before the turn started, such as "the assistant is switched off".

**Catching up.** A message more than **12 hours** old when it arrives (the bot
was offline) is not run. The reply is: "This arrived while I was offline
{N hours} ago — send it again if you still want it."

**Unreadable** messages get: "I couldn't read that message — it was encrypted
for a session I don't have. Try sending it again."

## The Matrix connector

**Signing in** (`connect`):
- The client is built with `homeserver_url(link.homeserver)`; there is no
  discovery.
- It uses `sqlite_store(state_dir, Some(passphrase))`.
- Encryption settings: `auto_enable_cross_signing: true`,
  `auto_enable_backups: true`.
- If the secret has a session and the store directory exists,
  `restore_session` is used.
- Otherwise, with a password, the bot logs in with
  `initial_device_display_name` "Every Day assistant", then saves the session
  and drops the password from the secret.
  - A session without a store is discarded. A session is never paired with a
    fresh crypto store.
- With neither, it fails with `NeedsSignIn`.
- A refused login or an unknown token (`M_UNKNOWN_TOKEN`, `M_FORBIDDEN`) also
  fails with `NeedsSignIn`. Everything else is `Transient`.

**Encryption identity.** After signing in:
- If cross-signing is not set up, the bot bootstraps it (with the password,
  for user-interactive auth), then `recovery().enable()` and saves the
  recovery key.
- If cross-signing exists but this device is not verified, and a recovery key
  is saved, it runs `recovery().recover(key)`.
- So Element shows the bot as verified, and clients that withhold keys from
  unverified devices still share with it.

**Interactive verification with the owner** (`matrix/verification.rs`). The
identity above makes the bot's *account* vouch for its device. This is the
owner vouching for the bot, and the bot for the owner, by comparing emoji as
two people do in Element.
- **Only the owner's requests are taken up.** A request from anyone else,
  including another session of the bot's own account, is ignored rather than
  declined, for the same reason strangers' messages get silence.
- **Both kinds of request are heard:** to-device (verifying the bot's
  session) and in-room (verifying the bot as a user). They are heard from the
  same moment messages are, so a request left in a fresh login's backlog is
  not taken up.
- **The bot accepts with emoji (`m.sas.v1`) only.** It cannot scan or show a
  QR code.
- **The emoji are compared in Settings → Chat, not in the chat.** The
  comparison exists to catch something sitting between the bot and the
  owner, and a room message is exactly what such a thing could rewrite. So
  the connector reports what it sees as `ChatTrust` (the stage, the seven
  emoji, the owner's device, and whether the owner's identity is verified).
  It registers a `ChatVerifier` while it runs, and the pane answers through
  `answer_chat_verification { id, flow, answer: match | mismatch | cancel }`.
  When the owner started from their phone, the bot posts one line in the chat
  saying where its emoji are. That line contains no emoji.
- **The bot can also ask.** `start_chat_verification { id }` sends the owner
  a user-verification request in the bot's direct room. When the owner
  accepts, the bot starts the emoji.
  - The SDK picks that room from the account's `m.direct` list, and makes a
    new room when the list names none. That happens for a room the owner
    made in Element and invited the bot to.
  - So the bot first checks that the room the SDK would pick is its private
    room with the owner. If not, it marks the private room as direct and
    waits for the sync to bring that back. If it still isn't, the bot
    refuses rather than leave a new room behind.
- **One verification at a time.** A new request cancels the one before it.
  A flow that runs longer than 15 minutes is cancelled.
- **A bot that stops cancels its verification with the owner's device,**
  which would otherwise wait out the protocol's timeout. After it stops, it
  starts nothing new and reports nothing more. The service has already
  forgotten the link's trust, with a change event, so the pane drops the
  emoji.
- **Errors carry codes:** `conflict` when a verification is over, not ready,
  or cannot start; `network` when the server could not be reached.
- **`ownerVerified` follows the SDK's identity stream,** not a single read
  after "done". The first look runs beside the sync, so a slow `/keys/query`
  never holds up listening. The pane offers "Send a request" only for
  `false`, never for the `null` a freshly started bot reports before it has
  looked. The bot's signature on the owner's keys is uploaded a moment
  after both sides finish, and it counts only once the SDK reads the keys
  back. The same watch notices an owner who resets their keys.
- **Both commands are `Write` with no change event.** Nothing in the vault
  changes. They are still writes, because each one vouches for somebody's
  keys.

**Who it answers.** A message reaches `Incoming` only if all of these hold:
- its sender is `link.owner`;
- the room's joined members are exactly the bot and the owner (anything wider
  would show replies to others; the bot ignores such rooms and says so once);
- it is not the bot's own message.

Invitations are accepted only from the owner. If the bot has no room with the
owner after its first sync, it creates an encrypted direct room, invites the
owner and says hello:
> Hi — this is {name}. Ask me anything. `!help` lists commands.

**The first sync** runs `sync_once` with handlers already registered, but
events from before this device first signed in are skipped. Later restarts
resume from the stored sync token, so messages sent while the bot was offline
arrive, with their `origin_server_ts`.

**`ask`** sends the question, then reacts to it with each choice's emoji so
the owner can tap one. The owner's `m.reaction` on that event becomes
`Incoming::Chose`.

**`typing`** keeps `typing_notice(true)` renewed every few seconds until it is
turned off.

**Messages** go out as `RoomMessageEventContent::text_markdown`.

**Status** reports:
- `Connecting` while signing in;
- `Online` with "Signed in as {bot}. Talking to {owner}." (or "Waiting for
  {owner} to accept the invitation.");
- `NeedsSignIn` with the reason.

## Testing

- Unit tests for every pure decision: the choice parsing, the relay's routing
  of answers, the conversation choice, the catch-up rule and the room
  privacy rule.
- The relay end to end with a fake `Connector` and the fake model pattern
  from `everyday-service/tests/routines.rs`. This covers a reply, a
  confirmation answered by reaction and by text, a timeout, `!new`, `!stop`,
  and an old message.
- The service commands and `reconcile` with a fake `ChatRunner`.
- `everyday-chat/tests/matrix_live.rs`, ignored unless
  `EVERYDAY_TEST_MATRIX_URL` names a throwaway homeserver with open
  registration. It registers a bot and an owner, and runs the whole path over
  encrypted Matrix with the fake model. Scenario 4 covers verification:
  - a stranger's to-device request is ignored;
  - the owner verifies the bot, and Settings shows the same seven emoji and
    answers "match";
  - the bot asks from Settings, and "mismatch" cancels it on both sides.

  `make test-matrix` starts a
  throwaway Conduit 0.10.12 in Docker and runs it; never point it at a real
  server (it refuses an address that is not loopback or private unless told
  otherwise).

## Not in phase 1

Images, files and voice notes (the speech pipeline could transcribe them
later). Messages typed on the desktop do not echo into the chat. Bridged
rooms. Other services.

## Routines in the chat (phase 2)

A routine can do real work while nobody is at the desk, and it needs two
things a chat gives it: a place to send its result, and someone to ask when
it needs a decision. Each routine now has a `Delivery`:

- `chat`: the link whose private room gets the result;
- `ask`: whether to ask questions in that room while the run works;
- `email`: an account that sends the result to its own address.

Every run is still kept in the Assistant app, and the desktop notification is
unchanged. Delivery only adds places. The editor's "When it is done" section
sets these fields, and so do the assistant's `create_routine` and
`update_routine` (`send_to_chat`, `ask_in_chat`, `email_me`,
`email_account`).

**What was decided, and why**

- **Ask in Matrix only.** The owner chose this over also putting the question
  in the desktop app. Email cannot take an answer. A routine that delivers by
  email alone therefore asks nobody: it refuses, or parks as a proposal, as it
  always has.
- **Silence is never yes.** A question waits for the relay's 30 minutes. After
  that, an approval becomes later if the call can be parked and no
  otherwise. A free question comes back empty, and the model is told to carry
  on with the sensible reading and say what it assumed.
- **A routine that asks in a chat always asks before a destructive call**,
  whatever `confirm_destructive` says. This is the same rule as a turn from
  the chat: the answer comes from a phone.
- **The email goes to the account's own address and nowhere else.** The
  address comes from the account, never from a model. A routine the
  assistant set up cannot be talked into mailing somebody else.
- **A reply carries on the run's own conversation.** Posting the result
  adopts the run's transcript as the chat's current conversation. It also
  marks it with the room's `via`, so a relay started afresh still finds it.
  "Move it to Thursday" then has everything the run knew.

**How it fits together**

- **The relay registers a `ChatOutlet`** (`everyday_service::chat`) for its
  link while it runs, and withdraws it when it stops. A routine's message,
  approval or question arrives through it as a `RoutineJob`. The job is
  queued on the owner's private room (`Connector::home`) in turn with
  whatever that room is doing, so it waits for a turn the owner started.
  Their answer is routed exactly as an answer to a chat turn's question.
- **A free question takes whatever the owner types next as its answer.** A
  bare number picks a numbered choice. A command withdraws the question, and
  `!stop` also stops the run.
- **The relay stopping does not stop a routine.** It only tells the routine
  that nobody will answer.
- **The agent gets a `Reach`** on its `Turn`:
  - an `Asker`, which the scheduler builds over the outlet;
  - the places the reply will go, in words.

  With an asker, the confirmation gate asks instead of refusing or parking,
  and the run is offered `ask_owner`. That tool allows at most three
  questions per run.
- **The run's context says the reply will be sent,** so the model writes the
  reply as the message.
- **A run that can ask runs beside the scheduler's tick, not inside it**
  (`RoutineRuntime::detach`). A half-hour wait must not hold up the meeting
  watcher, the snooze sweep or the next routine. The run is claimed before
  the tick moves on, and its routine is held in flight until it ends, so
  the slot is neither started again nor recorded as missed.
- **Time spent waiting does not count toward the fifteen-minute run budget.**
  A two-hour ceiling still bounds the whole run.
- **A stopping scheduler, or a closing vault, withdraws every waiting
  question.**
- **What happened is recorded on the run.** `RoutineRun.delivered` holds one
  entry per place, saying whether the result got there or why not. Examples:
  "Matrix was not connected" and "the owner has not joined the bot's
  private room yet". A failure is delivered once per streak, as the
  notification is.
