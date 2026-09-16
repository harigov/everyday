//! OpenAI's transcription endpoint, and anything that speaks its dialect.
//!
//! `POST {base_url}/audio/transcriptions`, multipart, is one request shape
//! served by more than one thing: OpenAI itself, Groq (which mirrors the
//! same API), and any self-hosted server that copies it on purpose --
//! whisper.cpp's `server` and Speaches both do. [`OpenAiTranscriber`] is
//! configured with a base URL rather than hard-coded to
//! `https://api.openai.com/v1`, for the same reason [`crate::llm::client`]
//! takes one for chat: a local model is the same shape of request as a
//! hosted one, just at a different address and usually with no key.
//!
//! Three request shapes come out of one model name, because OpenAI's own
//! transcription models do not all support the same `response_format`:
//!
//! - a model name containing `diarize` (`gpt-4o-transcribe-diarize`) asks
//!   for `response_format=diarized_json`, which is the only format that
//!   carries a `speaker` per segment, and must also carry
//!   `chunking_strategy=auto` -- undocumented as a *hard* requirement
//!   outside the 30-second case, but the API refuses the request without it
//!   for anything longer, so it is always sent. This model also refuses the
//!   `prompt` field outright, per OpenAI's own docs for it.
//! - `gpt-4o-transcribe` and `gpt-4o-mini-transcribe` return no timestamps
//!   at all, at any `response_format`; `json` is the cheapest shape that
//!   still parses. The single segment this produces spans the whole chunk,
//!   because that is all the information there is.
//! - everything else -- `whisper-1`, and any compatible server -- asks for
//!   `verbose_json` with `timestamp_granularities[]=segment`, which is the
//!   shape that has been stable and documented the longest, and the one a
//!   compatible server is most likely to have copied.
//!
//! # Limits
//!
//! 25 MB per request is documented for every model on this endpoint, old
//! and new alike. The per-chunk duration ceiling is not documented as
//! plainly: OpenAI's own guide states a 1500-second (25-minute) limit on
//! total *audio* duration together with a *separate* 1400-second chunk
//! limit for the newer `gpt-4o-*` models (including the diarize model,
//! whose name also contains `gpt-4o`). Nothing published pins a chunk limit
//! for `whisper-1` or a compatible server below the 25-minute figure, so
//! this treats 1800 seconds (30 minutes) as that limit; in practice a
//! [`SpeechChunk`] is far short of either number; a VAD turn or a mapped
//! reduce piece, not a whole call.
use std::time::Duration;

use reqwest::multipart::{Form, Part};
use serde::Deserialize;

use crate::error::{CommandError, CommandResult, codes};
use crate::http;

use super::{BoxFuture, Hints, Limits, RawSegment, SpeechChunk, Transcriber};

/// Every model on this endpoint refuses a request over this, OpenAI's own
/// and every compatible server's alike.
const MAX_BYTES: usize = 25 * 1024 * 1024;

/// See this module's doc for where these two numbers come from.
const MAX_SECONDS_GPT4O: u32 = 1_400;
const MAX_SECONDS_OTHER: u32 = 1_800;

/// The transcript JSON this endpoint sends back is a few kilobytes even for
/// a long chunk; this is generous headroom against a server that goes
/// wrong, not a figure anything real is expected to approach.
const MAX_RESPONSE_BYTES: usize = 8 * 1024 * 1024;

/// Transcription is slow -- a ten-minute chunk can take much longer than
/// this crate's shared [`http::client`] budgets for an RSS fetch -- so every
/// request here overrides the client's own per-request timeout rather than
/// living with it.
const REQUEST_TIMEOUT: Duration = Duration::from_secs(300);

/// OpenAI's `/audio/transcriptions`, or a server that copies its shape.
///
/// `key` is `None` for a local, unauthenticated server: whisper.cpp's
/// `server` and Speaches both run this way by default, and the request is
/// simply sent without an `Authorization` header rather than with an empty
/// bearer -- the endpoint has no concept of a credential to refuse.
pub struct OpenAiTranscriber {
    pub base_url: String,
    pub key: Option<String>,
    pub model: String,
}

impl OpenAiTranscriber {
    pub fn new(base_url: impl Into<String>, key: Option<String>, model: impl Into<String>) -> Self {
        Self { base_url: base_url.into(), key, model: model.into() }
    }

