// Voice typing's paste window. Never seen: what voice typing shows while it
// listens is a card in the panel (`src/panel/`), and this window survives only
// as the one that may set the clipboard on Wayland, shown for the moment a
// dictation is typed and hidden again. Its page draws nothing. See
// `crates/jatayu-app/src/voicetyping/paste.rs`.
import '../lib/legacy'
import '../styles/fonts.css'
import '../styles/theme.css'
import '../styles/themes.css'
import VoiceTypingPill from './VoiceTypingPill.svelte'
import { bootWindow } from '../lib/window-boot'

export default bootWindow('voice-typing', VoiceTypingPill, { follow: true })
