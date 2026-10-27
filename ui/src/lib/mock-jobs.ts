// The assistant's background jobs, for the mock backend.
//
// A job is work that happens without anybody pressing anything, which is
// exactly what a mock has no way to do: there is no worker here and no model.
// What there is instead is a script. Starting research queues a job, and a
// chain of timers walks it through what a real one does -- waits its turn,
// starts, searches and reads (a new progress line and a tool call in its own
// transcript every couple of seconds, a note growing as it goes), finishes,
// and comes back to the conversation that asked with a line saying so and an
// answer. The interface can be driven by hand and watched doing it.
//
// Three things to know about driving it.
//
//   - Typing a prompt with the word "research" in it to the assistant starts
//     one, with that conversation as the place to come back to. See
//     `mockSendMessage` in `mock.ts`.
//   - A question with the word "impossible" in it fails part way, so the
//     failed card can be watched arriving as well as read.
//   - A prompt that also says "unanswered" finishes the job and leaves the
//     answer undone, which is the state that offers "Answer now".
//
// Two jobs are seeded as plain data, one finished and one failed, so that
// Settings → "What it did" has something in it before anything is driven.
//
// # No timer at load
//
// Every timer is set from inside a command, never at module load. A timer
// running at import keeps a Node process alive, and `npm run test` loads this
// file in a dozen processes that have no use for it. The timers set here are
// `unref`'d for the same reason: a test that starts a job and finishes
// without waiting for it does not have to wait for the script either.
//
// # Change events
//
// Unlike the rest of the mock, which is read back by the store that made the
// write, a job moves on its own, so it raises the `changed` events a real
// backend would -- `job`, and `note` and `conversation` for what it wrote.
// `api.ts` hands `mockOnChange` to the live router in a browser, so a card
// moves without anybody reloading it.

import type {
  AgentMessage,
  ChangeEvent,
  Conversation,
  ConversationId,
  Job,
  JobId,
  JobQuery,
  Note,
  NoteId,
} from './types'
import { VaultError } from './types'

// ── What the rest of the mock lends this file ───────────────────────────

/**
 * The slices of `mock.ts`'s state a job writes into.
 *
 * Handed over once, as `mockMeetingsInit` is: the arrays are private to
 * that module, and a closure each is the smallest surface that lets a job
 * finishing here leave a note and a message there.
 */
export interface JobsSink {
  /** Add a note, or replace the one with that id. */
  putNote(note: Note): void
  /** Add a conversation, or replace the one with that id. */
  putConversation(conversation: Conversation): void
  addMessage(message: AgentMessage): void
  /** A conversation and its messages, for a job deleted with its transcript. */
  dropConversation(id: ConversationId): void
  /** Whether a turn is being written into this conversation right now. */
  busy(id: ConversationId): boolean
}

let sink: JobsSink | null = null

function lent(): JobsSink {
  if (!sink) throw new VaultError('unavailable', 'the mock jobs were never given the vault')
  return sink
}

// ── Time ────────────────────────────────────────────────────────────────

function minutesAgo(n: number): string {
  return new Date(Date.now() - n * 60_000).toISOString()
}

const now = () => new Date().toISOString()

/**
 * How long each beat of the script lasts, in milliseconds, before `tempo`.
 *
 * Slow enough to watch -- the queued card, a progress line that changes, a
 * finish -- and a whole job is under half a minute.
 */
const QUEUED_MS = 1500
const STEP_MS = 2200
const ANSWER_MS = 1800
/** How long to wait for a turn in flight to end before looking again. */
const BUSY_MS = 500

/** Scales every beat. Tests turn it down; nothing else does. */
let tempo = 1

/** Speed the script up or slow it down. 1 is the pace a person watches at. */
export function mockJobsTempo(scale: number): void {
  tempo = Math.max(0, scale)
}

type Timer = ReturnType<typeof setTimeout>

/** A timer that does not keep a Node process alive. See the top of this file. */
function unreffed(handle: Timer): Timer {
  ;(handle as unknown as { unref?: () => void }).unref?.()
  return handle
}

