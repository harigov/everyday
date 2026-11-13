//! The words a meeting's transcript is likely to get wrong: the names of the
//! people on the call, of the companies they work for, and the words the
//! person listed in Settings as their own -- products, acronyms, jargon.
//!
//! Speech recognition hears a name it does not know as the nearest word it
//! does, or spells it the way it sounds: "Jeremey" for Jeremy, "Kubernetis"
//! for Kubernetes. [`meeting_words`] gathers what is known before a call is
//! transcribed -- the invite's names, the companies in everybody's
//! addresses, the person's list -- and three places use it:
//!
//! - [`transcriber_prompt`] tells the recogniser, in sentences: "Weekly
//!   sync. With Ana Ruiz and Bo Chen of Acme. We talk about Kubernetes."
//!   Sentences give it context where a bare list of words changes little.
//! - [`spelling_line`] is a rule for the model that writes the notes: how
//!   those words are spelled, so a summary spells them right even where the
//!   transcript did not.
//! - [`respell`] mends the transcript afterwards, writing a word one letter
//!   off one of them the way the meeting spells it.
//!
//! Respelling is timid on purpose. A transcript is mostly everyday words,
//! and an everyday word turned into a name -- "Games" into "James" -- is a
//! worse mistake than a name left misspelled, which the spelling rule can
//! still put right in the notes. So only a capitalised word is ever changed,
//! never one the meeting also writes in lower case somewhere
//! ([`ordinary_words`]), never a short one, and only by a single edit.
//!
//! The approach -- sentences for the recogniser, a spelling rule for the
//! notes, one-edit respelling, companies read from addresses -- follows
//! Yogurt (github.com/FrigadeHQ/yogurt, MIT).

use super::people::{Invitee, Owner};
use std::collections::HashSet;

/// The most terms the person's own list holds. The recogniser hears only as
/// many as fit in [`MAX_PROMPT_BYTES`]; the notes get them all, so the list
/// stays a size a prompt can carry.
pub const MAX_TERMS: usize = 100;

/// The longest one term may be, in characters: room for a product's name
/// with a word or two after it.
pub const MAX_TERM_CHARS: usize = 50;

/// The longest [`transcriber_prompt`] may be, in bytes. Whisper reads only
/// the last 224 tokens or so of a prompt and ignores anything before them,
/// so a longer prompt would quietly lose its beginning: the title and the
/// people.
pub const MAX_PROMPT_BYTES: usize = 500;

/// The most of a title the recogniser is told, in characters. A title is
/// context rather than words to get right, and an agenda pasted into it must
/// not crowd out the people.
const MAX_TITLE_CHARS: usize = 150;

/// The most people the recogniser is told about. Past a dozen, an invite is
/// a mailing list more than the voices on the call.
const MAX_PROMPT_NAMES: usize = 12;

/// The most companies the recogniser is told about.
const MAX_PROMPT_COMPANIES: usize = 4;

/// The fewest letters a word [`respell`] changes, or changes a word into,
/// may have. A shorter name is too often one letter off an everyday word --
/// Hill and "hall", Dana and "data" -- for one letter to be evidence of
/// anything.
pub const MIN_RESPELL: usize = 5;

/// Mail providers. An address at one says nothing about anybody's company.
const MAIL_PROVIDERS: &[&str] = &[
    "gmail",
    "googlemail",
    "outlook",
    "hotmail",
    "live",
    "msn",
    "yahoo",
    "ymail",
    "icloud",
    "me",
    "mac",
    "aol",
    "proton",
    "protonmail",
    "pm",
    "gmx",
    "yandex",
    "mail",
    "fastmail",
    "hey",
    "zoho",
    "qq",
    "163",
    "126",
    "comcast",
    "att",
    "verizon",
];

/// One meeting's names and words.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct MeetingWords {
    /// The calendar's title; empty when there is none worth trusting. The
    /// caller decides: a title made from the transcript may carry the very
    /// word it misheard.
    pub title: String,
    /// People's names: the invitees', then the owner's.
    pub names: Vec<String>,
    /// Companies, from work email addresses. Their capitals are a guess
    /// ("Viyamd" for a company that writes itself "ViyaMD"): an address has
    /// none of its own.
    pub companies: Vec<String>,
    /// The person's own list, from Settings, spelled as they typed it.
    pub terms: Vec<String>,
}

/// The person's list made safe: trimmed, runs of spaces made one, none
/// longer than [`MAX_TERM_CHARS`] characters, each once whatever its case
/// (the first spelling kept), at most [`MAX_TERMS`].
pub fn clean_terms(raw: &[String]) -> Vec<String> {
    let mut seen: HashSet<String> = HashSet::new();
    let mut out: Vec<String> = Vec::new();
    for term in raw {
        if out.len() == MAX_TERMS {
            break;
        }
        let spaced = term.split_whitespace().collect::<Vec<_>>().join(" ");
        let term = cut_chars(&spaced, MAX_TERM_CHARS).trim_end();
        if !term.is_empty() && seen.insert(term.to_lowercase()) {
            out.push(term.to_string());
        }
    }
    out
}

