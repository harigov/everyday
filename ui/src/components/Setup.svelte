<script lang="ts">
  import { app } from '../lib/state.svelte'
  import Logo from './Logo.svelte'

  let name = $state('My Journal')
  let backend = $state('sqlite')
  let password = $state('')
  let confirm = $state('')
  let encrypt = $state(true)
  let busy = $state(false)
  let acknowledged = $state(false)

  const backends = $derived(app.boot?.backends ?? [])
  const path = $derived(app.boot?.defaultPath ?? '')

  const problem = $derived.by(() => {
    if (!encrypt) return acknowledged ? null : 'Confirm you understand the risk below.'
    if (password.length < 8) return password ? 'Use at least 8 characters.' : null
    if (confirm && password !== confirm) return 'The passwords do not match.'
    return null
  })

  const ready = $derived(
    !busy && !problem && (!encrypt || (password.length >= 8 && password === confirm)),
  )

  async function submit(e: Event) {
    e.preventDefault()
    if (!ready) return
    busy = true
    try {
      await app.createVault({
        path,
        name: name.trim() || 'My Journal',
        backend,
        password: encrypt ? password : null,
      })
    } finally {
      busy = false
      password = ''
      confirm = ''
    }
  }
</script>

<div class="setup scroll">
  <form class="card" onsubmit={submit}>
    <div class="mark"><Logo size={46} tile /></div>
    <h1>Every Day</h1>
    <p class="sub">A private journal. Let's set it up.</p>

    <label class="label" for="name">What should we call it?</label>
    <input id="name" class="field" bind:value={name} placeholder="My Journal" />

    <div class="group">
      <span class="label">How should it be stored?</span>
      {#each backends as b (b.id)}
        <label class="option" class:on={backend === b.id}>
          <input type="radio" name="backend" value={b.id} bind:group={backend} />
          <span class="opt-body">
            <span class="opt-name">{b.id === 'sqlite' ? 'Database' : 'Markdown files'}</span>
            <span class="opt-desc">{b.description}</span>
          </span>
        </label>
      {/each}
    </div>

    <div class="group">
      <label class="option" class:on={encrypt}>
        <input type="checkbox" bind:checked={encrypt} />
        <span class="opt-body">
          <span class="opt-name">Encrypt this journal</span>
          <span class="opt-desc">
            Everything is sealed with a key derived from your password.
          </span>
        </span>
      </label>
    </div>

    {#if encrypt}
      <label class="label" for="pw">Password</label>
      <input
        id="pw"
        class="field"
        type="password"
        bind:value={password}
        placeholder="At least 8 characters"
        autocomplete="new-password"
      />
      <div class="gap"></div>
      <input
        class="field"
        type="password"
        bind:value={confirm}
        placeholder="Repeat it"
        autocomplete="new-password"
      />

      <p class="hint warn">
        There is no way to recover this journal without the password. It is not stored anywhere and
        it cannot be reset.
      </p>
    {:else}
      <label class="option danger" class:on={acknowledged}>
        <input type="checkbox" bind:checked={acknowledged} />
        <span class="opt-body">
          <span class="opt-name">I understand this journal will not be encrypted</span>
          <span class="opt-desc">
            Anything written to it is stored in the clear, readable by any program on this computer
            and by anything that backs it up.
          </span>
        </span>
      </label>
    {/if}

    {#if problem}<p class="error">{problem}</p>{/if}
    {#if app.error}<p class="error">{app.error}</p>{/if}

    <button class="btn btn-primary wide" type="submit" disabled={!ready}>
      {busy ? 'Creating…' : 'Create journal'}
    </button>

    {#if path}<p class="hint where">It will live in <code>{path}</code></p>{/if}
  </form>
</div>

<style>
  .setup {
    height: 100%;
    padding: var(--sp-10) var(--sp-4);
    background:
      radial-gradient(
        1000px 520px at 50% -10%,
        color-mix(in oklab, var(--accent) 11%, transparent),
        transparent 70%
      ),
      var(--bg);
  }

  .card {
    width: min(440px, 100%);
    margin: 0 auto;
    padding: var(--sp-8) var(--sp-6) var(--sp-6);
    background: var(--bg-raised);
    border: 1px solid var(--border);
    border-radius: var(--radius-xl);
    box-shadow: var(--shadow-lg);
  }

  .mark {
    display: flex;
    justify-content: center;
  }
  h1 {
    font-family: var(--font-read);
    font-size: var(--text-2xl);
    font-weight: 650;
    letter-spacing: -0.02em;
    text-align: center;
    margin-top: var(--sp-2);
  }
  .sub {
    text-align: center;
    color: var(--fg-subtle);
    margin-bottom: var(--sp-8);
  }

  .group {
    margin-top: var(--sp-6);
  }
  .gap {
    height: var(--sp-2);
  }

  .option {
    display: flex;
    gap: var(--sp-3);
    align-items: flex-start;
    padding: var(--sp-3);
    border: 1px solid var(--border);
    border-radius: var(--radius);
    cursor: pointer;
    margin-top: var(--sp-2);
    transition:
      border-color var(--fast) var(--ease),
      background var(--fast) var(--ease);
  }
  .option:hover {
    background: var(--bg-hover);
  }
  .option.on {
    border-color: var(--accent);
    background: color-mix(in oklab, var(--accent) 6%, transparent);
  }
  .option input {
    margin-top: 2px;
    accent-color: var(--accent);
    flex: none;
  }
  .option.danger.on {
    border-color: var(--danger);
    background: color-mix(in oklab, var(--danger) 7%, transparent);
  }

  .opt-body {
    display: flex;
    flex-direction: column;
    gap: 2px;
  }
  .opt-name {
    font-weight: 550;
  }
  .opt-desc {
    font-size: var(--text-sm);
    color: var(--fg-subtle);
    line-height: var(--leading-normal);
  }

  .warn {
    margin-top: var(--sp-3);
    padding: var(--sp-3);
    border-radius: var(--radius);
    background: color-mix(in oklab, #d97706 12%, transparent);
    color: color-mix(in oklab, #92400e 80%, var(--fg));
  }
  :global([data-theme='dark']) .warn {
    color: #fbbf24;
  }

  .error {
    margin-top: var(--sp-4);
  }
  .wide {
    width: 100%;
    height: 38px;
    margin-top: var(--sp-6);
  }
  .where {
    margin-top: var(--sp-3);
    text-align: center;
  }
  .where code {
    font-family: var(--font-mono);
    font-size: var(--text-xs);
  }
</style>
