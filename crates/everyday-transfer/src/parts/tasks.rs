//! The todo app, as Markdown checklists and a table of hours.
//!
//! ```text
//!   todo/
//!     Inbox.md                       captured, not filed
//!     Home-renovation-a1b2c3d4.md    one file per project
//!     time.csv                       every block of time, planned and actual
//!     time.ics                       the same, for a calendar to read
//! ```
//!
//! # Why a checklist
//!
//! `- [ ] thing` is the one task format every editor, every note app and
//! every code host already renders. A project exports as the checklist a
//! person would have written by hand, and the fields the app knows about are
//! written in the notation its own capture line uses:
//!
//! ```text
//!   - [ ] Book the flights #travel !high ~1h30 @2026-09-12 16:30
//!         └── title ──────┘ └tag─┘ └prio┘ └est┘ └────── due ─────┘
//! ```
//!
//! which is exactly what the quick-add field in the todo app takes. Somebody
//! editing an exported project is writing the same language they type into
//! the application, rather than learning a second one.
//!
//! What has no readable notation -- the record's id, what it is *for*, when
//! it was captured -- rides in an HTML comment at the end of the line, which
//! every Markdown renderer draws as nothing at all. A line with no comment is
//! a new task, which is what makes adding one to the file work.
//!
//! # Why the hours are a table
//!
//! A block of time is a row: it starts, it ends, it was the plan or it was
//! what happened. `time.csv` is that, and it is what an import reads. It is
//! also written as `time.ics`, which is not read back -- an `.ics` has no
//! field for "this is the record rather than the plan", and inventing one
//! that only this application understood would be a worse answer than a
//! spreadsheet. The `.ics` is there so a week's work can be dropped into any
//! calendar; the `.csv` is there so it can come home.

use super::doc;
use crate::text::{Csv, FrontMatter, Table, safe_name};
use crate::{Files, Mode, Options, Part, Portable, Report, Spec, land};
use everyday_core::store::JournalStore;
use everyday_core::store::tasks::{BlockQuery, TaskQuery, TaskStore};
use everyday_core::task::{
    BlockKind, BlockSubject, Priority, Project, ProjectStatus, Task, TaskStatus, TimeBlock,
};
use everyday_core::{BlockId, ProjectId, Result, TaskId};
use jiff::civil::{Date, Time};
use std::collections::BTreeMap;
use std::fmt::Write as _;

pub struct TasksPart;
pub static TASKS: TasksPart = TasksPart;

static SPEC: Spec = Spec {
    id: "todo",
    label: "Todo",
    summary: "Projects, tasks and subtasks as Markdown checklists, and every block of \
              time booked against them — the plan and what actually happened.",
    format: "Markdown task lists, plus CSV and iCalendar for the hours",
    media: false,
    imports: true,
};

/// Tasks that are in no project.
const INBOX: &str = "Inbox.md";

