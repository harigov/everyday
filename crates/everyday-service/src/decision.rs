//! Asking a decision model a question.
//!
//! A decision model -- Jev, from TypeSafe, directly or through OpenRouter;
//! OpenAI's Decisions API -- does not write. It is handed some content and a
//! question with a fixed set of answers, and answers with one of them and how
//! sure it is. See [`everyday_core::llm::DecisionApi`] for the two request
//! shapes and where each is served.
//!
//! One question per request, of one kind -- a choice -- because that is the
//! one thing this application asks one: which of somebody's categories a
//! message belongs in. Both APIs take several questions about one piece of
//! content per request; neither takes several pieces of content, so sorting
//! twenty messages is twenty requests, which is what they are priced and
//! timed for (tenths of a second, a few hundred tokens each, output free).
//!
//! Over [`crate::http::client`], not `rig`: there is no chat, no stream and
//! no tool call here, just a JSON body posted to an address the person
//! chose, which is what that client is for.

use everyday_core::llm::{DecisionAccess, DecisionApi};
use serde_json::{Map, Value, json};

use crate::error::{CommandError, CommandResult, codes};

/// How long one answer may take. They are meant to take well under a
/// second; this is for a server that has stopped answering, not a slow one.
const DECISION_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(20);
/// The most of a response read. An answer is a few hundred bytes.
const MAX_RESPONSE_BYTES: usize = 256 * 1024;
/// The name the one question is asked under. Neither model sees it.
const QUESTION: &str = "category";

/// One of the answers a choice question offers.
#[derive(Debug, Clone)]
pub struct ChoiceOption {
    /// What it is called -- the answer the model gives back.
    pub name: String,
    /// When it applies, in the asker's words. Empty is fine: the model then
    /// goes by the name alone.
    pub description: String,
}

/// The model's answer: one of the options' names, and how sure it was if
/// it said.
#[derive(Debug, Clone, PartialEq)]
pub struct Choice {
    pub name: String,
    pub confidence: Option<f64>,
}

/// What the content is, twice: as fields, for TypeSafe's `state` (which takes
/// an object and is documented to do better with one), and as text, for
/// OpenAI's `input`.
pub struct Content {
    pub fields: Map<String, Value>,
    pub text: String,
}

/// Ask `access`'s model which of `options` fits `content`.
pub async fn choose(
    access: &DecisionAccess,
    content: &Content,
    instructions: &str,
    options: &[ChoiceOption],
) -> CommandResult<Choice> {
    let body = request_body(access, content, instructions, options);
    let mut request =
        crate::http::client()?.post(access.config.endpoint()).timeout(DECISION_TIMEOUT).json(&body);
    if let Some(key) = &access.key {
        request = request.bearer_auth(key);
    }
    let response = request.send().await.map_err(|e| crate::http::transport(&e))?;
    let status = response.status();
    let bytes = crate::http::read_capped(response, MAX_RESPONSE_BYTES, || {
        "the decision model's answer was too large to read".into()
    })
    .await?;
    if !status.is_success() {
        return Err(refused(status.as_u16(), &bytes));
    }
    let answer: Value = serde_json::from_slice(&bytes).map_err(|e| {
        CommandError::new(codes::PROVIDER, format!("the decision model's answer was not JSON: {e}"))
    })?;
    parse_answer(access.config.api, &answer)
}

