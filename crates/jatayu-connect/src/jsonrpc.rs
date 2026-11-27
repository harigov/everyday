//! JSON-RPC 2.0 messages as MCP uses them: build the ones a client sends,
//! and sort what comes back into a response, a request the server is making
//! of us, or a notification.
//!
//! Internal to the crate in spirit -- nothing outside it needs these shapes.

use serde_json::{Value, json};

use crate::ConnectError;

/// The `error` of a response.
#[derive(Debug, Clone, PartialEq)]
pub struct RpcError {
    pub code: i64,
    pub message: String,
    pub data: Option<Value>,
}

impl From<RpcError> for ConnectError {
    fn from(e: RpcError) -> Self {
        ConnectError::Rpc { code: e.code, message: e.message, data: e.data }
    }
}

/// One message from the server.
#[derive(Debug, Clone, PartialEq)]
pub enum Incoming {
    /// The answer to a request of ours. `id` is `Null` when the server could
    /// not even read the request (a parse error).
    Response { id: Value, outcome: Result<Value, RpcError> },
    /// The server asking something of us: `ping`, `roots/list`.
    Request { id: Value, method: String },
    /// A one-way message, such as `notifications/tools/list_changed`.
    Notification { method: String },
}

/// Sort one decoded message. Batches are the caller's to split first.
pub fn parse(message: Value) -> Result<Incoming, ConnectError> {
    let Value::Object(mut map) = message else {
        return Err(ConnectError::Protocol("a message was not a JSON object".into()));
    };
    let method = map.get("method").and_then(Value::as_str).map(str::to_string);
    let id = map.remove("id").filter(|id| !id.is_null());
    match (method, id) {
        (Some(method), Some(id)) => Ok(Incoming::Request { id, method }),
        (Some(method), None) => Ok(Incoming::Notification { method }),
        (None, id) => {
            let id = id.unwrap_or(Value::Null);
            if let Some(error) = map.remove("error") {
                let code = error.get("code").and_then(Value::as_i64).unwrap_or(-32603);
                let message =
                    error.get("message").and_then(Value::as_str).unwrap_or_default().to_string();
                let data = error.get("data").filter(|d| !d.is_null()).cloned();
                Ok(Incoming::Response { id, outcome: Err(RpcError { code, message, data }) })
            } else if let Some(result) = map.remove("result") {
                Ok(Incoming::Response { id, outcome: Ok(result) })
            } else {
                Err(ConnectError::Protocol(
                    "a message had neither a method, a result nor an error".into(),
                ))
            }
        }
    }
}

/// Whether a response's `id` is the one we sent. A server that could not
/// read the request answers with a null id, which can only be about ours.
pub fn id_matches(id: &Value, expected: i64) -> bool {
    match id {
        Value::Null => true,
        Value::Number(n) => n.as_i64() == Some(expected),
        Value::String(s) => s.parse::<i64>().ok() == Some(expected),
        _ => false,
    }
}

/// A request with an integer id.
pub fn request(id: i64, method: &str, params: Option<Value>) -> Value {
    let mut message = json!({ "jsonrpc": "2.0", "id": id, "method": method });
    if let (Some(params), Some(map)) = (params, message.as_object_mut()) {
        map.insert("params".into(), params);
    }
    message
}

/// A notification: a request nobody answers.
pub fn notification(method: &str, params: Option<Value>) -> Value {
    let mut message = json!({ "jsonrpc": "2.0", "method": method });
    if let (Some(params), Some(map)) = (params, message.as_object_mut()) {
        map.insert("params".into(), params);
    }
    message
}

/// Our answer to a request the server made.
pub fn result_response(id: Value, result: Value) -> Value {
    json!({ "jsonrpc": "2.0", "id": id, "result": result })
}

/// Our refusal of a request the server made.
pub fn error_response(id: Value, code: i64, message: &str) -> Value {
    json!({ "jsonrpc": "2.0", "id": id, "error": { "code": code, "message": message } })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sorts_the_four_kinds() {
        let ok = parse(json!({"jsonrpc":"2.0","id":3,"result":{"a":1}})).unwrap();
        assert_eq!(ok, Incoming::Response { id: json!(3), outcome: Ok(json!({"a":1})) });

        let err = parse(
            json!({"jsonrpc":"2.0","id":4,"error":{"code":-32602,"message":"bad","data":{"x":1}}}),
        )
        .unwrap();
        assert_eq!(
            err,
            Incoming::Response {
                id: json!(4),
                outcome: Err(RpcError {
                    code: -32602,
                    message: "bad".into(),
                    data: Some(json!({"x":1}))
                }),
            }
        );

        let req = parse(json!({"jsonrpc":"2.0","id":"a","method":"ping"})).unwrap();
        assert_eq!(req, Incoming::Request { id: json!("a"), method: "ping".into() });

        let note =
            parse(json!({"jsonrpc":"2.0","method":"notifications/tools/list_changed"})).unwrap();
        assert_eq!(
            note,
            Incoming::Notification { method: "notifications/tools/list_changed".into() }
        );
    }

    #[test]
    fn refuses_what_is_not_a_message() {
        assert!(parse(json!(5)).is_err());
        assert!(parse(json!({"jsonrpc":"2.0","id":1})).is_err());
    }

    #[test]
    fn a_null_id_error_is_still_an_answer() {
        let err =
            parse(json!({"jsonrpc":"2.0","id":null,"error":{"code":-32700,"message":"parse"}}))
                .unwrap();
        let Incoming::Response { id, outcome } = err else { panic!("not a response") };
        assert!(id_matches(&id, 9));
        assert!(outcome.is_err());
    }

    #[test]
    fn ids_match_by_number_or_numeric_string() {
        assert!(id_matches(&json!(7), 7));
        assert!(id_matches(&json!("7"), 7));
        assert!(!id_matches(&json!(8), 7));
        assert!(!id_matches(&json!([7]), 7));
    }

    #[test]
    fn builds_requests_with_and_without_params() {
        assert_eq!(request(1, "ping", None), json!({"jsonrpc":"2.0","id":1,"method":"ping"}));
        assert_eq!(
            request(2, "tools/list", Some(json!({"cursor":"c"}))),
            json!({"jsonrpc":"2.0","id":2,"method":"tools/list","params":{"cursor":"c"}})
        );
        assert_eq!(
            notification("notifications/initialized", None),
            json!({"jsonrpc":"2.0","method":"notifications/initialized"})
        );
    }
}
