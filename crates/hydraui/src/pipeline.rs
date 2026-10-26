//! Pipelines for the orchestrator (spec: docs/superpowers/specs/2026-10-22-hydraui-orchestrator-core-design.md
//! §2): `<HYDRA_HOME>/hydraui/pipelines/<name>.toml`, its stages, their bars and prompts.

use std::path::{Path, PathBuf};

use serde::Deserialize;

/// A pipeline as `run_start` reads it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Pipeline {
    /// The name the views, notifications and Slack show (the design's orchestrator name).
    pub name: String,
    /// The file's name without `.toml`, as `load` was given it; empty from `parse` alone.
    pub file: String,
    /// Where a ticket's fix runs, as a pattern: `{ticket}` and `{ticket_lower}` are filled
    /// in, e.g. `https://fev2-solutions-{ticket_lower}.dev.iov42.net` (a branch deploy). Prompts
    /// use it as `{feature_env}`.
    pub feature_env: Option<String>,
    /// The branch every repository's work for a ticket must be on, as a pattern (`{ticket}`,
    /// `{ticket_lower}`), e.g. `{ticket}` → `INTH-166`; a stage's report is refused while a PR
    /// in it is on another branch.
    pub branch: Option<String>,
    /// The bar every stage must clear unless it sets its own, 0 to 100.
    pub bar: u8,
    pub stages: Vec<Stage>,
    /// Where its tickets come from in Linear (phase 2 spec §2); `None`: named by hand.
    pub linear: Option<LinearSettings>,
    /// The environment its orchestrator starts in when hydraui launches (`[autostart] env`);
    /// `None`: started from the launcher.
    pub autostart: Option<String>,
    /// The Slack channel a stage asks a product question in, rather than deciding it
    /// (`[product] channel`); `None`: it asks the user in hydraui.
    pub product_channel: Option<String>,
    /// Its accent on the card and the frame round its sessions (`color = "#rrggbb"`), so
    /// pipelines are told apart at a glance; `None`: the default teal.
    pub color: Option<u32>,
    /// Where its work comes from in Slack instead (`[slack]`): pull requests posted in a
    /// channel. A pipeline has one trigger, this or `linear`.
    pub slack: Option<SlackSettings>,
}

/// A pipeline's `[slack]` table: each message in `channel` with a GitHub pull request link and
/// no `reaction` yet gets that reaction, and each of its pull requests is run.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SlackSettings {
    /// The channel's id (`C0C217CER6G`), as Slack's API wants it.
    pub channel: String,
    /// How the channel shows on the card, e.g. `#pull-requests`.
    pub label: String,
    /// The reaction that marks a message taken, by name (`eyes` is 👀). A message that already
    /// has it, from anyone, is someone else's.
    pub reaction: String,
    pub every_minutes: u32,
    /// How far back a look goes, so switching on doesn't take the channel's whole history.
    pub lookback_hours: u32,
    /// Its orchestrator starts in Automatic.
    pub watch: bool,
}

/// A pipeline's `[linear]` table (phase 2 spec §2): which tickets it takes, and the states it
/// moves them through.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LinearSettings {
    /// The team's key, e.g. `INTH`.
    pub team: String,
    /// The state that triggers a pickup.
    pub pick: String,
    pub assignee: Assignee,
    /// The state a picked ticket moves to.
    pub claim: String,
    /// The state a ticket moves to when every stage passed.
    pub passed: String,
    pub every_minutes: u32,
    /// Its orchestrator starts in Automatic (the card's switch on).
    pub watch: bool,
}

/// Which tickets in the `pick` state a pipeline takes.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Assignee {
    /// Only tickets nobody is assigned (the default).
    Unassigned,
    /// Whoever they're assigned to.
    Any,
}

