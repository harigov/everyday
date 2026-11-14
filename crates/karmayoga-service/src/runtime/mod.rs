//! Per-area session state, grouped the way phase 9.5 of
//! `docs/plans/architecture-refactor.md` asks: fields [`Service`](crate::service::Service)
//! held individually before this, one lock each, now owned by a small
//! struct per area instead -- [`mail::MailRuntime`], [`meeting::MeetingRuntime`]
//! and [`routine::RoutineRuntime`].
//!
//! This is a regrouping, not a redesign. Every field keeps the exact lock
//! type and granularity it had on `Service` -- no two fields that were
//! locked independently before share a lock now -- and `Service` keeps the
//! same public methods it always had, each now a thin delegate to the
//! runtime that owns the field. `tests/runtime_lifecycle.rs` pins the
//! lock/unlock/close behaviour this move must not change, including the gap
//! between what `close()` clears and what `locked()` does, and the fields
//! neither ever touches.

pub(crate) mod mail;
pub(crate) mod meeting;
pub(crate) mod routine;
