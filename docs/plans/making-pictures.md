# Pictures the assistant makes

Nov 19 2026. The assistant can make a picture when it is asked for one, in two
ways:

- **`generate_image`** asks the image model chosen in Settings → Models
  (OpenAI's Images API, or FLUX.2 Klein / Z-Image Turbo on this computer) to
  paint from a description. It is for a scene, a likeness, a mood or a style.
- **`draw_illustration`** draws markup the model writes, an SVG or an HTML
  page, into a PNG. It is for anything exact or with words in it: a diagram, a
  chart, a timeline, a table, a card.

Both keep what they make in Files. The panel draws it ahead of the reply
(where a reopened thread has it too), and a Matrix chat sends it as an
`m.image` the moment it is made, without waiting for the rest of the turn.

## Reusable pieces

| Module | What it does | Who else can use it |
|---|---|---|
| `illustrate` | `render(markup, source, width, height) -> Rendered { png, width, height, notes }` | the `render_illustration` command, anything that wants a chart or a card as a picture |
| `images` | `Service::generate_image(prompt, Shape)`; `painter(vault)` says whether a picture can be made | the `generate_image` command (Settings' test button) |
| `images::keep` | a made picture into Files: name, caption, origin's folder, thumbnail; `Encoding::AsMade` or `Smallest` | anything that makes a picture to keep |
| `images::tools` | the two assistant tools, a per-turn `Allowance`, `files_of` for the turn's ledger | `agent::build` |

## Efficiency

- **An SVG is drawn in-process by resvg**, with no browser. A diagram takes
  milliseconds on every platform. The system fonts are read once and kept.
  The prompt tells the model to prefer SVG, which is also shorter to write.
- **An HTML page is drawn by a warm headless Chrome.** It is started on first
  use and kept for 2 minutes after the last drawing, so a reply with several
  cards pays the roughly 1 s start once. Each drawing gets its own tab. Pages
  are drawn at 2× for sharp text, dropping to 1× and then cut at the bottom
  past 4096 px or 16 Mpx.
- **Generated pictures are re-encoded** as the smaller of PNG and JPEG
  (`pictures::prepare`). A 1024² PNG of 1.5 MB is usually a JPEG of about
  200 KB. Drawings are stored byte for byte, because a JPEG would blur their
  lettering.
- **A shape argument** (square, wide, tall) asks each model for a size it
  takes as is: OpenAI's 1024², 1536×1024 and 1024×1536; locally 512², 768×512
  and 512×768.
- **Per-turn allowance:** 4 generated pictures and 8 drawings. A looping model
  stops there, not at the person's bill or after an hour of local painting.
  The description tells the model that a local model takes a minute or more
  per picture.

## Safety

The markup is the model's writing, and the model may have read a stranger's
mail.

- **SVG:** `<image href>` that names a path or an address draws nothing.
  Only `data:` pictures are drawn. resvg has no network.
- **HTML:**
  - The browser's only proxy is a loopback listener that closes every
    connection at once, and `<-loopback>` removes Chrome's localhost
    exception.
  - A content policy goes ahead of the model's markup: `data:` only, no
    scripts, frames or forms.
  - The page is set with `Page.setDocumentContent`; nothing is navigated to.
  - The profile is throwaway, as in the browser module.
- **Only the two tools' results are read for `file_id`.** Another tool's
  result that happens to carry one is not drawn as a picture.

## Where it is not offered

- A dream gets neither tool.
- A vault without Files gets neither, since there is nowhere to keep a
  picture.
- With no usable image model (none chosen, no key, or local weights not
  downloaded), only `draw_illustration` is offered. The guidance then says to
  point at Settings → Models.
- Without Chrome or Chromium, or on Windows, `draw_illustration` takes SVG
  only.
- Routines and jobs keep their pictures under `FileOrigin::Jobs`.

## Routine deliveries

A routine's pictures go with its result (`delivery.rs`). They are read back
from the run's own transcript (`images::tools::made_in`): the picture tools'
stored results, matched to their calls, failures skipped.

- **Chat:** `ChatOutlet::show` sends them after the message, or after the call
  that said it. The relay queues them on the owner's room like a post, and a
  picture trashed since the run is skipped. The run log says "Matrix, with 2
  pictures", or why they did not go.
- **Email:** the pictures are attached to the message to self, up to 15 MB in
  all. The rest stay in Files, and the log says how many did.
- A run that failed sends no pictures.

## Not done yet

- A background job's answer posted to a chat (`jobs::answer`) still sends
  words only.
- A voice call does not send what it made into the chat.
- The model cannot see what it drew. It is told when a page came out too wide
  or was cut, and is asked to draw it again.
