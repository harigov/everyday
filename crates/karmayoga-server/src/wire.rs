//! What the router and the client agree on, written once.
//!
//! The `/v1` API has two ends that are built from this workspace together --
//! [`crate::routes`] answers and [`crate::client`] asks -- and until now each
//! wrote down the handshake's shapes, the paths and the header names for itself.
//! Two copies of a wire shape drift in exactly the way nothing catches: a field
//! added on one side that the other never reads, a path mistyped in one place.
//! Here is the single copy.
//!
//! Each shared type derives both directions, and the serde attributes are the
//! union of what the two sides needed: a field the client tolerated missing
//! carries `#[serde(default)]`, which has no effect on what is written. The
//! tests at the foot pin the JSON both ways, so a change to the wire is a test
//! that has to be edited on purpose.

use karmayoga_service::ctx::Scope;
use karmayoga_service::events::{Change, MeetingOffer, Notification};
use serde::{Deserialize, Serialize};

/// The routes, as patterns for the router and builders for the client.
///
/// A name ending in `_ROUTE` is the pattern axum registers, with its
/// `{placeholders}`; a function builds the path a client requests.
pub mod path {
    use std::fmt::Display;

    pub const HELLO: &str = "/v1/hello";
    pub const PAIR: &str = "/v1/pair";
    pub const EVENTS: &str = "/v1/events";
    pub const VOICE: &str = "/v1/voice";
    pub const SCREENTIME_HELLO: &str = "/v1/screentime/hello";
    pub const SCREENTIME_HEARTBEATS: &str = "/v1/screentime/heartbeats";
    pub const HEALTH_PUSH_ROUTE: &str = "/v1/health/hae/{source}";
    pub const CALL_ROUTE: &str = "/v1/call/{name}";
    pub const STREAM_ROUTE: &str = "/v1/stream/{name}";
    pub const BLOB: &str = "/v1/blob";
    pub const BLOB_ROUTE: &str = "/v1/blob/{id}";
    pub const MAIL_BODY_ROUTE: &str = "/v1/mail/body/{id}";
    pub const MAIL_PART_ROUTE: &str = "/v1/mail/part/{message_id}/{identifier}";
    pub const MAIL_IMAGE_ROUTE: &str = "/v1/mail/img/{message_id}/{token}";
    pub const MAIL_IMAGE_LEGACY_ROUTE: &str = "/v1/mail/img/{token}";
    pub const NEWS_BODY_ROUTE: &str = "/v1/news/body/{story}";
    pub const NEWS_IMAGE_ROUTE: &str = "/v1/news/img/{story}/{token}";
    pub const FILE_ROUTE: &str = "/v1/files/file/{id}";
    pub const UPLOAD_ROUTE: &str = "/v1/files/upload/{upload}";

    pub fn call(name: impl Display) -> String {
        format!("/v1/call/{name}")
    }

    pub fn stream(name: impl Display) -> String {
        format!("/v1/stream/{name}")
    }

    pub fn blob(id: impl Display) -> String {
        format!("/v1/blob/{id}")
    }

    pub fn file(id: impl Display) -> String {
        format!("/v1/files/file/{id}")
    }

    /// Without the `?offset=` the caller appends, and with `upload` already
    /// encoded for a path segment.
    pub fn upload(upload: impl Display) -> String {
        format!("/v1/files/upload/{upload}")
    }

    pub fn mail_body(id: impl Display) -> String {
        format!("/v1/mail/body/{id}")
    }

    pub fn mail_part(message_id: impl Display, identifier: impl Display) -> String {
        format!("/v1/mail/part/{message_id}/{identifier}")
    }

    pub fn mail_image(message_id: impl Display, token: impl Display) -> String {
        format!("/v1/mail/img/{message_id}/{token}")
    }

    pub fn news_body(story: impl Display) -> String {
        format!("/v1/news/body/{story}")
    }

    pub fn news_image(story: impl Display, token: impl Display) -> String {
        format!("/v1/news/img/{story}/{token}")
    }
}

/// The header names the two ends use, in the lower case HTTP/2 sends them in.
pub mod headers {
    /// Which build of the command table the sender speaks.
    pub const PROTOCOL: &str = "x-karmayoga-protocol";
    /// The same, under the name a copy from before the rename used.
    pub const LEGACY_PROTOCOL: &str = karmayoga_core::legacy::PROTOCOL_HEADER;
    /// A write's id, which is what makes a retry safe.
    pub const REQUEST: &str = "x-karmayoga-request";
    /// The paired device's own id, so a client is not told about its own writes.
    pub const CLIENT: &str = "x-karmayoga-client";
    /// Present on a browser's mutating requests: a cross-site form cannot send it.
    pub const WEB: &str = "x-karmayoga-web";
}