impl Portable for TasksPart {
    fn spec(&self) -> &'static Spec {
        &SPEC
    }

    fn tally(&self, store: &dyn JournalStore) -> Result<Option<u64>> {
        let Some(tasks) = store.tasks() else { return Ok(None) };
        Ok(Some(tasks.list_tasks(&TaskQuery::default())?.len() as u64))
    }

    fn export(&self, store: &dyn JournalStore, out: &mut Files<'_>, _opts: &Options) -> Result<()> {
        let Some(tasks) = store.tasks() else { return Ok(()) };
        let all = tasks.list_tasks(&TaskQuery::default())?;
        let projects = tasks.list_projects()?;

        // Children by parent, and roots by project, so the tree is walked
        // once rather than scanned per task.
        let mut children: BTreeMap<TaskId, Vec<&Task>> = BTreeMap::new();
        let mut roots: BTreeMap<Option<ProjectId>, Vec<&Task>> = BTreeMap::new();
        for task in &all {
            match task.parent_id {
                Some(parent) => children.entry(parent).or_default().push(task),
                None => roots.entry(task.project_id).or_default().push(task),
            }
        }
        let order = |list: &mut Vec<&Task>| {
            list.sort_by(|a, b| {
                a.sort_order.cmp(&b.sort_order).then_with(|| a.created_at.cmp(&b.created_at))
            })
        };
        children.values_mut().for_each(order);
        roots.values_mut().for_each(order);

        for project in &projects {
            let mut body = project_front(project).render();
            let _ = write!(body, "# {}\n\n", project.name);
            if !project.notes.trim().is_empty() {
                let _ = write!(body, "{}\n\n", project.notes.trim());
            }
            let mut count = 1;
            body.push_str("## Tasks\n\n");
            for task in roots.get(&Some(project.id)).into_iter().flatten() {
                count += render_task(&mut body, task, &children, 0);
            }
            let name = format!("{}-{}.md", safe_name(&project.name), project.id.short());
            out.records(&name, body, count)?;
        }

        // The inbox is a file even when it is empty, because "nothing is
        // waiting to be filed" is worth being able to see.
        let loose = roots.get(&None).cloned().unwrap_or_default();
        let mut body = String::from("# Inbox\n\nTasks that are in no project.\n\n");
        let mut count = 0;
        for task in &loose {
            count += render_task(&mut body, task, &children, 0);
        }
        out.records(INBOX, body, count)?;

        let blocks = tasks.list_blocks(&BlockQuery::default())?;
        if !blocks.is_empty() {
            out.records("time.csv", time_csv(&blocks), blocks.len() as u64)?;
            out.text("time.ics", time_ics(&blocks, &all, &projects))?;
        }
        Ok(())
    }

    fn import(&self, store: &dyn JournalStore, src: &Part<'_>, mode: Mode) -> Result<Report> {
        let mut report = Report::new(SPEC.id);
        let Some(tasks) = store.tasks() else {
            report.problem(SPEC.label, "this vault's backend does not store tasks");
            return Ok(report);
        };

        for (name, source) in src.documents(".md") {
            if let Err(e) = read_project(tasks, name, source, mode, &mut report) {
                report.problem(name, e);
            }
        }
        if let Some(csv) = src.text("time.csv") {
            read_time(tasks, csv, mode, &mut report);
        }
        Ok(report)
    }
}

// ---- projects and tasks, out --------------------------------------------

fn project_front(project: &Project) -> FrontMatter {
    let mut front = FrontMatter::new();
    front
        .always("project", &project.name)
        .always("id", project.id.to_string())
        .always("status", project.status.as_str())
        .set("priority", nonzero_priority(project.priority))
        .list("tags", &project.tags)
        .set("purpose", doc::purpose_text(project.purpose.as_ref()))
        .set_opt("start", project.start_date)
        .set_opt("due", project.due_date)
        .set("estimate", project.estimate_minutes.map(duration).unwrap_or_default())
        .set("color", &project.color)
        .set("icon", &project.icon)
        .set_opt("order", Some(project.sort_order))
        .set("created", doc::stamp(project.created_at))
        .set("updated", doc::stamp(project.updated_at));
    front
}

/// One task, its notes, and everything under it. Answers with how many rows
/// it wrote, subtasks included.
fn render_task(
    out: &mut String,
    task: &Task,
    children: &BTreeMap<TaskId, Vec<&Task>>,
    depth: usize,
) -> u64 {
    let pad = "  ".repeat(depth);
    let box_ = if matches!(task.status, TaskStatus::Done) { "x" } else { " " };
    let _ = write!(out, "{pad}- [{box_}] {}", task.title.trim());

    for tag in &task.tags {
        let _ = write!(out, " #{}", tag.replace(' ', "-"));
    }
    if task.priority != Priority::None {
        let _ = write!(out, " !{}", task.priority.as_str());
    }
    if let Some(minutes) = task.estimate_minutes {
        let _ = write!(out, " ~{}", duration(minutes));
    }
    if let Some(due) = task.due_date {
        let _ = write!(out, " @{due}");
        if let Some(time) = task.due_time {
            let _ = write!(out, " {:02}:{:02}", time.hour(), time.minute());
        }
    }
    if let Some(start) = task.start_date {
        let _ = write!(out, " @start:{start}");
    }
    // The checkbox says done or not done; the other four statuses have no
    // box to be, and are written as a word.
    if !matches!(task.status, TaskStatus::Todo | TaskStatus::Done) {
        let _ = write!(out, " %{}", task.status.as_str());
    }

    let mut hidden = format!("id:{}", task.id);
    if let Some(purpose) = &task.purpose {
        let _ = write!(hidden, "; purpose:{}", doc::purpose_text(Some(purpose)));
    }
    let _ = write!(hidden, "; created:{}", doc::stamp(task.created_at));
    if let Some(done) = task.completed_at {
        let _ = write!(hidden, "; completed:{}", doc::stamp(done));
    }
    let _ = writeln!(out, " <!-- {hidden} -->");

    if !task.notes.trim().is_empty() {
        for line in task.notes.trim().lines() {
            let _ = writeln!(out, "{pad}  {line}");
        }
    }

    let mut count = 1;
    for child in children.get(&task.id).into_iter().flatten() {
        count += render_task(out, child, children, depth + 1);
    }
    count
}

