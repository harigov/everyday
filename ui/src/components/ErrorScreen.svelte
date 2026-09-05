<script lang="ts">
  import { app } from '../lib/state.svelte'

  let retrying = $state(false)

  async function retry() {
    retrying = true
    try {
      await app.retry()
    } finally {
      retrying = false
    }
  }
</script>

<div class="wrap">
  <div class="card">
    <div class="mark">⚠</div>
    <h1>Every Day could not start</h1>

    {#if app.error}<p class="detail">{app.error}</p>{/if}

    <p class="reassure">
      Your journal has not been changed. Nothing was written, and nothing was
      deleted — this is a problem reaching the storage backend, not a problem
      with your entries.
    </p>

    <button class="btn btn-primary wide" onclick={retry} disabled={retrying}>
      {retrying ? 'Trying again…' : 'Try again'}
    </button>

    {#if app.boot?.defaultPath}
      <p class="hint">Your vault should be at <code>{app.boot.defaultPath}</code></p>
    {/if}
  </div>
</div>

<style>
  .wrap {
    display: grid; place-items: center; height: 100%; padding: var(--sp-6);
    background: var(--bg);
  }

  .card {
    width: min(400px, 100%);
    padding: var(--sp-8) var(--sp-6) var(--sp-6);
    text-align: center;
    background: var(--bg-raised);
    border: 1px solid var(--border);
    border-radius: var(--radius-xl);
    box-shadow: var(--shadow-lg);
  }

  .mark { font-size: 26px; color: var(--danger); margin-bottom: var(--sp-3); }

  h1 {
    font-family: var(--font-read);
    font-size: var(--text-xl); font-weight: 600; letter-spacing: -0.015em;
  }

  .detail {
    margin-top: var(--sp-4); padding: var(--sp-3);
    border-radius: var(--radius);
    background: color-mix(in oklab, var(--danger) 10%, transparent);
    color: var(--danger);
    font-size: var(--text-sm); line-height: var(--leading-normal);
    user-select: text;
  }

  .reassure {
    margin-top: var(--sp-4);
    font-size: var(--text-sm); line-height: var(--leading-normal);
    color: var(--fg-subtle);
  }

  .wide { width: 100%; height: 36px; margin-top: var(--sp-5); }

  .hint { margin-top: var(--sp-4); }
  .hint code {
    font-family: var(--font-mono); font-size: var(--text-xs);
    user-select: text;
  }
</style>
