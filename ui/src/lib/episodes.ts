// Playing an episode is asked for from three places -- the round control, the
// pill in the open story, and a row's right-click menu -- and is the same
// act in each, so it is written once, here, where all three can reach it.
// A module of its own rather than `EpisodeActions.svelte`'s module script:
// the typed lint cannot see what a `.svelte` file exports.
import { mediaUrl } from './api'
import { feed } from './feed.svelte'
import { sourceLabel } from './feed'
import { player } from './player.svelte'
import { playRequestForEpisode } from './podcast'
import { upnext } from './upnext.svelte'
import type { ContentRef, Story } from './types'

/**
 * Play a story's downloaded episode -- or pause it, when it is the one that
 * is loaded: pressing Play on what is already playing is not a second start.
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
