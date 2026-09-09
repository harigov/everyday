//! Why a thing is being done: the roles you play and the goals under them.
//!
//! Every other domain in this vault answers *what* — what you wrote, what
//! you have to do, what you spent an hour on, what is on the shelf. None of
//! them answers *why*, and without that "where did my week go" can only ever
//! be answered by project, which is a list of work and not a life.
//!
//! ```text
//!   Role ──── Goal ──── (anything: a project, a task, an hour, an entry)
//!  (parent)  (Viya rides without stabilisers)
//! ```
//!
//! # Two records, not one
//!
//! A [`Role`] is who you are being: parent, engineer, partner, yourself.
//! There are a handful, they rarely change, and they are the axis a balance
//! report is drawn against. A [`Goal`] is an outcome under a role with a
//! horizon: it has a status, it gets done or dropped, and there are as many
//! of them as you like. Folding the two together would mean either a
//! permanent goal or a role that finishes, and neither is a thing.
//!
//! # Neither of them is a project
//!
//! A [`Project`](crate::task::Project) is a body of work with tasks under
//! it. A goal is the reason work exists, and most goals have no project at
//! all — "read twelve books this year" is a shelf, "meditate daily" is a
//! tracker, "write more" is a journal. That is why goals are not a level
//! above projects in the todo app's tree: three quarters of them would never
//! reach it.
//!
//! # One pointer, on everything
//!
//! [`Purpose`] is what a record carries. It names a goal, or a role
//! directly, and the second case is not a fallback — a great deal of being a
//! parent serves no goal whatever and is still the thing you most want
//! counted. It is shaped like [`BlockSubject`](crate::task::BlockSubject) and
//! for the same reason: an enum with an id in it is one nullable pair of
//! columns, and a join rather than a table.
//!
//! It is `Option` everywhere it appears and is never required by anything.
//! An interface that demanded a purpose on capture would be an interface
//! people stop capturing into, which would cost the vault the very records
//! the reports are made of.
//!
//! # Inheritance, and where it happens
//!
//! A time block's purpose is its own if it has one, else its task's, else
//! that task's project's. Set it once on a project and everything under it
//! is attributed, which is what keeps the pointer from being a chore.
//! Resolution is done in SQL by
//! [`PurposeStore::time_by_purpose`](crate::store::purpose::PurposeStore::time_by_purpose)
//! over clear columns, so a year of blocks is a `GROUP BY` and not a
//! decryption; [`resolve`] is the same rule in Rust for callers holding the
//! records already.

use crate::id::{GoalId, RoleId};
use jiff::{Timestamp, civil::Date};
use serde::{Deserialize, Serialize};

/// Who you are being. A handful of these, changing about once a year.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Role {
    pub id: RoleId,
    pub name: String,
    /// `#rrggbb`. The axis colour of every balance chart, so it is identity
    /// rather than decoration — two roles the same colour makes the one
    /// report this domain exists for unreadable.
    pub color: String,
    /// A short emoji or glyph shown next to the name.
    pub icon: String,
    /// What being this means to you, in your own words. Plain text: it is
    /// read in a sidebar, not edited in an editor.
    #[serde(default)]
    pub notes: String,
    /// Retired: kept for its history, gone from the pickers.
    ///
    /// The alternative — deleting it — would take every goal under it, and
    /// with them the attribution of a year of hours. That is the wrong
    /// answer to "I changed jobs in March".
    #[serde(default)]
    pub archived: bool,
    /// Manual ordering in the sidebar; ties broken by `name`.
    #[serde(default)]
    pub sort_order: i32,
    pub created_at: Timestamp,
    pub updated_at: Timestamp,
}

impl Role {
    pub fn new(name: impl Into<String>) -> Self {
        let now = Timestamp::now();
        Self {
            id: RoleId::new(),
            name: name.into(),
            color: DEFAULT_ROLE_COLORS[0].to_string(),
            icon: "\u{1f9ed}".into(), // compass
            notes: String::new(),
            archived: false,
            sort_order: 0,
            created_at: now,
            updated_at: now,
        }
    }

    pub fn with_color(mut self, color: impl Into<String>) -> Self {
        self.color = color.into();
        self
    }

    pub fn with_icon(mut self, icon: impl Into<String>) -> Self {
        self.icon = icon.into();
        self
    }

