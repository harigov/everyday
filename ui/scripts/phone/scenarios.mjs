// What `check.mjs` opens on each app to reach every screen worth checking on
// a phone. One list per app (the key is its section id), each scenario a
// fresh page on that app, unlocked, before `run` does anything:
//
//   { name: 'thread', run: async ({ page, settle }) => { ... } }
//
// `section` overrides the app the page opens on. `run` gets the Playwright
// page and `settle(ms)`, which waits for loads and transitions to finish.
// Use tap() rather than click() for anything a finger presses: it is what a
// phone sends, and some handlers listen for pointer type.
//
// Prefer selectors a person would recognise -- roles and labels -- over
// class names, which a restyle changes.

/** The first element matching `selector`, tapped. */
export async function tap(page, selector) {
  await page.locator(selector).first().tap()
}

const home = { name: 'home' }

export const SCENARIOS = {
  overview: [home],
  journal: [home],
  notes: [home],
  todo: [home],
  calendar: [home],
  mail: [home],
  library: [home],
  feed: [home],
  files: [home],
  health: [home],
  money: [home],
  screentime: [home],
  assistant: [home],
}
