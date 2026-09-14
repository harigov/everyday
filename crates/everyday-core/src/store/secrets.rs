//! Sealed secrets that belong to a single record, rather than to the vault.
//!
//! [`agent::AgentStore`](super::agent::AgentStore) has kept one credential
//! since version 6: an API key, sealed under its own associated data, in a
//! table pinned to one row by a `CHECK`. That was the right shape for a
//! vault with exactly one assistant. It stops being the right shape the
//! moment a vault can hold several things that each need a secret of their
//! own -- an account's refresh token, and later a calendar's bearer token or
//! a webhook's signing key -- because a singleton table cannot say *which*
//! account a credential belongs to, and a fresh singleton table per domain is
//! the same four methods copied once per caller.
//!
//! [`SecretStore`] is the generalisation: one table, keyed by *who the
//! secret belongs to* rather than by what kind of thing it protects. A
//! caller names an owner -- `("account", "7f3a...")`, say -- and gets back
//! exactly the bytes it stored, sealed under associated data that binds both
//! halves of that name to the ciphertext. `agent_secret` is left exactly as
//! it is: a second table for the same *kind* of fact would not simplify
//! anything, and there is only ever one assistant to ask.
//!
//! # Bytes, not a record
//!
//! Unlike every other store in this crate, [`SecretStore`] does not take a
//! `Serialize` type. A credential is usually already a string -- a refresh
//! token, a password -- and the caller decides its own encoding; forcing it
//! through JSON first would add a format this store has no opinion about and
//! nothing to check. `put_secret` seals whatever bytes it is handed and
//! `get_secret` hands the same bytes back, exactly, or `None`.
//!
//! # The associated data is the whole safety story
//!
//! [`record_secret_aad`] binds *both* the owner's kind and its id into the
//! ciphertext. That is what stops a secret sealed for one owner from being
//! substituted for another's by anyone who can write to the database: an
//! attacker with row access can copy account A's ciphertext into account B's
//! row, but decrypting it there fails, because the associated data no longer
//! matches what was sealed. See the conformance suite's
//! `a_secret_cannot_be_opened_under_a_different_owner` for the check.

use crate::error::Result;

/// Storage for secrets scoped to a single record, of any domain.
///
/// Reached the way every optional domain is -- see
/// [`JournalStore::secrets`](super::JournalStore::secrets) -- because a
/// backend with nowhere durable to put a credential (an in-memory store used
/// only for a read-only import, say) should say so rather than silently
/// losing one.
pub trait SecretStore: Send + Sync {
    /// Seal `bytes` for `(owner_kind, owner_id)`, replacing whatever was
    /// stored there before.
    ///
    /// `owner_kind` is a short, stable word -- `"account"`, `"agent"` -- and
    /// `owner_id` is that record's id rendered as a string, exactly as
    /// `Display` on a `typed_id!` newtype gives it. Neither is secret; both
    /// are folded into the associated data, never into a clear column, so a
    /// vault backed by a hosted database learns only that *some* record of
    /// that kind has a credential stored, never which fields are sealed
    /// beneath it.
    fn put_secret(&self, owner_kind: &str, owner_id: &str, bytes: &[u8]) -> Result<()>;

    /// The bytes stored for `(owner_kind, owner_id)`, or `None` if there are
    /// none.
    fn get_secret(&self, owner_kind: &str, owner_id: &str) -> Result<Option<Vec<u8>>>;

    /// Forget the secret stored for `(owner_kind, owner_id)`.
    ///
    /// A no-op, not an error, when there was nothing there -- the same
    /// contract [`agent::AgentStore::delete_secret`](super::agent::AgentStore::delete_secret)
    /// makes, and for the same reason: "make sure this is gone" should not
    /// have to check first.
    fn delete_secret(&self, owner_kind: &str, owner_id: &str) -> Result<()>;
}

/// Associated data binding a per-record secret to the one record it was
/// sealed for.
///
/// Both halves of the owner's name go in, not just the id: a bare id is a
/// UUID, indistinguishable in shape from any other record's, so folding in
/// `owner_kind` is what stops an account's secret and a future domain's
/// secret from colliding if the two ever mint the same id space differently.
/// See the module docs for what this defends against.
pub fn record_secret_aad(owner_kind: &str, owner_id: &str) -> Vec<u8> {
    format!("everyday.record-secret.v1:{owner_kind}:{owner_id}").into_bytes()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn aad_differs_by_kind_and_by_id() {
        let a = record_secret_aad("account", "1");
        let b = record_secret_aad("account", "2");
        let c = record_secret_aad("mailbox", "1");
        assert_ne!(a, b, "two owners of the same kind must not share associated data");
        assert_ne!(a, c, "two kinds sharing an id must not share associated data");
    }
}
