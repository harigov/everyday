use super::*;

/// One line about what a tool did, for the card in the panel.
///
/// Reads the shape every mutating tool returns -- see `done` in the core --
/// and falls back to saying nothing rather than dumping JSON at somebody.
/// The tools this file declares itself each have a shape of their own, and
/// a line of their own: how far through a plan is, which page was read,
/// where the weather is for.
pub(super) fn summarise(tool: &str, output: &ToolOutput) -> String {
    let Some(result) = output.as_json() else {
        return output.as_text().unwrap_or_default().chars().take(120).collect();
    };
    let str_of = |key: &str| result.get(key).and_then(|v| v.as_str()).unwrap_or_default();
    if crate::browser::is_tool(tool) {
        return crate::browser::summarise(result);
    }
    if crate::connections::tools::is_tool(tool) {
        return crate::connections::tools::summarise(tool, result);
    }
    if crate::images::tools::is_tool(tool) {
        return crate::images::tools::summarise(result);
    }
    if tool == tools::LOAD_TOOLS {
        return kits::summarise(result);
    }
    match tool {
        ASK_OWNER => {
            return match result.get("answer").and_then(Value::as_str) {
                Some(answer) => {
                    format!("answered: {}", answer.chars().take(100).collect::<String>())
                }
                None => "no answer".to_string(),
            };
        }
        UPDATE_PLAN => {
            let steps = result.get("steps").and_then(Value::as_u64).unwrap_or_default();
            let done = result.get("done").and_then(Value::as_u64).unwrap_or_default();
            return format!("{done} of {steps} done");
        }
        OFFER_CHOICES => {
            let options = result.get("options").and_then(Value::as_u64).unwrap_or_default();
            return format!("{options} options");
        }
        // The card's own words say it; the note is for the model.
        END_VOICE_MODE | START_DICTATION => return String::new(),
        TYPE_TEXT => return str_of("typed").trim().to_string(),
        // What was done to what is selected is in their window; what was
        // written is shown, as what is typed is. A text held back to be shown
        // first is not yet either.
        REWRITE_SELECTION if result.get("replaced") == Some(&Value::Bool(true)) => {
            return "Rewrote the selection".to_string();
        }
        WRITE_MESSAGE if !str_of("typed").is_empty() => {
            return str_of("typed").trim().to_string();
        }
        REWRITE_SELECTION | WRITE_MESSAGE => return "Not typed yet".to_string(),
        SUMMARIZE_THREAD | READ_ALOUD | READ_BRIEF => {
            return str_of("title").trim().chars().take(120).collect();
        }
        MEETING_BRIEF => {
            let title = result.pointer("/untrusted_text/title").and_then(Value::as_str);
            return title.unwrap_or_default().trim().chars().take(120).collect();
        }
        REMEMBER_PRONUNCIATION => {
            let (word, say) = (str_of("word").trim(), str_of("say").trim());
            return if say.is_empty() {
                format!("{word}: as the voice would say it")
            } else {
                format!("{word}: {say}")
            };
        }
        READ_WEB_PAGE => {
            let title = str_of("title").trim();
            if !title.is_empty() {
                return title.chars().take(120).collect();
            }
            return webpage::parse(str_of("url"))
                .ok()
                .and_then(|u| u.host_str().map(str::to_string))
                .unwrap_or_default();
        }
        GET_WEATHER => {
            // The place alone, as before, when there is no `current` to add
            // to it -- a result the core itself never sends, but a model's
            // malformed tool output is not this function's business to
            // refuse. Otherwise a glance at the card is enough to read the
            // temperature and the sky without opening it: "Seattle,
            // Washington, United States — 61°F, partly cloudy".
            let place = str_of("place");
            let Some(current) = result.get("current").filter(|c| !c.is_null()) else {
                return place.to_string();
            };
            let condition = current.get("condition").and_then(Value::as_str).unwrap_or_default();
            let unit = result
                .get("units")
                .and_then(|u| u.get("temperature"))
                .and_then(Value::as_str)
                .unwrap_or_default();
            return match current.get("temperature").and_then(Value::as_f64) {
                Some(t) if !condition.is_empty() => {
                    format!("{place} — {}{unit}, {condition}", t.round() as i64)
                }
                Some(t) => format!("{place} — {}{unit}", t.round() as i64),
                None if !condition.is_empty() => format!("{place} — {condition}"),
                None => place.to_string(),
            };
        }
        // The core's own answer for these says `started research <question>`
        // through the fallback below, which reads like a log line. The
        // question is the useful part, and `name` already carries it cut to a
        // length a card can hold.
        START_RESEARCH => {
            let what = str_of("name");
            return match str_of("action") {
                "already_started" => format!("already looking into: {what}"),
                _ => format!("looking into: {what}"),
            };
        }
        // The same reading for the two that came after it: `name` is what is
        // watched, or the goal, cut for a card, and `action` says whether a
        // live one for the same thing was found instead of a second made.
        START_WATCH => {
            let what = str_of("name");
            return match str_of("action") {
                "already_started" => format!("already watching: {what}"),
                _ => format!("watching: {what}"),
            };
        }
        START_PROJECT => {
            let what = str_of("name");
            return match str_of("action") {
                "already_started" => format!("already working on: {what}"),
                _ => format!("working on: {what}"),
            };
        }
        // What it changed is for the model, in the tool's own words; the card
        // says which job.
        UPDATE_JOB => {
            let what = str_of("name");
            return if what.is_empty() {
                "updated".to_string()
            } else {
                format!("updated: {what}")
            };
        }
        // What a project told them, as it reads in their chat.
        REPORT_PROGRESS => {
            let said = str_of("text").trim();
            return if said.is_empty() {
                "told them".to_string()
            } else {
                format!("told them: {}", said.chars().take(100).collect::<String>())
            };
        }
        SEARCH_IMAGES => {
            return match result.get("count").and_then(Value::as_u64) {
                Some(1) => "1 picture".into(),
                Some(n) => format!("{n} pictures"),
                None => String::new(),
            };
        }
        SEARCH_PLACES => {
            let count = result.get("count").and_then(Value::as_u64).unwrap_or_default();
            let places = if count == 1 { "1 place".to_string() } else { format!("{count} places") };
            let near = str_of("near").trim();
            return if near.is_empty() { places } else { format!("{places} near {near}") };
        }
        GET_DIRECTIONS => {
            // A route: how long and how far, which is the answer. Transit:
            // how many ways there are, since each has its own times.
            if let Some(options) = result.get("options").and_then(Value::as_array) {
                return match options.len() {
                    0 => "no connections found".into(),
                    1 => "1 connection".into(),
                    n => format!("{n} connections"),
                };
            }
            let duration = str_of("duration").trim();
            let distance = str_of("distance").trim();
            return match (duration.is_empty(), distance.is_empty()) {
                (false, false) => format!("{duration}, {distance}"),
                (false, true) => duration.to_string(),
                (true, false) => distance.to_string(),
                (true, true) => String::new(),
            };
        }
        CANCEL_JOB => return format!("cancelled: {}", str_of("name")),
        // A file read or described is named; the reply's own words are what
        // it said, and not what a card should repeat.
        READ_FILE => {
            let truncated = result.get("truncated").and_then(Value::as_bool).unwrap_or(false);
            return if truncated {
                format!("{} (the first part)", str_of("name"))
            } else {
                str_of("name").to_string()
            };
        }
        FILE_INFO => return str_of("name").to_string(),
        DOCUMENT_OUTLINE => return str_of("name").to_string(),
        READ_DOCUMENT => {
            let page = |key: &str| result.get(key).and_then(Value::as_u64).unwrap_or(0);
            let (from, to) = (page("fromPage"), page("toPage"));
            return match (from, to) {
                (0, _) => str_of("name").to_string(),
                (from, to) if from == to => format!("{}, page {from}", str_of("name")),
                (from, to) => format!("{}, pages {from}\u{2013}{to}", str_of("name")),
            };
        }
        SEARCH_DOCUMENT | SEARCH_DOCUMENTS => {
            let found = result.get("hits").and_then(Value::as_array).map_or(0, Vec::len);
            let what = if found == 1 { "1 place" } else { "places" };
            let what = if found == 1 { what.to_string() } else { format!("{found} {what}") };
            return match str_of("name") {
                "" => format!("{what} for {:?}", str_of("query")),
                name => format!("{what} in {name} for {:?}", str_of("query")),
            };
        }
        // What was done, in the tool's own words, rather than "organized
        // file 3 files".
        ORGANIZE_FILES => {
            let changes: Vec<&str> = result
                .get("changes")
                .and_then(Value::as_array)
                .map(|list| list.iter().filter_map(Value::as_str).collect())
                .unwrap_or_default();
            return changes.join("; ").chars().take(120).collect();
        }
        TRASH_FILES => return format!("trashed {}", str_of("name")),
        JOB_STATUS => {
            // One job: where it stands, and what it is doing if it says. A
            // watch between looks is `waiting` on the wire, which on a card
            // reads as though it were stuck; it is watching.
            if let Some(status) = result.get("status").and_then(Value::as_str) {
                let status = if status == "waiting" { "watching" } else { status };
                let progress = str_of("progress").trim();
                return if progress.is_empty() {
                    format!("{status}: {}", str_of("title"))
                } else {
                    format!("{status}: {progress}")
                };
            }
            // Or the recent ones, counted.
            return match result.get("count").and_then(Value::as_u64) {
                Some(1) => "1 job".into(),
                Some(n) => format!("{n} jobs"),
                None => String::new(),
            };
        }
        _ => {}
    }
    let action = str_of("action");
    let kind = str_of("kind");
    let name = str_of("name");
    if action.is_empty() {
        // A read. The count is the only interesting thing about it.
        return match result.get("count").and_then(|v| v.as_u64()) {
            Some(1) => "1 result".into(),
            Some(n) => format!("{n} results"),
            None => String::new(),
        };
    }
    format!("{action} {kind} {name}").trim().to_string()
}