/**
 * A pause in the script. Unreffed like every other timer here: the follow-up
 * waits in a loop for the asking thread to go quiet, and a test that leaves a
 * turn in flight would otherwise keep Node alive for ever with nothing left to
 * check -- the hang the top of this file is about.
 */
const wait = (ms: number) =>
  new Promise<void>((resolve) => void unreffed(setTimeout(resolve, ms * tempo)))

// ── Change events ───────────────────────────────────────────────────────

const handlers = new Set<(change: ChangeEvent) => void>()

/** Register for the writes this file makes. `api.ts` hands it to the router. */
export function mockOnChange(handler: (change: ChangeEvent) => void): void {
  handlers.add(handler)
}

/**
 * `origin` is anything but `local`, which the router takes for its own
 * window's write and ignores -- a job is nobody's write but its own.
 */
function emit(kind: ChangeEvent['kind'], op: ChangeEvent['op'], id: string): void {
  for (const handler of handlers) {
    try {
      handler({ kind, op, id, origin: 'mock-jobs' })
    } catch {
      // One listener's fault is not another's.
    }
  }
}

// ── Records ─────────────────────────────────────────────────────────────

const jobs: Job[] = []
let jobCounter = 2
let messageCounter = 0

/** A copy, because the script mutates the stored job and a caller's must not move. */
function snapshot(job: Job): Job {
  return { ...job, outputs: job.outputs.map((o) => ({ ...o })) }
}

function find(id: JobId): Job {
  const job = jobs.find((j) => j.id === id)
  if (!job) throw new VaultError('not_found', 'no such job')
  return job
}

function message(
  conversationId: ConversationId,
  role: AgentMessage['role'],
  content: string,
  extra: Partial<AgentMessage> = {},
): AgentMessage {
  messageCounter += 1
  return {
    id: `msg-job-${messageCounter}`,
    conversationId,
    role,
    content,
    toolCalls: [],
    toolCallId: null,
    failed: false,
    createdAt: now(),
    ...extra,
  }
}

/** A job's question and brief. The mock only researches; a task's title stands in. */
function asked(job: Job): { question: string; brief: string } {
  return job.kind.type === 'research' ? job.kind : { question: job.kind.title, brief: '' }
}

/** A question cut to fit a title or a progress line, at a word if it can. */
function gist(question: string, chars = 70): string {
  const text = question.replace(/\s+/g, ' ').trim()
  if (text.length <= chars) return text
  const cut = text.slice(0, chars)
  const space = cut.lastIndexOf(' ')
  return `${cut.slice(0, space > chars / 2 ? space : chars).replace(/[\s,;:.-]+$/, '')}…`
}

// ── Notes ───────────────────────────────────────────────────────────────

const heading = (text: string) => ({
  type: 'heading',
  attrs: { level: 2 },
  content: [{ type: 'text', text }],
})
const paragraph = (text: string) => ({ type: 'paragraph', content: [{ type: 'text', text }] })
const bullets = (items: string[]) => ({
  type: 'bulletList',
  content: items.map((text) => ({ type: 'listItem', content: [paragraph(text)] })),
})
const sourceList = (sources: [string, string][]) =>
  sources.length === 0
    ? paragraph('None yet.')
    : {
        type: 'bulletList',
        content: sources.map(([label, href]) => ({
          type: 'listItem',
          content: [
            {
              type: 'paragraph',
              content: [{ type: 'text', text: label, marks: [{ type: 'link', attrs: { href } }] }],
            },
          ],
        })),
      }

/** Where each page the script reads is, for the note's list of sources. */
const SOURCES: Record<string, [string, string]> = {
  'overview.example': ['overview.example: a plain guide', 'https://overview.example/guide'],
  'forum.example': ['forum.example: people who tried it', 'https://forum.example/threads/4417'],
  'tests.example': ['tests.example: the options side by side', 'https://tests.example/reviews'],
}

/** The note as it stands partway through: what has been read, so far. */
function workingBody(found: string[]): Note['body'] {
  return {
    type: 'doc',
    content: [
      heading('What it has read so far'),
      found.length > 0 ? bullets(found) : paragraph('Nothing yet.'),
    ],
  }
}

