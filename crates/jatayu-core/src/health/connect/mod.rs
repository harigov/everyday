//! Connectors: what turns somebody else's format into a [`HealthBatch`].
//!
//! Every connector is a pure reader -- bytes in, a batch out -- so each is
//! tested on fixtures without a vault, and the vault has exactly one way in
//! (`Vault::ingest_health`) whatever the data came through. A new connector
//! (Oura, Withings, Dexcom, FIT files) is a module here, a
//! [`Connector`](super::Connector) variant, and whatever fetches its bytes.
//!
//! [`HealthBatch`]: super::HealthBatch

pub mod apple;
pub mod fhir;
pub mod hae;
