//! Real verification of the decision models that run on this computer: the
//! actual GGUF files, downloaded through the catalogue the app uses, run
//! through llama.cpp -- not a mock.
//!
//! `#[ignore]`, and gated a second time by `KARMAYOGA_TEST_DECISIONS=1`, for
//! the same reason as `speech_real.rs`: the first run downloads one to
//! three models of one to four gigabytes each into the real
//! [`files::models_dir`], and then runs them. Both have to be asked for.
//!
//! `KARMAYOGA_TEST_DECISION_MODELS` picks which (`d3Lite,d3Nano`; all three
//! by default).
//!
//! `KARMAYOGA_TEST_DECISION_EVAL` may name a JSON Lines file of labelled
//! messages to score each model on -- one object a line, with `category`
//! (one of the starting categories), `from`, `to` (`to`, `cc` or
//! `neither`), `subject`, `body` and `known` (whether you have written to the
//! sender). Nothing in this repository; the scores are printed, not checked.

#![cfg(feature = "decisions")]

use karmayoga_core::llm::LocalDecisionModel;
use karmayoga_service::decision::Content;
use karmayoga_service::local::decide::{self, samples};
use karmayoga_service::local::files;
use std::time::{Duration, Instant};

fn enabled() -> bool {
    std::env::var("KARMAYOGA_TEST_DECISIONS").as_deref() == Ok("1")
}

fn models() -> Vec<LocalDecisionModel> {
    match std::env::var("KARMAYOGA_TEST_DECISION_MODELS") {
        Ok(list) => list
            .split(',')
            .map(|id| {
                LocalDecisionModel::from_id(id).unwrap_or_else(|| panic!("{id} is not a model"))
            })
            .collect(),
        Err(_) => LocalDecisionModel::ALL.to_vec(),
    }
}

async fn ensure_installed(model: LocalDecisionModel) {
    let id = model.id();
    if files::decision_installed(model) {
        return;
    }
    files::start_download(id).unwrap_or_else(|e| panic!("{id}: {e}"));
    let deadline = Instant::now() + Duration::from_secs(60 * 60);
    loop {
        let status = files::status(id).unwrap_or_else(|| panic!("{id} is not in the catalogue"));
        if status.installed {
            return;
        }
        if let Some(e) = status.error {
            panic!("{id} did not download: {e}");
        }
        assert!(Instant::now() < deadline, "{id} took over an hour to download");
        tokio::time::sleep(Duration::from_secs(2)).await;
    }
}

fn content(text: String) -> Content {
    Content { fields: Default::default(), text }
}

/// Every model sorts the five sample messages, one a category, and says how
/// long it took.
#[tokio::test(flavor = "multi_thread")]
#[ignore = "downloads gigabytes of models; set KARMAYOGA_TEST_DECISIONS=1"]
async fn each_model_sorts_the_sample_mail() {
    if !enabled() {
        eprintln!("KARMAYOGA_TEST_DECISIONS is not 1; skipping");
        return;
    }
    let options = samples::categories();
    for model in models() {
        ensure_installed(model).await;
        let bench = decide::benchmark(model).await.unwrap();
        eprintln!(
            "{}: loaded in {:?} ms, {} ms a message",
            model.name(),
            bench.load_ms,
            bench.ms_per_message
        );
        let mut right = 0;
        for (want, text) in samples::labelled() {
            let choice = decide::choose(model, &content(text), samples::INSTRUCTIONS, &options)
                .await
                .unwrap();
            eprintln!("  {want:>13} -> {} ({:.2})", choice.name, choice.confidence.unwrap_or(0.0));
            assert!(options.iter().any(|o| o.name == choice.name), "{} is no option", choice.name);
            right += usize::from(choice.name == want);
        }
        assert!(right >= 4, "{} sorted only {right} of 5 sample messages right", model.name());
    }
}

/// Scores each model on `KARMAYOGA_TEST_DECISION_EVAL`'s messages, if given:
/// accuracy, where it went wrong, and time a message.
#[tokio::test(flavor = "multi_thread")]
#[ignore = "downloads gigabytes of models; set KARMAYOGA_TEST_DECISIONS=1"]
async fn each_model_is_scored_on_an_evaluation_set() {
    if !enabled() {
        eprintln!("KARMAYOGA_TEST_DECISIONS is not 1; skipping");
        return;
    }
    let Ok(path) = std::env::var("KARMAYOGA_TEST_DECISION_EVAL") else {
        eprintln!("KARMAYOGA_TEST_DECISION_EVAL is not set; skipping");
        return;
    };
    let text = std::fs::read_to_string(&path).unwrap_or_else(|e| panic!("{path}: {e}"));
    let set: Vec<(String, String)> = text
        .lines()
        .filter(|l| !l.trim().is_empty())
        .map(|line| {
            let v: serde_json::Value = serde_json::from_str(line).unwrap();
            let s = |k: &str| v[k].as_str().unwrap_or_default().to_string();
            let message = samples::message(
                &s("from"),
                &s("to"),
                &s("subject"),
                &s("body"),
                v["known"].as_bool().unwrap_or(false),
            );
            (s("category"), message)
        })
        .collect();
    let options = samples::categories();
    for model in models() {
        ensure_installed(model).await;
        decide::benchmark(model).await.unwrap();
        let started = Instant::now();
        let mut right = 0;
        let mut wrong = Vec::new();
        for (want, message) in &set {
            let choice =
                decide::choose(model, &content(message.clone()), samples::INSTRUCTIONS, &options)
                    .await
                    .unwrap();
            if &choice.name == want {
                right += 1;
            } else {
                let subject = message.lines().nth(2).unwrap_or_default().to_string();
                wrong.push(format!("{want} -> {} ({subject})", choice.name));
            }
        }
        let ms = started.elapsed().as_millis() / set.len().max(1) as u128;
        eprintln!(
            "{}: {right}/{} right ({:.0}%), {ms} ms a message",
            model.name(),
            set.len(),
            100.0 * right as f64 / set.len().max(1) as f64
        );
        for w in wrong {
            eprintln!("    {w}");
        }
    }
}
