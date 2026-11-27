# Reading an article in the Feed

Hari asked (Nov 26 2026) how hard it would be to render an article's text
cleanly, the way Instapaper or Pocket do, and then for the parts that were
proposed: the typography, the details above the article, reading settings,
and FiveFilters' site rules if they were straightforward.

## Delivered (Nov 26 2026)

**Typography.** A story's document (`STYLE` in `news/view.rs`) is set as a
reader: 18px, line height 1.6, headings and quotes set apart, links underlined
lightly, pictures centred, a lead picture with room under it, captions and
quiet text as the text colour faded so they suit any page. The face, size and
spacing are custom properties (`--read-font`, `--read-size`, `--read-leading`)
so the window can set them. The app's own reading face stays the default; the
frame cannot load the fonts the app bundles, so its stacks end in faces most
systems have.

**Above the article.** When an article is read from its page, the page's
byline and the publication's own name are kept on the body
(`StoryBody.byline`, `.site_name`, optional, so no schema step) and ride in
the document's head as `news-byline` / `news-site`, as the origin already did.
The header shows the byline when the feed named no author, the publication's
name in place of its address, and "N min read" (230 words a minute, from 200
words). The page's own picture (`og:image`) leads an article that starts
without one, unless the same picture is further down at another size
(`with_lead_picture` in `extract.rs`).

**Reading settings** (`ui/src/lib/reading.ts`, `reading.svelte.ts`,
`ReadingSettings.svelte`): Sans or Serif, nine sizes from 14 to 28px, tight,
normal or loose spacing, a narrow, medium or wide column, and the theme's page
or a white, sepia or night one. Kept on this device (`jatayu.feed.reading`),
applied as the document is loaded (`feed.loadBody`), so the article behind the
popover is the preview. A page of its own colours the pane around the frame
too, through the theme's tokens, so the title stands on the same paper.
Newsletters are mail's documents, often designed down to their fonts, and are
left as they are.

The frame is still sized from the document string, never measured
(`estimateArticleHeight`), so it now takes the type it is set in. Calibrated
against real rendering with this machine's fallback fonts at four widths and
six settings: it errs long by 6-14%, never short. Lines ending at a word and
headings set larger were what made the first version short.

**FiveFilters' site rules** (`news/rules.rs`, `news/xpath.rs`,
`site_rules.txt`): 1,967 sites from ftr-site-config at `6c60966` (public
domain), refreshed with `node scripts/site-rules.mjs`. Read: find/replace,
`strip`, `strip_id_or_class`, `strip_image_src`, `body`, `author`. XPath is
translated to CSS for what CSS can say (96% of body rules, 85% of strip
rules); the rest is left out, never guessed. A site's rule article is kept
unless Readability finds three times as much on the same page, which is a
rule the site has outgrown. Readability reads every page anyway, for the
byline, the site name and the lead picture.

A live run over the day's Hacker News links and pages from ruled sites (18
pages): every one extracted; on Ars Technica plain Readability kept only the
second half of the article and the rule kept all of it; The Verge's and the
BBC's rules keep some furniture (a video notice, a newsletter footer) that
FiveFilters' own pruning would take out.

**Left for later:** fetching the article without being asked, pages that need
JavaScript (the headless Chrome the assistant uses), multi-page articles and
logins from the rules, XPath axes as `:has()`, a pruning pass over a rule's
article, and newsletters run through Readability.