/** The note once it is written up: what is known, how sure, from where, and what is not. */
function finalBody(question: string, found: string[], sources: [string, string][]): Note['body'] {
  return {
    type: 'doc',
    content: [
      heading('What is known'),
      paragraph(
        `This is a stand-in for the answer to “${gist(question, 90)}”. The demo build ` +
          'has no model behind it, so what follows is the shape a real note takes rather than ' +
          'anything that was found out.',
      ),
      bullets(found),
      heading('How sure'),
      paragraph(
        'Not at all, here. A real one says which claims have two sources and which have one.',
      ),
      heading('Sources'),
      sourceList(sources),
      heading('Still open'),
      paragraph('Whatever the sources disagreed about, and what none of them covered.'),
    ],
  }
}

/** The notes by id, so a step can rewrite the one the job made. */
const written = new Map<NoteId, Note>()

function noteFor(job: Job): Note | null {
  const id = job.outputs[0]?.noteId
  return id ? (written.get(id) ?? null) : null
}

/**
 * Hand a note to the vault. A copy, because the script goes on rewriting its
 * own and the vault's must not change under a store that has read it.
 */
function save(note: Note): void {
  written.set(note.id, note)
  lent().putNote({ ...note })
}

// ── Seeds ───────────────────────────────────────────────────────────────

const DONE_QUESTION =
  'Which outboard motors under 6 hp hold up best on a small wooden sailboat, and what should I check before buying one second-hand?'

const FAILED_QUESTION = 'How have berthing fees along the south coast changed since 2023?'

function seedNotes(): Note[] {
  const done: Note = {
    id: 'n-job-1',
    title: 'Outboards for a small wooden sailboat',
    body: {
      type: 'doc',
      content: [
        heading('What is known'),
        paragraph(
          'A four-stroke of 4 to 6 hp with a long shaft is the usual answer for a boat of about ' +
            'twenty-two feet on a mooring. Two-strokes are lighter and cheaper, and smell in the ' +
            'cabin; most of the people writing about wooden boats have moved off them.',
        ),
        heading('How sure'),
        paragraph(
          'Fairly. Three of the four sources agree on the size. The fourth is a forum thread.',
        ),
        heading('Before buying one second-hand'),
        bullets([
          'Pull the cord cold. It should catch within a few pulls and settle without hunting.',
          'Look for a steady stream at the water tell-tale once it is running.',
          'Gearbox oil should be clean. Milky oil means water has got in past the seals.',
          'Ask for the service history, and assume there is none if there is no answer.',
        ]),
        heading('Sources'),
        sourceList([
          ['overview.example: a plain guide', 'https://overview.example/guide'],
          ['tests.example: the options side by side', 'https://tests.example/reviews'],
          ['forum.example: people who tried it', 'https://forum.example/threads/4417'],
        ]),
        heading('Still open'),
        paragraph(
          'Whether 4 hp is enough for the tide at the harbour mouth. Worth asking on the mooring.',
        ),
      ],
    },
    tags: ['research'],
    pinned: false,
    attachments: [],
    createdAt: minutesAgo(189),
    updatedAt: minutesAgo(172),
  }
  const partial: Note = {
    id: 'n-job-2',
    title: 'Berthing fees along the south coast',
    body: {
      type: 'doc',
      content: [
        heading('What it has read so far'),
        bullets([
          'Two harbour authorities publish a 2023 and a 2025 price list. Both rose by a tenth.',
          'A third publishes only the current one, so there is nothing to compare it with.',
        ]),
        heading('Sources'),
        sourceList([
          ['overview.example: a plain guide', 'https://overview.example/guide'],
          ['tests.example: the options side by side', 'https://tests.example/reviews'],
        ]),
      ],
    },
    tags: ['research'],
    pinned: false,
    attachments: [],
    createdAt: minutesAgo(60 * 26 - 1),
    updatedAt: minutesAgo(60 * 25 + 40),
  }
  return [done, partial]
}

