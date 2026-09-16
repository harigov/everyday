//! TODO: filled in by the agent that owns this module. See docs/plans/meeting-notes.md.
//!
//! Placeholder signatures only, so `meeting::pipeline`'s `SpoolSource`
//! adapter (see that module's own doc on the seam) has something to compile
//! against ahead of the spool engineer's real implementation landing.
//! Neither function here is called by anything in this crate's own tests --
//! `meeting::pipeline`'s tests all drive an in-memory `AudioSource` fake
//! instead -- so returning an error rather than `todo!()` is a deliberate
//! choice: if this *is* somehow reached before the real spool lands, a
//! supervised pipeline task fails cleanly (`Stage::Failed`) rather than
//! panicking the task.
//!
//! Replace this whole file rather than reconciling it with whatever lands
//! here for real; nothing in it is meant to survive that merge.

use crate::error::{CommandError, CommandResult, codes};
use everyday_core::meeting::Track;
use everyday_core::{RecordingId, Vault};

pub fn read_chunk(
    _vault: &Vault,
    _id: RecordingId,
    _track: Track,
    _seq: u32,
) -> CommandResult<Vec<i16>> {
    Err(CommandError::new(codes::UNSUPPORTED, "the spool has not landed yet"))
}

pub fn remove_audio(_vault: &Vault, _id: RecordingId) -> CommandResult<()> {
    Err(CommandError::new(codes::UNSUPPORTED, "the spool has not landed yet"))
}
