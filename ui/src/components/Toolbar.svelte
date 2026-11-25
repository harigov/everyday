<script lang="ts">
  import { onDestroy } from 'svelte'
  import type { Editor } from '@tiptap/core'
  import Icon from './Icon.svelte'
  import { focusOnMount, trapFocus } from '../lib/focus'
  import type { IconName } from '../lib/icons'
  import { menu } from '../lib/menu.svelte'
  import { phone } from '../lib/phone.svelte'
  import { notify } from '../lib/notify.svelte'
  import { templates } from '../lib/templates.svelte'
  import type { Template, TemplateUse } from '../lib/types'

  let {
    editor,
    templateFor,
    oninsert,
    markdown,
  }: {
    editor: Editor | null
    /**
     * Offer "Insert template" with the templates kept for this place. The
     * caller fills the chosen one in -- it knows the day and the journal --
     * and puts it at the caret.
     */
    templateFor?: TemplateUse
    oninsert?: (template: Template) => void
    /**
     * Offer "Copy as Markdown", with the page as the caller writes it. Asked
     * for at the press rather than handed over ready-made, so what lands on
     * the clipboard is the page as it is now, not as it was last render --
     * and the caller owns the title, which is not in the editor.
     */
    markdown?: () => string
  } = $props()

  $effect(() => {
    if (templateFor) void templates.ensure()
  })

  const offered = $derived(templateFor ? templates.for(templateFor) : [])

  function pickTemplate(e: MouseEvent) {
    const box = (e.currentTarget as HTMLElement).getBoundingClientRect()
    menu.showAt(box.left, box.bottom + 4, [
      { kind: 'heading', label: 'Insert template' },
      ...offered.map((t) => ({ label: t.name, run: () => oninsert?.(t) })),
    ])
  }

  // Re-read the editor's mark/node state after a transaction, so the buttons
  // reflect the caret rather than the last click.
  //
  // Coalesced to one bump per frame. Writing state straight out of the
  // handler meant a re-render of all twelve buttons -- and twelve `isActive`
  // walks -- per character typed, and it wrote to state from inside whatever
  // effect happened to be dispatching the transaction, which is exactly the
  // pattern Svelte refuses to let recurse.
  let tick = $state(0)
  $effect(() => {
    if (!editor) return
    let queued = 0
    const bump = () => {
      if (queued) return
      queued = requestAnimationFrame(() => {
        queued = 0
        tick += 1
      })
    }
    editor.on('transaction', bump)
    editor.on('selectionUpdate', bump)
    return () => {
      if (queued) cancelAnimationFrame(queued)
      editor.off('transaction', bump)
      editor.off('selectionUpdate', bump)
    }
  })

  function active(name: string, attrs?: Record<string, unknown>): boolean {
    void tick
    return editor?.isActive(name, attrs) ?? false
  }

  // Tooltips should quote the key the reader actually presses.
  const MAC = /mac/i.test(navigator.platform ?? '')
  const MOD = MAC ? '⌘' : 'Ctrl+'
  const SHIFT = MAC ? '⇧' : 'Shift+'

  type Tool = {
    label: string
    icon: IconName
    run: () => void
    on?: () => boolean
    keys?: string
    /** The shortcut is the modifier *and* Shift: Strikethrough's is Mod+Shift+S. */
    shift?: boolean
  }

  const tip = (tool: Tool) =>
    tool.keys ? `${tool.label}  ${MOD}${tool.shift ? SHIFT : ''}${tool.keys}` : tool.label

  /** Ordered groups; the gaps between them are the only separators. */
  const groups = (): Tool[][] => {
    if (!editor) return []
    const c = () => editor.chain().focus()
    return [
      [
        {
          label: 'Bold',
          icon: 'bold',
          keys: 'B',
          run: () => c().toggleBold().run(),
          on: () => active('bold'),
        },
        {
          label: 'Italic',
          icon: 'italic',
          keys: 'I',
          run: () => c().toggleItalic().run(),
          on: () => active('italic'),
        },
        {
          label: 'Underline',
          icon: 'underline',
          keys: 'U',
          run: () => c().toggleUnderline().run(),
          on: () => active('underline'),
        },
        {
          label: 'Strikethrough',
          icon: 'strike',
          keys: 'S',
          shift: true,
          run: () => c().toggleStrike().run(),
          on: () => active('strike'),
        },
        {
          label: 'Highlight',
          icon: 'highlight',
          run: () => c().toggleHighlight().run(),
          on: () => active('highlight'),
        },
      ],
      [
        {
          label: 'Heading',
          icon: 'heading',
          run: () => c().toggleHeading({ level: 2 }).run(),
          on: () => active('heading', { level: 2 }),
        },
        {
          label: 'Quote',
          icon: 'quote',
          run: () => c().toggleBlockquote().run(),
          on: () => active('blockquote'),
        },
        {
          label: 'Code',
          icon: 'code',
          keys: 'E',
          run: () => c().toggleCode().run(),
          on: () => active('code'),
        },
      ],
      [
        {
          label: 'Bullet list',
          icon: 'bulletList',
          run: () => c().toggleBulletList().run(),
          on: () => active('bulletList'),
        },
        {
          label: 'Numbered list',
          icon: 'orderedList',
          run: () => c().toggleOrderedList().run(),
          on: () => active('orderedList'),
        },
        {
          label: 'Checklist',
          icon: 'taskList',
          run: () => c().toggleTaskList().run(),
          on: () => active('taskList'),
        },
      ],
      [
        { label: 'Divider', icon: 'divider', run: () => c().setHorizontalRule().run() },
        { label: 'Link', icon: 'link', keys: 'K', run: promptLink, on: () => active('link') },
      ],
    ]
  }

  /**
   * A press on a button takes the focus from the editor, and on a phone that
   * puts the keyboard away for as long as it takes the command to hand the
   * focus back. Keeping the press from moving it leaves the keyboard where
   * it is -- and the caret, and the selection the command is about.
   */
  function keepFocus(e: MouseEvent) {
    if (phone.on) e.preventDefault()
  }

  // ── Copy as Markdown ───────────────────────────────────────────────────

  /** Whether the button is showing its tick, for a moment after a copy. */
  let copied = $state(false)
  let copiedTimer: ReturnType<typeof setTimeout> | null = null

  /**
   * Put the page on the clipboard as Markdown, and show that it went.
   *
   * The tick is the only answer a success gets -- a toast for something the
   * reader just did and can see would be noise -- but a clipboard the webview
   * refuses is a copy that silently did nothing, so that one is said aloud.
   * Not in a dialog: it is a failed convenience, and nothing is lost.
   */
  async function copyMarkdown() {
    if (!markdown) return
    try {
      await navigator.clipboard.writeText(markdown())
      copied = true
      if (copiedTimer) clearTimeout(copiedTimer)
      copiedTimer = setTimeout(() => {
        copiedTimer = null
        copied = false
      }, 1500)
    } catch {
      notify.warn('Could not copy to the clipboard', {
        body: 'Select the text and copy it by hand; that copies Markdown too.',
      })
    }
  }
  onDestroy(() => {
    if (copiedTimer) clearTimeout(copiedTimer)
  })

  // ── The link sheet ─────────────────────────────────────────────────────
  //
  // Not `window.prompt`. A native JavaScript dialog cannot be styled, blocks
  // the whole webview while it is up, and is titled by the embedder -- which
  // in a Tauri window on Linux means the user is asked for a link address by
  // something calling itself "javascript tauri host".
  let linkOpen = $state(false)
  let linkDraft = $state('')

  function promptLink() {
    if (!editor) return
    linkDraft = (editor.getAttributes('link').href as string) ?? ''
    linkOpen = true
  }

  function closeLink() {
    linkOpen = false
    editor?.commands.focus()
  }

  function applyLink(e?: Event) {
    e?.preventDefault()
    if (!editor) return
    const href = normalise(linkDraft.trim())
    linkOpen = false
    if (!href) {
      editor.chain().focus().extendMarkRange('link').unsetLink().run()
      return
    }
    editor.chain().focus().extendMarkRange('link').setLink({ href }).run()
  }

  function removeLink() {
    linkOpen = false
    editor?.chain().focus().extendMarkRange('link').unsetLink().run()
  }

  /** `example.com` is what people type and a URL is what a link needs. */
  function normalise(href: string): string {
    if (!href) return ''
    if (/^(https?|mailto):/i.test(href)) return href
    if (/^[^\s@]+@[^\s@]+\.[^\s@]+$/.test(href)) return `mailto:${href}`
    return `https://${href}`
  }
