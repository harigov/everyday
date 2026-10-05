//! Past conversations: finding one by what was said, and reading it back.
//!
//! # Memory is not this
//!
//! [`crate::agent::Memory`] is a standing fact distilled out of something
//! that was said -- "plans the week on Sunday evening" -- and it is loaded
//! into every prompt in full for exactly that reason: it is short, and it is
//! always true. These two tools are for the opposite case, where the
//! distillation is not what is wanted and the actual exchange is: "what did
//! we decide about the deck", "did I already ask about this". There is no
//! tool for turning one into the other -- a model that thinks a past
//! exchange is worth keeping as a standing fact still reaches for
//! `remember`, same as always. `search_conversations` finds the thread;
//! `read_conversation` reads it back.
//!
//! # Why this is a scan, not an index
//!
//! A conversation's messages are sealed -- see the module docs on
//! [`crate::store::agent`] -- so there is nothing for a database index to
//! read until a message has already been decrypted, which rules out the
//! trick [`crate::search`] plays for entries and notes. `search_conversations`
//! answers its query by opening candidate conversations, newest first, and
//! scanning their messages in the application. [`CONVERSATION_SCAN_CAP`] is
//! the one number that keeps that bounded, and the tool's own description
//! says "recent history" rather than promising a search of everything ever
//! said, because past that cap it genuinely is not one.
//!
//! # What is left out, and why
//!
//! A routine run and a dream both hold their own transcript -- see
//! [`crate::agent::Conversation::run_id`] -- and neither is a conversation
//! anybody had: nobody typed into it, and a week of morning briefs or
//! overnight dreams is not what "what did we discuss" means. Both tools
//! reach the vault through [`ConversationQuery::chats`], which leaves them
//! out the same way the history pane already does, so there is no separate
//! flag here to forget to set. `search_conversations` also leaves out the
//! very conversation the call was made from: it is the one thing on the
//! screen already, and finding it would only report back what the model
//! just said.
//!
//! # What a reply carries
//!
//! A tool call's own arguments and a tool result's own payload are not "what
//! was said" either -- they are the machinery underneath it, not either
//! side of the conversation -- so `read_conversation` keeps only the name
//! of each tool a turn called and drops the rest, and does not report a
//! [`Role::Tool`] message or a [`Role::System`] one at all. Every message
//! that does come back is cut to [`MAX_MESSAGE_CHARS`], for the reason every
//! cap in this crate exists: the model reading the reply pays for it.

use serde_json::{Value, json};

use super::{
    Args, Caller, Tool, ToolContext, day, limit_arg, load_by_id, number, schema, text,
    truncate_chars,
};
use crate::agent::{Conversation, Message, Role};
use crate::error::Result;
use crate::id::ConversationId;
use crate::model::local_date_in;
use crate::store::agent::ConversationQuery;

/// Most conversations `search_conversations` opens and scans looking for a
/// match, newest first -- see the module docs' "Why this is a scan, not an
/// index". Large enough that an ordinary person's last few months of chats
/// fit inside it, and fixed regardless of `limit` so that a larger `limit`
/// cannot turn one search into a scan of a vault's entire history.
const CONVERSATION_SCAN_CAP: u32 = 300;

/// Most excerpts `search_conversations` keeps for one conversation. Enough
/// to show that it is the right thread without reprinting most of it.
const MAX_EXCERPTS_PER_CONVERSATION: usize = 3;

/// Characters of context kept either side of a match in one of
/// `search_conversations`'s excerpts -- the same job
/// [`crate::search`]'s own `SNIPPET_RADIUS` does for a journal hit, kept
/// separately because a conversation is scanned message by message here
/// rather than through that shared index.
const EXCERPT_RADIUS: usize = 80;

/// Longest a single message's text may be in `read_conversation`'s reply,
/// characters not bytes -- see [`truncate_chars`]. A real turn can run to
/// several paragraphs; this is what keeps one long reply from spending the
/// whole of the calling model's budget retelling itself, the same trade
/// [`crate::agent::MAX_MEMORY_CHARS`] makes for a memory and
/// [`crate::proposal::MAX_WHY_CHARS`] makes for a proposal's reason.
const MAX_MESSAGE_CHARS: usize = 1_500;

