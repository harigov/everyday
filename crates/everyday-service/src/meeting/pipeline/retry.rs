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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::error::CommandError;
    use std::sync::atomic::{AtomicU32, Ordering};
    use std::sync::{Arc, Mutex};

    /// Phase 9.3's pinning step: [`is_transient`]'s verdict on a
    /// representative set of codes, fixed before anything about the retry
    /// mechanism moves. See `outbox::is_transient_local_failure`'s own
    /// pinning test for why the two must not be merged: they disagree on
    /// this exact set.
    #[test]
    fn is_transient_verdicts_are_pinned() {
        for code in [codes::NETWORK, codes::TIMED_OUT, codes::RATE_LIMITED, codes::PROVIDER] {
            assert!(is_transient(code), "{code} must be transient");
        }
        for code in [
            codes::FORBIDDEN,
            codes::INVALID,
            codes::NOT_FOUND,
            codes::LOCKED,
            codes::IO,
            codes::BACKEND,
            codes::AGENT,
            codes::TOO_LARGE,
        ] {
            assert!(!is_transient(code), "{code} must not be transient");
        }
    }

    /// Phase 9.3's pinning step, part two: the exact delay sequence
    /// [`retry_transient_from`] sleeps between attempts, and its exact
    /// give-up point -- both fixed here, before the manual doubling this
    /// function does today is replaced by a shared `RetryPolicy`.
    #[tokio::test(start_paused = true)]
    async fn retry_transient_from_sleeps_the_pinned_doubling_sequence_then_gives_up() {
        let calls = Arc::new(AtomicU32::new(0));
        let timestamps = Arc::new(Mutex::new(Vec::new()));
        let calls2 = calls.clone();
        let timestamps2 = timestamps.clone();
        let op = move || {
            let calls = calls2.clone();
            let timestamps = timestamps2.clone();
            async move {
                calls.fetch_add(1, Ordering::SeqCst);
                timestamps.lock().unwrap().push(tokio::time::Instant::now());
                Err::<(), _>(CommandError::new(codes::NETWORK, "down"))
            }
        };

        let result = retry_transient_from(op, TRANSIENT_RETRY_BASE).await;

        assert!(result.is_err(), "a transient error that never clears must still fail eventually");
        assert_eq!(
            calls.load(Ordering::SeqCst),
            TRANSIENT_RETRIES,
            "bounded: exactly TRANSIENT_RETRIES attempts, never more"
        );
        let ts = timestamps.lock().unwrap();
        let gaps: Vec<Duration> = ts.windows(2).map(|w| w[1].duration_since(w[0])).collect();
        assert_eq!(
            gaps,
            vec![
                Duration::from_secs(30),
                Duration::from_secs(60),
                Duration::from_secs(120),
                Duration::from_secs(240), // capped by TRANSIENT_RETRY_CAP
            ],
            "the doubling-then-capped sequence between each of the five attempts"
        );
    }

    #[tokio::test(start_paused = true)]
    async fn retry_transient_from_stops_the_moment_the_error_clears() {
        let calls = Arc::new(AtomicU32::new(0));
        let calls2 = calls.clone();
        let op = move || {
            let calls = calls2.clone();
            async move {
                let n = calls.fetch_add(1, Ordering::SeqCst);
                if n < 2 { Err(CommandError::new(codes::NETWORK, "down")) } else { Ok(()) }
            }
        };

        let result = retry_transient_from(op, TRANSIENT_RETRY_BASE).await;

        assert!(result.is_ok());
        assert_eq!(
            calls.load(Ordering::SeqCst),
            3,
            "two failures, then a third attempt that succeeded"
        );
    }

    #[tokio::test]
    async fn retry_transient_from_never_retries_a_permanent_error() {
        let calls = Arc::new(AtomicU32::new(0));
        let calls2 = calls.clone();
        let op = move || {
            let calls = calls2.clone();
            async move {
                calls.fetch_add(1, Ordering::SeqCst);
                Err::<(), _>(CommandError::new(codes::FORBIDDEN, "refused"))
            }
        };

        let result = retry_transient_from(op, TRANSIENT_RETRY_BASE).await;

        let err = result.unwrap_err();
        assert_eq!(err.code, codes::FORBIDDEN);
        assert_eq!(
            calls.load(Ordering::SeqCst),
            1,
            "a permanent error fails on the very first attempt"
        );
    }
}