    /// What this role points at, for a record that serves the role itself
    /// rather than any one goal under it.
    pub fn purpose(&self) -> Purpose {
        Purpose::Role { id: self.id }
    }

    /// Everything a text filter should look at.
    pub fn searchable_text(&self) -> String {
        let mut out = String::with_capacity(64);
        out.push_str(&self.name);
        out.push('\n');
        out.push_str(&self.notes);
        out
    }
}

/// Role accents. The journal palette, so one vault has one set of colours.
pub const DEFAULT_ROLE_COLORS: &[&str] = crate::model::DEFAULT_JOURNAL_COLORS;

/// Roles a vault starts with when somebody asks for a starting point.
///
/// Offered, never imposed: [`Vault::seed_roles`](crate::vault::Vault) writes
/// these only into a vault that has none, and the app asks first. A list of
/// what a life is made of is a personal document, and shipping one as a
/// default would be this application telling somebody who they are.
pub fn suggested_roles() -> Vec<Role> {
    let specs: [(&str, &str); 5] = [
        ("Work", "\u{1f4bc}"),    // briefcase
        ("Family", "\u{1f3e1}"),  // house with garden
        ("Health", "\u{1f331}"),  // seedling
        ("Friends", "\u{1f465}"), // busts in silhouette
        ("Myself", "\u{1f9ed}"),  // compass
    ];
    specs
        .into_iter()
        .enumerate()
        .map(|(i, (name, icon))| {
            let mut role = Role::new(name)
                .with_icon(icon)
                .with_color(DEFAULT_ROLE_COLORS[i % DEFAULT_ROLE_COLORS.len()]);
            role.sort_order = i as i32;
            role
        })
        .collect()
}

/// How a goal is going.
///
/// A closed set, and the same closed set for every role, which is the
/// argument [`TaskStatus`](crate::task::TaskStatus) and
/// [`ItemStatus`](crate::library::ItemStatus) both make: "how long do my
/// goals sit paused" has to be askable across a whole life at once, and
/// per-role vocabularies would make it unanswerable.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum GoalStatus {
    #[default]
    Active,
    /// Real, and deliberately not now. Distinct from dropped, because
    /// "later" and "never" are different answers.
    Paused,
    Done,
    /// Decided against. Kept rather than deleted, so the hours already spent
    /// against it still add up to something.
    Dropped,
}

impl GoalStatus {
    pub const ALL: [GoalStatus; 4] =
        [GoalStatus::Active, GoalStatus::Paused, GoalStatus::Done, GoalStatus::Dropped];

    /// Is this still something you are pursuing? `Paused` counts as open:
    /// it is on the books, it is just not this month.
    pub fn is_open(self) -> bool {
        matches!(self, GoalStatus::Active | GoalStatus::Paused)
    }

    /// Stable wire name, matching the serde representation.
    pub fn as_str(self) -> &'static str {
        match self {
            GoalStatus::Active => "active",
            GoalStatus::Paused => "paused",
            GoalStatus::Done => "done",
            GoalStatus::Dropped => "dropped",
        }
    }

    pub fn parse(s: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|k| k.as_str() == s)
    }
}

/// An outcome you want, under a role.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Goal {
    pub id: GoalId,
    /// Every goal belongs to exactly one role. Not optional: a goal with no
    /// role is a task, and this app already has those.
    pub role_id: RoleId,
    pub title: String,
    /// What "done" looks like, in your own words. Plain text, for the reason
    /// a task's notes are: it keeps the record cheap to make.
    #[serde(default)]
    pub notes: String,
    pub status: GoalStatus,
    /// When you would like this to be true by. Deliberately not called a due
    /// date and deliberately soft — nothing is ever *overdue* against it and
    /// no notification is raised. A goal is not a task and a horizon that
    /// nagged would turn it into one.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub horizon: Option<Date>,
    /// Manual ordering within its role; ties broken by `title`.
    #[serde(default)]
    pub sort_order: i32,
    pub created_at: Timestamp,
    pub updated_at: Timestamp,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub completed_at: Option<Timestamp>,
}

