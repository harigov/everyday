//! Something written to be heard: who says what, in order. What the daily
//! podcast and an article turned into audio are made from. See
//! `docs/plans/podcasts.md` (phase 2).
//!
//! # One narrator now, more later
//!
//! A [`Script`] is lines, each said by one of its [`ScriptSpeaker`]s. Today every
//! script has one -- [`NARRATOR`] -- and the voice chosen in Settings → Models
//! says everything. The shape is already the one a conversation between two
//! hosts needs: a speaker per line, a voice per speaker. And each line may
//! name the story it is about ([`ScriptLine::about`]), which is what a video
//! version would put on screen while the line is said.
//!
//! # Written down
//!
//! A script a model writes, or a person edits, is text: a line to a paragraph,
//! and when there is more than one voice each line begins with who says it,
//! `ALEX: ...`. [`Script::parse`] reads that and [`Script::tagged`] writes it,
//! so what is kept with the daily podcast can be shown, corrected and said
//! again without a format of its own.

use crate::id::StoryId;
use serde::{Deserialize, Serialize};

/// The one speaker every script has today.
pub const NARRATOR: &str = "narrator";

/// Lines to be said, by whom.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[serde(rename_all = "camelCase")]
pub struct Script {
    pub title: String,
    pub speakers: Vec<ScriptSpeaker>,
    pub lines: Vec<ScriptLine>,
}

/// One voice in a script.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[serde(rename_all = "camelCase")]
pub struct ScriptSpeaker {
    /// What a line names it by.
    pub id: String,
    /// What it is called, if it is ever introduced.
    #[serde(default)]
    pub name: String,
    /// The local voice's speaker to use (`LocalVoice` speakers by name).
    /// `None` is the voice chosen in Settings → Models.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub voice: Option<String>,
}

/// One thing said.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[serde(rename_all = "camelCase")]
pub struct ScriptLine {
    /// A [`ScriptSpeaker::id`].
    pub speaker: String,
    pub text: String,
    /// The story it is about, if it is about one.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub about: Option<StoryId>,
}

impl Script {
    /// A script for one narrator: every non-empty paragraph of `text` a line.
    pub fn narrated(title: impl Into<String>, text: &str) -> Self {
        let lines = text
            .split("\n\n")
            .map(|p| p.split_whitespace().collect::<Vec<_>>().join(" "))
            .filter(|p| !p.is_empty())
            .map(|text| ScriptLine { speaker: NARRATOR.into(), text, about: None })
            .collect();
        Self {
            title: title.into(),
            speakers: vec![ScriptSpeaker { id: NARRATOR.into(), name: String::new(), voice: None }],
            lines,
        }
    }

    /// A script for one narrator in which each of `paragraphs` is a line. As
    /// [`Self::narrated`], except that line breaks inside a paragraph stay, so
    /// the items of a list are still separate things to say (a voice pauses
    /// longer after each than after a comma). Blank paragraphs are dropped.
    pub fn narrated_paragraphs<S: AsRef<str>>(
        title: impl Into<String>,
        paragraphs: impl IntoIterator<Item = S>,
    ) -> Self {
        let lines = paragraphs
            .into_iter()
            .map(|p| squeeze(p.as_ref()))
            .filter(|p| !p.is_empty())
            .map(|text| ScriptLine { speaker: NARRATOR.into(), text, about: None })
            .collect();
        Self { title: title.into(), speakers: vec![ScriptSpeaker::new(NARRATOR, "")], lines }
    }

    /// A script read from `text`, in which a line that begins `NAME:` is said
    /// by that speaker and a paragraph with no name is `default_speaker`'s.
    ///
    /// Any name will do: one the script has not met becomes a [`ScriptSpeaker`] of
    /// that name, whose id is the name in lower case with hyphens for spaces
    /// (a name that is the default speaker's id is that speaker). A name is
    /// one word, or up to three that each begin with a capital, of letters,
    /// digits, hyphens and apostrophes, and may be set in bold
    /// (`**Alex:** ...`); a space or the end of the line follows its colon, so
    /// an address (`https://`) and a time (`10:30`) are not names. That is
    /// still a guess where one speaker does the talking -- "Today: three
    /// things" names a speaker called Today -- so a caller that knows who the
    /// speakers are uses [`Self::parse_known`].
    ///
    /// Lines that follow a named one with no blank line between are more of
    /// what that speaker says; a blank line ends the turn, and what follows
    /// with no name is the default speaker's again.
    pub fn parse(title: impl Into<String>, text: &str, default_speaker: &str) -> Self {
        Self::read(title.into(), text, default_speaker, None)
    }

