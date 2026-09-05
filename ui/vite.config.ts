import { defineConfig } from 'vite'
import { svelte } from '@sveltejs/vite-plugin-svelte'

// Tauri serves this over a custom protocol in production and over a plain
// dev server in development. `EVERYDAY_MOCK=1 npm run dev` swaps the backend
// for an in-memory fake so the interface can be worked on in a browser,
// without a vault and without building the Rust side.
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
