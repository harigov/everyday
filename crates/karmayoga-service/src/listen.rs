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
use crate::speech::listen::ListenLink;
use karmayoga_core::StoryId;
use karmayoga_core::id::ThreadId;
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
        };

        let service = self.clone();
        let voice: VoiceAtSpeed = Arc::new(move |multiple| {
            let service = service.clone();
            Box::pin(async move { service.speech_synthesiser_at(multiple).await })
        });
        let link = crate::speech::listen::spawn(ListenParts { pieces, synthesiser, speed, voice });
        Ok(ListenOpened { link, title, notice })
    }
}
