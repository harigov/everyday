//! Gemini's `generateContent`, asked to transcribe rather than converse.
//!
//! There is no dedicated transcription endpoint; the audio is one more part
//! in the same request shape every other Gemini call uses, which is why
//! `rig-core`'s Gemini transcriber (built around one fixed "transcribe this"
//! preamble) is the wrong shape for `Hints`, `diarised_json` and a schema
//! this crate controls -- see `docs/plans/meeting-notes.md`'s table.
//!
//! The request carries two parts: a text instruction (transcribe verbatim,
//! label speakers, use seconds from the start of *this* clip, plus whatever
//! `Hints` says about the language and the names likely to come up) and the
//! audio itself, inline as base64 rather than through the separate Files
//! API -- inline is the simpler request for something this small, and it is
//! deleted with the response rather than living on Google's servers until a
//! TTL expires. `generationConfig.responseSchema` asks for
//! `{"segments": [{start, end, speaker, text}]}` directly, so nothing here
//! is scraping prose out of a free-form answer.
//!
//! # Limits
//!
//! Google's own guidance is to reach for the Files API once a request
//! (audio, text and everything else together) passes 20 MB, since inline
//! data travels as part of the JSON body rather than as its own upload. A
//! 16 kHz mono `i16` WAV inflates by about a third once base64-encoded, so
//! this budgets 14 MB of *raw* WAV for the audio part -- comfortably inside
//! 20 MB once the framing and the instruction text are added, without
//! reaching for the Files API this transcriber does not implement. 600
//! seconds (10 minutes) at that rate is roughly 19 MB of 16-bit PCM, so the
//! duration ceiling is the one that binds first for anything but a very
//! quiet chunk.
use std::time::Duration;

use serde::Deserialize;

use crate::error::{CommandError, CommandResult, codes};
use crate::http;

use super::{BoxFuture, Hints, Limits, RawSegment, SpeechChunk, Transcriber};

pub const DEFAULT_BASE_URL: &str = "https://generativelanguage.googleapis.com/v1beta";

/// See this module's doc for where these come from.
const MAX_BYTES: usize = 14 * 1024 * 1024;
const MAX_SECONDS: u32 = 600;

/// A JSON reply of this size would already be an enormous transcript; this
/// is headroom against a broken server, not a figure a real chunk reaches.
const MAX_RESPONSE_BYTES: usize = 8 * 1024 * 1024;

/// Transcription is slow; see `openai.rs`'s identical note on why every
/// request here overrides the shared client's own timeout.
const REQUEST_TIMEOUT: Duration = Duration::from_secs(300);

pub struct GeminiTranscriber {
    pub key: String,
    pub model: String,
    pub base_url: String,
}

impl GeminiTranscriber {
    pub fn new(key: impl Into<String>, model: impl Into<String>) -> Self {
        Self { key: key.into(), model: model.into(), base_url: DEFAULT_BASE_URL.to_string() }
    }

    pub fn with_base_url(mut self, base_url: impl Into<String>) -> Self {
        self.base_url = base_url.into();
        self
    }

    fn url(&self) -> String {
        format!("{}/models/{}:generateContent", self.base_url.trim_end_matches('/'), self.model)
    }

    fn host(&self) -> String {
        reqwest::Url::parse(&self.base_url)
            .ok()
            .and_then(|u| u.host_str().map(str::to_string))
            .unwrap_or_else(|| self.base_url.clone())
    }

    fn build_body(&self, chunk: &SpeechChunk, hints: &Hints) -> serde_json::Value {
        use base64::Engine;
        let data = base64::engine::general_purpose::STANDARD.encode(chunk.wav());
        serde_json::json!({
            "contents": [{
                "parts": [
                    { "text": instructions(hints) },
                    { "inline_data": { "mime_type": "audio/wav", "data": data } },
                ],
            }],
            "generationConfig": {
                "temperature": 0,
                "responseMimeType": "application/json",
                "responseSchema": RESPONSE_SCHEMA.clone(),
            },
        })
    }

