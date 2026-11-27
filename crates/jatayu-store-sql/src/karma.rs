//! When Karma started -- see `jatayu_core::karma::KarmaBook`.
//!
//! A singleton with one sealed column and nothing in the clear, on the
//! pattern of `mail_voice_learning`.

use jatayu_core::error::Result;
use jatayu_core::karma::{KarmaBook, book_aad};

use crate::SqlStore;

impl SqlStore {
    pub(crate) fn read_karma_book(&self) -> Result<Option<KarmaBook>> {
        self.get_singleton("karma_book", &book_aad())
    }

    pub(crate) fn write_karma_book(&self, book: &KarmaBook) -> Result<()> {
        self.put_singleton("karma_book", &book_aad(), book)
    }
}
