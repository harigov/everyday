// The frame around every app on a phone: the sidebar's drawer, the More
// sheet and choosing the bar, the assistant over an app, Settings, and back
// out of each. Keyed `shell`; each scenario names the app it opens on.

import { back, press, tap } from '../helpers.mjs'

const desktop = () => globalThis.PHONE_CHECK_DESKTOP === true

/** Something the mock's own modules can say, in the page: the store a screen
 *  reads, set the way the app would have. The dev server serves each module
 *  at its path, so this reaches the one instance the app is using. */
const inPage = (page, fn, arg) => page.evaluate(fn, arg)

/** A long press on a row: Android sends `contextmenu` at the finger. */
async function longPress(page, selector) {
  await page.evaluate((sel) => {
    const el = document.querySelector(sel)
    const r = el.getBoundingClientRect()
    el.dispatchEvent(
      new MouseEvent('contextmenu', {
        bubbles: true,
        cancelable: true,
        clientX: r.left + Math.min(r.width / 2, 120),
        clientY: r.top + Math.min(r.height / 2, 20),
      }),
    )
  }, selector)
}

/** Type into the top bar's field, as a person with a keyboard up would. */
async function typeInBar({ page, settle }, text) {
  await press(page.locator('[data-search]').first())
  await page.keyboard.type(text)
  await settle()
}

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
    // The sheet over the todo list: it is the sheet that is being checked, not
    // whichever app happens to be under it.
    {
      name: 'more',
      section: 'todo',
      run: async ({ page, settle }) => {
        await press(page.getByRole('button', { name: 'More', exact: true }))
        await settle()
      },
    },
    {
      name: 'more-edit',
      section: 'todo',
      run: async ({ page, settle }) => {
        await press(page.getByRole('button', { name: 'More', exact: true }))
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
        await press(page.getByRole('button', { name: 'More', exact: true }))
        await settle()
        await press(page.getByRole('button', { name: 'Settings' }))
        await settle()
      },
    },
    // ── What lies over every app ─────────────────────────────────────────
    {
      // The top bar's field with a search typed in: the list of commands and
      // captures it drops down.
      name: 'commandbar',
      section: 'todo',
      run: (ctx) => typeInBar(ctx, 'go'),
    },
    {
      name: 'commandbar-notes',
      section: 'notes',
      run: (ctx) => typeInBar(ctx, 'lock'),
    },
    {
      // Back puts the dropdown away.
      name: 'commandbar-back',
      section: 'todo',
      run: async (ctx) => {
        await typeInBar(ctx, 'go')
        if (!desktop()) {
          await back(ctx.page)
          await ctx.settle()
        }
      },
    },
    {
      // A long press on a task.
      name: 'contextmenu',
      section: 'todo',
      run: async ({ page, settle }) => {
        await longPress(page, '.list-row')
        await settle()
      },
    },
    {
      // A menu with a submenu in it, opened.
      name: 'contextmenu-sub',
      section: 'todo',
      run: async ({ page, settle }) => {
        await longPress(page, '.list-row')
        await settle()
        await press(page.getByRole('menuitem', { name: /^Priority/ }))
        await settle()
      },
    },
    {
      name: 'contextmenu-back',
      section: 'todo',
      run: async ({ page, settle }) => {
        await longPress(page, '.list-row')
        await settle()
        if (!desktop()) {
          await back(page)
          await settle()
        }
      },
    },
    {
      // "Delete task..." asks first.
      name: 'confirm',
      section: 'todo',
      run: async ({ page, settle }) => {
        await longPress(page, '.list-row')
        await settle()
        await press(page.getByRole('menuitem', { name: /^Delete task/ }))
        await settle()
      },
    },
    {
      name: 'confirm-back',
      section: 'todo',
      run: async ({ page, settle }) => {
        await longPress(page, '.list-row')
        await settle()
        await press(page.getByRole('menuitem', { name: /^Delete task/ }))
        await settle()
        if (!desktop()) {
          await back(page)
          await settle()
        }
      },
    },
    {
      // A question with three answers: which of a series.
      name: 'choice',
      section: 'todo',
      run: async ({ page, settle }) => {
        await inPage(page, async () => {
          const { money } = await import('/src/lib/money.svelte.ts')
          const noop = () => {}
          money.question = {
            title: 'Delete "Monzo Current Account"?',
            detail:
              'Its 1,204 transactions go with it. Transactions typed in by hand are kept elsewhere.',
            options: [
              { label: 'Delete the account and its transactions', danger: true, run: noop },
              { label: 'Delete the account, keep the transactions', danger: true, run: noop },
            ],
            cancel: noop,
          }
        })
        await settle()
      },
    },
    {
      name: 'toasts',
      section: 'todo',
      run: async ({ page, settle }) => {
        await inPage(page, async () => {
          const { notify } = await import('/src/lib/notify.svelte.ts')
          notify.success('Marked 12 conversations read', { body: 'Inbox' })
          notify.info('Draft saved', {
            body: 'It is in Drafts.',
            action: { label: 'Undo', run() {} },
          })
          notify.error('Could not write to /Volumes/Backups/very/long/path/that/goes/on', {
            body: 'The disk is full or not mounted.',
          })
        })
        await settle()
      },
    },
    {
      // The three banners, one at a time would hide that they share a line.
      name: 'notice-error',
      section: 'todo',
      run: async ({ page, settle }) => {
        await inPage(page, async () => {
          const { app } = await import('/src/lib/state.svelte.ts')
          app.error =
            'Could not reach the server at imap.example.com:993 after three tries; the connection was reset.'
        })
        await settle()
      },
    },
    {
      name: 'notice-conflict',
      section: 'todo',
      run: async ({ page, settle }) => {
        await inPage(page, async () => {
          const { app } = await import('/src/lib/state.svelte.ts')
          app.conflict = true
        })
        await settle()
      },
    },
    {
      name: 'notice-readonly',
      section: 'todo',
      run: async ({ page, settle }) => {
        await inPage(page, async () => {
          const { app } = await import('/src/lib/state.svelte.ts')
          app.status = { ...app.status, writable: false }
        })
        await settle()
      },
    },
    {
      // Nothing to press it with on a phone, but if it is opened it must not
      // look broken.
      name: 'shortcuts',
      section: 'todo',
      run: async ({ page, settle }) => {
        await inPage(page, async () => {
          const { panels } = await import('/src/lib/panels.svelte.ts')
          panels.shortcuts = true
        })
        await settle()
      },
    },
    {
      name: 'lock',
      section: 'overview',
      run: async ({ page, settle }) => {
        if (desktop()) {
          await press(page.locator('.foot .barbtn[title^="Lock"]'))
        } else {
          await press(page.getByRole('button', { name: 'More', exact: true }))
          await settle()
          await press(page.getByRole('button', { name: 'Lock' }))
        }
        await settle()
      },
    },
    {
      // A wrong password: the message under the field.
      name: 'lock-wrong',
      section: 'overview',
      run: async ({ page, settle }) => {
        await inPage(page, async () => {
          const { app } = await import('/src/lib/state.svelte.ts')
          app.screen = 'locked'
          app.error = 'That password did not open the vault.'
        })
        await settle()
      },
    },
    {
      name: 'error-screen',
      section: 'overview',
      run: async ({ page, settle }) => {
        await inPage(page, async () => {
          const { app } = await import('/src/lib/state.svelte.ts')
          app.error =
            'Could not reach 192.168.1.20:7397 (connection refused). The certificate this phone pinned is for a different computer.'
          app.screen = 'error'
        })
        await settle()
      },
    },
    {
      // A phone offers pairing only; a desktop, a new vault as well.
      name: 'setup',
      section: 'overview',
      run: async ({ page, settle }) => {
        await inPage(page, async () => {
          const { app } = await import('/src/lib/state.svelte.ts')
          app.screen = 'setup'
        })
        await settle()
      },
    },
    {
      // Having paired before: the computers it knows.
      name: 'setup-known',
      section: 'overview',
      run: async ({ page, settle }) => {
        await inPage(page, async () => {
          const { app } = await import('/src/lib/state.svelte.ts')
          app.remotes = [
            {
              id: 'a',
              name: "Hari's workstation",
              host: '192.168.1.20:7397',
              paired: '2026-11-20',
            },
            {
              id: 'b',
              name: 'Laptop on the tailnet',
              host: 'laptop.tail1234.ts.net:7397',
              paired: '2026-11-02',
            },
          ]
          app.screen = 'setup'
        })
        await settle()
      },
    },
  ],
}