    async fn post(&self, body: serde_json::Value) -> CommandResult<reqwest::Response> {
        http::client()?
            .post(self.url())
            .timeout(REQUEST_TIMEOUT)
            .header("x-goog-api-key", &self.key)
            .json(&body)
            .send()
            .await
            .map_err(|e| self.transport_error(&e))
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

    /// Map a non-2xx status. Gemini's error body is read here (unlike
    /// `openai.rs`, which never reads one) only far enough to look for the
    /// `API_KEY_INVALID` reason a `400` can carry alongside plenty of other,
    /// unrelated `400`s -- see this module's tests for the exact shape. The
    /// body is never included in the message itself.
    fn status_error(&self, status: reqwest::StatusCode, body: &[u8]) -> CommandError {
        let host = self.host();
        if status.as_u16() == 400 && body_names_invalid_key(body) {
            return CommandError::new(
                codes::FORBIDDEN,
                "the transcription key was refused".to_string(),
            );
        }
        match status.as_u16() {
            401 | 403 => {
                CommandError::new(codes::FORBIDDEN, "the transcription key was refused".to_string())
            }
            404 => CommandError::new(
                codes::NOT_FOUND,
                format!("{host} does not know the model \"{}\"", self.model),
            ),
            413 => CommandError::new(
                codes::TOO_LARGE,
                "that chunk was too large for the transcription service".to_string(),
            ),
            429 => CommandError::new(
                codes::RATE_LIMITED,
                "the transcription service is rate limiting this key, or its quota is used up"
                    .to_string(),
            ),
            500..=599 => CommandError::new(
                codes::NETWORK,
                format!("{host} is having transcription trouble ({status})"),
            ),
            _ => CommandError::new(codes::NETWORK, format!("{host} answered {status}")),
        }
    }

    async fn do_transcribe(
        &self,
        chunk: &SpeechChunk,
        hints: &Hints,
    ) -> CommandResult<Vec<RawSegment>> {
        let wav_len = chunk.wav().len();
        if wav_len > MAX_BYTES {
            return Err(CommandError::new(
                codes::TOO_LARGE,
                format!(
                    "that chunk is {} MB, over the {} MB this service accepts inline",
                    wav_len / 1_048_576,
                    MAX_BYTES / 1_048_576
                ),
            ));
        }
        let body = self.build_body(chunk, hints);
        let response = self.post(body).await?;
        let status = response.status();
        if !status.is_success() {
            let error_body = http::read_capped(response, MAX_RESPONSE_BYTES, || {
                "the transcription service sent back more than this app will read".to_string()
            })
            .await
            .unwrap_or_default();
            return Err(self.status_error(status, &error_body));
        }
        let body = http::read_capped(response, MAX_RESPONSE_BYTES, || {
            "the transcription service sent back more than this app will read".to_string()
        })
        .await?;
        parse_response(&body, chunk.duration_ms())
    }

    /// Send a second of silence and see whether the service answers at all.
    /// Used for Settings' "Test" button.
    pub async fn probe(&self) -> CommandResult<()> {
        let silence = SpeechChunk {
            samples: vec![0i16; everyday_core::meeting::SAMPLE_RATE as usize],
            offset_ms: 0,
            scope: 0,
        };
        self.do_transcribe(&silence, &Hints::default()).await.map(|_| ())
    }
}

impl Transcriber for GeminiTranscriber {
    fn limits(&self) -> Limits {
        Limits { max_bytes: MAX_BYTES, max_seconds: MAX_SECONDS }
    }

    fn diarises(&self) -> bool {
        true
    }

    fn transcribe<'a>(
        &'a self,
        chunk: &'a SpeechChunk,
        hints: &'a Hints,
    ) -> BoxFuture<'a, CommandResult<Vec<RawSegment>>> {
        Box::pin(async move { self.do_transcribe(chunk, hints).await })
    }
}

/// `true` when a `400` body names Gemini's own `API_KEY_INVALID` reason,
/// rather than one of the many other things a `400` can mean (a schema the
/// model rejected, a part it would not accept). Best-effort: any body that
/// does not even parse is simply "not this".
fn body_names_invalid_key(body: &[u8]) -> bool {
    let Ok(text) = std::str::from_utf8(body) else { return false };
    text.contains("API_KEY_INVALID")
}

fn instructions(hints: &Hints) -> String {
    let mut text = String::from(
        "Transcribe this audio clip verbatim. Label each distinct speaker \"Speaker 1\", \
         \"Speaker 2\" and so on, used consistently for the whole clip. Give every segment's \
         start and end as seconds from the start of this audio clip, not a wall-clock time. \
         Write down only what was actually said -- do not summarise, translate or add anything.",
    );
    if let Some(language) = &hints.language {
        text.push_str(&format!(" The spoken language is {language}."));
    }
    if !hints.prompt.is_empty() {
        text.push_str(&format!(
            " For context, and to help spell names correctly: {}",
            hints.prompt
        ));
    }
    text
}

