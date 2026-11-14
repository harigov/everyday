//! Storage for screen time: spans, labels, categories and the settings.
//!
//! # Why this is its own trait
//!
//! Money's reason: a backend that keeps journals as Markdown files has no
//! opinion about which app had focus, so the domain is reached through
//! [`JournalStore::screentime`](super::JournalStore::screentime), `None` by
//! default, and the interface reads
//! [`Capabilities::screentime`](super::Capabilities::screentime) to know
//! whether to offer the app at all.
//!
//! # The rules an implementation must keep
//!
//! Every one of these is in the conformance suite (`conformance/screentime.rs`):
//!
//! - **Everything but the index is sealed.** Clear columns are a span's id,
//!   its stream (a keyed hash the vault computes), and its start and end; a
//!   label's or category's id, sort order and two timestamps. Never a
//!   device, an app, a host, a title, a key or a category's name.
//! - **A span is bound to its id**, so a payload copied into another row
//!   will not open there.
//! - **[`ScreenTimeStore::spans`] answers every span that overlaps the
//!   range** -- `start < to` and `end > from` -- in start order, then id.
//! - **No span lasts longer than
//!   [`MAX_SPAN_SECS`](crate::screentime::MAX_SPAN_SECS) or ends before it
//!   starts.** [`ScreenTimeStore::put_spans`] refuses such a batch whole,
//!   with [`Error::Invalid`](crate::error::Error::Invalid), so a range read
//!   may look back no further than that before its start.
//! - **[`ScreenTimeStore::last_span`] answers the stream's span with the
//!   greatest end** (ties by id, greatest first).
//! - **Deleting a category touches nothing else.** A label still pointing at
//!   it is the vault's to tidy.

use crate::error::Result;
use crate::id::{ScreenAppId, ScreenCategoryId, ScreenSpanId};
use crate::screentime::{ScreenApp, ScreenCategory, ScreenSpan, ScreenTimeSettings};
use jiff::Timestamp;

/// The persistence contract for screen time.
pub trait ScreenTimeStore: Send + Sync {
    // ---- spans ----------------------------------------------------------

    /// Insert or replace each by id, all or nothing. `stream` is the
    /// writing stream's keyed hash, kept in the clear for
    /// [`ScreenTimeStore::last_span`].
    fn put_spans(&self, stream: &str, spans: &[ScreenSpan]) -> Result<()>;
    /// The stream's latest span, if it has any.
    fn last_span(&self, stream: &str) -> Result<Option<ScreenSpan>>;
    /// Every span overlapping `from..to`, in start order.
    fn spans(&self, from: Timestamp, to: Timestamp) -> Result<Vec<ScreenSpan>>;
    /// Unknown ids are ignored. Answers how many went.
    fn delete_spans(&self, ids: &[ScreenSpanId]) -> Result<u64>;
    /// Every span that ended before `before`. Answers how many went.
    fn delete_spans_before(&self, before: Timestamp) -> Result<u64>;
    /// Empty the title of every span that ended in `from..before`. Answers
    /// how many changed. Nothing else about a span is touched, and its
    /// payload stays bound to its id.
    fn strip_titles(&self, from: Timestamp, before: Timestamp) -> Result<u64>;
    /// When the earliest span began, if there is one.
    fn earliest_span(&self) -> Result<Option<Timestamp>>;

    // ---- labels ---------------------------------------------------------

    fn list_apps(&self) -> Result<Vec<ScreenApp>>;
    fn get_app(&self, id: ScreenAppId) -> Result<ScreenApp>;
    /// Insert or replace by id.
    fn put_app(&self, app: &ScreenApp) -> Result<()>;
    fn delete_app(&self, id: ScreenAppId) -> Result<()>;

    // ---- categories -----------------------------------------------------

    fn list_categories(&self) -> Result<Vec<ScreenCategory>>;
    fn get_category(&self, id: ScreenCategoryId) -> Result<ScreenCategory>;
    /// Insert or replace by id.
    fn put_category(&self, category: &ScreenCategory) -> Result<()>;
    fn delete_category(&self, id: ScreenCategoryId) -> Result<()>;

    // ---- settings -------------------------------------------------------

    /// [`ScreenTimeSettings::default`] when never saved.
    fn screen_settings(&self) -> Result<ScreenTimeSettings>;
    fn put_screen_settings(&self, settings: &ScreenTimeSettings) -> Result<()>;
}

/// Associated data bound into a record's ciphertext. See
/// [`entry_aad`](super::entry_aad) for why records are bound to their id.
pub fn screen_span_aad(id: ScreenSpanId) -> Vec<u8> {
    format!("everyday.screen_span.v1:{id}").into_bytes()
}

pub fn screen_app_aad(id: ScreenAppId) -> Vec<u8> {
    format!("everyday.screen_app.v1:{id}").into_bytes()
}

pub fn screen_category_aad(id: ScreenCategoryId) -> Vec<u8> {
    format!("everyday.screen_category.v1:{id}").into_bytes()
}

/// The settings singleton.
pub fn screen_settings_aad() -> Vec<u8> {
    b"everyday.screen-settings.v1".to_vec()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_label_is_distinct() {
        let id = uuid::Uuid::now_v7();
        let labels = [
            screen_span_aad(ScreenSpanId(id)),
            screen_app_aad(ScreenAppId(id)),
            screen_category_aad(ScreenCategoryId(id)),
            screen_settings_aad(),
        ];
        for (i, a) in labels.iter().enumerate() {
            for b in &labels[i + 1..] {
                assert_ne!(a, b);
            }
        }
    }
}
