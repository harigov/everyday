import '../styles/fonts.css'
import '../styles/theme.css'
import '../styles/themes.css'
import { mount } from 'svelte'
import WritingPopup from './WritingPopup.svelte'
import { parseThemeChoice, resolveTheme, showTheme } from '../lib/themes'

// The same theme as the main window, which keeps the choice in the
// localStorage both windows share.
showTheme(resolveTheme(parseThemeChoice(localStorage.getItem('everyday.theme'))))

const target = document.getElementById('writing')
if (!target) throw new Error('#writing is missing from writing.html')

export default mount(WritingPopup, { target })
