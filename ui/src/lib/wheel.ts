/**
 * The wheel, listened for only while hearing it would change something.
 *
 * WebKitGTK scrolls a pane on a thread of its own only while nothing in the
 * window listens for `wheel`. One listener anywhere in it -- even a passive
 * one, which promises never to cancel the scroll -- and every turn of the
 * wheel waits for this page's script to be free before anything on screen
 * moves. The window listened for the wheel all the time, twice, to know
 * somebody was there (`app.touch`, and `notify`'s "is anybody watching"),
 * and so every pane in every app scrolled only as fast as the main thread
 * came free: a long note crawled behind its own word count, and a moment's
 * work elsewhere froze the list under the pointer outright.
 *
 * Both of those only need a wheel when their clock is getting near its
 * limit: a turn of the wheel a minute after a click changes nothing that the
 * click had not already. So each says when a wheel would matter, and only
 * then is the listener on the window -- taken off again at the first turn it
 * hears, which has started its clock over. Reading with the wheel alone
 * still holds the lock off, since every stretch of half a timeout with a turn
 * in it starts the clock again; what it costs is that one turn in each such
 * stretch waits on the script, rather than every turn.
 *
 * Not `scroll`, which would need no listener on the wheel at all: a chat log
 * following a reply as it streams in, or a list put back where it was after a
 * refresh, scrolls with nobody's hand on it, and a wheel that moves nothing
 * -- a pane at its end, a board that does not scroll -- is still somebody
 * there.
 */
export function hearWheelWhen(
  wanted: () => boolean,
  heard: () => void,
  everyMs = 5_000,
): () => void {
  let listening = false
  const onWheel = () => {
    heard()
    update()
  }
  const update = () => {
    const want = wanted()
    if (want === listening) return
    listening = want
    if (want) window.addEventListener('wheel', onWheel, { capture: true, passive: true })
    else window.removeEventListener('wheel', onWheel, { capture: true })
  }
  update()
  const timer = setInterval(update, everyMs)
  // Under Node -- a store test that loads a module using this -- the timer
  // must not be what keeps the process from exiting.
  ;(timer as unknown as { unref?: () => void }).unref?.()
  return () => {
    clearInterval(timer)
    if (listening) window.removeEventListener('wheel', onWheel, { capture: true })
    listening = false
  }
}