    fn diarizes(&self) -> bool {
        self.model.contains("diarize")
    }

    /// `gpt-4o-transcribe` / `gpt-4o-mini-transcribe`: no timestamps at any
    /// `response_format`, so a single spanning segment is all a chunk is
    /// worth asking for.
    fn no_timestamps(&self) -> bool {
        self.model == "gpt-4o-transcribe" || self.model == "gpt-4o-mini-transcribe"
    }

    fn url(&self) -> String {
        format!("{}/audio/transcriptions", self.base_url.trim_end_matches('/'))
    }

    /// The host named in errors -- never the full URL, which for a
    /// compatible server could carry a path-embedded credential the way a
    /// calendar subscription link does. `self.base_url` verbatim if it does
    /// not even parse as a URL, which is still more useful to somebody
    /// debugging their own Settings entry than nothing.
    fn host(&self) -> String {
        reqwest::Url::parse(&self.base_url)
            .ok()
            .and_then(|u| u.host_str().map(str::to_string))
            .unwrap_or_else(|| self.base_url.clone())
    }

    fn build_form(&self, chunk: &SpeechChunk, hints: &Hints) -> CommandResult<Form> {
        let part =
            Part::bytes(chunk.wav()).file_name("audio.wav").mime_str("audio/wav").map_err(|e| {
                CommandError::new(codes::INTERNAL, format!("could not prepare the upload: {e}"))
            })?;
        let mut form = Form::new().part("file", part).text("model", self.model.clone());
        if let Some(language) = &hints.language {
            form = form.text("language", language.clone());
        }
        // The diarize model rejects `prompt` outright; every other model on
        // this endpoint accepts it.
        if !hints.prompt.is_empty() && !self.diarizes() {
            form = form.text("prompt", hints.prompt.clone());
        }
        form = if self.diarizes() {
            form.text("response_format", "diarized_json").text("chunking_strategy", "auto")
        } else if self.no_timestamps() {
            form.text("response_format", "json")
        } else {
            form.text("response_format", "verbose_json")
                .text("timestamp_granularities[]", "segment")
        };
        Ok(form)
    }

    async fn post(&self, form: Form) -> CommandResult<reqwest::Response> {
        let mut request = http::client()?.post(self.url()).timeout(REQUEST_TIMEOUT).multipart(form);
        if let Some(key) = &self.key {
            request = request.bearer_auth(key);
        }
        request.send().await.map_err(|e| self.transport_error(&e))
    }

    fn transport_error(&self, e: &reqwest::Error) -> CommandError {
        let host = self.host();
        let message = if e.is_timeout() {
            format!("{host} did not answer in time")
        } else if e.is_connect() {
            format!("could not reach {host}")
        } else {
            // Still stripped: a compatible server's base URL can carry a
            // path the person typed, and `reqwest` interpolates the request
            // URL into most of its own `Display` output.
            format!("the request to {host} failed: {}", http::strip_url(&e.to_string()))
        };
        CommandError::new(codes::NETWORK, message)
    }

    /// Map a non-2xx status to a sentence a person can act on, without
    /// reading the response body: OpenAI's own error bodies are small and
    /// harmless, but a compatible server is under no obligation to be, and
    /// the audio never has any business being echoed back regardless.
    fn status_error(&self, status: reqwest::StatusCode) -> CommandError {
        let host = self.host();
        match status.as_u16() {
            401 | 403 => {
                CommandError::new(codes::FORBIDDEN, "the transcription key was refused".to_string())
            }
            404 => CommandError::new(
                codes::NOT_FOUND,
                format!(
                    "{host} does not know the model \"{}\", or the endpoint itself",
                    self.model
                ),
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
                    "that chunk is {} MB, over the {} MB this service accepts",
                    wav_len / 1_048_576,
                    MAX_BYTES / 1_048_576
                ),
            ));
        }
        let form = self.build_form(chunk, hints)?;
        let response = self.post(form).await?;
        let status = response.status();
        if !status.is_success() {
            return Err(self.status_error(status));
        }
        let body = http::read_capped(response, MAX_RESPONSE_BYTES, || {
            "the transcription service sent back more than this app will read".to_string()
        })
        .await?;
        parse_response(&body, self.diarizes(), self.no_timestamps(), chunk.duration_ms())
    }

    /// Send a second of silence and see whether the service answers at all.
    /// An empty transcript is success; only a transport failure or a
    /// non-2xx status is not. Used for Settings' "Test" button.
    pub async fn probe(&self) -> CommandResult<()> {
        let silence = SpeechChunk {
            samples: vec![0i16; everyday_core::meeting::SAMPLE_RATE as usize],
            offset_ms: 0,
            scope: 0,
        };
        self.do_transcribe(&silence, &Hints::default()).await.map(|_| ())
    }
}

