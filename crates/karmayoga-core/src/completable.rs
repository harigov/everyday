//! An opt-in trait for a record that can be finished.
//!
//! [`Project::set_status`], [`Task::set_status`] and [`Goal::set_status`]
//! were the same six lines typed out three times: move to a new status,
//! stamp `completed_at` once on first reaching "done" and clear it
//! otherwise, and stamp `updated_at`. [`Completable::set_status`] is that
//! body, written once; each of the three implements the trait by naming its
//! own status type, its "done" value, and where its three fields live.
//!
//! [`crate::library::Item`] is *not* one of the three: its own
//! [`Item::set_status`](crate::library::Item::set_status) also moves
//! `started_on` and `finished_on` off a `today: Date` this trait's
//! signature has no room for, so it keeps its own hand-written method
//! rather than forcing a fourth shape through this one.
//!
//! [`Project::set_status`]: crate::task::Project::set_status
//! [`Task::set_status`]: crate::task::Task::set_status
//! [`Goal::set_status`]: crate::purpose::Goal::set_status

use jiff::Timestamp;

/// A record whose status can reach a single "done" value, stamping
/// `completed_at` and `updated_at` the same way every time.
pub trait Completable {
    /// The record's own status enum.
    type Status: PartialEq + Copy;

    /// The value of [`Status`](Completable::Status) that means finished.
    const DONE: Self::Status;

    fn status_mut(&mut self) -> &mut Self::Status;
    fn completed_at_mut(&mut self) -> &mut Option<Timestamp>;
    fn updated_at_mut(&mut self) -> &mut Timestamp;

    /// Move to `status`, keeping `completed_at` honest: set the first time
    /// [`DONE`](Completable::DONE) is reached, left alone if it is reached
    /// again without ever leaving, and cleared by any other status.
    fn set_status(&mut self, status: Self::Status) {
        let done = status == Self::DONE;
        *self.status_mut() = status;
        let previous = *self.completed_at_mut();
        *self.completed_at_mut() = if done { previous.or(Some(Timestamp::now())) } else { None };
        *self.updated_at_mut() = Timestamp::now();
    }
}
