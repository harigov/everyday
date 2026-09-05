/// <reference types="vite/client" />

interface ImportMetaEnv {
  /** Set by `EVERYDAY_MOCK=1 npm run dev` to run without the Rust backend. */
  readonly VITE_EVERYDAY_MOCK?: string
}

interface ImportMeta {
  readonly env: ImportMetaEnv
}
