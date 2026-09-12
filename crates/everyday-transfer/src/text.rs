//! The two plain-text shapes every part is written in.
//!
//! **Front matter** — a fenced block of `key: value` at the top of a Markdown
//! file — is how a note keeps its id, its tags and its timestamps while still
//! being a file you can open in any editor. It is the convention Obsidian,
//! Jekyll, Hugo and most static-site generators already read, which is the
//! whole reason for choosing it: an exported journal drops into a folder those
//! tools already understand.
//!
//! **CSV** is for the records that are rows rather than documents: a year of
//! weight readings, a shelf of books, the log of what was watched when. RFC
//! 4180, so a spreadsheet opens it.
//!
//! # Why not a YAML library
//!
//! Because what is written here is a strict subset -- scalars and flat lists,
//! one level, no anchors, no references, no block scalars -- and reading a
//! *general* YAML document back would mean accepting constructs this
//! application has no meaning for and then deciding what to do about them.
//! The subset is small enough to state: a key, a colon, a space, and either a
//! scalar or a `[a, b]` list. Anything else in the block is kept as text and
//! ignored, which is what lets somebody's own front matter survive a round
//! trip through the vault rather than being an error.

use std::collections::BTreeMap;
use std::fmt::Write as _;

// ---- front matter -------------------------------------------------------

/// A front-matter block being built.
///
/// Ordered by insertion rather than sorted, because the order is editorial:
/// a reader should meet the title before the timestamps.
#[derive(Default)]
pub struct FrontMatter {
    fields: Vec<(String, String)>,
}

impl FrontMatter {
    pub fn new() -> Self {
        Self::default()
    }

    /// Add a scalar. Skipped when empty, so an absent subtitle is an absent
    /// line rather than `subtitle: ""`.
    pub fn set(&mut self, key: &str, value: impl AsRef<str>) -> &mut Self {
        let value = value.as_ref();
        if !value.is_empty() {
            self.fields.push((key.to_string(), scalar(value)));
        }
        self
    }

    /// Add a scalar that is written even when it is empty or false, because
    /// its absence would mean something different from its default.
    pub fn always(&mut self, key: &str, value: impl AsRef<str>) -> &mut Self {
        self.fields.push((key.to_string(), scalar(value.as_ref())));
        self
    }

    pub fn set_opt(&mut self, key: &str, value: Option<impl std::fmt::Display>) -> &mut Self {
        if let Some(value) = value {
            self.fields.push((key.to_string(), scalar(&value.to_string())));
        }
        self
    }

    pub fn flag(&mut self, key: &str, value: bool) -> &mut Self {
        if value {
            self.fields.push((key.to_string(), "true".into()));
        }
        self
    }

    /// Add a list. Skipped when empty.
    pub fn list(&mut self, key: &str, values: &[String]) -> &mut Self {
        if !values.is_empty() {
            let inner: Vec<String> = values.iter().map(|v| scalar(v)).collect();
            self.fields.push((key.to_string(), format!("[{}]", inner.join(", "))));
        }
        self
    }

    /// The block, fences included, ending in a blank line. Empty when nothing
    /// was set: a file with no metadata should not carry an empty fence.
    pub fn render(&self) -> String {
        if self.fields.is_empty() {
            return String::new();
        }
        let mut out = String::from("---\n");
        for (key, value) in &self.fields {
            let _ = writeln!(out, "{key}: {value}");
        }
        out.push_str("---\n\n");
        out
    }
}

/// Quote a value if leaving it bare would change what it means.
fn scalar(value: &str) -> String {
    let plain = !value.is_empty()
        && !value.starts_with(['#', '&', '*', '!', '|', '>', '%', '@', '`', '\'', '"', '[', '{', '-', ' '])
        && !value.ends_with(' ')
        && !value.contains([':', ',', '\n', '\r', ']', '}', '\t'])
        && value != "true"
        && value != "false"
        && value != "null"
        // A bare `2026` read back would be a number and a bare `01234` would
        // lose its zero. Titles that look like numbers are common enough --
        // "1984" is a book -- that this is not a hypothetical.
        && value.parse::<f64>().is_err();
    if plain {
        value.to_string()
    } else {
        let escaped = value.replace('\\', "\\\\").replace('"', "\\\"").replace('\n', "\\n");
        format!("\"{escaped}\"")
    }
}

