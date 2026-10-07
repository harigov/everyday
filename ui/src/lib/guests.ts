// The one rule an event's guest list is checked against before it goes to a
// calendar's server. Its own module, rather than an export from
// `GuestsField.svelte`'s module script, so the editor and the field can both
// import it with its type intact.

/**
 * Could a calendar send an invitation to this?
 *
 * As loose as the core's own test, on purpose -- one `@` with something
 * either side and no spaces -- because the server is the real judge, and a
 * stricter rule here would only turn away addresses that work.
 */
export function looksLikeEmail(text: string): boolean {
  return /^[^\s@]+@[^\s@]+$/.test(text.trim())
}
