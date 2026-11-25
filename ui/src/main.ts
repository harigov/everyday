// First: an installation of Every Day's preferences move before any store
// reads its own. See `lib/legacy.ts`.
import './lib/legacy'
import './styles/app.css'
import { mount } from 'svelte'
import App from './App.svelte'

// "ResizeObserver loop completed with undelivered notifications": a list that
// measures its rows as they are drawn (virtua, behind `VirtualList`) changes
// size in the same frame it is measured in, the browser puts off telling it
// until the next, and WebKit reports that deferral as an uncaught error.
// Nothing is lost -- the next frame delivers it -- so it is not let through
// as one.
window.addEventListener('error', (e) => {
  if (e.message?.includes('ResizeObserver loop')) {
    e.preventDefault()
    e.stopImmediatePropagation()
  }
})

const target = document.getElementById('app')
if (!target) throw new Error('#app is missing from index.html')

export default mount(App, { target })