/// `Type` in Gemini's schema dialect is the `OBJECT` / `ARRAY` / `STRING` /
/// `NUMBER` subset of OpenAPI 3.0's, spelled in capitals.
fn response_schema() -> serde_json::Value {
    serde_json::json!({
        "type": "OBJECT",
        "properties": {
            "segments": {
                "type": "ARRAY",
                "items": {
                    "type": "OBJECT",
                    "properties": {
                        "start": { "type": "NUMBER" },
                        "end": { "type": "NUMBER" },
                        "speaker": { "type": "STRING" },
                        "text": { "type": "STRING" },
                    },
                    "required": ["start", "end", "speaker", "text"],
                },
            },
        },
        "required": ["segments"],
    })
}

// A fresh `serde_json::Value` per call would be no less correct, only
// noisier at every call site; `LazyLock` builds it once.
static RESPONSE_SCHEMA: std::sync::LazyLock<serde_json::Value> =
    std::sync::LazyLock::new(response_schema);

// ---- response parsing -------------------------------------------------

#[derive(Deserialize)]
struct GenerateContentResponse {
    #[serde(default)]
    candidates: Vec<Candidate>,
}

#[derive(Deserialize)]
struct Candidate {
    content: Option<Content>,
}

#[derive(Deserialize)]
struct Content {
    #[serde(default)]
    parts: Vec<Part>,
}

#[derive(Deserialize)]
struct Part {
    #[serde(default)]
    text: Option<String>,
}

#[derive(Deserialize)]
struct SegmentsPayload {
    #[serde(default)]
    segments: Vec<SegmentJson>,
}

#[derive(Deserialize)]
struct SegmentJson {
    start: TimeValue,
    end: TimeValue,
    #[serde(default)]
    speaker: Option<String>,
    #[serde(default)]
    text: String,
}

/// The schema asks for a number of seconds, but a model is free to ignore a
/// schema's *type* even when it keeps to its shape -- so a "mm:ss" or
/// "hh:mm:ss" string is read the same as the number it should have been,
/// rather than failing the whole chunk over one field.
#[derive(Deserialize)]
#[serde(untagged)]
enum TimeValue {
    Seconds(f64),
    Text(String),
}

impl TimeValue {
    fn to_ms(&self) -> u64 {
        let seconds = match self {
            TimeValue::Seconds(s) => *s,
            TimeValue::Text(s) => parse_clock(s),
        };
        (seconds.max(0.0) * 1000.0).round() as u64
    }
}

/// "12.5", "01:02" or "01:02:03" -- each further `:` multiplies what came
/// before by sixty, the same reading a person gives a stopwatch.
fn parse_clock(text: &str) -> f64 {
    text.split(':').fold(0.0, |acc, part| acc * 60.0 + part.trim().parse::<f64>().unwrap_or(0.0))
}

