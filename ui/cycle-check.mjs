// Does importing the two stores that reference each other actually work?
import { createServer } from 'vite'
import { svelte } from '@sveltejs/vite-plugin-svelte'

const server = await createServer({
  configFile: false,
  root: process.cwd(),
  plugins: [svelte({ compilerOptions: { hmr: false } })],
  server: { middlewareMode: true },
  appType: 'custom',
  logLevel: 'error',
})

try {
  // The order that matters: whichever loads first must not find the other
  // half-built.
  const p = await server.ssrLoadModule('/src/lib/purpose.svelte.ts')
  const s = await server.ssrLoadModule('/src/lib/state.svelte.ts')
  console.log('purpose ->', typeof p.purpose, 'app ->', typeof s.app)
  console.log('describe(null) ->', JSON.stringify(p.purpose.describe(null).name))
  // And the other order, in a fresh graph.
  await server.close()
  const s2 = await createServer({
    configFile: false, root: process.cwd(),
    plugins: [svelte({ compilerOptions: { hmr: false } })],
    server: { middlewareMode: true }, appType: 'custom', logLevel: 'error',
  })
  const a = await s2.ssrLoadModule('/src/lib/state.svelte.ts')
  const b = await s2.ssrLoadModule('/src/lib/purpose.svelte.ts')
  console.log('reverse order: app ->', typeof a.app, 'purpose ->', typeof b.purpose)
  await s2.close()
  console.log('OK: no import cycle failure')
} catch (e) {
  console.log('FAILED:', e.message)
  process.exitCode = 1
}
