//! Roles and goals, as one page.
//!
//! ```text
//!   purpose/
//!     roles-and-goals.md
//! ```
//!
//! One file, not a folder. There are a handful of roles and a few dozen goals
//! in a whole life, and splitting them across forty files would be filing for
//! the sake of it. What they want to be is a page you can read top to bottom
//! and say "yes, that is what I am trying to do" -- so a role is a heading, a
//! goal is a checklist item under it, and the whole thing is one document.
//!
//! Every id in this file is the id half the vault points at: a purpose on an
//! entry, a task, an hour or a book is written `goal:<id>`, and it is *this*
//! part that says what that id means. Importing the rest of an archive
//! without this one leaves those pointers dangling, which is why the interface
//! ticks this part along with anything that carries a purpose.

use super::doc;
use crate::text::{FrontMatter, split_front_matter};
use crate::{Files, Mode, Options, Part, Portable, Report, Spec, land};
use everyday_core::purpose::{Goal, GoalStatus, Role};
use everyday_core::store::JournalStore;
use everyday_core::store::purpose::{GoalQuery, PurposeStore};
use everyday_core::{GoalId, Result, RoleId};
use std::fmt::Write as _;

pub struct PurposePart;
pub static PURPOSE: PurposePart = PurposePart;

static SPEC: Spec = Spec {
    id: "purpose",
    label: "Roles and goals",
    summary: "Who you are being and what you are trying to do — the axis every balance \
              chart is drawn against, and what the rest of the vault points at.",
    format: "One Markdown page: a heading per role, a checklist item per goal",
    media: false,
    imports: true,
};

const PAGE: &str = "roles-and-goals.md";

impl Portable for PurposePart {
    fn spec(&self) -> &'static Spec {
        &SPEC
    }

    fn tally(&self, store: &dyn JournalStore) -> Result<Option<u64>> {
        let Some(purpose) = store.purpose() else { return Ok(None) };
        let roles = purpose.list_roles()?.len() as u64;
        Ok(Some(roles + purpose.list_goals(&GoalQuery::default())?.len() as u64))
    }

    fn export(&self, store: &dyn JournalStore, out: &mut Files<'_>, _opts: &Options) -> Result<()> {
        let Some(purpose) = store.purpose() else { return Ok(()) };
        let roles = purpose.list_roles()?;
        let goals = purpose.list_goals(&GoalQuery::default())?;
        if roles.is_empty() && goals.is_empty() {
            return Ok(());
        }

        let mut front = FrontMatter::new();
        front.always("page", "Roles and goals");
        let mut body = front.render();
        body.push_str("# Roles and goals\n\n");

        for role in &roles {
            let _ = write!(body, "## {} {}\n\n", role.icon, role.name.trim());
            let mut hidden = format!("id:{}", role.id);
            let _ = write!(hidden, "; color:{}", role.color);
            if role.archived {
                hidden.push_str("; archived:true");
            }
            let _ = write!(hidden, "; created:{}", doc::stamp(role.created_at));
            let _ = writeln!(body, "<!-- {hidden} -->\n");
            if !role.notes.trim().is_empty() {
                let _ = write!(body, "{}\n\n", role.notes.trim());
            }
            for goal in goals.iter().filter(|g| g.role_id == role.id) {
                write_goal(&mut body, goal);
            }
            body.push('\n');
        }

        // A goal whose role has gone still says what a year of hours was for.
        let orphans: Vec<&Goal> =
            goals.iter().filter(|g| !roles.iter().any(|r| r.id == g.role_id)).collect();
        if !orphans.is_empty() {
            body.push_str("## Unfiled\n\n");
            for goal in orphans {
                write_goal(&mut body, goal);
            }
        }

        out.records(PAGE, body, (roles.len() + goals.len()) as u64)?;
        Ok(())
    }

    fn import(&self, store: &dyn JournalStore, src: &Part<'_>, mode: Mode) -> Result<Report> {
        let mut report = Report::new(SPEC.id);
        let Some(purpose) = store.purpose() else {
            report.problem(SPEC.label, "this vault's backend does not store roles and goals");
            return Ok(report);
        };
        for (name, source) in src.documents(".md") {
            if let Err(e) = read_page(purpose, source, mode, &mut report) {
                report.problem(name, e);
            }
        }
        Ok(report)
    }
}

fn write_goal(body: &mut String, goal: &Goal) {
    let box_ = if matches!(goal.status, GoalStatus::Done) { "x" } else { " " };
    let _ = write!(body, "- [{box_}] {}", goal.title.trim());
    if let Some(horizon) = goal.horizon {
        let _ = write!(body, " @{horizon}");
    }
    if !matches!(goal.status, GoalStatus::Active | GoalStatus::Done) {
        let _ = write!(body, " %{}", goal.status.as_str());
    }
    let mut hidden = format!("id:{}; role:{}", goal.id, goal.role_id);
    let _ = write!(hidden, "; created:{}", doc::stamp(goal.created_at));
    if let Some(done) = goal.completed_at {
        let _ = write!(hidden, "; completed:{}", doc::stamp(done));
    }
    let _ = writeln!(body, " <!-- {hidden} -->");
    for line in goal.notes.trim().lines().filter(|l| !l.trim().is_empty()) {
        let _ = writeln!(body, "  {line}");
    }
}

