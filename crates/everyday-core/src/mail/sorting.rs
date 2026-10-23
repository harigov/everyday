//! The categories mail is sorted into by a decision model, as somebody
//! wrote them -- Settings → Mail sorting.
//!
//! Each [`SortCategory`] is a name, a description of what belongs in it, and
//! the inbox tab it files under ([`SortCategory::treat_as`]). Every choice
//! the app makes about a message -- which tab shows it, whether it is
//! Priority -- reads the [`Category`] its sorted category files under, so a
//! category of somebody's own is a finer-grained name for one of the five
//! the app already understands, never a sixth thing nothing else knows how
//! to treat. The name and description are what the model is shown: they
//! are the choice's label and its criteria, and the description is where
//! the person says what should and should not count.
//!
//! Vault-wide rather than per account. Somebody's idea of a receipt does
//! not change from their work inbox to their personal one, and one list is
//! one place to say it.
//!
//! # Which mail
//!
//! Only recent mail is sorted unasked: whatever arrives, and anything still
//! within [`MailSorting::window_days`] of now when sorting starts. Older
//! mail is sorted only when somebody asks (`sort_older_mail`): sorting a
//! decade of somebody's archive because they switched a setting on is a
//! bill, and a reshuffle, nobody agreed to.

use serde::{Deserialize, Serialize};

use super::Category;
use crate::error::{Error, Result};

/// The most categories a list may hold. TypeSafe's own limit on a choice is
/// 255 options; this is far below it because every category is shown to
/// the model with every message, and a list nobody can read through in the
/// settings pane is not one anybody is maintaining.
pub const MAX_CATEGORIES: usize = 40;
/// Longest a category's name may be.
pub const MAX_NAME_CHARS: usize = 60;
/// Longest a category's description may be.
pub const MAX_DESCRIPTION_CHARS: usize = 1_000;
/// What [`MailSorting::window_days`] starts as.
pub const DEFAULT_WINDOW_DAYS: u32 = 30;
/// The longest window that runs unasked. Anything older is `sort_older_mail`'s.
pub const MAX_WINDOW_DAYS: u32 = 90;

/// One category somebody sorts their mail into.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[serde(rename_all = "camelCase")]
pub struct SortCategory {
    /// Stable across renames: what a sorted thread points at
    /// ([`SortedAs::category`]), so renaming "Bills" to "Receipts" renames
    /// every thread already sorted into it.
    pub id: String,
    /// What it is called -- and the option's name as the model sees it.
    pub name: String,
    /// What belongs here and what does not, in the person's own words. The
    /// option's criteria, as the model sees them.
    #[serde(default)]
    pub description: String,
    /// The inbox tab mail sorted here files under.
    pub treat_as: Category,
}

/// Every category, and how far back sorting reaches unasked.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[serde(rename_all = "camelCase")]
pub struct MailSorting {
    pub categories: Vec<SortCategory>,
    /// How many days back the background pass reaches. Mail older than
    /// this is sorted only when somebody asks.
    #[serde(default = "default_window_days")]
    pub window_days: u32,
}

fn default_window_days() -> u32 {
    DEFAULT_WINDOW_DAYS
}

impl Default for MailSorting {
    /// The five tabs, described the way the small model's sorting prompt
    /// described them, plus the two sorts of notification people most
    /// often want apart. A starting point to be edited, not a taxonomy.
    fn default() -> Self {
        let c = |id: &str, name: &str, description: &str, treat_as| SortCategory {
            id: id.into(),
            name: name.into(),
            description: description.into(),
            treat_as,
        };
        Self {
            categories: vec![
                c(
                    "needs-you",
                    "Needs you",
                    "A real person directly asks you to do, answer or decide something; a \
                     deadline or a time-sensitive matter involves you; or someone is waiting on \
                     you. Not: FYI updates, anything automated, threads you are only cc'd on.",
                    Category::Priority,
                ),
                c(
                    "people",
                    "People",
                    "Written by a real person, to you, and worth reading: conversations, \
                     replies, introductions and updates from people you know or work with, that \
                     do not need anything from you soon.",
                    Category::Important,
                ),
                c(
                    "newsletters",
                    "Newsletters",
                    "Mail you subscribed to or were added to a list for: newsletters, digests, \
                     blog posts, announcements, marketing and promotions.",
                    Category::Newsletter,
                ),
                c(
                    "receipts",
                    "Receipts",
                    "Orders, receipts, invoices, payment confirmations, bills and shipping or \
                     delivery updates.",
                    Category::Notification,
                ),
                c(
                    "notifications",
                    "Notifications",
                    "Automated messages from apps and services: alerts, account and security \
                     notices, sign-in codes, calendar notices, comments and mentions.",
                    Category::Notification,
                ),
                c(
                    "other",
                    "Other",
                    "Anything that fits none of the other categories.",
                    Category::Other,
                ),
            ],
            window_days: DEFAULT_WINDOW_DAYS,
        }
    }
}

