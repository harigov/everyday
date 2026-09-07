<script lang="ts">
  import { app } from '../lib/state.svelte'
  import { isMock } from '../lib/api'
  import Logo from './Logo.svelte'

  let password = $state('')
  let busy = $state(false)
  let shake = $state(false)

  async function submit(e: Event) {
    e.preventDefault()
    if (!password || busy) return
    busy = true
    try {
      await app.unlock(password)
      password = ''
    } catch {
      // The message is already in `app.error`; nudge the form so the
      // failure is felt as well as read.
      shake = true
      setTimeout(() => (shake = false), 420)
      password = ''
    } finally {
      busy = false
    }
  }
</script>

<div class="lock">
  <form class="card" class:shake onsubmit={submit}>
    <div class="mark"><Logo size={46} tile /></div>
    <h1>Every Day</h1>
    <p class="sub">{app.status?.name ?? 'Your journal'} is locked.</p>

    <!-- svelte-ignore a11y_autofocus -->
    <input
      class="field pw"
      type="password"
      placeholder="Password"
      bind:value={password}
      autofocus
      autocomplete="current-password"
      disabled={busy}
    />

    {#if app.error}<p class="error">{app.error}</p>{/if}

    <button class="btn btn-primary wide" type="submit" disabled={busy || !password}>
      {busy ? 'Unlocking…' : 'Unlock'}
    </button>

    {#if isMock}
      <p class="hint demo">Demo build — the password is <code>everyday</code></p>
    {/if}
  </form>
</div>

<style>
  .lock {
    display: grid;
    place-items: center;
    height: 100%;
    background:
      radial-gradient(
        1000px 520px at 50% -10%,
        color-mix(in oklab, var(--accent) 11%, transparent),
        transparent 70%
      ),
      var(--bg);
  }

  .card {
    width: 320px;
    padding: var(--sp-8) var(--sp-6) var(--sp-6);
    text-align: center;
    background: var(--bg-raised);
    border: 1px solid var(--border);
    border-radius: var(--radius-xl);
    box-shadow: var(--shadow-lg);
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

  .shake {
    animation: shake 0.4s var(--ease);
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
