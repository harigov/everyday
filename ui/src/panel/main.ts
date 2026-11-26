// The panel: the cards Karma Yoga shows over whatever the person is looking at
// -- a meeting coming up, one being recorded, new mail, notes written, the
// assistant talking or asking, voice typing listening. Its own window, like
// writing help's popup: the main window may be closed or on another workspace,
// and the panel has to be on screen anyway. See
// `crates/karmayoga-app/src/panel/`.
import '../lib/legacy'
import '../styles/fonts.css'
import '../styles/theme.css'
import '../styles/themes.css'
import './panel.css'
import PanelStack from './PanelStack.svelte'
import { MOCK } from './source'
import { bootWindow } from '../lib/window-boot'

// A review build in a browser takes `?theme=`, as the main interface does, so
// the cards can be looked at in each theme without a Settings page to find.
const forced = MOCK ? new URLSearchParams(location.search).get('theme') : null

// No desktop behind the page in a browser: give it the theme's backdrop.
if (MOCK) document.body.classList.add('mock')

// The theme stored by the main window is the first guess, shown before the
// page draws. From then on `PanelStack` follows it: the shell's word on which
// theme the main window shows comes first (`panel_theme`, `THEME_EVENT`) --
// the panel may be a process of its own with nothing stored -- then the stored
// choice and the desktop's light or dark. So `follow` is off here, or a change
// of the desktop's scheme would paint over the theme the shell asked for.
export default bootWindow('panel', PanelStack, { forced, follow: false })