impl MailSorting {
    /// Reject a list the model could not be asked about, saying which
    /// category is wrong.
    pub fn validate(&self) -> Result<()> {
        if self.categories.is_empty() {
            return Err(Error::Invalid("mail sorting needs at least one category".into()));
        }
        if self.categories.len() > MAX_CATEGORIES {
            return Err(Error::Invalid(format!(
                "mail sorting takes at most {MAX_CATEGORIES} categories"
            )));
        }
        if !(1..=MAX_WINDOW_DAYS).contains(&self.window_days) {
            return Err(Error::Invalid(format!(
                "mail is sorted unasked for between 1 and {MAX_WINDOW_DAYS} days back"
            )));
        }
        let mut names = std::collections::HashSet::new();
        let mut ids = std::collections::HashSet::new();
        for (i, category) in self.categories.iter().enumerate() {
            let name = category.name.trim();
            let which = if name.is_empty() { format!("category {}", i + 1) } else { name.into() };
            if category.id.trim().is_empty() || !ids.insert(category.id.as_str()) {
                return Err(Error::Invalid(format!("{which}: every category needs its own id")));
            }
            if name.is_empty() {
                return Err(Error::Invalid(format!("{which} needs a name")));
            }
            if name.chars().count() > MAX_NAME_CHARS {
                return Err(Error::Invalid(format!(
                    "{which}: a name is at most {MAX_NAME_CHARS} characters"
                )));
            }
            // The name is the option the model picks, and its answer is
            // matched back by name, so two the same would be one option.
            if !names.insert(name.to_lowercase()) {
                return Err(Error::Invalid(format!("there are two categories called {name}")));
            }
            if category.description.chars().count() > MAX_DESCRIPTION_CHARS {
                return Err(Error::Invalid(format!(
                    "{which}: a description is at most {MAX_DESCRIPTION_CHARS} characters"
                )));
            }
        }
        Ok(())
    }

    /// The category a model's answer names, matched as the options were
    /// written: by name, trimmed, ignoring case.
    pub fn by_name(&self, name: &str) -> Option<&SortCategory> {
        let name = name.trim();
        self.categories.iter().find(|c| c.name.trim().eq_ignore_ascii_case(name))
    }

    pub fn by_id(&self, id: &str) -> Option<&SortCategory> {
        self.categories.iter().find(|c| c.id == id)
    }
}

/// When a decision model was last asked about a thread, and how many
/// messages it had then -- [`super::Thread::ai_sorted`].
///
/// Its own marker rather than the small model's
/// `ai_categorize_asked_at_count`: that one says a thread was asked about
/// the *five tabs*, which is not an answer to the person's own categories,
/// and sharing it would leave every thread Small had already looked at
/// unsorted the day a decision model is switched on. The time is what lets
/// an asked-for re-sort tell a thread it has reached from one sorted before
/// it began, without remembering every thread it has been through.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[serde(rename_all = "camelCase")]
pub struct SortMark {
    pub message_count: u32,
    pub at: jiff::Timestamp,
}

/// Which of somebody's categories a decision model sorted a thread into,
/// and for which message.
///
/// On the [`super::Thread`] rather than on each message: it is what a
/// thread row shows, and a thread's category is its newest message's. The
/// message it was decided for is kept so that the moment a newer message
/// arrives -- or a person moves the thread themselves -- the label stops
/// being shown, rather than naming a sorting nobody made of what the thread
/// now is. `everyday_store_sql::mail::write::recompute_thread` does that.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[serde(rename_all = "camelCase")]
pub struct SortedAs {
    /// [`SortCategory::id`]. One since deleted from the list names nothing,
    /// and is drawn as nothing.
    pub category: String,
    /// The message it was decided for: the thread's newest at the time.
    pub message: crate::id::MailMessageId,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_starting_list_is_valid_and_covers_every_tab_but_none() {
        let s = MailSorting::default();
        s.validate().unwrap();
        for tab in Category::ALL {
            assert!(
                s.categories.iter().any(|c| c.treat_as == tab),
                "{tab:?} has a category to land in"
            );
        }
    }

    #[test]
    fn names_must_differ_and_be_present() {
        let mut s = MailSorting::default();
        s.categories[1].name = " needs YOU ".into();
        let err = s.validate().unwrap_err().to_string();
        assert!(err.contains("two categories"), "{err}");

        let mut s = MailSorting::default();
        s.categories[0].name = "  ".into();
        assert!(s.validate().unwrap_err().to_string().contains("category 1 needs a name"));

        let mut s = MailSorting::default();
        s.categories[2].id = s.categories[0].id.clone();
        assert!(s.validate().unwrap_err().to_string().contains("own id"));

        let s = MailSorting { categories: vec![], ..MailSorting::default() };
        assert!(s.validate().is_err());

        let s = MailSorting { window_days: 0, ..MailSorting::default() };
        assert!(s.validate().is_err());
        let s = MailSorting { window_days: 365, ..MailSorting::default() };
        assert!(s.validate().is_err(), "a year back is sort_older_mail's to do");
    }

    #[test]
    fn an_answer_is_matched_by_name_as_written() {
        let s = MailSorting::default();
        assert_eq!(s.by_name("receipts").unwrap().id, "receipts");
        assert_eq!(s.by_name(" Needs you ").unwrap().treat_as, Category::Priority);
        assert!(s.by_name("Spam").is_none());
    }

    #[test]
    fn a_record_without_a_window_reads_with_the_default() {
        let s: MailSorting = serde_json::from_value(serde_json::json!({
            "categories": [{ "id": "a", "name": "A", "treatAs": "important" }]
        }))
        .unwrap();
        assert_eq!(s.window_days, DEFAULT_WINDOW_DAYS);
        assert_eq!(s.categories[0].description, "");
    }
}
