<script lang="ts">
  // One signature, edited as it will look: the same editor the compose sheet
  // has, cut down to what a signature is, and handing back the HTML a draft
  // starts with. See `signature.ts`.

  import { untrack } from 'svelte'
  import { Editor } from '@tiptap/core'
  import StarterKit from '@tiptap/starter-kit'
  import Link from '@tiptap/extension-link'
  import Placeholder from '@tiptap/extension-placeholder'
  import { LineBreakOnEnter } from '../lib/signature'

  let {
    value = $bindable(''),
    label,
    placeholder = 'Your name, and anything that goes under it',
  }: { value: string; label: string; placeholder?: string } = $props()

  let host = $state<HTMLDivElement>()
  let editor: Editor | null = null
  /** What this field last handed back -- so a change that came from anywhere
   *  else, an identity above this one being removed, can be told from typing. */
  let own = untrack(() => value)

  $effect(() => {
    const el = host
    if (!el) return
    const ed = new Editor({
      element: el,
      extensions: [
        StarterKit.configure({ heading: false, link: false }),
        Link.configure({
          openOnClick: false,
          autolink: true,
          protocols: ['http', 'https', 'mailto'],
        }),
        Placeholder.configure({ placeholder }),
        LineBreakOnEnter,
      ],
      content: untrack(() => value),
      editorProps: {
        attributes: { class: 'ed-content', 'aria-label': label, spellcheck: 'true' },
      },
      onUpdate: ({ editor: e }) => {
        own = e.getText().trim() ? e.getHTML() : ''
        value = own
      },
    })
    editor = ed
    return () => {
      ed.destroy()
      editor = null
    }
  })

  $effect(() => {
    if (value !== own) {
      own = value
      editor?.commands.setContent(value, { emitUpdate: false })
    }
  })
</script>

<div class="signature-field" bind:this={host}></div>

<style>
  .signature-field {
    min-height: 64px;
    padding: var(--sp-2) var(--sp-3);
    border: 1px solid var(--border);
    border-radius: var(--radius);
    background: var(--bg);
    font-size: var(--text-base);
    line-height: var(--leading-normal);
    user-select: text;
    cursor: text;
  }
  .signature-field:focus-within {
    border-color: var(--accent);
    box-shadow: 0 0 0 3px color-mix(in oklab, var(--accent) 16%, transparent);
  }
  .signature-field :global(.ed-content) {
    min-height: 48px;
    outline: none;
  }
  .signature-field :global(p) {
    margin: 0;
  }
  .signature-field :global(p.is-editor-empty:first-child::before) {
    content: attr(data-placeholder);
    float: left;
    height: 0;
    pointer-events: none;
    color: var(--fg-faint);
  }
</style>
