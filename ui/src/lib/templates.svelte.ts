// Templates: one list for the journal, notes and meeting notes.
//
// Loaded once after unlock, the way roles are, because three places want it
// -- Settings → Templates edits it, a journal's settings pick from it, and
// the notes app and both editors offer it -- and none of them should have to
// ask again. Applying a template is not here: `newEntry` and `newNote` fill
// one in on the Rust side, and "Insert template" asks for a rendering.

import type { Editor } from '@tiptap/core'
import { api } from './api'
import { app, handle } from './state.svelte'
import { mergeTags } from './tags'
import type { JournalId, RenderedTemplate, Template, TemplateId, TemplateUse } from './types'

/** How each place a template is offered is named on screen. */
export const USE_LABELS: Record<TemplateUse, string> = {
  journal: 'Journal',
  notes: 'Notes',
  meetings: 'Meeting notes',
}

export const ALL_USES: TemplateUse[] = ['journal', 'notes', 'meetings']

class TemplatesState {
  all = $state.raw<Template[]>([])
  loaded = $state(false)
  #generation = 0

  constructor() {
    app.onLock(() => this.reset())
  }

  reset() {
    this.#generation += 1
    this.all = []
    this.loaded = false
  }

  /** Whether this vault's backend keeps templates -- exactly when it keeps notes. */
  get enabled(): boolean {
    return app.supportsNotes
  }

  /** The templates offered for one place, oldest first. */
  for(place: TemplateUse): Template[] {
    return this.all.filter((t) => t.uses.includes(place))
  }

  byId(id: TemplateId | null | undefined): Template | undefined {
    return id ? this.all.find((t) => t.id === id) : undefined
  }

  /** Read the list. Quiet when the backend has no templates. */
  async load(): Promise<void> {
    if (!this.enabled) return
    const token = ++this.#generation
    try {
      const all = await api.templates()
      if (token !== this.#generation) return
      this.all = all
      this.loaded = true
    } catch (e) {
      await handle(e)
    }
  }

  /** Read it the first time something asks; after that, what is held. */
  async ensure(): Promise<void> {
    if (!this.loaded) await this.load()
  }

  /** For a change raised elsewhere: only worth reading if it has been read. */
  liveRefresh(): Promise<void> | void {
    return this.loaded ? this.load() : undefined
  }

  /** A template with an id and starting text, not yet saved. */
  draft(uses: TemplateUse[]): Promise<Template> {
    return api.newTemplate(uses)
  }

  async save(template: Template): Promise<void> {
    await api.saveTemplate({ ...template, updatedAt: new Date().toISOString() })
    await this.load()
  }

  async remove(id: TemplateId): Promise<void> {
    await api.deleteTemplate(id)
    await this.load()
  }

  lint(template: Template): Promise<string[]> {
    return api.lintTemplate(template)
  }

  /** Filled in for a day, as "Insert template" and the preview show it. */
  render(
    template: Template,
    date: string | null = null,
    journalId: JournalId | null = null,
  ): Promise<RenderedTemplate> {
    return api.renderTemplate(template, date, journalId)
  }

  /** A meeting template's preview: the real model, on a sample call. */
  previewMeeting(body: string): Promise<string> {
    return api.previewMeetingTemplate(body)
  }
}

export const templates = new TemplatesState()

/**
 * Put a filled-in template at the caret: what "Insert template" does in the
 * journal and in a note alike. Inserting adds to what was written and never
 * replaces it -- a title is taken only into an empty one, and tags are added
 * to those already there. The caller saves.
 */
export function insertFilled(
  editor: Editor,
  target: { title: string; tags: string[] },
  filled: RenderedTemplate,
) {
  editor
    .chain()
    .focus()
    .insertContent(filled.body.content ?? [])
    .run()
  if (!target.title.trim() && filled.title) target.title = filled.title
  target.tags = mergeTags(target.tags, filled.tags)
}
