//! Next steps, read back out of a meeting note.
//!
//! The note is the summary the person will read. A task proposal is made
//! from a step in it, not from a second guess at the transcript, so what
//! lands in the todo app is a line they can find again in the note.
//!
//! Three shapes are read, because a template somebody edited still writes
//! the older ones: `- **Name:** what, by when`, the checkbox
//! `Owner — what — by when`, and the `Owner | Action | Due` table. A
//! section that was never written (`(not written)`) contributes nothing.
//!
//! Whether a step is the owner's is decided here only from the name. The
//! decision model has its own say, and [`keep`] is how the two combine: a
//! confident "not mine" is dropped even when the name matches, and a model
//! that is missing, refused or unsure falls back to the name.

use jiff::civil::Date;

use super::people::Owner;

/// One thing somebody said they would do.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NextStep {
    pub owner: String,
    /// The task, without a due date that was successfully read.
    pub action: String,
    /// Set only when the line states a calendar date. "By Friday" is not one.
    pub due: Option<Date>,
}

/// What a decision model said about one step.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ModelSay {
    /// Clearly this person's.
    Mine,
    /// Clearly somebody else's, or nobody's.
    Theirs,
    /// No answer worth trusting.
    Unsure,
}

/// Below this, a probability is not a decision. The name rule answers instead.
const SURE: f64 = 0.5;

/// The next steps in `note`, dated against `on` when a line gives a month
/// and a day but no year.
pub fn next_steps(note: &str, on: Date) -> Vec<NextStep> {
    let Some(body) = section(note) else { return Vec::new() };
    let mut out = Vec::new();
    for line in body.lines() {
        let line = line.trim();
        if line.is_empty() || is_blank_marker(line) {
            continue;
        }
        if let Some(step) = parse_line(line, on)
            && !step.action.is_empty()
        {
            out.push(step);
        }
    }
    out
}

/// `name` is a step nobody took, which is never the owner's -- or one that a
/// voice nobody could name took on, which the notes write as "Unassigned" too.
pub fn is_unassigned(name: &str) -> bool {
    let folded = fold(name);
    matches!(
        folded.as_str(),
        "" | "unknown" | "nobody" | "someone" | "somebody" | "n/a" | "na" | "tbd" | "none" | "tbc"
    ) || folded == "unassigned"
        || folded.strip_prefix("unassigned").is_some_and(|rest| rest.starts_with([' ', '(']))
}

/// Whether `name` is the owner, and not also somebody else in the room.
///
/// The full name and any of the owner's addresses match on their own. A
/// first name matches only when the step says just that first name and no
/// one else invited has it: "Hari" is the owner when the profile is "Hari
/// Govardhanam" and the invite has no other Hari, and it is not when two
/// people share it.
///
/// "You" and "Me" are the owner too. Whoever has no name in their profile is
/// "You" on the microphone's lines -- see `stages::owner_label` in the
/// service -- so the notes write `**You:**` for their own steps; with only
/// the name to go by, none of them would ever be proposed as tasks.
pub fn assigned_to_owner(name: &str, owner: &Owner, others: &[impl AsRef<str>]) -> bool {
    let name = name.trim();
    if is_unassigned(name) {
        return false;
    }
    let folded = fold(name);
    if matches!(folded.as_str(), "you" | "me")
        || owner.addresses.iter().any(|a| a == &folded)
        || contains_address(name, owner)
    {
        return true;
    }
    let mine = owner.name.trim();
    if mine.is_empty() {
        return false;
    }
    if folded == fold(mine) {
        return true;
    }
    let my_first = first_name(mine);
    if my_first.is_empty() || fold(name) != fold(&my_first) {
        return false;
    }
    others.iter().all(|other| fold(&first_name(other.as_ref())) != fold(&my_first))
}

/// A model's answer. A probability under [`SURE`] is [`ModelSay::Unsure`],
/// whichever name it picked: a weak "mine" must not create a task, and a
/// weak "not mine" must not throw away a step the name rule would keep.
pub fn model_says(choice: &str, probability: Option<f64>) -> ModelSay {
    if probability.is_some_and(|p| p < SURE) {
        return ModelSay::Unsure;
    }
    match fold(choice).as_str() {
        "mine" => ModelSay::Mine,
        "not mine" => ModelSay::Theirs,
        _ => ModelSay::Unsure,
    }
}

