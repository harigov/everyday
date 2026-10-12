//! What every chat service has to do, and nothing more.
//!
//! A connector is one signed-in bot account on one service. It owns
//! everything specific to that service -- the protocol, the encryption,
//! which rooms or chats exist -- and above all it owns *who is trusted*:
//! only messages from the link's owner, in a chat where nobody else can read
//! the reply, ever reach [`Incoming`]. Everything after that is the same for
//! every service and lives in [`crate::relay`].
//!
//! Deliberately small. Matrix is the only service today, and the trait is
//! what the relay needs from it, not a guess at what a future service might
//! offer -- a service that can edit messages or draw buttons gets a method
//! for it the day something uses one.

use async_trait::async_trait;
use everyday_core::chat::{ChatLink, ChatSecret};
use everyday_service::chat::ChatState;
use jiff::Timestamp;
use std::path::PathBuf;
use std::sync::Arc;
use tokio::sync::{mpsc, watch};

/// One chat on a service, as that service names it: a Matrix room id.
#[derive(Debug, Clone, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct ChatId(pub String);

/// A message this side sent, as the service names it: a Matrix event id.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct MessageRef(pub String);

/// An answer to a [`Question`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Choice {
    Yes,
    No,
    Later,
}

impl Choice {
    /// The reaction that stands for this answer.
    pub fn emoji(self) -> &'static str {
        match self {
            Choice::Yes => "✅",
            Choice::No => "❌",
            Choice::Later => "⏰",
        }
    }

    /// The answer a reaction stands for. Matches the emoji with or without
    /// the variation selector some clients add, and the obvious thumbs.
    pub fn from_emoji(key: &str) -> Option<Self> {
        match key.trim_end_matches('\u{fe0f}') {
            "✅" | "✔" | "👍" => Some(Choice::Yes),
            "❌" | "✖" | "👎" => Some(Choice::No),
            "⏰" | "🕐" | "⏳" => Some(Choice::Later),
            _ => None,
        }
    }

    /// The answer a typed reply stands for: "yes", "no", "later" and their
    /// usual short forms, case and a trailing full stop ignored. Anything
    /// else is not an answer, and is treated as a new message.
    pub fn from_text(text: &str) -> Option<Self> {
        let word = text.trim().trim_end_matches(['.', '!']).to_lowercase();
        match word.as_str() {
            "yes" | "y" | "ok" | "okay" | "go" | "go ahead" | "do it" | "sure" => Some(Choice::Yes),
            "no" | "n" | "cancel" | "stop" | "don't" | "dont" => Some(Choice::No),
            "later" | "not now" | "save it" | "park it" => Some(Choice::Later),
            _ => Choice::from_emoji(&word),
        }
    }
}

/// Something the bot needs a decision on: Markdown, and the answers that
/// may be given. A service that can draw tappable choices -- reactions on
/// Matrix -- draws them; the text still says what to type.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Question {
    pub markdown: String,
    pub choices: Vec<Choice>,
}

/// What the owner did.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Incoming {
    /// They wrote something.
    Said { chat: ChatId, text: String, at: Timestamp },
    /// They picked an answer to a [`Question`] this side asked.
    Chose { chat: ChatId, question: MessageRef, choice: Choice, at: Timestamp },
    /// They sent something this side could not read -- an encrypted message
    /// whose key never arrived. Worth saying so rather than staying silent.
    Unreadable { chat: ChatId, at: Timestamp },
}

impl Incoming {
    pub fn chat(&self) -> &ChatId {
        match self {
            Incoming::Said { chat, .. }
            | Incoming::Chose { chat, .. }
            | Incoming::Unreadable { chat, .. } => chat,
        }
    }

    pub fn at(&self) -> Timestamp {
        match self {
            Incoming::Said { at, .. }
            | Incoming::Chose { at, .. }
            | Incoming::Unreadable { at, .. } => *at,
        }
    }
}

