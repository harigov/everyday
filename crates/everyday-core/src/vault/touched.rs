//! What a [`Vault`](super::Vault) write actually touched, collected for
//! `everyday-service`'s own use -- see `docs/plans/architecture-refactor.md`,
//! Phase 7.
//!
//! Every `save_*`/`delete_*` on [`Vault`](super::Vault) calls [`touch`] once
//! it knows the record it wrote landed, naming the kind and id the same way
//! [`crate::record::RecordDescriptor`] would. Nothing reads that unless
//! something is collecting -- which is what keeps this off the hot path the
//! overwhelming majority of calls take.
//!
//! # Why a thread-local, given the service dispatches every write through
//! `tokio::task::spawn_blocking`
//!
//! `everyday-service`'s `blocking` helper -- "every command that touches
//! storage goes through here", by its own doc -- runs the vault call on
//! whichever thread `spawn_blocking`'s own pool happens to hand it, a
//! different one on every call and never the thread that is polling the
//! `async fn` that awaited it. A thread-local set before that `.await` and
//! read after it would see two different threads and silently lose
//! everything written in between -- worse, it would sometimes *not*, on a
//! runtime that happened to reuse the same pool thread, so the bug would be
//! the kind that passes locally and fails under load.
//!
//! [`collect`] is built the other way around: it opens its scope around one
//! synchronous call -- `f` -- and closes it before returning, entirely on
//! whichever single thread is running that call. `everyday-service::blocking`
//! calls it *inside* its own `spawn_blocking` closure, so the scope never
//! crosses an `.await` at all, and carries the resulting `Vec` back across
//! the `.await` boundary the same safe way it already carries `f`'s own
//! result -- through the closure's return value, not through thread- or
//! task-local state. A command that calls `blocking` more than once collects
//! more than once, and its caller is what adds the two lists together; nesting
//! `collect` itself is not a shape this crate's callers produce.
use std::cell::RefCell;
use std::fmt::Display;

use crate::record::RecordKind;

thread_local! {
    /// `Some` for exactly the span of one [`collect`] call on this thread.
    /// `None` the rest of the time, which is what makes [`touch`] a cheap
    /// no-op outside a collecting scope: one thread-local access and a
    /// pattern match, no allocation.
    static SINK: RefCell<Option<Vec<(RecordKind, String)>>> = const { RefCell::new(None) };
}

/// Record that a write just landed for `kind`'s row `id`, if anyone is
/// currently collecting on this thread.
///
/// Called from inside [`Vault`](super::Vault)'s own `save_*`/`delete_*`
/// methods, after the write those methods asked the store to do has
/// actually succeeded -- never before, and never on an error path, so a
/// failed write touches nothing.
pub(super) fn touch(kind: RecordKind, id: impl Display) {
    SINK.with(|cell| {
        if let Some(sink) = cell.borrow_mut().as_mut() {
            sink.push((kind, id.to_string()));
        }
    });
}

/// Run `f`, collecting every [`touch`] call it makes -- directly, or through
/// whatever `Vault` method it calls into -- and hand back both `f`'s result
/// and what was touched, in the order it happened.
///
/// See the module doc for why this has to be scoped tightly around one
/// synchronous call rather than around the `.await` that waits for it.
///
/// Nests: an inner scope hands its own touches back to its caller *and*
/// replays them into the scope it interrupted, so an outer collector still
/// sees everything that happened while it was open. Without that, the
/// `collect` a caller wraps around one particular call -- `run_tool` around a
/// tool's dispatch -- would silently swallow those writes from the
/// `blocking` scope already collecting around it, and the outer sink would
/// answer "nothing was written" for a command that wrote.
pub fn collect<T>(f: impl FnOnce() -> T) -> (T, Vec<(RecordKind, String)>) {
    let previous = SINK.with(|cell| cell.replace(Some(Vec::new())));
    let out = f();
    let touched = SINK.with(|cell| cell.replace(previous)).unwrap_or_default();
    SINK.with(|cell| {
        if let Some(outer) = cell.borrow_mut().as_mut() {
            outer.extend(touched.iter().cloned());
        }
    });
    (out, touched)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn touch_outside_a_collecting_scope_is_a_no_op() {
        // Nothing to assert on besides "this does not panic and produces no
        // observable state" -- there is no sink to inspect, which is the
        // point.
        touch(RecordKind::Note, "not-collected");
    }

    #[test]
    fn collect_gathers_every_touch_in_order() {
        let (out, touched) = collect(|| {
            touch(RecordKind::Journal, "j1");
            touch(RecordKind::Entry, "e1");
            touch(RecordKind::Entry, "e2");
            42
        });
        assert_eq!(out, 42);
        assert_eq!(
            touched,
            vec![
                (RecordKind::Journal, "j1".to_string()),
                (RecordKind::Entry, "e1".to_string()),
                (RecordKind::Entry, "e2".to_string()),
            ]
        );
    }

    #[test]
    fn a_scope_that_touches_nothing_collects_an_empty_list() {
        let (out, touched) = collect(|| "value");
        assert_eq!(out, "value");
        assert!(touched.is_empty());
    }

    #[test]
    fn touches_from_a_previous_scope_do_not_leak_into_the_next() {
        let _ = collect(|| touch(RecordKind::Note, "n1"));
        let (_, touched) = collect(|| {});
        assert!(touched.is_empty());
    }

    #[test]
    fn an_inner_scope_reports_to_its_caller_and_to_the_scope_it_interrupted() {
        let (inner_seen, outer) = collect(|| {
            touch(RecordKind::Journal, "j1");
            let (_, inner) = collect(|| touch(RecordKind::Note, "n1"));
            inner
        });
        assert_eq!(inner_seen, vec![(RecordKind::Note, "n1".to_string())]);
        assert_eq!(
            outer,
            vec![(RecordKind::Journal, "j1".to_string()), (RecordKind::Note, "n1".to_string()),]
        );
    }
}
