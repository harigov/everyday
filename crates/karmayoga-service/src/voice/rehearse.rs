//! The session's own few words, made into a voice before they are wanted.
//!
//! A phrase said while the assistant works ([`super::phrases`]) is worth
//! saying only at once: "Let me see" a second late, just ahead of the answer,
//! is worse than nothing. A voice takes from a tenth of a second (Phonon) to
//! most of one (a voice over the network) to make a phrase, so the session
//! has the next one it may say made while it is only listening, and
//! [`Rehearsed`] hands that back at once when it is asked for. Anything else
//! goes straight to the voice.
//!
//! Asked for while its rehearsal is still under way, a phrase waits for that
//! one making of it rather than starting another.

use crate::error::CommandResult;
use crate::speech::synth::{Speech, Synthesiser};
use crate::speech::transcribe::BoxFuture;
use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use tokio::sync::OnceCell;

/// A voice that keeps the phrases it was asked to rehearse.
pub struct Rehearsed {
    voice: Arc<dyn Synthesiser>,
    /// Every phrase rehearsed, made or being made. A session's phrases are a
    /// few dozen short ones at most, so nothing is ever let go.
    made: Mutex<HashMap<String, Arc<OnceCell<Speech>>>>,
}

impl Rehearsed {
    pub fn new(voice: Arc<dyn Synthesiser>) -> Arc<Self> {
        Arc::new(Self { voice, made: Mutex::new(HashMap::new()) })
    }

    /// Have `text` made now, on a task of its own, so that asking for it is
    /// instant. Must be called on a Tokio runtime. A rehearsal that fails is
    /// forgotten: the phrase is made again when it is said, and that one
    /// reports what went wrong.
    pub fn rehearse(self: &Arc<Self>, text: String) {
        let cell = {
            let mut made = self.made.lock().unwrap_or_else(|e| e.into_inner());
            made.entry(text.clone()).or_default().clone()
        };
        if cell.initialized() {
            return;
        }
        let this = self.clone();
        tokio::spawn(async move {
            let _ = cell.get_or_try_init(|| this.voice.synthesise(&text)).await;
        });
    }

    fn rehearsed(&self, text: &str) -> Option<Arc<OnceCell<Speech>>> {
        self.made.lock().unwrap_or_else(|e| e.into_inner()).get(text).cloned()
    }
}

impl Synthesiser for Rehearsed {
    fn synthesise<'a>(&'a self, text: &'a str) -> BoxFuture<'a, CommandResult<Speech>> {
        match self.rehearsed(text) {
            Some(cell) => Box::pin(async move {
                cell.get_or_try_init(|| self.voice.synthesise(text)).await.cloned()
            }),
            None => self.voice.synthesise(text),
        }
    }

    /// The voice's: a phrase handed back from rehearsal took no time, which
    /// says nothing about how fast the voice is.
    fn rate(&self) -> Option<f64> {
        self.voice.rate()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::error::{CommandError, codes};
    use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
    use std::time::Duration;

    /// Counts what it is asked to make, and takes a while over each.
    #[derive(Default)]
    struct Slow {
        made: AtomicUsize,
        fails: AtomicBool,
    }

    impl Synthesiser for Slow {
        fn synthesise<'a>(&'a self, text: &'a str) -> BoxFuture<'a, CommandResult<Speech>> {
            self.made.fetch_add(1, Ordering::SeqCst);
            let fails = self.fails.load(Ordering::SeqCst);
            let len = text.len();
            Box::pin(async move {
                tokio::time::sleep(Duration::from_millis(50)).await;
                if fails {
                    return Err(CommandError::new(codes::INVALID, "no voice"));
                }
                Ok(Speech { samples: vec![1; len], sample_rate: 16_000 })
            })
        }

        fn rate(&self) -> Option<f64> {
            Some(42.0)
        }
    }

    #[test]
    fn how_fast_the_voice_is_is_the_voice_s() {
        assert_eq!(Rehearsed::new(Arc::new(Slow::default())).rate(), Some(42.0));
    }

    #[tokio::test]
    async fn a_rehearsed_phrase_is_made_once_however_often_it_is_said() {
        let voice = Arc::new(Slow::default());
        let rehearsed = Rehearsed::new(voice.clone());
        rehearsed.rehearse("Let me see.".into());
        // Asked for while still being made: it waits for that making.
        let first = rehearsed.synthesise("Let me see.").await.unwrap();
        let again = rehearsed.synthesise("Let me see.").await.unwrap();
        rehearsed.rehearse("Let me see.".into());
        assert_eq!(first.samples, again.samples);
        assert_eq!(voice.made.load(Ordering::SeqCst), 1);
    }

    #[tokio::test]
    async fn anything_else_goes_to_the_voice_every_time() {
        let voice = Arc::new(Slow::default());
        let rehearsed = Rehearsed::new(voice.clone());
        rehearsed.synthesise("The weather is fine.").await.unwrap();
        rehearsed.synthesise("The weather is fine.").await.unwrap();
        assert_eq!(voice.made.load(Ordering::SeqCst), 2);
    }

    #[tokio::test]
    async fn a_rehearsal_that_fails_is_tried_again_when_it_is_said() {
        let voice = Arc::new(Slow::default());
        voice.fails.store(true, Ordering::SeqCst);
        let rehearsed = Rehearsed::new(voice.clone());
        rehearsed.rehearse("One sec.".into());
        tokio::time::sleep(Duration::from_millis(120)).await;
        voice.fails.store(false, Ordering::SeqCst);
        assert!(rehearsed.synthesise("One sec.").await.is_ok());
        assert_eq!(voice.made.load(Ordering::SeqCst), 2);
    }
}
