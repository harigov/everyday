//! A voice served over HTTP, in OpenAI's dialect.

use super::{Speech, Synthesiser, nothing_to_say};
use crate::error::{CommandError, CommandResult, codes};
use crate::http;
use crate::speech::transcribe::BoxFuture;
use crate::speech::wav;
use std::time::Duration;

/// A sentence or two of speech is a few hundred kilobytes; this is headroom
/// against a server that goes wrong, as the transcriber's is, and it is still
/// minutes of audio at any rate a voice produces.
const MAX_RESPONSE_BYTES: usize = 32 * 1024 * 1024;

/// A long sentence from a voice on a modest server can take a while, but
/// somebody is waiting for it to be said: a minute is long enough for any
/// that is going to answer.
const REQUEST_TIMEOUT: Duration = Duration::from_secs(60);

/// OpenAI's `/audio/speech`, or a server that copies its shape.
///
/// `key` is `None` for a server on this machine, and the request is then sent
/// with no `Authorization` header at all, as the transcriber's is.
pub struct OpenAiSynthesiser {
    base_url: String,
    key: Option<String>,
    model: String,
    voice: String,
    speed: f32,
}

impl OpenAiSynthesiser {
    pub fn new(
        base_url: impl Into<String>,
        key: Option<String>,
        model: impl Into<String>,
        voice: impl Into<String>,
        speed: f32,
    ) -> Self {
        Self { base_url: base_url.into(), key, model: model.into(), voice: voice.into(), speed }
    }

    fn url(&self) -> String {
        format!("{}/audio/speech", self.base_url.trim_end_matches('/'))
    }

    /// The host named in errors -- never the full URL, which for a compatible
    /// server could carry a credential in its path.
    fn host(&self) -> String {
        reqwest::Url::parse(&self.base_url)
            .ok()
            .and_then(|u| u.host_str().map(str::to_string))
            .unwrap_or_else(|| self.base_url.clone())
    }

    fn transport_error(&self, e: &reqwest::Error) -> CommandError {
        let host = self.host();
        let message = if e.is_timeout() {
            format!("{host} did not answer in time")
        } else if e.is_connect() {
            format!("could not reach {host}")
        } else {
            format!("the request to {host} failed: {}", http::strip_url(&e.to_string()))
        };
        CommandError::new(codes::NETWORK, message)
    }

    /// A non-2xx status as a sentence a person can act on. The body is not
    /// read: a compatible server is under no obligation to send anything
    /// harmless, and the words said have no business being echoed back.
    fn status_error(&self, status: reqwest::StatusCode) -> CommandError {
        let host = self.host();
        match status.as_u16() {
            401 | 403 => CommandError::new(codes::FORBIDDEN, "the voice key was refused"),
            404 => CommandError::new(
                codes::NOT_FOUND,
                format!(
                    "{host} does not know the model \"{}\", or the endpoint itself",
                    self.model
                ),
            ),
            400 | 422 => CommandError::new(
                codes::INVALID,
                format!(
                    "{host} did not accept the model \"{}\" or the voice \"{}\"",
                    self.model, self.voice
                ),
            ),
            429 => CommandError::new(
                codes::RATE_LIMITED,
                "the voice service is rate limiting this key, or its quota is used up",
            ),
            500..=599 => CommandError::new(
                codes::NETWORK,
                format!("{host} is having voice trouble ({status})"),
            ),
            _ => CommandError::new(codes::NETWORK, format!("{host} answered {status}")),
        }
    }

    async fn speak(&self, text: &str) -> CommandResult<Speech> {
        if text.trim().is_empty() {
            return Err(nothing_to_say());
        }
        let body = serde_json::json!({
            "model": self.model,
            "input": text,
            "voice": self.voice,
            "speed": self.speed,
            "response_format": "wav",
        });
        let mut request = http::client()?.post(self.url()).timeout(REQUEST_TIMEOUT).json(&body);
        if let Some(key) = &self.key {
            request = request.bearer_auth(key);
        }
        let response = request.send().await.map_err(|e| self.transport_error(&e))?;
        let status = response.status();
        if !status.is_success() {
            return Err(self.status_error(status));
        }
        let host = self.host();
        let audio = http::read_capped(response, MAX_RESPONSE_BYTES, || {
            format!("{host} sent back more audio than this app will read")
        })
        .await?;
        wav::decode(&audio)
            .map_err(|why| CommandError::new(codes::UNREADABLE, format!("{host} {why}")))
    }
}

