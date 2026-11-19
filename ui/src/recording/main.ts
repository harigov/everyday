// The meeting card. Its own window, the way writing help's popup is: the
// main window may be closed or on another workspace, and the card has to
// sit in the corner of the screen anyway.
import '../lib/legacy'
import '../styles/fonts.css'
import '../styles/theme.css'
import '../styles/themes.css'
import { mount } from 'svelte'
import RecordingCard from './RecordingCard.svelte'
import { parseThemeChoice, resolveTheme, showTheme } from '../lib/themes'

showTheme(resolveTheme(parseThemeChoice(localStorage.getItem('karmayoga.theme'))))

const target = document.getElementById('recording')
if (!target) throw new Error('#recording is missing from recording.html')

export default mount(RecordingCard, { target })
