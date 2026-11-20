//! What is said to a LiveKit media server over its WebSocket: protobuf, one
//! message per binary frame.
//!
//! Only the messages, and the fields of them, that a voice call uses --
//! declared by hand from LiveKit's `livekit_rtc.proto` and
//! `livekit_models.proto` (github.com/livekit/protocol, `protobufs/`), with
//! their field numbers as they are there. protobuf skips what a reader does
//! not declare, so everything else the server says arrives as a message
//! with no content, and is ignored.

use prost::{Message, Oneof};

/// What the client says: `SignalRequest`.
#[derive(Clone, PartialEq, Message)]
pub struct SignalRequest {
    #[prost(oneof = "Request", tags = "1, 2, 3, 4, 8, 16")]
    pub message: Option<Request>,
}

#[derive(Clone, PartialEq, Oneof)]
pub enum Request {
    /// An offer for the publisher connection, the one this side sends on.
    #[prost(message, tag = "1")]
    Offer(SessionDescription),
    /// The answer to the server's offer for the subscriber connection, the
    /// one this side receives on.
    #[prost(message, tag = "2")]
    Answer(SessionDescription),
    #[prost(message, tag = "3")]
    Trickle(TrickleRequest),
    #[prost(message, tag = "4")]
    AddTrack(AddTrackRequest),
    #[prost(message, tag = "8")]
    Leave(LeaveRequest),
    #[prost(message, tag = "16")]
    PingReq(Ping),
}

/// What the server says: `SignalResponse`.
#[derive(Clone, PartialEq, Message)]
pub struct SignalResponse {
    #[prost(oneof = "Response", tags = "1, 2, 3, 4, 5, 6, 8, 20, 23")]
    pub message: Option<Response>,
}

#[derive(Clone, PartialEq, Oneof)]
pub enum Response {
    /// The join was accepted. Always the first thing the server says.
    #[prost(message, tag = "1")]
    Join(JoinResponse),
    /// The server's answer to the publisher offer.
    #[prost(message, tag = "2")]
    Answer(SessionDescription),
    /// The server's offer for the subscriber connection: again each time the
    /// tracks this side receives change.
    #[prost(message, tag = "3")]
    Offer(SessionDescription),
    #[prost(message, tag = "4")]
    Trickle(TrickleRequest),
    /// Somebody in the room changed: joined, published, left.
    #[prost(message, tag = "5")]
    Update(ParticipantUpdate),
    /// The track this side asked to publish has been accepted.
    #[prost(message, tag = "6")]
    TrackPublished(TrackPublishedResponse),
    /// The server is ending this side's session.
    #[prost(message, tag = "8")]
    Leave(LeaveRequest),
    #[prost(message, tag = "20")]
    PongResp(Pong),
    /// Somebody has subscribed to this side's track for the first time: what
    /// it says from now on is heard.
    #[prost(message, tag = "23")]
    TrackSubscribed(TrackSubscribed),
}

#[derive(Clone, PartialEq, Message)]
pub struct SessionDescription {
    /// `"offer"` or `"answer"`.
    #[prost(string, tag = "1")]
    pub r#type: String,
    #[prost(string, tag = "2")]
    pub sdp: String,
    /// Which negotiation this is, so an answer is matched to its offer. Zero
    /// from a server too old to number them.
    #[prost(uint32, tag = "3")]
    pub id: u32,
}

/// Which of the two connections a candidate is for.
#[derive(Clone, Copy, Debug, PartialEq, Eq, prost::Enumeration)]
#[repr(i32)]
pub enum SignalTarget {
    Publisher = 0,
    Subscriber = 1,
}

