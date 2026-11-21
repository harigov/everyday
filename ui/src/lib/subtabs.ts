// The one rule every page drawn with `SubTabs.svelte` shares.

/**
 * Which tab to draw: the one picked, unless it is not offered any more --
 * Mail unticked on an account, Security on a vault with no password -- and
 * then the first that is. Every page with tabs that come and go keeps its
 * pick and draws this, so a tab coming back finds the pick still there.
 */
export function shownTab<T extends string>(tabs: { id: T }[], picked: T): T {
  return tabs.some((t) => t.id === picked) ? picked : (tabs[0]?.id ?? picked)
}
