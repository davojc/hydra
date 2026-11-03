//! The projects repo's files (spec §2), as types.

use serde::{Deserialize, Serialize};
use toml::value::Datetime;

/// `project.toml`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ProjectFile {
    pub name: String,
    /// The ticket prefix, e.g. `PG`.
    pub key: String,
    /// The lead's GitHub login.
    pub lead: String,
    #[serde(default)]
    pub members: Vec<String>,
    #[serde(default)]
    pub status: ProjectStatus,
    /// Code repos, `owner/name`: a hint for the checks.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub repos: Vec<String>,
    #[serde(default)]
    pub sync: Sync,
    /// Keys hydra doesn't know, kept on a rewrite.
    #[serde(flatten)]
    pub extra: toml::Table,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum ProjectStatus {
    #[default]
    Active,
    Paused,
    Done,
}

/// `[sync]` in `project.toml`: when the repo sync runs (spec §7).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Sync {
    /// Local times, `HH:MM`, once or twice a day.
    pub times: Vec<String>,
}

pub const DEFAULT_SYNC_TIMES: [&str; 2] = ["12:30", "17:30"];

impl Default for Sync {
    fn default() -> Self {
        Sync {
            times: DEFAULT_SYNC_TIMES.iter().map(|t| t.to_string()).collect(),
        }
    }
}

impl ProjectFile {
    /// Whether `login` is the lead or a member.
    pub fn is_on(&self, login: &str) -> bool {
        self.lead.eq_ignore_ascii_case(login)
            || self.members.iter().any(|m| m.eq_ignore_ascii_case(login))
    }
}

/// `milestones.toml`: `[[milestone]]` entries, in order.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct MilestonesFile {
    #[serde(default, rename = "milestone")]
    pub milestones: Vec<Milestone>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Milestone {
    pub id: String,
    pub title: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub target: Option<Datetime>,
    #[serde(default)]
    pub status: MilestoneStatus,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub description: String,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum MilestoneStatus {
    #[default]
    Planned,
    Active,
    Done,
}

/// A ticket's front matter. Its id is its file name (`tickets/PG-12.md`).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct TicketMeta {
    pub title: String,
    #[serde(default)]
    pub kind: TicketKind,
    #[serde(default)]
    pub status: TicketStatus,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub milestone: String,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub assignee: String,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub created_by: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub created: Option<Datetime>,
    #[serde(flatten)]
    pub extra: toml::Table,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum TicketKind {
    #[default]
    Feature,
    Bug,
    Chore,
}

#[derive(
    Debug, Clone, Copy, Default, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize,
)]
pub enum TicketStatus {
    #[default]
    #[serde(rename = "todo")]
    Todo,
    #[serde(rename = "in progress")]
    InProgress,
    #[serde(rename = "in review")]
    InReview,
    #[serde(rename = "done")]
    Done,
    #[serde(rename = "dropped")]
    Dropped,
}

impl TicketStatus {
    /// The board's columns, in order. Dropped tickets aren't on the board.
    pub const BOARD: [TicketStatus; 4] = [
        TicketStatus::Todo,
        TicketStatus::InProgress,
        TicketStatus::InReview,
        TicketStatus::Done,
    ];

    pub fn label(self) -> &'static str {
        match self {
            TicketStatus::Todo => "Todo",
            TicketStatus::InProgress => "In progress",
            TicketStatus::InReview => "In review",
            TicketStatus::Done => "Done",
            TicketStatus::Dropped => "Dropped",
        }
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct Ticket {
    /// `PG-12`.
    pub id: String,
    pub meta: TicketMeta,
    pub body: String,
}

/// A decision file's front matter: an open question, or a decision made once `answer` is set.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct DecisionMeta {
    pub question: String,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub asked_by: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub asked_at: Option<Datetime>,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub assigned_to: String,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub options: Vec<String>,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub ticket: String,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub milestone: String,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub answer: String,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub decided_by: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub decided_at: Option<Datetime>,
    /// The decision this one reverses, e.g. `D-7`.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub replaces: String,
    #[serde(flatten)]
    pub extra: toml::Table,
}

#[derive(Debug, Clone, PartialEq)]
pub struct Decision {
    /// `D-7`.
    pub id: String,
    pub meta: DecisionMeta,
    pub body: String,
}

impl Decision {
    pub fn is_open(&self) -> bool {
        self.meta.answer.trim().is_empty()
    }
}

/// A meeting note: `meetings/<date>-<slug>.md`, front matter optional.
#[derive(Debug, Clone, PartialEq)]
pub struct Meeting {
    /// The file name without `.md`.
    pub name: String,
    pub body: String,
}

/// `resources/links.toml`: `[[link]]` entries.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct LinksFile {
    #[serde(default, rename = "link")]
    pub links: Vec<Link>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Link {
    pub title: String,
    pub url: String,
}

/// One engineer's write-up for a day: `progress/<login>/<date>.md`.
#[derive(Debug, Clone, PartialEq)]
pub struct Progress {
    pub login: String,
    /// `YYYY-MM-DD`.
    pub date: String,
    pub body: String,
}

/// One project folder, read whole.
#[derive(Debug, Clone, PartialEq)]
pub struct Project {
    /// The folder name.
    pub slug: String,
    pub file: ProjectFile,
    pub spec: String,
    pub milestones: Vec<Milestone>,
    pub tickets: Vec<Ticket>,
    pub decisions: Vec<Decision>,
    pub meetings: Vec<Meeting>,
    /// File names under `resources/`, other than `links.toml`.
    pub files: Vec<String>,
    pub links: Vec<Link>,
    pub progress: Vec<Progress>,
}