impl Goal {
    pub fn new(role_id: RoleId, title: impl Into<String>) -> Self {
        let now = Timestamp::now();
        Self {
            id: GoalId::new(),
            role_id,
            title: title.into(),
            notes: String::new(),
            status: GoalStatus::Active,
            horizon: None,
            sort_order: 0,
            created_at: now,
            updated_at: now,
            completed_at: None,
        }
    }

    pub fn with_horizon(mut self, horizon: Date) -> Self {
        self.horizon = Some(horizon);
        self
    }

    /// Move to `status`, keeping `completed_at` honest.
    ///
    /// Only [`Done`](GoalStatus::Done) stamps. Dropping a goal is not
    /// finishing it, and a "completed" date on something you gave up on
    /// would poison the one count anybody wants from this — how many of the
    /// things you set out to do you actually did.
    pub fn set_status(&mut self, status: GoalStatus) {
        self.status = status;
        self.completed_at = match status {
            GoalStatus::Done => self.completed_at.or_else(|| Some(Timestamp::now())),
            _ => None,
        };
        self.updated_at = Timestamp::now();
    }

    /// What this goal points at, for a record that serves it.
    pub fn purpose(&self) -> Purpose {
        Purpose::Goal { id: self.id }
    }

    /// Everything a text filter should look at.
    pub fn searchable_text(&self) -> String {
        let mut out = String::with_capacity(64);
        out.push_str(&self.title);
        out.push('\n');
        out.push_str(&self.notes);
        out
    }
}

/// What a record is *for*.
///
/// Pointing at a role directly is not a degraded case of pointing at a goal.
/// Reading to a child at bedtime serves being a parent and no particular
/// outcome, and an hour of it should count against that role rather than
/// against nothing at all.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "camelCase")]
pub enum Purpose {
    Goal { id: GoalId },
    Role { id: RoleId },
}

impl Purpose {
    pub fn goal_id(&self) -> Option<GoalId> {
        match self {
            Purpose::Goal { id } => Some(*id),
            Purpose::Role { .. } => None,
        }
    }

    /// The role this names *directly*. A goal's role is one join away and is
    /// deliberately not resolved here: this module has no store.
    pub fn role_id(&self) -> Option<RoleId> {
        match self {
            Purpose::Role { id } => Some(*id),
            Purpose::Goal { .. } => None,
        }
    }

    /// The discriminant, as the `purpose_kind` column spells it.
    pub fn kind_str(&self) -> &'static str {
        match self {
            Purpose::Goal { .. } => "goal",
            Purpose::Role { .. } => "role",
        }
    }

    /// The id, as the `purpose_id` column spells it.
    pub fn id_str(&self) -> String {
        match self {
            Purpose::Goal { id } => id.to_string(),
            Purpose::Role { id } => id.to_string(),
        }
    }

    /// Rebuild one from the pair of clear columns.
    ///
    /// Both are nullable and are only ever written together, so a half-set
    /// pair is a corrupt row rather than a state to model: it reads as no
    /// purpose, which is what an unattributed record already looks like.
    pub fn from_columns(kind: Option<&str>, id: Option<&str>) -> Option<Self> {
        match (kind?, id?) {
            ("goal", id) => GoalId::parse(id).ok().map(|id| Purpose::Goal { id }),
            ("role", id) => RoleId::parse(id).ok().map(|id| Purpose::Role { id }),
            _ => None,
        }
    }
}

/// The inheritance rule, for callers holding the records rather than a
/// database: a thing's own purpose, else its parent's, else its
/// grandparent's.
///
/// Written as a fold over an ordered list rather than as three arguments so
/// that a task (own, then project) and a block (own, then task, then
/// project) are the same call with a different number of terms.
pub fn resolve(chain: impl IntoIterator<Item = Option<Purpose>>) -> Option<Purpose> {
    chain.into_iter().flatten().next()
}

/// Minutes recorded against one purpose over a window.
///
/// `purpose` is `None` for the unattributed row, which is always present and
/// is the point of the report as much as any other row: most of a life is
/// not booked against anything, and a chart that quietly dropped that share
/// would be flattering rather than useful.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PurposeMinutes {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub purpose: Option<Purpose>,
    /// Total across [`BlockKind::Actual`](crate::task::BlockKind::Actual)
    /// blocks in the window.
    pub actual_minutes: u64,
    /// Total across [`BlockKind::Planned`](crate::task::BlockKind::Planned)
    /// blocks. Beside the actual rather than instead of it, because "meant
    /// to be a parent for five hours, was one for two" is the comparison the
    /// whole planned/actual split exists to make.
    pub planned_minutes: u64,
    /// Blocks counted, both kinds. A report that says nine hours over two
    /// blocks means something different from nine hours over thirty.
    pub blocks: u64,
}