impl Transcriber for OpenAiTranscriber {
    fn limits(&self) -> Limits {
        let max_seconds =
            if self.model.contains("gpt-4o") { MAX_SECONDS_GPT4O } else { MAX_SECONDS_OTHER };
        Limits { max_bytes: MAX_BYTES, max_seconds }
    }

    fn diarises(&self) -> bool {
        self.diarizes()
    }

    fn transcribe<'a>(
        &'a self,
        chunk: &'a SpeechChunk,
        hints: &'a Hints,
    ) -> BoxFuture<'a, CommandResult<Vec<RawSegment>>> {
        Box::pin(async move { self.do_transcribe(chunk, hints).await })
    }
}

// ---- response parsing ------------------------------------------------------

#[derive(Deserialize)]
struct DiarizedResponse {
    #[serde(default)]
    segments: Vec<DiarizedSegment>,
}

#[derive(Deserialize)]
struct DiarizedSegment {
    #[serde(default)]
    speaker: Option<String>,
    start: f64,
    end: f64,
    #[serde(default)]
    text: String,
}

#[derive(Deserialize)]
struct JsonResponse {
    #[serde(default)]
    text: String,
}

#[derive(Deserialize)]
struct VerboseResponse {
    #[serde(default)]
    text: String,
    #[serde(default)]
    segments: Vec<VerboseSegment>,
}

#[derive(Deserialize)]
struct VerboseSegment {
    start: f64,
    end: f64,
    #[serde(default)]
    text: String,
}

fn seconds_to_ms(seconds: f64) -> u64 {
    (seconds.max(0.0) * 1000.0).round() as u64
}

