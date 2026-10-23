//! The Chrome DevTools Protocol, spoken over the pipe Chrome opens with
//! `--remote-debugging-pipe`.
//!
//! A pipe rather than the usual WebSocket on a local port, because a port is
//! a door: any process on this machine -- any *user* on it -- could connect
//! to `127.0.0.1:<port>` and drive the same browser, and a page the browser
//! itself has open could try to. A pipe is two file descriptors that only
//! this process and the browser it started hold. See [`super::launch`].
//!
//! The wire is JSON messages, each ended by a NUL byte. A command is
//! `{id, method, params, sessionId?}`; its answer carries the same `id` and a
//! `result` or an `error`; anything without an `id` is an event. That is the
//! whole protocol this needs, so it is written here in a page rather than
//! depended on: the crates that wrap all of CDP generate tens of thousands of
//! lines of types, and this application uses about twenty methods.
//!
//! Two plain threads move the bytes -- one reading, one writing -- because a
//! pipe is a blocking file descriptor, and a thread blocked on one costs
//! nothing while the browser is idle.

use std::collections::HashMap;
use std::io::{BufRead, BufReader, Read, Write};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use serde_json::{Value, json};
use tokio::sync::{broadcast, oneshot};

/// Longest one command may take. Generous: an accessibility tree of a long
/// page is a big answer, and a navigation is answered only once the server
/// has replied.
const CALL_TIMEOUT: Duration = Duration::from_secs(30);

/// The largest single message read from the browser. A page's accessibility
/// tree runs to a few megabytes; past this something is wrong, and the
/// connection is dropped rather than the memory spent.
const MAX_MESSAGE_BYTES: usize = 64 * 1024 * 1024;

/// Something the browser said without being asked.
#[derive(Debug, Clone)]
pub struct Event {
    pub method: String,
    pub params: Value,
    /// The page session it came from, or `None` for the browser's own.
    pub session: Option<String>,
}

type Waiter = oneshot::Sender<Result<Value, String>>;

/// What the reading thread shares with the client.
struct Shared {
    waiting: Mutex<HashMap<u64, Waiter>>,
    /// Taken (and so dropped) when the pipe closes, which is what tells
    /// every listener there will be no more events.
    events: Mutex<Option<broadcast::Sender<Event>>>,
    closed: AtomicBool,
}

/// One connection to one browser.
pub struct Cdp {
    shared: Arc<Shared>,
    next_id: AtomicU64,
    out: std::sync::mpsc::Sender<Vec<u8>>,
}

impl Cdp {
    /// Speak CDP over `reader` and `writer` -- the browser's end of the
    /// pipe in, and its end out.
    pub fn over(
        reader: impl Read + Send + 'static,
        mut writer: impl Write + Send + 'static,
    ) -> Arc<Self> {
        let (events, _) = broadcast::channel(256);
        let shared = Arc::new(Shared {
            waiting: Mutex::new(HashMap::new()),
            events: Mutex::new(Some(events)),
            closed: AtomicBool::new(false),
        });

        let reading = shared.clone();
        std::thread::Builder::new()
            .name("browser-read".into())
            .spawn(move || read_loop(reader, &reading))
            .expect("a thread can be started");

        let (out, outgoing) = std::sync::mpsc::channel::<Vec<u8>>();
        std::thread::Builder::new()
            .name("browser-write".into())
            .spawn(move || {
                // Ends when every sender is gone -- the client was dropped --
                // or when the browser has gone and a write fails.
                for mut message in outgoing {
                    message.push(0);
                    if writer.write_all(&message).and_then(|()| writer.flush()).is_err() {
                        break;
                    }
                }
            })
            .expect("a thread can be started");

        Arc::new(Self { shared, next_id: AtomicU64::new(1), out })
    }

    /// Send `method` and wait for its answer. `session` is a page session's
    /// id, or `None` for the browser itself.
    pub async fn call(
        &self,
        session: Option<&str>,
        method: &str,
        params: Value,
    ) -> Result<Value, String> {
        if self.shared.closed.load(Ordering::Acquire) {
            return Err("the browser has closed".into());
        }
        let id = self.next_id.fetch_add(1, Ordering::Relaxed);
        let mut message = json!({ "id": id, "method": method, "params": params });
        if let Some(session) = session {
            message["sessionId"] = Value::String(session.to_string());
        }
        let (tx, rx) = oneshot::channel();
        self.shared.waiting.lock().unwrap().insert(id, tx);
        let bytes = serde_json::to_vec(&message).map_err(|e| e.to_string())?;
        if self.out.send(bytes).is_err() {
            self.shared.waiting.lock().unwrap().remove(&id);
            return Err("the browser has closed".into());
        }
        match tokio::time::timeout(CALL_TIMEOUT, rx).await {
            Ok(Ok(answer)) => answer,
            Ok(Err(_)) => Err("the browser has closed".into()),
            Err(_) => {
                self.shared.waiting.lock().unwrap().remove(&id);
                Err(format!("the browser did not answer {method} in time"))
            }
        }
    }

    /// Every event from now on. A receiver that falls too far behind loses
    /// the oldest, which is the right trade for events nobody waits on.
    pub fn events(&self) -> Option<broadcast::Receiver<Event>> {
        self.shared.events.lock().unwrap().as_ref().map(broadcast::Sender::subscribe)
    }

    /// Whether the browser is gone.
    pub fn is_closed(&self) -> bool {
        self.shared.closed.load(Ordering::Acquire)
    }
}

