//! What every chat service has to do, and nothing more.
//!
//! A connector is one signed-in bot account on one service. It owns
//! everything specific to that service -- the protocol, the encryption,
//! which rooms or chats exist -- and above all it owns *who is trusted*:
//! only messages from the link's owner -- from a device the owner picked, and
//! in a chat where nobody else can read the reply -- ever reach [`Incoming`].
//! Everything after that is the same for every service and lives in
//! [`crate::relay`].
//!
//! Deliberately small. Matrix is the only service today, and the trait is
//! what the relay needs from it, not a guess at what a future service might
//! offer -- a service that can edit messages or draw buttons gets a method
//! for it the day something uses one.

use async_trait::async_trait;
use jiff::Timestamp;
use karmayoga_core::chat::{ChatLink, ChatSecret};
use karmayoga_service::chat::{
    ChatDevices, ChatState, ChatTrust, ChatUntrustedDevice, ChatVerifier,
};
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
    /// Yes, and always allow this website -- offered only on a question about
    /// opening a page that names a site.
    Always,
}

impl Choice {
    /// The reaction that stands for this answer.
    pub fn emoji(self) -> &'static str {
        match self {
            Choice::Yes => "✅",
            Choice::No => "❌",
            Choice::Later => "⏰",
            Choice::Always => "🌐",
        }
    }

    /// The answer a reaction stands for. Matches the emoji with or without
    /// the variation selector some clients add, and the obvious thumbs.
    pub fn from_emoji(key: &str) -> Option<Self> {
        match key.trim_end_matches('\u{fe0f}') {
            "✅" | "✔" | "👍" => Some(Choice::Yes),
            "❌" | "✖" | "👎" => Some(Choice::No),
            "⏰" | "🕐" | "⏳" => Some(Choice::Later),
            "🌐" | "🔓" => Some(Choice::Always),
            _ => None,
        }
    }

    /// The answer a typed reply stands for: "yes", "no", "later", "always" and
    /// their usual short forms, case and a trailing full stop ignored. Anything
    /// else is not an answer, and is treated as a new message.
    pub fn from_text(text: &str) -> Option<Self> {
        let word = text.trim().trim_end_matches(['.', '!']).to_lowercase();
        match word.as_str() {
            "yes" | "y" | "ok" | "okay" | "go" | "go ahead" | "do it" | "sure" => Some(Choice::Yes),
            "no" | "n" | "cancel" | "stop" | "don't" | "dont" => Some(Choice::No),
            "later" | "not now" | "save it" | "park it" => Some(Choice::Later),
            "always" | "always allow" | "allow always" | "always allow it" | "trust it"
            | "trust this site" => Some(Choice::Always),
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

/// A picture the owner sent, as it came off the service: its bytes, and
/// what it was called.
#[derive(Clone, PartialEq, Eq)]
pub struct Picture {
    /// What the sender's device called it, for the thread to show. May be
    /// empty; the vault names it then.
    pub filename: String,
    pub bytes: Vec<u8>,
}

impl std::fmt::Debug for Picture {
    /// The name and the size, not the bytes: a log line or a failed
    /// assertion has no use for a few megabytes of numbers.
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Picture")
            .field("filename", &self.filename)
            .field("bytes", &self.bytes.len())
            .finish()
    }
}

/// The most a connector takes of one file the owner sends: 200 MB. A file is
/// held in memory from the moment it is downloaded until it is in the files
/// folder, and the owner is writing to a bot on a phone, not copying a
/// library; anything bigger is for the Files app, or for Syncthing.
pub const MAX_FILE_BYTES: u64 = 200 * 1000 * 1000;

/// A file the owner sent that is not a picture for the assistant to look at
/// -- a document, a film, a recording -- as it came off the service: its
/// bytes, and what it was called.
///
/// Saved whole into Files, as the file it is; the assistant is handed its id
/// and can read it if it is text. See `Service::chat_turn_with_files`.
#[derive(Clone, PartialEq, Eq)]
pub struct Document {
    /// What the sender's device called it. May be empty; the service names
    /// it from what it is then.
    pub filename: String,
    pub bytes: Vec<u8>,
}

impl std::fmt::Debug for Document {
    /// The name and the size, not the bytes, for the reason [`Picture`]
    /// gives.
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Document")
            .field("filename", &self.filename)
            .field("bytes", &self.bytes.len())
            .finish()
    }
}

