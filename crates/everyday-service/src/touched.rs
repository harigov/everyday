//! One command's worth of [`everyday_core::vault::touched`] records, carried
//! from wherever a vault write actually runs back to whoever emits the
//! [`crate::events::Change`] for it -- `docs/plans/architecture-refactor.md`,
//! Phase 7.
//!
//! # Why a task-local, given `spawn_blocking`
//!
//! [`crate::service::blocking`] is "every command that touches storage goes
//! through here", on its own doc, and it dispatches onto
//! `tokio::task::spawn_blocking`'s pool -- a different OS thread on every
//! call, never the one polling the `async fn` that awaited it. A single
//! command can call it more than once (`Command::invoke` still has to see
//! everything from all of them), so the accumulator has to survive between
//! those calls without caring which thread carried each one.
//!
//! [`scope`] opens a `tokio::task_local!` around the whole command, which
//! -- unlike a thread-local -- travels with the *task*, not the thread: it
//! is still reachable after an `.await` resumes on a different worker.
//! [`merge`] is what [`crate::service::blocking`] calls with what
//! `everyday_core::vault::touched::collect` gathered on the blocking-pool
//! thread, and it runs back on the async task's own side of that `.await`,
//! which is the one place this module ever touches the task-local directly.
//! Nothing on the blocking-pool thread ever sees `TOUCHED` -- see
//! `everyday_core::vault::touched`'s own doc for why a thread-local *there*
//! would already be the wrong tool, `spawn_blocking` or not.
use std::cell::RefCell;
use std::future::Future;

use everyday_core::record::RecordKind;

tokio::task_local! {
    static TOUCHED: RefCell<Vec<(RecordKind, String)>>;
}

/// Run `f`, collecting every [`merge`] call made anywhere inside it --
/// directly, or through a `blocking` call nested arbitrarily deep -- and
/// hand back both `f`'s own output and what was touched, in the order it
/// arrived.
///
/// `Command::invoke` opens one of these around a single command; a
/// background writer (a scheduler tick, a meeting pipeline stage) opens one
/// around its own unit of work the same way.
pub async fn scope<Fut, T>(f: Fut) -> (T, Vec<(RecordKind, String)>)
where
    Fut: Future<Output = T>,
{
    TOUCHED
        .scope(RefCell::new(Vec::new()), async {
            let out = f.await;
            let touched = TOUCHED.with(|cell| cell.borrow().clone());
            (out, touched)
        })
        .await
}

/// Add what a single `blocking` call collected to the current [`scope`], if
/// there is one.
///
/// A no-op outside a `scope` -- background work this phase does not yet wrap
/// (see the module doc on why only `blocking`'s own callers are covered so
/// far) calls `blocking` exactly as before and nobody outside it is any the
/// wiser.
pub(crate) fn merge(touched: Vec<(RecordKind, String)>) {
    if touched.is_empty() {
        return;
    }
    let _ = TOUCHED.try_with(|cell| cell.borrow_mut().extend(touched));
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn merge_outside_a_scope_is_a_no_op() {
        // Nothing to assert beyond "this does not panic" -- there is no
        // scope for it to have written into.
        merge(vec![(RecordKind::Note, "n1".into())]);
    }

    #[tokio::test]
    async fn scope_gathers_every_merge_inside_it() {
        let (out, touched) = scope(async {
            merge(vec![(RecordKind::Journal, "j1".into())]);
            merge(vec![(RecordKind::Entry, "e1".into()), (RecordKind::Entry, "e2".into())]);
            "value"
        })
        .await;
        assert_eq!(out, "value");
        assert_eq!(
            touched,
            vec![
                (RecordKind::Journal, "j1".to_string()),
                (RecordKind::Entry, "e1".to_string()),
                (RecordKind::Entry, "e2".to_string()),
            ]
        );
    }

    #[tokio::test]
    async fn a_scope_that_merges_nothing_collects_an_empty_list() {
        let (out, touched) = scope(async { 7 }).await;
        assert_eq!(out, 7);
        assert!(touched.is_empty());
    }

    #[tokio::test]
    async fn merges_from_a_finished_scope_do_not_leak_into_the_next() {
        let _ = scope(async { merge(vec![(RecordKind::Note, "n1".into())]) }).await;
        let (_, touched) = scope(async {}).await;
        assert!(touched.is_empty());
    }
}