fn parse_response(body: &[u8], chunk_duration_ms: u64) -> CommandResult<Vec<RawSegment>> {
    let unreadable = |e: serde_json::Error| {
        CommandError::new(
            codes::UNREADABLE,
            format!("the transcription service answered something this app could not read: {e}"),
        )
    };
    let envelope: GenerateContentResponse = serde_json::from_slice(body).map_err(unreadable)?;
    let text = envelope
        .candidates
        .into_iter()
        .find_map(|c| c.content)
        .into_iter()
        .flat_map(|c| c.parts)
        .find_map(|p| p.text)
        .ok_or_else(|| {
            CommandError::new(
                codes::UNREADABLE,
                "the transcription service's answer had no text in it".to_string(),
            )
        })?;
    let payload: SegmentsPayload = serde_json::from_str(&text).map_err(unreadable)?;
    let segments = payload
        .segments
        .into_iter()
        .filter(|s| !s.text.trim().is_empty())
        .map(|s| RawSegment {
            start_ms: s.start.to_ms().min(chunk_duration_ms),
            end_ms: s.end.to_ms().min(chunk_duration_ms),
            text: s.text,
            speaker_hint: s.speaker,
        })
        .collect();
    Ok(segments)
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::extract::State;
    use axum::response::IntoResponse;
    use axum::routing::post;
    use std::sync::{Arc, Mutex};

    fn chunk() -> SpeechChunk {
        SpeechChunk { samples: vec![100i16; 8_000], offset_ms: 0, scope: 0 }
    }

    fn gemini_reply(segments_json: &str) -> serde_json::Value {
        serde_json::json!({
            "candidates": [{
                "content": { "parts": [{ "text": segments_json }] },
            }],
        })
    }

    // ---- request building -------------------------------------------------

    #[test]
    fn the_request_body_carries_the_audio_inline_and_asks_for_the_schema() {
        let t = GeminiTranscriber::new("secret-key", "gemini-2.5-flash");
        let body = t.build_body(&chunk(), &Hints::default());
        assert_eq!(body["generationConfig"]["responseMimeType"], "application/json");
        assert_eq!(body["generationConfig"]["temperature"], 0);
        assert!(body["generationConfig"]["responseSchema"]["properties"]["segments"].is_object());
        let parts = body["contents"][0]["parts"].as_array().unwrap();
        assert_eq!(parts.len(), 2);
        assert!(!parts[1]["inline_data"]["data"].as_str().unwrap().is_empty());
        assert_eq!(parts[1]["inline_data"]["mime_type"], "audio/wav");
    }

    #[test]
    fn hints_are_folded_into_the_instruction_text() {
        let hints = Hints { language: Some("fr".into()), prompt: "Design sync, with Priya".into() };
        let text = instructions(&hints);
        assert!(text.contains("fr"));
        assert!(text.contains("Priya"));
        assert!(text.to_lowercase().contains("speaker"));
    }

    #[test]
    fn the_key_never_appears_in_the_request_body() {
        let t = GeminiTranscriber::new("sk-super-secret", "gemini-2.5-flash");
        let body = t.build_body(&chunk(), &Hints::default());
        assert!(!body.to_string().contains("sk-super-secret"));
    }

    // ---- response parsing ---------------------------------------------

    #[test]
    fn a_well_formed_reply_is_parsed_with_numeric_seconds() {
        let segments = serde_json::json!({
            "segments": [
                {"start": 0.0, "end": 1.2, "speaker": "Speaker 1", "text": "Hi."},
                {"start": 1.2, "end": 1.2, "speaker": "Speaker 1", "text": "  "},
            ]
        })
        .to_string();
        let body = gemini_reply(&segments).to_string();
        let out = parse_response(body.as_bytes(), 5_000).unwrap();
        assert_eq!(out.len(), 1, "the empty-text segment is dropped");
        assert_eq!(out[0].end_ms, 1_200);
        assert_eq!(out[0].speaker_hint.as_deref(), Some("Speaker 1"));
    }

    #[test]
    fn mm_ss_strings_are_tolerated_when_the_model_ignores_the_schema() {
        let segments = serde_json::json!({
            "segments": [
                {"start": "00:01", "end": "01:02", "speaker": "Speaker 2", "text": "Late."},
            ]
        })
        .to_string();
        let body = gemini_reply(&segments).to_string();
        let out = parse_response(body.as_bytes(), 120_000).unwrap();
        assert_eq!(out[0].start_ms, 1_000);
        assert_eq!(out[0].end_ms, 62_000);
    }

    #[test]
    fn a_reply_with_no_text_part_is_reported_rather_than_panicking() {
        let body = serde_json::json!({"candidates": [{"content": {"parts": []}}]}).to_string();
        let err = parse_response(body.as_bytes(), 1_000).unwrap_err();
        assert_eq!(err.code, codes::UNREADABLE);
    }

    #[test]
    fn segments_are_clamped_to_the_chunk_duration() {
        let segments = serde_json::json!({
            "segments": [{"start": 0.0, "end": 999.0, "speaker": "Speaker 1", "text": "Long."}]
        })
        .to_string();
        let body = gemini_reply(&segments).to_string();
        let out = parse_response(body.as_bytes(), 4_000).unwrap();
        assert_eq!(out[0].end_ms, 4_000);
    }

    // ---- integration against a fake server ---------------------------

    #[derive(Clone, Default)]
    struct Captured {
        header: Arc<Mutex<Option<String>>>,
        body: Arc<Mutex<serde_json::Value>>,
    }

    async fn capture(
        State(captured): State<Captured>,
        headers: axum::http::HeaderMap,
        axum::Json(body): axum::Json<serde_json::Value>,
    ) -> impl IntoResponse {
        *captured.header.lock().unwrap() =
            headers.get("x-goog-api-key").and_then(|v| v.to_str().ok()).map(str::to_string);
        *captured.body.lock().unwrap() = body;
        axum::Json(gemini_reply(&serde_json::json!({"segments": []}).to_string()))
    }

    async fn spawn(captured: Captured) -> String {
        let app = axum::Router::new().route("/models/{*rest}", post(capture)).with_state(captured);
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        tokio::spawn(async move {
            axum::serve(listener, app).await.ok();
        });
        format!("http://{addr}")
    }

    #[tokio::test]
    async fn the_key_travels_as_a_header_not_a_query_or_body_field() {
        let captured = Captured::default();
        let base = spawn(captured.clone()).await;
        let t = GeminiTranscriber::new("secret-header-key", "gemini-2.5-flash").with_base_url(base);
        t.do_transcribe(&chunk(), &Hints::default()).await.unwrap();
        assert_eq!(captured.header.lock().unwrap().as_deref(), Some("secret-header-key"));
        assert!(!captured.body.lock().unwrap().to_string().contains("secret-header-key"));
    }

    async fn spawn_status(status: axum::http::StatusCode, body: serde_json::Value) -> String {
        let app = axum::Router::new().route(
            "/models/{*rest}",
            post(move || {
                let body = body.clone();
                async move { (status, axum::Json(body)) }
            }),
        );
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        tokio::spawn(async move {
            axum::serve(listener, app).await.ok();
        });
        format!("http://{addr}")
    }

    #[tokio::test]
    async fn a_400_naming_api_key_invalid_is_reported_as_a_refused_key() {
        let error_body = serde_json::json!({
            "error": { "code": 400, "message": "API key not valid.", "status": "INVALID_ARGUMENT",
                "details": [{"reason": "API_KEY_INVALID"}] }
        });
        let base = spawn_status(axum::http::StatusCode::BAD_REQUEST, error_body).await;
        let t = GeminiTranscriber::new("bad-key", "gemini-2.5-flash").with_base_url(base);
        let err = t.do_transcribe(&chunk(), &Hints::default()).await.unwrap_err();
        assert_eq!(err.code, codes::FORBIDDEN);
        assert!(err.message.contains("refused"));
    }

    #[tokio::test]
    async fn a_plain_400_is_not_mistaken_for_a_refused_key() {
        let error_body = serde_json::json!({
            "error": { "code": 400, "message": "Invalid JSON payload", "status": "INVALID_ARGUMENT" }
        });
        let base = spawn_status(axum::http::StatusCode::BAD_REQUEST, error_body).await;
        let t = GeminiTranscriber::new("a-key", "gemini-2.5-flash").with_base_url(base);
        let err = t.do_transcribe(&chunk(), &Hints::default()).await.unwrap_err();
        assert_ne!(err.code, codes::FORBIDDEN);
    }

    #[tokio::test]
    async fn a_key_never_appears_in_an_error_message() {
        let base = spawn_status(
            axum::http::StatusCode::INTERNAL_SERVER_ERROR,
            serde_json::json!({"error": {"message": "trouble"}}),
        )
        .await;
        let t =
            GeminiTranscriber::new("sk-super-secret-value", "gemini-2.5-flash").with_base_url(base);
        let err = t.do_transcribe(&chunk(), &Hints::default()).await.unwrap_err();
        assert!(!err.message.contains("sk-super-secret-value"));
    }

    #[tokio::test]
    async fn a_connection_that_cannot_be_reached_names_the_host() {
        let t =
            GeminiTranscriber::new("a-key", "gemini-2.5-flash").with_base_url("http://127.0.0.1:1");
        let err = t.do_transcribe(&chunk(), &Hints::default()).await.unwrap_err();
        assert_eq!(err.code, codes::NETWORK);
        assert!(err.message.contains("127.0.0.1"));
    }

    #[tokio::test]
    async fn probe_succeeds_on_an_empty_segments_list() {
        let captured = Captured::default();
        let base = spawn(captured).await;
        let t = GeminiTranscriber::new("a-key", "gemini-2.5-flash").with_base_url(base);
        t.probe().await.unwrap();
    }

    // ---- optional live tests, gated on a real key --------------------

    /// Only runs when `EVERYDAY_TEST_GEMINI_KEY` is set.
    #[tokio::test]
    #[ignore]
    async fn a_real_gemini_key_can_transcribe_a_short_clip() {
        let Ok(key) = std::env::var("EVERYDAY_TEST_GEMINI_KEY") else { return };
        let t = GeminiTranscriber::new(key, "gemini-2.5-flash");
        let chunk = SpeechChunk {
            samples: vec![0i16; everyday_core::meeting::SAMPLE_RATE as usize],
            offset_ms: 0,
            scope: 0,
        };
        t.transcribe(&chunk, &Hints::default()).await.unwrap();
    }
}