impl PurposeMinutes {
    pub fn empty(purpose: Option<Purpose>) -> Self {
        Self { purpose, actual_minutes: 0, planned_minutes: 0, blocks: 0 }
    }
}

/// Minutes from a subscribed calendar, attributed by the calendar's role.
///
/// Kept apart from [`PurposeMinutes`] rather than summed into it. An event
/// is somebody else's claim on an hour and a block is your own record of
/// one; adding them would double-count every meeting you also logged, and
/// the two questions — "how much of my week did other people book" and "what
/// did I actually do" — are different questions.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RoleEventMinutes {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub role_id: Option<RoleId>,
    pub minutes: u64,
    pub events: u64,
}

/// What has happened against one goal.
///
/// Every field is a count or an instant, so the whole thing is answerable
/// from clear columns and nothing is decrypted to draw the list.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct GoalActivity {
    /// Tasks pointing here, or at a project pointing here, that are neither
    /// done nor cancelled.
    pub open_tasks: u64,
    pub done_tasks: u64,
    pub projects: u64,
    /// Minutes of [`BlockKind::Actual`](crate::task::BlockKind::Actual) over
    /// all time.
    pub actual_minutes: u64,
    /// Journal entries filed against it.
    pub entries: u64,
    /// Readings from trackers that measure it.
    pub readings: u64,
    /// Shelf items pointing here.
    pub items: u64,
    /// The most recent of everything above.
    ///
    /// What the Overview sorts by, because "the goal you have not touched
    /// since June" is the single most useful thing this domain can tell
    /// somebody, and it is the one thing no individual app can see.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last_touched: Option<Timestamp>,
}

impl GoalActivity {
    /// Fold in one candidate instant, keeping the latest.
    pub fn touch(&mut self, at: Option<Timestamp>) {
        if let Some(at) = at
            && self.last_touched.is_none_or(|held| at > held)
        {
            self.last_touched = Some(at);
        }
    }

