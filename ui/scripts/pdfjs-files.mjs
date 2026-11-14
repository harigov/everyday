// pdf.js's own data files, at `/pdfjs/…` in the dev server and the build.
//
// pdf.js reads three kinds of file by name from a folder it is given the
// address of: the Adobe CMaps a CJK document's text is mapped through, the
// standard fonts a document that embeds none is drawn with, and the decoders
// for JPEG 2000 and JBIG2 pictures -- the second of which is what nearly every
// black-and-white scan is made of. It builds those addresses itself, from the
// folder and a file name, so they cannot be hashed assets that Vite names; and
// it loads one of them as a module, so they cannot be anything but real files.
// This serves them from `node_modules` in development and copies them into the
// build for production, under one fixed path the viewer points pdf.js at
// (`src/lib/pdf.ts`).
//
// Not `quickjs-eval`, which runs a document's own JavaScript: the viewer never
// does that.
//
// Typed by `pdfjs-files.d.mts` beside it, because the interface's tsconfig has
// no Node types to check this file against.

import { createReadStream, readdirSync, readFileSync, statSync } from 'node:fs'
import { extname, join } from 'node:path'
import { fileURLToPath } from 'node:url'

const source = fileURLToPath(new URL('../node_modules/pdfjs-dist/', import.meta.url))

/** Where the files are served from, under the application's own origin. */
export const PDFJS_PATH = 'pdfjs/'

/** Each folder of `pdfjs-dist` that ships, and which of its files. */
const FOLDERS = {
  cmaps: (name) => name.endsWith('.bcmap'),
  standard_fonts: (name) => name.endsWith('.pfb') || name.endsWith('.ttf'),
  wasm: (name) => !name.startsWith('quickjs') && !name.startsWith('LICENSE'),
}

const TYPES = {
  '.js': 'text/javascript',
  '.wasm': 'application/wasm',
}

function shipped(folder, name) {
  const keep = FOLDERS[folder]
  return Boolean(keep) && Boolean(name) && !name.includes('/') && !name.includes('..') && keep(name)
}

export function pdfjsFiles() {
  return {
    name: 'karmayoga-pdfjs-files',
    configureServer(server) {
      server.middlewares.use(`/${PDFJS_PATH}`, (request, response, next) => {
        const path = decodeURIComponent((request.url ?? '').split('?')[0]).replace(/^\/+/, '')
        const [folder, name, ...rest] = path.split('/')
        if (rest.length > 0 || !shipped(folder, name)) return next()
        const file = join(source, folder, name)
        try {
          statSync(file)
        } catch {
          return next()
        }
        response.setHeader('Content-Type', TYPES[extname(name)] ?? 'application/octet-stream')
        createReadStream(file).pipe(response)
      })
    },
    generateBundle() {
      for (const folder of Object.keys(FOLDERS)) {
        for (const name of readdirSync(join(source, folder))) {
          if (!shipped(folder, name)) continue
          this.emitFile({
            type: 'asset',
            fileName: `${PDFJS_PATH}${folder}/${name}`,
            source: readFileSync(join(source, folder, name)),
          })
        }
      }
    },
  }
}