/// Whether to propose. The model wins when it is sure. Otherwise the name.
pub fn keep(model: ModelSay, by_name: bool) -> bool {
    match model {
        ModelSay::Mine => true,
        ModelSay::Theirs => false,
        ModelSay::Unsure => by_name,
    }
}

fn section(note: &str) -> Option<String> {
    let mut found: Option<(u8, String)> = None;
    for line in note.lines() {
        if let Some((level, text)) = heading(line) {
            if found.is_some() && level <= found.as_ref().unwrap().0 {
                break;
            }
            if found.is_none() && is_steps_heading(&text) {
                found = Some((level, String::new()));
            }
            continue;
        }
        if let Some((_, body)) = found.as_mut() {
            body.push_str(line);
            body.push('\n');
        }
    }
    let body = found.map(|(_, body)| body)?;
    let trimmed = body.trim();
    if trimmed.is_empty() || is_blank_marker(trimmed) { None } else { Some(body) }
}

fn heading(line: &str) -> Option<(u8, String)> {
    let t = line.trim();
    let level = t.bytes().take_while(|b| *b == b'#').count();
    if !(1..=6).contains(&level) {
        return None;
    }
    let rest = t.get(level..)?.trim();
    if rest.is_empty() {
        return None;
    }
    let text = rest.trim_end_matches(':').trim().to_string();
    if text.is_empty() { None } else { Some((level as u8, text)) }
}

fn is_steps_heading(text: &str) -> bool {
    matches!(fold(text).as_str(), "next steps" | "action items" | "actions")
}

fn is_blank_marker(text: &str) -> bool {
    matches!(fold(text).as_str(), "(not written)" | "not written" | "(none)" | "none" | "n/a")
}

fn parse_line(line: &str, on: Date) -> Option<NextStep> {
    let t = line.trim();
    if t.starts_with('|') {
        return parse_table(t, on);
    }
    let t = t.trim_start_matches(['-', '*', '+']).trim();
    let t = strip_checkbox(t);
    if let Some(step) = parse_bold(t, on) {
        return Some(step);
    }
    parse_dashes(t, on)
}

fn strip_checkbox(t: &str) -> &str {
    let t = t.trim();
    for mark in ["[ ]", "[x]", "[X]"] {
        if let Some(rest) = t.strip_prefix(mark) {
            return rest.trim();
        }
    }
    t
}

/// `- **Name:** what they will do, by when`
fn parse_bold(t: &str, on: Date) -> Option<NextStep> {
    let rest = t.strip_prefix("**")?;
    let (name, action) = rest.split_once(":**")?;
    let name = name.trim().trim_matches('*').trim();
    if name.is_empty() {
        return None;
    }
    Some(step(name, action, None, on))
}

/// `Owner — what — by when`, also an en dash or ` - `.
fn parse_dashes(t: &str, on: Date) -> Option<NextStep> {
    let parts = split_dash(t);
    if parts.len() < 2 {
        return None;
    }
    let name = parts[0].trim();
    // A sentence with a dash in it ("pricing - the page slips") is not a
    // step. A name is a few words; anything longer is the sentence.
    let words = name.split_whitespace().count();
    if name.is_empty() || words > 4 {
        return None;
    }
    let action = parts[1].trim();
    let due_text = parts.get(2).map(|s| s.trim()).filter(|s| !s.is_empty());
    if action.is_empty() {
        return None;
    }
    Some(step(name, action, due_text, on))
}

fn split_dash(t: &str) -> Vec<&str> {
    let mut parts = Vec::new();
    let mut rest = t;
    loop {
        let found = ["\u{2014}", "\u{2013}", " - "]
            .iter()
            .filter_map(|sep| rest.find(sep).map(|at| (at, *sep)))
            .min_by_key(|(at, _)| *at);
        let Some((at, sep)) = found else {
            parts.push(rest.trim());
            break;
        };
        parts.push(rest[..at].trim());
        rest = rest[at + sep.len()..].trim();
    }
    parts.retain(|p| !p.is_empty());
    parts
}

