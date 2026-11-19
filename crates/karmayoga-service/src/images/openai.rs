//! OpenAI's Images API, `POST /v1/images/generations`.

use crate::error::{CommandError, CommandResult, codes};
use crate::http;
use base64::Engine;
use std::time::Duration;

/// A picture is a few megabytes. This is headroom against a server that
/// answers with something that is not one.
const MAX_RESPONSE_BYTES: usize = 32 * 1024 * 1024;

/// OpenAI's own ceiling for a complex prompt is two minutes.
const REQUEST_TIMEOUT: Duration = Duration::from_secs(120);

/// The square the hosted models are asked for. Their API takes this size
/// without further negotiation.
const SIZE: &str = "1024x1024";

pub struct OpenAiPainter {
    base_url: String,
    key: String,
    model: String,
}

impl OpenAiPainter {
    pub fn new(
        base_url: impl Into<String>,
        key: impl Into<String>,
        model: impl Into<String>,
    ) -> Self {
        Self { base_url: base_url.into(), key: key.into(), model: model.into() }
    }

    fn url(&self) -> String {
        format!("{}/images/generations", self.base_url.trim_end_matches('/'))
    }

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

    fn status_error(&self, status: reqwest::StatusCode) -> CommandError {
        let host = self.host();
        match status.as_u16() {
            401 | 403 => CommandError::new(codes::FORBIDDEN, "the image key was refused"),
            404 => CommandError::new(
                codes::NOT_FOUND,
                format!("{host} does not know the model \"{}\"", self.model),
            ),
            400 | 422 => CommandError::new(
                codes::INVALID,
                format!("{host} did not accept the model \"{}\" or the prompt", self.model),
            ),
            429 => CommandError::new(
                codes::RATE_LIMITED,
                "the image service is rate limiting this key, or its quota is used up",
            ),
            500..=599 => CommandError::new(
                codes::NETWORK,
                format!("{host} is having image trouble ({status})"),
            ),
            _ => CommandError::new(codes::NETWORK, format!("{host} answered {status}")),
        }
    }

    pub async fn paint(&self, prompt: &str) -> CommandResult<Vec<u8>> {
        let body = serde_json::json!({
            "model": self.model,
            "prompt": prompt,
            "size": SIZE,
            "n": 1,
            "output_format": "png",
        });
        let response = http::client()?
            .post(self.url())
            .timeout(REQUEST_TIMEOUT)
            .bearer_auth(&self.key)
            .json(&body)
            .send()
            .await
            .map_err(|e| self.transport_error(&e))?;
        let status = response.status();
        if !status.is_success() {
            return Err(self.status_error(status));
        }
        let host = self.host();
        let bytes = http::read_capped(response, MAX_RESPONSE_BYTES, || {
            format!("{host} sent back more than this app will read")
        })
        .await?;
        let parsed: serde_json::Value = serde_json::from_slice(&bytes).map_err(|_| {
            CommandError::new(codes::UNREADABLE, format!("{host} did not send back a picture"))
        })?;
        let b64 = parsed.pointer("/data/0/b64_json").and_then(|v| v.as_str()).ok_or_else(|| {
            CommandError::new(codes::UNREADABLE, format!("{host} did not send back a picture"))
        })?;
        let png = base64::engine::general_purpose::STANDARD.decode(b64).map_err(|_| {
            CommandError::new(codes::UNREADABLE, format!("{host} did not send back a picture"))
        })?;
        if png.is_empty() || !png.starts_with(b"\x89PNG") {
            return Err(CommandError::new(
                codes::UNREADABLE,
                format!("{host} did not send back a picture"),
            ));
        }
        Ok(png)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::extract::State;
    use axum::response::IntoResponse;
    use axum::routing::post;
    use std::sync::{Arc, Mutex};

    const PNG: &str = "iVBORw0KGgoAAAANSUhEUgAAAAEAAAABCAYAAAAfFcSJAAAADUlEQVR42mP8z8BQDwAEhQGAhKmMIQAAAABJRU5ErkJggg==";

    #[derive(Clone, Default)]
    struct Captured {
        body: Arc<Mutex<Option<serde_json::Value>>>,
        auth: Arc<Mutex<Option<String>>>,
    }

    async fn serve(
        State(captured): State<Captured>,
        headers: axum::http::HeaderMap,
        axum::Json(body): axum::Json<serde_json::Value>,
    ) -> impl IntoResponse {
        *captured.auth.lock().unwrap() =
            headers.get("authorization").and_then(|v| v.to_str().ok()).map(str::to_string);
        *captured.body.lock().unwrap() = Some(body);
        axum::Json(serde_json::json!({ "data": [{ "b64_json": PNG }] }))
    }

    async fn spawn(captured: Captured) -> String {
        let app =
            axum::Router::new().route("/v1/images/generations", post(serve)).with_state(captured);
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        tokio::spawn(async move {
            axum::serve(listener, app).await.ok();
        });
        format!("http://{addr}/v1")
    }

    #[tokio::test]
    async fn the_request_names_the_model_and_the_png_comes_back() {
        let captured = Captured::default();
        let base = spawn(captured.clone()).await;
        let painter = OpenAiPainter::new(base, "sk-secret", "gpt-image-2.6-flare");
        let png = painter.paint("a red fox").await.unwrap();
        assert!(png.starts_with(b"\x89PNG"));

        let body = captured.body.lock().unwrap().clone().unwrap();
        assert_eq!(body["model"], "gpt-image-2.6-flare");
        assert_eq!(body["prompt"], "a red fox");
        assert_eq!(body["size"], "1024x1024");
        assert_eq!(body["output_format"], "png");
        assert_eq!(captured.auth.lock().unwrap().as_deref(), Some("Bearer sk-secret"));
    }
}