/** A job's prompt as its transcript opens, which is what the worker writes. */
function transcriptPrompt(question: string, brief: string): string {
  return [
    'Research this question and write what you find up as a note.',
    '',
    `Question: ${question}`,
    ...(brief ? ['', `Background: ${brief}`] : []),
  ].join('\n')
}

/** One tool call and its result, as a transcript holds them. */
function callPair(
  conversationId: ConversationId,
  n: number,
  name: string,
  args: unknown,
  result: string,
  at: string,
): AgentMessage[] {
  const id = `call-${conversationId}-${n}`
  return [
    message(conversationId, 'assistant', '', {
      toolCalls: [{ id, name, arguments: args }],
      createdAt: at,
    }),
    message(conversationId, 'tool', result, { toolCallId: id, createdAt: at }),
  ]
}

/**
 * Put the seeds into the vault: two jobs, their notes, their transcripts, and
 * the conversation the finished one answered in.
 *
 * Called once from `mock.ts`, after the arrays it writes into exist.
 */
export function mockJobsInit(lend: JobsSink): void {
  sink = lend
  if (jobs.length > 0) return

  for (const note of seedNotes()) save(note)

  const doneBrief =
    'Buying a second-hand outboard for a 22 ft wooden sloop kept on a mooring. Wants a short ' +
    'list and a checklist for viewing one. Has ruled out two-strokes because of the fumes.'
  jobs.push(
    {
      id: 'job-2',
      kind: { type: 'research', question: FAILED_QUESTION, brief: '' },
      askedIn: null,
      status: 'failed',
      reason:
        'The model provider stopped answering (HTTP 503) after 14 steps. What it had found ' +
        'before then is in the note.',
      progress: 'Reading a harbour authority tariff',
      steps: 14,
      outputs: [{ type: 'note', noteId: 'n-job-2' }],
      summary: '',
      conversationId: 'c-job-2',
      answered: false,
      attempts: 1,
      seen: true,
      createdAt: minutesAgo(60 * 26),
      startedAt: minutesAgo(60 * 26 - 1),
      finishedAt: minutesAgo(60 * 25 + 40),
    },
    {
      id: 'job-1',
      kind: { type: 'research', question: DONE_QUESTION, brief: doneBrief },
      askedIn: 'c-job-ask',
      status: 'done',
      reason: '',
      progress: 'Wrote up 3 sources',
      steps: 23,
      outputs: [{ type: 'note', noteId: 'n-job-1' }],
      summary:
        'A four-stroke of 4 to 6 hp with a long shaft suits the boat. The note has a checklist ' +
        'for viewing one second-hand, three sources, and one question that is still open.',
      conversationId: 'c-job-1',
      answered: true,
      attempts: 1,
      seen: false,
      createdAt: minutesAgo(190),
      startedAt: minutesAgo(189),
      finishedAt: minutesAgo(172),
    },
  )

  // The thread that asked, as it reads after the job came back: the call, the
  // assistant saying so, the application's line, and the answer.
  const ask: ConversationId = 'c-job-ask'
  lend.putConversation({
    id: ask,
    title: 'Which outboard should I buy for the boat?',
    createdAt: minutesAgo(191),
    updatedAt: minutesAgo(171),
  })
  const callId = 'call-job-ask-1'
  for (const m of [
    message(ask, 'user', 'Which outboard should I buy for the boat? Look into it properly.', {
      createdAt: minutesAgo(191),
    }),
    message(
      ask,
      'assistant',
      'This one wants real reading, so I have started researching it. I will come back here with ' +
        'the answer when it is done.',
      {
        toolCalls: [
          {
            id: callId,
            name: 'start_research',
            arguments: { question: DONE_QUESTION, brief: doneBrief },
          },
        ],
        createdAt: minutesAgo(190),
      },
    ),
    message(ask, 'tool', `Started research: ${DONE_QUESTION}`, {
      toolCallId: callId,
      createdAt: minutesAgo(190),
    }),
    message(ask, 'system', `Research finished: ${DONE_QUESTION}`, { createdAt: minutesAgo(172) }),
    message(
      ask,
      'assistant',
      'I have finished looking into it. **A four-stroke of 4 to 6 hp with a long shaft** is the ' +
        'safe choice for your boat, and two-strokes are out as you wanted.\n\n' +
        'The note “Outboards for a small wooden sailboat” has a short checklist for viewing ' +
        'one second-hand, the three sources I leaned on, and the one thing I could not settle: ' +
        'whether 4 hp is enough for the tide at your harbour mouth.',
      { createdAt: minutesAgo(171) },
    ),
  ]) {
    lend.addMessage(m)
  }

  // The jobs' own transcripts, which the rail's history leaves out.
  lend.putConversation({
    id: 'c-job-1',
    title: gist(DONE_QUESTION),
    createdAt: minutesAgo(189),
    updatedAt: minutesAgo(172),
    jobId: 'job-1',
  })
  const t1: AgentMessage[] = [
    message('c-job-1', 'user', transcriptPrompt(DONE_QUESTION, doneBrief), {
      createdAt: minutesAgo(189),
    }),
    ...callPair(
      'c-job-1',
      1,
      'update_plan',
      { steps: [{ text: 'Search widely', status: 'active' }] },
      '',
      minutesAgo(188),
    ),
    ...callPair(
      'c-job-1',
      2,
      'web_search',
      { query: 'best small four-stroke outboard for a 22 ft wooden sailboat' },
      '8 results',
      minutesAgo(186),
    ),
    ...callPair(
      'c-job-1',
      3,
      'read_web_page',
      { url: 'https://overview.example/guide' },
      'Read overview.example',
      minutesAgo(182),
    ),
    ...callPair(
      'c-job-1',
      4,
      'read_web_page',
      { url: 'https://tests.example/reviews' },
      'Read tests.example',
      minutesAgo(178),
    ),
    ...callPair(
      'c-job-1',
      5,
      'update_note',
      { title: 'Outboards for a small wooden sailboat' },
      'Updated the note',
      minutesAgo(173),
    ),
    message(
      'c-job-1',
      'assistant',
      'A four-stroke of 4 to 6 hp with a long shaft suits the boat. The note has a checklist for ' +
        'viewing one second-hand, three sources, and one question that is still open.',
      { createdAt: minutesAgo(172) },
    ),
  ]
  for (const m of t1) lend.addMessage(m)

  lend.putConversation({
    id: 'c-job-2',
    title: gist(FAILED_QUESTION),
    createdAt: minutesAgo(60 * 26 - 1),
    updatedAt: minutesAgo(60 * 25 + 40),
    jobId: 'job-2',
  })
  const t2: AgentMessage[] = [
    message('c-job-2', 'user', transcriptPrompt(FAILED_QUESTION, ''), {
      createdAt: minutesAgo(60 * 26 - 1),
    }),
    ...callPair(
      'c-job-2',
      1,
      'web_search',
      { query: 'south coast harbour berthing fees 2025' },
      '8 results',
      minutesAgo(60 * 26 - 3),
    ),
    ...callPair(
      'c-job-2',
      2,
      'read_web_page',
      { url: 'https://overview.example/guide' },
      'Read overview.example',
      minutesAgo(60 * 26 - 8),
    ),
    ...callPair(
      'c-job-2',
      3,
      'read_web_page',
      { url: 'https://tests.example/reviews' },
      'Read tests.example',
      minutesAgo(60 * 25 + 41),
    ),
  ]
  for (const m of t2) lend.addMessage(m)
}

