//! Health: samples in through one door, reports out, and the records a
//! person keeps by hand.
//!
//! Everything goes through [`Vault::with_health`], which fails with
//! `unsupported` on a backend that holds journals only.
//!
//! - [`data`]: settings, sources, ingest, series and the reports drawn from
//!   them (activity, sleep, glucose, today), workouts, the food-lookup keys,
//!   and the readings health trackers derive.
//! - [`care`]: medications and doses, allergies, conditions and their
//!   templates, lab results, documents, foods and meals, and the facts a
//!   visit brief is written from.
//!
//! # Read-modify-write
//!
//! Filing samples into a series day, matching a dose event to a
//! medication, recording a dose change in a medication's history: each
//! reads before it writes, so each does both inside one [`Vault::write`],
//! which is exclusive.

use super::Vault;
use super::session::Domain;
use crate::error::Result;
use crate::store::health::HealthStore;

mod care;
mod data;

pub(crate) use care::{file_documents, file_source_doses};

impl Vault {
    /// Does this vault's backend store health data?
    pub fn supports_health(&self) -> bool {
        self.with_health(|_| Ok(())).is_ok()
    }

    pub(super) fn with_health<T>(
        &self,
        f: impl FnOnce(&dyn HealthStore) -> Result<T>,
    ) -> Result<T> {
        self.with_domain(Domain::Health, |s| s.health().map(f))
    }
}