/// `90` becomes `1h30`, `45` becomes `45m`. What the capture line takes.
fn duration(minutes: u32) -> String {
    match (minutes / 60, minutes % 60) {
        (0, m) => format!("{m}m"),
        (h, 0) => format!("{h}h"),
        (h, m) => format!("{h}h{m:02}"),
    }
}

fn parse_duration(text: &str) -> Option<u32> {
    let text = text.trim();
    if let Some((h, m)) = text.split_once('h') {
        let hours: u32 = h.parse().ok()?;
        let mins: u32 =
            if m.trim().is_empty() { 0 } else { m.trim_end_matches('m').parse().ok()? };
        return Some(hours * 60 + mins);
    }
    text.trim_end_matches('m').parse().ok()
}

fn nonzero_priority(priority: Priority) -> &'static str {
    if priority == Priority::None { "" } else { priority.as_str() }
}

// ---- projects and tasks, back in ----------------------------------------

/// One task as it was read off a line, before it is turned into a record.
struct Parsed {
    depth: usize,
    title: String,
    notes: String,
    status: Option<TaskStatus>,
    done: bool,
    priority: Priority,
    estimate: Option<u32>,
    due: Option<Date>,
    due_time: Option<Time>,
    start: Option<Date>,
    tags: Vec<String>,
    id: Option<TaskId>,
    purpose: Option<everyday_core::purpose::Purpose>,
    created: Option<jiff::Timestamp>,
    completed: Option<jiff::Timestamp>,
}