    /// [`Self::parse`] for a script whose speakers are known: only a name
    /// that is one of `known` (by id or by name, in any case) begins a line,
    /// and anything else with a colon is something said. The speakers it
    /// has are those of `known` that say something, as given -- their voices
    /// included -- and `default_speaker` if it is among them or says
    /// anything unnamed.
    pub fn parse_known(
        title: impl Into<String>,
        text: &str,
        default_speaker: &str,
        known: &[ScriptSpeaker],
    ) -> Self {
        Self::read(title.into(), text, default_speaker, Some(known))
    }

    fn read(
        title: String,
        text: &str,
        default_speaker: &str,
        known: Option<&[ScriptSpeaker]>,
    ) -> Self {
        let default = match default_speaker.trim() {
            "" => NARRATOR,
            id => id,
        };
        let mut speakers: Vec<ScriptSpeaker> = Vec::new();
        let mut lines: Vec<ScriptLine> = Vec::new();
        for paragraph in blocks(text) {
            // The line this paragraph has got to, for the lines that go on
            // with it.
            let mut open: Option<usize> = None;
            for raw in paragraph.lines().map(str::trim) {
                if let Some((speaker, said)) = named(raw, default, known) {
                    open = Some(push_line(&mut speakers, &mut lines, speaker, squeeze(said)));
                } else if let Some(at) = open {
                    let more = squeeze(raw);
                    let said = &mut lines[at].text;
                    if !said.is_empty() && !more.is_empty() {
                        said.push('\n');
                    }
                    said.push_str(&more);
                } else {
                    let speaker = known
                        .and_then(|k| k.iter().find(|s| s.id == default))
                        .cloned()
                        .unwrap_or_else(|| ScriptSpeaker::new(default, ""));
                    open = Some(push_line(&mut speakers, &mut lines, speaker, squeeze(raw)));
                }
            }
        }
        lines.retain(|l| !l.text.is_empty());
        speakers.retain(|s| lines.iter().any(|l| l.speaker == s.id));
        if speakers.is_empty() {
            speakers.push(ScriptSpeaker::new(default, ""));
        }
        Self { title, speakers, lines }
    }

    /// The script as [`Self::parse`] reads it: a paragraph to a line, and
    /// each line headed by its speaker's name unless the script has only the
    /// narrator, whose lines are plain paragraphs.
    pub fn tagged(&self) -> String {
        let plain = self.speakers.iter().all(|s| s.id == NARRATOR);
        self.lines
            .iter()
            .map(|line| {
                if plain {
                    return line.text.clone();
                }
                let who = self
                    .speaker(&line.speaker)
                    .map_or(line.speaker.as_str(), ScriptSpeaker::called);
                format!("{who}: {}", line.text)
            })
            .collect::<Vec<_>>()
            .join("\n\n")
    }

    /// How many words are said.
    pub fn words(&self) -> usize {
        self.lines.iter().map(|l| l.text.split_whitespace().count()).sum()
    }

    /// Everything said, in order, as plain text: a paragraph a line.
    pub fn text(&self) -> String {
        self.lines.iter().map(|l| l.text.as_str()).collect::<Vec<_>>().join("\n\n")
    }

    /// The speaker a line names, if the script has it.
    pub fn speaker(&self, id: &str) -> Option<&ScriptSpeaker> {
        self.speakers.iter().find(|s| s.id == id)
    }
}

impl ScriptSpeaker {
    /// A speaker said by `id`, called `name` (which may be empty), in the
    /// configured voice.
    pub fn new(id: impl Into<String>, name: impl Into<String>) -> Self {
        Self { id: id.into(), name: name.into(), voice: None }
    }

    /// What it is called: its name, or its id when it has none.
    pub fn called(&self) -> &str {
        if self.name.trim().is_empty() { &self.id } else { &self.name }
    }

    /// Whether `name` is this speaker's id or name, in any case.
    fn is_called(&self, name: &str) -> bool {
        let name = name.trim();
        self.id.eq_ignore_ascii_case(name)
            || (!self.name.trim().is_empty() && self.name.trim().eq_ignore_ascii_case(name))
            || self.id == speaker_id(name)
    }
}

