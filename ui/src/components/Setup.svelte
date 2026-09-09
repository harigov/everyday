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

  /**
   * What has been typed into each backend's fields, keyed by backend then by
   * field. Kept per backend rather than per field so that clicking away to
   * look at another option and coming back does not lose a pasted
   * connection URL.
   */
  let settings = $state<Record<string, Record<string, string>>>({})

  const backends = $derived(app.boot?.backends ?? [])
  const path = $derived(app.boot?.defaultPath ?? '')

  /** The fields the chosen backend asked for. Empty for a local vault. */
  const fields = $derived(backends.find((b) => b.id === backend)?.settings ?? [])

  function value(key: string): string {
    return settings[backend]?.[key] ?? ''
  }

  function setValue(key: string, v: string) {
    settings[backend] = { ...(settings[backend] ?? {}), [key]: v }
  }

  const missingField = $derived(
    fields.find((f) => f.required && !value(f.key).trim())?.label ?? null,
  )

  /**
   * Say what is still missing, but only once they have started.
   *
   * The same rule the password field follows: an empty form on first sight
   * is not a mistake anyone has made yet, and telling someone they have got
   * it wrong before they have touched it is not help. The Create button is
   * disabled either way.
   */
  const missingHint = $derived(
    missingField && Object.values(settings[backend] ?? {}).some((v) => v.trim())
      ? missingField
      : null,
  )

  const problem = $derived.by(() => {
    if (!encrypt) return acknowledged ? null : 'Confirm you understand the risk below.'
    if (password.length < 8) return password ? 'Use at least 8 characters.' : null
    if (confirm && password !== confirm) return 'The passwords do not match.'
    return null
  })

  const ready = $derived(
    !busy &&
      !problem &&
      !missingField &&
      (!encrypt || (password.length >= 8 && password === confirm)),
  )

  async function submit(e: Event) {
    e.preventDefault()
    if (!ready) return
    busy = true
    try {
      // Only the chosen backend's fields, and only the ones with something
      // in them: an empty optional field means "use the default", not "set
      // this to the empty string".
      const chosen: Record<string, string> = {}
      for (const f of fields) {
        const v = value(f.key).trim()
        if (v) chosen[f.key] = v
      }
      await app.createVault({
        path,
        name: name.trim() || 'My Journal',
        backend,
        settings: chosen,
        password: encrypt ? password : null,
      })
      // Only on the way out. The credentials have been sealed into the
      // vault, so there is no reason for a copy to stay in the webview's
      // memory -- but clearing them in a `finally` would also clear them
      // when the create *failed*, which is precisely when they are still
      // needed: a typo in the host is the common error here, and answering
      // it by emptying the form makes the correction cost a whole
      // credential and a re-chosen password.
      settings = {}
      password = ''
      confirm = ''
    } catch {
      // `createVault` re-throws so a caller can react to the failure. This
      // one reacts by leaving the form as it is -- the message is already
      // in `app.error` and is drawn under the fields. Swallowed rather than
      // left to escape, because a rejected handler is an unhandled
      // rejection and not a second way of saying the same thing.
    } finally {
      busy = false
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
            <span class="opt-name">{b.name}</span>
            <span class="opt-desc">{b.description}</span>
          </span>
        </label>
      {/each}
    </div>

    <!-- Whatever the chosen backend asked for. Nothing at all for a vault
         that lives in a folder on this computer, which is why this is driven
         by the backend's own declaration rather than by a branch on its id. -->
    {#each fields as f (backend + f.key)}
      <label class="label" for="set-{f.key}">
        {f.label}{#if !f.required}<span class="opt-desc"> — optional</span>{/if}
      </label>
      <input
        id="set-{f.key}"
        class="field"
        type={f.secret ? 'password' : 'text'}
        placeholder={f.placeholder}
        autocomplete="off"
        spellcheck="false"
        value={value(f.key)}
        oninput={(e) => setValue(f.key, e.currentTarget.value)}
      />
      <div class="gap"></div>
    {/each}

    {#if fields.length}
      <p class="hint">
        Entries are sealed on this computer before they are sent, so the server holds ciphertext and
        never your password. What it can see is the shape of the journal: how many entries there are
        and which days you wrote on.
      </p>
    {/if}

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

    {#if missingHint}<p class="error">{missingHint} is needed.</p>{/if}
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