fn read_project(
    tasks: &dyn TaskStore,
    name: &str,
    source: &str,
    mode: Mode,
    report: &mut Report,
) -> Result<()> {
    let (fields, body) = crate::text::split_front_matter(source);
    let file = name.rsplit('/').next().unwrap_or(name);

    // A file with no project id and called `Inbox.md` is the inbox. Any other
    // file without one is a project somebody wrote by hand, and gets made.
    let project_id = if file == INBOX && fields.get("id").is_none() {
        None
    } else {
        let id = fields.parse::<ProjectId>("id").unwrap_or_else(ProjectId::new);
        let title = fields
            .get("project")
            .map(str::to_string)
            .or_else(|| heading(body))
            .unwrap_or_else(|| file.trim_end_matches(".md").replace('-', " "));

        let existing = tasks.get_project(id).ok();
        let mut landing = land(existing, || Project::new(&title), mode);
        if let Some(project) = landing.as_mut() {
            project.id = id;
            project.name = title;
            project.notes = prose(body);
            project.status = project_status(&fields.text("status")).unwrap_or(project.status);
            project.priority = priority(&fields.text("priority")).unwrap_or(project.priority);
            project.tags = fields.list("tags");
            project.purpose = doc::parse_purpose(&fields.text("purpose"));
            project.start_date = fields.parse("start");
            project.due_date = fields.parse("due");
            project.estimate_minutes = fields.get("estimate").and_then(parse_duration);
            if let Some(color) = fields.get("color") {
                project.color = color.to_string();
            }
            if let Some(icon) = fields.get("icon") {
                project.icon = icon.to_string();
            }
            if let Some(order) = fields.parse("order") {
                project.sort_order = order;
            }
            project.created_at = fields.parse("created").unwrap_or(project.created_at);
            project.updated_at = jiff::Timestamp::now();
            tasks.put_project(project)?;
        }
        report.landed(&landing);
        Some(id)
    };

    // The tree is rebuilt from the indentation, so a line's parent is
    // whichever task most recently appeared at a shallower depth.
    let mut ancestry: Vec<TaskId> = Vec::new();
    let mut order = 0;
    for parsed in parse_tasks(body) {
        ancestry.truncate(parsed.depth);
        let parent = ancestry.last().copied();
        let id = parsed.id.unwrap_or_else(TaskId::new);
        ancestry.push(id);

        let existing = tasks.get_task(id).ok();
        let mut landing = land(existing, || Task::new(&parsed.title), mode);
        let Some(task) = landing.as_mut() else {
            report.landed(&landing);
            continue;
        };
        task.id = id;
        task.project_id = project_id;
        task.parent_id = parent;
        task.title = parsed.title;
        task.notes = parsed.notes;
        task.status =
            parsed.status.unwrap_or(if parsed.done { TaskStatus::Done } else { TaskStatus::Todo });
        task.priority = parsed.priority;
        task.estimate_minutes = parsed.estimate;
        task.due_date = parsed.due;
        task.due_time = parsed.due_time;
        task.start_date = parsed.start;
        task.tags = parsed.tags;
        task.purpose = parsed.purpose;
        task.sort_order = order;
        task.created_at = parsed.created.unwrap_or(task.created_at);
        task.updated_at = jiff::Timestamp::now();
        // A finished task with no stamp gets one now rather than none: the
        // "what did I finish this week" column reads this field, and a null
        // in it makes a done task invisible to every report.
        task.completed_at = match task.status {
            TaskStatus::Done => parsed.completed.or(task.completed_at).or(Some(task.updated_at)),
            _ => None,
        };
        tasks.put_task(task)?;
        report.landed(&landing);
        order += 1;
    }
    Ok(())
}

/// The `# Heading` of a project file.
fn heading(body: &str) -> Option<String> {
    body.lines().find(|l| !l.trim().is_empty())?.strip_prefix("# ").map(|h| h.trim().to_string())
}

/// The prose between the heading and the first task, which is the project's
/// own description.
fn prose(body: &str) -> String {
    let mut out = String::new();
    for line in body.lines() {
        let trimmed = line.trim();
        if trimmed.starts_with("## ") || is_task_line(line).is_some() {
            break;
        }
        if trimmed.starts_with("# ") {
            continue;
        }
        out.push_str(line);
        out.push('\n');
    }
    out.trim().to_string()
}

/// `(indent, done, rest)` for a line that is a checklist item.
fn is_task_line(line: &str) -> Option<(usize, bool, &str)> {
    let indent = line.len() - line.trim_start().len();
    let trimmed = line.trim_start();
    let rest = trimmed
        .strip_prefix("- ")
        .or_else(|| trimmed.strip_prefix("* "))
        .or_else(|| trimmed.strip_prefix("+ "))?;
    if let Some(r) = rest.strip_prefix("[x] ").or_else(|| rest.strip_prefix("[X] ")) {
        return Some((indent, true, r));
    }
    rest.strip_prefix("[ ] ").map(|r| (indent, false, r))
}

fn parse_tasks(body: &str) -> Vec<Parsed> {
    let mut out: Vec<Parsed> = Vec::new();
    // Indentation is in spaces, but nothing says how many per level -- a
    // person may have used two, four, or a tab that became four. Levels are
    // therefore worked out from the indents actually seen, in order.
    let mut ladder: Vec<usize> = Vec::new();

    for line in body.lines() {
        let Some((indent, done, rest)) = is_task_line(line) else {
            // An indented line under a task is that task's notes. Anything
            // else -- a heading, a blank, a paragraph -- is not.
            if let Some(task) = out.last_mut()
                && !line.trim().is_empty()
                && line.starts_with(' ')
                && !line.trim_start().starts_with('#')
            {
                if !task.notes.is_empty() {
                    task.notes.push('\n');
                }
                task.notes.push_str(line.trim());
            }
            continue;
        };

        while ladder.last().is_some_and(|&l| l >= indent) {
            ladder.pop();
        }
        ladder.push(indent);
        let depth = ladder.len() - 1;
        out.push(parse_task_line(depth, done, rest));
    }
    out
}