#[derive(Clone, PartialEq, Message)]
pub struct TrickleRequest {
    /// A JSON `RTCIceCandidateInit`: `{"candidate": "candidate:…",
    /// "sdpMid": "0", "sdpMLineIndex": 0}`.
    #[prost(string, tag = "1")]
    pub candidate_init: String,
    #[prost(enumeration = "SignalTarget", tag = "2")]
    pub target: i32,
    #[prost(bool, tag = "3")]
    pub r#final: bool,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, prost::Enumeration)]
#[repr(i32)]
pub enum TrackType {
    Audio = 0,
    Video = 1,
    Data = 2,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, prost::Enumeration)]
#[repr(i32)]
pub enum TrackSource {
    Unknown = 0,
    Camera = 1,
    Microphone = 2,
    ScreenShare = 3,
    ScreenShareAudio = 4,
}

/// `Encryption.Type`: how a track's frames are encrypted end to end.
#[derive(Clone, Copy, Debug, PartialEq, Eq, prost::Enumeration)]
#[repr(i32)]
pub enum EncryptionType {
    None = 0,
    Gcm = 1,
    Custom = 2,
}

#[derive(Clone, PartialEq, Message)]
pub struct AddTrackRequest {
    /// The client's own name for the track: the track id in the publisher
    /// offer's `a=msid`, which is how the server matches the two.
    #[prost(string, tag = "1")]
    pub cid: String,
    #[prost(string, tag = "2")]
    pub name: String,
    #[prost(enumeration = "TrackType", tag = "3")]
    pub r#type: i32,
    #[prost(bool, tag = "6")]
    pub muted: bool,
    #[prost(enumeration = "TrackSource", tag = "8")]
    pub source: i32,
    /// Redundant audio is not wanted: every frame is encrypted, and a
    /// receiver that encrypts turns it off too.
    #[prost(bool, tag = "13")]
    pub disable_red: bool,
    #[prost(enumeration = "EncryptionType", tag = "14")]
    pub encryption: i32,
}

#[derive(Clone, PartialEq, Message)]
pub struct TrackSubscribed {
    #[prost(string, tag = "1")]
    pub track_sid: String,
}

#[derive(Clone, PartialEq, Message)]
pub struct TrackPublishedResponse {
    #[prost(string, tag = "1")]
    pub cid: String,
    #[prost(message, optional, tag = "2")]
    pub track: Option<TrackInfo>,
}

#[derive(Clone, PartialEq, Message)]
pub struct JoinResponse {
    #[prost(message, optional, tag = "1")]
    pub room: Option<Room>,
    /// This side, as the server knows it.
    #[prost(message, optional, tag = "2")]
    pub participant: Option<ParticipantInfo>,
    #[prost(message, repeated, tag = "3")]
    pub other_participants: Vec<ParticipantInfo>,
    #[prost(string, tag = "4")]
    pub server_version: String,
    /// Seconds without an answer to a ping before the server is taken to be
    /// gone, and seconds between pings.
    #[prost(int32, tag = "10")]
    pub ping_timeout: i32,
    #[prost(int32, tag = "11")]
    pub ping_interval: i32,
    /// What the server ends the silent frames it makes up itself with. They
    /// are not encrypted, and say so with this.
    #[prost(bytes = "vec", tag = "13")]
    pub sif_trailer: Vec<u8>,
}

#[derive(Clone, PartialEq, Message)]
pub struct Room {
    #[prost(string, tag = "1")]
    pub sid: String,
    #[prost(string, tag = "2")]
    pub name: String,
}

#[derive(Clone, PartialEq, Message)]
pub struct ParticipantUpdate {
    #[prost(message, repeated, tag = "1")]
    pub participants: Vec<ParticipantInfo>,
}

/// `ParticipantInfo.State`.
#[derive(Clone, Copy, Debug, PartialEq, Eq, prost::Enumeration)]
#[repr(i32)]
pub enum ParticipantState {
    Joining = 0,
    Joined = 1,
    Active = 2,
    Disconnected = 3,
}

