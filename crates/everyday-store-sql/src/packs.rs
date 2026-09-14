//! Raw messages, in rows, for a store with no local disk of its own.
//!
//! [`everyday_core::packstore`] frames many messages into one append-only
//! file so that a local vault pays for one `fsync` per batch rather than one
//! per message. A row in a table already buys that isolation for free --
//! Postgres never lets one row's write corrupt another's, and a `SELECT ...
//! WHERE id = ?1` never touches a neighbour's bytes -- so [`TablePacks`]
//! does not reinvent the framing `FilePackStore` needs. It stores one sealed
//! message per row in `mail_packs` and lets the database be the pack.
//!
//! # What `compact` does here
//!
//! Nothing. [`PackStore::compact`]'s whole job on a file backend is
//! rewriting a pack to reclaim the space dead frames still occupy inside it
//! -- there is no such space here, because [`TablePacks::mark_dead`] deletes
//! the row outright rather than flagging it. A deleted row is already
//! reclaimed by Postgres in its own time, the same way a deleted attachment
//! row in `blobs` is, and there is no pack left to remap addresses within.
//! `mail_packs` therefore has no "dead" column at all -- it would record a
//! state nothing here ever reads back.

use everyday_core::error::{Error, Result};
use everyday_core::id::PackId;
use everyday_core::packstore::{PackRef, PackStore};

use crate::conn::SqlExt;
use crate::{SqlStore, vals};

/// Associated data for one `mail_packs` row.
///
/// Binds the account as well as the row's own id, for the same reason
/// [`everyday_core::store::secrets::record_secret_aad`] binds an owner's
/// kind alongside its id: a bare `PackId` is a UUID, indistinguishable in
/// shape from any other account's, so folding the account in is what stops
/// a row copied into a different account's name from opening as though it
/// still belonged to the first.
fn row_aad(account: &str, id: PackId) -> Vec<u8> {
    format!("everyday.mailpack.row.v1:{account}:{id}").into_bytes()
}

/// The `mail_packs`-table [`PackStore`]. Borrows the store rather than
/// owning anything, the same shape [`crate::blobs`]'s `TableBlobs` takes and
/// for the same reason: everything it needs -- connections, the cipher --
/// already belongs to the [`SqlStore`] it is built from.
pub struct TablePacks<'s>(&'s SqlStore);

impl<'s> TablePacks<'s> {
    pub fn new(store: &'s SqlStore) -> Self {
        Self(store)
    }
}

impl PackStore for TablePacks<'_> {
    fn append_batch(&self, account: &str, messages: &[&[u8]]) -> Result<Vec<PackRef>> {
        if messages.is_empty() {
            return Ok(Vec::new());
        }
        let store = self.0;
        let mut conn = store.write();
        let mut tx = conn.begin()?;

        // `seq` is an arrival order, not an address -- nothing here is
        // looked up by it -- so one query for the account's current high
        // water mark, claimed for the whole batch, is enough; it does not
        // need to be re-read between inserts.
        let mut seq = tx.scalar_i64(
            "SELECT COALESCE(MAX(seq), 0) FROM mail_packs WHERE account_id = ?1",
            &vals![account],
        )?;

        let mut refs = Vec::with_capacity(messages.len());
        for msg in messages {
            let id = PackId::new();
            let sealed = store.cipher.seal(&row_aad(account, id), msg)?;
            seq += 1;
            let len = sealed.len() as u32;
            tx.execute(
                "INSERT INTO mail_packs (id, account_id, seq, data) VALUES (?1, ?2, ?3, ?4)",
                &vals![id.to_string(), account, seq, sealed],
            )?;
            refs.push(PackRef { account: account.to_string(), pack: id, offset: 0, len });
        }
        // One commit for the whole batch: the SQL equivalent of the one
        // `fsync` a file-backed pack promises.
        tx.commit()?;
        Ok(refs)
    }

    fn read(&self, r: &PackRef) -> Result<Vec<u8>> {
        let sealed = self
            .0
            .read()
            .sealed("SELECT data FROM mail_packs WHERE id = ?1", &vals![r.pack.to_string()])?
            .ok_or_else(|| Error::not_found("pack", r.pack))?;
        self.0.cipher.open(&row_aad(&r.account, r.pack), &sealed)
    }

    fn mark_dead(&self, refs: &[PackRef]) -> Result<()> {
        if refs.is_empty() {
            return Ok(());
        }
        let mut conn = self.0.write();
        let mut tx = conn.begin()?;
        for r in refs {
            tx.execute("DELETE FROM mail_packs WHERE id = ?1", &vals![r.pack.to_string()])?;
        }
        tx.commit()
    }

    fn compact(&self, _account: &str) -> Result<Vec<(PackRef, PackRef)>> {
        // See the module docs: a row is already its own pack, so there is
        // nothing to rewrite and nothing to remap.
        Ok(Vec::new())
    }
}