fn parse_task_line(depth: usize, done: bool, rest: &str) -> Parsed {
    let mut parsed = Parsed {
        depth,
        title: String::new(),
        notes: String::new(),
        status: None,
        done,
        priority: Priority::None,
        estimate: None,
        due: None,
        due_time: None,
        start: None,
        tags: Vec::new(),
        id: None,
        purpose: None,
        created: None,
        completed: None,
    };

    // The hidden fields first, so the visible half is parsed without them.
    let (visible, hidden) = match rest.split_once("<!--") {
        Some((visible, rest)) => (visible, rest.trim_end().trim_end_matches("-->")),
        None => (rest, ""),
    };
    for field in hidden.split(';') {
        let Some((key, value)) = field.split_once(':') else { continue };
        let value = value.trim();
        match key.trim() {
            "id" => parsed.id = TaskId::parse(value).ok(),
            "purpose" => parsed.purpose = doc::parse_purpose(value),
            "created" => parsed.created = value.parse().ok(),
            "completed" => parsed.completed = value.parse().ok(),
            _ => {}
        }
    }

    // The same rule the capture line follows: a marker that is not understood
    // stays in the title rather than being silently dropped.
    let mut title: Vec<&str> = Vec::new();
    let mut words = visible.split_whitespace().peekable();
    while let Some(word) = words.next() {
        match word.chars().next() {
            Some('#') if word.len() > 1 => parsed.tags.push(word[1..].replace('-', " ")),
            Some('!') if priority(&word[1..]).is_some() => {
                parsed.priority = priority(&word[1..]).unwrap_or_default();
            }
            Some('~') if parse_duration(&word[1..]).is_some() => {
                parsed.estimate = parse_duration(&word[1..]);
            }
            Some('%') if status(&word[1..]).is_some() => parsed.status = status(&word[1..]),
            Some('@') => {
                let value = &word[1..];
                if let Some(date) = value.strip_prefix("start:").and_then(|d| d.parse().ok()) {
                    parsed.start = Some(date);
                } else if let Ok(date) = value.parse::<Date>() {
                    parsed.due = Some(date);
                    // A clock time immediately after a date belongs to it.
                    if let Some(next) = words.peek()
                        && let Some(time) = parse_clock(next)
                    {
                        parsed.due_time = Some(time);
                        words.next();
                    }
                } else if let Some(time) = parse_clock(value) {
                    parsed.due_time = Some(time);
                } else {
                    title.push(word);
                }
            }
            _ => title.push(word),
        }
    }
    parsed.title = title.join(" ").trim().to_string();
    parsed
}

fn parse_clock(text: &str) -> Option<Time> {
    let (h, m) = text.split_once(':')?;
    Time::new(h.parse().ok()?, m.parse().ok()?, 0, 0).ok()
}

fn priority(word: &str) -> Option<Priority> {
    Priority::parse(&word.to_ascii_lowercase())
}

fn status(word: &str) -> Option<TaskStatus> {
    TaskStatus::parse(&word.to_ascii_lowercase())
}

fn project_status(word: &str) -> Option<ProjectStatus> {
    ProjectStatus::parse(&word.to_ascii_lowercase())
}

// ---- time ---------------------------------------------------------------

const TIME_COLUMNS: &[&str] = &[
    "date",
    "kind",
    "title",
    "start",
    "end",
    "minutes",
    "timezone",
    "all_day",
    "subject",
    "subject_id",
    "tags",
    "purpose",
    "notes",
    "id",
];

fn time_csv(blocks: &[TimeBlock]) -> String {
    let mut csv = Csv::new(TIME_COLUMNS);
    let mut rows: Vec<&TimeBlock> = blocks.iter().collect();
    rows.sort_by_key(|b| b.start);
    for block in rows {
        let (subject, subject_id) = match block.subject {
            BlockSubject::Task { id } => ("task", id.to_string()),
            BlockSubject::Project { id } => ("project", id.to_string()),
            BlockSubject::Adhoc => ("adhoc", String::new()),
        };
        csv.row(&[
            block.local_date.to_string(),
            block.kind.as_str().to_string(),
            block.title.clone(),
            block.start.to_string(),
            block.end.to_string(),
            ((block.end.as_second() - block.start.as_second()).max(0) / 60).to_string(),
            block.tz.clone(),
            block.all_day.to_string(),
            subject.to_string(),
            subject_id,
            block.tags.join(", "),
            doc::purpose_text(block.purpose.as_ref()),
            block.notes.clone(),
            block.id.to_string(),
        ]);
    }
    csv.finish()
}