pub(super) static TOOLS: &[Tool] = &[
    tool!(
        "search_conversations",
        Read,
        Agent,
        schema(
            vec![
                (
                    "query",
                    text(
                        "Case-insensitive substring to look for in what either of you said. \
                         Omit to just list recent conversations."
                    )
                ),
                ("from", day("Earliest day the conversation started, inclusive.")),
                ("to", day("Latest day the conversation started, inclusive.")),
                limit_arg(),
            ],
            &[]
        ),
        "Find a past conversation with this person by what was said in it, or list \
         recent ones with no query. For recovering an actual discussion \u{2014} 'what \
         did we decide about the deck', 'did I already ask about this' \u{2014} rather \
         than a fact distilled from one, which is what your memories already hold \
         without a tool call. Searches recent history only, newest first, and never \
         finds the conversation you are in right now, a routine run, or a dream's own \
         transcript. Call read_conversation with the id this gives you to see the \
         whole thing.",
        run_search_conversations
    ),
    tool!(
        "read_conversation",
        Read,
        Agent,
        schema(
            vec![
                ("conversation_id", text("Id from search_conversations.")),
                (
                    "limit",
                    number(
                        "Most recent messages to keep; earlier ones in the thread are left \
                         out. Defaults to 50, capped at 200."
                    )
                ),
            ],
            &["conversation_id"]
        ),
        "Read back a past conversation found with search_conversations: what the \
         person said and what you said, in order, with the name of any tool you \
         called but not its arguments or result. Use this once search_conversations \
         has told you which conversation to look at.",
        run_read_conversation
    ),
];

/// The conversation this call is being made from, if the caller is the chat
/// assistant -- so `search_conversations` can leave it out of its own
/// results. See the module docs' "What is left out, and why".
///
/// `None` for every other caller -- an MCP client, a script, the vault's
/// owner acting directly -- none of which is sitting in a thread this search
/// could be asked to exclude.
fn current_conversation(ctx: &ToolContext<'_>) -> Option<ConversationId> {
    match &ctx.caller {
        Some(Caller::Assistant { conversation }) => Some(*conversation),
        _ => None,
    }
}

/// One `search_conversations` hit: enough to recognise the thread and decide
/// whether to read the rest of it with `read_conversation`.
fn conversation_hit_json(
    c: &Conversation,
    local_date: jiff::civil::Date,
    excerpts: &[String],
) -> Value {
    let mut v = json!({
        "id": c.id.to_string(),
        "title": c.title,
        "date": local_date.to_string(),
    });
    if !excerpts.is_empty() {
        v.as_object_mut().expect("built as an object").insert("excerpts".into(), json!(excerpts));
    }
    v
}

fn run_search_conversations(ctx: &ToolContext<'_>, args: &Args<'_>) -> Result<Value> {
    // Folded once, here, rather than per candidate message -- see
    // `excerpt`'s own doc for why this is ASCII-only rather than
    // `str::to_lowercase`.
    let query = args.opt_str("query").map(|q| q.to_ascii_lowercase());
    let from = args.opt_date("from")?;
    let to = args.opt_date("to")?;
    if let (Some(from), Some(to)) = (from, to)
        && to < from
    {
        return Err(args.bad("`to` is before `from`"));
    }
    let limit = args.limit() as usize;
    let exclude = current_conversation(ctx);

    let candidates = ctx.vault.conversations(&ConversationQuery::chats(CONVERSATION_SCAN_CAP))?;

    let mut hits = Vec::new();
    for c in candidates {
        if Some(c.id) == exclude {
            continue;
        }
        let local_date = local_date_in(c.created_at, ctx.tz);
        if from.is_some_and(|f| local_date < f) || to.is_some_and(|t| local_date > t) {
            continue;
        }

        let excerpts = match &query {
            Some(q) => {
                let messages = ctx.vault.messages(c.id)?;
                let found = excerpts_for(&messages, q, MAX_EXCERPTS_PER_CONVERSATION);
                if found.is_empty() {
                    continue;
                }
                found
            }
            None => Vec::new(),
        };

        hits.push(conversation_hit_json(&c, local_date, &excerpts));
        if hits.len() >= limit {
            break;
        }
    }

    Ok(json!({ "count": hits.len(), "conversations": hits }))
}

/// The excerpts to show for one conversation: a window of text around each
/// case-insensitive match of `query_lower` (already folded to ASCII
/// lowercase) in a person's or the assistant's own message, in the order
/// the messages happened, capped at `cap`. A tool call's own arguments and a
/// tool result's own payload are never searched -- see the module docs'
/// "What a reply carries" -- only [`Role::User`] and [`Role::Assistant`]
/// text is.
fn excerpts_for(messages: &[Message], query_lower: &str, cap: usize) -> Vec<String> {
    let mut out = Vec::new();
    for m in messages {
        if !matches!(m.role, Role::User | Role::Assistant) {
            continue;
        }
        if let Some(e) = excerpt(&m.content, query_lower) {
            out.push(e);
            if out.len() >= cap {
                break;
            }
        }
    }
    out
}

