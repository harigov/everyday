// `fetchWhole` (`lib/api.ts`): all of a file's bytes for pdf.js, from an
// address that answers a long file with its first eight megabytes and a 206.
//
// Driven with a `fetch` of its own that answers ranges the way the server's
// `file_plan` does, so what is checked is the arithmetic of asking for the
// rest and the refusal to stitch together pieces of two different files.

import assert from 'node:assert/strict'
import { load, stubBrowser } from './harness.mjs'

stubBrowser({
  window: globalThis,
  localStorage: true,
  matchMedia: true,
  document: {
    querySelector: () => null,
    documentElement: {
      style: { setProperty: () => {} },
      classList: { toggle: () => {} },
      setAttribute: () => {},
      removeAttribute: () => {},
    },
    addEventListener: () => {},
  },
  navigator: { userAgent: 'node', language: 'en-GB' },
  location: new URL('http://localhost/?unlocked=1'),
  createObjectURL: () => 'blob:stub',
})

const { module: api, close } = await load('/src/lib/api.ts', { svelte: true })
const { contentRange, fetchWhole } = api

const MB8 = 8 * 1024 * 1024
/** The byte at `i` of the pretend file: where it is, so a piece put in the
 *  wrong place shows. */
const byteAt = (i) => (i * 7 + (i >> 16)) & 0xff

/** A server for one file of `total` bytes: whole up to `whole`, the first
 *  eight megabytes and a 206 past it, and any range asked for (at most eight
 *  megabytes of it). `tamper` changes what a ranged answer says. */
function server(total, { whole = 64 * 1024 * 1024, tamper } = {}) {
  const asked = []
  const fetch = async (_url, init) => {
    const range = init?.headers?.Range ?? null
    asked.push(range)
    let start = 0
    let end = total - 1
    let partial = false
    if (range) {
      const m = /^bytes=(\d+)-(\d+)$/.exec(range)
      start = Number(m[1])
      end = Math.min(Number(m[2]), total - 1, start + MB8 - 1)
      partial = true
    } else if (total > whole) {
      end = MB8 - 1
      partial = true
    }
    const body = new Uint8Array(end - start + 1)
    for (let i = 0; i < body.length; i++) body[i] = byteAt(start + i)
    const headers = new Headers()
    let status = 200
    if (partial) {
      status = 206
      headers.set('Content-Range', `bytes ${start}-${end}/${total}`)
    }
    const answer = { status, headers, body }
    if (range && tamper) tamper(answer, start)
    return new Response(answer.body, { status: answer.status, headers: answer.headers })
  }
  return { fetch, asked }
}

function same(bytes, total) {
  assert.equal(bytes.length, total, 'every byte')
  for (let i = 0; i < total; i += 4093) assert.equal(bytes[i], byteAt(i), `byte ${i}`)
  assert.equal(bytes[total - 1], byteAt(total - 1), 'the last byte')
}

// ── the header ───────────────────────────────────────────────────────

assert.deepEqual(contentRange('bytes 0-8388607/70000000'), { start: 0, total: 70_000_000 })
assert.deepEqual(contentRange('bytes 8388608-16777215/70000000'), {
  start: 8_388_608,
  total: 70_000_000,
})
assert.equal(contentRange(null), null)
assert.equal(contentRange('bytes */70000000'), null, 'an unsatisfied range says no start')

// ── a file small enough to be answered whole ─────────────────────────

{
  const { fetch, asked } = server(100_000)
  globalThis.fetch = fetch
  same(await fetchWhole('jatayu://files/file/x'), 100_000)
  assert.deepEqual(asked, [null], 'one read, as before')
}

// ── a long one: the rest a range at a time ───────────────────────────

{
  const total = 20_000_000
  const { fetch, asked } = server(total, { whole: 1_000_000 })
  globalThis.fetch = fetch
  same(await fetchWhole('jatayu://files/file/x'), total)
  assert.deepEqual(asked, [null, `bytes=${MB8}-${2 * MB8 - 1}`, `bytes=${2 * MB8}-${total - 1}`])
}

// ── a piece of something else ────────────────────────────────────────

{
  // The file changed between the reads: the second answer is of a longer one.
  const { fetch } = server(20_000_000, {
    whole: 1_000_000,
    tamper: (answer, start) =>
      answer.headers.set(
        'Content-Range',
        `bytes ${start}-${start + answer.body.length - 1}/20000001`,
      ),
  })
  globalThis.fetch = fetch
  await assert.rejects(fetchWhole('jatayu://files/file/x'), 'a different length is refused')
}
{
  // A range ignored, and the start of the file sent again.
  const { fetch } = server(20_000_000, {
    whole: 1_000_000,
    tamper: (answer) => answer.headers.set('Content-Range', `bytes 0-${MB8 - 1}/20000000`),
  })
  globalThis.fetch = fetch
  await assert.rejects(fetchWhole('jatayu://files/file/x'), 'a piece from the wrong place too')
}
{
  // A 206 that does not say how long the file is cannot be finished.
  globalThis.fetch = async () => new Response(new Uint8Array(10), { status: 206 })
  await assert.rejects(fetchWhole('jatayu://files/file/x'))
}
{
  globalThis.fetch = async () => new Response('gone', { status: 404 })
  await assert.rejects(fetchWhole('jatayu://files/file/x'), 'an error is an error')
}

await close()
console.log('fetch-whole: all checks passed')
process.exit(0)
