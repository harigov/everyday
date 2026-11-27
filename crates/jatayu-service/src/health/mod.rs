//! Health's service side: what needs the network, the models, a background
//! task or the clock -- everything the vault cannot do from inside a lock.
//!
//! - [`import`]: an Apple Health `export.zip` read on a thread of its own,
//!   with progress and cancel.
//! - [`reminders`]: dose reminders and quiet-source notices, swept by the
//!   scheduler every minute.
//! - [`food`]: food search and barcode lookup (USDA FoodData Central, Open
//!   Food Facts).
//! - [`extract`]: lab results read out of a clinical document by the large
//!   model.
//! - [`photo`]: a meal estimated from a photo by the medium model.
//! - [`brief`]: a visit summary written by the large model from the vault's
//!   own numbers.

pub mod brief;
pub mod extract;
pub mod food;
pub mod import;
pub mod photo;
pub mod reminders;