/// One role's block, as it was read off the page.
///
/// Buffered rather than written line by line, because a role's notes are the
/// prose *after* its heading and a goal's notes are the indented lines *after*
/// its checklist item -- so neither record is complete at the moment its own
/// line goes past. The first cut of this wrote each one as soon as it was
/// recognised, and silently dropped every note in the vault.
#[derive(Default)]
struct Block {
    heading: String,
    fields: std::collections::BTreeMap<String, String>,
    notes: String,
    goals: Vec<Pending>,
}

struct Pending {
    done: bool,
    line: String,
    notes: String,
}

fn read_page(
    purpose: &dyn PurposeStore,
    source: &str,
    mode: Mode,
    report: &mut Report,
) -> Result<()> {
    let (_, body) = split_front_matter(source);
    let mut blocks: Vec<Block> = Vec::new();
    let mut loose: Vec<Pending> = Vec::new();

    for line in body.lines() {
        let trimmed = line.trim();
        if let Some(heading) = trimmed.strip_prefix("## ") {
            blocks.push(Block { heading: heading.trim().to_string(), ..Default::default() });
            continue;
        }
        if trimmed.starts_with("<!--")
            && let Some(block) = blocks.last_mut()
            && block.fields.is_empty()
            && block.goals.is_empty()
        {
            block.fields = hidden(trimmed);
            continue;
        }
        if let Some((done, rest)) = goal_line(trimmed) {
            let goal = Pending { done, line: rest.to_string(), notes: String::new() };
            match blocks.last_mut() {
                Some(block) => block.goals.push(goal),
                None => loose.push(goal),
            }
            continue;
        }
        if trimmed.is_empty() || trimmed.starts_with('#') {
            continue;
        }
        // Anything else is prose: a goal's, if one is open and the line is
        // indented under it, else the role's own.
        match blocks.last_mut() {
            Some(block) => {
                let target = match block.goals.last_mut() {
                    Some(goal) if line.starts_with(' ') || line.starts_with('\t') => {
                        &mut goal.notes
                    }
                    _ => &mut block.notes,
                };
                if !target.is_empty() {
                    target.push('\n');
                }
                target.push_str(trimmed);
            }
            None => {
                if let Some(goal) = loose.last_mut() {
                    if !goal.notes.is_empty() {
                        goal.notes.push('\n');
                    }
                    goal.notes.push_str(trimmed);
                }
            }
        }
    }

    let mut goal_order = 0;
    for (role_order, block) in blocks.iter().enumerate() {
        let role_id = match read_role(purpose, block, role_order as i32, mode, report) {
            Ok(id) => id,
            Err(e) => {
                report.problem(PAGE, e);
                continue;
            }
        };
        for goal in &block.goals {
            write_one(purpose, goal, role_id, goal_order, mode, report);
            goal_order += 1;
        }
    }

    // Goals on a page that never named a role: a list somebody started by
    // hand. They need a role to hang from, because every report is drawn
    // against one.
    if !loose.is_empty() {
        let role_id = default_role(purpose)?;
        for goal in &loose {
            write_one(purpose, goal, role_id, goal_order, mode, report);
            goal_order += 1;
        }
    }
    Ok(())
}

fn write_one(
    purpose: &dyn PurposeStore,
    goal: &Pending,
    role_id: RoleId,
    order: i32,
    mode: Mode,
    report: &mut Report,
) {
    if let Err(e) = read_goal(purpose, goal, role_id, order, mode, report) {
        report.problem(PAGE, e);
    }
}

/// `- [ ] title @horizon %status <!-- ... -->`, minus the box.
fn goal_line(line: &str) -> Option<(bool, &str)> {
    let rest = line.strip_prefix("- ")?;
    if let Some(r) = rest.strip_prefix("[x] ").or_else(|| rest.strip_prefix("[X] ")) {
        return Some((true, r));
    }
    rest.strip_prefix("[ ] ").map(|r| (false, r))
}

fn read_role(
    purpose: &dyn PurposeStore,
    block: &Block,
    order: i32,
    mode: Mode,
    report: &mut Report,
) -> Result<RoleId> {
    let id = block.fields.get("id").and_then(|id| RoleId::parse(id).ok()).unwrap_or_default();
    // The heading is "<icon> <name>", and the icon is one grapheme. Splitting
    // on the first space is wrong for an icon that is two code points -- a
    // flag, a profession with a modifier -- so the split is on the first
    // *alphanumeric* run instead.
    let (icon, name) = split_icon(&block.heading);

    let existing = purpose.get_role(id).ok();
    let mut landing = land(existing, || Role::new(name), mode);
    let Some(role) = landing.as_mut() else {
        report.landed(&landing);
        return Ok(id);
    };
    role.id = id;
    role.name = name.to_string();
    role.notes = block.notes.clone();
    if !icon.is_empty() {
        role.icon = icon.to_string();
    }
    if let Some(color) = block.fields.get("color") {
        role.color = color.clone();
    }
    role.archived = block.fields.get("archived").is_some_and(|a| a == "true");
    role.sort_order = order;
    if let Some(created) = block.fields.get("created").and_then(|c| c.parse().ok()) {
        role.created_at = created;
    }
    role.updated_at = jiff::Timestamp::now();
    purpose.put_role(role)?;
    report.landed(&landing);
    Ok(id)
}

