// Voice typing's pill. Its own window, like writing help's popup and the meeting
// card: the main window may be closed or on another workspace, and the pill
// has to sit at the bottom of the screen anyway.
import '../lib/legacy'
import '../styles/fonts.css'
import '../styles/theme.css'
import '../styles/themes.css'
import { mount } from 'svelte'
import VoiceTypingPill from './VoiceTypingPill.svelte'
import { parseThemeChoice, resolveTheme, showTheme } from '../lib/themes'

showTheme(resolveTheme(parseThemeChoice(localStorage.getItem('karmayoga.theme'))))

const target = document.getElementById('voice-typing')
if (!target) throw new Error('#voice-typing is missing from voice-typing.html')

export default mount(VoiceTypingPill, { target })
