<script lang="ts">
  // Settings → Backup: sealed copies of this vault, to a folder or a bucket,
  // on a schedule, and how the last one went.
  //
  // A platform setting rather than any app's: a copy is the whole vault, every
  // app's records and attachments at once, sealed under the vault's own key.
  // Data, beside it, is the other way out -- the same records unsealed, in
  // formats anything can read -- and the two are different on purpose.
  //
  // Three tabs, in the order the decisions get made: where, then when, then
  // what has happened since and how to get a copy back. Where and When are a
  // draft with a Save, the way Models is, because a half-typed bucket name
  // must not be where tonight's backup goes; the Save is at the foot of both,
  // and saves what was changed on either, since the draft is this
  // component's. What happened is read live from the vault and never
  // drafted, so its tab has no Save -- it used to sit under one, in a column
  // so tall the Save landed in the middle of the scroll.
  //
  // Restoring is the command line's for now -- see `karmayoga restore` -- so
  // the last section is the exact command for the destination saved here,
  // rather than instructions to assemble one on the worst day to be doing it.

  import { onDestroy, onMount, untrack } from 'svelte'
  import { api, isWeb, mobileShell } from '../lib/api'
  import { backup } from '../lib/backup.svelte'
  import {
    DESTINATION_KINDS,
    FREQUENCY_CHOICES,
    MAX_KEEP,
    RESTORE_INTO,
    blankDestination,
    destinationUrl,
    nextDueLabel,
    normalizeDestination,
    normalizeSettings,
    phaseLabel,
    progressNote,
    progressValue,
    reportDetail,
    reportSummary,
    restoreCommand,
    sameSettings,
    sentence,
    usesStoredKey,
    whenLabel,
    type DestinationKind,
  } from '../lib/backup'
  import { errorMessage } from '../lib/errors'
  import { plural, pluralWord, relativeTime } from '../lib/format'
  import { app } from '../lib/state.svelte'
  import type { BackupSettings, Destination, SnapshotInfo } from '../lib/types'
  import Icon from './Icon.svelte'
  import Meter from './Meter.svelte'
  import SubTabs from './SubTabs.svelte'
  import Loading from './Loading.svelte'
  import RemoteNote from './RemoteNote.svelte'

  // ── The draft ───────────────────────────────────────────────────────

  let draft = $state<BackupSettings | null>(null)
  /** The two halves of an S3 access key, typed and not yet stored. */
  let accessKeyId = $state('')
  let secretAccessKey = $state('')
  let notice = $state<string | null>(null)
  let saving = $state(false)
  /** "Another S3-compatible service", open when one is already set. */
  let endpointOpen = $state(false)
  let sub = $state<'where' | 'when' | 'copies'>('where')
  /**
   * What each kind of destination was last filled in as, so clicking from
   * S3 to a folder and back finds the bucket where it was left. Not drawn,
   * so not reactive.
   */
  let stash: Partial<Record<DestinationKind, Destination>> = {}

  function seed(from: BackupSettings) {
    draft = structuredClone($state.snapshot(from))
    endpointOpen = from.destination?.type === 's3' && !!from.destination.endpoint
    stash = {}
  }

  // Fresh every time the tab opens, and the draft seeded from that read: a
  // draft seeded from a copy loaded earlier would save over whatever another
  // window has changed since.
  void backup.refresh().then((ok) => {
    if (ok && backup.settings) seed(backup.settings)
  })
  // Polling while a backup runs, and listening for one finishing, last as
  // long as this pane does. `onMount` rather than an effect: the store's own
  // state must not become something this re-subscribes on.
  onMount(() => backup.watch())

  let noticeTimer: ReturnType<typeof setTimeout> | null = null
  let copiedTimer: ReturnType<typeof setTimeout> | null = null
  onDestroy(() => {
    if (noticeTimer) clearTimeout(noticeTimer)
    if (copiedTimer) clearTimeout(copiedTimer)
  })

  /** Clears "Saved." after a moment, and only that -- see `ModelsPanel`. */
  function announce(message: string) {
    notice = message
    if (noticeTimer) clearTimeout(noticeTimer)
    noticeTimer = setTimeout(() => {
      noticeTimer = null
      notice = null
    }, 4000)
  }
  function clearNotice() {
    if (noticeTimer) clearTimeout(noticeTimer)
    noticeTimer = null
    notice = null
  }

  const view = $derived(backup.view)
  const saved = $derived(backup.settings)
  const savedDestination = $derived(saved?.destination ?? null)
  const status = $derived(view?.status ?? null)
  const running = $derived(backup.running)
  const report = $derived(status?.lastReport ?? null)

  const kind = $derived(draft?.destination?.type ?? null)
  const keyTyped = $derived(accessKeyId.trim() !== '' || secretAccessKey.trim() !== '')
  /** Whether the key fields mean anything to the destination being drafted. */
  const keyApplies = $derived(usesStoredKey(draft?.destination))
  const dirty = $derived(
    !!draft && !!saved && (!sameSettings(draft, saved) || (keyApplies && keyTyped)),
  )

  // The platform's folder picker, where there is one and it belongs to the
  // right machine. A browser on web access has no shell to ask; a desktop
  // looking at another computer's vault has one, but its picker would offer
  // this computer's folders for a backup that runs on that one. Both get the
  // text field alone. The mock answers with a made-up disk.
  const canPick = $derived(!isWeb && !app.remote)
  /** Restoring is a command typed where the vault is: not on a phone, nor in
   *  a browser, nor on a computer looking at another's vault. */
  const canRestoreHere = $derived(!isWeb && !app.remote && !mobileShell)

  function pickKind(next: DestinationKind) {
    if (!draft) return
    const current = draft.destination ? $state.snapshot(draft.destination) : null
    if (current?.type === next) return
    if (current) stash[current.type] = current
    const kept = stash[next]
    draft.destination = kept ? structuredClone(kept) : blankDestination(next, current)
  }

  let picking = $state(false)
  let pickProblem = $state<string | null>(null)

  async function chooseFolder() {
    pickProblem = null
    picking = true
    try {
      const path = await api.pickFolder('Where should backups go?')
      if (path && draft?.destination?.type === 'folder') draft.destination.path = path
    } catch (e) {
      pickProblem = sentence(errorMessage(e))
    } finally {
      picking = false
    }
  }

  // ── Trying a destination before saving it ──────────────────────────

  let checking = $state(false)
  /** The last answer, and the destination it was about. */
  let checked = $state<{ for: string; ok: boolean; message: string } | null>(null)
  const draftKey = $derived(
    draft?.destination ? JSON.stringify(normalizeDestination(draft.destination)) : '',
  )
  // Shown only while the fields still say what was tried: "Works" under a
  // bucket name edited since is a claim about somewhere else.
  const shownCheck = $derived(checked && checked.for === draftKey ? checked : null)

  async function test() {
    const d = draft?.destination
    if (!d) return
    const tried = normalizeDestination($state.snapshot(d))
    const key = JSON.stringify(tried)
    checking = true
    try {
      checked = { for: key, ...(await backup.check(tried)) }
    } catch (e) {
      checked = { for: key, ok: false, message: errorMessage(e) }
    } finally {
      checking = false
    }
  }

  // ── Saving ──────────────────────────────────────────────────────────

  async function save() {
    if (!draft) return
    clearNotice()
    const settings = normalizeSettings($state.snapshot(draft))
    const id = accessKeyId.trim()
    const secret = secretAccessKey.trim()
    const withKey = usesStoredKey(settings.destination) && (id !== '' || secret !== '')
    if (withKey && (!id || !secret)) {
      notice = 'An access key comes in two halves: paste both the key ID and its secret.'
      return
    }
    saving = true
    try {
      // The key before the settings: saving can make a backup due at once,
      // and one that starts before the key it signs with is stored fails for
      // nothing.
      if (withKey) {
        await backup.setKeys(id, secret)
        accessKeyId = ''
        secretAccessKey = ''
      }
      await backup.save(settings)
      if (backup.settings) seed(backup.settings)
      announce('Saved.')
    } catch (e) {
      notice = sentence(errorMessage(e))
    } finally {
      saving = false
    }
  }

  async function removeKeys() {
    clearNotice()
    try {
      await backup.clearKeys()
    } catch (e) {
      notice = sentence(errorMessage(e))
    }
  }

  // ── Backing up now ──────────────────────────────────────────────────

  let starting = $state(false)
  let statusNotice = $state<string | null>(null)

  async function runNow() {
    statusNotice = null
    starting = true
    try {
      await backup.run()
    } catch (e) {
      statusNotice = sentence(errorMessage(e))
    } finally {
      starting = false
    }
  }

  async function stop() {
    statusNotice = null
    try {
      await backup.cancel()
    } catch (e) {
      statusNotice = sentence(errorMessage(e))
    }
  }

  // ── The copies kept, and getting one back ───────────────────────────

  const savedKey = $derived(
    savedDestination ? JSON.stringify(normalizeDestination(savedDestination)) : '',
  )
  let listing = $state(false)
  let listed = $state<{ for: string; copies: SnapshotInfo[] | null; error: string | null } | null>(
    null,
  )
  // A list read from the destination saved before is not this one's.
  const shownList = $derived(listed && listed.for === savedKey ? listed : null)
  /** The copy the restore command names; `null` is the newest. */
  let chosen = $state<string | null>(null)

  async function loadCopies() {
    const key = savedKey
    listing = true
    try {
      const copies = [...(await backup.snapshots())].sort(
        (a, b) => new Date(b.created).getTime() - new Date(a.created).getTime(),
      )
      listed = { for: key, copies, error: null }
      if (chosen && !copies.slice(1).some((c) => c.name === chosen)) chosen = null
    } catch (e) {
      listed = { for: key, copies: null, error: errorMessage(e) }
    } finally {
      listing = false
    }
  }

  // A backup finishing while the list is open adds a copy to it, and may let
  // go of the oldest; read it again rather than leave it a copy behind.
  let wasRunning = false
  $effect(() => {
    const now = running != null
    if (wasRunning && !now && untrack(() => shownList?.copies)) void loadCopies()
    wasRunning = now
  })

  // Only while the list it was chosen from is the one on screen: a copy
  // picked at the destination saved before is not at this one.
  const chosenCopy = $derived(shownList?.copies ? chosen : null)
  const command = $derived(
    savedDestination ? restoreCommand(savedDestination, { snapshot: chosenCopy }) : '',
  )
  let copied = $state(false)

  async function copyCommand() {
    try {
      await navigator.clipboard.writeText(command)
    } catch {
      // A webview that refuses the clipboard still shows the command, and it
      // can be selected by hand.
      return
    }
    copied = true
    if (copiedTimer) clearTimeout(copiedTimer)
    copiedTimer = setTimeout(() => {
      copiedTimer = null
      copied = false
    }, 2000)
  }