/// The request, in whichever shape the configured API takes.
pub fn request_body(
    access: &DecisionAccess,
    content: &Content,
    instructions: &str,
    options: &[ChoiceOption],
) -> Value {
    let model = access.config.model.trim();
    match access.config.api {
        DecisionApi::TypeSafe => {
            // A choice without a description "is interpreted by its name
            // alone" -- sent as null rather than an empty string, which
            // would read as a description saying nothing.
            let criteria: Map<String, Value> = options
                .iter()
                .map(|o| {
                    let d = o.description.trim();
                    (o.name.trim().to_string(), if d.is_empty() { Value::Null } else { d.into() })
                })
                .collect();
            json!({
                "model": model,
                "state": Value::Object(content.fields.clone()),
                "questions": {
                    QUESTION: {
                        "type": "choice",
                        "instructions": instructions,
                        "criteria": criteria,
                    }
                },
            })
        }
        DecisionApi::OpenAi => {
            let choices: Vec<Value> = options
                .iter()
                .map(|o| {
                    let d = o.description.trim();
                    let mut c = json!({ "value": o.name.trim() });
                    if !d.is_empty() {
                        c["description"] = d.into();
                    }
                    c
                })
                .collect();
            json!({
                "model": model,
                "input": content.text,
                "questions": [{
                    "type": "choice",
                    "name": QUESTION,
                    "instructions": instructions,
                    "choices": choices,
                }],
            })
        }
    }
}

/// The one answer, out of either API's response.
///
/// TypeSafe keys answers by question name; OpenAI lists them, each naming
/// its question, and may answer one with a refusal instead.
pub fn parse_answer(api: DecisionApi, response: &Value) -> CommandResult<Choice> {
    let answer = match api {
        DecisionApi::TypeSafe => response.get("answers").and_then(|a| a.get(QUESTION)),
        DecisionApi::OpenAi => response.get("answers").and_then(Value::as_array).and_then(|a| {
            a.iter().find(|x| x.get("name").and_then(Value::as_str) == Some(QUESTION))
        }),
    };
    let Some(answer) = answer else {
        return Err(CommandError::new(codes::PROVIDER, "the decision model did not answer"));
    };
    if answer.get("type").and_then(Value::as_str) == Some("refusal") {
        return Err(CommandError::new(codes::INVALID, "the decision model declined to answer"));
    }
    let Some(name) = answer.get("choice").and_then(Value::as_str) else {
        return Err(CommandError::new(
            codes::PROVIDER,
            "the decision model's answer named no choice",
        ));
    };
    Ok(Choice {
        name: name.to_string(),
        confidence: answer.get("confidence").and_then(Value::as_f64),
    })
}

/// Whether `e` is about the one request it answered -- the model declined
/// it, or found something in that message's content it would not take --
/// rather than about the model: a refused key, a server down or busy. Asking
/// again about the same message would get the same answer, so a caller
/// moves past it; a failure of the model is worth waiting out instead.
pub fn about_this_request(e: &CommandError) -> bool {
    e.code == codes::INVALID
}

/// A refusal from the server, as a sentence. The error body's own message
/// is passed on, shortened and with any address taken out, because "422"
/// alone says nothing about which field was wrong.
fn refused(status: u16, body: &[u8]) -> CommandError {
    let detail = serde_json::from_slice::<Value>(body)
        .ok()
        .and_then(|v| {
            v.pointer("/error/message")
                .or_else(|| v.get("message"))
                .or_else(|| v.get("detail"))
                .map(|d| match d {
                    Value::String(s) => s.clone(),
                    other => other.to_string(),
                })
        })
        .unwrap_or_else(|| String::from_utf8_lossy(body).into_owned());
    let detail: String = crate::http::strip_url(detail.trim()).chars().take(300).collect();
    let (code, what) = match status {
        401 | 403 => (codes::PROVIDER, "the decision model's key was refused".to_string()),
        404 => (codes::PROVIDER, "nothing answers at the decision model's address".to_string()),
        429 | 529 => (codes::RATE_LIMITED, "the decision model is busy".to_string()),
        // The request itself -- something about this one message's content
        // -- rather than the model: see `about_this_request`.
        400 | 413 | 422 => {
            (codes::INVALID, format!("the decision model refused the request ({status})"))
        }
        _ => (codes::PROVIDER, format!("the decision model refused the request ({status})")),
    };
    let message = if detail.is_empty() { what } else { format!("{what}: {detail}") };
    CommandError::new(code, message)
}

#[cfg(test)]
mod tests {
    use super::*;
    use everyday_core::llm::DecisionConfig;

    fn access(api: DecisionApi) -> DecisionAccess {
        DecisionAccess {
            config: DecisionConfig { api, url: None, model: api.default_model().into() },
            key: Some("k".into()),
            acknowledgement: String::new(),
        }
    }

