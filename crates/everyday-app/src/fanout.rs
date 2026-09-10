//! Combining this window's own sink with whatever else has something to say.
//!
//! [`crate::sharing::Sharing`] and [`crate::mcp::Mcp`] are independent
//! switches -- either, neither or both may be on -- and each contributes its
//! own [`EventSink`] when it is: a broadcaster fanning writes out to paired
//! devices, a stream telling an open MCP session the vault just unlocked.
//! Neither module knows the other exists, on purpose, so this is the one
//! place that reconciles them: a caller in `commands.rs` asks each lifecycle
//! for its current sink and hands the answers here, rather than the two
//! lifecycles reaching into each other's state.

use everyday_service::events::EventSink;
use std::sync::Arc;

/// `window`, plus whatever `other` sinks are live right now.
///
/// A bare `window` is handed back unwrapped when `other` is empty, rather
/// than wrapped in a one-element [`everyday_server::Fanout`] -- the ordinary
/// case is neither switch on, and that case should cost nothing beyond what
/// the window already had.
pub fn compose(window: Arc<dyn EventSink>, other: Vec<Arc<dyn EventSink>>) -> Arc<dyn EventSink> {
    if other.is_empty() {
        return window;
    }
    let mut sinks = vec![window];
    sinks.extend(other);
    Arc::new(everyday_server::Fanout::new(sinks))
}
