//! The wake word with Jatayu's own model, end to end: recordings of voices it
//! never trained on saying the name -- alone, or with a request after it --
//! through `Service::wake_open` with the real voice detector and Parakeet,
//! and what it wakes with.
//!
//! `#[ignore]`, and gated by `JATAYU_TEST_SPEECH=1`, as the other real-model
//! tests are; the recordings come from `scripts/wakeword/eval_wavs.py --clean`
//! into `JATAYU_TEST_WAKEWORD_DIR`.

#![cfg(feature = "speech")]

use jatayu_core::llm::ModelSettings;
use jatayu_core::meeting::LocalModel;
use jatayu_core::speech::{SpeechConfig, TranscriberConfig};
use jatayu_service::local::files as models;
use jatayu_service::speech::wake::Woken;
use std::collections::HashSet;
use std::time::Duration;

#[allow(dead_code)]
mod support;

fn words(text: &str) -> HashSet<String> {
    text.split(|c: char| !c.is_alphanumeric() && c != '\'')
        .filter(|w| !w.is_empty())
        .map(str::to_lowercase)
        .collect()
}

#[tokio::test(flavor = "multi_thread")]
#[ignore = "runs real models on recordings; needs JATAYU_TEST_SPEECH=1 and JATAYU_TEST_WAKEWORD_DIR"]
async fn the_name_wakes_it_with_what_came_after() {
    if std::env::var("JATAYU_TEST_SPEECH").as_deref() != Ok("1") {
        return;
    }
    let Ok(dir) = std::env::var("JATAYU_TEST_WAKEWORD_DIR") else { return };
    let dir = std::path::PathBuf::from(dir);
    assert!(models::speech_kit_installed(), "the speech kit is needed");
    assert!(models::installed(LocalModel::ParakeetV3), "Parakeet is needed");

    let (svc, _vault_dir) = support::vault::service(None);
    let vault = svc.get().unwrap();
    let mut settings = ModelSettings::default();
    let mut speech = SpeechConfig::new(TranscriberConfig::Local { model: LocalModel::ParakeetV3 });
    speech.language = Some("en".into());
    settings.speech = Some(speech);
    vault.save_model_settings(&settings).unwrap();

    let manifest = std::fs::read_to_string(dir.join("manifest.jsonl")).unwrap();
    let (mut total, mut woke, mut alone_right, mut alone, mut asked_right, mut asked) =
        (0, 0, 0, 0, 0, 0);
    let limit: usize = std::env::var("JATAYU_TEST_WAKEWORD_LIMIT")
        .ok()
        .and_then(|n| n.parse().ok())
        .unwrap_or(usize::MAX);
    for line in manifest.lines().take(limit) {
        let entry: serde_json::Value = serde_json::from_str(line).unwrap();
        let bytes = std::fs::read(dir.join(entry["file"].as_str().unwrap())).unwrap();
        let clip = jatayu_service::speech::wav::decode(&bytes).unwrap();
        let mut link = svc.wake_open(vec!["Jatayu".into()]).await.unwrap();
        for block in clip.samples.chunks(480) {
            link.input.send(block.to_vec()).unwrap();
        }
        // The pause that ends an utterance, and a little more.
        for _ in 0..100 {
            link.input.send(vec![0; 480]).unwrap();
        }
        total += 1;
        let got = tokio::time::timeout(Duration::from_secs(30), link.woken.recv()).await;
        let Ok(Some(Woken { rest })) = got else {
            println!("missed: {}", entry["file"]);
            continue;
        };
        woke += 1;
        match entry["request"].as_str() {
            None => {
                alone += 1;
                alone_right += usize::from(rest.trim().is_empty());
                if !rest.trim().is_empty() {
                    println!("the name alone woke it with {rest:?}");
                }
            }
            Some(request) => {
                asked += 1;
                let (want, got) = (words(request), words(&rest));
                let shared = want.intersection(&got).count();
                let right = shared * 10 >= want.len() * 7;
                asked_right += usize::from(right);
                if !right {
                    println!("asked {request:?}, woke with {rest:?}");
                }
            }
        }
    }
    println!(
        "woke for {woke}/{total}; the name alone with nothing after it {alone_right}/{alone}; \
         the request after it {asked_right}/{asked}"
    );
    assert!(woke * 10 >= total * 9, "woke for only {woke} of {total}");
    assert!(
        alone_right * 10 >= alone * 8,
        "the name alone came with words {}/{alone}",
        alone - alone_right
    );
    assert!(asked_right * 10 >= asked * 7, "the request was wrong {}/{asked}", asked - asked_right);
}