</script>

<!-- The press is only kept from taking the focus; the buttons are what act. -->
<!-- svelte-ignore a11y_interactive_supports_focus -->
<div class="bar" role="toolbar" aria-label="Formatting" onmousedown={keepFocus}>
  {#each groups() as group, g (g)}
    {#if g > 0}<span class="sep" aria-hidden="true"></span>{/if}
    <div class="group">
      {#each group as tool (tool.label)}
        {@const on = tool.on?.() ?? false}
        <button
          class="tool"
          class:on
          title={tip(tool)}
          aria-label={tool.label}
          aria-pressed={on}
          onclick={tool.run}
        >
          <Icon name={tool.icon} size={19} weight={1.6} />
        </button>
      {/each}
    </div>
  {/each}
  {#if editor && oninsert && offered.length > 0}
    <span class="sep" aria-hidden="true"></span>
    <div class="group">
      <button
        class="tool"
        title="Insert template"
        aria-label="Insert template"
        aria-haspopup="menu"
        onclick={pickTemplate}
      >
        <Icon name="layers" size={19} weight={1.6} />
      </button>
    </div>
  {/if}
  {#if editor && markdown}
    <span class="sep" aria-hidden="true"></span>
    <div class="group">
      <button
        class="tool"
        title="Copy as Markdown"
        aria-label="Copy as Markdown"
        onclick={() => void copyMarkdown()}
      >
        <Icon name={copied ? 'tick' : 'copy'} size={19} weight={1.6} />
      </button>
      <!-- Always in the page, so the reader's screen reader has a region to
           hear change; text added to a region that is created with it is
           often not announced. -->
      <span class="sr-only" role="status" aria-live="polite">{copied ? 'Copied' : ''}</span>
    </div>
  {/if}
</div>

{#if linkOpen}
  <!-- svelte-ignore a11y_no_static_element_interactions, a11y_click_events_have_key_events -->
  <div class="scrim" onclick={closeLink}></div>
  <div class="sheet" role="dialog" aria-label="Link address" aria-modal="true" use:trapFocus>
    <form onsubmit={applyLink}>
      <label class="field-label" for="link-href">Link address</label>
      <input
        id="link-href"
        class="field"
        type="text"
        placeholder="example.com"
        bind:value={linkDraft}
        use:focusOnMount
        spellcheck="false"
        autocapitalize="off"
        autocomplete="off"
        onkeydown={(e) => {
          if (e.key === 'Escape') {
            e.preventDefault()
            closeLink()
          }
        }}
      />
      <div class="sheet-row">
        {#if editor?.isActive('link')}
          <button class="btn btn-ghost-danger" type="button" onclick={removeLink}>Remove</button>
        {/if}
        <span class="spacer"></span>
        <button class="btn" type="button" onclick={closeLink}>Cancel</button>
        <button class="btn btn-primary" type="submit">Apply</button>
      </div>
    </form>
  </div>
{/if}

<style>
  /* Centred, so the row sits over the page column rather than trailing off
     to the left of it on a wide window. */
  .bar {
    display: flex;
    align-items: center;
    justify-content: center;
    height: var(--header-h);
    padding: 0 var(--sp-3);
    border-bottom: 1px solid var(--border);
    background: var(--bg-raised);
    flex: none;
    /* The row is denser than the page; let it scroll rather than wrap or
       clip when the window is narrow. */
    overflow-x: auto;
    scrollbar-width: none;
  }
  .bar::-webkit-scrollbar {
    display: none;
  }

  .group {
    display: flex;
    align-items: center;
    gap: 2px;
  }

  /* 32px targets on the header bar: comfortably clickable, and the 7px radius
     matches the pill the active state draws. */
  .tool {
    display: grid;
    place-items: center;
    width: 32px;
    height: 32px;
    border-radius: 7px;
    color: var(--fg-muted);
    transition:
      background var(--fast) var(--ease),
      color var(--fast) var(--ease);
  }
  .tool:hover {
    background: var(--bg-hover);
    color: var(--fg);
  }
  .tool:active {
    background: var(--bg-active);
  }

  /* Active marks read as accent-on-tint rather than as a grey box, so the
     caret's current formatting is legible at a glance across the row. */
  .tool.on {
    background: color-mix(in oklab, var(--journal-accent, var(--accent)) 14%, transparent);
    color: var(--journal-accent, var(--accent));
  }
  .tool.on:hover {
    background: color-mix(in oklab, var(--journal-accent, var(--accent)) 20%, transparent);
  }

  .sr-only {
    position: absolute;
    width: 1px;
    height: 1px;
    overflow: hidden;
    clip-path: inset(50%);
    white-space: nowrap;
  }

  .sep {
    width: 1px;
    height: 18px;
    margin: 0 var(--sp-2);
    background: var(--border);
    flex: none;
  }

  /* ── On a phone ──────────────────────────────────────────────────────── */

  /* A strip that scrolls sideways, from its left end: centred, as above, the
     part of a row wider than the screen is cut off on both sides and the
     left half of it can never be scrolled to. */
  :global([data-phone]) .bar {
    /* The boundary of what is absolutely placed in it -- the live region at
       its end -- so that a strip wider than the screen does not carry that
       span off past the edge and widen the whole page. */
    position: relative;
    justify-content: flex-start;
    height: 48px;
    padding: 0 var(--sp-2);
    overscroll-behavior-x: contain;
  }
  :global([data-phone]) .group {
    flex: none;
  }
  :global([data-phone]) .tool {
    width: 40px;
    height: 40px;
  }
  :global([data-phone]) .sep {
    margin: 0 var(--sp-1);
  }
</style>