/// The company in a work address (`ana@acme-labs.co.uk` → "Acme-Labs"), or
/// `None` for a mail provider's address or a name too short to be sure of.
///
/// The name is the label before the top-level domain, or before a country's
/// registry (`co.uk`, `com.au`) where there is one, capitalised part by part
/// around its hyphens. A label that does not look like a word -- a leading
/// digit, an underscore, a part of one letter -- gives nothing rather than
/// something unpronounceable.
pub fn company_of(email: &str) -> Option<String> {
    let host = email.split('@').nth(1)?.trim().to_lowercase();
    let mut labels: Vec<&str> = host.split('.').filter(|l| !l.is_empty()).collect();
    if labels.len() < 2 {
        return None;
    }
    let top = labels.pop()?;
    // co.uk, com.au: under a country's two letters, a short label is the
    // registry's, not the company's.
    if top.chars().count() == 2
        && labels.len() > 1
        && labels.last().is_some_and(|l| l.chars().count() <= 3)
    {
        labels.pop();
    }
    let name = *labels.last()?;
    if MAIL_PROVIDERS.contains(&name) || name.len() < 3 || !name.split('-').all(is_name_part) {
        return None;
    }
    Some(name.split('-').map(capitalised).collect::<Vec<_>>().join("-"))
}

/// One hyphen-separated part of a company's label: a letter, then at least
/// one more letter or digit.
fn is_name_part(part: &str) -> bool {
    let mut chars = part.chars();
    chars.next().is_some_and(|c| c.is_ascii_lowercase())
        && part.len() >= 2
        && chars.all(|c| c.is_ascii_lowercase() || c.is_ascii_digit())
}

/// Everything known about one meeting's words.
///
/// `title` is kept as given, trimmed. Names are the invitees' -- leaving out
/// an address standing in for a name -- then the owner's, each once whatever
/// its case. Companies come from every invitee's address and every one of
/// the owner's, the owner's in sorted order so the same meeting always gives
/// the same list. `terms` goes through [`clean_terms`].
pub fn meeting_words(
    title: &str,
    invitees: &[Invitee],
    owner: &Owner,
    terms: &[String],
) -> MeetingWords {
    let names = unique(
        invitees
            .iter()
            .map(|i| i.name.as_str())
            .filter(|n| !n.contains('@'))
            .chain(std::iter::once(owner.name.as_str())),
    );
    let mut own: Vec<&str> = owner.addresses.iter().map(String::as_str).collect();
    own.sort_unstable();
    // A calendar that gave only an address may have put it in the name.
    let addresses = invitees
        .iter()
        .filter_map(|i| {
            i.email.as_deref().or_else(|| i.name.contains('@').then_some(i.name.as_str()))
        })
        .chain(own);
    let companies = unique(addresses.filter_map(company_of));
    MeetingWords { title: title.trim().to_string(), names, companies, terms: clean_terms(terms) }
}

/// What a recogniser is told, in sentences: "Weekly sync. With Ana Ruiz
/// and Bo Chen of Acme. We talk about Kubernetes and OKRs." At most
/// [`MAX_PROMPT_BYTES`]; empty when there is nothing to say.
///
/// A term already said as one of the names or companies is not said again.
/// When there is too much, whole names go first, from the end of the list,
/// then companies, then the title, and the person's terms last: theirs are
/// the words most surely worth getting right, and every name is still in
/// the summary's [`spelling_line`]. Nothing is ever cut partway, so the
/// prompt can end neither inside a character nor inside a name.
pub fn transcriber_prompt(words: &MeetingWords) -> String {
    let mut title = cut_chars(words.title.trim(), MAX_TITLE_CHARS)
        .trim_end_matches(|c: char| c == '.' || c.is_whitespace());
    let mut names: Vec<&str> =
        words.names.iter().take(MAX_PROMPT_NAMES).map(String::as_str).collect();
    let mut companies: Vec<&str> =
        words.companies.iter().take(MAX_PROMPT_COMPANIES).map(String::as_str).collect();
    let said: HashSet<String> = names.iter().chain(&companies).map(|w| w.to_lowercase()).collect();
    let mut terms: Vec<&str> = words
        .terms
        .iter()
        .map(String::as_str)
        .filter(|t| !said.contains(&t.to_lowercase()))
        .collect();

    let mut prompt = sentences(title, &names, &companies, &terms);
    while prompt.len() > MAX_PROMPT_BYTES && names.pop().is_some() {
        prompt = sentences(title, &names, &companies, &terms);
    }
    while prompt.len() > MAX_PROMPT_BYTES && companies.pop().is_some() {
        prompt = sentences(title, &names, &companies, &terms);
    }
    if prompt.len() > MAX_PROMPT_BYTES {
        title = "";
        prompt = sentences(title, &names, &companies, &terms);
    }
    while prompt.len() > MAX_PROMPT_BYTES && terms.pop().is_some() {
        prompt = sentences(title, &names, &companies, &terms);
    }
    prompt
}

