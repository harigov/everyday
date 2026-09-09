<script lang="ts">
  import type { Editor } from '@tiptap/core'
  import Icon from './Icon.svelte'
  import { focusOnMount, trapFocus } from '../lib/focus'
  import type { IconName } from '../lib/icons'

  let { editor }: { editor: Editor | null } = $props()

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
  const MOD = /mac/i.test(navigator.platform ?? '') ? '⌘' : 'Ctrl+'

  type Tool = {
    label: string
    icon: IconName
    run: () => void
    on?: () => boolean
    keys?: string
  }

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

<div class="bar" role="toolbar" aria-label="Formatting">
  {#each groups() as group, g (g)}
    {#if g > 0}<span class="sep" aria-hidden="true"></span>{/if}
    <div class="group">
      {#each group as tool (tool.label)}
        {@const on = tool.on?.() ?? false}
        <button
          class="tool"
          class:on
          title={tool.keys ? `${tool.label}  ${MOD}${tool.keys}` : tool.label}
          aria-label={tool.label}
          aria-pressed={on}
          onclick={tool.run}
        >
          <Icon name={tool.icon} size={19} weight={1.6} />
        </button>
      {/each}
    </div>
  {/each}
</div>

{#if linkOpen}
  <!-- svelte-ignore a11y_no_static_element_interactions, a11y_click_events_have_key_events -->
  <div class="scrim" onclick={closeLink}></div>
  <div class="sheet" role="dialog" aria-label="Link address" aria-modal="true" use:trapFocus>
    <form onsubmit={applyLink}>
      <label class="label" for="link-href">Link address</label>
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

  .sep {
    width: 1px;
    height: 18px;
    margin: 0 var(--sp-2);
    background: var(--border);
    flex: none;
  }
</style>
