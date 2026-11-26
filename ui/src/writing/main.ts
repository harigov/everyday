// First, before the theme is read: see `lib/legacy.ts`.
import '../lib/legacy'
import '../styles/fonts.css'
import '../styles/theme.css'
import '../styles/themes.css'
import WritingPopup from './WritingPopup.svelte'
import { bootWindow } from '../lib/window-boot'

// The same theme as the main window, which keeps the choice in the
// localStorage both windows share. The window is made once and reused, so it
// keeps following a change made while it is hidden.
export default bootWindow('writing', WritingPopup, { follow: true })
