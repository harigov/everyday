//! A model's plain-text reply, as it is written.
//!
//! [`crate::quick`] asks for one structured answer and waits for all of it.
//! Something read aloud cannot wait: the first sentence should be playing
//! while the model is still writing the rest. This is the same one request --
//! no tools, a system prompt and a user message -- with the reply streamed
//! back as text deltas.
//!
//! # Nothing is written that nobody is waiting for
//!
//! The request runs on a task of its own, feeding a channel that holds only a
//! few deltas. When the reader stops reading, the task stops pulling from the
//! provider, so a listener who stops after a minute of a long article has not
//! made the model write the whole of it, or not much more than what was
//! heard. When the reader drops the stream the task is aborted, which closes
//! the connection.
//!
//! # How long a silence is a failure
//!
//! Until the first thing arrives, as long as [`crate::quick::timeout_for`]
//! gives the model: the same wait every other one-shot request makes of it, and
//! longer when it was told to think at length. After that, the longer of that
//! and [`MIN_GAP`]: a model that has begun writing and then goes quiet is
//! usually thinking, or being slow, rather than gone, and the listener is
//! already hearing something. Reasoning counts as something arriving, so a
//! model that thinks aloud is not cut off for it, but it is never read.

use crate::error::{CommandError, CommandResult, codes};
use futures::stream::{BoxStream, Stream, StreamExt};
use jatayu_core::llm::LLMModelConfig;
use rig_agent::AgentBuilder;
use rig_agent::agent::MultiTurnStreamItem;
use rig_agent::core::client::completion::CompletionClient;
use rig_agent::core::streaming::StreamedAssistantContent;
use rig_agent::prelude::*;
use std::pin::Pin;
use std::task::{Context, Poll};
use std::time::Duration;
use tokio::sync::mpsc;
use tokio::task::JoinHandle;

/// The least a model that has begun writing may be quiet for before it is
/// given up on.
pub const MIN_GAP: Duration = Duration::from_secs(30);

/// How many deltas wait for the reader before the request stops being read
/// from. Small on purpose: see the module docs.
const BUFFER: usize = 4;

/// Stream `model`'s plain-text reply to `user` under `system`, as deltas in
/// the order they were written. The stream ends when the reply does; an
/// `Err` (the model could not be reached, it stopped answering for longer
/// than [`crate::quick::timeout_for`] allows between deltas) is the last item.
///
/// Must be called on a Tokio runtime, which is where the request is run.
pub fn stream_text(
    client: crate::llm::Client,
    model: &LLMModelConfig,
    system: &str,
    user: &str,
) -> BoxStream<'static, CommandResult<String>> {
    let builder = AgentBuilder::new(client.completion_model(&model.model)).preamble(system);
    let agent = crate::llm::configure(builder, model).build();
    let user = user.to_string();
    let model_config = model.clone();
    let first = crate::quick::timeout_for(model);
    let gap = first.max(MIN_GAP);

    // The request is made inside the task, so that it ends with the task.
    let source = futures::stream::once(async move { agent.stream_prompt(user.as_str()).await })
        .flatten()
        .filter_map(move |item| {
            let model = model_config.clone();
            async move {
                match item {
                    Ok(MultiTurnStreamItem::StreamAssistantItem(
                        StreamedAssistantContent::Text(text),
                    )) => Some(Ok(Arrived::Text(text.text))),
                    Ok(MultiTurnStreamItem::StreamAssistantItem(
                        StreamedAssistantContent::Reasoning { .. }
                        | StreamedAssistantContent::ReasoningDelta { .. },
                    )) => Some(Ok(Arrived::Thinking)),
                    Ok(_) => None,
                    Err(e) => Some(Err(crate::llm::friendly_for(&e.to_string(), &model))),
                }
            }
        });
    // And the task is started only when the stream is first read, so that a
    // stream made and then dropped unread -- a start that was replaced, a
    // chunk nobody reached -- never sends anything.
    futures::stream::once(async move { feed(Box::pin(source), first, gap) }).flatten().boxed()
}

/// What a provider's stream amounts to, for the clock.
enum Arrived {
    Text(String),
    /// The model is working, and nothing is to be said of it.
    Thinking,
}

/// A stream of the deltas of a task's work, which stops the task when it is
/// dropped.
struct Feed {
    deltas: mpsc::Receiver<CommandResult<String>>,
    task: JoinHandle<()>,
}

impl Stream for Feed {
    type Item = CommandResult<String>;

