<script lang="ts">
  // Connecting a bank: the sign-in in flight, what went wrong, and what to do
  // when there are no Plaid keys to connect with.
  //
  // Two shapes, one subject.
  //
  // The sheet (the default) is drawn once, by `MoneyView`, and appears on its
  // own while the store is linking or has a linking error. Linking never runs
  // Plaid's code in this window: the store asks the service for a Hosted Link
  // page, opens it in the person's own browser, and polls until it finishes.
  // All the sheet does is say so, offer Cancel, and show what went wrong if
  // it did. The page's address is shown only as a fallback, for when no
  // browser could be opened (a build served to a browser elsewhere has no
  // shell to ask), and only if it is https -- it came from the service, but a
  // link that is about to be clicked is worth checking twice.
  //
  // `inline` is the card the Accounts pane and the empty screen show in place
  // of "Connect a bank" while no Plaid keys are set: there is nothing to ask
  // Plaid with, so instead of a button that can only fail, it says where the
  // keys go and takes you there.
  //
  // Escape while waiting cancels the sign-in, the same as the button. The
  // sheet is modal because a half-finished link is not something to leave
  // running unnoticed behind other work.

  import { money } from '../lib/money.svelte'
  import { panels } from '../lib/panels.svelte'
  import { focusOnMount, trapFocus } from '../lib/focus'
  import Icon from './Icon.svelte'
  import SignInLink from './SignInLink.svelte'

  const { inline = false }: { inline?: boolean } = $props()

  const linking = $derived(money.linking)
  const error = $derived(money.linkError)
  const keysMissing = $derived(money.plaid !== null && !money.plaid.configured)
  const showing = $derived(!inline && (linking !== null || error !== null))

  /** The bank being signed in to again, by name. */
  const relinkName = $derived.by(() => {
    const id = linking?.relink
    if (!id) return null
    return money.links.find((s) => s.link.id === id)?.link.name ?? null
  })

  const title = $derived(relinkName ? `Sign in to ${relinkName} again` : 'Connect a bank')

  /** Only an https address is ever shown as a link. */
  const safeUrl = $derived(linking && /^https:\/\//i.test(linking.url) ? linking.url : null)

  // An error with the keys' state unknown: read it, so the sheet can say
  // "no keys" rather than whatever the service called that.
  $effect(() => {
    if (!inline && error !== null && money.plaid === null) void money.loadPlaid()
  })

  function close() {
    money.linkError = null
  }

  function cancel() {
    void money.cancelLink()
  }

  function openSettings() {
    if (money.linkError !== null) money.linkError = null
    panels.openSettings('money')
  }

  function onkeydown(e: KeyboardEvent) {
    if (!showing || e.key !== 'Escape' || e.defaultPrevented) return
    if (linking) cancel()
    else close()
  }
</script>

<svelte:window {onkeydown} />

{#if inline}
  {#if keysMissing}
    <div class="keys">
      <span class="keys-mark" aria-hidden="true"><Icon name="lock" size={16} /></span>
      <div class="keys-text">
        <b>Add your Plaid keys first</b>
        <span>
          Banks are connected through Plaid, with keys from your own Plaid account — none are set
          yet. They go under Settings → Money and are sealed in the vault.
        </span>
      </div>
      <button class="btn btn-primary" onclick={openSettings}>Open Settings → Money</button>
    </div>
  {/if}
{:else if showing}
  <!-- svelte-ignore a11y_no_static_element_interactions, a11y_click_events_have_key_events -->
  <div class="scrim" onclick={() => !linking && close()}></div>
  <div class="sheet" role="dialog" aria-modal="true" aria-label={title} use:trapFocus>
    <h2>{title}</h2>

    {#if linking}
      <div class="waiting" role="status">
        <span class="spinner" aria-hidden="true"></span>
        <p class="lead">Finish signing in in your browser…</p>
      </div>
      {#if money.linkOpened}
        <p class="hint">
          A page from Plaid has opened in your browser. Choose your bank and sign in there — your
          bank password goes to Plaid and your bank, never to Every Day. This closes by itself when
          you are done.
        </p>
        {#if safeUrl}
          <details class="fallback">
            <summary>No page opened?</summary>
            <p class="hint">Open this address in your browser yourself:</p>
            <SignInLink url={safeUrl} />
          </details>
        {/if}
      {:else if safeUrl}
        <p class="hint">
          This window cannot open a browser itself. Open this page from Plaid in your browser and
          sign in to your bank there — your bank password goes to Plaid and your bank, never to
          Every Day. This closes by itself when you are done.
        </p>
        <SignInLink url={safeUrl} />
      {:else}
        <p class="error">The sign-in page could not be opened.</p>
      {/if}
      <div class="sheet-row">
        <span class="spacer"></span>
        <button class="btn" use:focusOnMount onclick={cancel}>Cancel</button>
      </div>
    {:else if keysMissing}
      <p class="hint">
        Banks are connected through Plaid, with keys from your own Plaid account. None are set yet:
        add your client id and secret under Settings → Money, then connect again.
      </p>
      <div class="sheet-row">
        <span class="spacer"></span>
        <button class="btn" onclick={close}>Close</button>
        <button class="btn btn-primary" use:focusOnMount onclick={openSettings}>
          Open Settings → Money
        </button>
      </div>
    {:else if error}
      <p class="error">{error}</p>
      <div class="sheet-row">
        <span class="spacer"></span>
        <button class="btn" use:focusOnMount onclick={close}>Close</button>
      </div>
    {/if}
  </div>
{/if}

<style>
  h2 {
    font-size: var(--text-md);
    font-weight: 620;
    letter-spacing: -0.008em;
    margin-bottom: var(--sp-3);
  }
  .hint {
    margin: 0 0 var(--sp-2);
  }
  .error {
    margin-top: var(--sp-2);
    overflow-wrap: anywhere;
  }

  .waiting {
    display: flex;
    align-items: center;
    gap: var(--sp-3);
    margin-bottom: var(--sp-3);
  }
  .lead {
    margin: 0;
    font-size: var(--text-base);
    font-weight: 550;
    color: var(--fg);
  }
  .spinner {
    flex: none;
    width: 16px;
    height: 16px;
    border: 2px solid color-mix(in oklab, var(--accent) 25%, transparent);
    border-top-color: var(--accent);
    border-radius: 50%;
    animation: spin 0.9s linear infinite;
  }
  @keyframes spin {
    to {
      transform: rotate(360deg);
    }
  }

  .fallback {
    margin-top: var(--sp-2);
  }
  .fallback summary {
    cursor: pointer;
    font-size: var(--text-sm);
    color: var(--fg-subtle);
  }
  .fallback .hint {
    margin-top: var(--sp-2);
  }

  .keys {
    display: flex;
    align-items: center;
    flex-wrap: wrap;
    gap: var(--sp-3);
    padding: var(--sp-4);
    border: 1px solid var(--border);
    border-radius: var(--radius-lg);
    background: var(--bg);
    text-align: left;
  }
  .keys-mark {
    flex: none;
    display: grid;
    place-items: center;
    width: 32px;
    height: 32px;
    border-radius: 50%;
    background: var(--bg-sunken);
    color: var(--fg-subtle);
  }
  .keys-text {
    flex: 1;
    min-width: 220px;
    display: flex;
    flex-direction: column;
    gap: 2px;
  }
  .keys-text b {
    font-size: var(--text-base);
    font-weight: 600;
    color: var(--fg);
  }
  .keys-text span {
    font-size: var(--text-sm);
    line-height: var(--leading-snug);
    color: var(--fg-subtle);
  }
</style>
