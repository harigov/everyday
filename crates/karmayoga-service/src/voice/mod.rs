//! Talking to the assistant.
//!
//! One session per conversation being talked: audio in, audio and events out.
//! Nothing here knows where the audio comes from. The desktop app's microphone
//! and speakers are the first transport (`karmayoga-app`'s `voice` module); a
//! WebRTC peer in a browser or a Matrix or XMPP call is another caller of
//! [`Service::voice_open`](crate::service::Service), handing over 16 kHz mono
//! audio and playing what comes back. Echo cancellation belongs to the
//! transport, which is the only place that knows what was really played.
//!
//! A session is a user of [`crate::speech`], not a part of it. Listening is a
//! [`LiveTranscription`] (a detector and a recogniser) and speaking is a
//! [`Synthesiser`] behind a `Narrator`; what a conversation adds is deciding
//! what the words mean -- when to pause, what is an echo, what a spoken yes
//! approves -- and running the assistant's turn on them.
//!
//! ```text
//!   VoiceInput::Audio ─▶ LiveTranscription ─▶ assistant turn ─▶ text ─▶ Narrator ─▶ VoiceOutput::Play
//!                                                   │
//!                                                   └─▶ VoiceEvent::Agent, for the thread on screen
//! ```
//!
//! See `docs/plans/voice.md`.

pub mod assistant;
pub mod confirm;
pub mod phrases;
pub mod session;

use crate::agent::{AgentEvent, ConfirmAnswer, Sink};
use crate::error::CommandResult;
use crate::speech::live::LiveTranscription;
use crate::speech::synth::Synthesiser;
pub use crate::speech::transcribe::BoxFuture;
use karmayoga_core::agent::onscreen::OnScreen;
use serde::{Deserialize, Serialize};

pub use crate::speech::SAMPLE_RATE;

/// What the session is doing, for the bar on screen.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum VoiceState {
    /// Models are loading.
    Starting,
    /// Waiting for the person to talk.
    Listening,
    /// The person is talking.
    Hearing,
    /// What they said is being turned into text.
    Understanding,
    /// The assistant is working on it.
    Thinking,
    /// The assistant is talking.
    Speaking,
    /// The assistant asked a yes-or-no question aloud and is waiting.
    Asking,
    /// The assistant is waiting on a card only a click can answer.
    WaitingForClick,
}

/// What a session tells the interface. On the wire to the webview.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "camelCase", rename_all_fields = "camelCase")]
pub enum VoiceEvent {
    State {
        state: VoiceState,
    },
    /// What the person said; the assistant's turn on it follows. The thread
    /// shows this as their message.
    Heard {
        text: String,
    },
    /// Something from the running turn, exactly as `send_message` streams it.
    Agent {
        event: AgentEvent,
    },
    /// The person talked over the reply: `message_id` now reads `text`, which
    /// is what they actually heard of it.
    ReplyCut {
        message_id: String,
        text: String,
    },
    /// Something went wrong, and the session carries on.
    Trouble {
        message: String,
    },
    /// The session is over. `error` says why, when it did not end on request.
    Ended {
        error: Option<String>,
    },
}

/// What a transport hands a session.
#[derive(Debug)]
pub enum VoiceInput {
    /// Microphone audio at [`SAMPLE_RATE`], echo already removed.
    Audio(Vec<i16>),
    /// Everything of [`VoiceOutput::Play`]'s `utterance` has been played.
    Played { utterance: u64 },
    /// While muted nothing is listened to, and what is being said goes on.
    Muted(bool),
    /// What the person has on screen now, for the next turn.
    Screen(Option<OnScreen>),
    /// End the session.
    Stop,
}

/// What a session asks of a transport.
#[derive(Debug)]
pub enum VoiceOutput {
    /// Queue this to be played after whatever is queued already.
    Play { utterance: u64, samples: Vec<i16>, sample_rate: u32 },
    /// Hold playback where it is: the person may be talking over it.
    Pause,
    /// Carry on from where [`VoiceOutput::Pause`] held it.
    Resume,
    /// Throw away everything queued, and anything held.
    Clear,
    /// Tell the interface.
    Event(VoiceEvent),
}

/// Both ends of a running session, from the transport's side. The session
/// ends with [`VoiceEvent::Ended`], after which `output` closes.
pub struct VoiceLink {
    pub input: tokio::sync::mpsc::UnboundedSender<VoiceInput>,
    pub output: tokio::sync::mpsc::UnboundedReceiver<VoiceOutput>,
}

/// How a turn ended.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TurnOutcome {
    pub stopped: bool,
    /// The assistant ended the conversation out loud -- see [`END_TOOL`]. The
    /// session ends once what it said has been heard.
    pub ended_voice: bool,
}

/// The tool a spoken turn is offered for ending the conversation out loud,
/// when the person has said they are done: "that's it", "that's all I have".
/// The model decides, so that "that's it!" said of an answer that is right is
/// not taken for goodbye. Only the talking ends; the thread stays. Declared
/// by `agent`'s `build`, which offers it to spoken turns alone; named in the
/// core, whose note to a spoken turn tells the model about it.
pub const END_TOOL: &str = karmayoga_core::agent::END_VOICE_TOOL;

/// The assistant, as a session needs it: what [`assistant::ServiceAssistant`]
/// implements over the real thing, and a test over a script.
pub trait VoiceAssistant: Send + Sync {
    /// Run one spoken turn, streaming into `sink`.
    fn turn(
        &self,
        prompt: String,
        on_screen: Option<OnScreen>,
        sink: Sink,
    ) -> BoxFuture<'static, CommandResult<TurnOutcome>>;
    /// Stop the running turn. Whether there was one.
    fn cancel(&self) -> bool;
    /// Answer a waiting confirmation. False if nothing was waiting.
    fn answer(&self, call_id: &str, answer: ConfirmAnswer) -> bool;
    /// Make stored reply `message_id` read `heard`.
    fn keep_heard(
        &self,
        message_id: String,
        heard: String,
    ) -> BoxFuture<'static, CommandResult<()>>;
}

/// What a session is built from.
pub struct VoiceParts {
    /// The microphone side: audio in, timed words out.
    pub live: LiveTranscription,
    /// The voice, for everything the session says.
    pub synthesiser: std::sync::Arc<dyn Synthesiser>,
    pub assistant: std::sync::Arc<dyn VoiceAssistant>,
    /// What is said when only a click can answer a question: where the person
    /// will find it. [`confirm::NEEDS_A_CLICK`] at this computer,
    /// [`confirm::NEEDS_A_TAP`] on a call.
    pub needs_a_click: &'static str,
}
