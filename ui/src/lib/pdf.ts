// Opening a PDF for the attachment viewer, with pdf.js.
//
// Imported only when a PDF is opened (`PdfView.svelte` asks for it with a
// dynamic `import()`), so the half megabyte of pdf.js and the worker beside it
// are never loaded otherwise.
//
// The legacy build, not the modern one: it is the build pdf.js makes for
// browsers older than the newest, which is what a webview on a desktop that
// has not been updated in a while is. `vite.config.ts` aims at the same floor.
//
// The document is parsed in pdf.js's worker, which has no way to call the
// shell; the window only draws the pages the worker describes, onto canvases.
// Nothing is run that the PDF brings with it: pdf.js 6 has no `eval` path left
// for fonts, the window's CSP has no `'unsafe-eval'` for one anyway, and the
// viewer has no annotation layer, so a document's forms, links and scripts are
// never put on screen.

import {
  getDocument,
  GlobalWorkerOptions,
  type PDFDocumentLoadingTask,
} from 'pdfjs-dist/legacy/build/pdf.mjs'
// Through Vite's worker build, not as a file copied whole (`?url`): that is
// what lowers its syntax to the same floor as the rest of the interface. The
// legacy worker still has class static blocks, which a webview older than
// Safari 16.4 cannot parse.
import worker from 'pdfjs-dist/legacy/build/pdf.worker.mjs?worker&url'

GlobalWorkerOptions.workerSrc = worker

/** Where `scripts/pdfjs-files.mjs` serves pdf.js's CMaps, fonts and image
 *  decoders, under this window's own origin. */
const files = new URL('/pdfjs/', document.baseURI).href

/** Start reading `data` as a PDF. pdf.js takes the bytes over: `data` is
 *  empty afterwards. `destroy()` the task when the document is closed. */
export function openPdf(data: Uint8Array): PDFDocumentLoadingTask {
  return getDocument({
    data,
    cMapUrl: `${files}cmaps/`,
    standardFontDataUrl: `${files}standard_fonts/`,
    wasmUrl: `${files}wasm/`,
    enableXfa: false,
  })
}
