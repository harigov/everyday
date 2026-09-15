//! Mail search: `tantivy` behind a sealed [`directory::SealedDirectory`],
//! kept in a crate of its own so its build cost and its dependency tree are
//! paid for once.
//!
//! # Why tantivy, and why not `everyday-core::search`
//!
//! `everyday-core::search` is an in-memory inverted index, rebuilt whole on
//! every unlock — right for a journal's few thousand entries, wrong for a
//! mailbox's hundred thousand messages for exactly the reasons that
//! module's own docs give (see it, and `everyday_core::mailsearch`'s module
//! docs, before touching either). Mail's index has to live on disk, survive
//! a lock/unlock cycle without being rebuilt, and answer a page of results
//! inside the 150 ms budget `docs/plans/mail.md` sets — which is a real
//! search engine's job, not a hand-rolled one's. tantivy is that engine:
//! embedded (a library linked in, not a service to run), incremental
//! (commits merge rather than rebuilding), and — the property this crate
//! exists to use — built around segment *files*, immutable once written,
//! which is exactly the shape [`directory::SealedDirectory`] needs in order
//! to seal each one once and cache it decrypted rather than re-deriving
//! plaintext on every read.
//!
//! # Layout
//!
//! - [`directory`] — the sealed `Directory` tantivy writes and reads
//!   through, and the whole reason this crate is not simply `tantivy`
//!   re-exported.
//! - [`cache`] — the size-capped store of segments this session has already
//!   decrypted, used by `directory`.
//! - [`tokenizer`] — the address-aware tokenizer `from`, `to` and `cc` are
//!   indexed with.
//! - [`schema`] — the tantivy schema every document is indexed under.
//! - [`query`] — translates [`everyday_core::MailQuery`] into the boolean,
//!   term, phrase and range queries tantivy understands.
//! - [`index`] — [`index::MailIndex`], the [`everyday_core::MailSearch`]
//!   implementation this crate exists to provide.

pub mod cache;
pub mod directory;
pub mod index;
pub mod query;
pub mod schema;
pub mod tokenizer;

pub use directory::SealedDirectory;
pub use index::MailIndex;
