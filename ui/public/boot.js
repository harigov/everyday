// The theme, on the page before the application has loaded.
//
// The application applies the stored theme itself once it starts
// (`app.start`), which is after its whole script has been fetched and run --
// and until then the window showed the page in `index.html`'s light theme, a
// white flash at every launch for anybody on a dark one. This runs first, as
// a plain script (an inline one would need the CSP loosened), and does
// exactly what `themes.ts` does with the stored choice: a theme it knows by
// id, and otherwise -- `system`, nothing stored, a theme since removed --
// Paper or Ink by the desktop. `scripts/themes.test.mjs` holds the two to
// the same answers, and the list below to `THEMES`.
;(function () {
  const THEMES = [
    'light',
    'lagoon',
    'blossom',
    'meadow',
    'dune',
    'mica',
    'dark',
    'aurora',
    'ember',
    'dusk',
    'tide',
    'mica-dark',
  ]
  let theme = null
  try {
    theme = localStorage.getItem('jatayu.theme')
  } catch {
    // No storage: the desktop's choice, as with nothing stored.
  }
  if (!THEMES.includes(theme)) {
    const dark =
      typeof matchMedia === 'function' && matchMedia('(prefers-color-scheme: dark)').matches
    theme = dark ? 'dark' : 'light'
  }
  document.documentElement.setAttribute('data-theme', theme)
})()
