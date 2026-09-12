//! The assistant's tool catalogue, end to end.
//!
//! These run the real catalogue against a real vault. Everything the
//! assistant can do to somebody's data goes through `dispatch`, so this is
//! where "it marked the wrong task done" is caught -- offline, with no API
//! key, and independently of whichever harness is calling it.
//!
//! Split by domain the way `everyday_core::agent::tools` itself is; see
//! `tools/mod.rs` for the tests that are not any one domain's -- the
//! confirmation gate, the read-only rule, and an unknown tool name.

#[path = "tools/mod.rs"]
mod tools;
