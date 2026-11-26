/**
 * Which component draws each Settings page.
 *
 * Most pages are a component that takes no props and fills the page, so the
 * choice is a table rather than a chain of `{#if tab === …}` in
 * `SettingsView`. Left out of it, and drawn there: the assistant's four
 * standing panes (one component, told which by a `pane` prop), General (its
 * markup is `SettingsView`'s own, tied to its state), and the Vault page that
 * is every tab not named. Ids are unique, so the order is for reading only.
 */
import type { Component } from 'svelte'
import type { SettingsTab } from './panels.svelte'
import AccountsPanel from '../components/AccountsPanel.svelte'
import AgentPanel from '../components/AgentPanel.svelte'
import AiFeaturesPanel from '../components/AiFeaturesPanel.svelte'
import BackupPanel from '../components/BackupPanel.svelte'
import BriefSettingsPanel from '../components/BriefSettingsPanel.svelte'
import ChatLinksPanel from '../components/ChatLinksPanel.svelte'
import ConnectionsPanel from '../components/ConnectionsPanel.svelte'
import DataPanel from '../components/DataPanel.svelte'
import FeedSettingsPanel from '../components/FeedSettingsPanel.svelte'
import FilesSettingsPanel from '../components/FilesSettingsPanel.svelte'
import HealthSettingsPanel from '../components/HealthSettingsPanel.svelte'
import KeepInTouchPanel from '../components/KeepInTouchPanel.svelte'
import MailSortingPanel from '../components/MailSortingPanel.svelte'
import MailWritingPanel from '../components/MailWritingPanel.svelte'
import MeetingsPanel from '../components/MeetingsPanel.svelte'
import ModelsPanel from '../components/ModelsPanel.svelte'
import MoneySettingsPanel from '../components/MoneySettingsPanel.svelte'
import ProfilePanel from '../components/ProfilePanel.svelte'
import ScreenTimeSettingsPanel from '../components/ScreenTimeSettingsPanel.svelte'
import SkillsPanel from '../components/SkillsPanel.svelte'
import TemplatesPanel from '../components/TemplatesPanel.svelte'

export const SETTINGS_PAGES: { id: SettingsTab; component: Component }[] = [
  { id: 'assistant', component: AgentPanel },
  { id: 'models', component: ModelsPanel },
  { id: 'ai', component: AiFeaturesPanel },
  { id: 'briefs', component: BriefSettingsPanel },
  { id: 'skills', component: SkillsPanel },
  { id: 'chat', component: ChatLinksPanel },
  { id: 'money', component: MoneySettingsPanel },
  { id: 'screentime', component: ScreenTimeSettingsPanel },
  { id: 'files', component: FilesSettingsPanel },
  { id: 'feed', component: FeedSettingsPanel },
  { id: 'health', component: HealthSettingsPanel },
  { id: 'meetings', component: MeetingsPanel },
  { id: 'templates', component: TemplatesPanel },
  { id: 'keepintouch', component: KeepInTouchPanel },
  { id: 'profile', component: ProfilePanel },
  { id: 'accounts', component: AccountsPanel },
  { id: 'sorting', component: MailSortingPanel },
  { id: 'writing', component: MailWritingPanel },
  { id: 'connections', component: ConnectionsPanel },
  { id: 'backup', component: BackupPanel },
  { id: 'data', component: DataPanel },
]

/** The component for a tab drawn from the table, or `undefined` for one `SettingsView` draws itself. */
export function settingsPage(tab: SettingsTab): Component | undefined {
  return SETTINGS_PAGES.find((p) => p.id === tab)?.component
}
