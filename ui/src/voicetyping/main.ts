// Voice typing's paste window. Never seen: what voice typing shows while it
// listens is a card in the panel (`src/panel/`), and this window survives only
// as the one that may set the clipboard on Wayland, shown for the moment a
// dictation is typed and hidden again. Its page draws nothing. See
// `crates/karmayoga-app/src/voicetyping/paste.rs`.
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