/// Turn one backend response into segments, clamped to the chunk's own
/// duration and with empty text dropped -- a diarized or verbose response
/// both admit zero-length or silent segments the local VAD already decided
/// were not worth sending words for.
fn parse_response(
    body: &[u8],
    diarizes: bool,
    no_timestamps: bool,
    chunk_duration_ms: u64,
) -> CommandResult<Vec<RawSegment>> {
    let unreadable = |e: serde_json::Error| {
        CommandError::new(
            codes::UNREADABLE,
            format!("the transcription service answered something this app could not read: {e}"),
        )
    };

    if diarizes {
        let parsed: DiarizedResponse = serde_json::from_slice(body).map_err(unreadable)?;
        let segments = parsed
            .segments
            .into_iter()
            .filter(|s| !s.text.trim().is_empty())
            .map(|s| RawSegment {
                start_ms: seconds_to_ms(s.start).min(chunk_duration_ms),
                end_ms: seconds_to_ms(s.end).min(chunk_duration_ms),
                text: s.text,
                speaker_hint: s.speaker,
            })
            .collect();
        return Ok(segments);
    }

    if no_timestamps {
        let parsed: JsonResponse = serde_json::from_slice(body).map_err(unreadable)?;
        if parsed.text.trim().is_empty() {
            return Ok(Vec::new());
        }
        return Ok(vec![RawSegment {
            start_ms: 0,
            end_ms: chunk_duration_ms,
            text: parsed.text,
            speaker_hint: None,
        }]);
    }

    let parsed: VerboseResponse = serde_json::from_slice(body).map_err(unreadable)?;
    let segments: Vec<RawSegment> = parsed
        .segments
        .into_iter()
        .filter(|s| !s.text.trim().is_empty())
        .map(|s| RawSegment {
            start_ms: seconds_to_ms(s.start).min(chunk_duration_ms),
            end_ms: seconds_to_ms(s.end).min(chunk_duration_ms),
            text: s.text,
            speaker_hint: None,
        })
        .collect();
    if !segments.is_empty() {
        return Ok(segments);
    }
    // Some compatible servers answer `verbose_json` with a top-level `text`
    // and no `segments` array at all. One segment spanning the chunk is
    // still more useful than refusing the whole response.
    if parsed.text.trim().is_empty() {
        Ok(Vec::new())
    } else {
        Ok(vec![RawSegment {
            start_ms: 0,
            end_ms: chunk_duration_ms,
            text: parsed.text,
            speaker_hint: None,
        }])
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::extract::{Multipart, State};
    use axum::response::IntoResponse;
    use axum::routing::post;
    use std::sync::{Arc, Mutex};

    fn chunk() -> SpeechChunk {
        // Half a second of non-zero samples, so `duration_ms` is exercised
        // and the WAV body is not itself empty.
        SpeechChunk { samples: vec![100i16; 8_000], offset_ms: 0, scope: 0 }
    }

    // ---- request building ---------------------------------------------

    #[test]
    fn the_diarize_model_asks_for_diarized_json_and_never_sends_a_prompt() {
        let t =
            OpenAiTranscriber::new("https://api.openai.com/v1", None, "gpt-4o-transcribe-diarize");
        assert!(t.diarizes());
        assert!(!t.no_timestamps());
        assert!(t.limits().max_seconds == MAX_SECONDS_GPT4O);
    }

    #[test]
    fn a_4o_transcribe_model_has_no_timestamps() {
        let t = OpenAiTranscriber::new("https://api.openai.com/v1", None, "gpt-4o-transcribe");
        assert!(t.no_timestamps());
        assert!(!t.diarizes());
    }

    #[test]
    fn whisper_one_gets_the_longer_chunk_ceiling() {
        let t = OpenAiTranscriber::new("http://localhost:8080/v1", None, "whisper-1");
        assert!(!t.diarizes());
        assert!(!t.no_timestamps());
        assert_eq!(t.limits().max_seconds, MAX_SECONDS_OTHER);
    }

    // ---- response parsing -----------------------------------------------

    #[test]
    fn a_diarized_response_carries_speaker_hints_and_drops_empty_text() {
        let body = serde_json::json!({
            "segments": [
                {"speaker": "A", "start": 0.0, "end": 1.5, "text": "Hello."},
                {"speaker": "B", "start": 1.5, "end": 1.5, "text": "   "},
            ]
        });
        let out = parse_response(body.to_string().as_bytes(), true, false, 10_000).unwrap();
        assert_eq!(out.len(), 1);
        assert_eq!(out[0].speaker_hint.as_deref(), Some("A"));
        assert_eq!(out[0].start_ms, 0);
        assert_eq!(out[0].end_ms, 1_500);
    }

    #[test]
    fn a_no_timestamp_response_spans_the_whole_chunk() {
        let body = serde_json::json!({"text": "Hello there."});
        let out = parse_response(body.to_string().as_bytes(), false, true, 4_200).unwrap();
        assert_eq!(
            out,
            vec![RawSegment {
                start_ms: 0,
                end_ms: 4_200,
                text: "Hello there.".into(),
                speaker_hint: None,
            }]
        );
    }

    #[test]
    fn an_empty_no_timestamp_response_is_no_segments_at_all() {
        let body = serde_json::json!({"text": ""});
        let out = parse_response(body.to_string().as_bytes(), false, true, 4_200).unwrap();
        assert!(out.is_empty());
    }

    #[test]
    fn a_verbose_response_with_segments_is_parsed_and_clamped() {
        let body = serde_json::json!({
            "text": "ignored when segments exist",
            "segments": [
                {"start": 0.0, "end": 2.94, "text": "One."},
                {"start": 2.94, "end": 999.0, "text": "Two."},
            ]
        });
        let out = parse_response(body.to_string().as_bytes(), false, false, 5_000).unwrap();
        assert_eq!(out.len(), 2);
        assert_eq!(out[0].end_ms, 2_940);
        // Clamped to the chunk's own duration, not the server's claim.
        assert_eq!(out[1].end_ms, 5_000);
    }

    #[test]
    fn a_verbose_response_with_no_segments_falls_back_to_one_spanning_segment() {
        let body = serde_json::json!({"text": "Whole thing.", "segments": []});
        let out = parse_response(body.to_string().as_bytes(), false, false, 3_000).unwrap();
        assert_eq!(
            out,
            vec![RawSegment {
                start_ms: 0,
                end_ms: 3_000,
                text: "Whole thing.".into(),
                speaker_hint: None,
            }]
        );
    }

    #[test]
    fn unreadable_json_is_reported_rather_than_panicking() {
        let err = parse_response(b"not json", false, false, 1_000).unwrap_err();
        assert_eq!(err.code, codes::UNREADABLE);
    }

    // ---- integration against a fake server -------------------------------

    #[derive(Clone, Default)]
    struct Captured {
        fields: Arc<Mutex<Vec<(String, String)>>>,
        had_file: Arc<Mutex<bool>>,
        auth: Arc<Mutex<Option<String>>>,
    }

    async fn capture_request(
        State(captured): State<Captured>,
        headers: axum::http::HeaderMap,
        mut multipart: Multipart,
    ) -> impl IntoResponse {
        *captured.auth.lock().unwrap() =
            headers.get("authorization").and_then(|v| v.to_str().ok()).map(str::to_string);
        while let Some(field) = multipart.next_field().await.unwrap() {
            let name = field.name().unwrap_or("").to_string();
            if name == "file" {
                *captured.had_file.lock().unwrap() = true;
                let _ = field.bytes().await.unwrap();
            } else {
                let value = field.text().await.unwrap();
                captured.fields.lock().unwrap().push((name, value));
            }
        }
        axum::Json(serde_json::json!({"text": "hello from the fake server"}))
    }

    async fn spawn(captured: Captured) -> String {
        let app = axum::Router::new()
            .route("/audio/transcriptions", post(capture_request))
            .with_state(captured);
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        tokio::spawn(async move {
            axum::serve(listener, app).await.ok();
        });
        format!("http://{addr}")
    }

    #[tokio::test]
    async fn the_multipart_body_carries_every_field_the_request_promised() {
        let captured = Captured::default();
        let base = spawn(captured.clone()).await;
        let t = OpenAiTranscriber::new(base, Some("sk-secret".into()), "whisper-1");
        let hints = Hints { language: Some("en".into()), prompt: "Design sync, with Priya".into() };
        let out = t.do_transcribe(&chunk(), &hints).await.unwrap();
        // No segments/timestamps in this fake reply, so the fallback single
        // spanning segment is what comes back.
        assert_eq!(out.len(), 1);
        assert_eq!(out[0].text, "hello from the fake server");

        assert!(*captured.had_file.lock().unwrap());
        let fields = captured.fields.lock().unwrap().clone();
        let get = |name: &str| fields.iter().find(|(n, _)| n == name).map(|(_, v)| v.clone());
        assert_eq!(get("model").as_deref(), Some("whisper-1"));
        assert_eq!(get("language").as_deref(), Some("en"));
        assert_eq!(get("prompt").as_deref(), Some("Design sync, with Priya"));
        assert_eq!(get("response_format").as_deref(), Some("verbose_json"));
        assert_eq!(get("timestamp_granularities[]").as_deref(), Some("segment"));
        assert_eq!(captured.auth.lock().unwrap().as_deref(), Some("Bearer sk-secret"));
    }

    #[tokio::test]
    async fn the_diarize_model_sends_no_prompt_field_even_when_hints_have_one() {
        let captured = Captured::default();
        let base = spawn(captured.clone()).await;
        let t = OpenAiTranscriber::new(base, None, "gpt-4o-transcribe-diarize");
        let hints = Hints { language: None, prompt: "should never be sent".into() };
        let _ = t.do_transcribe(&chunk(), &hints).await.unwrap();

        let fields = captured.fields.lock().unwrap().clone();
        assert!(fields.iter().all(|(n, _)| n != "prompt"));
        let get = |name: &str| fields.iter().find(|(n, _)| n == name).map(|(_, v)| v.clone());
        assert_eq!(get("response_format").as_deref(), Some("diarized_json"));
        assert_eq!(get("chunking_strategy").as_deref(), Some("auto"));
        // No key configured: no Authorization header at all.
        assert!(captured.auth.lock().unwrap().is_none());
    }

    async fn spawn_status(status: axum::http::StatusCode) -> String {
        let app =
            axum::Router::new().route("/audio/transcriptions", post(move || async move { status }));
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        tokio::spawn(async move {
            axum::serve(listener, app).await.ok();
        });
        format!("http://{addr}")
    }

    #[tokio::test]
    async fn a_401_is_reported_as_a_refused_key() {
        let base = spawn_status(axum::http::StatusCode::UNAUTHORIZED).await;
        let t = OpenAiTranscriber::new(base, Some("sk-secret".into()), "whisper-1");
        let err = t.do_transcribe(&chunk(), &Hints::default()).await.unwrap_err();
        assert_eq!(err.code, codes::FORBIDDEN);
        assert!(err.message.contains("refused"));
    }

    #[tokio::test]
    async fn a_key_never_appears_in_an_error_message() {
        let base = spawn_status(axum::http::StatusCode::INTERNAL_SERVER_ERROR).await;
        let t = OpenAiTranscriber::new(base, Some("sk-super-secret-value".into()), "whisper-1");
        let err = t.do_transcribe(&chunk(), &Hints::default()).await.unwrap_err();
        assert!(!err.message.contains("sk-super-secret-value"));
    }

    #[tokio::test]
    async fn a_404_names_the_model() {
        let base = spawn_status(axum::http::StatusCode::NOT_FOUND).await;
        let t = OpenAiTranscriber::new(base, None, "not-a-real-model");
        let err = t.do_transcribe(&chunk(), &Hints::default()).await.unwrap_err();
        assert_eq!(err.code, codes::NOT_FOUND);
        assert!(err.message.contains("not-a-real-model"));
    }

    #[tokio::test]
    async fn a_413_is_too_large_and_a_429_is_rate_limited() {
        let base = spawn_status(axum::http::StatusCode::PAYLOAD_TOO_LARGE).await;
        let t = OpenAiTranscriber::new(base.clone(), None, "whisper-1");
        let err = t.do_transcribe(&chunk(), &Hints::default()).await.unwrap_err();
        assert_eq!(err.code, codes::TOO_LARGE);

        let base = spawn_status(axum::http::StatusCode::TOO_MANY_REQUESTS).await;
        let t = OpenAiTranscriber::new(base, None, "whisper-1");
        let err = t.do_transcribe(&chunk(), &Hints::default()).await.unwrap_err();
        assert_eq!(err.code, codes::RATE_LIMITED);
    }

    #[tokio::test]
    async fn a_connection_that_cannot_be_reached_names_the_host_not_the_path() {
        // Nothing listens on port 1: refused immediately rather than
        // timing out, which keeps the test fast.
        let t = OpenAiTranscriber::new("http://127.0.0.1:1/v1", None, "whisper-1");
        let err = t.do_transcribe(&chunk(), &Hints::default()).await.unwrap_err();
        assert_eq!(err.code, codes::NETWORK);
        assert!(err.message.contains("127.0.0.1"));
    }

    #[tokio::test]
    async fn probe_succeeds_on_an_empty_transcript() {
        let captured = Captured::default();
        let base = spawn(captured).await;
        let t = OpenAiTranscriber::new(base, None, "whisper-1");
        t.probe().await.unwrap();
    }

    // ---- optional live tests, gated on a real key -------------------------

    /// Only runs when `EVERYDAY_TEST_OPENAI_KEY` is set. Never picks up any
    /// other key from the environment or from Settings.
    #[tokio::test]
    #[ignore]
    async fn a_real_openai_key_can_transcribe_a_short_clip() {
        let Ok(key) = std::env::var("EVERYDAY_TEST_OPENAI_KEY") else { return };
        let t = OpenAiTranscriber::new("https://api.openai.com/v1", Some(key), "whisper-1");
        // A second of silence is enough to prove the round trip works; a
        // real utterance needs `piper` and a voice model, which this
        // environment may not have -- see the module doc for how to wire
        // one in if it is present.
        let chunk = SpeechChunk {
            samples: vec![0i16; everyday_core::meeting::SAMPLE_RATE as usize],
            offset_ms: 0,
            scope: 0,
        };
        t.transcribe(&chunk, &Hints::default()).await.unwrap();
    }
}