    /// Has anything at all been recorded against this goal?
    pub fn is_empty(&self) -> bool {
        self.open_tasks == 0
            && self.done_tasks == 0
            && self.projects == 0
            && self.actual_minutes == 0
            && self.entries == 0
            && self.readings == 0
            && self.items == 0
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use jiff::civil::date;

    #[test]
    fn a_new_goal_is_active_and_unfinished() {
        let g = Goal::new(RoleId::new(), "learn to swim");
        assert!(g.status.is_open());
        assert_eq!(g.completed_at, None);
        assert_eq!(g.horizon, None, "a goal need not have a date to be a goal");
    }

    #[test]
    fn finishing_a_goal_stamps_it_and_dropping_one_does_not() {
        let mut g = Goal::new(RoleId::new(), "x");
        g.set_status(GoalStatus::Done);
        let stamped = g.completed_at.expect("a finished goal carries the moment");

        // Twice must not move the stamp: re-saving a finished goal should
        // not restate when it was finished.
        g.set_status(GoalStatus::Done);
        assert_eq!(g.completed_at, Some(stamped));

        // Giving up is not finishing, and must not leave a completion date
        // behind to be counted as one.
        g.set_status(GoalStatus::Dropped);
        assert_eq!(g.completed_at, None);
        assert!(!g.status.is_open());
    }

    #[test]
    fn a_paused_goal_is_still_open() {
        // "Later" and "never" are different answers, and only one of them
        // should drop off the list of things you are trying to do.
        assert!(GoalStatus::Paused.is_open());
        assert!(!GoalStatus::Dropped.is_open());
        assert!(!GoalStatus::Done.is_open());
    }

    #[test]
    fn a_purpose_survives_the_pair_of_columns() {
        let goal = Purpose::Goal { id: GoalId::new() };
        let role = Purpose::Role { id: RoleId::new() };
        for p in [goal, role] {
            let id = p.id_str();
            let back = Purpose::from_columns(Some(p.kind_str()), Some(&id));
            assert_eq!(back, Some(p));
        }
    }

    #[test]
    fn half_a_pair_of_columns_is_no_purpose_rather_than_a_panic() {
        let id = GoalId::new().to_string();
        assert_eq!(Purpose::from_columns(Some("goal"), None), None);
        assert_eq!(Purpose::from_columns(None, Some(&id)), None);
        // A kind written by a later build that this one does not know.
        assert_eq!(Purpose::from_columns(Some("project"), Some(&id)), None);
        // A pair that does not parse is unattributed, not an error: the
        // report has to draw either way.
        assert_eq!(Purpose::from_columns(Some("goal"), Some("not-a-uuid")), None);
    }

    #[test]
    fn a_goal_and_a_role_are_different_purposes_even_with_the_same_bytes() {
        // The two id types are distinct in Rust but the same 16 bytes on the
        // wire, so the discriminant is doing real work here.
        let uuid = uuid::Uuid::now_v7();
        let goal = Purpose::Goal { id: GoalId::from(uuid) };
        let role = Purpose::Role { id: RoleId::from(uuid) };
        assert_ne!(goal, role);
        assert_ne!(goal.kind_str(), role.kind_str());
        assert_eq!(goal.id_str(), role.id_str());
    }

    #[test]
    fn resolution_takes_the_first_purpose_in_the_chain() {
        let own = Purpose::Goal { id: GoalId::new() };
        let inherited = Purpose::Role { id: RoleId::new() };

        // Its own wins over what it would inherit.
        assert_eq!(resolve([Some(own), Some(inherited)]), Some(own));
        // With none of its own, the parent's is used.
        assert_eq!(resolve([None, Some(inherited)]), Some(inherited));
        // A block with nothing anywhere up the chain is unattributed, which
        // is a real answer and not a missing one.
        assert_eq!(resolve([None, None, None]), None);
    }

    #[test]
    fn a_goal_serialises_with_the_shape_the_interface_expects() {
        let mut g = Goal::new(RoleId::new(), "ride without stabilisers");
        g.horizon = Some(date(2027, 3, 1));
        let json = serde_json::to_value(&g).expect("a goal serialises");
        assert_eq!(json["title"], "ride without stabilisers");
        assert_eq!(json["roleId"], g.role_id.to_string());
        assert_eq!(json["status"], "active");
        assert_eq!(json["horizon"], "2027-03-01");
        assert!(json.get("completedAt").is_none(), "an unfinished goal omits the field");
    }

    #[test]
    fn a_purpose_serialises_as_a_tagged_object() {
        // The same shape as BlockSubject, so the interface has one idea of
        // what a pointer-with-a-kind looks like.
        let id = GoalId::new();
        let json = serde_json::to_value(Purpose::Goal { id }).expect("a purpose serialises");
        assert_eq!(json["type"], "goal");
        assert_eq!(json["id"], id.to_string());
    }

    #[test]
    fn suggested_roles_are_distinct_and_not_written_by_default() {
        let roles = suggested_roles();
        assert_eq!(roles.len(), 5);
        let mut names: Vec<&str> = roles.iter().map(|r| r.name.as_str()).collect();
        names.sort_unstable();
        names.dedup();
        assert_eq!(names.len(), 5, "a starting point should not offer the same role twice");
        let mut colors: Vec<&str> = roles.iter().map(|r| r.color.as_str()).collect();
        colors.sort_unstable();
        colors.dedup();
        assert_eq!(colors.len(), 5, "role colour is identity, so a default set cannot collide");
        for (i, role) in roles.iter().enumerate() {
            assert_eq!(role.sort_order, i as i32);
            assert!(!role.archived);
        }
    }

    #[test]
    fn activity_keeps_the_latest_instant_and_ignores_the_absent() {
        let mut a = GoalActivity::default();
        assert!(a.is_empty());
        assert_eq!(a.last_touched, None);

        let early = Timestamp::from_second(1_700_000_000).expect("a valid instant");
        let late = Timestamp::from_second(1_800_000_000).expect("a valid instant");

        a.touch(Some(late));
        a.touch(Some(early));
        a.touch(None);
        assert_eq!(a.last_touched, Some(late), "an older instant must not win");

        a.entries = 1;
        assert!(!a.is_empty());
    }
}
