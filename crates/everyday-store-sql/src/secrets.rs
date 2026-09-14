//! A credential per record, generalising the singleton pattern `agent.rs`
//! uses for the assistant's one API key. See
//! [`everyday_core::store::secrets`] for the trait and the associated-data
//! argument; this file is the SQL half, and it is short because the table
//! is simple on purpose -- a composite primary key and one sealed column,
//! nothing denormalised beside it, since nothing here is ever searched or
//! sorted on.
//!
//! Sealed directly rather than through [`SqlStore::seal`]: that helper JSON-
//! encodes a `Serialize` value first, which is the right shape for a
//! structured record and the wrong one for a credential that is already
//! bytes -- a refresh token, a password -- and gains nothing from a trip
//! through JSON but a few escaped quotes.

use everyday_core::error::Result;
use everyday_core::store::secrets::{SecretStore, record_secret_aad};

use crate::conn::SqlExt;
use crate::{SqlStore, vals};

impl SecretStore for SqlStore {
    fn put_secret(&self, owner_kind: &str, owner_id: &str, bytes: &[u8]) -> Result<()> {
        let sealed = self.cipher.seal(&record_secret_aad(owner_kind, owner_id), bytes)?;
        self.write().execute(
            "INSERT INTO record_secrets (owner_kind, owner_id, data)
             VALUES (?1, ?2, ?3)
             ON CONFLICT (owner_kind, owner_id) DO UPDATE SET data = ?3",
            &vals![owner_kind, owner_id, sealed],
        )?;
        Ok(())
    }

    fn get_secret(&self, owner_kind: &str, owner_id: &str) -> Result<Option<Vec<u8>>> {
        let sealed = self.read().sealed(
            "SELECT data FROM record_secrets WHERE owner_kind = ?1 AND owner_id = ?2",
            &vals![owner_kind, owner_id],
        )?;
        match sealed {
            Some(sealed) => {
                Ok(Some(self.cipher.open(&record_secret_aad(owner_kind, owner_id), &sealed)?))
            }
            None => Ok(None),
        }
    }

    fn delete_secret(&self, owner_kind: &str, owner_id: &str) -> Result<()> {
        self.write().execute(
            "DELETE FROM record_secrets WHERE owner_kind = ?1 AND owner_id = ?2",
            &vals![owner_kind, owner_id],
        )?;
        Ok(())
    }
}
