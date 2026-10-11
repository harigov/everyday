<script lang="ts">
  // An OAuth sign-in's URL, for when the browser did not open (or opened
  // somewhere out of sight): a link that can be clicked, selected, or copied
  // whole. Shared by the add-account sheet and an account's own "Sign in
  // again", which both show it beneath their waiting message.
  //
  // Clicking goes through `openExternal` in the desktop app, because a plain
  // `target="_blank"` there would ask the webview to navigate rather than
  // the system browser. Served to a browser by `everyday-server`, there is
  // no shell to ask, and the anchor's own default does the job.

  import { openExternal } from '../lib/open-external'

  let { url }: { url: string } = $props()

  const shell = typeof window !== 'undefined' && '__TAURI_INTERNALS__' in window

  let copied = $state(false)

  function open(e: MouseEvent) {
    if (!shell) return
    e.preventDefault()
    void openExternal(url)
  }

  async function copy() {
    await navigator.clipboard.writeText(url)
    copied = true
    setTimeout(() => (copied = false), 1500)
  }
</script>

<div class="url-row">
  <a class="link" href={url} target="_blank" rel="noopener noreferrer" onclick={open}>{url}</a>
  <button class="btn" onclick={() => void copy()}>{copied ? 'Copied' : 'Copy'}</button>
</div>

<style>
  .url-row {
    display: flex;
    align-items: flex-start;
    gap: var(--sp-2);
  }
  .link {
    flex: 1;
    min-width: 0;
    max-height: 6.5em;
    overflow-y: auto;
    overflow-wrap: anywhere;
    font-family: var(--font-mono);
    font-size: var(--text-xs);
    color: var(--accent);
    background: var(--bg-sunken);
    padding: 0.4rem 0.5rem;
    border-radius: 4px;
    /* The window turns selection off everywhere (app.css); a URL is
       exactly the thing someone wants to select by hand. */
    user-select: text;
    cursor: pointer;
  }
</style>