// ── Commands ────────────────────────────────────────────────────────────

export function mockListJobs(query: JobQuery = {}): Job[] {
  let rows = [...jobs]
  if (query.statuses?.length) rows = rows.filter((j) => query.statuses!.includes(j.status))
  if (query.unseen != null) rows = rows.filter((j) => j.seen === !query.unseen)
  if (query.askedIn) rows = rows.filter((j) => j.askedIn === query.askedIn)
  if (query.since) rows = rows.filter((j) => j.createdAt >= query.since!)
  rows.sort((a, b) => b.createdAt.localeCompare(a.createdAt) || b.id.localeCompare(a.id))
  return rows.slice(0, query.limit ?? rows.length).map(snapshot)
}

export function mockGetJob(id: JobId): Job {
  return snapshot(find(id))
}

export function mockUnseenJobs(): number {
  return jobs.filter((j) => !j.seen).length
}

export function mockMarkJobsSeen(ids: JobId[]): void {
  // Empty means all of them, as `mark_runs_seen` does.
  for (const job of jobs) {
    if (ids.length === 0 || ids.includes(job.id)) job.seen = true
  }
}

/**
 * Queue a research job and start its script.
 *
 * Returns at once with the job queued, as the real command does; everything
 * after that happens on timers, so this is the only place the script begins.
 * `holdAnswer` finishes the job without the follow-up, to leave the state that
 * offers "Answer now" -- see the top of this file.
 */