/// The same hours, for a calendar to draw.
///
/// The title says which it was -- "Book the flights (planned)" -- because
/// there is no property for it that a calendar would show, and an hour you
/// meant to spend sitting on top of the hour you did spend, both unlabelled,
/// is worse than not exporting them at all.
fn time_ics(blocks: &[TimeBlock], tasks: &[Task], projects: &[Project]) -> String {
    let mut ics = everyday_core::ics::Ics::new("Every Day \u{2014} time");
    for block in blocks {
        let subject = match block.subject {
            BlockSubject::Task { id } => tasks.iter().find(|t| t.id == id).map(|t| t.title.clone()),
            BlockSubject::Project { id } => {
                projects.iter().find(|p| p.id == id).map(|p| p.name.clone())
            }
            BlockSubject::Adhoc => None,
        };
        let title = match (block.title.trim(), subject) {
            ("", Some(name)) => name,
            ("", None) => "Time".to_string(),
            (own, _) => own.to_string(),
        };
        ics.begin(&block.id.to_string(), block.updated_at)
            .line("SUMMARY", &format!("{title} ({})", block.kind.as_str()))
            .text("DESCRIPTION", &block.notes)
            .when(block.all_day, block.start, block.end, block.local_date, block.local_date)
            // Planned time is time you have set aside; recorded time is a
            // note about the past and should not make you look busy.
            .line(
                "TRANSP",
                match block.kind {
                    BlockKind::Planned => "OPAQUE",
                    BlockKind::Actual => "TRANSPARENT",
                },
            )
            .end();
    }
    ics.finish()
}

