<script lang="ts">
  // Who the vault belongs to: the About You tab.
  //
  // Two halves, here together because they answer one question. First, six
  // fields the assistant reads. Then the roles -- the parts of a life this
  // vault files things under, which used to be defined in the Overview's
  // sidebar and are a standing fact about the person rather than one app's
  // data. See `RolesPanel` for why they moved.
  //
  // The halves save differently, and that is deliberate rather than untidy.
  // The fields are a form with an explicit Save, because a half-typed
  // birthday must never be written; a role is a record, and renaming one is a
  // write in the same way renaming a project is. So they are two tabs rather
  // than one column: the roles used to sit under the Save, where the button
  // read as being for a list that had already saved itself, and the page grew
  // tall enough that the Save ended up in the middle of the scroll. Now the
  // Save is pinned to the foot of the fields' tab and the roles tab has none.
  //
  // Six fields, and the reason they exist is one sentence at the top: the
  // assistant was being asked to be a secretary while knowing nothing about
  // the person it worked for. Not their name, not their age, not what they do
  // all day, not which city's weather matters. Every one of those changes the
  // answer to an ordinary question, and none of them is something a model
  // should be inferring from the contents of somebody's journal.
  //
  // Edited on a draft with an explicit Save, like the assistant's own tab, so
  // that a half-typed birthday is never written.

  import { api } from '../lib/api'
  import { app } from '../lib/state.svelte'
  import RolesPanel from './RolesPanel.svelte'
  import SubTabs from './SubTabs.svelte'
  import type { Profile } from '../lib/types'

  let draft = $state<Profile | null>(null)
  let notice = $state<string | null>(null)
  let saving = $state(false)
  let noticeTimer: ReturnType<typeof setTimeout> | null = null
  let sub = $state<'about' | 'roles'>('about')

  // Loaded here rather than in a store: nothing else in the interface draws
  // it yet, and a store would be a singleton kept in memory for one dialog.
  void (async () => {
    try {
      draft = await api.profile()
    } catch {
      // A profile that cannot be read leaves the fields empty and the Save
      // button honest about what it will do.
      draft = {
        firstName: '',
        lastName: '',
        born: null,
        gender: '',
        location: '',
        about: '',
      }
    }
  })()

  function touched() {
    if (noticeTimer) clearTimeout(noticeTimer)
    noticeTimer = null
    notice = null
  }

  async function save() {
    if (!draft) return
    touched()
    saving = true
    try {
      draft = await api.saveProfile($state.snapshot(draft))
      notice = 'Saved.'
      noticeTimer = setTimeout(() => (notice = null), 2400)
    } catch (e) {
      notice = e instanceof Error ? e.message : String(e)
    } finally {
      saving = false
    }
  }
</script>

{#if draft}
  <!-- Roles only where the Overview is, as before: with no tabs to pick
       between, the fields are the whole page. -->
  {#if app.supportsOverview}
    <SubTabs
      tabs={[
        { id: 'about', label: 'About you' },
        { id: 'roles', label: 'Roles' },
      ]}
      bind:value={sub}
      label="About you"
    />
  {/if}

  {#if sub === 'about' || !app.supportsOverview}
    <section>
      <p class="lead">
        The assistant reads all of this, every time you talk to it and every time one of its
        routines runs. It is for the handful of things that do not change — anything that does, tell
        it and it will remember.
      </p>
    </section>

    <section>
      <span class="eyebrow">Name</span>
      <div class="pair">
        <input
          class="field"
          placeholder="First name"
          bind:value={draft.firstName}
          oninput={touched}
          autocomplete="given-name"
        />
        <input
          class="field"
          placeholder="Last name"
          bind:value={draft.lastName}
          oninput={touched}
          autocomplete="family-name"
        />
      </div>
    </section>

    <section>
      <span class="eyebrow">Born</span>
      <input
        class="field"
        type="date"
        value={draft.born ?? ''}
        oninput={(e) => {
          draft!.born = e.currentTarget.value || null
          touched()
        }}
      />
      <p class="hint">Your age is worked out from this rather than left to the model to guess.</p>
    </section>

    <section>
      <span class="eyebrow">Gender</span>
      <input
        class="field"
        placeholder="However you would put it"
        bind:value={draft.gender}
        oninput={touched}
      />
      <p class="hint">A free field, not a list. Nothing in the application branches on it.</p>
    </section>

    <section>
      <span class="eyebrow">Where you live</span>
      <input class="field" placeholder="City" bind:value={draft.location} oninput={touched} />
    </section>

    <section>
      <span class="eyebrow">About you</span>
      <textarea
        class="field area"
        rows="12"
        placeholder="Your work, your family, what you are trying to do this year, what you care about. Written as you would say it."
        bind:value={draft.about}
        oninput={touched}
      ></textarea>
    </section>

    <div class="save">
      {#if notice}<span class="notice">{notice}</span>{/if}
      <span class="spacer"></span>
      <button class="btn btn-primary" onclick={save} disabled={saving || !app.status?.writable}>
        {saving ? 'Saving…' : 'Save'}
      </button>
    </div>
  {:else}
    <!-- A tab of its own, with no Save: a role writes as it is edited, and
         a Save button beside a list that has already saved itself would be
         a button that appears to be for the list. The fields' draft is this
         component's, so an unsaved name waits on the other tab meanwhile. -->
    <RolesPanel />
  {/if}
{/if}

<style>
  section {
    display: grid;
    gap: var(--sp-2);
    padding: var(--sp-4) 0;
    border-bottom: 1px solid var(--border);
  }

  .lead {
    color: var(--fg-muted);
    font-size: var(--text-sm);
    line-height: 1.55;
  }

  .pair {
    display: grid;
    grid-template-columns: 1fr 1fr;
    gap: var(--sp-2);
  }

  .area {
    resize: vertical;
    font-family: inherit;
    line-height: 1.5;
  }

  .hint {
    color: var(--fg-faint);
    font-size: var(--text-xs);
  }

  .save {
    display: flex;
    align-items: center;
    gap: var(--sp-3);
    padding-top: var(--sp-4);
  }

  .notice {
    color: var(--fg-muted);
    font-size: var(--text-sm);
  }

  .save .spacer {
    flex: 1;
  }
</style>
