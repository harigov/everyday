<script lang="ts">
  // The people an event is sent to, as chips: added by typing or pasting an
  // address, taken off with the × on each.
  //
  // The tag chips in `TaskDetail` are the pattern, with what an invitation
  // adds on top: a list pasted from somewhere else -- "a@x.com, Ann Lee
  // <b@y.com>" -- becomes a chip per person, an address that could not be
  // sent to is refused where it was typed rather than at Save, and a guest
  // who has answered says so with a small mark. When the mail app is in
  // this vault the people already written to are offered as you type, the
  // same suggestions the compose sheet makes.

  import { api } from '../lib/api'
  import { looksLikeEmail } from '../lib/guests'
  import { parseTypedAddress } from '../lib/mail'
  import { app } from '../lib/state.svelte'
  import Icon from './Icon.svelte'
  import type { Attendee, MailAddress } from '../lib/types'

  let {
    guests,
    id,
    disabled = false,
    typed = $bindable(''),
    onchange,
  }: {
    guests: Attendee[]
    /** For a `<label for>` outside. */
    id?: string
    disabled?: boolean
    /**
     * What is in the field and not yet a chip. Bindable, so the editor can
     * refuse to save over a half-typed address rather than quietly drop it.
     */
    typed?: string
    onchange: (guests: Attendee[]) => unknown
  } = $props()

  let problem = $state<string | null>(null)
  let suggestions = $state<MailAddress[]>([])
  let active = $state(0)
  let input = $state<HTMLInputElement | null>(null)
  /** Which lookup is current, so a slow answer for "an" cannot land over "ann". */
  let asked = 0

  const RESPONSE: Record<string, { label: string; mark: 'accepted' | 'declined' | 'tentative' }> = {
    accepted: { label: 'Accepted', mark: 'accepted' },
    declined: { label: 'Declined', mark: 'declined' },
    tentative: { label: 'Maybe', mark: 'tentative' },
  }

  const key = (email: string) => email.trim().toLowerCase()

  /**
   * Split a list somebody pasted: commas, semicolons and line breaks, but
   * not the ones inside a `<…>` -- which never hold one in an address, and
   * would cut "Lee <ann@x.com>" in two if they did.
   */
  function splitList(text: string): string[] {
    const out: string[] = []
    let depth = 0
    let part = ''
    for (const ch of text) {
      if (ch === '<') depth += 1
      if (ch === '>') depth = Math.max(0, depth - 1)
      if (depth === 0 && (ch === ',' || ch === ';' || ch === '\n')) {
        out.push(part)
        part = ''
      } else {
        part += ch
      }
    }
    out.push(part)
    return out.map((p) => p.trim()).filter(Boolean)
  }

  /**
   * Turn what was typed into chips. Whatever could not be read as an
   * address stays in the field, with a line saying so, so it can be fixed
   * rather than retyped.
   */
  function commit(text: string) {
    const refused: string[] = []
    const next = [...guests]
    for (const part of splitList(text)) {
      const address = parseTypedAddress(part)
      if (!address || !looksLikeEmail(address.email)) {
        refused.push(part)
        continue
      }
      if (next.some((g) => key(g.email) === key(address.email))) continue
      next.push({ email: address.email.trim(), name: address.name.trim() })
    }
    if (next.length !== guests.length) onchange(next)
    typed = refused.join(', ')
    suggestions = []
    problem =
      refused.length === 0
        ? null
        : refused.length === 1
          ? `“${refused[0]}” is not an email address.`
          : `${refused.length} of those are not email addresses.`
  }

  function add(address: MailAddress) {
    if (!guests.some((g) => key(g.email) === key(address.email))) {
      onchange([...guests, { email: address.email.trim(), name: address.name.trim() }])
    }
    typed = ''
    problem = null
    suggestions = []
    input?.focus()
  }

  function remove(email: string) {
    onchange(guests.filter((g) => key(g.email) !== key(email)))
  }

  async function lookUp(text: string) {
    const prefix = text.trim()
    const mine = ++asked
    if (!prefix || !app.supportsMail || prefix.includes(',')) {
      suggestions = []
      return
    }
    try {
      const found = await api.suggestAddresses(prefix, 6)
      if (mine !== asked) return
      // Never offer somebody already on the list.
      suggestions = found.filter((s) => !guests.some((g) => key(g.email) === key(s.email)))
      active = 0
    } catch {
      // A suggestion is a convenience; failing to fetch one is not an error.
      if (mine === asked) suggestions = []
    }
  }

  /**
   * Enter, a comma or a semicolon commit what was typed; with suggestions
   * showing, the arrows move through them and Enter takes one. Backspace in
   * an empty field takes back the last chip, as in every address field.
   */
  function onKeydown(e: KeyboardEvent) {
    const open = suggestions.length > 0
    if (open && (e.key === 'ArrowDown' || e.key === 'ArrowUp')) {
      e.preventDefault()
      const step = e.key === 'ArrowDown' ? 1 : -1
      active = (active + step + suggestions.length) % suggestions.length
      return
    }
    if (open && e.key === 'Enter') {
      e.preventDefault()
      const chosen = suggestions[active]
      if (chosen) add(chosen)
      return
    }
    if (open && e.key === 'Escape') {
      e.preventDefault()
      e.stopPropagation()
      suggestions = []
      return
    }
    if (e.key === 'Enter' || e.key === ',' || e.key === ';') {
      e.preventDefault()
      if (typed.trim()) commit(typed)
      return
    }
    if (e.key === 'Backspace' && !typed && guests.length > 0) {
      e.preventDefault()
      onchange(guests.slice(0, -1))
    }
  }

  /** A pasted list goes straight in, a chip per address. */
  function onPaste(e: ClipboardEvent) {
    const text = e.clipboardData?.getData('text') ?? ''
    if (!/[,;\n]/.test(text)) return
    e.preventDefault()
    commit(`${typed}${text}`)
  }

  /**
   * Leaving the field with an address in it keeps it, as a chip -- but not
   * a half-typed name, which is still a search. The suggestions close a
   * beat later, so a click on one still lands.
   */
  function onBlur() {
    if (typed.includes('@')) commit(typed)
    setTimeout(() => {
      if (document.activeElement !== input) suggestions = []
    }, 150)
  }