impl Assignee {
    pub fn word(self) -> &'static str {
        match self {
            Assignee::Unassigned => "unassigned",
            Assignee::Any => "any",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Stage {
    pub id: String,
    pub title: String,
    pub bar: Option<u8>,
    /// The skill the stage's session uses, shown in the views.
    pub skill: Option<String>,
    /// The folder its session starts in, as written (`~` allowed): a project whose skills and
    /// instructions it needs, such as the QA or development one. Takes the place of the
    /// orchestrator's folder and the run's worktree.
    pub folder: Option<String>,
    /// Runs in the run's worktree (made for the first such stage, §3.2).
    pub worktree: bool,
    /// The stage session's first prompt, before `render` fills it in: the file's, or one made
    /// from the skill and title when it has none (§2).
    pub prompt: String,
}

/// What a stage may do without asking when its pipeline has a product channel (core spec §15):
/// post its product question in Slack and read the replies, so asking product never waits on
/// the user's approval.
pub const PRODUCT_TOOLS: &str =
    "mcp__claude_ai_Slack__slack_send_message,mcp__claude_ai_Slack__slack_read_thread";

impl Pipeline {
    /// Whether its orchestrator starts in Automatic (`watch` in `[linear]` or `[slack]`).
    pub fn watches(&self) -> bool {
        self.linear.as_ref().is_some_and(|l| l.watch)
            || self.slack.as_ref().is_some_and(|s| s.watch)
    }

    /// Whether work reaches it by itself, from Linear or Slack.
    pub fn has_trigger(&self) -> bool {
        self.linear.is_some() || self.slack.is_some()
    }

    /// How a stage's Claude starts, before its own arguments: with [`PRODUCT_TOOLS`] allowed
    /// when the pipeline asks product in a channel.
    pub fn stage_claude(&self) -> Vec<String> {
        let mut argv = vec!["claude".to_string()];
        if self.product_channel.is_some() {
            // One word: the flag takes a list, and would take the prompt after it too.
            argv.push(format!("--allowedTools={PRODUCT_TOOLS}"));
        }
        argv
    }

    /// The branch for `ticket`; `None` when the pipeline names no branch rule.
    pub fn branch_for(&self, ticket: &str) -> Option<String> {
        self.branch.as_ref().map(|b| {
            b.replace("{ticket}", ticket)
                .replace("{ticket_lower}", &ticket.to_lowercase())
        })
    }

    /// `feature_env` for `ticket`; empty when the pipeline names none.
    pub fn feature_env_for(&self, ticket: &str) -> String {
        self.feature_env
            .as_deref()
            .map_or(String::new(), |pattern| {
                pattern
                    .replace("{ticket_lower}", &ticket.to_lowercase())
                    .replace("{ticket}", ticket)
            })
    }

    /// The bar stage `stage` must clear: its own, else the pipeline's.
    pub fn bar_for(&self, stage: usize) -> u8 {
        self.stages
            .get(stage)
            .and_then(|s| s.bar)
            .unwrap_or(self.bar)
    }
}

/// The bar when a pipeline names none (the user's decision).
pub const DEFAULT_BAR: u8 = 80;

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct File {
    name: String,
    bar: Option<u8>,
    feature_env: Option<String>,
    branch: Option<String>,
    #[serde(default, rename = "stage")]
    stages: Vec<FileStage>,
    linear: Option<FileLinear>,
    autostart: Option<FileAutostart>,
    product: Option<FileProduct>,
    slack: Option<FileSlack>,
    color: Option<String>,
}

/// The colours Pipeline settings offers, by name: distinct from each other and from the card's
/// state colours' meanings, and readable on the dark sidebar.
pub const COLORS: [(&str, u32); 8] = [
    ("Teal", 0x3fbfa6),
    ("Blue", 0x5b9cf0),
    ("Violet", 0x9b7cf0),
    ("Pink", 0xe27bb8),
    ("Orange", 0xe8935a),
    ("Yellow", 0xd8c35a),
    ("Green", 0x7cc46a),
    ("Slate", 0x8fa1b3),
];

/// `#rrggbb` as a number.
pub fn parse_color(text: &str) -> Result<u32, String> {
    let hex = text.trim().strip_prefix('#').unwrap_or("");
    if hex.len() != 6 {
        return Err(format!("color {text:?} must be #rrggbb, e.g. \"#5b9cf0\""));
    }
    u32::from_str_radix(hex, 16).map_err(|_| format!("color {text:?} must be #rrggbb"))
}

pub fn color_text(c: u32) -> String {
    format!("#{c:06x}")
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct FileSlack {
    channel: String,
    label: Option<String>,
    reaction: Option<String>,
    every_minutes: Option<u32>,
    lookback_hours: Option<u32>,
    watch: Option<bool>,
}

/// The `[slack]` defaults.
pub const DEFAULT_REACTION: &str = "eyes";
pub const DEFAULT_SLACK_EVERY_MINUTES: u32 = 2;
pub const DEFAULT_LOOKBACK_HOURS: u32 = 24;

/// A Slack channel id, e.g. `C0C217CER6G`.
pub fn is_channel_id(s: &str) -> bool {
    s.len() >= 9
        && s.starts_with(['C', 'G'])
        && s.chars()
            .all(|c| c.is_ascii_uppercase() || c.is_ascii_digit())
}

/// The `[slack]` table, its defaults filled in.
fn slack_settings(f: FileSlack) -> Result<SlackSettings, String> {
    let channel = f.channel.trim().to_string();
    // Names can be renamed and need another scope to look up: the id is what's stored.
    if !is_channel_id(&channel) {
        return Err(format!(
            "[slack] channel {channel:?} must be the channel's id, e.g. \"C0C217CER6G\" (in \
             Slack: the channel's name → About → Channel ID)"
        ));
    }
    let every_minutes = f.every_minutes.unwrap_or(DEFAULT_SLACK_EVERY_MINUTES);
    if every_minutes == 0 {
        return Err("[slack] every_minutes must be at least 1".into());
    }
    let reaction = f
        .reaction
        .map(|r| r.trim().trim_matches(':').to_string())
        .filter(|r| !r.is_empty())
        .unwrap_or_else(|| DEFAULT_REACTION.to_string());
    Ok(SlackSettings {
        label: f
            .label
            .map(|l| l.trim().to_string())
            .filter(|l| !l.is_empty())
            .unwrap_or_else(|| channel.clone()),
        channel,
        reaction,
        every_minutes,
        lookback_hours: f.lookback_hours.unwrap_or(DEFAULT_LOOKBACK_HOURS).max(1),
        watch: f.watch.unwrap_or(false),
    })
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct FileAutostart {
    env: String,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct FileProduct {
    channel: String,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct FileLinear {
    team: Option<String>,
    pick: Option<String>,
    assignee: Option<String>,
    claim: Option<String>,
    passed: Option<String>,
    every_minutes: Option<u32>,
    watch: Option<bool>,
}

/// The `[linear]` defaults (phase 2 spec §2).
pub const DEFAULT_PICK: &str = "Ready for Investigation";
pub const DEFAULT_CLAIM: &str = "In Investigation";
pub const DEFAULT_PASSED: &str = "Ready for Peer Review";
pub const DEFAULT_EVERY_MINUTES: u32 = 5;

/// The `[linear]` table, its defaults filled in (phase 2 spec §2).
fn linear_settings(f: FileLinear) -> Result<LinearSettings, String> {
    let text = |v: Option<String>, default: &str| {
        v.map(|s| s.trim().to_string())
            .filter(|s| !s.is_empty())
            .unwrap_or_else(|| default.to_string())
    };
    let team = text(f.team, "");
    if team.is_empty() {
        return Err("[linear] needs the team's key, e.g. team = \"INTH\"".into());
    }
    let assignee = match f.assignee.as_deref().map(str::trim) {
        None | Some("unassigned") => Assignee::Unassigned,
        Some("any") => Assignee::Any,
        Some(other) => {
            return Err(format!(
                "[linear] assignee {other:?} must be \"unassigned\" or \"any\""
            ));
        }
    };
    let every_minutes = f.every_minutes.unwrap_or(DEFAULT_EVERY_MINUTES);
    if every_minutes == 0 {
        return Err("[linear] every_minutes must be at least 1".into());
    }
    Ok(LinearSettings {
        team,
        pick: text(f.pick, DEFAULT_PICK),
        assignee,
        claim: text(f.claim, DEFAULT_CLAIM),
        passed: text(f.passed, DEFAULT_PASSED),
        every_minutes,
        watch: f.watch.unwrap_or(false),
    })
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct FileStage {
    id: String,
    title: String,
    bar: Option<u8>,
    skill: Option<String>,
    folder: Option<String>,
    #[serde(default)]
    worktree: bool,
    prompt: Option<String>,
}

/// The prompt of a stage that has none (§2): a stage added in Pipeline settings. Its skill comes
/// first anyway (`skill_line`).
fn default_prompt(title: &str) -> String {
    format!("Do the \"{title}\" stage of ticket {{ticket}}.")
}

fn plain_name(s: &str) -> bool {
    !s.is_empty()
        && s.bytes()
            .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'-')
}

fn check_bar(bar: Option<u8>, what: &str) -> Result<(), String> {
    match bar {
        Some(b) if b > 100 => Err(format!("{what}: bar {b} is over 100")),
        _ => Ok(()),
    }
}

/// Reads a pipeline file, naming the first mistake.
pub fn parse(text: &str) -> Result<Pipeline, String> {
    let file: File = toml::from_str(text).map_err(|e| e.message().to_string())?;
    check_bar(file.bar, "the pipeline")?;
    if file.linear.is_some() && file.slack.is_some() {
        return Err("a pipeline has one trigger: [linear] or [slack], not both".into());
    }
    if file.stages.is_empty() {
        return Err("the pipeline has no stages: add a [[stage]]".into());
    }
    let mut stages: Vec<Stage> = Vec::new();
    for s in file.stages {
        if !plain_name(&s.id) {
            return Err(format!(
                "stage id {:?} must be lowercase letters, digits and -",
                s.id
            ));
        }
        if stages.iter().any(|t| t.id == s.id) {
            return Err(format!("stage id {:?} is used twice", s.id));
        }
        check_bar(s.bar, &format!("stage {}", s.id))?;
        let skill = s
            .skill
            .map(|k| k.trim().to_string())
            .filter(|k| !k.is_empty());
        let prompt = s.prompt.unwrap_or_else(|| default_prompt(&s.title));
        let folder = s
            .folder
            .map(|f| f.trim().to_string())
            .filter(|f| !f.is_empty());
        stages.push(Stage {
            id: s.id,
            title: s.title,
            bar: s.bar,
            skill,
            folder,
            worktree: s.worktree,
            prompt,
        });
    }
    Ok(Pipeline {
        name: file.name,
        file: String::new(),
        feature_env: file
            .feature_env
            .map(|f| f.trim().to_string())
            .filter(|f| !f.is_empty()),
        branch: file
            .branch
            .map(|b| b.trim().to_string())
            .filter(|b| !b.is_empty()),
        bar: file.bar.unwrap_or(DEFAULT_BAR),
        stages,
        linear: file.linear.map(linear_settings).transpose()?,
        autostart: match file.autostart {
            Some(a) if a.env.trim().is_empty() => {
                return Err("[autostart] needs the environment, e.g. env = \"work\"".into());
            }
            Some(a) => Some(a.env.trim().to_string()),
            None => None,
        },
        product_channel: match file.product {
            Some(p) if p.channel.trim().is_empty() => {
                return Err(
                    "[product] needs the Slack channel, e.g. channel = \"#product\"".into(),
                );
            }
            Some(p) => Some(p.channel.trim().to_string()),
            None => None,
        },
        slack: file.slack.map(slack_settings).transpose()?,
        color: file.color.as_deref().map(parse_color).transpose()?,
    })
}

/// `<HYDRA_HOME>/hydraui/pipelines`.
pub fn folder(hydra_root: &Path) -> PathBuf {
    hydra_root.join("hydraui").join("pipelines")
}

/// A pipeline as Pipeline settings edits it (spec §7.4): what the view shows, nothing else.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Edit {
    pub name: String,
    pub bar: u8,
    pub feature_env: Option<String>,
    pub branch: Option<String>,
    pub stages: Vec<EditStage>,
    pub linear: Option<LinearSettings>,
    pub autostart: Option<String>,
    pub product_channel: Option<String>,
    pub color: Option<u32>,
    pub slack: Option<SlackSettings>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EditStage {
    /// The stage's id in the file; `None` for a stage added in the view.
    pub id: Option<String>,
    pub title: String,
    pub skill: Option<String>,
    /// The folder its session starts in; `None`: the orchestrator's folder or the worktree.
    pub folder: Option<String>,
    /// Its own bar; `None` follows the pipeline's.
    pub bar: Option<u8>,
    /// What its session is asked, before hydraui's note; empty: the default prompt.
    pub prompt: String,
}

impl From<&Pipeline> for Edit {
    fn from(p: &Pipeline) -> Self {
        Edit {
            name: p.name.clone(),
            bar: p.bar,
            feature_env: p.feature_env.clone(),
            branch: p.branch.clone(),
            stages: p
                .stages
                .iter()
                .map(|s| EditStage {
                    id: Some(s.id.clone()),
                    title: s.title.clone(),
                    skill: s.skill.clone(),
                    folder: s.folder.clone(),
                    bar: s.bar,
                    prompt: s.prompt.clone(),
                })
                .collect(),
            linear: p.linear.clone(),
            autostart: p.autostart.clone(),
            product_channel: p.product_channel.clone(),
            color: p.color,
            slack: p.slack.clone(),
        }
    }
}

/// What keeps `edit` from being saved, as the design's save bar says it.
pub fn validate(edit: &Edit) -> Result<(), &'static str> {
    if edit.name.trim().is_empty() {
        return Err("Give the orchestrator a name");
    }
    if edit.stages.is_empty() {
        return Err("Add at least one stage");
    }
    if edit.stages.iter().any(|s| s.title.trim().is_empty()) {
        return Err("Every stage needs a name");
    }
    if let Some(l) = &edit.linear {
        if l.team.trim().is_empty() {
            return Err("Name the Linear team, e.g. INTH");
        }
        if [&l.pick, &l.claim, &l.passed]
            .iter()
            .any(|s| s.trim().is_empty())
        {
            return Err("Name the Linear states");
        }
        if l.every_minutes == 0 {
            return Err("Check Linear every minute or more");
        }
    }
    if let Some(sl) = &edit.slack {
        if edit.linear.is_some() {
            return Err("Take work from Linear or Slack, not both");
        }
        if !is_channel_id(sl.channel.trim()) {
            return Err("Give the Slack channel's id, e.g. C0C217CER6G");
        }
        if sl.every_minutes == 0 {
            return Err("Check Slack every minute or more");
        }
        if sl.lookback_hours == 0 {
            return Err("Look back at least an hour in Slack");
        }
        if sl.reaction.trim().trim_matches(':').is_empty() {
            return Err("Name the reaction that marks a message taken, e.g. eyes");
        }
    }
    Ok(())
}

/// An id for a new stage titled `title`: its words in lowercase joined by `-`, numbered when
/// `taken` has it.
fn new_id(title: &str, taken: &[String]) -> String {
    let words: Vec<String> = title
        .split(|c: char| !c.is_ascii_alphanumeric())
        .filter(|w| !w.is_empty())
        .map(str::to_ascii_lowercase)
        .collect();
    let base = if words.is_empty() {
        "stage".to_string()
    } else {
        words.join("-")
    };
    if !taken.contains(&base) {
        return base;
    }
    (2..)
        .map(|n| format!("{base}-{n}"))
        .find(|id| !taken.contains(id))
        .expect("an unbounded range has a free id")
}

/// Writes `edit` into the pipeline file `<name>.toml`, keeping everything the view doesn't
/// show: comments, each kept stage's prompt and worktree, unknown keys stay refused by
/// `parse`. Stages are written in `edit`'s order; one it dropped goes. Written whole, then
/// moved into place, so a reader never sees half a file.
pub fn save(hydra_root: &Path, name: &str, edit: &Edit) -> std::io::Result<()> {
    use toml_edit::{ArrayOfTables, DocumentMut, Item, Table, value};
    if !plain_name(name) {
        return Err(std::io::Error::other(format!(
            "{name:?} isn't a pipeline name"
        )));
    }
    let file = folder(hydra_root).join(format!("{name}.toml"));
    let text = match std::fs::read_to_string(&file) {
        Ok(text) => text,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => String::new(),
        Err(e) => return Err(e),
    };
    let mut doc: DocumentMut = text.parse().map_err(std::io::Error::other)?;
    doc["name"] = value(edit.name.trim());
    doc["bar"] = value(i64::from(edit.bar));
    match edit
        .feature_env
        .as_deref()
        .map(str::trim)
        .filter(|f| !f.is_empty())
    {
        Some(f) => doc["feature_env"] = value(f),
        None => {
            doc.remove("feature_env");
        }
    }
    match edit
        .branch
        .as_deref()
        .map(str::trim)
        .filter(|b| !b.is_empty())
    {
        Some(b) => doc["branch"] = value(b),
        None => {
            doc.remove("branch");
        }
    }
    match &edit.linear {
        Some(l) => {
            // Into the table that's there, so its comments stay.
            if !doc.get("linear").is_some_and(Item::is_table) {
                doc["linear"] = Item::Table(Table::new());
            }
            let t = &mut doc["linear"];
            t["team"] = value(l.team.trim());
            t["pick"] = value(l.pick.trim());
            t["assignee"] = value(l.assignee.word());
            t["claim"] = value(l.claim.trim());
            t["passed"] = value(l.passed.trim());
            t["every_minutes"] = value(i64::from(l.every_minutes));
            if l.watch {
                t["watch"] = value(true);
            } else if let Some(t) = t.as_table_mut() {
                t.remove("watch");
            }
        }
        None => {
            doc.remove("linear");
        }
    }
    match edit
        .autostart
        .as_deref()
        .map(str::trim)
        .filter(|e| !e.is_empty())
    {
        Some(env) => {
            if !doc.get("autostart").is_some_and(Item::is_table) {
                doc["autostart"] = Item::Table(Table::new());
            }
            doc["autostart"]["env"] = value(env);
        }
        None => {
            doc.remove("autostart");
        }
    }
    match edit
        .product_channel
        .as_deref()
        .map(str::trim)
        .filter(|c| !c.is_empty())
    {
        Some(channel) => {
            if !doc.get("product").is_some_and(Item::is_table) {
                doc["product"] = Item::Table(Table::new());
            }
            doc["product"]["channel"] = value(channel);
        }
        None => {
            doc.remove("product");
        }
    }
    match &edit.slack {
        Some(sl) => {
            // Into the table that's there, so its comments stay.
            if !doc.get("slack").is_some_and(Item::is_table) {
                doc["slack"] = Item::Table(Table::new());
            }
            let t = &mut doc["slack"];
            t["channel"] = value(sl.channel.trim());
            let label = sl.label.trim();
            if label.is_empty() || label == sl.channel.trim() {
                if let Some(t) = t.as_table_mut() {
                    t.remove("label");
                }
            } else {
                t["label"] = value(label);
            }
            t["reaction"] = value(sl.reaction.trim().trim_matches(':'));
            t["every_minutes"] = value(i64::from(sl.every_minutes));
            t["lookback_hours"] = value(i64::from(sl.lookback_hours));
            t["watch"] = value(sl.watch);
        }
        None => {
            doc.remove("slack");
        }
    }
    match edit.color {
        Some(c) => doc["color"] = value(color_text(c)),
        None => {
            doc.remove("color");
        }
    }
    let old: Vec<Table> = doc
        .get("stage")
        .and_then(Item::as_array_of_tables)
        .map(|a| a.iter().cloned().collect())
        .unwrap_or_default();
    let mut taken: Vec<String> = edit.stages.iter().filter_map(|s| s.id.clone()).collect();
    let mut stages = ArrayOfTables::new();
    for s in &edit.stages {
        let (mut table, id) = match &s.id {
            Some(id) => (
                old.iter()
                    .find(|t| t.get("id").and_then(Item::as_str) == Some(id.as_str()))
                    .cloned()
                    .unwrap_or_default(),
                id.clone(),
            ),
            None => {
                let id = new_id(&s.title, &taken);
                taken.push(id.clone());
                (Table::new(), id)
            }
        };
        table["id"] = value(id);
        table["title"] = value(s.title.trim());
        match &s.skill {
            Some(skill) => table["skill"] = value(skill.as_str()),
            None => {
                table.remove("skill");
            }
        }
        match &s.folder {
            Some(folder) => table["folder"] = value(folder.as_str()),
            None => {
                table.remove("folder");
            }
        }
        match s.bar {
            Some(bar) => table["bar"] = value(i64::from(bar)),
            None => {
                table.remove("bar");
            }
        }
        // An emptied prompt goes back to the default one (§2).
        match s.prompt.trim() {
            "" => {
                table.remove("prompt");
            }
            _ => table["prompt"] = value(s.prompt.trim_end()),
        }
        // Parsed tables are written by their place in the old file: number them afresh so
        // the file follows `edit`'s order.
        table.set_position(stages.len() + 1_000);
        stages.push(table);
    }
    doc["stage"] = Item::ArrayOfTables(stages);
    std::fs::create_dir_all(folder(hydra_root))?;
    let fresh = file.with_extension("toml.saving");
    std::fs::write(&fresh, doc.to_string())?;
    std::fs::rename(&fresh, &file)
}

/// The pipelines in `folder`, by file name without `.toml`, sorted.
pub fn names(hydra_root: &Path) -> Vec<String> {
    let mut names: Vec<String> = std::fs::read_dir(folder(hydra_root))
        .into_iter()
        .flatten()
        .flatten()
        .filter_map(|e| {
            let path = e.path();
            (path.extension()? == "toml").then(|| path.file_stem()?.to_str().map(str::to_string))?
        })
        .filter(|n| plain_name(n))
        .collect();
    names.sort();
    names
}

/// The pipeline `<name>.toml` in `folder`.
pub fn load(hydra_root: &Path, name: &str) -> Result<Pipeline, String> {
    if !plain_name(name) {
        return Err(format!(
            "{name:?} isn't a pipeline name: lowercase letters, digits and -"
        ));
    }
    let file = folder(hydra_root).join(format!("{name}.toml"));
    let text = std::fs::read_to_string(&file)
        .map_err(|e| format!("can't read pipeline {name} ({}): {e}", file.display()))?;
    let mut pipeline = parse(&text).map_err(|e| format!("pipeline {name}: {e}"))?;
    pipeline.file = name.to_string();
    Ok(pipeline)
}

/// Writes `ticket.toml` when the folder holds no pipeline yet (§2).
pub fn ensure_example(hydra_root: &Path) -> std::io::Result<()> {
    let dir = folder(hydra_root);
    let has_one = std::fs::read_dir(&dir).is_ok_and(|entries| {
        entries
            .flatten()
            .any(|e| e.path().extension().is_some_and(|x| x == "toml"))
    });
    if has_one {
        return Ok(());
    }
    std::fs::create_dir_all(&dir)?;
    std::fs::write(dir.join("ticket.toml"), EXAMPLE)
}

/// The file name for a pipeline called `title`: lowercase letters and digits, words joined by
/// `-` (`INTH SLA tickets` → `inth-sla-tickets`); empty when nothing is left.
pub fn file_name_for(title: &str) -> String {
    let mut out = String::new();
    for c in title.trim().chars() {
        if c.is_ascii_alphanumeric() {
            out.push(c.to_ascii_lowercase());
        } else if !out.is_empty() && !out.ends_with('-') {
            out.push('-');
        }
    }
    out.trim_end_matches('-').to_string()
}

/// A new pipeline's file when it starts blank: one stage to rename and fill in.
const BLANK: &str = r#"bar = 80

[[stage]]
id = "first"
title = "First stage"
prompt = "Work on ticket {ticket}."
"#;

/// **New pipeline…**: writes a pipeline called `title`, blank or a copy of pipeline `from`,
/// and returns its file name. A copy doesn't watch Linear or start at launch until those are
/// turned on in its settings, so it never takes tickets its original takes.
pub fn create(hydra_root: &Path, title: &str, from: Option<&str>) -> Result<String, String> {
    use toml_edit::{DocumentMut, Item, value};
    let title = title.trim();
    let name = file_name_for(title);
    if name.is_empty() {
        return Err("give the pipeline a name with a letter or digit in it".into());
    }
    let dir = folder(hydra_root);
    let file = dir.join(format!("{name}.toml"));
    if file.exists() {
        return Err(format!("there's already a pipeline file {name}.toml"));
    }
    let text = match from {
        None => BLANK.to_string(),
        Some(src) => {
            if !plain_name(src) {
                return Err(format!("{src:?} isn't a pipeline name"));
            }
            let path = dir.join(format!("{src}.toml"));
            std::fs::read_to_string(&path).map_err(|e| format!("can't read pipeline {src}: {e}"))?
        }
    };
    let mut doc: DocumentMut = text
        .parse()
        .map_err(|e| format!("pipeline {from:?}: {e}"))?;
    doc["name"] = value(title);
    doc.remove("autostart");
    for trigger in ["linear", "slack"] {
        if let Some(t) = doc.get_mut(trigger).and_then(Item::as_table_mut) {
            t["watch"] = value(false);
        }
    }
    let text = doc.to_string();
    parse(&text).map_err(|e| format!("the new pipeline: {e}"))?;
    std::fs::create_dir_all(&dir).map_err(|e| e.to_string())?;
    std::fs::write(&file, text).map_err(|e| format!("can't write {}: {e}", file.display()))?;
    Ok(name)
}

/// Values a stage prompt's placeholders are filled with.
pub struct PromptVars<'a> {
    pub ticket: &'a str,
    pub run: &'a str,
    pub stage: &'a str,
    /// The previous stage's handoff (§4); empty for the first stage.
    pub handoff: &'a str,
    /// The pipeline's `feature_env` for the ticket; empty without one.
    pub feature_env: &'a str,
    /// What the run is about, as a link (a Slack-triggered run's pull request); `{pr}` and
    /// `{link}` in a prompt. Empty without one.
    pub link: &'a str,
}

/// The stage session's first prompt as the pipeline writes it: its placeholders filled, then
/// the orchestrator's `brief` when it gave one. hydraui adds a short note of its own after it
/// (`runs`).
pub fn render(prompt: &str, vars: &PromptVars, brief: &str) -> String {
    let mut text = prompt
        .trim_end()
        .replace("{ticket_lower}", &vars.ticket.to_lowercase())
        .replace("{ticket}", vars.ticket)
        .replace("{run}", vars.run)
        .replace("{stage}", vars.stage)
        .replace("{handoff}", vars.handoff)
        .replace("{feature_env}", vars.feature_env)
        .replace("{pr}", vars.link)
        .replace("{link}", vars.link);
    let brief = brief.trim();
    if !brief.is_empty() {
        text.push_str("\n\n");
        text.push_str(brief);
    }
    text
}

/// The pipeline written when there is none: the four stages of the ticket pipeline with plain
/// prompts, enough to walk a ticket through by hand (phase 1's gate).
pub const EXAMPLE: &str = r#"# A ticket through four stages. Each stage is a fresh Claude session; it ends with
# stage_report, and the next stage starts only when the rating clears the bar. A stage may
# name the skill its session uses (skill = "..."); a stage with no prompt gets one from it.
name = "ticket"
bar = 80

[[stage]]
id = "replicate"
title = "Understand and replicate"
prompt = """
Read ticket {ticket}. Decide whether it has enough information to act on. If it does, reproduce
the problem it describes and record how.
"""

[[stage]]
id = "develop"
title = "Develop"
worktree = true
prompt = """
Fix ticket {ticket} in this worktree, test first. What the previous stage found: {handoff}
Commit the fix when its tests pass.
"""

[[stage]]
id = "review"
title = "Review"
worktree = true
prompt = """
Open a pull request for the fix to ticket {ticket} from this worktree's branch, and see it through
review until it is approved. What the previous stage handed over: {handoff}
"""

[[stage]]
id = "verify"
title = "Verify"
worktree = true
prompt = """
Prove that ticket {ticket} is fixed: repeat the reproduction and show it no longer fails.
What the previous stage handed over: {handoff}
"""
"#;

#[cfg(test)]
mod tests {
    #[test]
    fn settings_save_a_slack_trigger_and_refuse_two() {
        let d = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(folder(d.path())).unwrap();
        std::fs::write(
            folder(d.path()).join("prs.toml"),
            "name = \"PRs\"\n[[stage]]\nid = \"a\"\ntitle = \"A\"\n\
             # where PRs come from\n[slack]\nchannel = \"C0C217CER6G\"\nlabel = \"#pull-requests\"\nwatch = true\n",
        )
        .unwrap();
        let mut edit = Edit::from(&load(d.path(), "prs").unwrap());
        let sl = edit.slack.as_mut().unwrap();
        sl.every_minutes = 7;
        sl.lookback_hours = 48;
        sl.reaction = ":robot_face:".into();
        assert_eq!(validate(&edit), Ok(()));
        save(d.path(), "prs", &edit).unwrap();
        let text = std::fs::read_to_string(folder(d.path()).join("prs.toml")).unwrap();
        assert!(
            text.contains("# where PRs come from"),
            "comments kept: {text}"
        );
        let back = load(d.path(), "prs").unwrap().slack.unwrap();
        assert_eq!(
            (
                back.every_minutes,
                back.lookback_hours,
                back.reaction.as_str(),
                back.watch
            ),
            (7, 48, "robot_face", true)
        );
        assert_eq!(back.label, "#pull-requests");

        let mut both = edit.clone();
        both.linear = Some(LinearSettings {
            team: "INTH".into(),
            pick: DEFAULT_PICK.into(),
            assignee: Assignee::Unassigned,
            claim: DEFAULT_CLAIM.into(),
            passed: DEFAULT_PASSED.into(),
            every_minutes: 5,
            watch: false,
        });
        assert_eq!(
            validate(&both),
            Err("Take work from Linear or Slack, not both")
        );
        let mut bad = edit.clone();
        bad.slack.as_mut().unwrap().channel = "#pull-requests".into();
        assert!(validate(&bad).unwrap_err().contains("channel's id"));
        let mut zero = edit.clone();
        zero.slack.as_mut().unwrap().every_minutes = 0;
        assert!(validate(&zero).is_err());

        edit.slack = None;
        save(d.path(), "prs", &edit).unwrap();
        assert_eq!(load(d.path(), "prs").unwrap().slack, None);
    }

    #[test]
    fn a_pipeline_colour_is_read_saved_and_cleared() {
        let base = "name = \"PRs\"\n[[stage]]\nid = \"a\"\ntitle = \"A\"\n";
        assert_eq!(parse(base).unwrap().color, None);
        let p = parse(&format!("color = \"#5B9CF0\"\n{base}")).unwrap();
        assert_eq!(p.color, Some(0x5b9cf0));
        let bad = parse(&format!("color = \"blue\"\n{base}")).unwrap_err();
        assert!(bad.contains("#rrggbb"), "{bad}");

        let d = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(folder(d.path())).unwrap();
        std::fs::write(folder(d.path()).join("prs.toml"), format!("# mine\n{base}")).unwrap();
        let mut edit = Edit::from(&load(d.path(), "prs").unwrap());
        edit.color = Some(COLORS[2].1);
        save(d.path(), "prs", &edit).unwrap();
        let text = std::fs::read_to_string(folder(d.path()).join("prs.toml")).unwrap();
        assert!(text.contains("color = \"#9b7cf0\""), "{text}");
        assert_eq!(load(d.path(), "prs").unwrap().color, Some(0x9b7cf0));
        edit.color = None;
        save(d.path(), "prs", &edit).unwrap();
        assert_eq!(load(d.path(), "prs").unwrap().color, None);
    }

    #[test]
    fn a_slack_trigger_is_read_with_its_defaults() {
        let base =
            "name = \"PRs\"\n[[stage]]\nid = \"a\"\ntitle = \"A\"\nprompt = \"review {pr}\"\n";
        let p = parse(&format!(
            "{base}[slack]\nchannel = \"C0C217CER6G\"\nlabel = \"#pull-requests\"\n"
        ))
        .unwrap();
        let s = p.slack.unwrap();
        assert_eq!(
            (s.channel.as_str(), s.label.as_str(), s.reaction.as_str()),
            ("C0C217CER6G", "#pull-requests", "eyes")
        );
        assert_eq!((s.every_minutes, s.lookback_hours, s.watch), (2, 24, false));
        let named = parse(&format!("{base}[slack]\nchannel = \"#pull-requests\"\n")).unwrap_err();
        assert!(named.contains("channel's id"), "{named}");
        let both = parse(&format!(
            "{base}[slack]\nchannel = \"C0C217CER6G\"\n[linear]\nteam = \"INTH\"\n"
        ))
        .unwrap_err();
        assert!(both.contains("one trigger"), "{both}");
        let colons = parse(&format!(
            "{base}[slack]\nchannel = \"C0C217CER6G\"\nreaction = \":eyes:\"\n"
        ))
        .unwrap();
        assert_eq!(colons.slack.unwrap().reaction, "eyes");
    }

    #[test]
    fn a_pipelines_file_name_comes_from_its_title() {
        assert_eq!(file_name_for("INTH SLA tickets"), "inth-sla-tickets");
        assert_eq!(
            file_name_for("  Release notes — weekly! "),
            "release-notes-weekly"
        );
        assert_eq!(file_name_for("PRE/1"), "pre-1");
        assert_eq!(file_name_for("—!"), "");
    }

    #[test]
    fn a_new_pipeline_starts_blank_with_one_stage() {
        let d = tempfile::tempdir().unwrap();
        let name = create(d.path(), "Release \"notes\"", None).unwrap();
        assert_eq!(name, "release-notes");
        let p = load(d.path(), &name).unwrap();
        assert_eq!(p.name, "Release \"notes\"");
        assert_eq!(p.stages.len(), 1);
        assert_eq!(p.linear, None);
        let again = create(d.path(), "release notes", None).unwrap_err();
        assert!(again.contains("already"), "{again}");
        assert!(create(d.path(), "!!", None).is_err());
    }

    #[test]
    fn a_copy_keeps_its_stages_but_takes_no_tickets_until_turned_on() {
        let d = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(folder(d.path())).unwrap();
        std::fs::write(
            folder(d.path()).join("inth.toml"),
            "# INTH tickets\nname = \"INTH\"\nbar = 85\n[linear]\nteam = \"INTH\"\nwatch = true\n\
             [autostart]\nenv = \"iov42\"\n[[stage]]\nid = \"a\"\ntitle = \"A\"\nprompt = \"do {ticket}\"\n\
             [[stage]]\nid = \"b\"\ntitle = \"B\"\nprompt = \"check\"\n",
        )
        .unwrap();
        let name = create(d.path(), "PRE bugs", Some("inth")).unwrap();
        let p = load(d.path(), &name).unwrap();
        assert_eq!((p.name.as_str(), p.bar), ("PRE bugs", 85));
        assert_eq!(p.stages.len(), 2);
        assert_eq!(p.autostart, None);
        assert!(!p.linear.unwrap().watch);
        let text = std::fs::read_to_string(folder(d.path()).join("pre-bugs.toml")).unwrap();
        assert!(text.starts_with("# INTH tickets"), "comments kept: {text}");
        assert!(
            load(d.path(), "inth").unwrap().linear.unwrap().watch,
            "the original unchanged"
        );
        assert!(create(d.path(), "x", Some("nope")).is_err());
    }

    use super::*;

    const TWO: &str = "name = \"t\"\n[[stage]]\nid = \"replicate\"\ntitle = \"Replicate\"\nprompt = \"Read {ticket} ({run}, {stage}). {handoff}\"\n[[stage]]\nid = \"develop\"\ntitle = \"Develop\"\nbar = 85\nworktree = true\nprompt = \"Fix it.\"\n";

    #[test]
    fn bars_default_to_80_and_a_stage_can_set_its_own() {
        let p = parse(TWO).unwrap();
        assert_eq!((p.bar, p.bar_for(0), p.bar_for(1)), (80, 80, 85));
        assert!(p.stages[1].worktree && !p.stages[0].worktree);
        assert_eq!(p.stages[0].title, "Replicate");
    }

    #[test]
    fn mistakes_are_named() {
        for (text, says) in [
            ("name = \"t\"\n", "no stages"),
            (
                "name = \"t\"\nbar = 101\n[[stage]]\nid = \"a\"\ntitle = \"A\"\nprompt = \"p\"\n",
                "bar",
            ),
            (
                "name = \"t\"\n[[stage]]\nid = \"a\"\ntitle = \"A\"\nbar = 120\nprompt = \"p\"\n",
                "bar",
            ),
            (
                "name = \"t\"\n[[stage]]\nid = \"A b\"\ntitle = \"A\"\nprompt = \"p\"\n",
                "A b",
            ),
            (
                "name = \"t\"\n[[stage]]\nid = \"a\"\ntitle = \"A\"\nprompt = \"p\"\n[[stage]]\nid = \"a\"\ntitle = \"B\"\nprompt = \"q\"\n",
                "twice",
            ),
            (
                "name = \"t\"\nspeed = 2\n[[stage]]\nid = \"a\"\ntitle = \"A\"\nprompt = \"p\"\n",
                "speed",
            ),
            (
                "name = \"t\"\n[[stage]]\nid = \"a\"\nprompt = \"p\"\n",
                "title",
            ),
        ] {
            let e = parse(text).unwrap_err();
            assert!(e.contains(says), "{says}: {e}");
        }
    }

    #[test]
    fn a_prompt_gets_its_values_and_the_brief() {
        let p = parse(TWO).unwrap();
        let vars = PromptVars {
            ticket: "INTH-1",
            run: "INTH-1-1",
            stage: "replicate",
            handoff: "see log",
            feature_env: "",
            link: "",
        };
        let text = render(&p.stages[0].prompt, &vars, "Focus on the API.");
        assert_eq!(
            text,
            "Read INTH-1 (INTH-1-1, replicate). see log\n\nFocus on the API."
        );
        assert_eq!(render("Go.", &vars, "  "), "Go.");
        let pr = PromptVars {
            link: "https://github.com/iov42/svc-usage/pull/62",
            ..vars
        };
        assert_eq!(
            render("review {pr}", &pr, ""),
            "review https://github.com/iov42/svc-usage/pull/62"
        );
    }

    #[test]
    fn the_feature_env_follows_the_ticket() {
        let d = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(folder(d.path())).unwrap();
        std::fs::write(
            folder(d.path()).join("t.toml"),
            "name = \"t\"\nfeature_env = \"https://fev2-solutions-{ticket_lower}.dev.iov42.net\"\n\
             [[stage]]\nid = \"verify\"\ntitle = \"Verify\"\nprompt = \"test {ticket} against {feature_env}\"\n",
        )
        .unwrap();
        let p = load(d.path(), "t").unwrap();
        let env = p.feature_env_for("INTH-161");
        assert_eq!(env, "https://fev2-solutions-inth-161.dev.iov42.net");
        let vars = PromptVars {
            ticket: "INTH-161",
            run: "INTH-161-1",
            stage: "verify",
            handoff: "",
            feature_env: &env,
            link: "",
        };
        assert_eq!(
            render(&p.stages[0].prompt, &vars, ""),
            "test INTH-161 against https://fev2-solutions-inth-161.dev.iov42.net"
        );
        // Settings keep it, change it and clear it.
        let mut edit = Edit::from(&p);
        assert_eq!(
            edit.feature_env.as_deref(),
            Some("https://fev2-solutions-{ticket_lower}.dev.iov42.net")
        );
        edit.feature_env = None;
        save(d.path(), "t", &edit).unwrap();
        assert_eq!(load(d.path(), "t").unwrap().feature_env_for("INTH-161"), "");
    }

    #[test]
    fn the_example_parses_and_is_written_once() {
        assert_eq!(parse(EXAMPLE).unwrap().stages.len(), 4);
        let d = tempfile::tempdir().unwrap();
        ensure_example(d.path()).unwrap();
        let file = folder(d.path()).join("ticket.toml");
        let loaded = load(d.path(), "ticket").unwrap();
        assert_eq!(
            (loaded.name.as_str(), loaded.file.as_str()),
            ("ticket", "ticket")
        );
        std::fs::write(&file, "edited").unwrap();
        ensure_example(d.path()).unwrap();
        assert_eq!(std::fs::read_to_string(&file).unwrap(), "edited");
    }

    #[test]
    fn a_stage_without_a_prompt_gets_one_from_its_skill() {
        let p = parse(
            "name = \"t\"\n[[stage]]\nid = \"a\"\ntitle = \"Replicate\"\nskill = \"QA-Skills\"\n\
             [[stage]]\nid = \"b\"\ntitle = \"Check\"\n",
        )
        .unwrap();
        assert_eq!(p.stages[0].skill.as_deref(), Some("QA-Skills"));
        assert_eq!(
            p.stages[0].prompt,
            "Do the \"Replicate\" stage of ticket {ticket}."
        );
        assert_eq!(p.stages[1].skill, None);
        assert_eq!(
            p.stages[1].prompt,
            "Do the \"Check\" stage of ticket {ticket}."
        );
        assert_eq!(p.file, "");
    }

    const COMMENTED: &str = "# The team's pipeline.\nname = \"Tickets\"\nbar = 80\n\n\
# Reproduce first.\n[[stage]]\nid = \"replicate\"\ntitle = \"Replicate\"\nworktree = false\n\
prompt = \"\"\"\nRead {ticket}.\n\"\"\"\n\n[[stage]]\nid = \"develop\"\ntitle = \"Develop\"\n\
bar = 85\nworktree = true\nprompt = \"Fix it.\"\n";

    fn edit_of(p: &Pipeline) -> Edit {
        Edit::from(p)
    }

    const ONE_STAGE: &str = "name = \"t\"\n[[stage]]\nid = \"a\"\ntitle = \"A\"\n";

    #[test]
    fn a_pipeline_names_the_branch_for_a_ticket() {
        assert_eq!(parse(ONE_STAGE).unwrap().branch_for("INTH-166"), None);
        let p = parse(&format!("branch = \"{{ticket}}\"\n{ONE_STAGE}")).unwrap();
        assert_eq!(p.branch_for("INTH-166").as_deref(), Some("INTH-166"));
        assert_eq!(p.branch_for("PRE-151").as_deref(), Some("PRE-151"));
        let lower = parse(&format!("branch = \"fix/{{ticket_lower}}\"\n{ONE_STAGE}")).unwrap();
        assert_eq!(lower.branch_for("INTH-7").as_deref(), Some("fix/inth-7"));
        let blank = parse(&format!("branch = \" \"\n{ONE_STAGE}")).unwrap();
        assert_eq!(blank.branch, None);
    }

    #[test]
    fn saving_keeps_or_drops_the_branch() {
        let d = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(folder(d.path())).unwrap();
        std::fs::write(folder(d.path()).join("t.toml"), ONE_STAGE).unwrap();
        let mut edit = edit_of(&load(d.path(), "t").unwrap());
        assert_eq!(edit.branch, None);
        edit.branch = Some("{ticket}".into());
        save(d.path(), "t", &edit).unwrap();
        assert_eq!(
            load(d.path(), "t").unwrap().branch.as_deref(),
            Some("{ticket}")
        );
        edit.branch = None;
        save(d.path(), "t", &edit).unwrap();
        assert_eq!(load(d.path(), "t").unwrap().branch, None);
    }

    #[test]
    fn a_pipeline_can_start_itself_and_watch_linear() {
        let p = parse(ONE_STAGE).unwrap();
        assert_eq!(p.autostart, None);
        let p = parse(&format!(
            "{ONE_STAGE}[autostart]\nenv = \"iov42\"\n[linear]\nteam = \"INTH\"\nwatch = true\n"
        ))
        .unwrap();
        assert_eq!(p.autostart.as_deref(), Some("iov42"));
        assert!(p.linear.unwrap().watch);
        let quiet = parse(&format!("{ONE_STAGE}[linear]\nteam = \"INTH\"\n")).unwrap();
        assert!(!quiet.linear.unwrap().watch, "off unless asked");
        assert!(parse(&format!("{ONE_STAGE}[autostart]\nenv = \" \"\n")).is_err());
    }

    #[test]
    fn a_pipeline_names_the_channel_its_product_questions_go_to() {
        assert_eq!(parse(ONE_STAGE).unwrap().product_channel, None);
        let p = parse(&format!(
            "{ONE_STAGE}[product]\nchannel = \" #product- \"\n"
        ))
        .unwrap();
        assert_eq!(p.product_channel.as_deref(), Some("#product-"));
        assert!(parse(&format!("{ONE_STAGE}[product]\nchannel = \"\"\n")).is_err());

        let d = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(folder(d.path())).unwrap();
        std::fs::write(folder(d.path()).join("t.toml"), ONE_STAGE).unwrap();
        let mut edit = Edit::from(&load(d.path(), "t").unwrap());
        edit.product_channel = Some("#product-".into());
        save(d.path(), "t", &edit).unwrap();
        assert_eq!(
            load(d.path(), "t").unwrap().product_channel.as_deref(),
            Some("#product-")
        );
        edit.product_channel = Some(" ".into());
        save(d.path(), "t", &edit).unwrap();
        assert_eq!(load(d.path(), "t").unwrap().product_channel, None);
    }

    #[test]
    fn a_linear_table_takes_its_defaults_from_the_team_alone() {
        assert_eq!(parse(ONE_STAGE).unwrap().linear, None);
        let p = parse(&format!("{ONE_STAGE}[linear]\nteam = \"INTH\"\n")).unwrap();
        assert_eq!(
            p.linear,
            Some(LinearSettings {
                team: "INTH".into(),
                pick: "Ready for Investigation".into(),
                assignee: Assignee::Unassigned,
                claim: "In Investigation".into(),
                passed: "Ready for Peer Review".into(),
                every_minutes: 5,
                watch: false,
            })
        );
    }

    #[test]
    fn a_pipeline_can_trigger_off_another_state_and_any_assignee() {
        let p = parse(&format!(
            "{ONE_STAGE}[linear]\nteam = \"INTH\"\npick = \"Ready for Review\"\nassignee = \"any\"\n\
             claim = \"In Review\"\npassed = \"Done\"\nevery_minutes = 15\n"
        ))
        .unwrap();
        let l = p.linear.unwrap();
        assert_eq!(l.pick, "Ready for Review");
        assert_eq!(l.assignee, Assignee::Any);
        assert_eq!((l.claim.as_str(), l.passed.as_str()), ("In Review", "Done"));
        assert_eq!(l.every_minutes, 15);
    }

    #[test]
    fn linear_mistakes_are_named() {
        for (table, says) in [
            ("[linear]\npick = \"X\"\n", "team"),
            ("[linear]\nteam = \" \"\n", "team"),
            ("[linear]\nteam = \"INTH\"\nassignee = \"me\"\n", "assignee"),
            (
                "[linear]\nteam = \"INTH\"\nevery_minutes = 0\n",
                "every_minutes",
            ),
            ("[linear]\nteam = \"INTH\"\nlabel = \"x\"\n", "label"),
        ] {
            let e = parse(&format!("{ONE_STAGE}{table}")).unwrap_err();
            assert!(e.contains(says), "{says}: {e}");
        }
    }

    #[test]
    fn saving_keeps_or_drops_the_linear_table() {
        let d = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(folder(d.path())).unwrap();
        let file = folder(d.path()).join("t.toml");
        std::fs::write(
            &file,
            format!("{ONE_STAGE}[linear]\n# The INTH queue.\nteam = \"INTH\"\n"),
        )
        .unwrap();
        let mut edit = edit_of(&load(d.path(), "t").unwrap());
        let mut l = edit.linear.clone().unwrap();
        l.pick = "Ready for Review".into();
        l.assignee = Assignee::Any;
        edit.linear = Some(l);
        save(d.path(), "t", &edit).unwrap();
        let text = std::fs::read_to_string(&file).unwrap();
        assert!(text.contains("# The INTH queue."), "{text}");
        let l = load(d.path(), "t").unwrap().linear.unwrap();
        assert_eq!(
            (l.pick.as_str(), l.assignee),
            ("Ready for Review", Assignee::Any)
        );
        edit.linear = None;
        save(d.path(), "t", &edit).unwrap();
        assert_eq!(load(d.path(), "t").unwrap().linear, None);
    }

    #[test]
    fn saving_keeps_prompts_and_comments() {
        let d = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(folder(d.path())).unwrap();
        let file = folder(d.path()).join("tickets.toml");
        std::fs::write(&file, COMMENTED).unwrap();
        let mut edit = edit_of(&load(d.path(), "tickets").unwrap());
        edit.name = "INTH SLA tickets".into();
        edit.bar = 75;
        edit.stages[0].skill = Some("QA-Skills".into());
        edit.stages[1].bar = None;
        save(d.path(), "tickets", &edit).unwrap();
        let text = std::fs::read_to_string(&file).unwrap();
        for kept in [
            "# The team's pipeline.",
            "# Reproduce first.",
            "Read {ticket}.",
            "Fix it.",
            "worktree = true",
        ] {
            assert!(text.contains(kept), "{kept}: {text}");
        }
        let p = load(d.path(), "tickets").unwrap();
        assert_eq!((p.name.as_str(), p.bar), ("INTH SLA tickets", 75));
        assert_eq!(p.stages[0].skill.as_deref(), Some("QA-Skills"));
        assert_eq!(p.stages[1].bar, None);
        assert!(p.stages[1].worktree);
    }

    #[test]
    fn a_new_stage_gets_a_unique_id_from_its_title() {
        let d = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(folder(d.path())).unwrap();
        std::fs::write(folder(d.path()).join("tickets.toml"), COMMENTED).unwrap();
        let mut edit = edit_of(&load(d.path(), "tickets").unwrap());
        for title in ["Security review", "Develop"] {
            edit.stages.push(EditStage {
                id: None,
                title: title.into(),
                skill: Some("Security review".into()),
                folder: None,
                bar: Some(90),
                prompt: String::new(),
            });
        }
        save(d.path(), "tickets", &edit).unwrap();
        let p = load(d.path(), "tickets").unwrap();
        let ids: Vec<&str> = p.stages.iter().map(|s| s.id.as_str()).collect();
        assert_eq!(
            ids,
            ["replicate", "develop", "security-review", "develop-2"]
        );
        assert_eq!(
            p.stages[2].prompt,
            "Do the \"Security review\" stage of ticket {ticket}."
        );
        assert_eq!(p.bar_for(2), 90);
    }

    #[test]
    fn reordering_and_removing_stages() {
        let d = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(folder(d.path())).unwrap();
        std::fs::write(folder(d.path()).join("tickets.toml"), COMMENTED).unwrap();
        let mut edit = edit_of(&load(d.path(), "tickets").unwrap());
        edit.stages.swap(0, 1);
        save(d.path(), "tickets", &edit).unwrap();
        let p = load(d.path(), "tickets").unwrap();
        assert_eq!(p.stages[0].id, "develop");
        assert_eq!(p.stages[1].prompt.trim(), "Read {ticket}.");
        edit.stages.remove(0);
        save(d.path(), "tickets", &edit).unwrap();
        let p = load(d.path(), "tickets").unwrap();
        assert_eq!(p.stages.len(), 1);
        assert_eq!(p.stages[0].id, "replicate");
    }

    #[test]
    fn a_stage_can_start_in_a_folder() {
        let d = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(folder(d.path())).unwrap();
        std::fs::write(
            folder(d.path()).join("t.toml"),
            "name = \"t\"\n[[stage]]\nid = \"a\"\ntitle = \"A\"\nfolder = \"~/Projects/QA-Skills\"\n\
             [[stage]]\nid = \"b\"\ntitle = \"B\"\n",
        )
        .unwrap();
        let p = load(d.path(), "t").unwrap();
        assert_eq!(p.stages[0].folder.as_deref(), Some("~/Projects/QA-Skills"));
        assert_eq!(p.stages[1].folder, None);
        let mut edit = Edit::from(&p);
        assert_eq!(
            edit.stages[0].folder.as_deref(),
            Some("~/Projects/QA-Skills")
        );
        edit.stages[0].folder = None;
        edit.stages[1].folder = Some("~/Projects/iov42-agents".into());
        save(d.path(), "t", &edit).unwrap();
        let p = load(d.path(), "t").unwrap();
        assert_eq!(p.stages[0].folder, None);
        assert_eq!(
            p.stages[1].folder.as_deref(),
            Some("~/Projects/iov42-agents")
        );
    }

    #[test]
    fn settings_edit_each_stages_prompt() {
        let d = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(folder(d.path())).unwrap();
        std::fs::write(folder(d.path()).join("tickets.toml"), COMMENTED).unwrap();
        let mut edit = edit_of(&load(d.path(), "tickets").unwrap());
        // What the stage uses now is what the settings show.
        assert_eq!(edit.stages[0].prompt.trim(), "Read {ticket}.");
        assert_eq!(edit.stages[1].prompt, "Fix it.");
        edit.stages[0].prompt = "replicate {ticket}".into();
        edit.stages.push(EditStage {
            id: None,
            title: "Verify".into(),
            skill: None,
            folder: None,
            bar: None,
            prompt: "test {ticket} against {feature_env}".into(),
        });
        save(d.path(), "tickets", &edit).unwrap();
        let p = load(d.path(), "tickets").unwrap();
        assert_eq!(p.stages[0].prompt, "replicate {ticket}");
        assert_eq!(p.stages[2].prompt, "test {ticket} against {feature_env}");
        // A prompt left empty falls back to the default one.
        let mut edit = edit_of(&p);
        edit.stages[1].prompt = "  ".into();
        save(d.path(), "tickets", &edit).unwrap();
        assert_eq!(
            load(d.path(), "tickets").unwrap().stages[1].prompt,
            "Do the \"Develop\" stage of ticket {ticket}."
        );
    }

    #[test]
    fn validation_names_what_is_missing() {
        let mut edit = edit_of(&parse(COMMENTED).unwrap());
        assert_eq!(validate(&edit), Ok(()));
        edit.stages[1].title = " ".into();
        assert_eq!(validate(&edit), Err("Every stage needs a name"));
        edit.name = "".into();
        assert_eq!(validate(&edit), Err("Give the orchestrator a name"));
        edit.name = "x".into();
        edit.stages.clear();
        assert_eq!(validate(&edit), Err("Add at least one stage"));
    }

    #[test]
    fn linear_settings_are_checked_before_saving() {
        let mut edit = edit_of(&parse(&format!("{ONE_STAGE}[linear]\nteam = \"INTH\"\n")).unwrap());
        assert_eq!(validate(&edit), Ok(()));
        let mut l = edit.linear.clone().unwrap();
        l.team = " ".into();
        edit.linear = Some(l.clone());
        assert_eq!(validate(&edit), Err("Name the Linear team, e.g. INTH"));
        l.team = "INTH".into();
        l.pick = "".into();
        edit.linear = Some(l.clone());
        assert_eq!(validate(&edit), Err("Name the Linear states"));
        l.pick = "Ready for Review".into();
        l.every_minutes = 0;
        edit.linear = Some(l);
        assert_eq!(validate(&edit), Err("Check Linear every minute or more"));
    }

    #[test]
    fn names_lists_the_pipeline_files() {
        let d = tempfile::tempdir().unwrap();
        assert!(names(d.path()).is_empty());
        let dir = folder(d.path());
        std::fs::create_dir_all(&dir).unwrap();
        for f in ["zeta.toml", "alpha.toml", "notes.md", "Bad Name.toml"] {
            std::fs::write(dir.join(f), "").unwrap();
        }
        assert_eq!(names(d.path()), ["alpha", "zeta"]);
    }

    #[test]
    fn a_pipeline_name_is_a_plain_file_name() {
        let d = tempfile::tempdir().unwrap();
        assert!(load(d.path(), "../x").unwrap_err().contains("../x"));
        assert!(load(d.path(), "missing").unwrap_err().contains("missing"));
    }
}
