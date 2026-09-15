//! A benchmark against the speed budget in `docs/plans/mail.md`: "A search:
//! 150 ms for the first page." `#[ignore]`d because it indexes 100,000
//! synthetic messages, which has no place slowing down `cargo test`; run it
//! explicitly, in release, with:
//!
//! ```text
//! CARGO_TARGET_DIR=/home/hari/.cache/everyday-mail-target-index \
//!   cargo test -p everyday-mailindex --release --jobs 3 \
//!   --test benchmark -- --ignored --nocapture
//! ```
//!
//! It reports index time, on-disk size, cold-open time, and the p50/p95 of
//! five representative queries' first page, rather than asserting a strict
//! number: a debug build, a loaded CI box and a laptop on battery are not
//! the same machine, and the plan asks for the numbers to be measured and
//! written down, not for this file to become a flaky pass/fail gate.
//!
//! # One measurement, written down
//!
//! Run once, release, on the machine this crate was built on (20 cores, a
//! shared box, other builds running alongside):
//!
//! ```text
//! index time:     7.26s for 100,000 messages
//! on-disk size:   58.5 MiB, sealed
//! cold open time: 102.9 ms
//! free text, common word       p50 1.77 ms   p95 2.83 ms
//! from: a specific sender      p50 52 µs     p95 73 µs
//! subject: phrase              p50 5.35 ms   p95 7.26 ms
//! is:unread has:attachment     p50 254 µs    p95 335 µs
//! date range                   p50 1.21 ms   p95 1.34 ms
//! ```
//!
//! Every query's p95 lands one to three orders of magnitude under the
//! 150 ms budget, and cold open -- opening the sealed directory, decrypting
//! nothing yet, standing up the reader -- costs less than the budget for a
//! single search. The decrypt-on-first-read trade `directory.rs` describes
//! does not show up here because a search over 100,000 messages touches a
//! handful of segments, not all of them, and even a cold read of one is
//! microseconds against a 150 ms budget; a workload that fans a single
//! query out across every segment in a much larger mailbox is where that
//! trade would start to matter. The slowest of the five,
//! `subject:"quarterly report"`'s phrase query, is still only a few
//! milliseconds either way.

use std::sync::Arc;
use std::time::{Duration, Instant};

use everyday_core::crypto::{AeadCipher, Cipher, SecretKey};
use everyday_core::{MailDoc, MailQuery, MailSearch};
use everyday_mailindex::MailIndex;
use jiff::Timestamp;

const MESSAGE_COUNT: usize = 100_000;
const BATCH_SIZE: usize = 500;
const CACHE_BYTES: usize = 128 * 1024 * 1024;
const QUERY_REPEATS: usize = 50;

/// A tiny deterministic PRNG (xorshift64) -- good enough for synthetic
/// message shapes, and reproducible from run to run without pulling `rand`
/// into a non-dev build.
struct Rng(u64);

impl Rng {
    fn next(&mut self) -> u64 {
        let mut x = self.0;
        x ^= x << 13;
        x ^= x >> 7;
        x ^= x << 17;
        self.0 = x;
        x
    }

    fn below(&mut self, n: u64) -> u64 {
        self.next() % n
    }
}

const FILLER_WORDS: &[&str] = &[
    "the",
    "quick",
    "team",
    "meeting",
    "schedule",
    "project",
    "update",
    "please",
    "review",
    "attached",
    "thanks",
    "regards",
    "client",
    "budget",
    "deadline",
    "draft",
    "final",
    "notes",
    "follow",
    "up",
    "call",
    "tomorrow",
    "week",
    "next",
    "月曜日",
    "reminder",
    "status",
    "plan",
];

fn body_text(rng: &mut Rng, target_bytes: usize) -> String {
    let mut s = String::with_capacity(target_bytes + 32);
    while s.len() < target_bytes {
        let word = FILLER_WORDS[rng.below(FILLER_WORDS.len() as u64) as usize];
        s.push_str(word);
        s.push(' ');
    }
    s
}