/// A picture the assistant made, on its way to the owner: what it is called,
/// what it is, and its size, which a chat app uses to keep room for it while
/// it downloads.
#[derive(Clone, PartialEq, Eq)]
pub struct Shown {
    pub filename: String,
    /// `image/png` or `image/jpeg`.
    pub mime: String,
    pub bytes: Vec<u8>,
    pub width: Option<u32>,
    pub height: Option<u32>,
}

impl std::fmt::Debug for Shown {
    /// The name and the size, not the bytes, for the reason [`Picture`]
    /// gives.
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Shown")
            .field("filename", &self.filename)
            .field("mime", &self.mime)
            .field("bytes", &self.bytes.len())
            .finish()
    }
}

/// One thing the owner sent beside their words, in the order they sent it: a
/// picture the assistant is shown, or a file it is told about. What the relay
/// holds for what is said next, and hands the service with the turn.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Sent {
    Picture(Picture),
    Document(Document),
}

impl Sent {
    /// What it was called, for a log line or a test.
    pub fn filename(&self) -> &str {
        match self {
            Sent::Picture(picture) => &picture.filename,
            Sent::Document(document) => &document.filename,
        }
    }
}

/// One voice call, as the service names it. On Matrix: the owner's device's
/// membership of the room's call (MatrixRTC).
#[derive(Debug, Clone, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct CallId(pub String);

/// One key of a call's end-to-end encrypted audio, as the participant it
/// belongs to sent it over the chat service's own encryption. A call's audio
/// is encrypted frame by frame with keys the media server never sees.
#[derive(Clone, PartialEq, Eq)]
pub struct CallKey {
    /// Whose frames it opens, as the media server names that participant:
    /// on Matrix, `@user:server:DEVICE`.
    pub participant: String,
    /// Which of the participant's keys it is: each frame says which it was
    /// encrypted with.
    pub index: u8,
    /// The key material as sent, before it is derived into a cipher key.
    pub key: Vec<u8>,
}

impl std::fmt::Debug for CallKey {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("CallKey")
            .field("participant", &self.participant)
            .field("index", &self.index)
            .field("key", &format_args!("<{} bytes>", self.key.len()))
            .finish()
    }
}

/// Where a call's audio is, once this side has joined it: a room on a
/// LiveKit media server, which the owner's device sends to and receives from.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CallJoin {
    /// The media server's WebSocket address (`wss://…`).
    pub url: String,
    /// The access token it was given for this side, naming this side's
    /// participant and the room.
    pub token: String,
    /// The one participant whose audio is listened to, as the media server
    /// names them: the owner's device that is calling. Anybody else in the
    /// room is ignored.
    pub caller: String,
    /// This side's own key, which the caller has been sent: everything this
    /// side says is encrypted with it.
    pub own_key: CallKey,
    /// Join the media room only once the caller has been connected in it a
    /// moment. FluffyChat, calling, stops ringing only for somebody who joins
    /// after its own connection is up -- not for anybody already there -- and
    /// it says it is in the call before it connects, so answering at once
    /// leaves it ringing over the answer.
    pub wait_for_caller: bool,
}

/// Why this side is ending a call. A connector turns it into whatever its
/// service says: on Matrix, a call this side joined is left, and one it
/// never joined needs nothing (MatrixRTC has no rejection FluffyChat reads).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HangUp {
    /// The conversation is over: the owner said goodbye, `!stop` was sent,
    /// or the relay is stopping.
    Finished,
    /// The call cannot be taken at all -- the voice is not set up, the vault
    /// is locking, another call is running -- and the owner has been told why
    /// in the chat.
    Refused,
    /// The two devices never reached each other, or lost each other.
    Unreachable,
    /// Something broke on this side mid-call.
    Failed,
}