fn parse_table(t: &str, on: Date) -> Option<NextStep> {
    let cells: Vec<&str> =
        t.trim_matches('|').split('|').map(str::trim).filter(|c| !c.is_empty()).collect();
    if cells.len() < 2 {
        return None;
    }
    if cells.iter().all(|c| c.chars().all(|ch| matches!(ch, '-' | ':' | ' '))) {
        return None;
    }
    let owner = cells[0];
    let action = cells[1];
    if fold(owner) == "owner" || fold(action) == "action" {
        return None;
    }
    let due_text = cells.get(2).copied().filter(|s| !s.is_empty() && fold(s) != "due");
    Some(step(owner, action, due_text, on))
}

fn step(owner: &str, action: &str, due_text: Option<&str>, on: Date) -> NextStep {
    let (action, from_action) = split_due(action.trim(), on);
    let due = due_text.and_then(|t| parse_date(t, on)).or(from_action);
    NextStep { owner: owner.trim().to_string(), action, due }
}

/// A trailing ", by 12 May 2026" comes off the title once it is a real date.
/// Anything else ("by Friday") stays in the title and sets no due date.
fn split_due(action: &str, on: Date) -> (String, Option<Date>) {
    let folded = fold(action);
    for marker in [", by ", " by "] {
        if let Some(at) = folded.rfind(marker) {
            let when = action[at + marker.len()..].trim().trim_end_matches('.');
            if let Some(date) = parse_date(when, on) {
                return (action[..at].trim().trim_end_matches(',').trim().to_string(), Some(date));
            }
        }
    }
    (action.trim().to_string(), None)
}

fn parse_date(text: &str, on: Date) -> Option<Date> {
    let t = text.trim().trim_matches(|c: char| matches!(c, '.' | ',' | ';' | ')' | '('));
    if t.is_empty() {
        return None;
    }
    if let Some(date) = iso(t) {
        return Some(date);
    }
    let mut words: Vec<&str> = t.split_whitespace().collect();
    // "Fri 28 Nov" and "Friday, 28 November": the weekday is there to be
    // checked by a reader, not parsed. "Friday" alone is no date at all.
    if words.first().is_some_and(|w| is_weekday(w)) {
        words.remove(0);
    }
    match words.as_slice() {
        [a, b, y] => dmy(a, b, y).or_else(|| mdy(a, b, y)),
        [a, b] => {
            dm(a, b, on.year()).or_else(|| md(a, b, on.year())).map(|d| next_occurrence(d, on))
        }
        _ => None,
    }
}

/// "Fri", "Friday", "friday," or "Thurs." -- a day of the week and nothing else.
fn is_weekday(token: &str) -> bool {
    const DAYS: [&str; 7] =
        ["monday", "tuesday", "wednesday", "thursday", "friday", "saturday", "sunday"];
    let t = token.trim_matches(|c: char| matches!(c, ',' | '.')).to_ascii_lowercase();
    t.len() >= 3 && DAYS.iter().any(|d| *d == t || (d.starts_with(&t) && t.len() <= 5))
}

/// A day that was said with no year, as of the meeting on `on`: this year's,
/// unless that is more than [`ROLL_OVER_DAYS`] ago, in which case it is the
/// next one. Said in December, "5 Jan" is next month's, not eleven months gone.
/// A date a week or two back stays where it is: somebody who says "by the
/// 3rd" on the 10th has said a date that has passed, and may well mean it.
fn next_occurrence(date: Date, on: Date) -> Date {
    let ago = on.since(date).map_or(0, |span| i64::from(span.get_days()));
    if ago > ROLL_OVER_DAYS {
        return date.checked_add(jiff::Span::new().years(1)).unwrap_or(date);
    }
    date
}

/// How far back a day with no year may fall before it is taken to be next year's.
const ROLL_OVER_DAYS: i64 = 60;