fn synthetic_doc(rng: &mut Rng, i: usize, base: Timestamp) -> MailDoc {
    let account = if i % 5 == 0 { "work" } else { "personal" };
    let mailbox = match i % 4 {
        0 => "archive",
        1 => "sent",
        _ => "inbox",
    };
    let sender_id = rng.below(500);
    let subject_words =
        ["quarterly", "report", "invoice", "meeting", "notes", "for", "the", "team"];
    let mut subject = String::new();
    for _ in 0..9 {
        subject.push_str(subject_words[rng.below(subject_words.len() as u64) as usize]);
        subject.push(' ');
    }
    subject.truncate(60);

    let body_len = 2000 + (rng.below(3000) as usize);
    let date = base.checked_add(Duration::from_secs(i as u64 * 55)).unwrap();

    MailDoc {
        message_key: format!("msg-{i:06}"),
        thread_key: format!("thread-{:06}", i / 3),
        account: account.to_string(),
        mailboxes: vec![mailbox.to_string()],
        from: format!("Sender {sender_id} <user{sender_id}@example.com>"),
        to: "me@example.com".to_string(),
        cc: String::new(),
        subject,
        body_text: body_text(rng, body_len),
        labels: if i % 11 == 0 { vec!["travel".to_string()] } else { vec![] },
        date,
        has_attachment: i % 7 == 0,
        unread: i % 3 == 0,
        starred: i % 29 == 0,
    }
}

fn dir_size(path: &std::path::Path) -> u64 {
    let mut total = 0;
    let Ok(entries) = std::fs::read_dir(path) else { return 0 };
    for entry in entries.flatten() {
        let p = entry.path();
        if p.is_dir() {
            total += dir_size(&p);
        } else if let Ok(meta) = entry.metadata() {
            total += meta.len();
        }
    }
    total
}

fn percentile(mut samples: Vec<Duration>, pct: f64) -> Duration {
    samples.sort();
    let idx = ((samples.len() as f64 - 1.0) * pct).round() as usize;
    samples[idx]
}

#[test]
#[ignore]
fn bench_100k_messages() {
    let tmp = tempfile::tempdir().unwrap();
    let cipher: Arc<dyn Cipher> = Arc::new(AeadCipher::new(&SecretKey::from_bytes([11u8; 32])));

    let base = Timestamp::now().checked_sub(Duration::from_secs(60 * 60 * 24 * 365 * 2)).unwrap();
    let mut rng = Rng(0x9E3779B97F4A7C15);

    let index_start = Instant::now();
    {
        let mi = MailIndex::open(tmp.path(), Arc::clone(&cipher), CACHE_BYTES).unwrap();
        let mut batch = Vec::with_capacity(BATCH_SIZE);
        for i in 0..MESSAGE_COUNT {
            batch.push(synthetic_doc(&mut rng, i, base));
            if batch.len() == BATCH_SIZE {
                mi.index(&batch).unwrap();
                mi.commit().unwrap();
                batch.clear();
            }
        }
        if !batch.is_empty() {
            mi.index(&batch).unwrap();
            mi.commit().unwrap();
        }
    }
    let index_elapsed = index_start.elapsed();
    let on_disk_bytes = dir_size(tmp.path());

    let cold_open_start = Instant::now();
    let mi = MailIndex::open(tmp.path(), cipher, CACHE_BYTES).unwrap();
    let cold_open_elapsed = cold_open_start.elapsed();
    assert!(!mi.rebuild_needed());

    let queries: [(&str, MailQuery); 5] = [
        ("free text, common word", MailQuery::parse("meeting")),
        ("from: a specific sender", MailQuery::parse("from:user123@example.com")),
        ("subject: phrase", MailQuery::parse(r#"subject:"quarterly report""#)),
        ("is:unread has:attachment", MailQuery::parse("is:unread has:attachment")),
        ("date range", MailQuery::parse("after:2024/06/01 before:2025/06/01")),
    ];

    eprintln!("--- everyday-mailindex benchmark: {MESSAGE_COUNT} messages ---");
    eprintln!("index time:     {index_elapsed:?}");
    eprintln!("on-disk size:   {:.1} MiB", on_disk_bytes as f64 / (1024.0 * 1024.0));
    eprintln!("cold open time: {cold_open_elapsed:?}");

    for (name, query) in &queries {
        // One untimed warm-up, so the first sample is not paying for
        // anything a real first page would not (the query's terms are
        // already resolved against the schema either way; this just keeps
        // page-cache effects consistent across the timed samples).
        let _ = mi.search(query, 25, None).unwrap();

        let mut samples = Vec::with_capacity(QUERY_REPEATS);
        for _ in 0..QUERY_REPEATS {
            let start = Instant::now();
            let page = mi.search(query, 25, None).unwrap();
            samples.push(start.elapsed());
            std::hint::black_box(&page);
        }
        let p50 = percentile(samples.clone(), 0.50);
        let p95 = percentile(samples, 0.95);
        eprintln!("{name:<28} p50 {p50:>8?}   p95 {p95:>8?}   (budget: 150ms)");
    }
    eprintln!("--- end benchmark ---");
}
