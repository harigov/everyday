<script lang="ts">
  // "Loading…", said the same way everywhere.
  //
  // There were some forty bare "Loading…" strings before this, each in its
  // own size, colour and place -- a `.hint` here, a dimmed span there, a
  // spinner of its own in a third -- and a pane that was merely waiting
  // looked different in every app. This is one line: a small ring and the
  // words, quiet, announced to a screen reader once. A pane that has the
  // room centres it; a card, a rail or a menu passes `centred={false}`.

  let {
    label = 'Loading…',
    centred = false,
  }: {
    /** What is coming, in the reader's terms when there is something better
     *  than "Loading…": "Adding up your Karma…", "Asking this computer…". */
    label?: string
    /** Centre it across the space it is given, for an empty pane. */
    centred?: boolean
  } = $props()
</script>

<p class="loading" class:centred role="status" aria-live="polite">
  <span class="spinner" aria-hidden="true"></span>
  <span>{label}</span>
</p>

<style>
  .loading {
    display: flex;
    align-items: center;
    gap: var(--sp-2);
    margin: 0;
    padding: var(--sp-3) 0;
    font-size: var(--text-sm);
    color: var(--fg-subtle);
  }
  .loading.centred {
    justify-content: center;
    min-height: 40%;
    padding: var(--sp-8) var(--sp-4);
  }
</style>