export function mockStartResearch(
  question: string,
  brief: string,
  conversationId: ConversationId | null,
  holdAnswer = false,
): Job {
  const text = question.replace(/\s+/g, ' ').trim()
  if (!text) {
    throw new VaultError('invalid', 'a research job needs a question: what should it find out?')
  }
  jobCounter += 1
  const job: Job = {
    id: `job-${jobCounter}`,
    kind: { type: 'research', question: text, brief: brief.trim() },
    askedIn: conversationId,
    status: 'queued',
    reason: '',
    progress: '',
    steps: 0,
    outputs: [],
    summary: '',
    conversationId: null,
    answered: false,
    attempts: 0,
    seen: false,
    createdAt: now(),
    startedAt: null,
    finishedAt: null,
  }
  jobs.push(job)
  if (holdAnswer) held.add(job.id)
  emit('job', 'created', job.id)
  pump()
  return snapshot(job)
}

/**
 * Stop a queued or running job. Whatever it wrote stays, as the note does.
 * Refused for one that is over, as `Job::cancel` is.
 */
export function mockCancelJob(id: JobId): Job {
  const job = find(id)
  if (job.status !== 'queued' && job.status !== 'running') {
    throw new VaultError('invalid', `that job is already ${job.status}; there is nothing to cancel`)
  }
  clearTimeout(timers.get(id))
  timers.delete(id)
  job.status = 'cancelled'
  job.reason = 'cancelled by you'
  job.finishedAt = now()
  emit('job', 'updated', job.id)
  release(id)
  return snapshot(job)
}

/** Delete a job and its transcript. The note it made is left alone. */
export function mockDeleteJob(id: JobId): void {
  const at = jobs.findIndex((j) => j.id === id)
  if (at < 0) return
  const job = jobs[at]!
  clearTimeout(timers.get(id))
  timers.delete(id)
  jobs.splice(at, 1)
  if (job.conversationId) lent().dropConversation(job.conversationId)
  emit('job', 'deleted', id)
  release(id)
}

/**
 * Have the follow-up in the asking conversation, for a job whose answer did
 * not happen or failed. Resolves when it has.
 */
export async function mockAnswerJob(id: JobId): Promise<void> {
  const job = find(id)
  if (job.status !== 'done') {
    throw new VaultError('invalid', 'that job has not finished, so there is nothing to answer yet')
  }
  if (!job.askedIn) {
    throw new VaultError(
      'invalid',
      'that job was not started from a conversation, so there is nowhere to answer',
    )
  }
  await respond(job)
}

// ── The script ──────────────────────────────────────────────────────────

/** Timers by job, so a cancel or a delete can stop the one that is pending. */
const timers = new Map<JobId, Timer>()

/** The job the script is on, if any. One at a time, as the worker is. */
let working: JobId | null = null

/** Jobs to finish without the follow-up. */
const held = new Set<JobId>()

/** Run `fn` after `ms`, as this job's pending beat. */
function later(id: JobId, ms: number, fn: () => void): void {
  clearTimeout(timers.get(id))
  timers.set(
    id,
    unreffed(
      setTimeout(() => {
        timers.delete(id)
        fn()
      }, ms * tempo),
    ),
  )
}

