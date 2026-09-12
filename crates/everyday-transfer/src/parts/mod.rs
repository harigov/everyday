//! One module per app, each implementing [`Portable`](crate::Portable) once.
//!
//! They are listed in [`PARTS`](crate::PARTS) and nowhere else. Two of them
//! share [`doc`], which is the rich-document half the journal and the
//! notebook have in common; several more share [`index`], the two shapes
//! every "one file per record, plus an index CSV describing them" part
//! needed; the rest have nothing to say to each other, which is the property
//! that makes adding a sixth app a new file rather than an edit to five.

pub mod assistant;
pub mod calendar;
pub mod doc;
pub mod index;
pub mod journal;
pub mod library;
pub mod notes;
pub mod purpose;
pub mod tasks;
pub mod trackers;
pub mod you;