/// A window of `text` around the first case-insensitive match of
/// `query_lower`, or `None` if it does not occur there at all.
///
/// Case-folded with `str::to_ascii_lowercase` rather than `str::to_lowercase`,
/// and that narrowing is deliberate: a handful of
/// Unicode characters lowercase to a different number of bytes than they
/// started with, which would leave the byte offset a match was found at in
/// the lowered copy pointing at the wrong place -- or no valid place at all
/// -- in `text` itself. ASCII lowercasing never changes a string's length,
/// so the lowered copy and `text` stay aligned byte for byte and a found
/// offset is always safe to slice `text` with directly. The cost is that a
/// query in a script with its own case distinctions (Cyrillic, Greek,
/// accented Latin) only matches text already written in the matching case --
/// a worse search than full Unicode folding, and a safe trade for one
/// excerpt cut out of a sealed chat message rather than a reason to reach
/// for [`crate::search`]'s own tokenizer, which exists to rank entries and
/// notes, not to window a substring match.
///
/// `query_lower` must already be non-empty and lowercase; an empty needle
/// would otherwise report a match at the very start of every message, which
/// is never what "search for nothing" should mean.
fn excerpt(text: &str, query_lower: &str) -> Option<String> {
    if query_lower.is_empty() {
        return None;
    }
    let lower = text.to_ascii_lowercase();
    let at = lower.find(query_lower)?;
    let end_of_match = at + query_lower.len();

    let start = floor_char_boundary(text, at.saturating_sub(EXCERPT_RADIUS));
    let end = ceil_char_boundary(text, (end_of_match + EXCERPT_RADIUS).min(text.len()));

    let mut out = String::new();
    if start > 0 {
        out.push('\u{2026}');
    }
    out.push_str(text[start..end].trim());
    if end < text.len() {
        out.push('\u{2026}');
    }
    Some(out)
}

/// The same trick `crate::search`'s identically-named pair of helpers use,
/// copied rather than shared because they are private there: round a byte
/// offset down to the nearest character boundary, so a window cut into
/// `text` never lands mid-character.
fn floor_char_boundary(s: &str, mut n: usize) -> usize {
    if n >= s.len() {
        return s.len();
    }
    while n > 0 && !s.is_char_boundary(n) {
        n -= 1;
    }
    n
}

/// As [`floor_char_boundary`], rounding up instead.
fn ceil_char_boundary(s: &str, mut n: usize) -> usize {
    if n >= s.len() {
        return s.len();
    }
    while n < s.len() && !s.is_char_boundary(n) {
        n += 1;
    }
    n
}

fn run_read_conversation(ctx: &ToolContext<'_>, args: &Args<'_>) -> Result<Value> {
    let conversation: Conversation =
        load_by_id(args, "conversation_id", "conversation", |id| ctx.vault.conversation(id))?;
    let mut messages = ctx.vault.messages(conversation.id)?;

    // Messages come back oldest first; keeping only the latest `limit` means
    // dropping off the front, not the back.
    let limit = args.limit() as usize;
    if messages.len() > limit {
        let drop = messages.len() - limit;
        messages.drain(..drop);
    }

    Ok(json!({
        "id": conversation.id.to_string(),
        "title": conversation.title,
        "messages": messages.iter().filter_map(message_json).collect::<Vec<_>>(),
    }))
}

