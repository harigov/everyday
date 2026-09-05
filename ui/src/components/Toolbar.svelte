<script lang="ts">
  import type { Editor } from '@tiptap/core'

  let { editor }: { editor: Editor | null } = $props()

  // Re-read the editor's mark/node state on every transaction, so the
  // buttons reflect the caret rather than the last click.
  let tick = $state(0)
  $effect(() => {
    if (!editor) return
    const bump = () => (tick += 1)
    editor.on('transaction', bump)
    editor.on('selectionUpdate', bump)
    return () => {
      editor.off('transaction', bump)
      editor.off('selectionUpdate', bump)
    }
  })

  function active(name: string, attrs?: Record<string, unknown>): boolean {
    void tick
    return editor?.isActive(name, attrs) ?? false
  }

  type Item =
    | { kind: 'sep' }
    | {
        kind: 'btn'
        label: string
        glyph: string
        run: () => void
        on?: () => boolean
        keys?: string
      }

  const items = (): Item[] => {
    const c = () => editor!.chain().focus()
    if (!editor) return []
    return [
      { kind: 'btn', label: 'Bold', glyph: 'B', keys: 'Ctrl B', run: () => c().toggleBold().run(), on: () => active('bold') },
      { kind: 'btn', label: 'Italic', glyph: 'I', keys: 'Ctrl I', run: () => c().toggleItalic().run(), on: () => active('italic') },
      { kind: 'btn', label: 'Underline', glyph: 'U', keys: 'Ctrl U', run: () => c().toggleUnderline().run(), on: () => active('underline') },
      { kind: 'btn', label: 'Highlight', glyph: '▨', run: () => c().toggleHighlight().run(), on: () => active('highlight') },
      { kind: 'sep' },
      { kind: 'btn', label: 'Heading', glyph: 'H', run: () => c().toggleHeading({ level: 2 }).run(), on: () => active('heading', { level: 2 }) },
      { kind: 'btn', label: 'Quote', glyph: '❝', run: () => c().toggleBlockquote().run(), on: () => active('blockquote') },
      { kind: 'btn', label: 'Code', glyph: '‹›', run: () => c().toggleCode().run(), on: () => active('code') },
      { kind: 'sep' },
      { kind: 'btn', label: 'Bullet list', glyph: '•', run: () => c().toggleBulletList().run(), on: () => active('bulletList') },
      { kind: 'btn', label: 'Numbered list', glyph: '1.', run: () => c().toggleOrderedList().run(), on: () => active('orderedList') },
      { kind: 'btn', label: 'Checklist', glyph: '☑', run: () => c().toggleTaskList().run(), on: () => active('taskList') },
      { kind: 'sep' },
      { kind: 'btn', label: 'Divider', glyph: '—', run: () => c().setHorizontalRule().run() },
      { kind: 'btn', label: 'Link', glyph: '\u2197', run: promptLink, on: () => active('link') },
    ]
  }

  function promptLink() {
    if (!editor) return
    const previous = (editor.getAttributes('link').href as string) ?? ''
    const href = window.prompt('Link address', previous)
    if (href === null) return
    if (href === '') {
      editor.chain().focus().extendMarkRange('link').unsetLink().run()
      return
    }
    editor.chain().focus().extendMarkRange('link').setLink({ href }).run()
  }
</script>

<div class="bar" role="toolbar" aria-label="Formatting">
  {#each items() as item, i (i)}
    {#if item.kind === 'sep'}
      <span class="sep" aria-hidden="true"></span>
    {:else}
      <button
        class="tool"
        class:on={item.on?.() ?? false}
        title={item.keys ? `${item.label} (${item.keys})` : item.label}
        aria-label={item.label}
        aria-pressed={item.on?.() ?? false}
        onclick={item.run}
      >{item.glyph}</button>
    {/if}
  {/each}
</div>

<style>
  .bar {
    display: flex;
    align-items: center;
    gap: 1px;
    height: 38px;
    padding: 0 var(--sp-3);
    border-bottom: 1px solid var(--border);
    background: color-mix(in oklab, var(--bg-raised) 80%, var(--bg));
    backdrop-filter: blur(12px);
    flex: none;
  }

  .tool {
    display: grid;
    place-items: center;
    width: 28px;
    height: 26px;
    border-radius: var(--radius-sm);
    font-size: var(--text-base);
    color: var(--fg-muted);
    transition: background var(--fast) var(--ease), color var(--fast) var(--ease);
  }
  .tool:hover { background: var(--bg-hover); color: var(--fg); }
  .tool.on { background: var(--bg-active); color: var(--journal-accent, var(--accent)); }

  /* The letter buttons read as their own formatting, which makes the bar
     legible without icon assets. */
  .tool[aria-label='Bold'] { font-weight: 750; }
  .tool[aria-label='Italic'] { font-style: italic; font-family: var(--font-read); }
  .tool[aria-label='Underline'] { text-decoration: underline; text-underline-offset: 2px; }
  .tool[aria-label='Heading'] { font-family: var(--font-read); font-weight: 650; }

  .sep { width: 1px; height: 16px; margin: 0 var(--sp-2); background: var(--border); }
</style>
