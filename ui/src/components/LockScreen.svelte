<script lang="ts">
  import { app } from '../lib/state.svelte'
  import { isMock } from '../lib/api'
  import { VaultError } from '../lib/types'
  import Logo from './Logo.svelte'

  let password = $state('')
  // What to say about the last try. A wrong password and a backend that
  // could not be reached are different news and must not read alike.
  let failure = $state<string | null>(null)
  let busy = $state(false)
  let shake = $state(false)

  async function submit(e: Event) {
    e.preventDefault()
    if (!password || busy) return
    busy = true
    failure = null
    try {
      await app.unlock(password)
      password = ''
    } catch (err) {
      // The raw message is in `app.error`; the screen words it for a person.
      failure =
        err instanceof VaultError && err.code === 'bad_password'
          ? 'That password didn’t work.'
          : `Could not unlock: ${err instanceof Error ? err.message : String(err)}`
      // Nudge the form so the failure is felt as well as read.
      shake = true
      setTimeout(() => (shake = false), 420)
      password = ''
    } finally {
      busy = false
    }
  }
</script>

<div class="lock">
  <form class="card frost" class:shake onsubmit={submit}>
    <div class="mark"><Logo size={46} tile /></div>
    <h1>Jatayu</h1>
    <p class="sub">
      {#if app.screenOnly}
        This screen is locked. {app.status?.name ?? 'Your vault'} is still open.
      {:else}
        {app.status?.name ?? 'Your vault'} is locked.
      {/if}
    </p>

    <!-- svelte-ignore a11y_autofocus -->
    <input
      class="field pw"
      type="password"
      placeholder="Password"
      aria-label="Password"
      bind:value={password}
      autofocus
      autocomplete="current-password"
      disabled={busy}
    />

    {#if failure ?? app.error}<p class="error" role="alert">{failure ?? app.error}</p>{/if}

    <button class="btn btn-primary wide" type="submit" disabled={busy || !password}>
      {#if busy}
        {app.screenOnly ? 'Checking…' : 'Unlocking…'}
      {:else}
        {app.screenOnly ? 'Unlock the screen' : 'Unlock'}
      {/if}
    </button>

    {#if isMock}
      <p class="hint demo">Demo build — the password is <code>jatayu</code></p>
    {/if}
  </form>
</div>

<style>
  /* A glow of the accent over the window's own backdrop -- no page colour
     between them, so a glass theme's colours are the first thing seen. */
  .lock {
    display: grid;
    place-items: center;
    height: 100%;
    background: radial-gradient(
      1000px 520px at 50% -10%,
      color-mix(in oklab, var(--accent) 11%, transparent),
      transparent 70%
    );
  }

  .card {
    width: 320px;
    padding: var(--sp-8) var(--sp-6) var(--sp-6);
    text-align: center;
    border: 1px solid var(--border);
    border-radius: var(--radius-xl);
    box-shadow: var(--shadow-lg);
    /* Frosted over the window's backdrop (`.frost`), which needs it placed
       and stacked. */
    position: relative;
    z-index: 0;
  }

  /* The mark is an SVG block now, so it centres by layout, not by text. */
  .mark {
    display: flex;
    justify-content: center;
    margin-bottom: var(--sp-4);
  }

  h1 {
    font-family: var(--font-read);
    font-size: var(--text-xl);
    font-weight: 650;
    letter-spacing: -0.015em;
  }
  .sub {
    margin-top: var(--sp-1);
    margin-bottom: var(--sp-6);
    font-size: var(--text-sm);
    color: var(--fg-subtle);
  }

  .pw {
    text-align: center;
    letter-spacing: 0.14em;
  }
  .pw::placeholder {
    letter-spacing: normal;
  }

  .error {
    margin-top: var(--sp-3);
  }
  .wide {
    width: 100%;
    height: 36px;
    margin-top: var(--sp-4);
  }

  .demo {
    margin-top: var(--sp-4);
  }
  .demo code {
    font-family: var(--font-mono);
    font-size: var(--text-xs);
    background: var(--bg-sunken);
    padding: 1px 5px;
    border-radius: 4px;
  }

  /* A phone: the card in the middle of what is left above the keyboard --
     the page shrinks to that on Android -- and the page scrolls to it when
     there is no room to be in the middle of, rather than the card's top
     going off the screen. `margin: auto` centres when it fits and is
     top-aligned when it does not, which `place-items: center` is not. */
  :global([data-phone]) .lock {
    display: flex;
    overflow-y: auto;
    padding: calc(var(--safe-top) + var(--sp-4)) calc(var(--safe-right) + var(--sp-4))
      calc(var(--safe-bottom) + var(--sp-4)) calc(var(--safe-left) + var(--sp-4));
  }
  :global([data-phone]) .card {
    width: min(320px, 100%);
    margin: auto;
    padding: var(--sp-6) var(--sp-5) var(--sp-5);
  }
  :global([data-phone]) .wide {
    height: 48px;
  }
  :global([data-phone]) .pw {
    height: 48px;
  }

  /* The nudge is a motion; the message under it says the same thing, so
     somebody who asked for less of it loses nothing. */
  @media (prefers-reduced-motion: no-preference) {
    .shake {
      animation: shake 0.4s var(--ease);
    }
  }
  @keyframes shake {
    10%,
    90% {
      transform: translateX(-2px);
    }
    20%,
    80% {
      transform: translateX(4px);
    }
    30%,
    50%,
    70% {
      transform: translateX(-7px);
    }
    40%,
    60% {
      transform: translateX(7px);
    }
  }
</style>
