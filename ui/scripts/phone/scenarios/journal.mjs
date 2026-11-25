// The journal on a phone: the list of entries, an entry opened over it, and
// back to the list. See ../scenarios.mjs.

import { back, press } from '../helpers.mjs'

const openFirst = async ({ page, settle }) => {
  await press(page.locator('[role=option], .row, .entry').first())
  await settle()
}

export default {
  journal: [
    { name: 'home' },
    { name: 'entry', run: openFirst },
    {
      name: 'entry-back',
      run: async (ctx) => {
        await openFirst(ctx)
        await back(ctx.page)
        await ctx.settle()
      },
    },
  ],
}