/// What the owner did.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Incoming {
    /// They wrote something. `reply_to` is the message of this side's they
    /// sent it as a reply to, when they did -- which is how a reply to a
    /// background job's update is known to be about that job.
    Said { chat: ChatId, text: String, at: Timestamp, reply_to: Option<MessageRef> },
    /// They sent a picture, with `caption` -- what they wrote with it, or
    /// nothing. One picture each: a service that sends several at once
    /// sends them as several messages, and the relay gathers them up with
    /// what is said next. See `relay`'s `PICTURE_WAIT`.
    Showed { chat: ChatId, picture: Picture, caption: String, at: Timestamp },
    /// They sent a file that is not a picture to look at -- a document, a
    /// film, a recording -- with `caption`. Treated like [`Incoming::Showed`]
    /// in every way the relay cares about: one each, held for what is said
    /// next, never an answer to a question. Within the service's size limit:
    /// a connector refuses one over it, and says so, before it gets here.
    Handed { chat: ChatId, document: Document, caption: String, at: Timestamp },
    /// They picked an answer to a [`Question`] this side asked.
    Chose { chat: ChatId, question: MessageRef, choice: Choice, at: Timestamp },
    /// They sent something this side could not read -- an encrypted message
    /// whose key never arrived. Worth saying so rather than staying silent.
    Unreadable { chat: ChatId, at: Timestamp },
    /// They are calling: their device has joined a call in the chat, which
    /// this side joins -- [`Connector::join_call`] -- to answer, before
    /// `expires` or not at all. A connector delivers only a call that has not
    /// expired yet, from the owner, in a chat nobody else is in. A call that
    /// asks for video is answered with voice.
    Called { chat: ChatId, call: CallId, at: Timestamp, expires: Timestamp },
    /// Keys of the caller's encrypted audio, for a call delivered as
    /// [`Incoming::Called`]. May arrive before that call is answered, and
    /// again while it runs (a new key replaces none: frames name theirs).
    CallKeys { chat: ChatId, call: CallId, keys: Vec<CallKey>, at: Timestamp },
    /// The owner hung up, declined their own call, or answered it somewhere
    /// else. Nothing more is to be sent for it.
    CallEnded { chat: ChatId, call: CallId, at: Timestamp },
}

impl Incoming {
    pub fn chat(&self) -> &ChatId {
        match self {
            Incoming::Said { chat, .. }
            | Incoming::Showed { chat, .. }
            | Incoming::Handed { chat, .. }
            | Incoming::Chose { chat, .. }
            | Incoming::Unreadable { chat, .. }
            | Incoming::Called { chat, .. }
            | Incoming::CallKeys { chat, .. }
            | Incoming::CallEnded { chat, .. } => chat,
        }
    }

