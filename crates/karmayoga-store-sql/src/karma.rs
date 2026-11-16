//! When Karma started -- see `karmayoga_core::karma::KarmaBook`.
//!
//! A singleton with one sealed column and nothing in the clear, on the
//! pattern of `mail_voice_learning`.

use karmayoga_core::error::Result;
use karmayoga_core::karma::{KarmaBook, book_aad};

use crate::conn::SqlExt;
use crate::{SqlStore, vals};

impl SqlStore {
    pub(crate) fn read_karma_book(&self) -> Result<Option<KarmaBook>> {
        match self.read().sealed("SELECT data FROM karma_book WHERE id = 1", &[])? {
            Some(sealed) => Ok(Some(self.unseal(&book_aad(), &sealed)?)),
            None => Ok(None),
        }
    }

    pub(crate) fn write_karma_book(&self, book: &KarmaBook) -> Result<()> {
        let data = self.seal(&book_aad(), book)?;
        self.write().execute(
            "INSERT INTO karma_book (id, data) VALUES (1, ?1)
             ON CONFLICT (id) DO UPDATE SET data = ?1",
            &vals![data],
        )?;
        Ok(())
    }
}
