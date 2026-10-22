# Pictures for the assistant

> **Delivered.** This is the record of what was decided and why; the code is
> `everyday_service::pictures`, `Turn::pictures` and `replay` in
> `everyday_service::agent`, `Incoming::Showed` and the worker's picture
> hold in `everyday-chat`, and the composer in
> `ui/src/components/ChatThread.svelte`.

Somebody can send the assistant a picture -- a receipt, a plant, a
screenshot of an error -- from the composer (the paperclip, a paste, or a
drop) or from their phone over a Matrix chat link, and ask about it.

## Decisions

- **Kept for as long as the conversation is.** Hari expects something like a
  hundred pictures a year. At a few hundred kilobytes each once made small,
  that is tens of megabytes a year in the sealed blob store, which is not
  worth a retention policy. A picture is deleted when its conversation is:
  garbage collection then finds nothing referring to it.
- **Made small once, on the way in.** `pictures::prepare` turns a picture
  the right way up, fits it inside 2048 pixels (the box OpenAI fits every
  picture into before looking at it), and encodes it again -- always, so
  a photograph's location and camera never reach the provider, or the
  vault. It is a JPEG if it came as one, and otherwise whichever of PNG and
  JPEG is smaller: a screenshot stays a sharp PNG, and a photograph saved as
  a PNG becomes a JPEG. Only the small copy is kept; the original the
  composer uploaded is collected like any unreferenced blob.
- **Refused, not kept as it came.** Unlike a cover, a picture that cannot be
  read (HEIC, SVG, not a picture) or is over 32 MB is refused, with a
  sentence, while the person is still choosing what to send -- the provider
  would only refuse it later. At most four go with one message.
- **On the message, not in a table of their own.** `Message.attachments` is
  the journal's own `Attachment` type, inside the sealed payload, so there
  is no schema step. Garbage collection walks it through
  `AgentStore::attachment_blob_refs`; without that walk the first sweep after
  a day would take every picture out of every conversation.
- **Shown again for three of the person's messages, then named.** Every
  turn replays the thread, and a picture costs a thousand tokens or so on
  every turn it is shown in. The picture's own message and the person's next
  two carry it (`PICTURE_TURNS`); after that the model is told that a
  picture by that name was sent there and is no longer shown, and to ask
  for it again if it needs it. Each picture shown counts as 4,000
  characters against the history budget.
- **A turn that shows a picture is held to the rules for one that has read
  mail.** A picture is often a screenshot of somebody else's words, and a
  model that has read them may have been told what to search for or which
  page to open. So while any picture is in what the model is shown, a web
  search or a fetch of an address stops to ask first (`mail_read_this_turn`).
  Nothing read off a picture becomes a trusted address either: provenance
  still comes only from what the person typed.
- **A model that cannot see says so.** The usual providers' refusals
  (OpenAI, OpenRouter, LM Studio, vLLM) are read by `llm::refused_pictures`
  and become "the model the assistant runs on cannot look at pictures".
  Some local servers drop the picture silently instead, and nothing can tell
  that from an answer. There is no "can see" setting per tier: the refusal
  is the signal.

## Matrix

The bot downloads a picture where it hears it, decrypting it if the room is
encrypted, within a minute -- inside the sync, so a photo and the question
typed after it reach the relay in the order they were sent. It is not kept
in the SDK's media cache: the vault's copy is the only one.

A picture with a caption is a turn at once. One without is held by the
chat's worker for what is said next (`PICTURE_WAIT`, a minute): a phone more
often sends the photo and then the question than both together. Further
pictures sent meanwhile are gathered with it; if nothing is said, they are
answered on their own. A command while pictures wait sends them first. A
picture never answers a waiting question -- it is the owner moving on. A
photo sent "as a file" (`m.file` with an image type) counts.

## Testing

- `pictures.rs`: sizes, the EXIF block gone, PNG kept for a screenshot,
  refusals.
- `tests/chat.rs` in the service: a picture reaches the model as an
  `image_url` part, is shown for three turns and then named, the rail's
  `prepare_picture` → `send_message` path, refusals before anything is
  written.
- The conformance suite: a picture in a conversation survives garbage
  collection, and goes once the conversation does.
- `everyday-chat`: routing and gathering as pure functions, the relay end to
  end with a fake connector, and `make test-matrix` sending an encrypted
  picture with and without a caption through a real Conduit.

## Not done

- The assistant looking at pictures already in the vault (a journal photo, a
  mail attachment, a cover). OpenAI's chat completions refuse a picture in a
  tool's result, so this needs the picture sent as a message of its own, or
  described by a separate call, and mail's access gate in front of it.
- PDFs and other documents.
- Tauri's own drag-and-drop is switched on (`dragDropEnabled`), which may keep
  a file dropped from the desktop from reaching the page's `drop` event on
  some platforms, as it may for the editor. The paperclip and paste do not
  depend on it.