fn read_goal(
    purpose: &dyn PurposeStore,
    pending: &Pending,
    role_id: RoleId,
    order: i32,
    mode: Mode,
    report: &mut Report,
) -> Result<()> {
    let (visible, fields) = match pending.line.split_once("<!--") {
        Some((visible, hidden_part)) => (visible, hidden(hidden_part)),
        None => (pending.line.as_str(), Default::default()),
    };
    let id = fields.get("id").and_then(|id| GoalId::parse(id).ok()).unwrap_or_default();

    let mut title: Vec<&str> = Vec::new();
    let mut horizon = None;
    let mut status = None;
    for word in visible.split_whitespace() {
        match word.chars().next() {
            Some('@') if word[1..].parse::<jiff::civil::Date>().is_ok() => {
                horizon = word[1..].parse().ok();
            }
            Some('%') if goal_status(&word[1..]).is_some() => status = goal_status(&word[1..]),
            _ => title.push(word),
        }
    }

    let existing = purpose.get_goal(id).ok();
    let mut landing = land(existing, || Goal::new(role_id, title.join(" ")), mode);
    let Some(goal) = landing.as_mut() else {
        report.landed(&landing);
        return Ok(());
    };
    goal.id = id;
    goal.role_id = fields.get("role").and_then(|r| RoleId::parse(r).ok()).unwrap_or(role_id);
    goal.title = title.join(" ").trim().to_string();
    goal.notes = pending.notes.clone();
    goal.horizon = horizon;
    goal.status =
        status.unwrap_or(if pending.done { GoalStatus::Done } else { GoalStatus::Active });
    goal.sort_order = order;
    if let Some(created) = fields.get("created").and_then(|c| c.parse().ok()) {
        goal.created_at = created;
    }
    goal.updated_at = jiff::Timestamp::now();
    goal.completed_at = match goal.status {
        GoalStatus::Done => fields
            .get("completed")
            .and_then(|c| c.parse().ok())
            .or(goal.completed_at)
            .or(Some(goal.updated_at)),
        _ => None,
    };
    purpose.put_goal(goal)?;
    report.landed(&landing);
    Ok(())
}

/// A role for goals that arrived without one.
fn default_role(purpose: &dyn PurposeStore) -> Result<RoleId> {
    if let Some(role) = purpose.list_roles()?.into_iter().find(|r| r.name == "Unfiled") {
        return Ok(role.id);
    }
    let role = Role::new("Unfiled");
    purpose.put_role(&role)?;
    Ok(role.id)
}

/// `<!-- key:value; key:value -->` as a map.
fn hidden(text: &str) -> std::collections::BTreeMap<String, String> {
    let inner = text.trim().trim_start_matches("<!--").trim_end_matches("-->");
    inner
        .split(';')
        .filter_map(|field| {
            let (key, value) = field.split_once(':')?;
            Some((key.trim().to_string(), value.trim().to_string()))
        })
        .collect()
}

fn split_icon(heading: &str) -> (&str, &str) {
    match heading.split_once(' ') {
        Some((first, rest)) if !first.chars().any(char::is_alphanumeric) && !first.is_empty() => {
            (first, rest.trim())
        }
        _ => ("", heading.trim()),
    }
}

fn goal_status(word: &str) -> Option<GoalStatus> {
    GoalStatus::parse(&word.trim().to_ascii_lowercase())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_heading_gives_up_its_icon_without_eating_the_name() {
        assert_eq!(
            split_icon("\u{1f9d1}\u{200d}\u{1f4bb} Engineer"),
            ("\u{1f9d1}\u{200d}\u{1f4bb}", "Engineer")
        );
        assert_eq!(split_icon("Parent"), ("", "Parent"));
        assert_eq!(split_icon("\u{1f3e0} Home owner"), ("\u{1f3e0}", "Home owner"));
    }

    #[test]
    fn hidden_fields_read_back() {
        let map = hidden("<!-- id:abc; color:#ff0000; archived:true -->");
        assert_eq!(map.get("id").map(String::as_str), Some("abc"));
        assert_eq!(map.get("color").map(String::as_str), Some("#ff0000"));
    }

    #[test]
    fn a_goal_line_is_recognised_ticked_or_not() {
        assert_eq!(goal_line("- [x] Done thing"), Some((true, "Done thing")));
        assert_eq!(goal_line("- [ ] Open thing"), Some((false, "Open thing")));
        assert!(goal_line("## A role").is_none());
    }
}
