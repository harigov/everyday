# Templates for the journal, notes and meeting notes

> **Delivered** on `clumsy-lynx` (Nov 13 2026). Hari's calls:
>
> - **One record for all three.** Meeting templates and journal or note
>   templates are the same `Template`; where each is offered is a field on it.
> - **A day's template appears once an entry is started**, never as a preview
>   on an empty day. A page holding only its template text is not saved.

## The record

`karmayoga_core::template::Template`: a name, `uses` (`journal`, `notes`,
`meetings`), an optional title pattern, a Markdown body and starting tags.
It is sealed in its own `templates` table (schema v27, the shape of `skills`),
reached through `NoteStore`. So `supports_templates` is the same as
`supports_notes`.

**One rule for the body: headings are kept, and the text under a heading is
guidance for whoever writes that section.** In a meeting note the model writes
it, as before. In an entry or a note the text goes in as written, and the
person answers it.

## Placeholders

- **Every template** gets the day's: `{{date}}`, `{{weekday}}`, `{{day}}`,
  `{{month}}`, `{{year}}`, `{{week}}` (ISO) and `{{quarter}}`.
  - Any of them takes an offset with `d`, `w`, `m` or `y`: `{{date+7d}}`.
  - Only `{{date}}` takes a format: `{{date:%a %-d %b}}`, using jiff's strftime.
- **Journal only:** `{{journal}}`.
- **Meetings only:** `{{title}}`, `{{start}}`, `{{end}}`, `{{duration}}`,
  `{{organizer}}`, `{{attendees}}`, `{{present}}`, `{{calendar}}`.
- In a meeting, the day's placeholders use the meeting's own date, in its
  time zone.
- A placeholder the place does not know is left as typed. `template::lint`
  says so when a template is offered somewhere that would show it as typed.

## Choosing one for a day

`Journal.templates` is a `JournalTemplates`: a `default` and ordered `rules`.
The first rule that matches the entry's date wins; otherwise the default;
otherwise a blank page. A rule's `When` can be:

- days of the week;
- every N weeks, counted from a chosen day's week (not ISO parity, which goes
  wrong in years with a week 53);
- a day of the month, or the last day;
- the Nth weekday of the month, or the last one;
- a date each year.

A rule or default naming a deleted template is passed over, not pruned. A rule
applies whatever the template's `uses` says: `uses` only filters the pickers.

## Where it is applied

- **`new_entry`** fills in the journal's template for the new entry's date. In
  the interface (`state.svelte.ts`), a page that came back with text is held
  unsaved until the first edit. An untouched page that is left is dropped, so
  the list, insights and dreams never see a day nobody wrote. A blank page is
  still saved at once, as before. While a page is held:
  - `#untouched` (nothing typed yet) decides whether to write at all, and
    `#unstored` (the vault does not have it yet) lasts until the first save
    lands.
  - Readings and the assistant's view of the screen get no entry id
    (`storedEntryId`), so nothing points at a page that may never exist.
  - The list keeps the page selected only while the list is one it belongs in.
  - The first save checks whether the day was started elsewhere meanwhile (in
    another window, or by the assistant). If it was, the page is added to the
    end of that entry rather than filed as a second one.
- **The assistant's `create_entry` with a template and no body** writes a page
  that holds only the template. That is deliberate: it is what was asked for
  ("start today's page from my template").
- **`new_note { templateId }`**: "New note from template" in the notes sidebar.
  Saved at once, because the template was picked by name.
- **Insert template** in the editor toolbar of both the journal and notes:
  `render_template`, inserted at the caret. A title is used only if there is
  none yet, and tags are added to the ones already there.
- **Meetings.** `Vault::meeting_template(asked)` picks the template asked for,
  then `MeetingSettings.default_template`, then the first template offered for
  meetings, then the built-in starter.
- **The assistant:** `list_templates` (Notes domain), and a `template`
  argument on `create_entry` and `create_note`. On `create_entry`, `"journal"`
  means "the one this journal uses for that day".

## Moving the meeting templates

Older builds kept meeting templates inside the sealed `MeetingSettings`, so no
SQL step can move them. `Vault::migrate_meeting_templates` moves them on first
read (`Vault::templates`):

- They keep their ids and their order, so `Recording.template_id` and the
  meetings' default still resolve.
- The move sets `MeetingSettings.templates_moved`, and the emptied list is
  saved as `[]`.
- `Default` still seeds the starter, which is how a new vault gets one. That is
  also why the flag, not the empty list, says the move is done.
  `save_meeting_settings` always keeps the flag set, so a form that does not
  know about it cannot bring a deleted starter back.
- A vault this process cannot write to lists the old templates without moving
  them.
- An older build opening the vault afterwards finds an empty list and falls
  back to its own starter.

## Surfaces

- **Commands** (`domains::templates`, new `Scope::Templates`): `list_templates`,
  `new_template`, `save_template`, `delete_template`, `lint_template` and
  `render_template`.
- **Removed:** `new_meeting_template` and `lint_meeting_template`.
- **Change kind:** `template`.
- **Export:** a `templates` part, one Markdown page per template. It imports
  back.
- **Interface:**
  - Settings → Templates (`TemplatesPanel`): edit, check for problems as you
    type, and preview for a chosen day and journal, or with the model for
    meetings.
  - The journal's settings: the default and the rules.
  - The Meetings tab: the default only.
