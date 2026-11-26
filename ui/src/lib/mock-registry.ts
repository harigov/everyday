// Which mock answers which command, once.
//
// `mockInvoke` in `mock.ts` is one switch over every command the interface
// can send, and the bigger domains -- backups, connections, money, screen
// time, health -- keep their state and their answers in a file of their own
// with a switch of their own. Each of those used to be listed twice: the
// command's name in the file that answers it, and again in `mock.ts` as a
// bare label that did nothing but check the vault was open and hand the call
// on. Each domain now says here which commands are its own, and which of them
// want the vault open; `mockInvoke` asks this list before it reaches its
// switch. (Files and Documents keep their own gate in `mockInvoke`: the files
// mock has one answer that the Feed's mock gets first.)
//
// A command's name goes in its domain's file, next to the switch that answers
// it. A command that wants the vault open is in the domain's `*_COMMANDS`; one
// that is the shell's own -- this computer's, answered locked or not -- is in
// its `*_SHELL_COMMANDS`.

import { MOCK_BACKUP_COMMANDS, MOCK_BACKUP_SHELL_COMMANDS, mockBackup } from './mock-backup'
import { MOCK_CONNECTION_COMMANDS, mockConnections } from './mock-connections'
import { MOCK_HEALTH_COMMANDS, MOCK_HEALTH_SHELL_COMMANDS, mockHealth } from './mock-health'
import { MOCK_MONEY_COMMANDS, mockMoney } from './mock-money'
import {
  MOCK_SCREENTIME_COMMANDS,
  MOCK_SCREENTIME_SHELL_COMMANDS,
  mockScreenTime,
} from './mock-screentime'

/** One file's worth of the mock: the commands it answers, and how. */
export interface MockDomain {
  /** Whether this domain answers `cmd`. */
  handles(cmd: string): boolean
  /** Whether `cmd` wants the vault open. Asked only of a command `handles` accepts. */
  needsVault(cmd: string): boolean
  /** The answer, as the real command would give it. */
  invoke(cmd: string, args: Record<string, unknown>): unknown
}

function domain(
  vault: ReadonlySet<string>,
  shell: ReadonlySet<string>,
  invoke: (cmd: string, args: Record<string, unknown>) => unknown,
): MockDomain {
  return {
    handles: (cmd) => vault.has(cmd) || shell.has(cmd),
    needsVault: (cmd) => vault.has(cmd),
    invoke,
  }
}

const NONE: ReadonlySet<string> = new Set()

/** Every domain with a file of its own, in the order they are asked. */
export const MOCK_DOMAINS: readonly MockDomain[] = [
  domain(MOCK_BACKUP_COMMANDS, MOCK_BACKUP_SHELL_COMMANDS, mockBackup),
  domain(MOCK_CONNECTION_COMMANDS, NONE, mockConnections),
  domain(MOCK_MONEY_COMMANDS, NONE, mockMoney),
  domain(MOCK_SCREENTIME_COMMANDS, MOCK_SCREENTIME_SHELL_COMMANDS, mockScreenTime),
  domain(MOCK_HEALTH_COMMANDS, MOCK_HEALTH_SHELL_COMMANDS, mockHealth),
]

/** The domain that answers `cmd`, if one does. */
export function mockDomainFor(cmd: string): MockDomain | undefined {
  return MOCK_DOMAINS.find((d) => d.handles(cmd))
}