/// The prompt's sentences from what is left of each part: "{title}.",
/// "With {names} of {companies}." (or "At {companies}." with nobody named),
/// "We talk about {terms}.", whichever have anything in them.
fn sentences(title: &str, names: &[&str], companies: &[&str], terms: &[&str]) -> String {
    let title = if title.is_empty() { String::new() } else { format!("{title}.") };
    let who = match (names.is_empty(), companies.is_empty()) {
        (false, false) => format!("With {} of {}.", and_list(names), and_list(companies)),
        (false, true) => format!("With {}.", and_list(names)),
        (true, false) => format!("At {}.", and_list(companies)),
        (true, true) => String::new(),
    };
    let about = if terms.is_empty() {
        String::new()
    } else {
        format!("We talk about {}.", and_list(terms))
    };
    [title, who, about].into_iter().filter(|s| !s.is_empty()).collect::<Vec<_>>().join(" ")
}

/// "A", "A and B", "A, B and C".
fn and_list(items: &[&str]) -> String {
    match items {
        [] => String::new(),
        [one] => (*one).to_string(),
        [rest @ .., last] => format!("{} and {last}", rest.join(", ")),
    }
}

/// The rule for the summarising model about spelling, one line starting
/// "- Spelling: "; empty when there is nothing to spell.
///
/// The person's terms and people's names are spelled right, the person's
/// first: a term they typed wins over a company spelled the same but for
/// its capitals, which is then not listed again. Companies are listed apart,
/// as their addresses spell them, because an address has no capitals: the
/// model is told so, rather than told to write "Viyamd" for ViyaMD.
pub fn spelling_line(words: &MeetingWords) -> String {
    let right = unique(words.terms.iter().chain(&words.names));
    let known: HashSet<String> = right.iter().map(|w| w.to_lowercase()).collect();
    let companies =
        unique(words.companies.iter().filter(|c| !known.contains(&c.trim().to_lowercase())));
    if right.is_empty() && companies.is_empty() {
        return String::new();
    }
    let mut line = String::from("- Spelling: speech recognition mishears names and unusual words.");
    if !right.is_empty() {
        line.push_str(&format!(" These are spelled right: {}.", right.join(", ")));
    }
    if !companies.is_empty() {
        line.push_str(&format!(
            " Companies, as their email addresses spell them (the capitals may differ): {}.",
            companies.join(", ")
        ));
    }
    line.push_str(
        " Where the transcript has a word that sounds like one of these, write it as spelled here.",
    );
    line
}

/// Words written in lower case anywhere in `texts`, lower-cased: ordinary
/// words, never to be respelled as a name.
///
/// This stands in for a dictionary (Yogurt reads the computer's word list;
/// there is none to rely on here). A word somebody's transcript already
/// writes in lower case is an everyday word in this meeting, however close
/// it is to a name. Words are split on anything that is not a letter or a
/// digit, and one counts if it has a lower-case letter and no capital.
pub fn ordinary_words<'a>(texts: impl IntoIterator<Item = &'a str>) -> HashSet<String> {
    texts
        .into_iter()
        .flat_map(|text| text.split(|c: char| !c.is_alphanumeric()))
        .filter(|w| w.chars().any(char::is_lowercase) && !w.chars().any(char::is_uppercase))
        .map(str::to_lowercase)
        .collect()
}

/// `text` with every word one letter off a name, company or term of the
/// meeting written the way the meeting spells it. For many lines of one
/// meeting, build a [`Respeller`] once instead.
///
/// A word is a run of letters; everything between words is kept exactly as
/// it was. A word is changed only when it starts with a capital, does not
/// start a sentence (where every word has a capital: "Right, let's start"
/// is never "Wright"), is not in `ordinary` (see [`ordinary_words`]), is not
/// already one of the meeting's words whatever its capitals -- a right
/// "ViyaMD" is not made the address's "Viyamd" -- is not one of them with an
/// "s" on, and is exactly one edit (a letter added, dropped, changed, or two
/// neighbours swapped) from one of them, both having at least
/// [`MIN_RESPELL`] letters. Single-word terms and companies are tried first,
/// then the words of people's names and of longer terms; the first within
/// one edit wins. A term typed in lower case keeps the capital the sentence
/// gave the word it replaces.
pub fn respell(text: &str, words: &MeetingWords, ordinary: &HashSet<String>) -> String {
    Respeller::new(words).respell(text, ordinary)
}

