<script lang="ts">
  // The step-by-step guide beside the client-ID field, for Google and for
  // Microsoft -- the two providers this application signs in to with a
  // client id the *person* registers, per the plan's decision not to ship a
  // built-in one. Plain prose and links rather than a wizard: registering an
  // OAuth client is done in the provider's own console, which this
  // application cannot drive for them, so the honest thing is to say exactly
  // where to click and then get out of the way.
  //
  // Its own file, and not inlined into the add-account sheet, because the
  // sheet already has a provider picker, a client-id field and a permission
  // grid competing for room, and two long walls of prose would bury all of
  // it. Collapsed by default for the same reason -- `<details>` needs no
  // store of its own, and a guide nobody has opened yet costs nothing to
  // have on screen.

  import type { MailProvider } from '../lib/types'

  let {
    provider,
    calendar = false,
  }: {
    provider: Extract<MailProvider, 'google' | 'microsoft'>
    /** Whether to mention the calendar's own scope and API -- only true once
     *  the calendar checkbox in the sheet is ticked. */
    calendar?: boolean
  } = $props()
</script>

<details class="guide">
  <summary>How to get a client id for {provider === 'google' ? 'Google' : 'Microsoft'}</summary>

  {#if provider === 'google'}
    <ol>
      <li>
        Create a project in the
        <a href="https://console.cloud.google.com/projectcreate" target="_blank" rel="noreferrer"
          >Google Cloud console</a
        >, if you do not already have one to use.
      </li>
      <li>
        Enable the
        <a
          href="https://console.cloud.google.com/apis/library/gmail.googleapis.com"
          target="_blank"
          rel="noreferrer">Gmail API</a
        >{#if calendar}, and the
          <a
            href="https://console.cloud.google.com/apis/library/calendar-json.googleapis.com"
            target="_blank"
            rel="noreferrer">Calendar API</a
          > since calendar is ticked below{/if}.
      </li>
      <li>
        Configure the
        <a
          href="https://developers.google.com/workspace/guides/configure-oauth-consent"
          target="_blank"
          rel="noreferrer">OAuth consent screen</a
        >, and add the Gmail{calendar ? ' and Calendar' : ''} scopes this account needs to it.
      </li>
      <li>
        Create credentials for a <strong>Desktop app</strong> client -- not "Web application", which expects
        a server to receive the redirect. Copy its client id and client secret into the two fields below.
      </li>
      <li class="loud">
        Publish the app as <strong>"In production"</strong>, not "Testing". This is the step that
        matters most: a project left in Testing mode has Google expire its refresh token every seven
        days, so signing in would work once and then quietly stop.
      </li>
    </ol>
  {:else}
    <ol>
      <li>
        Register an app in
        <a href="https://entra.microsoft.com/" target="_blank" rel="noreferrer"
          >Microsoft Entra ID</a
        >.
      </li>
      <li>
        Set supported account types to
        <strong>"Accounts in any organizational directory and personal Microsoft accounts"</strong>,
        so both a work address and an outlook.com one can sign in through the same client.
      </li>
      <li>
        Add a redirect under the <strong>"Mobile and desktop applications"</strong> platform, set to
        <code>http://localhost</code>.
      </li>
      <li>
        Add the delegated permissions <code>IMAP.AccessAsUser.All</code>,
        <code>SMTP.Send</code>{calendar ? ', ' : ' and '}<code>offline_access</code>{#if calendar},
          and <code>Calendars.Read</code>{/if}.
      </li>
      <li>
        Copy the client id, and a client secret if Entra ID issued one for this registration, into
        the two fields below.
      </li>
      <li>
        A work or school tenant's administrator may need to approve this app, or turn on IMAP and
        SMTP AUTH for the mailbox, before sign-in will work there.
      </li>
    </ol>
  {/if}
</details>

<style>
  .guide {
    padding: var(--sp-3);
    border: 1px solid var(--border);
    border-radius: var(--radius);
    background: var(--bg-sunken);
  }

  summary {
    font-size: var(--text-sm);
    font-weight: 600;
    color: var(--fg-muted);
    cursor: pointer;
  }
  summary:hover {
    color: var(--fg);
  }

  ol {
    display: flex;
    flex-direction: column;
    gap: var(--sp-2);
    margin: var(--sp-3) 0 0;
    padding-left: 1.25em;
    font-size: var(--text-sm);
    line-height: var(--leading-normal);
    color: var(--fg-subtle);
  }

  li::marker {
    color: var(--fg-faint);
  }

  /* The one step that matters most -- see the module doc. Louder than the
     rest of the list without a second colour competing with the danger
     palette, which this is a caution rather than an error. */
  .loud {
    padding: var(--sp-2) var(--sp-3);
    border-radius: var(--radius-sm);
    background: var(--bg-raised);
    color: var(--fg);
    box-shadow: 0 0 0 1px var(--border);
  }

  a {
    color: var(--accent);
  }

  code {
    font-family: var(--font-mono);
    font-size: 0.92em;
    color: var(--fg-muted);
  }
</style>
