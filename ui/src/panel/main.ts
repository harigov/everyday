// The panel: the cards in the corner of the screen -- a meeting about to
// start, voice typing listening, a conversation from the talk key. Its own
// window, like writing help's popup: the main window may be closed or on
// another workspace, and the panel has to sit in the corner anyway. See
// `crates/karmayoga-app/src/panel/`.
import '../lib/legacy'
import '../styles/fonts.css'
import '../styles/theme.css'
import '../styles/themes.css'
import PanelStack from './PanelStack.svelte'
import { MOCK } from './source'
import { bootWindow } from '../lib/window-boot'

// A review build in a browser takes `?theme=`, as the main interface does, so
// the cards can be looked at in each theme without a Settings page to find.
const forced = MOCK ? new URLSearchParams(location.search).get('theme') : null

// No desktop behind the page in a browser: give it the theme's backdrop.
if (MOCK) document.body.classList.add('mock')

// This window outlives every card it draws -- it is only hidden between them
// -- so it follows the main window when the theme is changed there, and the
// desktop when it goes dark (`follow`).
export default bootWindow('panel', PanelStack, { forced, follow: true })