/** Start the oldest queued job, if the worker is free. */
function pump(): void {
  if (working) return
  const next = jobs
    .filter((j) => j.status === 'queued')
    .sort((a, b) => a.createdAt.localeCompare(b.createdAt))[0]
  if (!next) return
  working = next.id
  later(next.id, QUEUED_MS, () => begin(next.id))
}

/** The worker is free again, if it was this job it had. */
function release(id: JobId): void {
  if (working !== id) return
  working = null
  pump()
}

interface Step {
  tool: string
  args: (question: string) => Record<string, unknown>
  progress: (question: string) => string
  result: string
  /** What the note says it learned, for a step that reads something. */
  found?: string
  /** Which of `SOURCES` it read. */
  source?: string
}

const read = (source: string, found: string): Step => ({
  tool: 'read_web_page',
  args: () => ({ url: SOURCES[source]![1] }),
  progress: () => `Reading ${source}`,
  result: `Read ${source}`,
  found,
  source,
})

const SCRIPT: Step[] = [
  {
    tool: 'update_plan',
    args: () => ({
      steps: [
        { text: 'Search widely', status: 'active' },
        { text: 'Read the best sources', status: 'pending' },
        { text: 'Write it up', status: 'pending' },
      ],
    }),
    progress: () => 'Planning the search',
    result: '',
  },
  {
    tool: 'web_search',
    args: (q) => ({ query: gist(q, 80) }),
    progress: (q) => `Searching for ${gist(q, 44)}`,
    result: '8 results',
  },
  read('overview.example', 'overview.example sets out the usual answer, and the case against it.'),
  {
    tool: 'web_search',
    args: () => ({ query: 'problems and criticisms' }),
    progress: () => 'Searching for what the sceptics say',
    result: '6 results',
  },
  read('forum.example', 'forum.example has people who tried it, and where it went wrong for them.'),
  read('tests.example', 'tests.example compares the options side by side, method written down.'),
  {
    tool: 'update_note',
    args: (q) => ({ title: gist(q, 60) }),
    progress: () => 'Writing up what it found',
    result: 'Updated the note',
  },
]

/** The worker picked it up: give it a transcript and a note, and start on step one. */
function begin(id: JobId): void {
  const job = jobs.find((j) => j.id === id)
  if (!job || job.status !== 'queued') return
  const { question, brief } = asked(job)
  const lend = lent()

  job.status = 'running'
  job.attempts += 1
  job.startedAt = now()
  job.progress = 'Starting'
  job.conversationId = `c-${job.id}`
  lend.putConversation({
    id: job.conversationId,
    title: gist(question),
    createdAt: now(),
    updatedAt: now(),
    jobId: job.id,
  })
  lend.addMessage(message(job.conversationId, 'user', transcriptPrompt(question, brief)))
  emit('conversation', 'created', job.conversationId)

  // The note is the scratchpad: it exists from the first moment, and the job
  // points at it straight away rather than at the end.
  const note: Note = {
    id: `n-${job.id}`,
    title: gist(question, 80),
    body: workingBody([]),
    tags: ['research'],
    pinned: false,
    attachments: [],
    createdAt: now(),
    updatedAt: now(),
  }
  save(note)
  job.outputs = [{ type: 'note', noteId: note.id }]
  emit('note', 'created', note.id)
  emit('job', 'updated', job.id)

  later(id, STEP_MS, () => step(id, 0))
}

/** The sources the steps up to `upTo` had read. */
function readSoFar(upTo: number): { found: string[]; sources: [string, string][] } {
  const reads = SCRIPT.slice(0, upTo).filter((s) => s.found)
  return {
    found: reads.map((s) => s.found!),
    sources: reads.map((s) => SOURCES[s.source!]!),
  }
}

