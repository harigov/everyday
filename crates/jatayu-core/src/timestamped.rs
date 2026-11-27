//! An opt-in trait for a record whose `updated_at` a caller stamps by hand.
//!
//! Unlike [`Completable`](crate::Completable), `touch()` has nothing else to
//! coordinate — no status, no `completed_at` to keep honest alongside it —
//! so there is no accessor to share a default body through. Each
//! implementation states the one-line assignment itself; the trait exists
//! so every call site reads `x.touch()` instead of repeating the field
//! access, not to save typing at the impl site.
//!
//! Nothing calls `touch()` for you. Callers still choose *when* to stamp:
//! the refactor plan's "one rule" is that a phase may not change when
//! `updated_at` is stamped, and an automatic stamp on every write would turn
//! a second autosave into a false conflict (see "Out of scope" in
//! `docs/plans/architecture-refactor.md`).

/// A record with a plain `updated_at: Timestamp` field, stamped by whoever
/// writes it.
pub trait Timestamped {
    /// Stamp `updated_at` to now.
    fn touch(&mut self);
}
