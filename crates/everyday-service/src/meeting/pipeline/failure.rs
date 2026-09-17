use std::sync::Arc;

use everyday_core::meeting::Stage;
use everyday_core::{RecordingId, Vault};
use jiff::Timestamp;

use crate::error::{CommandError, CommandResult};
use crate::events::{Change, Kind, Op};
use crate::service::blocking;

/// Marks `id` [`Stage::Failed`] and tells anyone watching the recordings
/// list -- without this, a call that fails partway through the pipeline
/// (unlike one that reaches [`Stage::Done`], which raises its own
/// `Kind::Recording` change in [`do_summarise_and_write`]) would sit at its
/// last-seen stage in every other window until something else happened to
/// reload it.
///
/// Refuses to touch a row already at [`Stage::Done`] -- defence in depth
/// alongside [`do_summarise_and_write`]'s own care not to call this at all
/// once its note is committed: a stage that starts only after `Done` is
/// reached has nothing left to fail *of*, and overwriting that row would
/// orphan the note it already wrote (a retry recomputes from `partial`,
/// which `Done` has already cleared -- see `spool::retry`'s own guard
/// against exactly that). No change event is raised for a no-op refusal:
/// nothing about the row actually changed for a window to reload.
pub(crate) async fn fail(
    vault: &Arc<Vault>,
    id: RecordingId,
    at: Stage,
    e: &CommandError,
    events: &dyn crate::events::EventSink,
) -> CommandResult<()> {
    let vault = vault.clone();
    let reason = crate::llm::friendly(&e.message);
    let recording = blocking(move || {
        crate::meeting::spool::mutate_recording(&vault, id, move |recording| {
            if matches!(recording.stage, Stage::Done) {
                return Ok(());
            }
            recording.stage = Stage::Failed { reason, at: Box::new(at) };
            recording.updated_at = Timestamp::now();
            Ok(())
        })
    })
    .await?;
    if matches!(recording.stage, Stage::Failed { .. }) {
        let mut change = Change::new(Kind::Recording, Op::Updated);
        change.id = Some(id.to_string());
        events.changed(change);
    } else {
        tracing::warn!(
            recording = %id,
            "meeting pipeline: refused to mark a `Done` recording as failed"
        );
    }
    Ok(())
}