/// `text`'s lines with each one's white space made single spaces, the line
/// breaks themselves kept, and no blank lines.
fn squeeze(text: &str) -> String {
    text.lines()
        .map(|l| l.split_whitespace().collect::<Vec<_>>().join(" "))
        .filter(|l| !l.is_empty())
        .collect::<Vec<_>>()
        .join("\n")
}

/// The blocks of `text` between blank lines (lines of nothing but white
/// space), each with its lines as they were.
fn blocks(text: &str) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    let mut open = false;
    for line in text.lines() {
        if line.trim().is_empty() {
            open = false;
        } else if open {
            let block = out.last_mut().expect("a block is open");
            block.push('\n');
            block.push_str(line);
        } else {
            out.push(line.to_string());
            open = true;
        }
    }
    out
}

/// The longest name a line may begin with, in characters, and in words.
const MAX_NAME_CHARS: usize = 24;
const MAX_NAME_WORDS: usize = 3;

/// Add a line said by `speaker`, who joins the script if it is new to it.
/// Answers where the line is.
fn push_line(
    speakers: &mut Vec<ScriptSpeaker>,
    lines: &mut Vec<ScriptLine>,
    speaker: ScriptSpeaker,
    text: String,
) -> usize {
    let id = speaker.id.clone();
    if !speakers.iter().any(|s| s.id == id) {
        speakers.push(speaker);
    }
    lines.push(ScriptLine { speaker: id, text, about: None });
    lines.len() - 1
}

/// Who a line says it is by, and what it says, if it begins with a name that
/// is one: any, or one of `known` when there are some.
fn named<'a>(
    line: &'a str,
    default: &str,
    known: Option<&[ScriptSpeaker]>,
) -> Option<(ScriptSpeaker, &'a str)> {
    let (name, said) = tag(line)?;
    let speaker = match known {
        Some(known) => known.iter().find(|s| s.is_called(name))?.clone(),
        None if speaker_id(name) == default.to_lowercase() => ScriptSpeaker::new(default, ""),
        None => ScriptSpeaker::new(speaker_id(name), name),
    };
    Some((speaker, said))
}

/// The speaker a line names and what it says, if it begins `NAME:`.
fn tag(line: &str) -> Option<(&str, &str)> {
    let line = line.trim_start_matches(['*', '_']);
    let colon = line.find(':')?;
    let name = line[..colon].trim().trim_end_matches(['*', '_']).trim();
    let after = &line[colon + 1..];
    // A name is followed by white space, the end of the line, or the bold
    // mark that closes it: not `https://`, not `10:30`.
    if !after.is_empty()
        && !after.starts_with(char::is_whitespace)
        && !after.starts_with(['*', '_'])
    {
        return None;
    }
    is_name(name).then(|| (name, after.trim_start_matches(['*', '_']).trim()))
}

/// Whether `name` is one a line of a script may begin with: one word, or up to
/// three that each begin with a capital, of letters, digits, hyphens and
/// apostrophes, starting with a letter, and no longer than 24 characters.
/// What [`Script::parse`] takes for a name, so whatever names a speaker has to
/// be one that is.
pub fn is_name(name: &str) -> bool {
    let name = name.trim();
    let words: Vec<&str> = name.split_whitespace().collect();
    !words.is_empty()
        && words.len() <= MAX_NAME_WORDS
        && name.chars().count() <= MAX_NAME_CHARS
        && words.iter().all(|w| {
            w.chars().all(|c| c.is_alphanumeric() || matches!(c, '-' | '\'' | '\u{2019}'))
                && (words.len() == 1 || w.chars().next().is_some_and(char::is_uppercase))
                && w.chars().next().is_some_and(char::is_alphabetic)
        })
}