fn iso(t: &str) -> Option<Date> {
    let mut parts = t.split(['-', '/']);
    let y: i16 = parts.next()?.parse().ok()?;
    let m: i8 = parts.next()?.parse().ok()?;
    let d: i8 = parts.next()?.parse().ok()?;
    if parts.next().is_some() {
        return None;
    }
    Date::new(y, m, d).ok()
}

fn dmy(day: &str, month: &str, year: &str) -> Option<Date> {
    Date::new(year.parse().ok()?, month_of(month)?, day_of(day)?).ok()
}

fn mdy(month: &str, day: &str, year: &str) -> Option<Date> {
    Date::new(year.parse().ok()?, month_of(month)?, day_of(day)?).ok()
}

fn dm(day: &str, month: &str, year: i16) -> Option<Date> {
    Date::new(year, month_of(month)?, day_of(day)?).ok()
}

fn md(month: &str, day: &str, year: i16) -> Option<Date> {
    Date::new(year, month_of(month)?, day_of(day)?).ok()
}

fn day_of(token: &str) -> Option<i8> {
    let bare = token.trim_end_matches(|c: char| c.is_ascii_alphabetic());
    if bare.is_empty() || bare.chars().any(|c| !c.is_ascii_digit()) {
        return None;
    }
    bare.parse().ok()
}

fn month_of(token: &str) -> Option<i8> {
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
    let t = fold(token);
    let t = t.trim_end_matches('.');
    MONTHS.iter().position(|m| *m == t || m.starts_with(t) && t.len() >= 3).map(|i| i as i8 + 1)
}

fn first_name(name: &str) -> String {
    name.split_whitespace()
        .next()
        .unwrap_or("")
        .trim_matches(|c: char| !c.is_alphanumeric())
        .to_string()
}

fn contains_address(name: &str, owner: &Owner) -> bool {
    name.split(|c: char| c.is_whitespace() || matches!(c, '<' | '>' | '(' | ')' | ',' | ';'))
        .any(|part| owner.addresses.contains(&fold(part)))
}

fn fold(s: &str) -> String {
    s.split_whitespace().collect::<Vec<_>>().join(" ").to_ascii_lowercase()
}

#[cfg(test)]
mod tests {
    use std::collections::HashSet;

    use super::*;

    fn on() -> Date {
        Date::new(2026, 5, 4).unwrap()
    }

    fn owner(name: &str, addresses: &[&str]) -> Owner {
        Owner {
            addresses: addresses.iter().map(|a| a.to_string()).collect::<HashSet<_>>(),
            name: name.into(),
        }
    }

    #[test]
    fn the_starter_shape_is_read_and_a_real_date_comes_off_the_title() {
        let note = "\
## Pricing
- Kept the current price.

## Next steps
- **Hari:** send the draft, by 12 May 2026
- **Priya:** review it, by Friday
- **Unassigned:** book the room
";
        let steps = next_steps(note, on());
        assert_eq!(steps.len(), 3);
        assert_eq!(steps[0].owner, "Hari");
        assert_eq!(steps[0].action, "send the draft");
        assert_eq!(steps[0].due, Some(Date::new(2026, 5, 12).unwrap()));
        assert_eq!(steps[1].action, "review it, by Friday");
        assert_eq!(steps[1].due, None);
        assert!(is_unassigned(&steps[2].owner));
    }

    #[test]
    fn an_unwritten_section_and_a_missing_one_propose_nothing() {
        assert!(
            next_steps("## Topics\n- Pricing\n\n## Next steps\n(not written)\n", on()).is_empty()
        );
        assert!(next_steps("## Topics\n- Pricing\n", on()).is_empty());
    }

    #[test]
    fn the_older_checkbox_and_table_are_still_read() {
        let note = "\
## Action items
- [ ] Hari G \u{2014} ship the page \u{2014} 2026-05-20
- [ ] Priya \u{2014} tell the customer

| Owner | Action | Due |
|---|---|---|
| Sam | send the invoice | May 8 |
";
        let steps = next_steps(note, on());
        assert_eq!(steps[0].owner, "Hari G");
        assert_eq!(steps[0].action, "ship the page");
        assert_eq!(steps[0].due, Some(Date::new(2026, 5, 20).unwrap()));
        assert_eq!(steps[1].due, None);
        assert_eq!(steps[2].owner, "Sam");
        assert_eq!(steps[2].due, Some(Date::new(2026, 5, 8).unwrap()));
    }