    fn poll_next(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Option<Self::Item>> {
        self.deltas.poll_recv(cx)
    }
}

impl Drop for Feed {
    fn drop(&mut self) {
        self.task.abort();
    }
}

/// Run `source` on a task, answering its text as it arrives. `first` is how
/// long to wait for the first thing from it, and `gap` how long between
/// the things after. Split out of [`stream_text`] so that a stream that
/// stalls, fails or is dropped can be tried without a provider.
fn feed<S>(source: S, first: Duration, gap: Duration) -> BoxStream<'static, CommandResult<String>>
where
    S: Stream<Item = Result<Arrived, String>> + Send + Unpin + 'static,
{
    let (tx, deltas) = mpsc::channel(BUFFER);
    let task = tokio::spawn(async move {
        let mut source = source;
        let mut wait = first;
        loop {
            // Only the wait on the provider is timed: time spent holding a
            // delta for a reader who has not asked for it is not the model's.
            let item = match tokio::time::timeout(wait, source.next()).await {
                Ok(item) => item,
                Err(_) => {
                    let why = if first > crate::quick::TIMEOUT && wait == first {
                        "; a lower reasoning effort in Settings → Models answers sooner"
                    } else {
                        ""
                    };
                    let message =
                        format!("the model took longer than {}s to answer{why}", wait.as_secs());
                    let _ = tx.send(Err(CommandError::new(codes::QUICK, message))).await;
                    return;
                }
            };
            wait = gap;
            match item {
                None => return,
                Some(Ok(Arrived::Text(text))) => {
                    if !text.is_empty() && tx.send(Ok(text)).await.is_err() {
                        return;
                    }
                }
                Some(Ok(Arrived::Thinking)) => {}
                Some(Err(message)) => {
                    let _ = tx.send(Err(CommandError::new(codes::QUICK, message))).await;
                    return;
                }
            }
        }
    });
    Box::pin(Feed { deltas, task })
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Arc;
    use std::sync::atomic::{AtomicUsize, Ordering};

    fn text(s: &str) -> Result<Arrived, String> {
        Ok(Arrived::Text(s.to_string()))
    }

    const FIRST: Duration = Duration::from_secs(10);
    const GAP: Duration = Duration::from_secs(30);

    #[tokio::test]
    async fn text_arrives_in_order_and_the_stream_ends_with_the_reply() {
        let source = futures::stream::iter(vec![text("Hello, "), text(""), text("world.")]);
        let got: Vec<_> = feed(source, FIRST, GAP).collect().await;
        let got: Vec<String> = got.into_iter().map(|d| d.unwrap()).collect();
        assert_eq!(got, vec!["Hello, ", "world."]);
    }

    #[tokio::test]
    async fn thinking_is_not_read() {
        let source = futures::stream::iter(vec![
            Ok(Arrived::Thinking),
            text("Answer."),
            Ok(Arrived::Thinking),
        ]);
        let got: Vec<_> = feed(source, FIRST, GAP).collect().await;
        assert_eq!(got.len(), 1);
        assert_eq!(got[0].as_ref().unwrap(), "Answer.");
    }

    #[tokio::test]
    async fn an_error_is_the_last_item_and_says_what_the_provider_said() {
        let source =
            futures::stream::iter(vec![text("Half "), Err("no route to host".to_string())]);
        let mut got = feed(source, FIRST, GAP);
        assert_eq!(got.next().await.unwrap().unwrap(), "Half ");
        let error = got.next().await.unwrap().unwrap_err();
        assert_eq!(error.code, codes::QUICK);
        assert_eq!(error.message, "no route to host");
        assert!(got.next().await.is_none());
    }

    #[tokio::test(start_paused = true)]
    async fn a_model_that_never_answers_is_given_up_on_after_the_first_wait() {
        let source = futures::stream::pending::<Result<Arrived, String>>();
        let mut got = feed(source, FIRST, GAP);
        let error = got.next().await.unwrap().unwrap_err();
        assert_eq!(error.code, codes::QUICK);
        assert!(error.message.contains("longer than 10s"), "{}", error.message);
        assert!(got.next().await.is_none());
    }

    #[tokio::test(start_paused = true)]
    async fn a_model_that_goes_quiet_after_beginning_gets_the_longer_gap() {
        // One delta, and then nothing at all.
        let source = futures::stream::iter(vec![text("Start.")]).chain(futures::stream::pending());
        let mut got = feed(source, FIRST, GAP);
        assert_eq!(got.next().await.unwrap().unwrap(), "Start.");
        let error = got.next().await.unwrap().unwrap_err();
        assert!(error.message.contains("longer than 30s"), "{}", error.message);
    }

    #[tokio::test(start_paused = true)]
    async fn time_spent_waiting_for_the_reader_is_not_the_models() {
        let source = futures::stream::iter(vec![text("a"), text("b"), text("c")]);
        let mut got = feed(source, Duration::from_secs(1), Duration::from_secs(1));
        // Longer than both waits, with the reader not reading.
        tokio::time::sleep(Duration::from_secs(60)).await;
        let all: Vec<_> = got.by_ref().collect().await;
        assert!(all.iter().all(|d| d.is_ok()), "{all:?}");
        assert_eq!(all.len(), 3);
    }

    #[tokio::test(start_paused = true)]
    async fn a_reader_that_stops_reading_stops_the_request_being_read_from() {
        let pulled = Arc::new(AtomicUsize::new(0));
        let counter = pulled.clone();
        let source = futures::stream::iter(0..1000).map(move |i| {
            counter.fetch_add(1, Ordering::SeqCst);
            text(&format!("{i} "))
        });
        let mut got = feed(Box::pin(source), FIRST, GAP);
        assert_eq!(got.next().await.unwrap().unwrap(), "0 ");
        tokio::time::sleep(Duration::from_secs(5)).await;
        let n = pulled.load(Ordering::SeqCst);
        assert!(n <= BUFFER + 3, "{n} deltas were pulled for a reader that had read one");
    }

    #[tokio::test]
    async fn dropping_the_stream_stops_the_request() {
        struct Dropped(Arc<AtomicUsize>);
        impl Drop for Dropped {
            fn drop(&mut self) {
                self.0.fetch_add(1, Ordering::SeqCst);
            }
        }
        let dropped = Arc::new(AtomicUsize::new(0));
        let guard = Dropped(dropped.clone());
        let source = futures::stream::pending::<Result<Arrived, String>>().map(move |x| {
            let _ = &guard;
            x
        });
        let got = feed(Box::pin(source), FIRST, GAP);
        tokio::task::yield_now().await;
        assert_eq!(dropped.load(Ordering::SeqCst), 0);
        drop(got);
        for _ in 0..50 {
            if dropped.load(Ordering::SeqCst) == 1 {
                break;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
        assert_eq!(dropped.load(Ordering::SeqCst), 1, "the request was not abandoned");
    }
}
