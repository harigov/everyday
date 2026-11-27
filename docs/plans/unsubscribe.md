# Unsubscribing, and mail that is not a newsletter

What Hari asked for on Nov 26 2026:

> Add support for unsubscribing to emails. It should be possible for
> newsletters, email bulk cleanup, or individual emails. Also add bulk
> unsubscribe support in feed app. I should also be able to mark bulk
> newsletters (or any other email categories) as not newsletters because
> many of them are incorrectly marked.

So, four things:

1. **Unsubscribe from one conversation**: a button on the thread, the row
   menu, the phone's thread menu, and Cmd/Ctrl+U.
2. **Unsubscribe in bulk** from Mail's list (picked conversations) and from
   Quick Cleanup (picked senders), each with "and archive" offered.
3. **Unsubscribe in bulk in the Feed**: a select mode over the sources, where
   unsubscribing asks each newsletter's sender to stop and takes it out of
   the Feed, and unfollows a web feed or podcast.
4. **"Not newsletters" in bulk**, in Mail's list (any tab, through "Move to
   category" and a "Not newsletters" button on the Newsletters tab), in
   Quick Cleanup ("Move to…"), and in the Feed (which can also move the
   sender's mail out of Newsletters).

## How unsubscribing works

A list's mail carries `List-Unsubscribe` (RFC 2369): links in angle brackets,
in the sender's order of preference, usually a `mailto:` and an `https:`.
Some add `List-Unsubscribe-Post: List-Unsubscribe=One-Click` (RFC 8058),
which says the https link takes a bare POST and needs no page.

The way out is chosen in one place, `jatayu_core::mail::unsubscribe`:

1. **One click** when the sender offered it and named an https link: a POST
   of `List-Unsubscribe=One-Click` and nothing else (no cookies, no
   sign-in, no referrer), through its own client in `http.rs` that stays on
   the open internet (`PublicOnly`, no proxy) and **follows no redirect**: a
   redirect after a POST becomes a GET of another address, which is the one
   thing this must never do.
2. **An email** to the `mailto:` address, from the address the list writes
   to (the account's own address among the message's `To`/`Cc`), queued
   through the outbox like any send, without the undo delay. Only the
   link's `subject` and `body` are used: a `to`, `cc` or `bcc` in a
   stranger's link is ignored, so it cannot add recipients to mail sent in
   the person's name.
3. **A page**, handed to the interface to open in the browser. It is never
   fetched in the background: a GET to an unsubscribe page can confirm a
   subscription or tell a tracker the mail was read, and link scanners doing
   exactly that are why RFC 8058 exists.

When one click is refused, the email is tried, then the page. A 301, 302
or 303 after the POST counts as taken (plenty of list servers answer with a
pointer to a "you're unsubscribed" page) and is not followed; a 307 or 308
asks for the POST again elsewhere, so it is a refusal. A page is opened
straight away only when the question said it would be (the sender offers
nothing else); after a refused one-click request it is listed instead.

A sender asked is remembered on the message used
(`ListUnsubscribe.asked_at`), so the thread says "Unsubscribed" and Quick
Cleanup says when, rather than offering it as new. Opening a page does not
count as asking: what happened there is unknown. An email that fails for
good takes the mark back: the draft carries `Draft.unsubscribes` (the
message it is about), and the outbox's give-up path clears `asked_at` the
way it reverts an RSVP's answer.

### Keeping the header

The header pass already fetched the whole header block, and the header was
parsed for categorising and then dropped. Now `Message.unsubscribe` keeps
what it offered (`links`, `one_click`), filled at ingest for new mail and
for an existing row seen again with none. A message with no usable header
is kept as an empty one, "looked, nothing offered", so only `None` means
"never looked". It is a sealed, optional field (`skip_serializing_if`), so
no schema step, and the frozen goldens do not change. `recategorize` now
reads the kept header as its `List-Unsubscribe` signal too.

Mail synced before this has no stored header. The service reads it back from
the message's own bytes in the pack store when it needs it
(`mail::unsubscribe::header_of`): the thread's offer, the senders in Quick
Cleanup (capped per call) and the unsubscribe itself. A frame is decrypted
whole, so a message over 1 MiB (somebody's file, not a list's) is never
read, and what a message's bytes said is remembered for the rest of the run
(`READ_BACK`), so opening the same conversation or Quick Cleanup again reads
nothing. A message whose body is not fetched yet has no bytes to read; on a
synced account that is only the middle of a first sync.

### What it does not check

RFC 8058 asks that both headers be covered by a DKIM signature. Nothing in
the application verifies DKIM. A forged header can therefore make a press of
Unsubscribe POST one fixed string to an address on the open internet, or
send one email from the person's address to an address in their own mail.
Both need the person's own press; neither can reach a private network.

## Not a newsletter

Mail files a message by rules (a list header makes it a newsletter), by
Gmail's own labels, and by the models. A person's correction is a sender
rule (`CategoryRules`) plus a sweep of that sender's mail, stamped
`CategorySource::Person`. Three fixes made it usable in bulk:

- **The wrong sender.** `set_thread_category` took the thread's newest
  message, which is often the person's own reply: it then recorded a rule
  for their own address and swept all their sent mail. It now takes the
  newest message that is neither a draft nor theirs.
- **One sweep, not forty.** A sweep decrypts every message the account has
  (the sender is sealed), and moving forty senders was forty sweeps.
  `MailStore::correct_categories` takes them all in one.
- **Corrections that stick.** New mail from a corrected sender used to be
  stamped `Rules`, so the Small-model and decision-model passes could move
  it straight back into Newsletters. It is stamped `Person` now, which those
  passes leave alone. The cost: a corrected sender's new mail is not sorted
  into one of the person's own categories either.

`set_sender_category` re-files senders named outright (Quick Cleanup's rows,
one per account a sender writes to).

In the Feed, "not newsletters" turns the sources down (one save of the
Feed's settings for any number), removes them, and with a chosen category
moves their sender's mail there in Mail -- unless another newsletter that
stays in the Feed comes from the same address (MIT Technology Review sends
The Download and The Algorithm from one).

## Commands

| Command | Scope | Effect | What it does |
|---|---|---|---|
| `unsubscribe_offer {thread}` | Mail | Read | What unsubscribing from the conversation's sender would do |
| `unsubscribe {threads, senders?}` | Mail | Write | Ask each sender; one outcome per sender |
| `set_sender_category {senders, category}` | Mail | Write | Re-file senders and keep their new mail there |
| `unsubscribe_news_sources {ids}` | News (+Mail for newsletters) | Destructive | Ask newsletters' senders, remove them; unfollow feeds and podcasts |
| `mark_not_newsletters {ids, category?}` | News (+Mail with a category) | Destructive | Turn down and remove; optionally re-file their mail; says how many messages moved and how many newsletters kept theirs |

`unsubscribe` and `unsubscribe_news_sources` want the vault unlocked: the
lock screen is not a place requests leave the machine from.

## Delivered (Nov 26-27 2026)

Built by six agents against a contract laid first (core types, the stored
field, store methods, command stubs, TS types, the shared results sheet),
then reviewed. The review's eight findings are folded into the sections
above: the "looked, nothing offered" marker and `READ_BACK` (opening an
ordinary thread used to decrypt several whole messages every time), 301-303
taken as a yes, pages opened only when promised, the failed email taking
its mark back, the Feed command wanting an unlocked vault, the report from
`mark_not_newsletters`, Mod+U kept out of Sent and Drafts, and the results
sheet keyed by place. In the thread view Unsubscribe is a link in the
subject's line, beside "Not a newsletter?", rather than a toolbar pill,
which pushed Summarise onto a second row at an ordinary window width.

Tried in the mock interface in a headless browser (thread, bulk, Quick
Cleanup, the Feed's select mode, a phone-width Quick Cleanup); not tried on
a real account or a real list server.

## Left for later

- An assistant tool ("unsubscribe me from this"): it would be Outward,
  confirmed every time, and needs a network hook into core.
- Unsubscribe on the panel's new-mail card.
- A Settings page listing the sender rules a person has made, with a way to
  take one back (`CategoryRules` has no remove).
- An unsubscribe email lands in Sent and teaches the contact book the list's
  address, as any sent mail does.