/// [`respell`] for one meeting, its targets worked out once rather than
/// again for every line.
pub struct Respeller {
    targets: Vec<Target>,
}

impl Respeller {
    pub fn new(words: &MeetingWords) -> Self {
        Self { targets: respell_targets(words) }
    }

    /// See [`respell`].
    pub fn respell(&self, text: &str, ordinary: &HashSet<String>) -> String {
        if self.targets.is_empty() {
            return text.to_string();
        }
        let mut out = String::with_capacity(text.len());
        let mut rest = text;
        // At the start of the text, and after a sentence's closing mark.
        let mut sentence_start = true;
        while let Some(start) = rest.find(char::is_alphabetic) {
            let between = &rest[..start];
            if between.contains(['.', '!', '?', '\u{3002}']) {
                sentence_start = true;
            }
            out.push_str(between);
            let run = &rest[start..];
            let end = run.find(|c: char| !c.is_alphabetic()).unwrap_or(run.len());
            let token = &run[..end];
            let spelled =
                (!sentence_start).then(|| respelled(token, &self.targets, ordinary)).flatten();
            out.push_str(spelled.as_deref().unwrap_or(token));
            sentence_start = false;
            rest = &run[end..];
        }
        out.push_str(rest);
        out
    }
}

/// A spelling [`respell`] may write: as the meeting spells it, and
/// lower-cased letter by letter for comparing.
struct Target {
    spelled: String,
    lower: Vec<char>,
}

/// What [`respell`] compares words against, in the order it tries them:
/// single-word terms and companies whole, then each word of people's names
/// and of the terms longer than one word. Only spellings made entirely of
/// letters, since only a run of letters is ever compared with them, and
/// none shorter than [`MIN_RESPELL`].
fn respell_targets(words: &MeetingWords) -> Vec<Target> {
    let (single, multi): (Vec<&String>, Vec<&String>) =
        words.terms.iter().partition(|t| !t.contains(char::is_whitespace));
    let whole = unique(single.into_iter().chain(&words.companies));
    let parts = unique(words.names.iter().chain(multi).flat_map(|n| n.split_whitespace()));
    whole
        .into_iter()
        .chain(parts)
        .filter(|w| w.chars().all(char::is_alphabetic))
        .map(|spelled| Target { lower: spelled.to_lowercase().chars().collect(), spelled })
        .filter(|t| t.lower.len() >= MIN_RESPELL)
        .collect()
}

/// The spelling `token` should have, or `None` to leave it as written.
fn respelled(token: &str, targets: &[Target], ordinary: &HashSet<String>) -> Option<String> {
    if !token.chars().next().is_some_and(char::is_uppercase) {
        return None;
    }
    let lower = token.to_lowercase();
    if ordinary.contains(&lower) {
        return None;
    }
    let heard: Vec<char> = lower.chars().collect();
    if heard.len() < MIN_RESPELL || targets.iter().any(|t| t.lower == heard) {
        return None;
    }
    let target = targets.iter().find(|t| {
        t.lower.len().abs_diff(heard.len()) <= 1
            && !plural_of(&heard, &t.lower)
            && !plural_of(&t.lower, &heard)
            && distance(&heard, &t.lower) == 1
    })?;
    Some(keep_capital(&target.spelled))
}

/// Is `a` just `b` with an "s" on the end? One edit away, but "Grants" at
/// the start of a sentence is a plural, not a misheard Grant.
fn plural_of(a: &[char], b: &[char]) -> bool {
    a.len() == b.len() + 1 && a.last() == Some(&'s') && a.starts_with(b)
}

/// A spelling typed all in lower case takes the capital of the word it
/// replaces -- always a capitalised one: "Zentric said" becomes "Zentrik
/// said" for "zentrik". Any other spelling is written as typed.
fn keep_capital(spelled: &str) -> String {
    if spelled == spelled.to_lowercase() { capitalised(spelled) } else { spelled.to_string() }
}

/// Edits between two words -- a letter added, dropped or changed, or two
/// neighbours swapped, each counting one (the optimal string alignment
/// distance). The swap matters: "Mehar" for Mehra is a recogniser's slip.
fn distance(a: &[char], b: &[char]) -> usize {
    let mut d = vec![vec![0usize; b.len() + 1]; a.len() + 1];
    for (i, row) in d.iter_mut().enumerate() {
        row[0] = i;
    }
    for (j, cell) in d[0].iter_mut().enumerate() {
        *cell = j;
    }
    for i in 1..=a.len() {
        for j in 1..=b.len() {
            let cost = usize::from(a[i - 1] != b[j - 1]);
            let mut best = (d[i - 1][j] + 1).min(d[i][j - 1] + 1).min(d[i - 1][j - 1] + cost);
            if i > 1 && j > 1 && a[i - 1] == b[j - 2] && a[i - 2] == b[j - 1] {
                best = best.min(d[i - 2][j - 2] + 1);
            }
            d[i][j] = best;
        }
    }
    d[a.len()][b.len()]
}