    #[test]
    fn a_first_name_is_the_owner_only_when_nobody_else_has_it() {
        let me = owner("Hari Govardhanam", &["hari@x.com"]);
        let priya = ["Priya Raman"];
        assert!(assigned_to_owner("Hari", &me, &priya));
        assert!(assigned_to_owner("Hari Govardhanam", &me, &priya));
        assert!(assigned_to_owner("hari@x.com", &me, &priya));
        assert!(!assigned_to_owner("Priya", &me, &priya));
        assert!(!assigned_to_owner("Unassigned", &me, &priya));
        let two = ["Hari Raman"];
        assert!(!assigned_to_owner("Hari", &me, &two), "two Haris, so a first name is not enough");
        assert!(assigned_to_owner("Hari Govardhanam", &me, &two));
    }

    #[test]
    fn the_model_wins_when_it_is_sure_and_the_name_wins_when_it_is_not() {
        assert_eq!(model_says("mine", Some(0.8)), ModelSay::Mine);
        assert_eq!(model_says("not mine", None), ModelSay::Theirs);
        assert_eq!(model_says("mine", Some(0.2)), ModelSay::Unsure);
        assert!(keep(ModelSay::Mine, false));
        assert!(!keep(ModelSay::Theirs, true), "a sure no drops a step the name would keep");
        assert!(keep(ModelSay::Unsure, true));
        assert!(!keep(ModelSay::Unsure, false));
    }

    #[test]
    fn a_weekday_in_front_of_a_date_is_read_past() {
        let note = "\
## Next steps
- **Hari:** Send Acme the revised pricing deck, by Fri 8 May
- **Priya:** Book the room, by Friday, 15 May
- **Sam:** Review it by Monday 11 May 2026
- **Sam:** Review it by Friday
";
        let steps = next_steps(note, on());
        assert_eq!(steps[0].action, "Send Acme the revised pricing deck");
        assert_eq!(steps[0].due, Some(Date::new(2026, 5, 8).unwrap()));
        assert_eq!(steps[1].action, "Book the room");
        assert_eq!(steps[1].due, Some(Date::new(2026, 5, 15).unwrap()));
        assert_eq!(steps[2].due, Some(Date::new(2026, 5, 11).unwrap()));
        assert_eq!(steps[3].due, None, "a weekday alone is still no date");
        assert_eq!(steps[3].action, "Review it by Friday");
    }

    #[test]
    fn a_day_said_in_december_for_january_is_next_years() {
        let december = Date::new(2026, 12, 20).unwrap();
        let note = "## Next steps\n- **Hari:** Send the contract, by Tue 5 Jan\n\
                    - **Hari:** Pay the invoice, by 15 Dec\n\
                    - **Hari:** Renew it, by 2 Jan 2026\n";
        let steps = next_steps(note, december);
        assert_eq!(steps[0].due, Some(Date::new(2027, 1, 5).unwrap()));
        assert_eq!(
            steps[1].due,
            Some(Date::new(2026, 12, 15).unwrap()),
            "a week back is not next year"
        );
        assert_eq!(
            steps[2].due,
            Some(Date::new(2026, 1, 2).unwrap()),
            "a year that was said is kept"
        );
    }

    #[test]
    fn you_and_me_are_the_owner_and_a_step_nobody_could_name_is_nobodys() {
        let nameless = owner("", &[]);
        let nobody: [&str; 0] = [];
        assert!(assigned_to_owner("You", &nameless, &nobody), "the label a nameless profile has");
        assert!(assigned_to_owner("me", &owner("Hari Govardhanam", &[]), &nobody));
        assert!(!assigned_to_owner("Priya", &nameless, &nobody));
        assert!(is_unassigned("Unassigned (offered by someone on the call)"));
        assert!(is_unassigned("Unassigned"));
        assert!(!is_unassigned("Unassignedly Raman"));
        assert!(!assigned_to_owner(
            "Unassigned (offered by someone on the call)",
            &nameless,
            &nobody
        ));
    }
}