    pub fn at(&self) -> Timestamp {
        match self {
            Incoming::Said { at, .. }
            | Incoming::Showed { at, .. }
            | Incoming::Handed { at, .. }
            | Incoming::Chose { at, .. }
            | Incoming::Unreadable { at, .. }
            | Incoming::Called { at, .. }
            | Incoming::CallKeys { at, .. }
            | Incoming::CallEnded { at, .. } => *at,
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

/// Tells the settings pane what the bot knows of trust with its owner: a
/// verification in progress, with its emoji, and whether the owner's keys
/// are verified. Each call replaces the last.
pub type TrustReport = Arc<dyn Fn(ChatTrust) + Send + Sync>;

/// Tells the settings pane that a device of the owner's which is not on the
/// trusted list wrote to the bot and was ignored. Called each time one does,
/// so the connector keeps it to a trickle; the service raises a notification
/// only the first time for each device.
pub type UntrustedReport = Arc<dyn Fn(ChatUntrustedDevice) + Send + Sync>;

/// Everything a connector starts from.
pub struct Setup {
    pub link: ChatLink,
    /// As saved. A connector that signs in, or sets up encryption, writes
    /// what changed back through [`Setup::save_secret`] straight away -- and
    /// clears the password once it has a session.
    pub secret: ChatSecret,
    pub save_secret: SaveSecret,
    /// A directory of its own for local state, created if missing. See
    /// `karmayoga_service::chat::state_dir`.
    pub state_dir: PathBuf,
    /// What to encrypt that local state with.
    pub store_passphrase: String,
    pub report: Report,
    pub trust: TrustReport,
    /// Where an owner's device that is not trusted is reported.
    pub untrusted: UntrustedReport,
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

    /// Send a picture to `chat`, as a picture rather than a file: what the
    /// assistant made while answering, sent as soon as it is made.
    ///
    /// A service that cannot send pictures says so; the reply has already
    /// gone, and the picture is in the owner's Files either way.
    async fn show(&self, chat: &ChatId, picture: &Shown) -> Result<MessageRef, ChatError> {
        let _ = (chat, picture);
        Err(ChatError::Transient("This chat service cannot send pictures.".into()))
    }

    /// The owner's private chat with the bot, where something that is not a
    /// reply -- a routine's result, its question -- goes. `None` until there
    /// is one the owner has joined.
    async fn home(&self) -> Option<ChatId>;

    /// Show, or stop showing, that the bot is typing in `chat`. Stays on
    /// until it is turned off, however long that is: a service whose typing
    /// notice expires is the connector's to keep renewing.
    async fn typing(&self, chat: &ChatId, on: bool) -> Result<(), ChatError>;

    /// How Settings answers the owner verifying the bot, for a service that
    /// has interactive verification. Only works while [`Connector::listen`]
    /// runs: that is when the owner's requests are heard.
    fn verifier(&self) -> Option<Arc<dyn ChatVerifier>> {
        None
    }

    /// How Settings lists the owner's devices and chooses which of them the
    /// bot trusts, for a service that has devices. Only a signed-in
    /// connector has one.
    fn devices(&self) -> Option<Arc<dyn ChatDevices>> {
        None
    }

    /// Join a call delivered as [`Incoming::Called`], to answer it: tell the
    /// service this side is in it, send the caller this side's key, and say
    /// where its audio is. Hanging up afterwards is [`Connector::end_call`].
    ///
    /// A service without calls never delivers one, so is never asked.
    async fn join_call(&self, chat: &ChatId, call: &CallId) -> Result<CallJoin, ChatError> {
        let _ = (chat, call);
        Err(ChatError::Transient("This chat service has no calls.".into()))
    }

    /// Ring the owner in `chat`: this side joins a call there and the
    /// owner's devices ring. When they pick up, their joining arrives through
    /// [`Connector::listen`] as an ordinary [`Incoming::Called`], answered
    /// with [`Connector::join_call`]. The ring is the returned call: ending it
    /// with [`Connector::end_call`] stops the ringing and leaves, if nobody
    /// picked up.
    ///
    /// A service without calls says so.
    async fn ring(&self, chat: &ChatId) -> Result<CallId, ChatError> {
        let _ = chat;
        Err(ChatError::Transient("This chat service has no calls.".into()))
    }

    /// End a call, answered or not, saying why. Ending one that has ended
    /// already -- the owner hung up first -- is not an error.
    async fn end_call(&self, chat: &ChatId, call: &CallId, why: HangUp) -> Result<(), ChatError> {
        let _ = (chat, call, why);
        Ok(())
    }
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
    fn always_is_recognised_by_its_words_and_its_reaction() {
        for text in [
            "always",
            "Always.",
            " ALWAYS ALLOW ",
            "allow always",
            "Always allow it!",
            "trust it",
            "Trust this site.",
            "🌐",
            "🔓",
        ] {
            assert_eq!(Choice::from_text(text), Some(Choice::Always), "{text:?}");
        }
        assert_eq!(Choice::from_text("always allow it, and book a table"), None);
        assert_eq!(Choice::from_text("trust"), None);
    }

    #[test]
    fn reactions_match_with_or_without_the_variation_selector() {
        assert_eq!(Choice::from_emoji("✅"), Some(Choice::Yes));
        assert_eq!(Choice::from_emoji("✔\u{fe0f}"), Some(Choice::Yes));
        assert_eq!(Choice::from_emoji("❌"), Some(Choice::No));
        assert_eq!(Choice::from_emoji("⏰"), Some(Choice::Later));
        assert_eq!(Choice::from_emoji("🌐"), Some(Choice::Always));
        assert_eq!(Choice::from_emoji("🌐\u{fe0f}"), Some(Choice::Always));
        assert_eq!(Choice::from_emoji("🔓"), Some(Choice::Always));
        assert_eq!(Choice::from_emoji("🔓\u{fe0f}"), Some(Choice::Always));
        assert_eq!(Choice::from_emoji("🎉"), None);
    }
}
