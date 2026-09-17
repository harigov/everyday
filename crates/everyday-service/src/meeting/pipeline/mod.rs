//! The pipeline: one supervised task per recording, from spooled audio to a
//! written note. See `docs/plans/meeting-notes.md`, "The shape of it", "Who
//! said what", "Echo" and "Summarising".
//!
//! # Shape
//!
//! [`enqueue`] and [`chunk_closed`] are the seam the spool calls through --
//! see their own docs. Both, ultimately, ask [`everyday_service::supervisor`]
//! for one task keyed `"meeting:{id}"`; the supervisor's own restart-with-
//! backoff and stop-on-lock machinery (see that module's doc) is what gives
//! this "stops when the vault locks, resumes on unlock" for free, the same
//! way it already does for one mail account's sync task. A transient error
//! talking to a transcriber or the assistant -- a network hiccup, a
//! timeout, a rate limit, a provider having a bad moment, see
//! [`retry::is_transient`] -- is retried in place, with backoff, before this
//! module gives up on it (see [`retry::retry_transient`]); a permanent one (a
//! refused key, a model the endpoint does not know, a chunk too large to
//! send) is never retried at all. Either way, once a stage's own retries
//! are exhausted the recording is marked [`Stage::Failed`] and `Err` is
//! returned so the supervisor stops asking for it; reaching [`Stage::Done`]
//! or [`Stage::Failed`] is reported as [`supervisor::Outcome::Done`],
//! because there is nothing further for that key to do until a retry or a
//! discard re-`enqueue`s it.
//!
//! # The stages
//!
//! [`Stage::Transcribing`] → [`Stage::Identifying`] → [`Stage::Summarising`]
//! → [`Stage::Done`], driven by [`run_recording`]. Each stage saves the
//! recording as it finishes its own work (not before, and not batched with
//! the next), so a crash or a lock mid-pipeline resumes exactly where it
//! left off rather than repeating a stage that already succeeded -- the same
//! argument `ChunkMeta::transcribed` makes at the finer grain of one chunk.
//!
//! # Two seams for testability
//!
//! [`adapters::AudioSource`] and [`adapters::Summariser`] are the two places
//! this module would otherwise reach for a socket or a decrypted file it
//! cannot get in a unit test. Both are traits with a production
//! implementation and a fake one; see their own docs.
//! [`transcribe::Transcriber`] is already exactly this shape (built
//! elsewhere in this crate), so it needed no seam of its own -- tests hand
//! `run_recording` a fake one directly.
//!
//! # One seam, two implementations
//!
//! [`adapters::SpoolSource`] is the thin, production [`adapters::AudioSource`]:
//! `read_chunk` and `remove_audio` call straight through to
//! `meeting::spool`'s own functions of the same name. It is wired into
//! [`enqueue`] and [`chunk_closed`], but nothing in this crate's own tests
//! exercises it directly -- they all hand `run_recording` `FakeAudio`
//! instead, so a stage's logic is tested without a real spool directory on
//! disk.
//!
//! # Saving a recording mid-pipeline
//!
//! Every save this module makes of a recording's row goes through
//! `meeting::spool::mutate_recording`, not a bare `vault.recording` /
//! `vault.save_recording` pair -- the same per-`RecordingId` lock `spool`'s
//! own `append` and `finish` hold for theirs, so a chunk landing while this
//! module is mid-stage can never be dropped by, or drop, a save this module
//! makes at the same moment. [`stages::transcribe_stage`] re-reads the chunk
//! list under that lock on every pass for exactly this reason -- see its own
//! doc.
//!
//! # `Stage::Done` is never overwritten
//!
//! [`drivers::do_summarise_and_write`] commits the note, the transcript and
//! the recording's own `Stage::Done` row together, then clears the spool as
//! a last, separate step. If that last step fails, the note already exists
//! -- so the failure is logged and swallowed rather than returned, which
//! would otherwise reach [`failure::fail`] and overwrite the `Done` row with
//! `Failed { at: Summarising }`, orphaning the note a retry would then
//! duplicate. [`failure::fail`] itself refuses to touch a `Done` row
//! regardless, as a second line of defence. See
//! [`drivers::do_summarise_and_write`]'s own doc for the leftover spool
//! directory this leaves, and `spool::expire_failed_tick`'s
//! `sweep_orphaned_spool` for what clears it.

mod adapters;
mod drivers;
mod enqueue;
mod failure;
mod retry;
mod stages;
mod transcriber;
mod turns;

#[cfg(test)]
mod test_hooks;
#[cfg(test)]
mod tests;

pub use adapters::{AssistantSummariser, AudioSource, SpoolSource, Summariser};
pub use drivers::run_recording;
pub use enqueue::{chunk_closed, enqueue};
pub(crate) use stages::summarise_stage;
pub use transcriber::{OPENAI_BASE_URL, build_transcriber};
