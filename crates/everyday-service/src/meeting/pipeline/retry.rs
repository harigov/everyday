use std::time::Duration;

use crate::error::{CommandResult, codes};

/// How many attempts [`retry_transient`] gives a transient error before
/// giving up and letting it through to `fail` -- see [`is_transient`].
/// Bounded on purpose: an endpoint whose network trouble never clears is
/// still worth telling a person about eventually, rather than retrying
/// forever with nothing for them to see or act on.
pub(crate) const TRANSIENT_RETRIES: u32 = 5;

/// The delay before [`retry_transient`]'s *second* attempt (the first
/// retry); each one after that doubles it, capped by
/// [`TRANSIENT_RETRY_CAP`]. Chosen, with [`TRANSIENT_RETRIES`], so the
/// worst case -- every attempt transient, every delay maxed out -- spans
/// roughly ten minutes: long enough to outlast a network blip or a
/// provider's bad minute, short enough that a call's note is not left
/// waiting for the length of the call itself.
const TRANSIENT_RETRY_BASE: Duration = Duration::from_secs(30);

/// The largest gap [`retry_transient`] leaves between attempts.
const TRANSIENT_RETRY_CAP: Duration = Duration::from_secs(4 * 60);

/// Whether `code` names a failure worth retrying -- a network hiccup, a
/// timeout, a rate limit, or a provider having a bad moment -- as opposed to
/// one retrying can never fix: a refused key, a model or endpoint that does
/// not exist, a chunk too large to send, or an assistant that is not
/// configured at all. See `crate::error::codes`, and each `Transcriber`'s
/// own `status_error` (`transcribe/openai.rs`, `transcribe/gemini.rs`) for
/// what maps to which.
fn is_transient(code: &str) -> bool {
    matches!(code, codes::NETWORK | codes::TIMED_OUT | codes::RATE_LIMITED | codes::PROVIDER)
}

/// Run one stage's own operation -- `do_transcribe`, `do_identify`,
/// `do_summarise_and_write` -- retrying it in place while its error is
/// [`is_transient`], up to [`TRANSIENT_RETRIES`] times with growing backoff.
/// A permanent error is returned on the very first attempt: see
/// [`is_transient`] for why retrying one is never worth doing.
///
/// Safe to retry a whole stage rather than only the one request that
/// failed: every stage checkpoints as it goes (`transcribe_stage` saves
/// after each chunk; `do_identify` and `do_summarise_and_write` are cheap,
/// local recomputation until their own final network call -- see
/// `do_identify`'s own doc on why nothing is persisted between identifying
/// and summarising), so re-running one from the top never re-pays for
/// audio already transcribed, only for whatever a network hiccup actually
/// interrupted.
pub(crate) async fn retry_transient<F, Fut>(op: F) -> CommandResult<()>
where
    F: Fn() -> Fut,
    Fut: std::future::Future<Output = CommandResult<()>>,
{
    retry_transient_from(op, TRANSIENT_RETRY_BASE).await
}

/// [`retry_transient`], with the starting delay broken out so a test can
/// shrink it without a real recording ever needing to -- the same seam
/// `capture.rs`'s `finish_with_retry_from` uses for the same reason.
pub(crate) async fn retry_transient_from<F, Fut>(op: F, mut delay: Duration) -> CommandResult<()>
where
    F: Fn() -> Fut,
    Fut: std::future::Future<Output = CommandResult<()>>,
{
    let mut last_err = None;
    for attempt in 0..TRANSIENT_RETRIES {
        if attempt > 0 {
            tokio::time::sleep(delay).await;
            delay = (delay * 2).min(TRANSIENT_RETRY_CAP);
        }
        match op().await {
            Ok(()) => return Ok(()),
            Err(e) if is_transient(&e.code) => last_err = Some(e),
            Err(e) => return Err(e),
        }
    }
    Err(last_err.expect("looped at least once"))
}
