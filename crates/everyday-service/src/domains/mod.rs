//! The domains, and the table they add up to.
//!
//! Adding an app to this application is adding a module here and a line to
//! [`CATALOG`]. Nothing else in the service learns about it: authorisation is
//! the [`Scope`](crate::ctx::Scope) each command declares, live refresh is the
//! [`Kind`](crate::events::Kind) it declares, and the generated client is built
//! from the signatures. There is no central match to extend and no map from a
//! command name to anything, which is the point -- a central map is exactly the
//! sort of list the fifth app forgets to update.

pub mod assistant;
pub mod calendars;
pub mod journals;
pub mod library;
pub mod meta;
pub mod notes;
pub mod purpose;
pub mod quick;
pub mod routines;
pub mod tasks;
pub mod trackers;
pub mod transfer;
pub mod vault;
pub mod web;

use crate::command::Command;
use serde::Deserialize;

/// The arguments of a command that takes none.
///
/// Here rather than in whichever domain happened to need it first. It was
/// `vault`'s, imported from there by eight other modules -- while two more had
/// quietly declared a second copy beside their own commands. One empty struct,
/// in the module every domain is already a child of.
///
/// A struct rather than `()`: arguments arrive as a JSON object, and an object
/// is what `{}` deserialises into.
#[derive(Deserialize)]
pub struct Nothing {}

/// Every command, in the order a person would read them.
///
/// Flattened once on first use rather than declared as one `static`: there is
/// no const way to concatenate slices, and a build script that generated one
/// would put the table somewhere nobody thinks to look.
pub fn catalog() -> &'static [&'static Command] {
    static ALL: std::sync::OnceLock<Vec<&'static Command>> = std::sync::OnceLock::new();
    ALL.get_or_init(|| {
        [
            vault::COMMANDS,
            journals::COMMANDS,
            tasks::COMMANDS,
            calendars::COMMANDS,
            library::COMMANDS,
            notes::COMMANDS,
            trackers::COMMANDS,
            purpose::COMMANDS,
            routines::COMMANDS,
            transfer::COMMANDS,
            web::COMMANDS,
            assistant::COMMANDS,
            quick::COMMANDS,
            meta::COMMANDS,
        ]
        .into_iter()
        .flatten()
        .collect()
    })
}