/// `word` with its first letter a capital.
fn capitalised(word: &str) -> String {
    let mut chars = word.chars();
    match chars.next() {
        Some(first) => first.to_uppercase().chain(chars).collect(),
        None => String::new(),
    }
}

/// At most `max` characters of `text`, never cut inside one.
fn cut_chars(text: &str, max: usize) -> &str {
    match text.char_indices().nth(max) {
        Some((at, _)) => &text[..at],
        None => text,
    }
}

/// `items` trimmed, blanks dropped, each once whatever its case: the first
/// spelling met is the one kept.
fn unique<I>(items: I) -> Vec<String>
where
    I: IntoIterator,
    I::Item: AsRef<str>,
{
    let mut seen: HashSet<String> = HashSet::new();
    let mut out = Vec::new();
    for item in items {
        let item = item.as_ref().trim();
        if !item.is_empty() && seen.insert(item.to_lowercase()) {
            out.push(item.to_string());
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn strings(items: &[&str]) -> Vec<String> {
        items.iter().map(|s| s.to_string()).collect()
    }

    fn invitee(name: &str, email: Option<&str>) -> Invitee {
        Invitee {
            name: name.into(),
            email: email.map(str::to_string),
            response: None,
            organizer: false,
        }
    }

    fn owner(name: &str, addresses: &[&str]) -> Owner {
        Owner { addresses: addresses.iter().map(|a| a.to_string()).collect(), name: name.into() }
    }

    fn words(names: &[&str], companies: &[&str], terms: &[&str]) -> MeetingWords {
        MeetingWords {
            title: String::new(),
            names: strings(names),
            companies: strings(companies),
            terms: strings(terms),
        }
    }

    fn titled(title: &str, names: &[&str], companies: &[&str], terms: &[&str]) -> MeetingWords {
        MeetingWords { title: title.into(), ..words(names, companies, terms) }
    }

    /// Respelled as the service would: the text's own lower-case words are
    /// its ordinary ones.
    fn fixed(text: &str, words: &MeetingWords) -> String {
        respell(text, words, &ordinary_words([text]))
    }

    #[test]
    fn a_work_address_names_its_company() {
        assert_eq!(company_of("alex@zentrik.example").as_deref(), Some("Zentrik"));
        assert_eq!(company_of("ana@mail.acme.co.uk").as_deref(), Some("Acme"));
        assert_eq!(company_of("ana@acme-labs.co.uk").as_deref(), Some("Acme-Labs"));
        assert_eq!(company_of("bo@north-wind.io").as_deref(), Some("North-Wind"));
        assert_eq!(company_of("li@shop.com.au").as_deref(), Some("Shop"));
        assert_eq!(company_of("Hari@ViyaMD.com").as_deref(), Some("Viyamd"));
    }

    #[test]
    fn a_mail_provider_or_an_unsure_name_gives_no_company() {
        for email in [
            "sam@gmail.com",
            "sam@icloud.com",
            "sam@outlook.com",
            "sam@proton.me",
            "sam@yahoo.co.uk",
            "no-at-sign",
            "x@localhost",
            "x@ab.com",
            "x@1password.com",
            "x@acme_corp.com",
            "x@a-b.com",
            "",
        ] {
            assert_eq!(company_of(email), None, "{email}");
        }
    }

    #[test]
    fn meeting_words_has_everybody_s_names_and_companies() {
        let invitees = [
            invitee("Robin Smith", Some("robin@northwind.com")),
            invitee("Tarun Mehra", Some("tarun@gmail.com")),
        ];
        let me = owner("Alex Morgan", &["alex@zentrik.example"]);
        let w = meeting_words("  Northwind integration ", &invitees, &me, &[]);
        assert_eq!(
            w,
            MeetingWords {
                title: "Northwind integration".into(),
                names: strings(&["Robin Smith", "Tarun Mehra", "Alex Morgan"]),
                companies: strings(&["Northwind", "Zentrik"]),
                terms: vec![],
            }
        );
    }

    #[test]
    fn meeting_words_leaves_out_addresses_as_names_and_repeats() {
        let invitees = [
            invitee("jo@zentrik.example", Some("jo@zentrik.example")),
            invitee("Ann Lee", Some("a@zentrik.example")),
            invitee("ann lee", Some("b@Zentrik.example")),
            invitee("pat@acme.com", None),
        ];
        let w = meeting_words("", &invitees, &owner("", &[]), &[]);
        assert_eq!(w.names, ["Ann Lee"]);
        assert_eq!(w.companies, ["Zentrik", "Acme"]);
    }

    #[test]
    fn the_owner_s_addresses_give_companies_in_a_fixed_order() {
        // Every new HashSet iterates in its own order; the companies must not.
        for _ in 0..20 {
            let me = owner(
                "Hari Govardhanam",
                &["hari@viyamd.com", "hari@zeta.io", "hari@alpha.dev", "hari@gmail.com"],
            );
            let w = meeting_words("", &[invitee("Ana Ruiz", Some("ana@acme.com"))], &me, &[]);
            assert_eq!(w.companies, ["Acme", "Alpha", "Viyamd", "Zeta"]);
            assert_eq!(w.names, ["Ana Ruiz", "Hari Govardhanam"]);
        }
    }

    #[test]
    fn meeting_words_cleans_the_person_s_terms() {
        let terms = strings(&["PostHog", " SOC   2 ", "posthog", ""]);
        let w = meeting_words("", &[], &owner("", &[]), &terms);
        assert_eq!(w.terms, ["PostHog", "SOC 2"]);
    }

    #[test]
    fn terms_are_trimmed_with_single_spaces_inside() {
        assert_eq!(clean_terms(&strings(&["PostHog", "  SOC   2 "])), ["PostHog", "SOC 2"]);
    }

    #[test]
    fn a_blank_term_or_one_already_listed_is_dropped() {
        assert_eq!(clean_terms(&strings(&["PostHog", "   ", "posthog"])), ["PostHog"]);
        assert_eq!(clean_terms(&strings(&["a", " A ", "b ", ""])), ["a", "b"]);
    }

    #[test]
    fn terms_are_kept_short_and_the_list_a_sensible_length() {
        assert_eq!(clean_terms(&["x".repeat(200)])[0].len(), MAX_TERM_CHARS);
        // Characters, not bytes: fifty "é"s are a hundred bytes.
        assert_eq!(clean_terms(&["é".repeat(200)])[0].chars().count(), MAX_TERM_CHARS);
        // A cut that lands just after a space is trimmed again.
        let spaced = format!("{} tail", "a".repeat(MAX_TERM_CHARS - 1));
        assert_eq!(clean_terms(&[spaced]), ["a".repeat(MAX_TERM_CHARS - 1)]);
        let many: Vec<String> = (0..MAX_TERMS + 5).map(|i| format!("t{i}")).collect();
        assert_eq!(clean_terms(&many).len(), MAX_TERMS);
    }

    #[test]
    fn the_prompt_is_the_title_then_who_is_in_it_as_sentences() {
        let w = titled(
            "Northwind Integration and Zentrik Data with Robin",
            &["Robin Smith", "Tarun Mehra", "Alex Morgan"],
            &["Zentrik"],
            &[],
        );
        assert_eq!(
            transcriber_prompt(&w),
            "Northwind Integration and Zentrik Data with Robin. \
             With Robin Smith, Tarun Mehra and Alex Morgan of Zentrik."
        );
        assert_eq!(
            transcriber_prompt(&words(&[], &["Zentrik", "Northwind"], &[])),
            "At Zentrik and Northwind."
        );
        assert_eq!(
            transcriber_prompt(&titled("Weekly sync. ..", &["Ana Ruiz"], &[], &[])),
            "Weekly sync. With Ana Ruiz."
        );
        assert_eq!(transcriber_prompt(&MeetingWords::default()), "");
    }

    #[test]
    fn the_person_s_terms_follow_the_people_without_repeating_them() {
        let w = titled(
            "Launch",
            &["Ann Lee"],
            &["Zentrik"],
            &["PostHog", "zentrik", "SOC 2", "ann lee"],
        );
        assert_eq!(
            transcriber_prompt(&w),
            "Launch. With Ann Lee of Zentrik. We talk about PostHog and SOC 2."
        );
        assert_eq!(
            transcriber_prompt(&words(&[], &[], &["Kubernetes"])),
            "We talk about Kubernetes."
        );
    }

    #[test]
    fn terms_outlast_names_when_there_is_too_much_to_say() {
        let names: Vec<String> =
            (0..80).map(|i| format!("Person With A Long Name Number{i}")).collect();
        let w = MeetingWords {
            title: "A".repeat(300),
            names,
            companies: strings(&["Zentrik"]),
            terms: strings(&["PostHog", "Kubernetes", "SOC 2"]),
        };
        let p = transcriber_prompt(&w);
        assert!(p.len() <= MAX_PROMPT_BYTES, "{}", p.len());
        assert!(p.starts_with(&format!("{}. With Person", "A".repeat(150))), "{p}");
        assert!(p.contains("Number0") && !p.contains("Number11"), "{p}");
        assert!(p.ends_with("We talk about PostHog, Kubernetes and SOC 2."), "{p}");

        let long: Vec<String> = (0..100).map(|i| format!("Term{i}")).collect();
        let p = transcriber_prompt(&MeetingWords { terms: long, ..Default::default() });
        assert!(p.len() <= MAX_PROMPT_BYTES, "{}", p.len());
        assert!(p.starts_with("We talk about Term0, Term1"), "{p}");
    }

    #[test]
    fn the_prompt_never_cuts_through_a_character_or_a_name() {
        let title = "Réunion trimestrielle de l'équipe ".repeat(10);
        // Each padding moves the 500-byte mark to another byte of a name
        // full of two- and three-byte characters.
        for pad in 0..8 {
            let names: Vec<String> =
                (0..30).map(|i| format!("{}José 田中 Müller {i:02}", "é".repeat(pad))).collect();
            let w = MeetingWords {
                title: title.clone(),
                names: names.clone(),
                companies: strings(&["Acme"]),
                terms: vec![],
            };
            let p = transcriber_prompt(&w);
            assert!(p.len() <= MAX_PROMPT_BYTES, "{}: {p}", p.len());
            assert!(p.ends_with('.'), "{p}");
            assert!(p.starts_with("Réunion trimestrielle"), "{p}");
            let first = p.split(". ").next().unwrap();
            assert!(first.chars().count() <= MAX_TITLE_CHARS, "{first}");
            // Names go whole, from the end: what is left is the first few.
            let kept = names.iter().take_while(|n| p.contains(n.as_str())).count();
            assert!(kept <= MAX_PROMPT_NAMES, "{p}");
            assert!(names[kept..].iter().all(|n| !p.contains(n.as_str())), "{p}");
        }
        // A name longer than the whole budget is dropped, not cut.
        let long = "é".repeat(400);
        assert_eq!(transcriber_prompt(&titled("Réunion", &[long.as_str()], &[], &[])), "Réunion.");
    }

    #[test]
    fn the_spelling_line_trusts_typed_names_but_not_an_address_s_capitals() {
        let w = titled("x", &["Ann Lee"], &["Viyamd"], &["Kubernetes"]);
        assert_eq!(
            spelling_line(&w),
            "- Spelling: speech recognition mishears names and unusual words. \
             These are spelled right: Kubernetes, Ann Lee. \
             Companies, as their email addresses spell them (the capitals may differ): Viyamd. \
             Where the transcript has a word that sounds like one of these, \
             write it as spelled here."
        );
    }

    #[test]
    fn the_person_s_own_spelling_comes_first_and_wins() {
        let line = spelling_line(&words(&["Ann Lee"], &["Zentrik"], &["zentrik", "PostHog"]));
        assert!(line.contains("These are spelled right: zentrik, PostHog, Ann Lee."), "{line}");
        assert!(!line.contains("Zentrik"), "{line}");
        assert!(!line.contains("Companies"), "{line}");
    }

    #[test]
    fn the_spelling_line_says_only_what_there_is() {
        assert_eq!(spelling_line(&titled("x", &[], &[], &[])), "");
        let only = spelling_line(&words(&[], &["Acme"], &[]));
        assert!(only.starts_with("- Spelling: "), "{only}");
        assert!(only.contains("(the capitals may differ): Acme."), "{only}");
        assert!(!only.contains("spelled right"), "{only}");
        assert!(spelling_line(&words(&[], &[], &["SOC 2"])).contains("spelled right: SOC 2."));
    }

    #[test]
    fn ordinary_words_are_those_written_in_lower_case_somewhere() {
        let ordinary =
            ordinary_words(["Games night is on. We like games!", "GAMES with James, at 3pm."]);
        for word in ["games", "night", "is", "on", "like", "with", "at", "3pm"] {
            assert!(ordinary.contains(word), "{word}");
        }
        for word in ["we", "james"] {
            assert!(!ordinary.contains(word), "{word}");
        }
    }

    #[test]
    fn respell_fixes_a_name_or_term_one_letter_off() {
        let w = words(&["Jeremy Hale"], &[], &["Kubernetes", "PostHog", "Acme Rocketry"]);
        assert_eq!(
            fixed("So Jeremey moved Kubernetis to Posthog. The Rocketri team agreed.", &w),
            "So Jeremy moved Kubernetes to Posthog. The Rocketry team agreed."
        );
    }

    #[test]
    fn respell_fixes_companies_and_names_keeping_what_follows() {
        let w = words(&["Tarun Mehra", "Grant Hill", "Jo Li"], &["Zentrik", "Northwind"], &[]);
        assert_eq!(
            fixed("The Zentric checklist, Kentrik's SDK and Zantrik.", &w),
            "The Zentrik checklist, Zentrik's SDK and Zentrik."
        );
        assert_eq!(
            fixed("So Taran said the North wind sync is fine, and Mehar agreed.", &w),
            "So Tarun said the North wind sync is fine, and Mehra agreed."
        );
    }

    #[test]
    fn respell_leaves_everyday_words_alone() {
        let w = words(&["Tarun Mehra", "Grant Hill", "Jo Li"], &["Zentrik", "Northwind"], &[]);
        let text = "a grand plan, hills and a job for Joe";
        assert_eq!(fixed(text, &w), text);
        // A company that is an everyday word too: written in lower case
        // anywhere, the capitalised one is the everyday word as well.
        let text = "the motion passed, Notion and Motion";
        assert_eq!(fixed(text, &words(&[], &["Notion"], &[])), text);
        let w = words(&[], &[], &["Stripe", "API"]);
        let text = "a stripe of paint and a strip of tape, the APO";
        assert_eq!(fixed(text, &w), text);
        assert_eq!(fixed("The Strip is where we pay.", &w), "The Stripe is where we pay.");
    }

    #[test]
    fn respell_never_changes_a_lower_case_word() {
        let w = words(&["Jeremy Hale"], &["Zentrik"], &["Kubernetes", "zentrik"]);
        let text = "jeremey ran kubernetis at zentric";
        assert_eq!(respell(text, &w, &HashSet::new()), text);
    }

    #[test]
    fn respell_leaves_a_capitalised_word_the_meeting_writes_in_lower_case() {
        let w = words(&["James Park"], &[], &[]);
        let text = "It is Games night on Friday. We love games.";
        assert_eq!(fixed(text, &w), text);
        // Only the ordinary word holds it back.
        assert_eq!(
            respell(text, &w, &HashSet::new()),
            "It is James night on Friday. We love games."
        );
    }

    #[test]
    fn respell_leaves_a_right_spelling_as_written_whatever_its_capitals() {
        let text = "ViyaMD ships. VIYAMD and Viyamd too.";
        assert_eq!(respell(text, &words(&[], &["Viyamd"], &[]), &HashSet::new()), text);
        // One of the meeting's words exactly is not pulled toward another
        // one letter off it that is tried first.
        let w = words(&["Marcos Diaz"], &[], &["Marco"]);
        assert_eq!(respell("Marcos and Marco", &w, &HashSet::new()), "Marcos and Marco");
    }

    #[test]
    fn respell_leaves_short_words_and_short_names_alone() {
        let w = words(&["Jose Li", "Mark Hall", "Grant Wu"], &[], &[]);
        let text = "Josh met Mary in Hull. Gran came.";
        assert_eq!(respell(text, &w, &HashSet::new()), text);
    }

    #[test]
    fn a_term_typed_in_lower_case_keeps_the_sentence_s_capital() {
        let w = words(&[], &[], &["zentrik"]);
        assert_eq!(
            respell("Then Zentric said so, and the zentric SDK too.", &w, &HashSet::new()),
            "Then Zentrik said so, and the zentric SDK too."
        );
    }

    #[test]
    fn respell_keeps_everything_between_words_as_it_was() {
        let w = words(&["Jeremy Hale"], &[], &[]);
        assert_eq!(
            respell("  so Jeremey's (Q3)\n\t-- ok, Jeremey!  ", &w, &HashSet::new()),
            "  so Jeremy's (Q3)\n\t-- ok, Jeremy!  "
        );
        assert_eq!(respell("", &w, &HashSet::new()), "");
    }

    #[test]
    fn respell_changes_nothing_with_no_words_to_go_by() {
        assert_eq!(respell("Zentric", &titled("x", &[], &[], &[]), &HashSet::new()), "Zentric");
    }

    #[test]
    fn distance_counts_a_swap_of_neighbours_as_one_edit() {
        let d = |a: &str, b: &str| {
            distance(&a.chars().collect::<Vec<_>>(), &b.chars().collect::<Vec<_>>())
        };
        assert_eq!(d("mehar", "mehra"), 1);
        assert_eq!(d("strip", "stripe"), 1);
        assert_eq!(d("zantrik", "zentrik"), 1);
        assert_eq!(d("kitten", "sitting"), 3);
        assert_eq!(d("", "abc"), 3);
        assert_eq!(d("same", "same"), 0);
    }

    #[test]
    fn a_plural_is_never_taken_for_a_name() {
        let words = MeetingWords { names: vec!["Grant Hill".into()], ..Default::default() };
        let ordinary = HashSet::new();
        assert_eq!(respell("Grants are due.", &words, &ordinary), "Grants are due.");
        assert_eq!(respell("Ask Grnat.", &words, &ordinary), "Ask Grant.");
    }

    #[test]
    fn a_sentences_first_word_is_never_respelled() {
        let words = MeetingWords { names: vec!["Sam Wright".into()], ..Default::default() };
        let ordinary = HashSet::new();
        assert_eq!(respell("Right, let's start.", &words, &ordinary), "Right, let's start.");
        assert_eq!(respell("Okay. Right then.", &words, &ordinary), "Okay. Right then.");
        assert_eq!(respell("Thanks, Wrigth.", &words, &ordinary), "Thanks, Wright.");
    }
}