</script>

<div class="guests">
  <div class="chips" class:disabled>
    {#each guests as guest (key(guest.email))}
      {@const answer = guest.response ? RESPONSE[guest.response] : undefined}
      <span class="tag guest" title={guest.name ? `${guest.name} <${guest.email}>` : guest.email}>
        {#if answer}
          <span
            class="mark {answer.mark}"
            role="img"
            title={answer.label}
            aria-label={answer.label}
          >
            {#if answer.mark === 'accepted'}
              <Icon name="tick" size={10} weight={2.4} />
            {:else if answer.mark === 'declined'}
              <Icon name="close" size={10} weight={2.4} />
            {:else}
              ?
            {/if}
          </span>
        {/if}
        <span class="who">{guest.name || guest.email}</span>
        <button
          type="button"
          class="x"
          title="Remove {guest.name || guest.email}"
          aria-label="Remove {guest.name || guest.email}"
          {disabled}
          onclick={() => remove(guest.email)}
        >
          <Icon name="close" size={10} weight={2} />
        </button>
      </span>
    {/each}
    <input
      {id}
      bind:this={input}
      class="guestin"
      placeholder={guests.length === 0 ? 'Add people by email' : 'Add another'}
      autocomplete="off"
      spellcheck="false"
      aria-autocomplete="list"
      aria-expanded={suggestions.length > 0}
      {disabled}
      bind:value={typed}
      oninput={(e) => {
        problem = null
        void lookUp(e.currentTarget.value)
      }}
      onkeydown={onKeydown}
      onpaste={onPaste}
      onblur={onBlur}
    />
  </div>

  {#if suggestions.length > 0}
    <div class="suggestions" role="listbox" aria-label="People you have written to">
      {#each suggestions as s, i (s.email)}
        <button
          type="button"
          class="suggestion"
          class:active={i === active}
          role="option"
          aria-selected={i === active}
          onmousedown={(e) => e.preventDefault()}
          onmouseenter={() => (active = i)}
          onclick={() => add(s)}
        >
          <span class="s-name">{s.name || s.email}</span>
          {#if s.name}<span class="s-email">{s.email}</span>{/if}
        </button>
      {/each}
    </div>
  {/if}

  {#if problem}<p class="problem">{problem}</p>{/if}
</div>

<style>
  .guests {
    position: relative;
    display: flex;
    flex-direction: column;
    gap: 4px;
    min-width: 0;
  }

  .chips {
    display: flex;
    flex-wrap: wrap;
    align-items: center;
    gap: var(--sp-1);
  }
  .chips.disabled {
    opacity: 0.45;
  }

  /* The shared `.tag`, with room for a mark before the name and the × after. */
  .guest {
    gap: 3px;
    max-width: 100%;
    padding-right: 3px;
  }
  .who {
    overflow: hidden;
    text-overflow: ellipsis;
    white-space: nowrap;
  }
  .mark {
    display: flex;
    font-size: var(--text-xs);
    font-weight: 700;
    line-height: 1;
  }
  .mark.accepted {
    color: var(--accent);
  }
  .mark.declined {
    color: var(--danger);
  }
  .mark.tentative {
    color: var(--fg-subtle);
  }
  .x {
    display: flex;
    color: var(--fg-faint);
    border-radius: 99px;
    padding: 2px;
  }
  .x:hover {
    color: var(--danger);
    background: var(--bg-hover);
  }

  .guestin {
    flex: 1;
    min-width: 120px;
    height: 22px;
    padding: 0 var(--sp-2);
    border: 1px dashed var(--border-strong);
    border-radius: 99px;
    background: none;
    font-size: var(--text-xs);
    user-select: text;
  }
  .guestin:focus {
    outline: none;
    border-style: solid;
    border-color: var(--accent);
  }

  /* Over whatever is below, rather than pushing the description down a row
     at a time as the list fills. */
  .suggestions {
    position: absolute;
    top: calc(100% + 2px);
    left: 0;
    right: 0;
    z-index: 30;
    display: flex;
    flex-direction: column;
    max-height: 240px;
    overflow-y: auto;
    padding: 4px;
    border: 1px solid var(--border);
    border-radius: var(--radius);
    background: var(--bg-raised);
    box-shadow: var(--shadow-lg);
  }
  .suggestion {
    display: flex;
    flex-direction: column;
    padding: 5px var(--sp-2);
    border-radius: var(--radius-sm);
    text-align: left;
    min-width: 0;
  }
  .suggestion.active {
    background: var(--bg-hover);
  }
  .s-name,
  .s-email {
    overflow: hidden;
    text-overflow: ellipsis;
    white-space: nowrap;
  }
  .s-name {
    font-size: var(--text-sm);
  }
  .s-email {
    font-size: var(--text-xs);
    color: var(--fg-faint);
  }

  .problem {
    font-size: var(--text-xs);
    color: var(--danger);
  }
</style>
