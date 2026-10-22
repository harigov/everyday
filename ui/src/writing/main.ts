import '../styles/fonts.css'
import '../styles/theme.css'
import { mount } from 'svelte'
import WritingPopup from './WritingPopup.svelte'

// The same light/dark choice as the main window, which keeps it in the
// localStorage both windows share. `system` is the absence of the attribute.
const theme = localStorage.getItem('everyday.theme')
if (theme === 'light' || theme === 'dark')
  document.documentElement.setAttribute('data-theme', theme)

const target = document.getElementById('writing')
if (!target) throw new Error('#writing is missing from writing.html')

export default mount(WritingPopup, { target })