/// A front-matter block that has been read back.
#[derive(Debug, Default)]
pub struct Fields {
    values: BTreeMap<String, String>,
}

impl Fields {
    pub fn get(&self, key: &str) -> Option<&str> {
        self.values.get(key).map(String::as_str)
    }

    /// A field as text, or `""`. The common case: almost every field in this
    /// application has an empty-string default.
    pub fn text(&self, key: &str) -> String {
        self.values.get(key).cloned().unwrap_or_default()
    }

    pub fn parse<T: std::str::FromStr>(&self, key: &str) -> Option<T> {
        self.values.get(key)?.parse().ok()
    }

    pub fn flag(&self, key: &str) -> bool {
        matches!(self.values.get(key).map(String::as_str), Some("true" | "yes"))
    }

    /// A `[a, b]` list, or a bare scalar read as a list of one, or nothing.
    pub fn list(&self, key: &str) -> Vec<String> {
        let Some(raw) = self.values.get(key) else { return Vec::new() };
        let raw = raw.trim();
        let inner = raw.strip_prefix('[').and_then(|r| r.strip_suffix(']')).unwrap_or(raw);
        split_flow(inner).into_iter().filter(|s| !s.is_empty()).collect()
    }
}

/// Split a Markdown file into its front matter and its body.
///
/// A file with no fence is all body, which is the right answer for a
/// hand-written note somebody dropped into the folder: it gets imported with
/// its filename as its title rather than being refused.
pub fn split_front_matter(source: &str) -> (Fields, &str) {
    let source = source.strip_prefix('\u{feff}').unwrap_or(source);
    let Some(rest) = source.strip_prefix("---\n").or_else(|| source.strip_prefix("---\r\n")) else {
        return (Fields::default(), source);
    };
    let Some(end) = find_fence(rest) else {
        return (Fields::default(), source);
    };
    let (block, body) = rest.split_at(end);
    let body = body.trim_start_matches("---").trim_start_matches(['\r', '\n']);

    let mut values = BTreeMap::new();
    for line in block.lines() {
        let line = line.trim_end();
        if line.trim().is_empty() || line.trim_start().starts_with('#') {
            continue;
        }
        // Nested and multi-line YAML is not written by this application, and
        // a continuation line read as a key would invent a field. Anything
        // indented belongs to a construct above, so it is skipped whole.
        if line.starts_with([' ', '\t', '-']) {
            continue;
        }
        let Some((key, value)) = line.split_once(':') else { continue };
        values.insert(key.trim().to_string(), unquote(value.trim()));
    }
    (Fields { values }, body)
}

/// The offset of the closing `---`, which must be alone on its line.
fn find_fence(rest: &str) -> Option<usize> {
    let mut at = 0;
    for line in rest.split_inclusive('\n') {
        if line.trim_end() == "---" {
            return Some(at);
        }
        at += line.len();
    }
    None
}

fn unquote(value: &str) -> String {
    let Some(inner) = value.strip_prefix('"').and_then(|v| v.strip_suffix('"')) else {
        return value.trim_matches('\'').to_string();
    };
    let mut out = String::with_capacity(inner.len());
    let mut chars = inner.chars();
    while let Some(c) = chars.next() {
        match (c, chars.clone().next()) {
            ('\\', Some('n')) => {
                out.push('\n');
                chars.next();
            }
            ('\\', Some(next)) => {
                out.push(next);
                chars.next();
            }
            _ => out.push(c),
        }
    }
    out
}

