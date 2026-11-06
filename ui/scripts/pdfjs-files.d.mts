import type { Plugin } from 'vite'

/** Where pdf.js's data files are served from, under the application's origin. */
export declare const PDFJS_PATH: string

/** Serves pdf.js's data files in development and copies them into the build.
 *  See `pdfjs-files.mjs`. */
export declare function pdfjsFiles(): Plugin
