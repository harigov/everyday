<script lang="ts">
  // The assistant's dog, over the conversation, doing whatever the assistant
  // is doing.
  //
  // Drawn by the rail and by the Assistant app, which show the same thread,
  // so it reads the same store both do: `agent.turns`, and the act the
  // newest turn implies (see `headerAct`). The only thing kept here is a
  // clock, and only because a cheer has to end on time: the act changes by
  // itself when a turn finishes, and back again a couple of seconds later
  // with nothing having happened in between, so one timer is set for that
  // moment rather than the act being re-read every frame.
  //
  // Nothing until the settings have loaded. The choice of no dog is in them,
  // and a default dog that appeared for a moment and then vanished would be
  // the one thing worse than none.

  import { agent } from '../lib/agent.svelte'
  import { actChangesIn, headerAct, lookOf } from '../lib/companion'
  import Companion from './Companion.svelte'

  interface Props {
    width: number
    height: number
  }

  let { width, height }: Props = $props()

  let now = $state(Date.now())

  const newest = $derived(agent.turns.at(-1))
  const look = $derived(agent.settings ? lookOf(agent.settings.companion) : null)
  const act = $derived(
    headerAct({ ready: agent.ready, composing: agent.composing, turn: newest }, now),
  )

  // A turn has just ended (or a new one has begun): read the clock afresh,
  // or a cheer would be timed from whenever it was last read.
  $effect(() => {
    void newest?.phase
    void newest?.endedAt
    now = Date.now()
  })

  $effect(() => {
    const wait = actChangesIn(newest, now)
    if (wait === null) return
    const timer = setTimeout(() => (now = Date.now()), wait + 20)
    return () => clearTimeout(timer)
  })
</script>

{#if look}
  <Companion {look} {act} {width} {height} name={agent.displayName} />
{/if}
