<script lang="ts">
  let {
    size = 20,
    tile = false,
  }: {
    size?: number
    /** Draw the full app tile. Otherwise the mark alone, in `currentColor`. */
    tile?: boolean
  } = $props()

  // Unique per instance: two gradients with the same id on one page collapse
  // into whichever the browser saw first.
  const uid = $props.id()
</script>

<!--
  The same sunrise as the application icon (crates/everyday-app/icons/icon.svg),
  redrawn on a 24-unit grid. It is not a scaled-down copy: the bars are
  proportionally heavier and the gaps wider, because at 20px the icon's own
  proportions close up into a smudge.
-->
<svg
  class="logo"
  width={size}
  height={size}
  viewBox="0 0 24 24"
  fill="none"
  aria-hidden="true"
  focusable="false"
>
  {#if tile}
    <defs>
      <linearGradient id="sky-{uid}" x1=".12" y1="0" x2=".88" y2="1">
        <stop offset="0" stop-color="#ffc768" />
        <stop offset=".42" stop-color="#f18a2b" />
        <stop offset="1" stop-color="#ab440a" />
      </linearGradient>
    </defs>
    <rect width="24" height="24" rx="5.4" fill="url(#sky-{uid})" />
    <g fill="#fff9f2">
      <circle cx="12" cy="9" r="3.9" />
      <rect x="2.6" y="11.8" width="18.8" height="1.5" rx=".75" />
      <rect x="5.1" y="14.9" width="13.8" height="1.5" rx=".75" opacity=".74" />
      <rect x="7.6" y="18" width="8.8" height="1.5" rx=".75" opacity=".5" />
    </g>
  {:else}
    <g fill="currentColor">
      <circle cx="12" cy="8.1" r="4.4" />
      <rect x="2" y="11.2" width="20" height="2.4" rx="1.2" />
      <rect x="5" y="15.1" width="14" height="2.4" rx="1.2" opacity=".62" />
      <rect x="8" y="19" width="8" height="2.4" rx="1.2" opacity=".38" />
    </g>
  {/if}
</svg>

<style>
  .logo { display: block; flex: none; }
</style>
