//! The Feed's own settings, and the ranking that orders Recommended.
//!
//! One sealed row for the whole vault, saved whole from Settings → Feed, on
//! `mail_sorting`'s pattern. What the ranker learns about each thing is not
//! in it: a [`Rank`] is a row of its own per thing, written by a background
//! pass, so a save of the settings can never carry a stale copy of one over
//! another.
//!
//! # How a rank is worked out
//!
//! Each [`Criterion`] is a yes-or-no question somebody wrote ("about
//! building software with AI", "a show I always finish") with a weight from
//! -3 to 3 -- a negative weight for what they would rather skip. The
//! decision model is asked each one about each new story, and answers yes
//! with some probability; the score is the weighted mean of those
//! probabilities, from -1 to 1 ([`Ranking::score`]). A rank remembers the
//! [`Ranking::fingerprint`] of the criteria it answered, so editing them
//! makes every rank stale at once without touching a row.

use super::listening::ContentRef;
use super::podcast::PodcastDefaults;
use jiff::Timestamp;
use serde::{Deserialize, Serialize};

/// Everything Settings → Feed holds.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[serde(rename_all = "camelCase")]
pub struct FeedSettings {
    /// What every show downloads and keeps, unless it says otherwise.
    #[serde(default)]
    pub podcasts: PodcastDefaults,
    #[serde(default)]
    pub ranking: Ranking,
    /// Keep every article read or heard as Markdown under `Feed/Articles`,
    /// where the document index finds it: the Feed's memory of it, which
    /// outlives the story.
    #[serde(default = "yes")]
    pub keep_read: bool,
    /// Keep Up Next at least this many things long, topped up from
    /// Recommended. `None` never adds anything by itself.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub autofill: Option<u32>,
    /// Play the next thing in Up Next when one ends.
    #[serde(default = "yes")]
    pub autoplay: bool,
    /// What the player's buttons skip, in seconds.
    #[serde(default = "FeedSettings::default_skip_back")]
    pub skip_back_s: u32,
    #[serde(default = "FeedSettings::default_skip_forward")]
    pub skip_forward_s: u32,
}

fn yes() -> bool {
    true
}

impl FeedSettings {
    fn default_skip_back() -> u32 {
        15
    }
    fn default_skip_forward() -> u32 {
        30
    }
}

impl Default for FeedSettings {
    fn default() -> Self {
        Self {
            podcasts: PodcastDefaults::default(),
            ranking: Ranking::default(),
            keep_read: true,
            autofill: None,
            autoplay: true,
            skip_back_s: Self::default_skip_back(),
            skip_forward_s: Self::default_skip_forward(),
        }
    }
}

/// How Recommended is ordered.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[serde(rename_all = "camelCase")]
pub struct Ranking {
    /// Rank at all. Off, or with no criteria and no goals, Recommended is
    /// newest first.
    #[serde(default = "yes")]
    pub enabled: bool,
    #[serde(default)]
    pub criteria: Vec<Criterion>,
    /// Also ask whether a story helps with one of the person's active goals.
    #[serde(default)]
    pub goals: bool,
    /// How far back a story may be and still be recommended, in days.
    #[serde(default = "Ranking::default_window")]
    pub window_days: u32,
}

impl Ranking {
    fn default_window() -> u32 {
        7
    }

    /// The criteria's weight given to an answer about the person's goals.
    pub const GOALS_WEIGHT: i8 = 2;
    /// What [`RankAnswer::criterion`] is for the goals question.
    pub const GOALS: &'static str = "goals";

    /// Whether there is anything to ask.
    pub fn asks(&self) -> bool {
        self.enabled && (self.goals || self.criteria.iter().any(|c| !c.text.trim().is_empty()))
    }

    /// What a rank must have answered to be current: the questions and their
    /// weights, hashed. Short hex.
    pub fn fingerprint(&self) -> String {
        let mut hasher = blake3::Hasher::new();
        for c in &self.criteria {
            hasher.update(c.id.as_bytes());
            hasher.update(&[0]);
            hasher.update(c.text.trim().as_bytes());
            hasher.update(&[0, c.weight as u8, 0]);
        }
        hasher.update(if self.goals { b"goals" } else { b"-" });
        hasher.finalize().to_hex()[..16].to_string()
    }

