//! Searching the web, and bringing a picture home.
//!
//! Its own scope and its own module rather than a corner of the library,
//! because it is a facility and not a feature of one app: anything may call
//! `web_search`. It is also the one capability that puts a request on the
//! network, which is why a client can be issued a token that reaches a shelf
//! and not this.

use crate::command;
use crate::ctx::Ctx;
use crate::error::CommandResult;
use crate::service::{Service, blocking};
use crate::websearch;
use everyday_core::ItemId;
use everyday_core::KindId;
use everyday_core::library::Item;
use everyday_core::websearch::{SearchRequest, SearchResult, Source};
use serde::{Deserialize, Serialize};
use std::sync::Arc;

use super::vault::Nothing;

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SourceInfo {
    pub id: String,
    pub label: String,
    pub has_images: bool,
}

impl SourceInfo {
    fn of(source: Source) -> Self {
        Self {
            id: source.slug().to_string(),
            label: source.label().to_string(),
            has_images: source.has_images(),
        }
    }
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Search {
    pub request: SearchRequest,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Lookup {
    pub kind_id: KindId,
    pub query: String,
    #[serde(default)]
    pub limit: Option<u32>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Apply {
    pub id: ItemId,
    pub result: SearchResult,
    pub overwrite: bool,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct FetchImage {
    pub url: String,
}

/// Search the web. The general entry point; any part of the app may call it.
///
/// Takes a whole [`SearchRequest`] rather than a bare string so that the choice
/// of source, the kind hint and the limit are the caller's, and so that adding
/// a source later is not a new command.
async fn web_search(svc: Arc<Service>, _c: Ctx, args: Search) -> CommandResult<Vec<SearchResult>> {
    // A locked vault is not a technical obstacle to a search -- nothing here
    // touches storage -- but it is the wrong moment for one. The lock screen
    // must not be a place from which requests leave the machine.
    let _ = svc.require_unlocked()?;
    websearch::search(&args.request).await
}

/// The sources a search can be run against, for the picker.
async fn search_sources(_s: Arc<Service>, _c: Ctx, _a: Nothing) -> CommandResult<Vec<SourceInfo>> {
    Ok(Source::ALL.iter().map(|s| SourceInfo::of(*s)).collect())
}

/// Look a title up using whatever source a shelf prefers, falling back to a
/// plain web search when that source draws a blank.
async fn lookup_metadata(
    svc: Arc<Service>,
    _c: Ctx,
    args: Lookup,
) -> CommandResult<Vec<SearchResult>> {
    let vault = svc.require_unlocked()?;
    let kind_id = args.kind_id;
    let kind = {
        let vault = vault.clone();
        blocking(move || Ok(vault.kind(kind_id)?)).await?
    };
    websearch::lookup(args.query.trim(), &kind, args.limit.unwrap_or(websearch::DEFAULT_LIMIT))
        .await
}

/// Apply a chosen result to an item, downloading its cover on the way.
///
/// `overwrite` is the "yes, replace what is there" offered on an explicit
/// re-fetch. Even then it leaves notes, your rating and the status alone -- see
/// `everyday_core::websearch::apply`, which is where that rule lives and is
/// tested.
async fn apply_metadata(svc: Arc<Service>, _c: Ctx, args: Apply) -> CommandResult<Item> {
    let vault = svc.require_unlocked()?;
    let id = args.id;
    let (mut item, kind) = {
        let vault = vault.clone();
        blocking(move || {
            let item = vault.item(id)?;
            let kind = vault.kind(item.kind_id)?;
            Ok((item, kind))
        })
        .await?
    };
    websearch::apply_and_cover(&vault, &args.result, &kind, &mut item, args.overwrite).await;

    let saved = item.clone();
    let vault = vault.clone();
    blocking(move || Ok(vault.save_item(&saved)?)).await?;
    Ok(item)
}

/// Download a picture into the vault and return its content address.
///
/// The general entry point, beside `web_search`: anything that has found an
/// image address can put it in the blob store with this, and it comes back as a
/// blob id the `everyday://` protocol will serve. Nothing in the interface ever
/// loads a remote image directly -- see `websearch.rs` for why, and the content
/// security policy for the check that outlives the reason.
async fn fetch_image(svc: Arc<Service>, _c: Ctx, args: FetchImage) -> CommandResult<String> {
    // Checked *before* the fetch, like `web_search`, and not left to the blob
    // store to refuse afterwards. Without this a locked vault still put a
    // request on the wire and only failed once the answer came back, which is
    // precisely the thing the rule exists to prevent.
    let vault = svc.require_unlocked()?;
    let bytes = websearch::fetch_image(&args.url).await?;
    blocking(move || Ok(vault.put_blob(&bytes)?.to_hex())).await
}

pub static COMMANDS: &[crate::command::Command] = &[
    command! {
        name: "web_search", scope: Web, effect: Read,
        args: Search, returns: "SearchResult[]",
        signature: &[("request", "SearchRequest", true)],
        run: web_search,
    },
    command! {
        name: "search_sources", scope: Web, effect: Read,
        args: Nothing, returns: "SourceInfo[]", signature: &[],
        run: search_sources,
    },
    command! {
        name: "lookup_metadata", scope: Web, effect: Read,
        args: Lookup, returns: "SearchResult[]",
        signature: &[
            ("kindId", "KindId", true),
            ("query", "string", true),
            ("limit", "number | null", false),
        ],
        run: lookup_metadata,
    },
    command! {
        name: "apply_metadata", scope: Web, effect: Write,
        change: Item / Updated,
        args: Apply, returns: "Item",
        signature: &[
            ("id", "ItemId", true),
            ("result", "SearchResult", true),
            ("overwrite", "boolean", true),
        ],
        run: apply_metadata,
    },
    command! {
        name: "fetch_image", scope: Web, effect: Write,
        args: FetchImage, returns: "string",
        signature: &[("url", "string", true)],
        run: fetch_image,
    },
];