fn read_loop(reader: impl Read, shared: &Shared) {
    let mut reader = BufReader::new(reader);
    let mut buf = Vec::new();
    loop {
        buf.clear();
        match read_message(&mut reader, &mut buf) {
            Ok(true) => {}
            Ok(false) | Err(_) => break,
        }
        let Ok(message) = serde_json::from_slice::<Value>(&buf) else { continue };
        dispatch(shared, message);
    }
    shared.closed.store(true, Ordering::Release);
    // Everybody still waiting hears that nothing is coming, and the event
    // channel closes, which ends whatever was listening to it.
    for (_, waiter) in shared.waiting.lock().unwrap().drain() {
        let _ = waiter.send(Err("the browser has closed".into()));
    }
    shared.events.lock().unwrap().take();
}

/// Read up to the next NUL into `buf`. `Ok(false)` at the end of the pipe.
fn read_message(reader: &mut impl BufRead, buf: &mut Vec<u8>) -> std::io::Result<bool> {
    loop {
        let available = reader.fill_buf()?;
        if available.is_empty() {
            return Ok(false);
        }
        match available.iter().position(|&b| b == 0) {
            Some(end) => {
                buf.extend_from_slice(&available[..end]);
                reader.consume(end + 1);
                return Ok(true);
            }
            None => {
                let len = available.len();
                buf.extend_from_slice(available);
                reader.consume(len);
                if buf.len() > MAX_MESSAGE_BYTES {
                    return Err(std::io::Error::other("message too large"));
                }
            }
        }
    }
}

/// Route one message: an answer to whoever asked, an event to whoever
/// listens.
fn dispatch(shared: &Shared, message: Value) {
    if let Some(id) = message.get("id").and_then(Value::as_u64) {
        let Some(waiter) = shared.waiting.lock().unwrap().remove(&id) else { return };
        let answer = match message.get("error") {
            Some(error) => Err(error
                .get("message")
                .and_then(Value::as_str)
                .unwrap_or("the browser refused that")
                .to_string()),
            None => Ok(message.get("result").cloned().unwrap_or(Value::Null)),
        };
        let _ = waiter.send(answer);
        return;
    }
    let Some(method) = message.get("method").and_then(Value::as_str) else { return };
    let event = Event {
        method: method.to_string(),
        params: message.get("params").cloned().unwrap_or(Value::Null),
        session: message.get("sessionId").and_then(Value::as_str).map(str::to_string),
    };
    if let Some(events) = shared.events.lock().unwrap().as_ref() {
        let _ = events.send(event);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// One end of an in-memory pipe the test plays browser on.
    struct Fake {
        to_client: std::io::PipeWriter,
        from_client: BufReader<std::io::PipeReader>,
    }

    fn connected() -> (Arc<Cdp>, Fake) {
        let (client_reads, to_client) = std::io::pipe().unwrap();
        let (from_client, client_writes) = std::io::pipe().unwrap();
        let cdp = Cdp::over(client_reads, client_writes);
        (cdp, Fake { to_client, from_client: BufReader::new(from_client) })
    }

    impl Fake {
        fn next(&mut self) -> Value {
            let mut buf = Vec::new();
            assert!(read_message(&mut self.from_client, &mut buf).unwrap());
            serde_json::from_slice(&buf).unwrap()
        }
        fn say(&mut self, message: Value) {
            let mut bytes = serde_json::to_vec(&message).unwrap();
            bytes.push(0);
            self.to_client.write_all(&bytes).unwrap();
        }
    }

    #[tokio::test]
    async fn a_call_is_answered_by_its_own_id_and_events_reach_listeners() {
        let (cdp, mut fake) = connected();
        let mut events = cdp.events().unwrap();
        let browser = std::thread::spawn(move || {
            let asked = fake.next();
            assert_eq!(asked["method"], "Page.navigate");
            assert_eq!(asked["sessionId"], "S1");
            // An event first, then the answer, as Chrome does.
            fake.say(json!({ "method": "Page.loadEventFired", "params": {}, "sessionId": "S1" }));
            fake.say(json!({ "id": asked["id"], "result": { "frameId": "F" } }));
            let asked = fake.next();
            fake.say(json!({ "id": asked["id"], "error": { "message": "No node with given id" } }));
            fake
        });
        let answer = cdp.call(Some("S1"), "Page.navigate", json!({ "url": "x" })).await.unwrap();
        assert_eq!(answer["frameId"], "F");
        let event = events.recv().await.unwrap();
        assert_eq!(event.method, "Page.loadEventFired");
        assert_eq!(event.session.as_deref(), Some("S1"));
        let refused = cdp.call(None, "DOM.focus", json!({})).await.unwrap_err();
        assert!(refused.contains("No node"), "{refused}");
        drop(browser.join().unwrap());
    }

    #[tokio::test]
    async fn a_closed_pipe_fails_what_is_waiting_and_what_comes_after() {
        let (cdp, mut fake) = connected();
        let browser = std::thread::spawn(move || {
            let _ = fake.next();
            drop(fake);
        });
        let failed = cdp.call(None, "Browser.getVersion", json!({})).await.unwrap_err();
        assert!(failed.contains("closed"), "{failed}");
        browser.join().unwrap();
        // The reader has seen the end by the time the waiter was failed.
        assert!(cdp.is_closed());
        assert!(cdp.call(None, "Browser.getVersion", json!({})).await.is_err());
    }
}
