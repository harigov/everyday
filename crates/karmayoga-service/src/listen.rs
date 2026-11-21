//! Listening to mail and articles: what is read, and the session that reads it.
//!
//! [`crate::speech::listen`] is the reading -- text into audio, in order,
//! with the controls a player needs. This is what decides the text: a spoken
//! briefing of an email thread ([`crate::mailai::listen_pieces`]), or an
//! article made fit to hear ([`crate::news::listen::open`]).
//!
//! See `docs/plans/listen.md`.

use crate::error::CommandResult;
use crate::service::Service;
use crate::speech::listen::{ListenLink, PieceStream};
use karmayoga_core::id::{MailMessageId, ThreadId};
use karmayoga_core::{RoutineRunId, StoryId};
use serde::{Deserialize, Serialize};
use std::sync::Arc;

/// What is to be listened to. On the wire from the webview.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(tag = "type", rename_all = "camelCase", rename_all_fields = "camelCase")]
pub enum ListenSource {
    /// A spoken briefing of a mail thread, written by the model.
    MailThread { thread_id: ThreadId },
    /// An article from the Feed. `as_written` reads the text as it is,
    /// without the model making it fit to hear first.
    Story {
        story_id: StoryId,
        #[serde(default)]
        as_written: bool,
    },
    /// A daily brief, as it was written to be heard. Nothing is sent to a
    /// model: the spoken brief was written when the brief was.
    Brief { run_id: RoutineRunId },
}

/// What a spoken conversation reads out in full when the assistant is asked
/// to (`read_aloud`): read by the voice session after the reply, as the
/// Listen player reads it. Never on the wire; the tool decides it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ReadAloud {
    /// An article from the Feed, a newsletter story included, made fit to
    /// hear as the Feed's Listen makes it.
    Story(StoryId),
    /// One mail message -- a newsletter in the inbox -- in full, made fit to
    /// hear the same way.
    Message(MailMessageId),
    /// A daily brief's spoken version, kept on the run that wrote it.
    Brief(RoutineRunId),
}

/// A session under way, and what it is reading, for the player to show.
pub struct ListenOpened {
    pub link: ListenLink,
    pub title: String,
    /// Something the listener should be told as it starts, such as an
    /// article being read as written because no model may be used. The
    /// transport sends it as a [`ListenEvent::Trouble`](crate::speech::listen::ListenEvent).
    pub notice: Option<String>,
}

impl Service {
    /// Start reading `source` aloud at `speed` (a multiple of the voice's own
    /// speed, clamped to [`MIN_SPEED`](crate::speech::listen::MIN_SPEED) ..=
    /// [`MAX_SPEED`](crate::speech::listen::MAX_SPEED)). Refuses, in a
    /// sentence, when there is no voice, or when the source may not go to the
    /// model and cannot be read without it.
    pub async fn listen_open(
        self: &Arc<Self>,
        source: ListenSource,
        speed: f32,
    ) -> CommandResult<ListenOpened> {
        use crate::speech::listen::{ListenParts, MAX_SPEED, MIN_SPEED, VoiceAtSpeed};

        self.require_unlocked()?;
        // A speed that is not a number is the voice's own.
        let speed = if speed.is_finite() { speed.clamp(MIN_SPEED, MAX_SPEED) } else { 1.0 };

        // The voice first, so that a voice that is missing is refused in a
        // sentence before anything is sent to a model.
        let synthesiser = self.speech_synthesiser_at(speed).await?;
        let (title, pieces, notice) = match source {
            ListenSource::MailThread { thread_id } => {
                let (title, pieces) = crate::mailai::listen_pieces(self, thread_id).await?;
                (title, pieces, None)
            }
            ListenSource::Story { story_id, as_written } => {
                let opened = crate::news::listen::open(self, story_id, as_written).await?;
                (opened.title, opened.pieces, opened.notice)
            }
            ListenSource::Brief { run_id } => {
                let (title, pieces) = self.brief_pieces(run_id).await?;
                (title, pieces, None)
            }
        };

        let service = self.clone();
        let voice: VoiceAtSpeed = Arc::new(move |multiple| {
            let service = service.clone();
            Box::pin(async move { service.speech_synthesiser_at(multiple).await })
        });
        let link = crate::speech::listen::spawn(ListenParts { pieces, synthesiser, speed, voice });
        Ok(ListenOpened { link, title, notice })
    }

    /// The text of `what`, to be read aloud in a spoken conversation: the
    /// same preparation the Listen player's, and what it would have said to
    /// the listener first (a story read only from what its feed carried, or
    /// read as written because no model may be used) ahead of it.
    pub async fn read_aloud_text(self: &Arc<Self>, what: ReadAloud) -> CommandResult<PieceStream> {
        use crate::speech::listen::Piece;
        use futures::StreamExt;

        let opened = match what {
            ReadAloud::Story(story_id) => crate::news::listen::open(self, story_id, false).await?,
            ReadAloud::Message(message) => crate::news::listen::open_message(self, message).await?,
            ReadAloud::Brief(run_id) => return Ok(self.brief_pieces(run_id).await?.1),
        };
        let Some(notice) = opened.notice else { return Ok(opened.pieces) };
        // Its own part, so it is a sentence of its own before the first.
        let said = futures::stream::once(async move { Ok(Piece { text: notice, part: 0 }) });
        let rest = opened.pieces.map(|piece| piece.map(|p| Piece { part: p.part + 1, ..p }));
        Ok(said.chain(rest).boxed())
    }

    /// A brief's spoken version as pieces to read, a paragraph to a part, and
    /// what the player calls it.
    async fn brief_pieces(
        self: &Arc<Self>,
        run_id: RoutineRunId,
    ) -> CommandResult<(String, PieceStream)> {
        use crate::speech::listen::Piece;
        use futures::StreamExt;

        let vault = self.require()?;
        let run = crate::service::blocking(move || Ok(vault.run(run_id)?)).await?;
        let Some(brief) = run.brief else {
            return Err(crate::error::CommandError::new(
                crate::error::codes::INVALID,
                "that run did not write a brief",
            ));
        };
        let title = format!("{}, {}", brief.kind.name(), brief.day.strftime("%A %-d %B"));
        let pieces: Vec<CommandResult<Piece>> = brief
            .spoken
            .split("\n\n")
            .map(str::trim)
            .filter(|p| !p.is_empty())
            .enumerate()
            .map(|(part, text)| Ok(Piece { text: text.replace('\n', " "), part: part as u32 }))
            .collect();
        Ok((title, futures::stream::iter(pieces).boxed()))
    }
}