/// Split `a, "b, c", d` on the commas that are not inside quotes.
fn split_flow(inner: &str) -> Vec<String> {
    let mut out = Vec::new();
    let mut field = String::new();
    let mut quoted = false;
    let mut escaped = false;
    for c in inner.chars() {
        match c {
            _ if escaped => {
                field.push(c);
                escaped = false;
            }
            '\\' if quoted => escaped = true,
            '"' => quoted = !quoted,
            ',' if !quoted => {
                out.push(field.trim().to_string());
                field.clear();
            }
            _ => field.push(c),
        }
    }
    out.push(field.trim().to_string());
    out
}

// ---- csv ----------------------------------------------------------------

/// Builds a CSV document, RFC 4180.
pub struct Csv {
    out: String,
    columns: usize,
    /// Lines written, header included -- kept rather than counted from `out`
    /// on every call, which an index export otherwise did once per row
    /// added, turning a linear write into a quadratic one.
    lines: usize,
}

impl Csv {
    pub fn new(header: &[&str]) -> Self {
        let mut csv = Self { out: String::new(), columns: header.len(), lines: 0 };
        csv.row(&header.iter().map(|h| h.to_string()).collect::<Vec<_>>());
        csv
    }

    pub fn row(&mut self, cells: &[String]) {
        debug_assert_eq!(cells.len(), self.columns, "a CSV row does not match its header");
        let line: Vec<String> = cells.iter().map(|c| cell(c)).collect();
        // CRLF, which is what the specification says and what stops Excel
        // reading a file written on Linux as one long row.
        self.out.push_str(&line.join(","));
        self.out.push_str("\r\n");
        self.lines += 1;
    }

    /// Rows written so far, the header excluded.
    pub fn rows(&self) -> usize {
        self.lines.saturating_sub(1)
    }

    pub fn finish(self) -> String {
        self.out
    }
}

fn cell(value: &str) -> String {
    if value.contains([',', '"', '\n', '\r']) || value.starts_with(' ') || value.ends_with(' ') {
        format!("\"{}\"", value.replace('"', "\"\""))
    } else {
        value.to_string()
    }
}

/// One row of a CSV, addressed by column name.
pub struct Row<'a> {
    header: &'a [String],
    cells: &'a [String],
}

impl Row<'_> {
    /// A cell by column name, or `""` when the column is not in this file.
    /// Columns are matched case-insensitively and ignoring spaces, so a file
    /// somebody edited in a spreadsheet still lines up.
    pub fn get(&self, column: &str) -> &str {
        self.header
            .iter()
            .position(|h| same_column(h, column))
            .and_then(|i| self.cells.get(i))
            .map_or("", String::as_str)
    }

    pub fn parse<T: std::str::FromStr>(&self, column: &str) -> Option<T> {
        let raw = self.get(column).trim();
        if raw.is_empty() { None } else { raw.parse().ok() }
    }

    pub fn flag(&self, column: &str) -> bool {
        matches!(self.get(column).trim().to_ascii_lowercase().as_str(), "true" | "yes" | "1")
    }

    pub fn list(&self, column: &str) -> Vec<String> {
        self.get(column)
            .split([',', ';'])
            .map(str::trim)
            .filter(|s| !s.is_empty())
            .map(str::to_string)
            .collect()
    }

    pub fn is_empty(&self) -> bool {
        self.cells.iter().all(|c| c.trim().is_empty())
    }
}

fn same_column(a: &str, b: &str) -> bool {
    let norm = |s: &str| s.trim().to_lowercase().replace([' ', '_', '-'], "");
    norm(a) == norm(b)
}

/// Read a CSV document into rows keyed by its header.
pub struct Table {
    header: Vec<String>,
    rows: Vec<Vec<String>>,
}

impl Table {
    pub fn parse(source: &str) -> Self {
        let mut records = split_records(source.strip_prefix('\u{feff}').unwrap_or(source));
        let header = if records.is_empty() { Vec::new() } else { records.remove(0) };
        Self { header, rows: records }
    }

    pub fn rows(&self) -> impl Iterator<Item = Row<'_>> {
        self.rows
            .iter()
            .map(|cells| Row { header: &self.header, cells: cells.as_slice() })
            .filter(|row| !row.is_empty())
    }

    /// Does this file have the column that says what it is?
    pub fn has(&self, column: &str) -> bool {
        self.header.iter().any(|h| same_column(h, column))
    }
}

