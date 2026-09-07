// Svelte configuration.
//
// This file exists for `svelte-check`, not for the build: Vite gets its
// Svelte setup from `vite.config.ts`, and the plugin is happy without a
// config file of any kind. `svelte-check` is not. Without one it cannot
// resolve the compiler options for a component and reports
//
//   Error in vite.config -- No Svelte configuration found in vite config
//
// once per `.svelte` file, which is what `npm run check` did for every
// component in this interface: 27 errors, a non-zero exit, and not one line
// of the interface actually typechecked. The check looked like it was
// failing loudly while in fact it was not running at all.
//
// `vitePreprocess` is what the plugin applies during a build, so naming it
// here is what makes the checker compile a component the same way the
// application does -- `lang="ts"` in a `<script>` above all.
import { vitePreprocess } from '@sveltejs/vite-plugin-svelte'

export default {
  preprocess: vitePreprocess(),
}