/// The id a name is kept under: lower case, words joined with hyphens.
fn speaker_id(name: &str) -> String {
    name.split_whitespace().collect::<Vec<_>>().join("-").to_lowercase()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_narrated_script_is_a_line_a_paragraph() {
        let s = Script::narrated("Daily", "Good morning.\n\n\n  Three things today:\nfirst.  \n\n");
        assert_eq!(s.lines.len(), 2);
        assert_eq!(s.lines[1].text, "Three things today: first.");
        assert!(s.lines.iter().all(|l| l.speaker == NARRATOR));
        assert!(s.speaker(NARRATOR).is_some());
        assert_eq!(s.text(), "Good morning.\n\nThree things today: first.");
    }

    fn who(s: &Script) -> Vec<(&str, &str)> {
        s.lines.iter().map(|l| (l.speaker.as_str(), l.text.as_str())).collect()
    }

    #[test]
    fn paragraphs_keep_the_breaks_inside_them() {
        let s =
            Script::narrated_paragraphs("Brief", ["Three things:\n- one\n  - two", "  ", "Done."]);
        assert_eq!(who(&s), [("narrator", "Three things:\n- one\n- two"), ("narrator", "Done.")]);
        assert_eq!(s.speakers, [ScriptSpeaker::new(NARRATOR, "")]);
    }

    #[test]
    fn named_lines_are_said_by_their_speakers() {
        let s = Script::parse(
            "Talk",
            "Welcome.\n\nALEX: Hello there.\nSam: Hi, Alex.\nHow are you?\n\n**Alex:** Fine.\n\nGuest Two: Hey.",
            NARRATOR,
        );
        assert_eq!(
            who(&s),
            [
                ("narrator", "Welcome."),
                ("alex", "Hello there."),
                ("sam", "Hi, Alex.\nHow are you?"),
                ("alex", "Fine."),
                ("guest-two", "Hey."),
            ]
        );
        let names: Vec<_> = s.speakers.iter().map(|s| (s.id.as_str(), s.name.as_str())).collect();
        assert_eq!(
            names,
            [("narrator", ""), ("alex", "ALEX"), ("sam", "Sam"), ("guest-two", "Guest Two")]
        );
    }

    #[test]
    fn a_blank_line_ends_a_turn_and_the_default_speaker_has_the_rest() {
        let s = Script::parse("Talk", "Sam: One.\n\nTwo.", "host");
        assert_eq!(who(&s), [("sam", "One."), ("host", "Two.")]);
        // A name that is the default's is the default.
        let s = Script::parse("Talk", "Host: One.\n\nTwo.", "host");
        assert_eq!(who(&s), [("host", "One."), ("host", "Two.")]);
        assert_eq!(s.speakers.len(), 1);
    }

    #[test]
    fn what_only_looks_like_a_name_is_not_one() {
        let text = "Here's the plan: first.\n\nSee https://example.com/a for more.\n\n10:30 is when.\n\nThree things today: one.";
        let s = Script::parse("Plan", text, NARRATOR);
        assert_eq!(s.lines.len(), 4);
        assert!(s.lines.iter().all(|l| l.speaker == NARRATOR), "{:?}", s.lines);
        assert_eq!(s.speakers.len(), 1);
        // A name in bold, and a name with nothing after it on the line.
        let s = Script::parse("Talk", "**Sam**: Hi.\n\nAlex:\nHello.", NARRATOR);
        assert_eq!(who(&s), [("sam", "Hi."), ("alex", "Hello.")]);
    }

    #[test]
    fn a_script_that_knows_its_speakers_says_only_theirs() {
        let mut alex = ScriptSpeaker::new("alex", "Alex");
        alex.voice = Some("Miles".into());
        let known = [alex.clone(), ScriptSpeaker::new("sam", "Sam")];
        let s = Script::parse_known(
            "Talk",
            "Alex: Today: three things.\n\nNote: this is not a speaker.\n\nsam: Right.",
            NARRATOR,
            &known,
        );
        assert_eq!(
            who(&s),
            [
                ("alex", "Today: three things."),
                ("narrator", "Note: this is not a speaker."),
                ("sam", "Right."),
            ]
        );
        assert_eq!(s.speaker("alex"), Some(&alex), "as given, voice and all");
        // Known speakers that say nothing are not in the script.
        let s = Script::parse_known("Talk", "Alex: Hi.", NARRATOR, &known);
        assert_eq!(s.speakers, [alex]);
    }

    #[test]
    fn an_empty_script_still_has_a_speaker() {
        let s = Script::parse("Nothing", " \n\n ", "host");
        assert!(s.lines.is_empty());
        assert_eq!(s.speakers, [ScriptSpeaker::new("host", "")]);
    }

    #[test]
    fn a_script_is_written_as_it_is_read() {
        let s = Script::parse("Talk", "ALEX: Hello there.\n\nSam: Hi.\n\nAlex: Bye.", NARRATOR);
        assert_eq!(s.tagged(), "ALEX: Hello there.\n\nSam: Hi.\n\nALEX: Bye.");
        assert_eq!(Script::parse("Talk", &s.tagged(), NARRATOR).lines, s.lines);
        // One narrator is only paragraphs.
        let n = Script::narrated("Daily", "Good morning.\n\nThree things.");
        assert_eq!(n.tagged(), "Good morning.\n\nThree things.");
        assert_eq!(n.words(), 4);
    }
}