#[derive(Clone, PartialEq, Message)]
pub struct ParticipantInfo {
    /// The server's id for this session of the participant: `PA_…`.
    #[prost(string, tag = "1")]
    pub sid: String,
    /// Who it is, as the token named them: on MatrixRTC, `@user:server:DEVICE`.
    #[prost(string, tag = "2")]
    pub identity: String,
    #[prost(enumeration = "ParticipantState", tag = "3")]
    pub state: i32,
    #[prost(message, repeated, tag = "4")]
    pub tracks: Vec<TrackInfo>,
}

#[derive(Clone, PartialEq, Message)]
pub struct TrackInfo {
    /// The server's id for the track: `TR_…`.
    #[prost(string, tag = "1")]
    pub sid: String,
    #[prost(enumeration = "TrackType", tag = "2")]
    pub r#type: i32,
    #[prost(string, tag = "3")]
    pub name: String,
    #[prost(bool, tag = "4")]
    pub muted: bool,
    #[prost(enumeration = "TrackSource", tag = "9")]
    pub source: i32,
    #[prost(string, tag = "11")]
    pub mime_type: String,
    #[prost(enumeration = "EncryptionType", tag = "16")]
    pub encryption: i32,
}

/// `DisconnectReason`, the one value this side sends.
pub const CLIENT_INITIATED: i32 = 1;

/// `LeaveRequest.Action`, the one value this side sends.
pub const DISCONNECT: i32 = 0;

#[derive(Clone, PartialEq, Message)]
pub struct LeaveRequest {
    #[prost(bool, tag = "1")]
    pub can_reconnect: bool,
    #[prost(int32, tag = "2")]
    pub reason: i32,
    #[prost(int32, tag = "3")]
    pub action: i32,
}

#[derive(Clone, PartialEq, Message)]
pub struct Ping {
    /// Milliseconds since the Unix epoch.
    #[prost(int64, tag = "1")]
    pub timestamp: i64,
    #[prost(int64, tag = "2")]
    pub rtt: i64,
}

#[derive(Clone, PartialEq, Message)]
pub struct Pong {
    #[prost(int64, tag = "1")]
    pub last_ping_timestamp: i64,
    #[prost(int64, tag = "2")]
    pub timestamp: i64,
}

/// A request as bytes for the WebSocket.
pub fn encode(message: Request) -> Vec<u8> {
    SignalRequest { message: Some(message) }.encode_to_vec()
}

/// A frame from the WebSocket. `None` for one that is not a response this
/// side reads (anything the server says that a voice call has no use for).
pub fn decode(bytes: &[u8]) -> Option<Response> {
    SignalResponse::decode(bytes).ok()?.message
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_request_round_trips_with_its_field_numbers() {
        let bytes = encode(Request::Trickle(TrickleRequest {
            candidate_init: "{}".into(),
            target: SignalTarget::Subscriber as i32,
            r#final: false,
        }));
        // Field 3 (trickle), length-delimited; inside, field 1 "{}" and
        // field 2 = 1.
        assert_eq!(bytes, [0x1a, 0x06, 0x0a, 0x02, b'{', b'}', 0x10, 0x01]);
    }

    #[test]
    fn a_response_with_fields_not_declared_still_reads() {
        // A JoinResponse (field 1) carrying an unknown field 99 and a
        // server_version, as a newer server would send.
        let mut inner = Vec::new();
        prost::encoding::string::encode(99, &"future".to_string(), &mut inner);
        prost::encoding::string::encode(4, &"1.13.11".to_string(), &mut inner);
        let mut outer = Vec::new();
        prost::encoding::bytes::encode(1, &inner, &mut outer);
        match decode(&outer) {
            Some(Response::Join(join)) => assert_eq!(join.server_version, "1.13.11"),
            other => panic!("{other:?}"),
        }
    }

    #[test]
    fn a_response_this_side_has_no_use_for_is_nothing() {
        // Field 10 is `speakers_changed`, not declared here.
        let mut outer = Vec::new();
        prost::encoding::bytes::encode(10, &Vec::<u8>::new(), &mut outer);
        assert_eq!(decode(&outer), None);
    }
}