fn read_time(tasks: &dyn TaskStore, csv: &str, mode: Mode, report: &mut Report) {
    let table = Table::parse(csv);
    for row in table.rows() {
        let id = row.parse::<BlockId>("id").unwrap_or_else(BlockId::new);
        let existing = tasks.get_block(id).ok();
        // The order matters: a row already here and left alone by
        // `Mode::Skip` is never checked for a usable start and end, because
        // nothing about it is about to be read. Validating it first would
        // turn a row silently skipped today into one reported as broken.
        if existing.is_some() && mode == Mode::Skip {
            report.skipped += 1;
            continue;
        }
        let (Some(start), Some(end)) =
            (row.parse::<jiff::Timestamp>("start"), row.parse::<jiff::Timestamp>("end"))
        else {
            report.problem("time.csv", format!("a row has no usable start and end: {id}"));
            continue;
        };
        let subject = match (row.get("subject"), row.get("subject_id")) {
            ("task", id) => TaskId::parse(id).map(|id| BlockSubject::Task { id }).ok(),
            ("project", id) => ProjectId::parse(id).map(|id| BlockSubject::Project { id }).ok(),
            _ => None,
        }
        .unwrap_or(BlockSubject::Adhoc);

        let tz = match row.get("timezone") {
            "" => "UTC".to_string(),
            tz => tz.to_string(),
        };
        // `existing` can only be `None` or a record `mode` allows overwriting
        // here -- the one combination `land` would call `Skipped` was ruled
        // out above, before there was a start and end to build a fresh block
        // from.
        let mut landing = land(existing, || TimeBlock::new(subject, start, 0, &tz), mode);
        let Some(block) = landing.as_mut() else { continue };
        block.id = id;
        block.subject = subject;
        block.title = row.get("title").to_string();
        block.start = start;
        block.end = end;
        block.tz = tz;
        block.local_date = row
            .parse("date")
            .unwrap_or_else(|| everyday_core::model::local_date_in(start, &block.tz));
        block.all_day = row.flag("all_day");
        block.kind = match row.get("kind") {
            "actual" => BlockKind::Actual,
            _ => BlockKind::Planned,
        };
        block.notes = row.get("notes").to_string();
        block.tags = row.list("tags");
        block.purpose = doc::parse_purpose(row.get("purpose"));
        block.updated_at = jiff::Timestamp::now();

        match tasks.put_block(block) {
            Ok(()) => report.landed(&landing),
            Err(e) => report.problem("time.csv", e),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_task_line_round_trips_every_field_it_can_carry() {
        let mut task = Task::new("Book the flights");
        task.tags = vec!["travel".into()];
        task.priority = Priority::High;
        task.estimate_minutes = Some(90);
        task.due_date = Some(jiff::civil::date(2026, 9, 12));
        task.due_time = Time::new(16, 30, 0, 0).ok();
        task.start_date = Some(jiff::civil::date(2026, 9, 1));
        task.status = TaskStatus::Blocked;

        let mut out = String::new();
        render_task(&mut out, &task, &BTreeMap::new(), 0);
        assert!(out.contains("#travel"), "{out}");
        assert!(out.contains("!high"), "{out}");
        assert!(out.contains("~1h30"), "{out}");
        assert!(out.contains("@2026-09-12 16:30"), "{out}");
        assert!(out.contains("%blocked"), "{out}");

        let back = parse_tasks(&out);
        assert_eq!(back.len(), 1);
        assert_eq!(back[0].title, "Book the flights");
        assert_eq!(back[0].tags, ["travel"]);
        assert_eq!(back[0].priority, Priority::High);
        assert_eq!(back[0].estimate, Some(90));
        assert_eq!(back[0].due, task.due_date);
        assert_eq!(back[0].due_time, task.due_time);
        assert_eq!(back[0].start, task.start_date);
        assert_eq!(back[0].status, Some(TaskStatus::Blocked));
        assert_eq!(back[0].id, Some(task.id));
    }

    #[test]
    fn a_line_somebody_typed_becomes_a_new_task() {
        let parsed = parse_tasks("- [ ] Ring the plumber\n");
        assert_eq!(parsed.len(), 1);
        assert_eq!(parsed[0].title, "Ring the plumber");
        assert!(parsed[0].id.is_none(), "a hand-written line must not claim an id");
    }

    #[test]
    fn indentation_becomes_the_tree_whatever_its_width() {
        for indent in ["  ", "    ", "\t   "] {
            let text = format!("- [ ] Parent\n{indent}- [ ] Child\n");
            let parsed = parse_tasks(&text);
            assert_eq!(parsed.len(), 2, "{indent:?}");
            assert_eq!(parsed[1].depth, 1, "{indent:?}");
        }
    }

    #[test]
    fn the_lines_under_a_task_are_its_notes() {
        let parsed = parse_tasks("- [ ] Call the agent\n  Ask about the change fee.\n");
        assert_eq!(parsed[0].notes, "Ask about the change fee.");
        assert_eq!(parsed[0].title, "Call the agent");
    }

    #[test]
    fn a_marker_that_means_nothing_stays_in_the_title() {
        let parsed = parse_tasks("- [ ] Email @sarah about !urgent-ish ~stuff\n");
        assert_eq!(parsed[0].title, "Email @sarah about !urgent-ish ~stuff");
        assert_eq!(parsed[0].priority, Priority::None);
    }

    #[test]
    fn a_ticked_box_is_done_and_an_unticked_one_is_not() {
        let parsed = parse_tasks("- [x] Done\n- [ ] Not\n");
        assert!(parsed[0].done);
        assert!(!parsed[1].done);
    }

    #[test]
    fn durations_read_the_way_they_are_written() {
        for (minutes, text) in [(90u32, "1h30"), (45, "45m"), (120, "2h")] {
            assert_eq!(duration(minutes), text);
            assert_eq!(parse_duration(text), Some(minutes));
        }
    }

    #[test]
    fn the_project_description_stops_at_the_first_task() {
        let body = "# Home\n\nRip out the insulation.\n\n## Tasks\n\n- [ ] Ring the plasterer\n";
        assert_eq!(prose(body), "Rip out the insulation.");
        assert_eq!(heading(body).as_deref(), Some("Home"));
    }
}
