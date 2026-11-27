use std::sync::Arc;

use jatayu_core::meeting::Stage;
use jatayu_core::{RecordingId, Vault};
use jiff::Timestamp;

use crate::error::{CommandError, CommandResult};
use crate::events::{Change, Kind, Op};
use crate::service::blocking;

/// Marks `id` [`Stage::Failed`] and tells anyone watching the recordings
/// list -- without this, a call that fails partway through the pipeline
/// (unlike one that reaches [`Stage::Done`], which raises its own
/// `Kind::Recording` change in [`summarise_and_write_from`]) would sit at its
/// last-seen stage in every other window until something else happened to
/// reload it.
///
/// Refuses to touch a row already at [`Stage::Done`] -- defence in depth
/// alongside [`summarise_and_write_from`]'s own care not to call this at all
/// once its note is committed: a stage that starts only after `Done` is
/// reached has nothing left to fail *of*, and overwriting that row would
/// orphan the note it already wrote (a retry recomputes from `partial`,
/// which `Done` has already cleared -- see `spool::retry`'s own guard
/// against exactly that). No change event is raised for a no-op refusal:
/// nothing about the row actually changed for a window to reload.
///
/// A recording that already has its draft note (see `draft`) has the failure
/// said in it too, in place of the line promising a summary -- unless the
/// person has been writing in the note.
pub(crate) async fn fail(
    vault: &Arc<Vault>,
    id: RecordingId,
    at: Stage,
    e: &CommandError,
    events: &dyn crate::events::EventSink,
) -> CommandResult<()> {
    let reason = crate::llm::friendly(&e.message);
    let for_note = reason.clone();
    let note_vault = vault.clone();
    let vault = vault.clone();
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
        // The draft note says the summary is on its way. It is not: say so,
        // where the person is looking. Never allowed to hold up the failure
        // itself, which is already recorded.
        let failed = recording.clone();
        match blocking(move || super::draft::note_failed(&note_vault, &failed, &for_note)).await {
            Ok(Some(change)) => events.changed(change),
            Ok(None) => {}
            Err(e) => tracing::warn!(
                recording = %id,
                error = %e.message,
                "meeting pipeline: could not say in the draft note that the summary failed"
            ),
        }
    } else {
        tracing::warn!(
            recording = %id,
            "meeting pipeline: refused to mark a `Done` recording as failed"
        );
    }
    Ok(())
}