/// Split a CSV document into records, honouring quotes around newlines.
fn split_records(source: &str) -> Vec<Vec<String>> {
    let mut records = Vec::new();
    let mut record = Vec::new();
    let mut field = String::new();
    let mut quoted = false;
    let mut chars = source.chars().peekable();

    while let Some(c) = chars.next() {
        match c {
            '"' if quoted => {
                // A doubled quote inside a quoted field is one quote.
                if chars.peek() == Some(&'"') {
                    field.push('"');
                    chars.next();
                } else {
                    quoted = false;
                }
            }
            '"' if field.is_empty() => quoted = true,
            ',' if !quoted => record.push(std::mem::take(&mut field)),
            '\r' if !quoted => {}
            '\n' if !quoted => {
                record.push(std::mem::take(&mut field));
                records.push(std::mem::take(&mut record));
            }
            _ => field.push(c),
        }
    }
    if !field.is_empty() || !record.is_empty() {
        record.push(field);
        records.push(record);
    }
    records
}

// ---- names --------------------------------------------------------------

/// Turn a title into something every filesystem will accept.
///
/// Conservative on purpose: the archive is unzipped on macOS, Linux and
/// Windows, and Windows is the strict one -- no `<>:"/\|?*`, no trailing dot
/// or space, and a handful of reserved device names that cannot be used even
/// with an extension. A file called `CON.md` is unwritable on Windows and the
/// failure is reported as a permission error, which nobody would connect to
/// having titled an entry "Con".
///
/// A run of dots is somebody trailing off -- "Wait... what happened" -- and is
/// treated as a space rather than kept. That is not a nicety: `..` is the one
/// sequence an archive path may never contain, and a single entry carrying one
/// would be refused by [`zip::is_safe_path`](crate::zip::is_safe_path) and
/// take the whole import down with it. A *single* dot is left alone, because
/// that is an extension.
pub fn safe_name(title: &str) -> String {
    const RESERVED: &[&str] = &[
        "con", "prn", "aux", "nul", "com1", "com2", "com3", "com4", "com5", "com6", "com7", "com8",
        "com9", "lpt1", "lpt2", "lpt3", "lpt4", "lpt5", "lpt6", "lpt7", "lpt8", "lpt9",
    ];
    let mut out = String::with_capacity(title.len());
    let mut spaced = false;
    let mut chars = title.chars().peekable();
    while let Some(c) = chars.next() {
        match c {
            '<' | '>' | ':' | '"' | '/' | '\\' | '|' | '?' | '*' => spaced = true,
            c if (c as u32) < 0x20 || c == '\u{7f}' => spaced = true,
            c if c.is_whitespace() => spaced = true,
            '.' if chars.peek() == Some(&'.') || out.ends_with('.') => {
                // Eat the rest of the run, and leave a separator where it was.
                while chars.peek() == Some(&'.') {
                    chars.next();
                }
                if out.ends_with('.') {
                    out.pop();
                }
                spaced = true;
            }
            c => {
                if spaced && !out.is_empty() {
                    out.push('-');
                }
                spaced = false;
                out.push(c);
            }
        }
    }
    // Length is in bytes because that is what the filesystems count, and a
    // name is truncated on a character boundary so a multi-byte title does
    // not come out as invalid UTF-8.
    while out.len() > 60 {
        out.pop();
    }
    let trimmed = out.trim_matches(['-', '.', ' ']).to_string();
    if trimmed.is_empty() || RESERVED.contains(&trimmed.to_lowercase().as_str()) {
        format!("_{trimmed}")
    } else {
        trimmed
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn front_matter_round_trips() {
        let mut fm = FrontMatter::new();
        fm.set("title", "Morning: a start")
            .list("tags", &["travel".into(), "family, sort of".into()])
            .set("date", "2026-09-10")
            .flag("starred", true);
        let doc = format!("{}The body.\n", fm.render());

        let (fields, body) = split_front_matter(&doc);
        assert_eq!(fields.text("title"), "Morning: a start");
        assert_eq!(fields.list("tags"), ["travel", "family, sort of"]);
        assert_eq!(fields.text("date"), "2026-09-10");
        assert!(fields.flag("starred"));
        assert_eq!(body, "The body.\n");
    }

    #[test]
    fn a_title_that_looks_like_a_number_stays_a_string() {
        let mut fm = FrontMatter::new();
        fm.set("title", "1984");
        assert!(fm.render().contains("title: \"1984\""), "{}", fm.render());
        let (fields, _) = split_front_matter(&fm.render());
        assert_eq!(fields.text("title"), "1984");
    }

    #[test]
    fn a_file_with_no_front_matter_is_all_body() {
        let (fields, body) = split_front_matter("# Just a heading\n\nand text.\n");
        assert!(fields.get("title").is_none());
        assert_eq!(body, "# Just a heading\n\nand text.\n");
    }

    #[test]
    fn a_body_that_contains_a_rule_does_not_end_the_block_early() {
        let doc = "---\ntitle: One\n---\n\nfirst\n\n---\n\nsecond\n";
        let (fields, body) = split_front_matter(doc);
        assert_eq!(fields.text("title"), "One");
        assert!(body.contains("second"), "{body}");
    }

    #[test]
    fn front_matter_this_application_did_not_write_is_ignored_rather_than_fatal() {
        let doc = "---\ntitle: One\nnested:\n  - a\n  - b\n---\nbody\n";
        let (fields, body) = split_front_matter(doc);
        assert_eq!(fields.text("title"), "One");
        assert_eq!(body, "body\n");
    }

    #[test]
    fn csv_round_trips_the_awkward_cells() {
        let mut csv = Csv::new(&["title", "note"]);
        csv.row(&["Say \"hello\"".into(), "one,two\nthree".into()]);
        csv.row(&["plain".into(), String::new()]);
        assert_eq!(csv.rows(), 2);

        let table = Table::parse(&csv.finish());
        let rows: Vec<_> = table.rows().collect();
        assert_eq!(rows[0].get("title"), "Say \"hello\"");
        assert_eq!(rows[0].get("note"), "one,two\nthree");
        assert_eq!(rows[1].get("note"), "");
    }

    #[test]
    fn a_column_renamed_by_a_spreadsheet_still_matches() {
        let table = Table::parse("Local Date,Value\r\n2026-09-10,72.5\r\n");
        let row = table.rows().next().unwrap();
        assert_eq!(row.get("local_date"), "2026-09-10");
        assert_eq!(row.parse::<f64>("value"), Some(72.5));
    }

    #[test]
    fn names_windows_refuses_are_not_written() {
        assert_eq!(safe_name("What now? A: the answer/decision"), "What-now-A-the-answer-decision");
        assert_eq!(safe_name("   "), "_");
        assert_eq!(safe_name("CON"), "_CON");
        assert_eq!(safe_name("trailing. "), "trailing");
    }

    #[test]
    fn a_title_that_trails_off_does_not_produce_a_path_no_archive_may_hold() {
        for title in ["Wait... what happened", "..", "a..b", "one....two", "...leading"] {
            let name = safe_name(title);
            assert!(!name.contains(".."), "{title:?} became {name:?}");
            assert!(crate::zip::is_safe_path(&format!("journal/{name}.md")), "{title:?}");
        }
        assert_eq!(safe_name("Wait... what happened"), "Wait-what-happened");
        // A single dot is an extension and is left alone.
        assert_eq!(safe_name("IMG_0042.jpg"), "IMG_0042.jpg");
    }

    #[test]
    fn a_long_multibyte_title_is_cut_on_a_character() {
        let name = safe_name(&"\u{65e5}".repeat(60));
        assert!(name.len() <= 60);
        assert!(name.chars().all(|c| c == '\u{65e5}'));
    }
}
