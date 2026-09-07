// Lint rules for the interface.
//
// Correctness only. Prettier owns layout, and `eslint-config-prettier` is
// applied last to switch off every rule that would have an opinion about it,
// so the two can never disagree about a file.
//
// The rules that earn their place here are the type-aware ones -- they need a
// `tsconfig`, which is why `projectService` is on, and they catch the class of
// mistake this codebase is most exposed to. Nearly every method in the three
// stores is `async` and most call sites deliberately do not await; the
// codebase already marks those `void promise` by hand, which is exactly the
// convention `no-floating-promises` enforces. Before this config existed that
// convention was upheld by whoever was reading the diff.

import js from '@eslint/js'
import ts from 'typescript-eslint'
import svelte from 'eslint-plugin-svelte'
import prettier from 'eslint-config-prettier'
import globals from 'globals'
import svelteParser from 'svelte-eslint-parser'

export default ts.config(
  js.configs.recommended,
  ...ts.configs.recommendedTypeChecked,
  ...svelte.configs.recommended,

  {
    languageOptions: {
      globals: { ...globals.browser },
      parserOptions: {
        projectService: true,
        tsconfigRootDir: import.meta.dirname,
        extraFileExtensions: ['.svelte'],
      },
    },
  },

  {
    files: ['**/*.svelte', '**/*.svelte.ts'],
    languageOptions: {
      parser: svelteParser,
      parserOptions: {
        // Svelte components hold TypeScript in their `<script>`, and the
        // runes files are TypeScript outright.
        parser: ts.parser,
        svelteConfig: (await import('./svelte.config.js')).default,
      },
    },
  },

  {
    rules: {
      // The house style for a promise nobody is waiting on. Writing `void`
      // in front of it is a claim that the author considered it; leaving it
      // bare is usually an `await` someone forgot, which in a store method
      // means a write that races the next one.
      '@typescript-eslint/no-floating-promises': 'error',
      '@typescript-eslint/no-misused-promises': 'error',

      // `_`-prefixed arguments are the conventional way to say "required by
      // the signature, unused by me". Everything else unused is a mistake --
      // and `tsc` already rejects unused locals, so this covers the rest.
      '@typescript-eslint/no-unused-vars': [
        'error',
        { argsIgnorePattern: '^_', varsIgnorePattern: '^_' },
      ],

      // Off: it fires on every `new Date`, `new Map` and `new Set` in a
      // component, and all eight it found here were either a local in a
      // formatting helper -- `hourLabel` builds a `Date` to hand to an
      // `Intl` formatter and throws it away -- or the reassign-rather-than-
      // mutate pattern the todo store documents at `toggleExpanded`, which
      // is a correct way to make a `Set` reactive and not a mistake to be
      // talked out of. Reach for `SvelteSet` when you want mutation to be
      // observed; do not let a linter insist on it where nothing mutates.
      'svelte/prefer-svelte-reactivity': 'off',

      // Off: an `async` function with no `await` is how you satisfy a
      // signature that returns a promise. `api.ts` has one -- the `invoke`
      // stub whose whole body is a `throw` -- and it is right as it stands.
      '@typescript-eslint/require-await': 'off',
    },
  },

  {
    // The two behaviour suites run under Node, not in a browser, and are
    // plain JavaScript loaded through Vite rather than compiled by tsc.
    files: ['scripts/**/*.mjs', '*.config.js', '*.config.ts'],
    ...ts.configs.disableTypeChecked,
  },
  {
    // Separate from the block above on purpose: spreading
    // `disableTypeChecked` replaces `languageOptions` wholesale, so globals
    // set alongside it are silently dropped and `process` reads as undefined.
    files: ['scripts/**/*.mjs', '*.config.js', '*.config.ts'],
    languageOptions: { globals: { ...globals.node } },
  },

  { ignores: ['dist/', 'node_modules/'] },

  // Last, so it wins: no rule here may have an opinion about layout.
  prettier,
)
