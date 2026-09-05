import { defineConfig } from 'vite'
import { svelte } from '@sveltejs/vite-plugin-svelte'

// Tauri embeds the built output in the Rust binary for release, and points
// the webview at this dev server during `tauri dev`.
//
// Running `npm run dev` on its own also works: with no Tauri host present, a
// development build falls back to an in-memory backend so the interface can
// be worked on in a browser. That fallback is compiled out of release
// builds -- see the MOCK constant in src/lib/api.ts.
export default defineConfig({
  plugins: [svelte()],
  clearScreen: false,
  server: { port: 5273, strictPort: true },
  build: {
    // Matches the webview floor: WebKitGTK on Linux, WKWebView on macOS 11+,
    // and Edge WebView2 on Windows.
    target: ['es2022', 'safari15'],
    sourcemap: false,
    chunkSizeWarningLimit: 900,
  },
})