/** One tool call: it appears in the transcript, the progress line moves, the note grows. */
function step(id: JobId, index: number): void {
  const job = jobs.find((j) => j.id === id)
  if (!job || job.status !== 'running') return
  const { question } = asked(job)

  if (index >= SCRIPT.length) return finish(job)
  if (index === 4 && /impossible/i.test(question)) {
    return fail(
      job,
      `The model provider stopped answering (HTTP 503) after ${job.steps} steps. What it had ` +
        'found before then is in the note.',
    )
  }

  const lend = lent()
  const scripted = SCRIPT[index]!
  job.steps += 1
  job.progress = scripted.progress(question)
  if (job.conversationId) {
    for (const m of callPair(
      job.conversationId,
      job.steps,
      scripted.tool,
      scripted.args(question),
      scripted.result,
      now(),
    )) {
      lend.addMessage(m)
    }
  }

  const note = noteFor(job)
  if (note && scripted.found) {
    const { found } = readSoFar(index + 1)
    note.body = workingBody(found)
    note.updatedAt = now()
    save(note)
    emit('note', 'updated', note.id)
  }
  emit('job', 'updated', job.id)
  later(job.id, STEP_MS, () => step(job.id, index + 1))
}

/** It could not finish. The note it had written stays. */
function fail(job: Job, reason: string): void {
  job.status = 'failed'
  job.reason = reason
  job.finishedAt = now()
  job.seen = false
  emit('job', 'updated', job.id)
  release(job.id)
}

/** It finished: write the note up, tell the asking conversation, and answer in it. */
function finish(job: Job): void {
  const lend = lent()
  const { question } = asked(job)
  const { found, sources } = readSoFar(SCRIPT.length)
  const note = noteFor(job)
  if (note) {
    note.body = finalBody(question, found, sources)
    note.updatedAt = now()
    save(note)
    emit('note', 'updated', note.id)
  }

  const summary =
    'This is a stand-in: the demo build has no model. The note has the shape of a real one, ' +
    `with ${sources.length} sources.`
  job.status = 'done'
  job.progress = `Wrote up ${sources.length} sources`
  job.summary = summary
  job.finishedAt = now()
  // Finishing is what is worth a number on the app bar. A job somebody was
  // looking at while it ran is news again once it has something to say.
  job.seen = false
  if (job.conversationId) {
    lend.addMessage(message(job.conversationId, 'assistant', summary))
    emit('conversation', 'updated', job.conversationId)
  }
  emit('job', 'updated', job.id)
  release(job.id)

  if (job.askedIn) void comeBack(job)
}

/**
 * Come back to the conversation that asked: the application's line that the
 * job finished, then the assistant's answer from the note, a moment later.
 * A job started with `holdAnswer` stops after the line.
 */
async function comeBack(job: Job): Promise<void> {
  await announce(job)
  if (!held.has(job.id)) await respond(job)
}

/**
 * The line in the thread: "Research finished". Waits for a turn being written
 * into that conversation first, as the real follow-up does, so that it never
 * lands in the middle of somebody's reply.
 */
async function announce(job: Job): Promise<void> {
  const conversation = job.askedIn
  if (!conversation) return
  const lend = lent()
  while (lend.busy(conversation)) await wait(BUSY_MS)
  if (!jobs.includes(job)) return
  const line = `Research finished: ${gist(asked(job).question, 90)}`
  lend.addMessage(message(conversation, 'system', line))
  emit('conversation', 'updated', conversation)
  emit('job', 'updated', job.id)
}

/** The assistant's answer, in the conversation that asked. */
async function respond(job: Job): Promise<void> {
  const conversation = job.askedIn
  if (!conversation) return
  const lend = lent()
  await wait(ANSWER_MS)
  while (lend.busy(conversation)) await wait(BUSY_MS)
  // Deleted while it was being written: nothing to say now.
  if (!jobs.includes(job)) return
  const title = noteFor(job)?.title ?? gist(asked(job).question, 90)
  lend.addMessage(
    message(
      conversation,
      'assistant',
      `I have finished looking into that. The note “${title}” has what I found, ` +
        'how sure I am of each part, and what is still open.\n\n' +
        '*This is a scripted answer from the mock backend. There is no model behind it.*',
    ),
  )
  job.answered = true
  emit('conversation', 'updated', conversation)
  emit('job', 'updated', job.id)
}