    fn content() -> Content {
        let mut fields = Map::new();
        fields.insert("subject".into(), "Your order has shipped".into());
        Content { fields, text: "subject: Your order has shipped".into() }
    }

    fn options() -> Vec<ChoiceOption> {
        vec![
            ChoiceOption { name: "Receipts".into(), description: "Orders and shipping.".into() },
            ChoiceOption { name: "Other".into(), description: " ".into() },
        ]
    }

    /// The shape TypeSafe's reference gives for `POST /v1/systemone`, which
    /// OpenRouter's `/api/alpha/decisions` takes as it is.
    #[test]
    fn typesafe_is_asked_with_a_state_and_named_questions() {
        let body = request_body(&access(DecisionApi::TypeSafe), &content(), "Which?", &options());
        assert_eq!(
            body,
            json!({
                "model": "jev-latest",
                "state": { "subject": "Your order has shipped" },
                "questions": {
                    "category": {
                        "type": "choice",
                        "instructions": "Which?",
                        "criteria": { "Receipts": "Orders and shipping.", "Other": null },
                    }
                }
            })
        );
    }

    /// The shape OpenAI's Decisions guide gives for a choice question.
    #[test]
    fn openai_is_asked_with_an_input_and_a_list_of_questions() {
        let body = request_body(&access(DecisionApi::OpenAi), &content(), "Which?", &options());
        assert_eq!(
            body,
            json!({
                "model": "gpt-6-luna",
                "input": "subject: Your order has shipped",
                "questions": [{
                    "type": "choice",
                    "name": "category",
                    "instructions": "Which?",
                    "choices": [
                        { "value": "Receipts", "description": "Orders and shipping." },
                        { "value": "Other" },
                    ],
                }]
            })
        );
    }

    #[test]
    fn each_api_answers_in_its_own_shape() {
        let typesafe = json!({
            "model": "jev-latest",
            "answers": { "category": {
                "type": "choice", "choice": "Receipts", "confidence": 0.7,
                "probabilities": { "Receipts": 0.8, "Other": 0.2 }
            }},
            "usage": { "input_tokens": 120, "output_tokens": 12 }
        });
        assert_eq!(
            parse_answer(DecisionApi::TypeSafe, &typesafe).unwrap(),
            Choice { name: "Receipts".into(), confidence: Some(0.7) }
        );

        let openai = json!({ "answers": [{
            "type": "choice", "name": "category", "choice": "Other",
            "probabilities": [{ "value": "Other", "probability": 0.9 }], "confidence": 0.93
        }]});
        assert_eq!(
            parse_answer(DecisionApi::OpenAi, &openai).unwrap(),
            Choice { name: "Other".into(), confidence: Some(0.93) }
        );

        let refusal = json!({ "answers": [{ "type": "refusal", "name": "category" }] });
        let e = parse_answer(DecisionApi::OpenAi, &refusal).unwrap_err();
        assert!(about_this_request(&e), "a refusal is about this message, not the model");
        assert!(parse_answer(DecisionApi::TypeSafe, &json!({ "answers": {} })).is_err());
    }

    #[test]
    fn a_refusal_says_why_without_an_address() {
        let e = refused(422, br#"{"detail":[{"msg":"Field required","loc":["body","state"]}]}"#);
        assert!(e.message.contains("refused the request (422)"), "{}", e.message);
        assert!(e.message.contains("Field required"), "{}", e.message);
        let e = refused(401, br#"{"error":{"message":"bad key at https://api.example.com/x"}}"#);
        assert!(e.message.contains("key was refused"), "{}", e.message);
        assert!(!e.message.contains("https://"), "{}", e.message);
        assert_eq!(refused(429, b"").code, codes::RATE_LIMITED);
        assert!(about_this_request(&refused(422, b"")));
        assert!(!about_this_request(&refused(401, b"")), "a refused key is the model's");
        assert!(!about_this_request(&refused(500, b"")));
    }
}
