// Playing an episode is asked for from three places -- the round control, the
// pill in the open story, and a row's right-click menu -- and is the same
// act in each, so it is written once, here, where all three can reach it.
// Opening its transcript is the same: the story's pane, a row's menu and the
// Files inspector all lead to the reader.
// A module of its own rather than `EpisodeActions.svelte`'s module script:
// the typed lint cannot see what a `.svelte` file exports.
import { mediaUrl } from './api'
import { feed } from './feed.svelte'
import { sourceLabel } from './feed'
import { files } from './files.svelte'
import { player } from './player.svelte'
import { playRequestForEpisode } from './podcast'
import { upnext } from './upnext.svelte'
import type { ContentRef, FileId, Story } from './types'

/**
 * Play a story's downloaded episode -- or, for an article, the audio it was
 * saved as -- or pause it, when it is the one that is loaded: pressing Play on
 * what is already playing is not a second start.
 *
 * The show has its say: its speed, and its intro skipped when the episode
 * has not been begun. Played from Up Next when it is in it, so that what
 * follows it in the queue plays after it, which is what somebody who picks
 * an episode out of their queue means.
 */
export function playEpisode(story: Story): void {
  const ref: ContentRef = { type: 'story', id: story.id }
  if (player.isCurrent(ref)) {
    player.toggle()
    return
  }
  const source = feed.sourceOf(story)?.source
  const progress = upnext.progressOf(ref)
  // Something to resume from: begun, and not finished.
  const started = !!progress && !progress.finished && (progress.positionMs ?? 0) > 0
  const request = playRequestForEpisode(
    story,
    source ? sourceLabel(source) : '',
    source?.podcast,
    source?.icon ? mediaUrl(source.icon) : null,
    started,
  )
  if (request) void player.play({ ...request, fromQueue: upnext.isQueued(ref) })
}

/**
 * Open a transcript in the reader, beside the assistant, where it can be read,
 * searched and asked about like any other document. A transcript is an
 * ordinary Markdown file in Files, so this is what opening one anywhere does.
 *
 * The file is looked up first: somebody may have put it in the trash since the
 * story linked it, and the reader would only say it could not open it. Answers
 * whether it was opened; `false` means it is gone, and whoever asked should
 * offer to write it down again -- by then the lookup has told every button that
 * draws the link, so they already do. `before` runs once the file is known to
 * be there and before the reader opens, for a viewer that has to come down.
 */
export async function openTranscript(
  file: FileId | null | undefined,
  before?: () => void,
): Promise<boolean> {
  if (!file) return false
  await upnext.checkLinks([file])
  if (upnext.linkedFile(file) === null) return false
  before?.()
  await files.read(file)
  return true
}