    /// The weighted mean of the answers, from -1 to 1. An answer to a
    /// criterion that is no longer there counts for nothing; with nothing to
    /// count, 0.
    pub fn score(&self, answers: &[RankAnswer]) -> f32 {
        let weight_of = |id: &str| -> Option<i8> {
            if id == Self::GOALS {
                return self.goals.then_some(Self::GOALS_WEIGHT);
            }
            self.criteria.iter().find(|c| c.id == id).map(|c| c.weight.clamp(-3, 3))
        };
        let (mut sum, mut total) = (0.0f32, 0.0f32);
        for answer in answers {
            let Some(w) = weight_of(&answer.criterion) else { continue };
            if w == 0 {
                continue;
            }
            sum += w as f32 * answer.yes.clamp(0.0, 1.0);
            total += (w as f32).abs();
        }
        if total == 0.0 { 0.0 } else { (sum / total).clamp(-1.0, 1.0) }
    }
}

impl Default for Ranking {
    fn default() -> Self {
        Self { enabled: true, criteria: Vec::new(), goals: false, window_days: 7 }
    }
}

/// One thing somebody wants more or less of.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[serde(rename_all = "camelCase")]
pub struct Criterion {
    /// Stable across edits of the text, so an answer can be matched to it.
    pub id: String,
    /// The question, in the person's words: "about building software with AI".
    pub text: String,
    /// -3 to 3. Negative for what they would rather skip.
    pub weight: i8,
}

/// How well one thing fits the ranking.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[serde(rename_all = "camelCase")]
pub struct Rank {
    pub what: ContentRef,
    /// [`Ranking::score`] of `answers` when it was worked out.
    pub score: f32,
    pub answers: Vec<RankAnswer>,
    /// [`Ranking::fingerprint`] of the criteria answered.
    pub fingerprint: String,
    pub at: Timestamp,
}

/// The model's answer to one criterion.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[serde(rename_all = "camelCase")]
pub struct RankAnswer {
    /// [`Criterion::id`], or [`Ranking::GOALS`].
    pub criterion: String,
    /// The probability it gave "yes", from 0 to 1.
    pub yes: f32,
}

#[cfg(test)]
mod tests {
    use super::*;

    fn criterion(id: &str, weight: i8) -> Criterion {
        Criterion { id: id.into(), text: format!("about {id}"), weight }
    }

    #[test]
    fn the_score_is_a_weighted_mean_and_avoidance_pulls_it_down() {
        let ranking = Ranking {
            criteria: vec![criterion("ai", 3), criterion("crypto", -3)],
            ..Default::default()
        };
        let liked = [
            RankAnswer { criterion: "ai".into(), yes: 1.0 },
            RankAnswer { criterion: "crypto".into(), yes: 0.0 },
        ];
        assert!((ranking.score(&liked) - 0.5).abs() < 1e-6);
        let avoided = [
            RankAnswer { criterion: "ai".into(), yes: 0.0 },
            RankAnswer { criterion: "crypto".into(), yes: 1.0 },
        ];
        assert!((ranking.score(&avoided) + 0.5).abs() < 1e-6);
        assert_eq!(ranking.score(&[RankAnswer { criterion: "gone".into(), yes: 1.0 }]), 0.0);
    }

    #[test]
    fn editing_a_criterion_changes_the_fingerprint() {
        let mut ranking = Ranking { criteria: vec![criterion("ai", 3)], ..Default::default() };
        let before = ranking.fingerprint();
        ranking.criteria[0].weight = 2;
        assert_ne!(before, ranking.fingerprint());
        assert_eq!(ranking.fingerprint().len(), 16);
    }

    #[test]
    fn nothing_to_ask_means_no_ranking() {
        assert!(!Ranking::default().asks());
        let blank = Ranking {
            criteria: vec![Criterion { id: "x".into(), text: " ".into(), weight: 1 }],
            ..Default::default()
        };
        assert!(!blank.asks());
        assert!(Ranking { goals: true, ..Default::default() }.asks());
    }
}
