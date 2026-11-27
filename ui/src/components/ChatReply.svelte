<script lang="ts">
  // A reply still streaming in, drawn a block at a time.
  //
  // The words arrive once a frame (see `DeltaBatch` in `lib/agent.ts`), and
  // drawing the whole reply again for each frame re-read all of its Markdown
  // and replaced every node of it -- a cost that grew with the reply, and a
  // selection in it that went with every frame. Here each top-level block is
  // drawn on its own: one whose markup has not changed keeps its nodes, so
  // only the last paragraph, the one being written, is drawn again; and
  // `MarkdownStream` reads again only from near the end of the text, so the
  // paragraphs already finished are not re-read either.
  //
  // Only while it streams. A finished reply is drawn whole by the thread,
  // exactly as one opened from the history list is -- the blocks joined are
  // that same markup, so the change at the end moves nothing.
  //
  // Inside the thread's `.reply.md`, which carries the styles and the press
  // on a `files:` chip, so this draws the blocks and nothing around them.

  import { MarkdownStream } from '../lib/markdown'

  let { text }: { text: string } = $props()

  const stream = new MarkdownStream()
  const blocks = $derived(stream.render(text))
</script>

<!-- The rule below is right in general and this is the case it does not
     cover: `renderMarkdown` and its blocks HTML-escape the input before a
     single pattern runs, so every tag in what comes back was written by that
     module. See `lib/markdown.ts` and `scripts/markdown.test.mjs`. -->
{#each blocks as html, i (i)}
  <!-- eslint-disable-next-line svelte/no-at-html-tags -->
  {@html html}
{/each}
