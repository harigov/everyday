//! The tool surface, written down.
//!
//! `mcp.json` is the tool catalogue the way an MCP client -- or a palette, or
//! a model -- sees it: every tool's name, title, effect, domain, the scope
//! it is gated behind, whether it is one a model may ever be shown, and its
//! full argument schema. It is committed, this test compares the catalogue
//! against it, so a tool added to `ALL` in `tools.rs` shows up here as a
//! reviewable diff with no other edit required, and a tool quietly dropped
//! from it shows up as one too -- the snapshot notices what went missing
//! exactly the way `surface.rs` notices a command going missing.
//!
//! It also asserts the invariants MCP itself requires of a tool, so one that
//! breaks them cannot ship even if nobody thought to look: a name of 1 to
//! 128 characters matching `[A-Za-z0-9_.-]+`, parameters that are always a
//! JSON object -- never `null`, never something else shaped -- and no two
//! tools sharing a name.
//!
//! To accept a change: `UPDATE_SURFACE=1 cargo test -p everyday-service
//! --test mcp`.

use std::path::PathBuf;

use everyday_core::agent::tools::{self, Effect, Sensitivity};
use everyday_service::domains::meta::{scope_of, title_of};

fn snapshot_path() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("mcp.json")
}

/// A tool's domain, the way this file spells it -- lower case, matching
/// every other enum rendered here. `Domain` carries no such spelling of its
/// own, because nothing inside `everyday-core` needed one until now.
fn domain_name(domain: tools::Domain) -> String {
    format!("{domain:?}").to_lowercase()
}

fn effect_name(effect: Effect) -> &'static str {
    match effect {
        Effect::Read => "read",
        Effect::Write => "write",
        Effect::Destructive => "destructive",
    }
}

fn sensitivity_name(sensitivity: Sensitivity) -> &'static str {
    match sensitivity {
        Sensitivity::Ordinary => "ordinary",
        Sensitivity::Secret => "secret",
    }
}

/// The catalogue as JSON, in a stable order.
fn current() -> String {
    let mut entries: Vec<serde_json::Value> = tools::catalog()
        .iter()
        .map(|t| {
            serde_json::json!({
                "name": t.name,
                "title": title_of(t.name),
                "effect": effect_name(t.effect),
                "domain": domain_name(t.domain),
                "scope": scope_of(t.domain).as_str(),
                "sensitivity": sensitivity_name(t.domain.sensitivity()),
                "parameters": t.parameters(),
            })
        })
        .collect();
    // Sorted, so reordering the catalogue for a person reading it top to
    // bottom is not a wire change.
    entries.sort_by(|a, b| a["name"].as_str().cmp(&b["name"].as_str()));

    let doc = serde_json::json!({ "tools": entries });
    format!("{}\n", serde_json::to_string_pretty(&doc).unwrap())
}

#[test]
fn the_tool_surface_is_what_was_written_down() {
    let path = snapshot_path();
    let current = current();

    if std::env::var("UPDATE_SURFACE").is_ok() {
        std::fs::write(&path, &current).expect("could not write the tool snapshot");
        return;
    }

    let recorded = std::fs::read_to_string(&path).unwrap_or_default();
    if recorded != current {
        panic!(
            "the tool surface has changed.\n\n\
             This is what an MCP client sees, and what a model reads to choose between \
             tools, so the change is worth looking at rather than accepting -- especially \
             a tool that has gone missing, which this snapshot is the only thing that would \
             notice.\n\n\
             Then: UPDATE_SURFACE=1 cargo test -p everyday-service --test mcp\n"
        );
    }
}

/// MCP's own rule for a tool name, checked here so a tool that would be
/// silently unreachable over the protocol fails a build instead.
fn valid_mcp_name(name: &str) -> bool {
    let len = name.chars().count();
    (1..=128).contains(&len)
        && name.chars().all(|c| c.is_ascii_alphanumeric() || matches!(c, '_' | '.' | '-'))
}

#[test]
fn every_tool_name_is_one_mcp_will_accept() {
    for tool in tools::catalog() {
        assert!(
            valid_mcp_name(tool.name),
            "{:?} is not a valid MCP tool name: 1-128 characters of [A-Za-z0-9_.-]",
            tool.name
        );
    }
}

/// A model is handed `inputSchema` straight from [`tools::Tool::parameters`],
/// so it has to be an object schema every time -- a tool with nothing to say
/// still declares an empty one rather than `null`, which a client offering
/// strict function calling would reject outright.
#[test]
fn every_tools_parameters_are_an_object_schema() {
    for tool in tools::catalog() {
        let schema = tool.parameters();
        assert_eq!(
            schema.get("type").and_then(|v| v.as_str()),
            Some("object"),
            "{}: parameters() must be a JSON object schema, got {schema}",
            tool.name
        );
    }
}

#[test]
fn no_tool_is_declared_twice() {
    let mut seen = std::collections::HashSet::new();
    for tool in tools::catalog() {
        assert!(seen.insert(tool.name), "{} is declared twice", tool.name);
    }
}