impl Synthesiser for OpenAiSynthesiser {
    fn synthesise<'a>(&'a self, text: &'a str) -> BoxFuture<'a, CommandResult<Speech>> {
        Box::pin(self.speak(text))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::extract::State;
    use axum::response::IntoResponse;
    use axum::routing::post;
    use std::sync::{Arc, Mutex};

    // ---- against a fake server --------------------------------------------

    #[derive(Clone, Default)]
    struct Captured {
        body: Arc<Mutex<Option<serde_json::Value>>>,
        auth: Arc<Mutex<Option<String>>>,
    }

    #[derive(Clone)]
    struct Fake {
        captured: Captured,
        reply: Arc<Vec<u8>>,
    }

    async fn serve(
        State(fake): State<Fake>,
        headers: axum::http::HeaderMap,
        axum::Json(body): axum::Json<serde_json::Value>,
    ) -> impl IntoResponse {
        *fake.captured.auth.lock().unwrap() =
            headers.get("authorization").and_then(|v| v.to_str().ok()).map(str::to_string);
        *fake.captured.body.lock().unwrap() = Some(body);
        ([(axum::http::header::CONTENT_TYPE, "audio/wav")], fake.reply.to_vec())
    }

    async fn spawn(captured: Captured, reply: Vec<u8>) -> String {
        let app = axum::Router::new()
            .route("/audio/speech", post(serve))
            .with_state(Fake { captured, reply: Arc::new(reply) });
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        tokio::spawn(async move {
            axum::serve(listener, app).await.ok();
        });
        format!("http://{addr}")
    }

    async fn spawn_status(status: axum::http::StatusCode) -> String {
        let app = axum::Router::new().route("/audio/speech", post(move || async move { status }));
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        tokio::spawn(async move {
            axum::serve(listener, app).await.ok();
        });
        format!("http://{addr}")
    }

    fn voice(base: String, key: Option<&str>) -> OpenAiSynthesiser {
        OpenAiSynthesiser::new(base, key.map(str::to_string), "kokoro", "af_heart", 1.25)
    }

    #[tokio::test]
    async fn the_request_carries_what_the_dialect_asks_for_and_the_audio_comes_back() {
        let captured = Captured::default();
        let reply = wav::encode(&[1, 2, 3], 24_000);
        let base = spawn(captured.clone(), reply).await;
        let speech = voice(base, Some("sk-secret")).synthesise("Hello there.").await.unwrap();
        assert_eq!(speech, Speech { samples: vec![1, 2, 3], sample_rate: 24_000 });

        let body = captured.body.lock().unwrap().clone().unwrap();
        assert_eq!(body["model"], "kokoro");
        assert_eq!(body["input"], "Hello there.");
        assert_eq!(body["voice"], "af_heart");
        assert_eq!(body["speed"], 1.25);
        assert_eq!(body["response_format"], "wav");
        assert_eq!(captured.auth.lock().unwrap().as_deref(), Some("Bearer sk-secret"));
    }

    #[tokio::test]
    async fn a_server_on_this_machine_is_sent_no_key() {
        let captured = Captured::default();
        let base = spawn(captured.clone(), wav::encode(&[1], 24_000)).await;
        voice(base, None).synthesise("Hi.").await.unwrap();
        assert!(captured.auth.lock().unwrap().is_none());
    }

    #[tokio::test]
    async fn a_reply_with_no_audio_in_it_is_an_error() {
        let base = spawn(Captured::default(), wav::encode(&[], 24_000)).await;
        let err = voice(base, None).synthesise("Hi.").await.unwrap_err();
        assert_eq!(err.code, codes::UNREADABLE);
        assert!(err.message.contains("127.0.0.1") && err.message.contains("no audio"));
    }

    #[tokio::test]
    async fn a_reply_that_is_not_audio_is_an_error_naming_only_the_host() {
        let base = spawn(Captured::default(), b"<html>login</html>".to_vec()).await;
        let err = voice(base, None).synthesise("Hi.").await.unwrap_err();
        assert_eq!(err.code, codes::UNREADABLE);
        assert!(err.message.contains("127.0.0.1"));
        assert!(!err.message.contains("/audio/speech"), "{}", err.message);
        assert!(!err.message.contains("login"), "{}", err.message);
    }

    #[tokio::test]
    async fn blank_words_are_not_sent_anywhere() {
        let captured = Captured::default();
        let base = spawn(captured.clone(), wav::encode(&[1], 24_000)).await;
        let err = voice(base, None).synthesise("  \n ").await.unwrap_err();
        assert_eq!(err.code, codes::INVALID);
        assert!(captured.body.lock().unwrap().is_none());
    }

    #[tokio::test]
    async fn statuses_become_sentences_and_never_carry_the_key() {
        let cases = [
            (axum::http::StatusCode::UNAUTHORIZED, codes::FORBIDDEN),
            (axum::http::StatusCode::NOT_FOUND, codes::NOT_FOUND),
            (axum::http::StatusCode::UNPROCESSABLE_ENTITY, codes::INVALID),
            (axum::http::StatusCode::TOO_MANY_REQUESTS, codes::RATE_LIMITED),
            (axum::http::StatusCode::INTERNAL_SERVER_ERROR, codes::NETWORK),
        ];
        for (status, code) in cases {
            let base = spawn_status(status).await;
            let err =
                voice(base, Some("sk-super-secret-value")).synthesise("Hi.").await.unwrap_err();
            assert_eq!(err.code, code, "{status}");
            assert!(!err.message.contains("sk-super-secret-value"), "{}", err.message);
        }
    }

    #[tokio::test]
    async fn a_404_names_the_model() {
        let base = spawn_status(axum::http::StatusCode::NOT_FOUND).await;
        let err = voice(base, None).synthesise("Hi.").await.unwrap_err();
        assert!(err.message.contains("kokoro"), "{}", err.message);
    }

    #[tokio::test]
    async fn a_server_that_cannot_be_reached_is_named_by_its_host() {
        let err = voice("http://127.0.0.1:1/v1".into(), None).synthesise("Hi.").await.unwrap_err();
        assert_eq!(err.code, codes::NETWORK);
        assert!(err.message.contains("127.0.0.1"));
    }

    #[test]
    fn the_address_is_the_base_with_the_path_added() {
        let v = voice("http://localhost:8880/v1/".into(), None);
        assert_eq!(v.url(), "http://localhost:8880/v1/audio/speech");
        assert_eq!(v.host(), "localhost");
    }
}
