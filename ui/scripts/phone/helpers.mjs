// What scenarios share: see scenarios.mjs.

/** A finger's press on a phone; a click in a `--desktop` run, which has no
 *  touchscreen. */
export async function press(locator) {
  if (globalThis.PHONE_CHECK_DESKTOP) await locator.click()
  else await locator.tap()
}

/** The first element matching `selector`, tapped as a finger would. */
export async function tap(page, selector) {
  await press(page.locator(selector).first())
}

/** The phone's back button: what Android sends is a history step back. */
export async function back(page) {
  await page.evaluate(() => history.back())
}

/** The first visible element with this text, tapped. */
export async function tapText(page, text) {
  await press(page.getByText(text, { exact: false }).first())
}