</script>

{#if !view && backup.error}
  <p class="hint">Backups could not be read: {backup.error}</p>
{:else if !view || !draft}
  <Loading label="Loading backups…" centred />
{:else if !view.supported}
  <p class="hint lead">
    This vault’s database is on a server, so it is backed up where that server is — by whoever runs
    it, with Postgres’s own backups or Supabase’s. There is nothing for Karma Yoga to copy from
    here.
  </p>
{:else}
  {@const d = draft.destination}
  <SubTabs
    tabs={[
      { id: 'where', label: 'Where' },
      { id: 'when', label: 'When' },
      { id: 'copies', label: 'Copies' },
    ]}
    bind:value={sub}
    label="Backup"
  />

  <div class="panes">
    {#if sub === 'where'}
      <p class="hint lead">
        Sealed, date-stamped copies of this vault and of this computer’s settings, made on a
        schedule to a folder or a bucket. Each copy is complete on its own, and copies share
        whatever did not change between them, so keeping a week of them costs little more than
        keeping one. They open with this vault’s password, and with nothing else.
      </p>

      {#if !view.encrypted}
        <p class="warning" role="note">
          This vault has no password, so its backups are no more private than it is: anybody who can
          read the folder or the bucket they go to can read everything in them.
        </p>
      {/if}

      <section aria-labelledby="backup-where">
        <h3 class="eyebrow" id="backup-where">Where</h3>
        <div class="seg-group" role="radiogroup" aria-labelledby="backup-where">
          {#each DESTINATION_KINDS as k (k.value)}
            <button
              class="seg-opt"
              class:on={kind === k.value}
              role="radio"
              aria-checked={kind === k.value}
              onclick={() => pickKind(k.value)}
            >
              {k.label}
            </button>
          {/each}
        </div>

        {#if !d}
          <p class="hint">
            Nowhere yet. A folder can be another disk, a network share or a folder that syncs; a
            bucket can be at Amazon, at Google, or at any service that speaks S3.
          </p>
        {:else if d.type === 'folder'}
          <div class="setting">
            <label class="caption" for="backup-folder">Folder</label>
            <div class="with-button">
              <input
                id="backup-folder"
                bind:value={d.path}
                placeholder="/media/you/Backup Disk/Karma Yoga"
                spellcheck="false"
                autocomplete="off"
              />
              {#if canPick}
                <button
                  class="btn btn-outline"
                  disabled={picking}
                  onclick={() => void chooseFolder()}
                >
                  Choose…
                </button>
              {/if}
            </div>
          </div>
          {#if pickProblem}<p class="problem">{pickProblem}</p>{/if}
          <p class="hint">
            Another disk, a network share, or a folder that syncs somewhere else. A disk that is
            sometimes unplugged is fine: nothing is written while it is away, backups try again
            every half hour until it is back, and you are told if it stays away longer than the
            schedule allows.
          </p>
        {:else if d.type === 's3'}
          <div class="pair">
            <label class="setting">
              <span class="caption">Bucket</span>
              <input
                bind:value={d.bucket}
                placeholder="my-backups"
                spellcheck="false"
                autocomplete="off"
              />
            </label>
            <label class="setting">
              <span class="caption">Folder inside the bucket</span>
              <input
                bind:value={d.prefix}
                placeholder="karmayoga"
                spellcheck="false"
                autocomplete="off"
              />
            </label>
          </div>
          <label class="setting">
            <span class="caption">Region</span>
            <input
              value={d.region ?? ''}
              oninput={(e) => (d.region = e.currentTarget.value)}
              placeholder="From your AWS profile"
              spellcheck="false"
              autocomplete="off"
            />
          </label>

          <details class="more" bind:open={endpointOpen}>
            <summary>Another S3-compatible service</summary>
            <div class="more-body">
              <label class="setting">
                <span class="caption">Endpoint</span>
                <input
                  value={d.endpoint ?? ''}
                  oninput={(e) => (d.endpoint = e.currentTarget.value)}
                  placeholder="https://<account>.r2.cloudflarestorage.com"
                  spellcheck="false"
                  autocomplete="off"
                />
              </label>
              <p class="hint">
                The service’s address: Cloudflare R2, Backblaze B2, MinIO, Wasabi, or Google Cloud
                Storage with HMAC keys. Leave it empty for Amazon. It has to be https:// unless the
                service is on this computer.
              </p>
            </div>
          </details>

          <!-- `group` rather than `radiogroup`: the profile and the key fields
             sit between the two choices they belong to. -->
          <div class="setting" role="group" aria-labelledby="backup-s3-credentials">
            <span class="caption" id="backup-s3-credentials">Signed in with</span>
            <label class="choice">
              <input
                type="radio"
                name="backup-s3-credentials"
                checked={d.credentials === 'thisComputer'}
                onchange={() => (d.credentials = 'thisComputer')}
              />
              <span>
                <b>From this computer</b>
                <small>
                  The <code>AWS_*</code> variables, a profile in <code>~/.aws</code>, or whatever
                  the AWS CLI can export for it — SSO and assumed roles included. Nothing is stored
                  in the vault.
                </small>
              </span>
            </label>
            {#if d.credentials === 'thisComputer'}
              <label class="setting nested">
                <span class="caption">Profile</span>
                <input
                  value={d.profile ?? ''}
                  oninput={(e) => (d.profile = e.currentTarget.value)}
                  placeholder="default"
                  spellcheck="false"
                  autocomplete="off"
                />
              </label>
            {/if}
            <label class="choice">
              <input
                type="radio"
                name="backup-s3-credentials"
                checked={d.credentials === 'stored'}
                onchange={() => (d.credentials = 'stored')}
              />
              <span>
                <b>An access key</b>
                <small>
                  Pasted here and sealed in the vault, for a service whose keys are not kept in
                  <code>~/.aws</code>. It cannot be shown again, only replaced.
                </small>
              </span>
            </label>
            {#if d.credentials === 'stored'}
              <div class="nested">
                {#if view.hasKeys}
                  <div class="stored">
                    <span><Icon name="lock" size={14} /> An access key is stored.</span>
                    <button class="link" onclick={() => void removeKeys()}>Remove</button>
                  </div>
                {/if}
                <div class="pair">
                  <label class="setting">
                    <span class="caption"
                      >{view.hasKeys ? 'New access key ID' : 'Access key ID'}</span
                    >
                    <input
                      bind:value={accessKeyId}
                      placeholder="AKIA…"
                      spellcheck="false"
                      autocomplete="off"
                    />
                  </label>
                  <label class="setting">
                    <span class="caption">Secret access key</span>
                    <input
                      type="password"
                      bind:value={secretAccessKey}
                      spellcheck="false"
                      autocomplete="off"
                    />
                  </label>
                </div>
              </div>
            {/if}
          </div>
        {:else}
          <div class="pair">
            <label class="setting">
              <span class="caption">Bucket</span>
              <input
                bind:value={d.bucket}
                placeholder="my-backups"
                spellcheck="false"
                autocomplete="off"
              />
            </label>
            <label class="setting">
              <span class="caption">Folder inside the bucket</span>
              <input
                bind:value={d.prefix}
                placeholder="karmayoga"
                spellcheck="false"
                autocomplete="off"
              />
            </label>
          </div>
          <label class="setting">
            <span class="caption">Key file</span>
            <input
              value={d.credentialsFile ?? ''}
              oninput={(e) => (d.credentialsFile = e.currentTarget.value)}
              placeholder="Optional"
              spellcheck="false"
              autocomplete="off"
            />
          </label>
          <p class="hint">
            Signs in as this computer’s Google account: run
            <code>gcloud auth application-default login</code> once. Name a key file — a service account’s
            JSON key — to sign in with that instead.
          </p>
        {/if}

        {#if d}
          <div class="test">
            <button class="btn btn-outline" disabled={checking} onclick={() => void test()}>
              {checking ? 'Trying…' : 'Test'}
            </button>
            {#if shownCheck}
              <p class="check" class:ok={shownCheck.ok} class:bad={!shownCheck.ok} role="status">
                {sentence(shownCheck.message)}
              </p>
            {:else}
              <p class="hint">
                Writes a small file there, reads it back and deletes it. Nothing is saved.
              </p>
            {/if}
          </div>
          {#if keyApplies && keyTyped}
            <p class="hint">
              A test signs with the key already stored. Save to try the one typed above.
            </p>
          {/if}
        {/if}
      </section>
    {:else if sub === 'when'}
      <section aria-labelledby="backup-when">
        <h3 class="eyebrow" id="backup-when">When</h3>
        <label class="check-row" class:disabled={!d}>
          <input type="checkbox" bind:checked={draft.enabled} disabled={!d} />
          <span>
            <b>Back up automatically</b>
            <small>
              {#if !d}
                Choose where copies go first.
              {:else}
                While the vault is open here. One that comes due while this computer is asleep or
                the vault is locked is made as soon as it can be.
              {/if}
            </small>
          </span>
        </label>

        <div class="pair">
          <label class="setting">
            <span class="caption">How often</span>
            <select bind:value={draft.everyHours}>
              {#each FREQUENCY_CHOICES as f (f.value)}
                <option value={f.value}>{f.label}</option>
              {/each}
            </select>
          </label>
          <div class="setting">
            <label class="caption" for="backup-keep">How many</label>
            <span class="inline">
              Keep the last
              <input
                id="backup-keep"
                class="count"
                type="number"
                min="1"
                max={MAX_KEEP}
                step="1"
                bind:value={draft.keep}
              />
              {pluralWord(draft.keep, 'copy', 'copies')}
            </span>
          </div>
        </div>
        <p class="hint">
          When a new copy is made, the oldest beyond that is let go of — and only what no other copy
          shares goes with it.
        </p>

        <label class="check-row">
          <input type="checkbox" bind:checked={draft.includeConfig} />
          <span>
            <b>Include this computer’s settings</b>
            <small>
              Everything in <code>{view.configDir}</code>: the devices you have paired, the server’s
              certificate, the MCP token. Sealed like everything else. Downloaded models and the
              mail search index are never copied — they are downloaded or rebuilt instead.
            </small>
          </span>
        </label>
      </section>
    {:else}
      <section aria-labelledby="backup-status">
        <h3 class="eyebrow" id="backup-status">Last backup</h3>
        {#if !savedDestination || !status}
          <p class="hint">
            None yet. Choose where copies go under Where and save, and Back up now makes the first
            one.
          </p>
          <div class="row">
            <button class="btn btn-outline" disabled>Back up now</button>
          </div>
        {:else}
          <div class="facts">
            <div>
              <span>To</span>
              <b class="mono">{status.destination ?? destinationUrl(savedDestination)}</b>
            </div>
            <div>
              <span>Made</span>
              {#if status.lastSuccess}
                <b title={whenLabel(status.lastSuccess)}>{relativeTime(status.lastSuccess)}</b>
              {:else}
                <b>Not yet</b>
              {/if}
            </div>
            {#if report}
              <div>
                <span>Sent</span>
                <b>
                  {reportSummary(report)}
                  <small>{reportDetail(report)}</small>
                </b>
              </div>
            {/if}
            <div>
              <span>Next</span>
              {#if running}
                <b>Now</b>
              {:else if view.nextDue}
                <b title={whenLabel(view.nextDue)}>{nextDueLabel(view.nextDue)}</b>
              {:else}
                <b>When you ask — automatic backups are off</b>
              {/if}
            </div>
          </div>

          {#if report && report.skipped.length > 0}
            <p class="hint">
              {plural(report.skipped.length, 'file')} went away while that copy was being made — an attachment
              tidied up, mail repacked — and {report.skipped.length === 1 ? 'is' : 'are'} not in it.
            </p>
          {/if}

          {#if status.lastError}
            <div class="error failing" role="alert">
              <b>
                {status.failingSince
                  ? `Failing since ${whenLabel(status.failingSince)}`
                  : 'The last backup failed'}
              </b>
              <span>{sentence(status.lastError)}</span>
            </div>
          {/if}

          {#if running}
            <div class="model-progress">
              <Meter
                label={running.manual
                  ? phaseLabel(running.phase)
                  : `${phaseLabel(running.phase)} · on schedule`}
                value={progressValue(running)}
                note={progressNote(running)}
              />
              <button class="btn" onclick={() => void stop()}>Stop</button>
            </div>
            <p class="hint">
              Stopping keeps what was sent, and the next backup starts from there. A scheduled one
              waits until its next turn rather than starting again.
            </p>
          {:else}
            <div class="row">
              <button class="btn btn-outline" disabled={starting} onclick={() => void runNow()}>
                {starting ? 'Starting…' : 'Back up now'}
              </button>
              {#if dirty}
                <span class="hint"
                  >To where it was last saved, not the changes under Where and When.</span
                >
              {/if}
            </div>
          {/if}
        {/if}
        {#if statusNotice}<p class="problem">{statusNotice}</p>{/if}
      </section>

      {#if savedDestination}
        <section aria-labelledby="backup-copies">
          <h3 class="eyebrow" id="backup-copies">Copies kept</h3>
          {#if !shownList?.copies}
            <div class="row">
              <button class="btn btn-outline" disabled={listing} onclick={() => void loadCopies()}>
                {listing ? 'Looking…' : 'Show the copies kept'}
              </button>
            </div>
          {:else if shownList.copies.length === 0}
            <p class="hint">None yet.</p>
          {:else}
            <ul class="copies" aria-label="Copies kept, newest first">
              {#each shownList.copies as c, i (c.name)}
                <li>
                  <label class="copy">
                    <input
                      type="radio"
                      name="backup-copy"
                      checked={i === 0 ? chosen === null : chosen === c.name}
                      onchange={() => (chosen = i === 0 ? null : c.name)}
                    />
                    <span class="copy-when">{whenLabel(c.created)}</span>
                    {#if i === 0}<span class="newest">Newest</span>{/if}
                    <code class="copy-name">{c.name}</code>
                  </label>
                </li>
              {/each}
            </ul>
            <div>
              <button class="link" disabled={listing} onclick={() => void loadCopies()}>
                {listing ? 'Looking…' : 'Look again'}
              </button>
            </div>
          {/if}
          {#if shownList?.error}<p class="problem">{sentence(shownList.error)}</p>{/if}

          {#if !canRestoreHere}
            <!-- The command has to be typed on the computer that holds the
                 vault, which a phone or a browser is not. -->
            <RemoteNote what="Restoring a copy" />
          {:else}
            <p class="hint">
              <b>To restore, run this in a terminal on the computer that holds your vault.</b> It
              lays
              {chosenCopy ? 'the copy chosen above' : 'the newest copy'} down as a vault in a new folder,
              and that folder opens in Karma Yoga with this vault’s password:
            </p>
            <div class="command">
              <code>{command}</code>
              <button class="btn btn-sm" onclick={() => void copyCommand()}>
                {copied ? 'Copied' : 'Copy'}
              </button>
            </div>
            <p class="hint">
              The folder has to be empty or not there yet; change <code>{RESTORE_INTO}</code> to
              wherever it should go. Add <code>--snapshot</code> and a copy’s name for an older one
              — choosing one above does that — and <code>--config</code> to bring this computer’s settings
              back too, never over a file that is already there.
            </p>
            {#if usesStoredKey(savedDestination)}
              <p class="hint">
                The access key is kept in the vault, and the vault is what is being restored: on
                that computer, set <code>AWS_ACCESS_KEY_ID</code> and
                <code>AWS_SECRET_ACCESS_KEY</code> first.
              </p>
            {/if}
          {/if}
        </section>
      {/if}
    {/if}
  </div>

  {#if sub !== 'copies'}
    <div class="save save-row">
      {#if notice}<span class="notice" role="status">{notice}</span>{/if}
      <button class="btn btn-primary" onclick={() => void save()} disabled={saving || !dirty}>
        {saving ? 'Saving…' : 'Save'}
      </button>
    </div>
  {/if}
{/if}

<style>
  /* Every box in here is `width: 100%` of a column that has `min-width: 0`
     -- see `AgentPanel`'s note on why both halves are needed. */
  .panes {
    display: flex;
    flex-direction: column;
    gap: var(--sp-6);
    min-width: 0;
  }
  section {
    display: flex;
    flex-direction: column;
    gap: var(--sp-3);
    min-width: 0;
  }

  .hint {
    margin: 0;
    font-size: var(--text-sm);
    line-height: var(--leading-normal);
    color: var(--fg-subtle);
    overflow-wrap: anywhere;
  }
  .hint.lead {
    color: var(--fg-muted);
  }

  /* A vault with no password: said in the theme's own caution colour rather
     than the danger one, because nothing is wrong yet -- it is a fact about
     where the copies can be read. */
  .warning {
    margin: 0;
    padding: var(--sp-3);
    border-radius: var(--radius);
    background: color-mix(in oklab, var(--warning) 12%, transparent);
    color: color-mix(in oklab, var(--warning) 80%, var(--fg));
    font-size: var(--text-sm);
    line-height: var(--leading-normal);
  }

  /* A caption over a field -- not app.css's `.field`, which is a different
     input altogether; see `ModelsPanel`'s note on wearing both. */
  .setting {
    display: flex;
    flex-direction: column;
    gap: var(--sp-2);
    min-width: 0;
  }
  .caption {
    font-size: var(--text-sm);
    font-weight: 550;
    color: var(--fg-muted);
  }
  /* Bucket and folder, or how often and how many: two short fields that
     belong together, side by side while there is room for both. */
  .pair {
    display: grid;
    grid-template-columns: repeat(auto-fit, minmax(200px, 1fr));
    gap: var(--sp-3);
    min-width: 0;
  }
  .with-button {
    display: flex;
    gap: var(--sp-2);
    min-width: 0;
  }
  .with-button input {
    flex: 1;
  }

  input,
  select {
    width: 100%;
    min-width: 0;
    padding: var(--sp-2) var(--sp-3);
    border: 1px solid var(--border);
    border-radius: var(--radius);
    background: var(--bg);
    color: var(--fg);
    font: inherit;
    font-size: var(--text-base);
    user-select: text;
  }
  input:focus,
  select:focus {
    outline: none;
    border-color: var(--accent);
    box-shadow: 0 0 0 3px color-mix(in oklab, var(--accent) 16%, transparent);
  }
  input[type='checkbox'],
  input[type='radio'] {
    flex: none;
    width: auto;
    padding: 0;
    accent-color: var(--accent);
  }

  /* "Keep the last [7] copies": the number set into the sentence it is the
     number of. */
  .inline {
    display: flex;
    align-items: center;
    gap: var(--sp-2);
    font-size: var(--text-base);
    color: var(--fg-muted);
  }
  .count {
    width: 5.5em;
    flex: none;
    text-align: right;
    font-variant-numeric: tabular-nums;
  }

  .more summary {
    cursor: pointer;
    font-size: var(--text-sm);
    color: var(--fg-muted);
  }
  .more-body {
    display: flex;
    flex-direction: column;
    gap: var(--sp-2);
    margin-top: var(--sp-3);
  }

  .choice {
    display: flex;
    align-items: flex-start;
    gap: var(--sp-2);
    cursor: pointer;
  }
  .choice input {
    margin-top: 3px;
  }
  .choice span {
    display: flex;
    flex-direction: column;
    gap: 1px;
    min-width: 0;
  }
  .choice b {
    font-size: var(--text-sm);
    font-weight: 550;
  }
  .choice small {
    color: var(--fg-subtle);
    font-size: var(--text-xs);
    line-height: var(--leading-normal);
  }
  /* What belongs to the choice above it, set in under it. */
  .nested {
    display: flex;
    flex-direction: column;
    gap: var(--sp-2);
    margin-left: calc(13px + var(--sp-2));
  }

  .stored {
    display: flex;
    align-items: flex-start;
    gap: var(--sp-3);
    padding: var(--sp-3);
    border: 1px solid var(--border);
    border-radius: var(--radius);
    background: var(--bg-sunken);
    font-size: var(--text-sm);
    min-width: 0;
  }
  .stored span {
    display: flex;
    flex: 1;
    align-items: baseline;
    gap: var(--sp-2);
    min-width: 0;
    line-height: var(--leading-normal);
    color: var(--fg-muted);
  }
  .link {
    flex: none;
    color: var(--accent);
    font-size: var(--text-sm);
    font-weight: 550;
  }
  .link:hover {
    text-decoration: underline;
  }
  .link:disabled {
    opacity: 0.5;
    text-decoration: none;
  }

  .test {
    display: flex;
    align-items: center;
    gap: var(--sp-3);
    min-width: 0;
  }
  .test .btn {
    flex: none;
  }
  .check {
    margin: 0;
    font-size: var(--text-sm);
    line-height: var(--leading-normal);
    overflow-wrap: anywhere;
  }
  .check.ok {
    color: color-mix(in oklab, var(--success) 80%, var(--fg));
  }
  .check.bad,
  .problem {
    color: var(--danger);
  }
  .problem {
    margin: 0;
    font-size: var(--text-sm);
    line-height: var(--leading-normal);
    overflow-wrap: anywhere;
  }

  /* Label on the left, the fact on the right -- Settings → Vault's "This
     vault" table, whose styles are scoped to that file. */
  .facts {
    display: grid;
    gap: var(--sp-2);
    font-size: var(--text-base);
  }
  .facts > div {
    display: flex;
    justify-content: space-between;
    gap: var(--sp-4);
  }
  .facts span {
    flex: none;
    color: var(--fg-subtle);
  }
  .facts b {
    display: flex;
    flex-direction: column;
    align-items: flex-end;
    gap: 2px;
    min-width: 0;
    font-weight: 600;
    text-align: right;
    overflow-wrap: anywhere;
  }
  .facts small {
    font-size: var(--text-sm);
    font-weight: 400;
    color: var(--fg-subtle);
  }
  .mono {
    font-family: var(--font-mono);
    font-size: var(--text-sm);
    user-select: text;
  }

  .failing {
    display: flex;
    flex-direction: column;
    gap: 2px;
    line-height: var(--leading-normal);
    overflow-wrap: anywhere;
  }

  .row {
    display: flex;
    align-items: center;
    gap: var(--sp-3);
    min-width: 0;
  }

  .copies {
    display: flex;
    flex-direction: column;
    gap: 2px;
    margin: 0;
    padding: 0;
    list-style: none;
  }
  .copy {
    display: flex;
    align-items: center;
    gap: var(--sp-3);
    padding: var(--sp-2) 0;
    border-radius: var(--radius-sm);
    cursor: pointer;
    font-size: var(--text-sm);
  }
  .copy:hover {
    background: var(--bg-hover);
  }
  .copy-when {
    flex: 1;
    min-width: 0;
  }
  .newest {
    padding: 0 6px;
    border-radius: var(--radius-pill);
    background: var(--bg-hover);
    color: var(--fg-subtle);
    font-size: var(--text-xs);
  }
  .copy-name {
    color: var(--fg-faint);
    font-size: var(--text-xs);
  }

  /* The command, as it is to be pasted: monospaced, selectable, wrapped
     anywhere rather than scrolled, with its Copy beside it. */
  .command {
    display: flex;
    align-items: flex-start;
    gap: var(--sp-2);
    min-width: 0;
  }
  .command code {
    flex: 1;
    min-width: 0;
    padding: var(--sp-2) var(--sp-3);
    border-radius: var(--radius-sm);
    background: var(--bg-sunken);
    color: var(--fg-muted);
    font-family: var(--font-mono);
    font-size: var(--text-xs);
    line-height: var(--leading-normal);
    overflow-wrap: anywhere;
    user-select: text;
  }

  code {
    font-size: 0.92em;
  }
</style>
