// The frame around every app on a phone: the sidebar's drawer, the More
// sheet and choosing the bar, the assistant over an app, Settings, and back
// out of each. Keyed `shell`; each scenario names the app it opens on.

import { back, press, tap } from '../helpers.mjs'

export default {
  shell: [
    {
      name: 'drawer',
      section: 'mail',
      run: async ({ page, settle }) => {
        await tap(page, 'button[aria-label="Show the sidebar"]')
        await settle()
      },
    },
    {
      name: 'drawer-back',
      section: 'mail',
      run: async ({ page, settle }) => {
        await tap(page, 'button[aria-label="Show the sidebar"]')
        await settle()
        await back(page)
        await settle()
      },
    },
    {
      name: 'more',
      section: 'overview',
      run: async ({ page, settle }) => {
        await press(page.getByRole('button', { name: 'More' }))
        await settle()
      },
    },
    {
      name: 'more-edit',
      section: 'overview',
      run: async ({ page, settle }) => {
        await press(page.getByRole('button', { name: 'More' }))
        await settle()
        await press(page.getByRole('button', { name: 'Edit bar' }))
        await settle()
      },
    },
    {
      name: 'assistant',
      section: 'todo',
      run: async ({ page, settle }) => {
        await tap(page, 'button[aria-label="Open the assistant"]')
        await settle(800)
      },
    },
    {
      name: 'settings',
      section: 'overview',
      run: async ({ page, settle }) => {
        await press(page.getByRole('button', { name: 'More' }))
        await settle()
        await press(page.getByRole('button', { name: 'Settings' }))
        await settle()
      },
    },
  ],
}
