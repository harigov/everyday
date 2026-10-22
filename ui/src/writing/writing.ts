// Writing help's popup: the parts that are not drawing.
//
// The popup is its own page in its own window -- see `crates/everyday-app/
// src/writing/mod.rs` for why -- so it carries none of the main interface:
// no stores, no router, no mock backend. What it needs to decide is here,
// where `scripts/writing.test.mjs` can check it without a window.

import type { WritingAction } from '../lib/types'

/** What the shell hands the popup: one press of the writing shortcut. */
export interface Selection {
  seq: number
  text: string
  app?: string | null
  /** `overlay`: drawn at `x`/`y` inside a transparent window covering the desktop.
   *  `window`: the whole of a small window already at the pointer. */
  mode: 'overlay' | 'window'
  x?: number | null
  y?: number | null
  problem?: string | null
}

export interface Choice {
  action: WritingAction
  label: string
  /** The digit that picks it from the keyboard. */
  key: string
  /** What it is working on, while it works. */
  doing: string
}

export const CHOICES: readonly Choice[] = [
  { action: 'improve', label: 'Improve this', key: '1', doing: 'Improving' },
  { action: 'grammar', label: 'Correct grammar', key: '2', doing: 'Correcting' },
  { action: 'concise', label: 'Make it concise', key: '3', doing: 'Shortening' },
]

/** The choice a key picks, if it picks one. */
export function choiceForKey(key: string): Choice | undefined {
  return CHOICES.find((c) => c.key === key)
}

/**
 * Where to put a `size` popup in a `viewport`, given the pointer at `at`:
 * just below and right of it, flipped to the other side of the pointer where
 * it would run off the edge, and never off-screen. No pointer: centred.
 */
export function place(
  at: { x: number; y: number } | null,
  viewport: { w: number; h: number },
  size: { w: number; h: number },
  gap = 12,
): { left: number; top: number } {
  const margin = 8
  const clamp = (v: number, lo: number, hi: number) => Math.max(lo, Math.min(v, Math.max(lo, hi)))
  if (!at) {
    return {
      left: clamp((viewport.w - size.w) / 2, margin, viewport.w - size.w - margin),
      top: clamp((viewport.h - size.h) / 3, margin, viewport.h - size.h - margin),
    }
  }
  let left = at.x + gap
  let top = at.y + gap
  if (left + size.w > viewport.w - margin) left = at.x - gap - size.w
  if (top + size.h > viewport.h - margin) top = at.y - gap - size.h
  return {
    left: clamp(left, margin, viewport.w - size.w - margin),
    top: clamp(top, margin, viewport.h - size.h - margin),
  }
}

/** The first line or so of the selection, for the popup to show what it is about to work on. */
export function preview(text: string, max = 90): string {
  const flat = text.replace(/\s+/g, ' ').trim()
  return flat.length <= max ? flat : `${flat.slice(0, max - 1).trimEnd()}…`
}

/** What went wrong, said for somebody who only pressed a shortcut. */
export interface Trouble {
  message: string
  /** The fix is in the main window: unlock it, or set up a model. */
  openMain: boolean
}

export function trouble(code: string, message: string): Trouble {
  switch (code) {
    case 'no_vault':
      return { message: 'Open your vault in Every Day first.', openMain: true }
    case 'locked':
      return { message: 'Every Day is locked. Unlock it, then try again.', openMain: true }
    default:
      break
  }
  if (/error sending request|connection refused|dns error|timed out connecting/i.test(message)) {
    return {
      message: 'The medium model could not be reached. Check it in Settings → Models.',
      openMain: true,
    }
  }
  if (/medium model|Settings → Models|no API key/i.test(message)) {
    return { message: `${message}.`.replace(/\.\.$/, '.'), openMain: true }
  }
  const said = message.charAt(0).toUpperCase() + message.slice(1)
  return { message: /[.!?]$/.test(said) ? said : `${said}.`, openMain: false }
}
