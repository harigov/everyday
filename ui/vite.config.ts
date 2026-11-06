import { defineConfig } from 'vite'
import { svelte } from '@sveltejs/vite-plugin-svelte'
import { pdfjsFiles } from './scripts/pdfjs-files.mjs'

// Tauri embeds the built output in the Rust binary for release, and points
// the webview at this dev server during `tauri dev`.
//
// Running `npm run dev` on its own also works: with no Tauri host present, a
// development build falls back to an in-memory backend so the interface can
// be worked on in a browser. That fallback is compiled out of release
// builds -- see the MOCK constant in src/lib/api.ts.
export default defineConfig({
  // `pdfjsFiles`: the CMaps, fonts and decoders the attachment viewer's PDF
  // reader loads by name -- see `scripts/pdfjs-files.mjs`.
  plugins: [svelte(), pdfjsFiles()],
  clearScreen: false,
  server: { port: 5273, strictPort: true },
  // pdf.js is only ever reached through a dynamic `import()`, which the dev
  // server would otherwise find mid-session and re-bundle under a new
  // address, failing the first PDF opened with "Outdated Optimize Dep".
  optimizeDeps: { include: ['pdfjs-dist/legacy/build/pdf.mjs'] },
  // pdf.js starts its worker as a module (`src/lib/pdf.ts`), so it is built as
  // one.
  worker: { format: 'es' },
  build: {
    // Matches the webview floor: WebKitGTK on Linux, WKWebView on macOS 11+,
    // and Edge WebView2 on Windows.
    target: ['es2022', 'safari15'],
    sourcemap: false,
    chunkSizeWarningLimit: 900,
    // Two pages: the application, and writing help's popup -- a window of
    // its own that loads none of the application. See src/writing/.
    rollupOptions: {
      input: {
        main: 'index.html',
        writing: 'writing.html',
      },
    },
  },
})