/// Why a connector stopped.
#[derive(Debug, thiserror::Error)]
pub enum ChatError {
    /// The sign-in is missing, expired or was refused. Trying again will not
    /// help until somebody enters a password in Settings.
    #[error("{0}")]
    NeedsSignIn(String),
    /// Anything worth trying again: the server is down, the network
    /// dropped, a request timed out.
    #[error("{0}")]
    Transient(String),
}

/// Writes a link's secret back to the vault.
pub type SaveSecret = Arc<dyn Fn(&ChatSecret) -> Result<(), ChatError> + Send + Sync>;

/// Tells the settings pane where a link stands.
pub type Report = Arc<dyn Fn(ChatState, String) + Send + Sync>;

/// Everything a connector starts from.
pub struct Setup {
    pub link: ChatLink,
    /// As saved. A connector that signs in, or sets up encryption, writes
    /// what changed back through [`Setup::save_secret`] straight away -- and
    /// clears the password once it has a session.
    pub secret: ChatSecret,
    pub save_secret: SaveSecret,
    /// A directory of its own for local state, created if missing. See
    /// `everyday_service::chat::state_dir`.
    pub state_dir: PathBuf,
    /// What to encrypt that local state with.
    pub store_passphrase: String,
    pub report: Report,
    /// What the person named their assistant, for the greeting. Empty means
    /// it has no name: "the assistant".
    pub assistant_name: String,
}

/// One signed-in bot account on one chat service.
#[async_trait]
pub trait Connector: Send + Sync + 'static {
    /// Deliver the owner's messages to `inbox` until `stop` turns true
    /// (`Ok`) or the connection fails (`Err`).
    ///
    /// Only the owner's: a message from anybody else, or in a chat anybody
    /// else can read, never arrives here. Messages that arrived while the
    /// bot was offline are delivered too, with the time they were sent --
    /// what to do about old ones is the relay's decision.
    async fn listen(
        &self,
        inbox: mpsc::Sender<Incoming>,
        stop: watch::Receiver<bool>,
    ) -> Result<(), ChatError>;

    /// Send `markdown` to `chat`.
    async fn send(&self, chat: &ChatId, markdown: &str) -> Result<MessageRef, ChatError>;

    /// Ask a question in `chat`, offering its choices however this service
    /// can. The answer arrives through [`Connector::listen`] as
    /// [`Incoming::Chose`] (a tapped choice) or [`Incoming::Said`] (a typed
    /// one).
    async fn ask(&self, chat: &ChatId, question: &Question) -> Result<MessageRef, ChatError>;

    /// The owner's private chat with the bot, where something that is not a
    /// reply -- a routine's result, its question -- goes. `None` until there
    /// is one the owner has joined.
    async fn home(&self) -> Option<ChatId>;

    /// Show, or stop showing, that the bot is typing in `chat`. Stays on
    /// until it is turned off, however long that is: a service whose typing
    /// notice expires is the connector's to keep renewing.
    async fn typing(&self, chat: &ChatId, on: bool) -> Result<(), ChatError>;
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn typed_answers_are_recognised_and_other_text_is_not() {
        assert_eq!(Choice::from_text("Yes."), Some(Choice::Yes));
        assert_eq!(Choice::from_text(" no "), Some(Choice::No));
        assert_eq!(Choice::from_text("later"), Some(Choice::Later));
        assert_eq!(Choice::from_text("✅"), Some(Choice::Yes));
        assert_eq!(Choice::from_text("yes, and also book a table"), None);
    }

    #[test]
    fn reactions_match_with_or_without_the_variation_selector() {
        assert_eq!(Choice::from_emoji("✅"), Some(Choice::Yes));
        assert_eq!(Choice::from_emoji("✔\u{fe0f}"), Some(Choice::Yes));
        assert_eq!(Choice::from_emoji("❌"), Some(Choice::No));
        assert_eq!(Choice::from_emoji("⏰"), Some(Choice::Later));
        assert_eq!(Choice::from_emoji("🎉"), None);
    }
}