/// Say which protocol this build speaks, under both names, so a server from
/// before the rename answers with which side to update.
pub fn protocol_headers(request: reqwest::RequestBuilder) -> reqwest::RequestBuilder {
    let protocol = karmayoga_service::PROTOCOL.to_string();
    request.header(headers::PROTOCOL, protocol.clone()).header(headers::LEGACY_PROTOCOL, protocol)
}

/// What a server says about itself before anybody has paired.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Hello {
    pub name: String,
    pub protocol: u32,
    pub fingerprint: String,
    pub locked: bool,
    /// Is there an unused pairing code outstanding? A client that pastes a URL
    /// whose code has expired should be told that rather than "bad code".
    #[serde(default)]
    pub pairing: bool,
}

/// What a client sends to spend a pairing code.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PairRequest {
    pub code: String,
    pub device_name: String,
    #[serde(default)]
    pub scopes: Vec<Scope>,
}

/// What a spent pairing code buys.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Paired {
    pub token: String,
    pub device_id: String,
    /// A client has never read this -- it checked the protocol on [`Hello`]
    /// first -- so a server that left it out is still understood.
    #[serde(default)]
    pub protocol: u32,
}

/// One thing the server has to say, in the shape a client reads.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "camelCase")]
pub enum ServerEvent {
    Notify(Notification),
    Changed(Change),
    LockState { locked: bool },
    MeetingOffer(MeetingOffer),
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn hello_is_camel_case_and_a_missing_pairing_flag_is_false() {
        let hello = Hello {
            name: "desk".into(),
            protocol: 7,
            fingerprint: "ab".into(),
            locked: true,
            pairing: true,
        };
        let wire =
            json!({"name":"desk","protocol":7,"fingerprint":"ab","locked":true,"pairing":true});
        assert_eq!(serde_json::to_value(&hello).unwrap(), wire);
        let back: Hello = serde_json::from_value(wire).unwrap();
        assert!(back.pairing && back.locked && back.protocol == 7);
        let older: Hello = serde_json::from_value(
            json!({"name":"desk","protocol":7,"fingerprint":"ab","locked":false}),
        )
        .unwrap();
        assert!(!older.pairing);
    }

    #[test]
    fn a_pair_request_serialises_as_the_client_always_sent_it() {
        let request = PairRequest { code: "K".into(), device_name: "phone".into(), scopes: vec![] };
        assert_eq!(
            serde_json::to_string(&request).unwrap(),
            r#"{"code":"K","deviceName":"phone","scopes":[]}"#
        );
        let back: PairRequest =
            serde_json::from_value(json!({"code":"K","deviceName":"phone"})).unwrap();
        assert!(back.scopes.is_empty());
    }

    #[test]
    fn paired_is_camel_case_and_tolerates_a_missing_protocol() {
        let paired = Paired { token: "t".into(), device_id: "d".into(), protocol: 3 };
        let wire = json!({"token":"t","deviceId":"d","protocol":3});
        assert_eq!(serde_json::to_value(&paired).unwrap(), wire);
        let back: Paired = serde_json::from_value(wire).unwrap();
        assert_eq!((back.token.as_str(), back.device_id.as_str(), back.protocol), ("t", "d", 3));
        let older: Paired = serde_json::from_value(json!({"token":"t","deviceId":"d"})).unwrap();
        assert_eq!(older.protocol, 0);
    }

    #[test]
    fn an_event_is_tagged_by_type_in_camel_case() {
        let wire = json!({"type":"lockState","locked":true});
        let event: ServerEvent = serde_json::from_value(wire.clone()).unwrap();
        assert!(matches!(event, ServerEvent::LockState { locked: true }));
        assert_eq!(serde_json::to_value(&event).unwrap(), wire);
    }

    #[test]
    fn the_paths_and_their_patterns_agree() {
        assert_eq!(path::call("status"), "/v1/call/status");
        assert_eq!(path::blob("abc"), "/v1/blob/abc");
        assert_eq!(path::mail_part("m", "i"), "/v1/mail/part/m/i");
        assert_eq!(path::news_image("s", "t"), "/v1/news/img/s/t");
        assert_eq!(path::CALL_ROUTE.replace("{name}", "status"), path::call("status"));
        assert_eq!(path::BLOB_ROUTE.replace("{id}", "abc"), path::blob("abc"));
        assert_eq!(headers::PROTOCOL, "x-karmayoga-protocol");
        assert_eq!(headers::LEGACY_PROTOCOL, "x-everyday-protocol");
    }
}