/// One turn of `read_conversation`'s reply, or `None` for a message that is
/// not a side of the conversation either of you had -- a tool's own result
/// or an application notice. See the module docs' "What a reply carries".
fn message_json(m: &Message) -> Option<Value> {
    let role = match m.role {
        Role::User => "person",
        Role::Assistant => "assistant",
        Role::Tool | Role::System => return None,
    };
    let mut row = json!({ "role": role });
    let map = row.as_object_mut().expect("built as an object");
    let text = m.content.trim();
    if !text.is_empty() {
        map.insert("text".into(), json!(truncate_chars(text, MAX_MESSAGE_CHARS)));
    }
    if !m.tool_calls.is_empty() {
        map.insert(
            "tools_called".into(),
            json!(m.tool_calls.iter().map(|t| t.name.as_str()).collect::<Vec<_>>()),
        );
    }
    Some(row)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::agent::ToolCall;

    fn msg(role: Role, text: &str) -> Message {
        Message::new(ConversationId::new(), role, text)
    }

    // ---- excerpt extraction ------------------------------------------------

    #[test]
    fn finds_a_match_and_keeps_context_either_side() {
        let text = "we talked about whether the deck needs a second coat before winter";
        let hit = excerpt(text, "deck").unwrap();
        assert!(hit.contains("deck"), "{hit}");
        assert!(!hit.contains('\u{2026}'), "the whole sentence fits inside the radius: {hit}");
    }

    #[test]
    fn marks_a_truncated_excerpt_at_the_cut_ends() {
        let text = format!("{} deck {}", "x ".repeat(200), "y ".repeat(200));
        let hit = excerpt(&text, "deck").unwrap();
        assert!(hit.starts_with('\u{2026}'), "{hit}");
        assert!(hit.ends_with('\u{2026}'), "{hit}");
    }

    #[test]
    fn matching_is_case_insensitive() {
        assert!(excerpt("the DECK needs painting", "deck").is_some());
    }

    #[test]
    fn no_match_is_none() {
        assert!(excerpt("nothing relevant here", "deck").is_none());
    }

    #[test]
    fn an_empty_query_never_matches() {
        // Without this guard, an empty needle would report a hit at the
        // front of every message -- never what "search for nothing" means.
        assert!(excerpt("anything at all", "").is_none());
    }

    #[test]
    fn excerpts_for_only_reads_what_either_of_you_said() {
        let messages = vec![
            msg(Role::User, "what did we decide about the deck"),
            msg(Role::Tool, "deck deck deck"), // a tool result must not count
            msg(Role::System, "deck"),
            msg(Role::Assistant, "you decided to reseal the deck in spring"),
        ];
        let found = excerpts_for(&messages, "deck", 10);
        assert_eq!(found.len(), 2, "only the person's and the assistant's own messages count");
    }

    #[test]
    fn excerpts_for_stops_at_the_cap() {
        let messages: Vec<Message> =
            (0..10).map(|i| msg(Role::User, &format!("deck {i}"))).collect();
        assert_eq!(excerpts_for(&messages, "deck", 3).len(), 3);
    }

    // ---- read_conversation's reply -----------------------------------------

    #[test]
    fn message_json_truncates_long_text() {
        let long = "x".repeat(MAX_MESSAGE_CHARS * 2);
        let row = message_json(&msg(Role::Assistant, &long)).unwrap();
        assert_eq!(row["text"].as_str().unwrap().chars().count(), MAX_MESSAGE_CHARS);
    }

    #[test]
    fn message_json_drops_tool_and_system_messages() {
        assert!(message_json(&msg(Role::Tool, "result")).is_none());
        assert!(message_json(&msg(Role::System, "the vault was locked")).is_none());
    }

    #[test]
    fn message_json_names_tools_called_but_not_their_arguments() {
        let mut m = msg(Role::Assistant, "");
        m.tool_calls = vec![ToolCall {
            id: "call_1".into(),
            name: "create_task".into(),
            arguments: json!({ "title": "secret plan" }),
        }];
        let row = message_json(&m).unwrap();
        assert_eq!(row["tools_called"], json!(["create_task"]));
        assert!(row.get("text").is_none(), "an empty assistant turn has nothing to show");
        let rendered = row.to_string();
        assert!(rendered.contains("create_task"), "{rendered}");
        assert!(!rendered.contains("secret plan"), "arguments must not leak: {rendered}");
    }

    // ---- exclusion of the current conversation -----------------------------

    #[test]
    fn current_conversation_is_set_only_for_the_chat_assistant() {
        let dir = tempfile::tempdir().unwrap();
        let v = crate::vault::Vault::create(
            dir.path(),
            crate::vault::VaultConfig {
                name: "Test".into(),
                backend: "memory".into(),
                password: None,
                ..Default::default()
            },
            crate::testing::registry(),
        )
        .unwrap();
        let today = jiff::civil::Date::constant(2026, 9, 16);

        let direct = ToolContext::new(&v, today, "UTC");
        assert!(current_conversation(&direct).is_none(), "the vault's owner has no thread");

        let cid = ConversationId::new();
        let assistant =
            ToolContext::new(&v, today, "UTC").with_caller(Caller::Assistant { conversation: cid });
        assert_eq!(current_conversation(&assistant), Some(cid));

        let mcp = ToolContext::new(&v, today, "UTC")
            .with_caller(Caller::Mcp { client: "device-1".into() });
        assert!(current_conversation(&mcp).is_none(), "MCP is not sitting in a thread");
    }

    // ---- caps ---------------------------------------------------------------

    #[test]
    fn the_scan_cap_can_never_make_the_largest_limit_fall_short() {
        // If the scan cap were ever smaller than the largest `limit` a
        // caller may ask for, a search for the maximum could come back with
        // fewer results than it promised, for no reason the model could see.
        const { assert!(CONVERSATION_SCAN_CAP >= crate::agent::tools::MAX_LIMIT) };
    }
}
