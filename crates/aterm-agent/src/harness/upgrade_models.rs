// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! THE MODEL PRIORITY LIST (`docs/DESIGN-claude-live-upgrade-models-2026-09-24.md`): which model a relaunched session gets,
//! what "available" means, and how the list grows by itself when Claude Code
//! recommends the newest model of a family.
//!
//! The list is DATA the harness maintains, not a knob: `<state>/upgrade/
//! models.json`, seeded with the owner's list (2026-09-23: *"create a priority
//! list of Opus 5.5, Fable 5.1, Opus 5"*), editable by hand or through
//! `aterm harness upgrade models set`, and extended automatically — every
//! change carries its provenance and is journalled by the caller.
//!
//! Evidence is read from files Claude Code already keeps; no model call is
//! ever made to decide anything here.
//!
//! SAME FAMILY FIRST, THEN THE PRIORITY LIST (owner decision, 2026-09-27; the
//! incident behind it: a session restarted 2.1.282 -> 2.1.283 still on
//! `claude-opus-5` while that build's own newest Opus was `claude-opus-5-5`).
//! A restart moves a conversation to the NEWEST model of ITS OWN family that
//! the build offers ([`family_successor`]) — Opus 5 -> Opus 5.5, never down.
//! Only when nothing newer of its family is on offer does the list's order
//! move it ACROSS families ([`list_target`]), and then only a model nobody
//! chose, only up the list — the 2026-09-23 decision, scoped as it was. Both
//! steps choose among ONE set of candidates ([`offered`]): the installed
//! build's own `latest_per_family`, which needs no aterm release to name a new
//! model, and the list's entries, each available and allowed.

use std::path::{Path, PathBuf};

use aterm_json::{Map, Value};

use super::upgrade::is_model_id;
use super::upgrade_catalog::{Baked, version_triple};

/// The owner's list, verbatim order (2026-09-23).
pub const SEED: [&str; 3] = ["claude-opus-5-5", "claude-fable-5-1", "claude-opus-5"];

/// `claude-<family>-<major>[-<minor>][-<yyyymmdd>]` → family and version.
/// A date suffix (six or more digits) is not part of the version.
#[must_use]
pub fn family_version(id: &str) -> Option<(String, (u32, u32))> {
    let base = id.strip_suffix("[1m]").unwrap_or(id);
    if !is_model_id(base) {
        return None;
    }
    let mut parts = base.strip_prefix("claude-")?.split('-');
    let family = parts.next()?;
    if family.is_empty() || !family.bytes().all(|b| b.is_ascii_lowercase()) {
        return None;
    }
    let nums: Vec<u32> = parts
        .filter(|p| p.len() < 6)
        .map(str::parse::<u32>)
        .collect::<Result<_, _>>()
        .ok()?;
    let major = *nums.first()?;
    let minor = nums.get(1).copied().unwrap_or(0);
    if nums.len() > 2 {
        return None;
    }
    Some((family.to_string(), (major, minor)))
}

/// One automatic or manual change to the list.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Change {
    /// The id added (or the whole new list, for `set`).
    pub id: String,
    /// Where it went: `above <id>`, `seed`, or `set`.
    pub placed: String,
    /// What recommended it (`announcement:<id>`, `catalog:<build>`, `seed`,
    /// `owner`).
    pub source: String,
    /// Unix seconds.
    pub at: i64,
}

/// The list and its history.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Priority {
    /// Best first.
    pub ids: Vec<String>,
    /// Every change, oldest first.
    pub history: Vec<Change>,
}

impl Priority {
    /// The owner's seed.
    #[must_use]
    pub fn seed(now: i64) -> Priority {
        Priority {
            ids: SEED.iter().map(|s| (*s).to_string()).collect(),
            history: vec![Change {
                id: SEED.join(","),
                placed: "seed".to_string(),
                source: "owner 2026-09-23".to_string(),
                at: now,
            }],
        }
    }

    /// Rank of `id` (0 = best), the `[1m]` suffix ignored.
    #[must_use]
    pub fn rank(&self, id: &str) -> Option<usize> {
        let base = id.strip_suffix("[1m]").unwrap_or(id);
        self.ids.iter().position(|x| x == base)
    }

    /// Parse the file. `None` for anything malformed — the caller then acts
    /// on nothing model-related rather than on a list it cannot read.
    #[must_use]
    pub fn parse(text: &str) -> Option<Priority> {
        let v: Value = aterm_json::from_str(text).ok()?;
        let ids: Vec<String> = v
            .get("priority")?
            .as_array()?
            .iter()
            .map(|x| {
                x.as_str()
                    .filter(|s| family_version(s).is_some())
                    .map(str::to_string)
            })
            .collect::<Option<_>>()?;
        if ids.is_empty() || ids.len() > 32 {
            return None;
        }
        let mut dedup = ids.clone();
        dedup.sort();
        dedup.dedup();
        if dedup.len() != ids.len() {
            return None;
        }
        let history = v
            .get("history")
            .and_then(Value::as_array)
            .map(|rows| {
                rows.iter()
                    .filter_map(|r| {
                        Some(Change {
                            id: r.get("id")?.as_str()?.to_string(),
                            placed: r.get("placed")?.as_str()?.to_string(),
                            source: r.get("source")?.as_str()?.to_string(),
                            at: r.get("at")?.as_i64()?,
                        })
                    })
                    .collect()
            })
            .unwrap_or_default();
        Some(Priority { ids, history })
    }

    /// The file's text.
    #[must_use]
    pub fn render(&self) -> String {
        let mut root = Map::new();
        root.insert(
            "priority".to_string(),
            Value::Array(self.ids.iter().cloned().map(Value::String).collect()),
        );
        let hist = self
            .history
            .iter()
            .rev()
            .take(64)
            .rev()
            .map(|c| {
                let mut m = Map::new();
                m.insert("id".to_string(), Value::String(c.id.clone()));
                m.insert("placed".to_string(), Value::String(c.placed.clone()));
                m.insert("source".to_string(), Value::String(c.source.clone()));
                m.insert("at".to_string(), Value::from(c.at));
                Value::Object(m)
            })
            .collect();
        root.insert("history".to_string(), Value::Array(hist));
        aterm_json::to_string(&Value::Object(root)).unwrap_or_default() + "\n"
    }

    /// Admit a recommended id: the grammar holds, its family is
    /// already on the list, it is strictly newer than that family's best
    /// member, and `known` (the active build's own catalog) contains it. It
    /// is placed directly ABOVE that family's best member. `None` when it is
    /// not admitted; the change otherwise.
    pub fn admit(
        &mut self,
        id: &str,
        source: &str,
        known: &dyn Fn(&str) -> bool,
        now: i64,
    ) -> Option<Change> {
        let base = id.strip_suffix("[1m]").unwrap_or(id);
        let (family, version) = family_version(base)?;
        if self.rank(base).is_some() || !known(base) {
            return None;
        }
        // The family's best member = the highest-ranked id of that family.
        let (pos, best) = self
            .ids
            .iter()
            .enumerate()
            .filter_map(|(i, x)| {
                family_version(x)
                    .filter(|(f, _)| *f == family)
                    .map(|(_, v)| (i, v))
            })
            .min_by_key(|(i, _)| *i)?;
        // Strictly newer than EVERY listed member of the family, not only the
        // best-ranked — a hand-ordered list must never be extended downward.
        let newest_listed = self
            .ids
            .iter()
            .filter_map(|x| {
                family_version(x)
                    .filter(|(f, _)| *f == family)
                    .map(|(_, v)| v)
            })
            .max()?;
        if version <= best || version <= newest_listed {
            return None;
        }
        let above = self.ids.get(pos)?.clone();
        self.ids.insert(pos, base.to_string());
        let change = Change {
            id: base.to_string(),
            placed: format!("above {above}"),
            source: source.to_string(),
            at: now,
        };
        self.history.push(change.clone());
        Some(change)
    }
}

/// Load the list, seeding (and writing) it when the file is absent.
///
/// # Errors
/// The file exists but does not parse — the caller refuses model changes.
pub fn load_or_seed(path: &Path, now: i64) -> Result<Priority, String> {
    match std::fs::read_to_string(path) {
        Ok(text) => {
            Priority::parse(&text).ok_or_else(|| format!("{} does not parse", path.display()))
        }
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
            let p = Priority::seed(now);
            write_atomic(path, &p.render()).map_err(|e| format!("{}: {e}", path.display()))?;
            Ok(p)
        }
        Err(e) => Err(format!("{}: {e}", path.display())),
    }
}

/// Write `text` to `path` through a sibling temp file and a rename.
///
/// # Errors
/// The directory could not be made or the write/rename failed.
pub fn write_atomic(path: &Path, text: &str) -> std::io::Result<()> {
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir)?;
    }
    use std::io::Write as _;
    let tmp = path.with_extension(format!("tmp{}", std::process::id()));
    let mut open = std::fs::OpenOptions::new();
    open.write(true).create(true).truncate(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt as _;
        open.mode(0o600);
    }
    let mut f = open.open(&tmp)?;
    f.write_all(text.as_bytes())?;
    f.sync_all()?;
    drop(f);
    std::fs::rename(&tmp, path)
}

// ---------------------------------------------------------------------------
// Evidence
// ---------------------------------------------------------------------------

/// One row of the served catalog (`~/.claude/cache/model-catalog/*-cc.json`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ServedRow {
    /// Model id.
    pub id: String,
    /// `main` | `overflow` | `deprecated` | ….
    pub section: String,
    /// The lowest Claude Code version the row is offered for.
    pub min_cc: Option<String>,
    /// The row is present but switched off.
    pub disabled: bool,
    /// The effort ids the row offers.
    pub efforts: Vec<String>,
}

/// One launch announcement Claude Code shows (`tengu_startup_announcements`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Announcement {
    /// Its id (`opus-5-5-launch`).
    pub id: String,
    /// The model it recommends.
    pub requires_model: String,
    /// The text shown.
    pub text: String,
}

/// Everything read from Claude Code's own files about models.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct Evidence {
    /// The served catalog rows, when a cache file exists.
    pub served: Option<Vec<ServedRow>>,
    /// `modelAccessCache` rows with `entitled:false`.
    pub denied: Vec<String>,
    /// `additionalModelOptionsCache` rows marked `disabled`.
    pub disabled_options: Vec<String>,
    /// Launch announcements.
    pub announcements: Vec<Announcement>,
}

/// Parse a served catalog cache document.
#[must_use]
pub fn parse_served(text: &str) -> Option<Vec<ServedRow>> {
    let v: Value = aterm_json::from_str(text).ok()?;
    let rows = v.pointer("/catalog/config/models")?.as_array()?;
    Some(
        rows.iter()
            .filter_map(|m| {
                let id = m.get("id")?.as_str()?;
                is_model_id(id).then(|| ServedRow {
                    id: id.to_string(),
                    section: m
                        .get("section")
                        .and_then(Value::as_str)
                        .unwrap_or("")
                        .to_string(),
                    min_cc: m
                        .get("min_claude_code_version")
                        .and_then(Value::as_str)
                        .map(str::to_string),
                    disabled: m.get("disabled").and_then(Value::as_bool) == Some(true),
                    efforts: m
                        .pointer("/thinking/effort_options")
                        .and_then(Value::as_array)
                        .map(|a| {
                            a.iter()
                                .filter_map(|e| {
                                    e.get("id").and_then(Value::as_str).map(str::to_string)
                                })
                                .collect()
                        })
                        .unwrap_or_default(),
                })
            })
            .collect(),
    )
}

/// Parse the model-related parts of `~/.claude.json`. Nothing else in that
/// file is read or kept (it also holds account identity).
#[must_use]
pub fn parse_claude_json(text: &str) -> (Vec<String>, Vec<String>, Vec<Announcement>) {
    let Ok(v) = aterm_json::from_str::<Value>(text) else {
        return (Vec::new(), Vec::new(), Vec::new());
    };
    let denied = v
        .get("modelAccessCache")
        .and_then(Value::as_array)
        .map(|a| {
            a.iter()
                .filter(|r| r.get("entitled").and_then(Value::as_bool) == Some(false))
                .filter_map(|r| r.get("apiName").and_then(Value::as_str).map(str::to_string))
                .collect()
        })
        .unwrap_or_default();
    let disabled = v
        .get("additionalModelOptionsCache")
        .and_then(Value::as_array)
        .map(|a| {
            a.iter()
                .filter(|r| r.get("disabled").and_then(Value::as_bool) == Some(true))
                .filter_map(|r| r.get("value").and_then(Value::as_str))
                .map(|s| s.strip_suffix("[1m]").unwrap_or(s).to_string())
                .collect()
        })
        .unwrap_or_default();
    let announcements = v
        .pointer("/cachedGrowthBookFeatures/tengu_startup_announcements")
        .and_then(Value::as_array)
        .map(|a| {
            a.iter()
                .filter_map(|r| {
                    let model = r.get("requiresModel")?.as_str()?;
                    is_model_id(model).then(|| Announcement {
                        id: r
                            .get("id")
                            .and_then(Value::as_str)
                            .unwrap_or("")
                            .chars()
                            .take(64)
                            .collect(),
                        requires_model: model.to_string(),
                        text: r
                            .get("text")
                            .and_then(Value::as_str)
                            .unwrap_or("")
                            .chars()
                            .take(200)
                            .collect(),
                    })
                })
                .collect()
        })
        .unwrap_or_default();
    (denied, disabled, announcements)
}

/// Read every piece of evidence from a Claude config directory
/// (`~/.claude`) and the `.claude.json` beside or inside it.
#[must_use]
pub fn read_evidence(config_dir: &Path, claude_json: &Path) -> Evidence {
    let served = std::fs::read_dir(config_dir.join("cache").join("model-catalog"))
        .ok()
        .and_then(|rd| {
            let mut files: Vec<PathBuf> = rd
                .filter_map(Result::ok)
                .map(|e| e.path())
                .filter(|p| p.to_string_lossy().ends_with("-cc.json"))
                .collect();
            // The newest cache wins when an account has several.
            files.sort_by_key(|p| std::fs::metadata(p).and_then(|m| m.modified()).ok());
            files.last().and_then(|p| std::fs::read_to_string(p).ok())
        })
        .and_then(|t| parse_served(&t));
    let (denied, disabled_options, announcements) = std::fs::read_to_string(claude_json)
        .map(|t| parse_claude_json(&t))
        .unwrap_or_default();
    Evidence {
        served,
        denied,
        disabled_options,
        announcements,
    }
}

/// Why a model is, or is not, usable now.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Availability {
    /// Usable; the evidence that says so.
    Yes(&'static str),
    /// Not usable; why.
    No(String),
}

impl Availability {
    /// Usable?
    #[must_use]
    pub fn ok(&self) -> bool {
        matches!(self, Availability::Yes(_))
    }
}

/// Is `id` usable for a session that will run Claude Code `cc_version`
/// (the served catalog, the person's deny lists, the build's own catalog)?
#[must_use]
pub fn availability(
    id: &str,
    cc_version: &str,
    ev: &Evidence,
    baked: Option<&Baked>,
) -> Availability {
    let base = id.strip_suffix("[1m]").unwrap_or(id);
    if ev.denied.iter().any(|d| d == base) {
        return Availability::No("modelAccessCache entitled:false".to_string());
    }
    if ev.disabled_options.iter().any(|d| d == base) {
        return Availability::No("additionalModelOptionsCache disabled".to_string());
    }
    if let Some(b) = baked
        && b.model(base).is_none()
    {
        return Availability::No(format!("Claude Code {cc_version} does not know it"));
    }
    match &ev.served {
        Some(rows) => {
            let Some(row) = rows.iter().find(|r| r.id == base) else {
                return Availability::No("not offered by the served catalog".to_string());
            };
            if row.disabled {
                return Availability::No("served catalog: disabled".to_string());
            }
            if row.section == "deprecated" {
                return Availability::No("served catalog: deprecated".to_string());
            }
            if let Some(min) = &row.min_cc
                && version_triple(cc_version) < version_triple(min)
            {
                return Availability::No(format!("needs Claude Code {min}"));
            }
            Availability::Yes("served catalog")
        }
        None if baked.is_some() => Availability::Yes("baked catalog"),
        None => Availability::No("no catalog evidence".to_string()),
    }
}

/// The best AVAILABLE entry of the list, with every entry's verdict.
#[must_use]
pub fn target<'a>(
    list: &'a Priority,
    cc_version: &str,
    ev: &Evidence,
    baked: Option<&Baked>,
) -> (Option<&'a str>, Vec<(&'a str, Availability)>) {
    let verdicts: Vec<(&str, Availability)> = list
        .ids
        .iter()
        .map(|id| (id.as_str(), availability(id, cc_version, ev, baked)))
        .collect();
    let best = verdicts.iter().find(|(_, a)| a.ok()).map(|(id, _)| *id);
    (best, verdicts)
}

/// Claude Code's own recommendations, newest-first by source: every
/// announcement's `requiresModel`, then the build's `latest_per_family`.
#[must_use]
pub fn recommendations(
    ev: &Evidence,
    baked: Option<&Baked>,
    build_label: &str,
) -> Vec<(String, String)> {
    let mut out: Vec<(String, String)> = ev
        .announcements
        .iter()
        .map(|a| (a.requires_model.clone(), format!("announcement:{}", a.id)))
        .collect();
    if let Some(b) = baked {
        out.extend(
            b.latest_per_family
                .iter()
                .map(|(_, id)| (id.clone(), format!("catalog:{build_label}"))),
        );
    }
    out
}

/// Apply every admissible recommendation to the list. Returns the changes.
pub fn ingest(
    list: &mut Priority,
    recs: &[(String, String)],
    baked: Option<&Baked>,
    now: i64,
) -> Vec<Change> {
    let known = |id: &str| baked.is_some_and(|b| b.model(id).is_some());
    recs.iter()
        .filter_map(|(id, source)| list.admit(id, source, &known, now))
        .collect()
}

// ---------------------------------------------------------------------------
// The live model and the model decision (the cooperative upgrade driver,
// `upgrade_drive`, asks these at each session's idle point, and every restart
// the harness makes asks them for the model that rides it)
// ---------------------------------------------------------------------------

/// Seconds a model a relaunch of this harness asked for (`--model`) is given
/// to show in the transcript before the choice is recorded as FAILED for that
/// conversation (never retried for it).
pub const MODEL_SETTLE_S: u64 = 600;

/// A `/model` result's display name (`Opus 5.5 (default)`, `Fable 5.1 (1M
/// context)`) mapped back to an id through the build's baked catalog; a 1M
/// display of a model without a native 1M window gets `[1m]`.
#[must_use]
pub fn model_of_display(display: &str, baked: Option<&Baked>) -> Option<String> {
    let mut name = display.trim().to_string();
    let mut one_m = false;
    loop {
        let before = name.clone();
        for suffix in [" (default)", " (1M context)", " (recommended)"] {
            if let Some(s) = name.strip_suffix(suffix) {
                one_m |= suffix == " (1M context)";
                name = s.trim().to_string();
            }
        }
        if name == before {
            break;
        }
    }
    if is_model_id(&name) && baked.is_some_and(|b| b.model(&name).is_some()) {
        return Some(name);
    }
    let m = baked?.models.iter().find(|m| m.display_name == name)?;
    Some(if one_m && !m.native_1m {
        format!("{}[1m]", m.id)
    } else {
        m.id.clone()
    })
}

/// What a session runs NOW, read from its transcript's tail.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LiveModel {
    /// The model id.
    pub id: String,
    /// It comes from a `/model` result newer than the last answer (someone
    /// CHOSE it), not from the last answer itself.
    pub by_command: bool,
}

/// The phrase Claude's `/model` result opens with.
const SET_MODEL: &str = "Set model to ";

/// The model display a `/model` RESULT row names: Claude's own `type:user`
/// row whose content OPENS with `<local-command-stdout>Set model to ` and the
/// display quoted in backticks (2.1.267 on) or ANSI bold (2.1.201 to 2.1.222).
/// The phrase anywhere else is text, not a choice: an answer that quotes it,
/// a tool's output, a person's prompt, a notification (measured across this
/// machine's transcripts, 2026-09-27: 36 real results and nine rows of each of
/// those kinds that the old substring match read as `/model` choices — one of
/// them an answer it then stopped counting as an answer).
fn model_command_display(line: &str) -> Option<String> {
    if !line.contains(SET_MODEL) {
        return None;
    }
    let v: Value = aterm_json::from_str(line).ok()?;
    if v.get("type").and_then(Value::as_str) != Some("user") {
        return None;
    }
    let content = v.get("message")?.get("content")?.as_str()?;
    let rest = content
        .trim_start()
        .strip_prefix("<local-command-stdout>")?
        .trim_start()
        .strip_prefix(SET_MODEL)?;
    let display = if let Some(quoted) = rest.strip_prefix('`') {
        &quoted[..quoted.find('`')?]
    } else {
        let bold = rest.strip_prefix("\u{1b}[1m")?;
        &bold[..bold.find("\u{1b}[22m")?]
    };
    Some(display.chars().take(80).collect())
}

/// The live model: the newest `/model` result ([`model_command_display`])
/// when it is newer than the last main-chain answer, else the last answer's
/// model. A `/model` display this build does not know yields `None` —
/// unknown is never guessed. `<synthetic>` rows and subagents' rows are not
/// answers.
#[must_use]
pub fn live_model(tail: &str, baked: Option<&Baked>) -> Option<LiveModel> {
    let mut answer: Option<(usize, String)> = None;
    let mut command: Option<(usize, String)> = None;
    for (i, line) in tail.lines().enumerate() {
        if let Some(display) = model_command_display(line) {
            command = Some((i, display));
            continue;
        }
        if !line.contains("\"assistant\"") {
            continue;
        }
        let Ok(v) = aterm_json::from_str::<Value>(line) else {
            continue;
        };
        if v.get("type").and_then(Value::as_str) != Some("assistant")
            || v.get("isSidechain").and_then(Value::as_bool) == Some(true)
        {
            continue;
        }
        if let Some(m) = v
            .get("message")
            .and_then(|m| m.get("model"))
            .and_then(Value::as_str)
            .filter(|m| is_model_id(m))
        {
            answer = Some((i, m.to_string()));
        }
    }
    match (command, answer) {
        (Some((ci, display)), a) if a.as_ref().is_none_or(|(ai, _)| ci > *ai) => {
            model_of_display(&display, baked).map(|id| LiveModel {
                id,
                by_command: true,
            })
        }
        (_, Some((_, id))) => Some(LiveModel {
            id,
            by_command: false,
        }),
        _ => None,
    }
}

/// Seconds without an answer after which a conversation's prompt cache is
/// COLD — Claude's cache lifetime on these sessions (1 h). A model change
/// PREFERS that moment: a switch re-reads the whole history, which a warm cache
/// would have spared (Claude 2.1.282 says so in its own "Switch model?" box).
/// It is a preference and not a condition — see [`model_moves_now`].
pub const CACHE_COLD_S: u64 = 3600;

/// The longest a DUE model move waits for a cold cache, in seconds of being due.
///
/// Waiting for a cold cache is a preference, never a condition: a switch on a
/// warm cache costs one uncached re-read of the history, but a conversation in
/// active use never goes an hour without an answer, so an unbounded wait for
/// cold never lands at all. That is not hypothetical — on 2026-09-25 a session
/// kept on `claude-opus-5` through a 2.1.282 -> 2.1.283 restart whose own
/// managed build offered `claude-opus-5-5`, and the owner had to type `/model`.
/// One cache lifetime is the horizon: within it, a conversation that goes quiet
/// switches for free; past it, it has shown it will not, and it moves anyway at
/// its next stopping point (the upgrade's READY handshake still gates that).
pub const MODEL_WARM_MAX_S: u64 = CACHE_COLD_S;

/// THE MODEL LADDER: whether a DUE move is taken on this visit, and why — or
/// `None` to wait. Monotone and bounded: every due move lands.
///
/// * `cache-cold` — no answer for [`CACHE_COLD_S`]: the switch re-reads a
///   history that is cold anyway, so it costs nothing extra.
/// * `build-restart` — the conversation is being restarted onto a newer build
///   regardless. Relaunching it on the OLD model there only schedules a SECOND
///   restart for the model later, and an active session may never go cold to
///   earn one. The restart already paid for is the moment.
/// * `warm-wait-over` — due for [`MODEL_WARM_MAX_S`] without either of the
///   above: the cheap moment is not coming; move at the next stopping point.
#[must_use]
pub fn model_moves_now(cold: bool, build_restart: bool, due_for_s: u64) -> Option<&'static str> {
    if cold {
        Some("cache-cold")
    } else if build_restart {
        Some("build-restart")
    } else if due_for_s >= MODEL_WARM_MAX_S {
        Some("warm-wait-over")
    } else {
        None
    }
}

/// Unix seconds of an RFC 3339 UTC stamp (`2026-09-24T04:18:02.796Z`, the
/// shape Claude Code's transcript stamps carry). The workspace's one parser,
/// [`aterm_types::rfc3339::parse_utc_fractional`]: a zone offset (`…+05:30`)
/// is refused rather than read as UTC, which would move the instant by up to
/// 14 h; a stamp before the epoch is `None`.
#[must_use]
pub fn parse_utc(s: &str) -> Option<u64> {
    u64::try_from(aterm_types::rfc3339::parse_utc_fractional(s)?).ok()
}

/// Unix seconds of the last MAIN-CHAIN answer in a transcript tail (`None`: no
/// answer in the tail — the conversation has not answered for a long time, or
/// never).
#[must_use]
pub fn last_answer_at(tail: &str) -> Option<u64> {
    // From the end: the newest answer is the first one met.
    tail.lines()
        .rev()
        .filter(|l| l.contains("\"assistant\""))
        .filter_map(|l| aterm_json::from_str::<Value>(l).ok())
        .filter(|v| {
            v.get("type").and_then(Value::as_str) == Some("assistant")
                && v.get("isSidechain").and_then(Value::as_bool) != Some(true)
        })
        .find_map(|v| {
            v.get("timestamp")
                .and_then(Value::as_str)
                .and_then(parse_utc)
        })
}

/// Unix seconds of a `ps -o lstart=` stamp in UTC, C locale — the session
/// file's `procStart` (`Thu Sep 24 23:21:50 2026`).
#[must_use]
pub fn parse_lstart(s: &str) -> Option<u64> {
    let w: Vec<&str> = s.split_whitespace().collect();
    let [_, mon, day, hms, year] = w.as_slice() else {
        return None;
    };
    const MONTHS: [&str; 12] = [
        "Jan", "Feb", "Mar", "Apr", "May", "Jun", "Jul", "Aug", "Sep", "Oct", "Nov", "Dec",
    ];
    let m = MONTHS.iter().position(|x| x == mon)? + 1;
    let day: u32 = day.parse().ok()?;
    let year: u32 = year.parse().ok()?;
    parse_utc(&format!("{year:04}-{m:02}-{day:02}T{hms}Z"))
}

/// [`live_model`] for a process started at `started_s` with the launch's own
/// `--model` (`launch`): when neither the last answer nor the last `/model` is
/// newer than the process, what runs is the launch flag — a resumed
/// conversation's transcript still names the model of the process before it.
#[must_use]
pub fn live_model_at(
    tail: &str,
    baked: Option<&Baked>,
    launch: Option<&str>,
    started_s: Option<u64>,
) -> Option<LiveModel> {
    let from_transcript = live_model(tail, baked);
    // A real id (`claude-<family>-<n>…`), not an alias (`opus`), which only
    // the build resolves.
    let real =
        |m: &&str| is_model_id(m) && family_version(m.strip_suffix("[1m]").unwrap_or(m)).is_some();
    let (Some(launch), Some(started)) = (launch.filter(real), started_s) else {
        return from_transcript;
    };
    let newest = tail
        .lines()
        .filter(|l| l.contains("\"assistant\"") || model_command_display(l).is_some())
        .filter_map(|l| aterm_json::from_str::<Value>(l).ok())
        .filter(|v| v.get("isSidechain").and_then(Value::as_bool) != Some(true))
        .filter_map(|v| {
            v.get("timestamp")
                .and_then(Value::as_str)
                .and_then(parse_utc)
        })
        .max();
    if newest.is_none_or(|t| t < started) {
        return Some(LiveModel {
            id: launch.to_string(),
            by_command: false,
        });
    }
    from_transcript
}

/// The person's own Claude settings that bear on the model
/// (`~/.claude/settings.json`): the saved default and the allowlist.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct UserModelSettings {
    /// `model`: the default new sessions start on (an id or a family alias) —
    /// a choice of the person's, which the list never moves across families
    /// ([`model_due`]).
    pub default: Option<String>,
    /// `availableModels`: when present, the only models a session may use.
    pub allowed: Option<Vec<String>>,
}

/// Read [`UserModelSettings`] out of a settings file's text; anything
/// malformed reads as absent.
#[must_use]
pub fn parse_user_settings(text: &str) -> UserModelSettings {
    let Ok(v) = aterm_json::from_str::<Value>(text) else {
        return UserModelSettings::default();
    };
    UserModelSettings {
        default: v
            .get("model")
            .and_then(Value::as_str)
            .filter(|m| is_model_id(m))
            .map(str::to_string),
        allowed: v.get("availableModels").and_then(Value::as_array).map(|a| {
            a.iter()
                .filter_map(Value::as_str)
                .filter(|m| is_model_id(m))
                .map(str::to_string)
                .collect()
        }),
    }
}

/// THE LIST'S TARGET — the second step of the rule ([`model_due`]): the first
/// entry of the list that is available ([`availability`]) AND allowed by the
/// person's `availableModels`, i.e. the best-ranked entry [`offered`] holds
/// ([`list_target`]), so the fallback and the same-family step read ONE set
/// of candidates.
#[must_use]
pub fn target_allowed(
    list: &Priority,
    cc_version: &str,
    ev: &Evidence,
    baked: Option<&Baked>,
    allowed: Option<&[String]>,
) -> Option<String> {
    list_target(list, &offered(list, cc_version, ev, baked, allowed)).map(str::to_string)
}

/// The best-ranked entry of `list` that `offered` holds (the `[1m]` tag
/// ignored), as the list spells it; `None` when it holds none.
#[must_use]
pub fn list_target<'a>(list: &'a Priority, offered: &[String]) -> Option<&'a str> {
    list.ids.iter().map(String::as_str).find(|id| {
        let base = id.strip_suffix("[1m]").unwrap_or(id);
        offered.iter().any(|o| o == base)
    })
}

/// Whether the person's `availableModels` (`None`: they set none) admits
/// `id`, as itself or in its `[1m]` form.
fn allowed_by(allowed: Option<&[String]>, id: &str) -> bool {
    allowed.is_none_or(|ok| {
        ok.iter()
            .any(|x| x == id || x.strip_suffix("[1m]") == Some(id))
    })
}

/// EVERY MODEL A RESTART ON THE BUILD `cc_version` MAY ASK FOR — the ONE set
/// of candidates both steps of the rule choose among ([`family_successor`],
/// then [`list_target`]): the build's own newest
/// model of each family (`latest_per_family`, read out of the INSTALLED
/// Claude Code, so a new model needs no aterm release to be named) and every
/// entry of the list (which Claude Code's own announcements grow), each one
/// AVAILABLE ([`availability`]: known to the build, offered by the served
/// catalog, not denied or switched off for the account) and allowed by the
/// person's `availableModels`. Bare ids (no `[1m]`), each once, in that order.
/// A model the build does not know is never among them: nothing here is
/// guessed.
#[must_use]
pub fn offered(
    list: &Priority,
    cc_version: &str,
    ev: &Evidence,
    baked: Option<&Baked>,
    allowed: Option<&[String]>,
) -> Vec<String> {
    let latest = baked
        .into_iter()
        .flat_map(|b| b.latest_per_family.iter().map(|(_, id)| id.as_str()));
    let mut out: Vec<String> = Vec::new();
    for id in latest.chain(list.ids.iter().map(String::as_str)) {
        let base = id.strip_suffix("[1m]").unwrap_or(id);
        if family_version(base).is_some()
            && !out.iter().any(|o| o == base)
            && availability(base, cc_version, ev, baked).ok()
            && allowed_by(allowed, base)
        {
            out.push(base.to_string());
        }
    }
    out
}

/// THE SAME-FAMILY RESOLVER — the one place a model id is read for "what does
/// this conversation move to": the NEWEST of `offered` in `current`'s own
/// family ([`family_version`]), when it is STRICTLY newer than `current`,
/// with the `[1m]` context tag appended when `one_m` (the conversation runs
/// the 1M window: `claude-opus-5[1m]` -> `claude-opus-5-5[1m]`). `None`, and
/// nothing moves, for everything else:
///
/// * another family — never Sonnet -> Opus, never Opus -> Fable or Haiku;
/// * an older or equal version — never down, never sideways (a dated id is
///   its undated version: `claude-haiku-4-5-20251001` is Haiku 4.5);
/// * a `current` outside the grammar — a family alias (`opus`), a provider's
///   id (`us.anthropic.…`), an id from before it (`claude-3-5-sonnet-…`), a
///   model newer than this reader: never guessed at.
#[must_use]
pub fn family_successor(current: &str, one_m: bool, offered: &[String]) -> Option<String> {
    let (family, version) = family_version(current)?;
    let (_, newest) = offered
        .iter()
        .filter_map(|id| {
            let base = id.strip_suffix("[1m]").unwrap_or(id);
            family_version(base)
                .filter(|(f, v)| *f == family && *v > version)
                .map(|(_, v)| (v, base))
        })
        .max_by_key(|(v, _)| *v)?;
    Some(if one_m {
        format!("{newest}[1m]")
    } else {
        newest.to_string()
    })
}

/// What the harness remembers about ONE conversation's model, beside main's
/// per-conversation upgrade state (`<state>/upgrade/models/<session>.json`).
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ModelRecord {
    /// Models this harness already moved the conversation to: never re-applied
    /// (a person who moves away from one has decided).
    pub applied: Vec<String>,
    /// The model the harness last asked for (a relaunch's `--model`), and
    /// when.
    pub set: String,
    /// Unix seconds.
    pub set_at: u64,
    /// Models this harness asked for (a relaunch's `--model`) that did not
    /// become what runs: never asked for again.
    pub failed: Vec<String>,
    /// The model a PERSON last chose for this conversation with `/model` (the
    /// harness never types `/model`), remembered past the answers that follow
    /// it: the list never moves that choice across families.
    pub human: String,
    /// The screen sequence last seen, and since when (the quiet gate).
    pub last_seq: u64,
    /// Unix seconds.
    pub seq_since_s: u64,
    /// The model a move has been DUE to, and since when (Unix seconds) — the
    /// clock [`MODEL_WARM_MAX_S`] bounds. Reset whenever the due target
    /// changes, cleared when nothing is due.
    pub due_to: String,
    /// Unix seconds.
    pub due_since: u64,
}

impl ModelRecord {
    /// Parse; `None` for anything else.
    #[must_use]
    pub fn parse(text: &str) -> Option<ModelRecord> {
        let v: Value = aterm_json::from_str(text).ok()?;
        let list = |k: &str| -> Vec<String> {
            v.get(k)
                .and_then(Value::as_array)
                .map(|a| {
                    a.iter()
                        .filter_map(Value::as_str)
                        .filter(|s| is_model_id(s))
                        .map(str::to_string)
                        .collect()
                })
                .unwrap_or_default()
        };
        let num = |k: &str| v.get(k).and_then(Value::as_u64).unwrap_or(0);
        Some(ModelRecord {
            applied: list("applied"),
            set: v
                .get("set")
                .and_then(Value::as_str)
                .filter(|s| s.is_empty() || is_model_id(s))
                .unwrap_or_default()
                .to_string(),
            set_at: num("set_at"),
            failed: list("failed"),
            human: v
                .get("human")
                .and_then(Value::as_str)
                .filter(|s| s.is_empty() || is_model_id(s))
                .unwrap_or_default()
                .to_string(),
            last_seq: num("last_seq"),
            seq_since_s: num("seq_since_s"),
            // Absent in a record an older harness wrote: nothing due yet, so
            // the clock starts on this harness's first due visit.
            due_to: v
                .get("due_to")
                .and_then(Value::as_str)
                .filter(|s| s.is_empty() || is_model_id(s))
                .unwrap_or_default()
                .to_string(),
            due_since: num("due_since"),
        })
    }

    /// Render (the inverse of [`ModelRecord::parse`]).
    #[must_use]
    pub fn render(&self) -> String {
        let mut m = Map::new();
        let arr = |xs: &[String]| Value::Array(xs.iter().cloned().map(Value::String).collect());
        m.insert("applied".into(), arr(&self.applied));
        m.insert("set".into(), Value::String(self.set.clone()));
        m.insert("set_at".into(), Value::from(self.set_at));
        m.insert("failed".into(), arr(&self.failed));
        m.insert("human".into(), Value::String(self.human.clone()));
        m.insert("last_seq".into(), Value::from(self.last_seq));
        m.insert("seq_since_s".into(), Value::from(self.seq_since_s));
        m.insert("due_to".into(), Value::String(self.due_to.clone()));
        m.insert("due_since".into(), Value::from(self.due_since));
        aterm_json::to_string(&Value::Object(m)).unwrap_or_default()
    }
}

/// Why a conversation's model is (or is not) moved.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ModelVerdict {
    /// Move it from the live model to the target.
    Due {
        /// The live model.
        from: String,
        /// The newest model of its family on offer ([`family_successor`],
        /// `[1m]` kept) — or, with none newer, the list's target
        /// ([`list_target`]).
        to: String,
    },
    /// Leave it, and why (one word for the report).
    Keep(&'static str),
}

/// THE MODEL RULE — SAME FAMILY FIRST, THEN THE PRIORITY LIST (owner
/// decision, 2026-09-27). Both steps choose among `offered` ([`offered`]:
/// only models the build knows and the account and the person's
/// `availableModels` allow), and neither ever moves a conversation down.
///
/// 1. **Its own family.** The NEWEST offered model of the live model's family,
///    strictly newer ([`family_successor`]) — Opus 5 -> Opus 5.5, its `[1m]`
///    window kept. Whoever chose the model, this is the move: aterm cannot
///    tell a pin made on purpose from one made before the newer model existed
///    (Claude Code's own `/model` saves its choice as the default for every
///    new session), and the live proof of design §9 took it for a person's
///    `--model claude-opus-5`. When a newer model of the family is on offer,
///    nothing ever crosses families, whatever the list ranks above it.
/// 2. **Then the list** — ONLY when nothing newer of its family is on offer:
///    the list's target ([`list_target`]), when it is of ANOTHER family and
///    ranks ABOVE the live model, and only for a model NOBODY CHOSE — the
///    2026-09-23 decision, scoped as it was. A person's choice is any of: a
///    launch `--model` this harness did not put there, a `/model` (now, or
///    remembered from before its answers, [`ModelRecord::human`]), or the
///    saved default in their settings of the live model's family (an id, or
///    an alias like `opus`). A list entry of the live model's own family is
///    never a move here: it is no newer (step 1 found none), so it could only
///    be a step sideways or down.
///
/// Each `Keep` word:
///
/// * `no-model-available` — the build offers nothing (no catalog to read);
/// * `model-alias` — the launch's `--model` is not an id this grammar reads: a
///   family ALIAS (`opus`, `sonnet[1m]`), which the build already resolves to
///   the newest of its family by its own table, or a provider's id. The flag
///   is carried verbatim, never replaced with a guess, and — a person's
///   choice of family — never moved across;
/// * `model-unknown` — what the conversation runs could not be read now;
/// * `model-unreadable` — it runs an id outside the grammar: never guessed;
/// * `model-applied-before` — the harness already moved this conversation to
///   that model once: a person who moved back from it has decided;
/// * `model-failed-before` — it was asked for once and did not take;
/// * `model-current` — nothing newer of its family, and the list offers
///   nothing of another family (or it is what runs);
/// * `model-off-list` — nothing newer of its family, and the live model is not
///   on the list: a model the list does not name is left alone;
/// * `model-ranks-higher` — nothing newer of its family, and the list's
///   target does not rank above the live model: never down, never sideways;
/// * `model-chosen-by-hand` — nothing newer of its family, and a person chose
///   the live model: the list never moves it across families.
#[must_use]
pub fn model_due(
    list: &Priority,
    live: Option<&LiveModel>,
    offered: &[String],
    launch_model: Option<&str>,
    default_model: Option<&str>,
    record: &ModelRecord,
) -> ModelVerdict {
    if offered.is_empty() {
        return ModelVerdict::Keep("no-model-available");
    }
    if launch_model.is_some_and(|m| family_version(m).is_none()) {
        return ModelVerdict::Keep("model-alias");
    }
    let Some(live) = live else {
        return ModelVerdict::Keep("model-unknown");
    };
    let Some((family, _)) = family_version(&live.id) else {
        return ModelVerdict::Keep("model-unreadable");
    };
    let base = |m: &str| m.strip_suffix("[1m]").unwrap_or(m).to_string();
    // Never twice, never after a failure — for either step's target.
    let spent = |to: &str| {
        if record.applied.iter().any(|a| base(a) == base(to)) {
            Some(ModelVerdict::Keep("model-applied-before"))
        } else if record.failed.iter().any(|a| base(a) == base(to)) {
            Some(ModelVerdict::Keep("model-failed-before"))
        } else {
            None
        }
    };
    // 1. SAME FAMILY FIRST. The 1M window rides the move: the live id's tag,
    // or — a transcript names the bare API id — the launch `--model`'s, when
    // that is the same family and no `/model` has chosen since.
    let tagged = |m: &str| m.ends_with("[1m]");
    let one_m = tagged(&live.id)
        || (!live.by_command
            && launch_model
                .is_some_and(|m| tagged(m) && family_version(m).is_some_and(|(f, _)| f == family)));
    if let Some(to) = family_successor(&live.id, one_m, offered) {
        return spent(&to).unwrap_or(ModelVerdict::Due {
            from: live.id.clone(),
            to,
        });
    }
    // 2. THEN THE PRIORITY LIST: across families, up the list, for a model
    // nobody chose.
    let Some(target) = list_target(list, offered) else {
        return ModelVerdict::Keep("model-current");
    };
    if family_version(target).is_some_and(|(f, _)| f == family) {
        return ModelVerdict::Keep("model-current");
    }
    let (Some(rl), Some(rt)) = (list.rank(&live.id), list.rank(target)) else {
        return ModelVerdict::Keep("model-off-list");
    };
    if rt >= rl {
        return ModelVerdict::Keep("model-ranks-higher");
    }
    if let Some(keep) = spent(target) {
        return keep;
    }
    let ours = |m: &str| !record.set.is_empty() && base(m) == base(&record.set);
    let family_of = |m: &str| {
        family_version(&base(m))
            .map(|f| f.0)
            .unwrap_or_else(|| base(m).to_ascii_lowercase())
    };
    let chosen = launch_model.is_some_and(|m| !ours(m))
        || (live.by_command && !ours(&live.id))
        || (!record.human.is_empty() && base(&record.human) == base(&live.id))
        || default_model.is_some_and(|d| family_of(d) == family);
    if chosen {
        return ModelVerdict::Keep("model-chosen-by-hand");
    }
    ModelVerdict::Due {
        from: live.id.clone(),
        to: target.to_string(),
    }
}

/// What one visit's read of the live model did to a conversation's
/// [`ModelRecord`] ([`ModelRecord::settle`]).
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Settled {
    /// The model this harness asked for runs: recorded APPLIED (the caller
    /// says `model-verified`).
    pub verified: Option<String>,
    /// The model this harness asked for is still not what runs
    /// [`MODEL_SETTLE_S`] after it was asked for: recorded FAILED and the ask
    /// cleared (the caller says `model-failed`).
    pub failed: Option<String>,
    /// The record changed (either of the above, or a `/model` remembered):
    /// the caller saves it.
    pub changed: bool,
}

impl ModelRecord {
    /// THE SETTLE STEP, taken on every visit that reads what the
    /// conversation runs (`live`, `None` when it could not be read) at `now`
    /// (Unix seconds), before [`model_due`] is asked:
    ///
    /// * a model this harness asked for ([`ModelRecord::set`]) that now runs
    ///   is recorded APPLIED, never asked for again;
    /// * one still not what runs (an unreadable live model counts as not
    ///   running) [`MODEL_SETTLE_S`] after it was asked for, and never
    ///   recorded applied, is recorded FAILED for this conversation, never
    ///   asked for again, and the ask is cleared;
    /// * an ask once APPLIED stands: it RAN, so a conversation that runs
    ///   something else later (a person's `/model`) is not a relaunch that
    ///   failed, and the ask stays this harness's own — its `--model` on the
    ///   process is never then read as a person's choice (skeptic review of
    ///   `HarnessModelSwitch`, 2026-09-27: it was recorded FAILED, and
    ///   ledgered `model-failed`, after its own `model-verified`);
    /// * a `/model` newer than the last answer is a PERSON's (the harness
    ///   never types one): remembered ([`ModelRecord::human`]), so the
    ///   answers that follow do not end its protection.
    pub fn settle(&mut self, live: Option<&LiveModel>, now: u64) -> Settled {
        let base = |m: &str| m.strip_suffix("[1m]").unwrap_or(m).to_string();
        let mut out = Settled::default();
        let applied = |rec: &ModelRecord| rec.applied.iter().any(|a| base(a) == base(&rec.set));
        if !self.set.is_empty()
            && live.is_some_and(|l| base(&l.id) == base(&self.set))
            && !applied(self)
        {
            self.applied.push(self.set.clone());
            out.verified = Some(self.set.clone());
            out.changed = true;
        }
        if !self.set.is_empty()
            && live.is_none_or(|l| base(&l.id) != base(&self.set))
            && now.saturating_sub(self.set_at) >= MODEL_SETTLE_S
            && !applied(self)
        {
            let failed = std::mem::take(&mut self.set);
            self.failed.push(failed.clone());
            out.failed = Some(failed);
            out.changed = true;
        }
        if let Some(l) = live.filter(|l| l.by_command)
            && self.human != l.id
        {
            self.human.clone_from(&l.id);
            out.changed = true;
        }
        out
    }

    /// THE DUE CLOCK, stepped by `verdict` at `now`: since when THIS move has
    /// been due ([`ModelRecord::due_to`], [`ModelRecord::due_since`]). It
    /// restarts when the target changes (a newer model is a new wait, not the
    /// old one's remainder) and is cleared when the conversation DEFINITELY
    /// needs no move (it runs the target, or the move is not the harness's to
    /// make), so a move that stopped being due and came back never inherits a
    /// stale clock.
    ///
    /// `model-unknown` is NOT such a verdict. It means this visit could not
    /// read what the conversation runs — a transcript tail with no answer in
    /// it, which a long tool result can cause for many visits running — and
    /// clearing on it would restart the bound on every such flicker, so an
    /// active session's move might never land: the never-lands shape this
    /// clock exists to end (adversarial review, 2026-09-25). An unknown visit
    /// leaves the clock exactly as it was.
    ///
    /// A start AHEAD of `now` (the wall clock stepped back after it was
    /// stamped: a manual date change, a boot before NTP) would read as 0 s due
    /// for the whole skew and postpone the bound by exactly that much: it is
    /// restarted instead, so the bound lands on time.
    ///
    /// Returns the seconds the current move has been due (0 when none is) —
    /// what [`model_moves_now`] bounds — and whether the record changed.
    pub fn due_clock(&mut self, verdict: &ModelVerdict, now: u64) -> (u64, bool) {
        let base = |m: &str| m.strip_suffix("[1m]").unwrap_or(m).to_string();
        match verdict {
            ModelVerdict::Due { to, .. } => {
                let restart =
                    base(&self.due_to) != base(to) || self.due_since == 0 || self.due_since > now;
                if restart {
                    self.due_to.clone_from(to);
                    self.due_since = now;
                }
                (now.saturating_sub(self.due_since), restart)
            }
            ModelVerdict::Keep("model-unknown") => (0, false),
            ModelVerdict::Keep(_) => {
                let clear = !self.due_to.is_empty() || self.due_since != 0;
                if clear {
                    self.due_to.clear();
                    self.due_since = 0;
                }
                (0, clear)
            }
        }
    }

    /// Remember `model` as the one a relaunch of this conversation asks for,
    /// at `now` — written BEFORE the relaunch's one act, so it is never asked
    /// for twice (a model then not taken is recorded as failed,
    /// [`ModelRecord::settle`]).
    pub fn asked(&mut self, model: &str, now: u64) {
        model.clone_into(&mut self.set);
        self.set_at = now;
    }
}

/// What ONE VISIT reads a conversation's model against
/// ([`model_read_step`]): the list, the catalog and the offer of the build the
/// session runs (the managed build's, or the native build's for a session
/// that runs it), the launch's own `--model`, the person's saved default,
/// when the process started (`procStart`, Unix seconds), and the wall clock.
#[derive(Debug, Clone, Copy)]
pub struct ModelView<'a> {
    /// The priority list.
    pub list: &'a Priority,
    /// The build's baked catalog (a `/model` display is read through it).
    pub baked: Option<&'a Baked>,
    /// Every model a restart on that build may ask for ([`offered`]).
    pub offered: &'a [String],
    /// The launch's own `--model`.
    pub launch: Option<&'a str>,
    /// The person's saved default (`~/.claude/settings.json` `model`).
    pub default_model: Option<&'a str>,
    /// When the process started ([`live_model_at`]).
    pub started_s: Option<u64>,
    /// The wall clock, Unix seconds.
    pub now: u64,
}

/// What one visit's read decided ([`model_read_step`]).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ModelReadStep {
    /// THE MODEL RULE's verdict ([`model_due`]).
    pub verdict: ModelVerdict,
    /// The conversation's prompt cache is cold (no answer for
    /// [`CACHE_COLD_S`]).
    pub cold: bool,
    /// Seconds the CURRENT due move has been due (0 when none is) — the clock
    /// [`MODEL_WARM_MAX_S`] bounds ([`ModelRecord::due_clock`]).
    pub due_for_s: u64,
    /// What THE SETTLE STEP recorded ([`ModelRecord::settle`]): the caller
    /// ledgers `model-verified` / `model-failed`.
    pub settled: Settled,
    /// The record changed (the settle step, or the clock): the caller saves
    /// it.
    pub changed: bool,
}

/// ONE VISIT'S READ of a conversation's model, over its record (changed in
/// place) and against `view`, in the one order the harness takes it:
///
/// * no model can be run and none is pending (nothing on `view.offered`, no
///   ask in the record): `no-model-available`, the due clock cleared as that
///   verdict clears it (a move that comes back later must not inherit this
///   one's start and skip the warm-cache grace) — the transcript is not read;
/// * else the transcript's tail (`tail`, read only now) says what the
///   conversation runs ([`live_model_at`]); THE SETTLE STEP
///   ([`ModelRecord::settle`]) records what the harness asked for; THE MODEL
///   RULE ([`model_due`]) judges the SETTLED record; the cache is cold when no
///   answer came for [`CACHE_COLD_S`] ([`last_answer_at`]); and THE DUE CLOCK
///   ([`ModelRecord::due_clock`]) is stepped by the verdict.
///
/// The whole of `upgrade_drive::model_read_for` but its I/O (loading and
/// saving the record, reading the transcript, the ledger rows), so the
/// derived model `HarnessModelSwitch` binds the read itself at Tier 1.
pub fn model_read_step<S: AsRef<str>>(
    record: &mut ModelRecord,
    view: &ModelView<'_>,
    tail: impl FnOnce() -> S,
) -> ModelReadStep {
    if view.offered.is_empty() && record.set.is_empty() {
        let verdict = ModelVerdict::Keep("no-model-available");
        let (_, changed) = record.due_clock(&verdict, view.now);
        return ModelReadStep {
            verdict,
            cold: false,
            due_for_s: 0,
            settled: Settled::default(),
            changed,
        };
    }
    let tail = tail();
    let tail = tail.as_ref();
    let live = live_model_at(tail, view.baked, view.launch, view.started_s);
    let settled = record.settle(live.as_ref(), view.now);
    let verdict = model_due(
        view.list,
        live.as_ref(),
        view.offered,
        view.launch,
        view.default_model,
        record,
    );
    let cold = last_answer_at(tail).is_none_or(|at| view.now.saturating_sub(at) >= CACHE_COLD_S);
    let (due_for_s, clocked) = record.due_clock(&verdict, view.now);
    let changed = settled.changed || clocked;
    ModelReadStep {
        verdict,
        cold,
        due_for_s,
        settled,
        changed,
    }
}

/// THE MODEL A RESTART OF THIS CONVERSATION ASKS FOR (`--model` on its
/// relaunch), or `None` for none:
///
/// * an ANNOUNCED model (`announced`: the upgrade already told the agent it
///   is moving to it) rides the upgrade's own state whatever this visit
///   decides: the READY answer warms the cache again, and a decision re-taken
///   then would drop the model half-way;
/// * else a DUE move ([`model_due`]) when THE MODEL LADDER
///   ([`model_moves_now`]) takes it now — `build_restart` when the restart
///   happens regardless (a newer build is due, or it is a restart made for
///   another reason);
/// * else none.
#[must_use]
pub fn model_to(
    announced: Option<&str>,
    verdict: &ModelVerdict,
    cold: bool,
    build_restart: bool,
    due_for_s: u64,
) -> Option<String> {
    if let Some(model) = announced {
        return Some(model.to_string());
    }
    match verdict {
        ModelVerdict::Due { to, .. }
            if model_moves_now(cold, build_restart, due_for_s).is_some() =>
        {
            Some(to.clone())
        }
        _ => None,
    }
}

/// WHY a relaunch asks for `to` over `before` (the model the conversation ran),
/// read back from the two ids — which the rule makes exact: [`model_due`]
/// crosses families only by the list's step, so the same family is the first
/// step's move and another family the list's. `Unknown` when either id is
/// outside the grammar (or `before` is not known). The words every notice
/// says it in are [`MoveWhy::words`].
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum MoveWhy {
    /// The newest model of the conversation's own family.
    Family,
    /// The priority list's target, nothing newer of its family being on offer.
    List,
    /// Not known from the ids.
    Unknown,
}

impl MoveWhy {
    /// The reason as the notices say it, after the model's id.
    #[must_use]
    pub fn words(self) -> &'static str {
        match self {
            MoveWhy::Family => "the newest model of its family",
            MoveWhy::List => {
                "the best available model on aterm's priority list, nothing newer of its own \
                 family being on offer"
            }
            MoveWhy::Unknown => "the model aterm's upgrade chose for it",
        }
    }
}

/// [`MoveWhy`] for a move from `before` to `to`.
#[must_use]
pub fn move_why(before: Option<&str>, to: &str) -> MoveWhy {
    let family = |m: &str| family_version(m).map(|(f, _)| f);
    match (before.and_then(family), family(to)) {
        (Some(a), Some(b)) if a == b => MoveWhy::Family,
        (Some(_), Some(_)) => MoveWhy::List,
        _ => MoveWhy::Unknown,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::harness::upgrade_catalog::BakedModel;

    fn baked(ids: &[&str], latest: &[(&str, &str)]) -> Baked {
        Baked {
            latest_per_family: latest
                .iter()
                .map(|(f, i)| ((*f).to_string(), (*i).to_string()))
                .collect(),
            models: ids
                .iter()
                .map(|id| BakedModel {
                    id: (*id).to_string(),
                    family: family_version(id).expect("id").0,
                    display_name: (*id).to_string(),
                    native_1m: true,
                    efforts: vec![],
                })
                .collect(),
        }
    }

    #[test]
    fn family_versions() {
        assert_eq!(
            family_version("claude-opus-5-5"),
            Some(("opus".into(), (5, 5)))
        );
        assert_eq!(
            family_version("claude-opus-5"),
            Some(("opus".into(), (5, 0)))
        );
        assert_eq!(
            family_version("claude-fable-5-1[1m]"),
            Some(("fable".into(), (5, 1)))
        );
        assert_eq!(
            family_version("claude-haiku-4-5-20251001"),
            Some(("haiku".into(), (4, 5)))
        );
        assert_eq!(family_version("claude-opus"), None);
        assert_eq!(family_version("claude-opus-x"), None);
        assert_eq!(family_version("gpt-5"), None);
    }

    #[test]
    fn the_seed_round_trips_and_a_malformed_file_is_refused() {
        let p = Priority::seed(7);
        let q = Priority::parse(&p.render()).expect("round trip");
        assert_eq!(q, p);
        assert_eq!(q.rank("claude-fable-5-1[1m]"), Some(1));
        for bad in [
            "{}",
            r#"{"priority":[]}"#,
            r#"{"priority":["claude-opus-5","claude-opus-5"]}"#,
            r#"{"priority":["claude-opus-5; rm"]}"#,
            "not json",
        ] {
            assert_eq!(Priority::parse(bad), None, "{bad}");
        }
    }

    #[test]
    fn a_new_opus_is_inserted_above_the_family_best_only_when_the_build_knows_it() {
        let mut p = Priority::seed(0);
        let b = baked(
            &[
                "claude-opus-5-5",
                "claude-opus-5-6",
                "claude-fable-5-1",
                "claude-opus-5",
                "claude-sonnet-6",
            ],
            &[("opus", "claude-opus-5-6"), ("sonnet", "claude-sonnet-6")],
        );
        let recs = recommendations(&Evidence::default(), Some(&b), "2.1.290");
        let changes = ingest(&mut p, &recs, Some(&b), 5);
        assert_eq!(changes.len(), 1, "sonnet is not a family on the list");
        assert_eq!(changes[0].placed, "above claude-opus-5-5");
        assert_eq!(changes[0].source, "catalog:2.1.290");
        assert_eq!(
            p.ids,
            [
                "claude-opus-5-6",
                "claude-opus-5-5",
                "claude-fable-5-1",
                "claude-opus-5"
            ]
        );
        // Idempotent.
        assert!(ingest(&mut p, &recs, Some(&b), 6).is_empty());
    }

    #[test]
    fn recommendations_that_must_not_be_admitted() {
        let b = baked(
            &[
                "claude-opus-5-5",
                "claude-opus-5",
                "claude-opus-4-8",
                "claude-fable-5-1",
            ],
            &[],
        );
        let known = |id: &str| b.model(id).is_some();
        let mut p = Priority::seed(0);
        // Older than the family's best: a downgrade is never an "upgrade".
        assert_eq!(p.admit("claude-opus-4-8", "x", &known, 1), None);
        // Unknown to the build.
        assert_eq!(p.admit("claude-opus-6", "x", &known, 1), None);
        // Grammar.
        assert_eq!(p.admit("claude-opus-6;ls", "x", &|_| true, 1), None);
        // Already listed.
        assert_eq!(p.admit("claude-opus-5-5", "x", &known, 1), None);
        // A family not on the list.
        assert_eq!(p.admit("claude-mythos-5", "x", &|_| true, 1), None);
        // A hand-ordered list with the older member ranked first still never
        // receives an id older than its newest member.
        let mut hand = Priority {
            ids: vec!["claude-opus-5".into(), "claude-opus-5-5".into()],
            history: vec![],
        };
        assert_eq!(hand.admit("claude-opus-5-1", "x", &|_| true, 1), None);
        assert_eq!(p.ids, SEED);
    }

    #[test]
    fn availability_follows_the_served_catalog_and_the_deny_lists() {
        let served = r#"{"version":2,"catalog":{"config":{"models":[
            {"id":"claude-opus-5-5","section":"main","min_claude_code_version":"2.1.280","thinking":{"effort_options":[{"id":"low"},{"id":"xhigh"}]}},
            {"id":"claude-fable-5-1","section":"main","min_claude_code_version":"2.1.251"},
            {"id":"claude-opus-5","section":"overflow"},
            {"id":"claude-opus-4-1","section":"deprecated"}]}}}"#;
        let rows = parse_served(served).expect("parses");
        assert_eq!(rows[0].efforts, ["low", "xhigh"]);
        let mut ev = Evidence {
            served: Some(rows),
            ..Evidence::default()
        };
        let p = Priority::seed(0);
        let (best, _) = target(&p, "2.1.280", &ev, None);
        assert_eq!(best, Some("claude-opus-5-5"));
        // An older Claude Code cannot take Opus 5.5 (min 2.1.280).
        let (best, verdicts) = target(&p, "2.1.278", &ev, None);
        assert_eq!(best, Some("claude-fable-5-1"));
        assert_eq!(
            verdicts[0].1,
            Availability::No("needs Claude Code 2.1.280".into())
        );
        // Denied by the account.
        ev.denied = vec!["claude-opus-5-5".into(), "claude-fable-5-1".into()];
        assert_eq!(target(&p, "2.1.280", &ev, None).0, Some("claude-opus-5"));
        assert!(!availability("claude-opus-4-1", "2.1.280", &ev, None).ok());
        // No served catalog: the build's own catalog is the evidence; with
        // neither, nothing is available.
        let none = Evidence::default();
        assert_eq!(target(&p, "2.1.280", &none, None).0, None);
        let b = baked(&["claude-fable-5-1"], &[]);
        assert_eq!(
            target(&p, "2.1.280", &none, Some(&b)).0,
            Some("claude-fable-5-1")
        );
    }

    #[test]
    fn claude_json_yields_only_model_facts() {
        let text = r#"{"oauthAccount":{"emailAddress":"x@y"},
            "modelAccessCache":[{"apiName":"claude-opus-5","entitled":false},{"apiName":"claude-opus-5-5","entitled":true}],
            "additionalModelOptionsCache":[{"value":"claude-fable-5-1[1m]","label":"Fable","description":"d"},{"value":"claude-fable-5[1m]","disabled":true}],
            "cachedGrowthBookFeatures":{"tengu_startup_announcements":[{"id":"opus-5-5-launch","text":"Get to finished work sooner with Opus 5.5.","requiresModel":"claude-opus-5-5"},{"id":"bad","requiresModel":"rm -rf"}]}}"#;
        let (denied, disabled, ann) = parse_claude_json(text);
        assert_eq!(denied, ["claude-opus-5"]);
        assert_eq!(disabled, ["claude-fable-5"]);
        assert_eq!(ann.len(), 1);
        assert_eq!(ann[0].requires_model, "claude-opus-5-5");
        assert_eq!(ann[0].id, "opus-5-5-launch");
    }

    fn answer(model: &str) -> String {
        format!(
            r#"{{"type":"assistant","isSidechain":false,"message":{{"model":"{model}","content":[{{"type":"text","text":"x"}}]}}}}"#
        )
    }

    fn set_model(display: &str) -> String {
        format!(
            r#"{{"type":"user","message":{{"content":"<local-command-stdout>Set model to `{display}`</local-command-stdout>"}}}}"#
        )
    }

    #[test]
    fn the_live_model_is_a_later_model_command_else_the_last_answer() {
        let b = baked(
            &["claude-opus-5-5", "claude-fable-5-1", "claude-opus-5"],
            &[],
        );
        let tail = [answer("claude-opus-5"), answer("<synthetic>")].join("\n");
        assert_eq!(
            live_model(&tail, Some(&b)),
            Some(LiveModel {
                id: "claude-opus-5".into(),
                by_command: false
            }),
            "a synthetic row is not an answer"
        );
        let sub =
            r#"{"type":"assistant","isSidechain":true,"message":{"model":"claude-fable-5-1"}}"#;
        let tail = [answer("claude-opus-5"), sub.to_string()].join("\n");
        assert_eq!(
            live_model(&tail, Some(&b)).map(|l| l.id),
            Some("claude-opus-5".into())
        );
        // A /model after the last answer is what runs now, and it was CHOSEN.
        let tail = [answer("claude-opus-5"), set_model("claude-fable-5-1")].join("\n");
        assert_eq!(
            live_model(&tail, Some(&b)),
            Some(LiveModel {
                id: "claude-fable-5-1".into(),
                by_command: true
            })
        );
        // ...but an answer after it supersedes it.
        let tail = [set_model("claude-fable-5-1"), answer("claude-fable-5-1")].join("\n");
        assert!(!live_model(&tail, Some(&b)).expect("an answer").by_command);
        // A display the build does not know is not guessed.
        let tail = [answer("claude-opus-5"), set_model("Mystery 9")].join("\n");
        assert_eq!(live_model(&tail, Some(&b)), None);
    }

    /// Only Claude's own result row is a `/model` choice: the phrase quoted
    /// in an answer, a tool's output or a person's prompt is text — and the
    /// answer that quotes it is still an answer.
    #[test]
    fn a_quoted_model_result_is_text_not_a_choice() {
        let b = baked(
            &["claude-opus-5-5", "claude-fable-5-1", "claude-opus-5"],
            &[],
        );
        let quoting_answer = r#"{"type":"assistant","isSidechain":false,"message":{"model":"claude-opus-5-5","content":[{"type":"text","text":"it printed Set model to `claude-fable-5-1` there"}]}}"#;
        let tool_output = r#"{"type":"user","message":{"content":[{"type":"tool_result","content":"<local-command-stdout>Set model to `claude-fable-5-1`</local-command-stdout>"}]}}"#;
        let prompt = r#"{"type":"user","message":{"content":"why did it say Set model to `claude-fable-5-1`?"}}"#;
        let notification = r#"{"type":"user","message":{"content":"<task-notification>... Set model to `claude-fable-5-1` ...</task-notification>"}}"#;
        let tail = [answer("claude-opus-5"), quoting_answer.to_string()].join("\n");
        assert_eq!(
            live_model(&tail, Some(&b)),
            Some(LiveModel {
                id: "claude-opus-5-5".into(),
                by_command: false
            }),
            "the quoting answer is the newest answer"
        );
        for text in [tool_output, prompt, notification] {
            let tail = [answer("claude-opus-5"), text.to_string()].join("\n");
            assert_eq!(
                live_model(&tail, Some(&b)),
                Some(LiveModel {
                    id: "claude-opus-5".into(),
                    by_command: false
                }),
                "{text}"
            );
        }
        // The older builds' ANSI-bold result is still a result.
        let bold = "{\"type\":\"user\",\"message\":{\"content\":\"<local-command-stdout>Set model to \\u001b[1mclaude-fable-5-1\\u001b[22m and saved as your default</local-command-stdout>\"}}";
        let tail = [answer("claude-opus-5"), bold.to_string()].join("\n");
        assert_eq!(
            live_model(&tail, Some(&b)),
            Some(LiveModel {
                id: "claude-fable-5-1".into(),
                by_command: true
            })
        );
    }

    fn ids(xs: &[&str]) -> Vec<String> {
        xs.iter().map(|x| (*x).to_string()).collect()
    }

    /// THE SAME-FAMILY RESOLVER, one row per case the rule names.
    #[test]
    fn the_resolver_moves_to_the_newest_of_the_same_family_and_nowhere_else() {
        let offered = ids(&[
            "claude-opus-5-5",
            "claude-fable-5-1",
            "claude-sonnet-5-5",
            "claude-opus-5",
        ]);
        let next = |current: &str, one_m: bool| family_successor(current, one_m, &offered);
        // The incident: Opus 5 -> Opus 5.5.
        assert_eq!(
            next("claude-opus-5", false).as_deref(),
            Some("claude-opus-5-5")
        );
        // The 1M window rides the move, whether the id carried it or the
        // caller says so; a tag on an offered id is not what decides it.
        assert_eq!(
            next("claude-opus-5[1m]", true).as_deref(),
            Some("claude-opus-5-5[1m]")
        );
        assert_eq!(
            family_successor("claude-opus-5", false, &ids(&["claude-opus-5-5[1m]"])).as_deref(),
            Some("claude-opus-5-5")
        );
        // An older major of the family moves too; a dated id is its version.
        assert_eq!(
            next("claude-opus-4-1", false).as_deref(),
            Some("claude-opus-5-5")
        );
        assert_eq!(
            next("claude-sonnet-4-20250514", false).as_deref(),
            Some("claude-sonnet-5-5")
        );
        // The NEWEST of the family, whatever order the candidates come in.
        assert_eq!(
            family_successor(
                "claude-opus-5",
                false,
                &ids(&["claude-opus-5-5", "claude-opus-5-6", "claude-opus-5-1"])
            )
            .as_deref(),
            Some("claude-opus-5-6")
        );
        // NEVER ACROSS FAMILIES, whatever else is on offer.
        assert_eq!(
            family_successor("claude-sonnet-5", false, &ids(&["claude-opus-5-5"])),
            None
        );
        assert_eq!(
            family_successor(
                "claude-opus-5",
                false,
                &ids(&["claude-fable-5-1", "claude-haiku-4-5"])
            ),
            None
        );
        // NEVER DOWN, NEVER SIDEWAYS.
        assert_eq!(next("claude-opus-5-5", false), None, "already the newest");
        assert_eq!(
            next("claude-opus-6", false),
            None,
            "newer than anything offered"
        );
        assert_eq!(
            family_successor(
                "claude-haiku-4-5-20251001",
                false,
                &ids(&["claude-haiku-4-5"])
            ),
            None,
            "the dated and undated ids are one version"
        );
        // NEVER GUESSED: an alias, a provider's id, an id from before the
        // grammar, a shape this reader does not know, another vendor's.
        for current in [
            "opus",
            "sonnet[1m]",
            "us.anthropic.claude-opus-5-v1:0",
            "claude-3-5-sonnet-20241022",
            "claude-opus-5-5-v2",
            "gpt-5.5",
            "",
        ] {
            assert_eq!(next(current, false), None, "{current:?}");
        }
        assert_eq!(family_successor("claude-opus-5", false, &[]), None);
    }

    /// WHERE "NEWEST OF THE FAMILY" COMES FROM: the INSTALLED build's own
    /// `latest_per_family` (so a family the list never named — Sonnet here —
    /// still moves) and the list, each only when the build knows it and the
    /// account may run it. Nothing the build does not know is ever offered.
    #[test]
    fn what_is_offered_is_what_the_installed_build_knows_and_the_person_allows() {
        let list = Priority::seed(0);
        let b = baked(
            &[
                "claude-opus-5-5",
                "claude-fable-5-1",
                "claude-opus-5",
                "claude-sonnet-5-5",
                "claude-sonnet-5",
            ],
            &[
                ("opus", "claude-opus-5-5"),
                ("sonnet", "claude-sonnet-5-5"),
                ("fable", "claude-fable-5-1"),
            ],
        );
        let none = Evidence::default();
        let got = offered(&list, "2.1.283", &none, Some(&b), None);
        assert_eq!(
            got,
            [
                "claude-opus-5-5",
                "claude-sonnet-5-5",
                "claude-fable-5-1",
                "claude-opus-5"
            ],
            "the build's newest of each family first, then the list, each once"
        );
        assert_eq!(
            family_successor("claude-sonnet-5", false, &got).as_deref(),
            Some("claude-sonnet-5-5"),
            "a family off the list moves by the build's own word"
        );
        // A listed id the build does not know is not offered (never guessed).
        let hand = Priority {
            ids: ids(&["claude-opus-5-6", "claude-opus-5-5"]),
            history: vec![],
        };
        assert!(
            !offered(&hand, "2.1.283", &none, Some(&b), None).contains(&"claude-opus-5-6".into())
        );
        // Denied for the account, or outside the person's availableModels.
        let denied = Evidence {
            denied: ids(&["claude-opus-5-5"]),
            ..Evidence::default()
        };
        let got = offered(&list, "2.1.283", &denied, Some(&b), None);
        assert!(!got.contains(&"claude-opus-5-5".into()), "{got:?}");
        assert_eq!(family_successor("claude-opus-5", false, &got), None);
        let only = ids(&["claude-opus-5-5[1m]"]);
        assert_eq!(
            offered(&list, "2.1.283", &none, Some(&b), Some(&only)),
            ["claude-opus-5-5"]
        );
        // No catalog at all: nothing is offered.
        assert!(offered(&list, "2.1.283", &none, None, None).is_empty());
    }

    /// STEP 1, SAME FAMILY FIRST: whatever the list ranks, a newer model of the
    /// conversation's own family on offer is the move.
    #[test]
    fn the_model_rule_moves_within_the_family_first() {
        let list = Priority::seed(0);
        let offer = ids(&[
            "claude-opus-5-5",
            "claude-fable-5-1",
            "claude-sonnet-5-5",
            "claude-opus-5",
        ]);
        let live = |id: &str, by_command: bool| LiveModel {
            id: id.into(),
            by_command,
        };
        let none = ModelRecord::default();
        let due = |from: &str, to: &str| ModelVerdict::Due {
            from: from.into(),
            to: to.into(),
        };
        let rule = |l: &str, by_command: bool, launch: Option<&str>| {
            model_due(
                &list,
                Some(&live(l, by_command)),
                &offer,
                launch,
                None,
                &none,
            )
        };
        // THE INCIDENT (2026-09-25): no --model, the transcript on Opus 5, the
        // build's newest Opus is Opus 5.5.
        assert_eq!(
            rule("claude-opus-5", false, None),
            due("claude-opus-5", "claude-opus-5-5")
        );
        // An EXACT id the launch named moves the same way (a pin cannot be
        // told from a stale one), and so does a /model and a saved default.
        assert_eq!(
            rule("claude-opus-5", false, Some("claude-opus-5")),
            due("claude-opus-5", "claude-opus-5-5")
        );
        assert_eq!(
            rule("claude-opus-5", true, None),
            due("claude-opus-5", "claude-opus-5-5")
        );
        for default in ["claude-opus-5", "opus"] {
            assert_eq!(
                model_due(
                    &list,
                    Some(&live("claude-opus-5", false)),
                    &offer,
                    None,
                    Some(default),
                    &none
                ),
                due("claude-opus-5", "claude-opus-5-5"),
                "{default}"
            );
        }
        // A family the list never named moves by the build's own newest.
        assert_eq!(
            rule("claude-sonnet-5", false, None),
            due("claude-sonnet-5", "claude-sonnet-5-5")
        );
        // NEGATIVE CONTROL for the order: a hand list that ranks Fable 5.1
        // ABOVE Opus 5.5 — the list's step alone would take Opus 5 to Fable.
        // A newer Opus is on offer, so the move stays in the family.
        let fable_first = Priority {
            ids: ids(&["claude-fable-5-1", "claude-opus-5-5", "claude-opus-5"]),
            history: vec![],
        };
        assert_eq!(list_target(&fable_first, &offer), Some("claude-fable-5-1"));
        assert_eq!(
            model_due(
                &fable_first,
                Some(&live("claude-opus-5", false)),
                &offer,
                None,
                None,
                &none
            ),
            due("claude-opus-5", "claude-opus-5-5")
        );
        // THE 1M WINDOW: kept from the live id, or from the launch flag the
        // transcript's bare API id stands for — but not from a launch of
        // another family, nor once a /model chose since.
        assert_eq!(
            rule("claude-opus-5[1m]", true, None),
            due("claude-opus-5[1m]", "claude-opus-5-5[1m]")
        );
        assert_eq!(
            rule("claude-opus-5", false, Some("claude-opus-5[1m]")),
            due("claude-opus-5", "claude-opus-5-5[1m]")
        );
        assert_eq!(
            rule("claude-opus-5", false, Some("claude-sonnet-5[1m]")),
            due("claude-opus-5", "claude-opus-5-5")
        );
        assert_eq!(
            rule("claude-opus-5", true, Some("claude-opus-5[1m]")),
            due("claude-opus-5", "claude-opus-5-5")
        );
        // Already the newest of its family, and the newest ranks first: no
        // move, and never down or sideways.
        assert_eq!(
            rule("claude-opus-5-5[1m]", false, None),
            ModelVerdict::Keep("model-current")
        );
        // AN ALIAS IS KEPT: it already names the newest of its family, by the
        // build's own table, and the relaunch carries it verbatim. So is any
        // launch id this grammar does not read.
        for alias in ["opus", "sonnet[1m]", "us.anthropic.claude-opus-5-v1:0"] {
            assert_eq!(
                rule("claude-opus-5", false, Some(alias)),
                ModelVerdict::Keep("model-alias"),
                "{alias}"
            );
        }
        // UNKNOWN IDS ARE UNTOUCHED.
        assert_eq!(
            rule("us.anthropic.claude-opus-5-v1:0", false, None),
            ModelVerdict::Keep("model-unreadable")
        );
        assert_eq!(
            model_due(&list, None, &offer, None, None, &none),
            ModelVerdict::Keep("model-unknown")
        );
        assert_eq!(
            model_due(
                &list,
                Some(&live("claude-opus-5", false)),
                &[],
                None,
                None,
                &none
            ),
            ModelVerdict::Keep("no-model-available")
        );
        // Never twice, never after a failure — and a same-family move spent
        // that way never falls through to the list's step (Fable here).
        let applied = ModelRecord {
            applied: ids(&["claude-opus-5-5"]),
            ..ModelRecord::default()
        };
        assert_eq!(
            model_due(
                &list,
                Some(&live("claude-opus-5", false)),
                &offer,
                None,
                None,
                &applied
            ),
            ModelVerdict::Keep("model-applied-before")
        );
        let failed = ModelRecord {
            failed: ids(&["claude-opus-5-5[1m]"]),
            ..ModelRecord::default()
        };
        assert_eq!(
            model_due(
                &list,
                Some(&live("claude-opus-5", false)),
                &offer,
                None,
                None,
                &failed
            ),
            ModelVerdict::Keep("model-failed-before")
        );
    }

    /// STEP 2, THEN THE PRIORITY LIST (owner, 2026-09-27; the list is the
    /// owner's of 2026-09-23): only when nothing newer of its family is on
    /// offer, a model NOBODY CHOSE moves UP the list across families — and a
    /// deliberate choice never does.
    #[test]
    fn with_nothing_newer_in_its_family_the_list_moves_only_a_model_nobody_chose() {
        let list = Priority::seed(0);
        let live = |id: &str, by_command: bool| LiveModel {
            id: id.into(),
            by_command,
        };
        let none = ModelRecord::default();
        let due = |from: &str, to: &str| ModelVerdict::Due {
            from: from.into(),
            to: to.into(),
        };
        // Opus 5.5 is NOT on offer (denied, say): nothing newer of Opus 5's
        // family, and the list ranks Fable 5.1 above Opus 5.
        let no_opus55 = ids(&["claude-fable-5-1", "claude-opus-5"]);
        assert_eq!(
            model_due(
                &list,
                Some(&live("claude-opus-5", false)),
                &no_opus55,
                None,
                None,
                &none
            ),
            due("claude-opus-5", "claude-fable-5-1")
        );
        // The 2026-09-23 case: a Fable 5.1 nobody chose, the newest Fable
        // there is, moves up to the Opus 5.5 the list ranks above it.
        let offer = ids(&["claude-opus-5-5", "claude-fable-5-1", "claude-opus-5"]);
        let fable = |by_command: bool| live("claude-fable-5-1", by_command);
        assert_eq!(
            model_due(&list, Some(&fable(false)), &offer, None, None, &none),
            due("claude-fable-5-1", "claude-opus-5-5")
        );
        // ...and so does one the harness itself put there (its own earlier
        // `--model`, the record's `set`), and one whose saved default is of
        // ANOTHER family.
        let ours = ModelRecord {
            set: "claude-fable-5-1".into(),
            ..ModelRecord::default()
        };
        for (by_command, launch) in [(false, Some("claude-fable-5-1")), (true, None)] {
            assert_eq!(
                model_due(&list, Some(&fable(by_command)), &offer, launch, None, &ours),
                due("claude-fable-5-1", "claude-opus-5-5"),
                "the harness's own model, by_command={by_command}"
            );
        }
        assert_eq!(
            model_due(
                &list,
                Some(&fable(false)),
                &offer,
                None,
                Some("opus"),
                &none
            ),
            due("claude-fable-5-1", "claude-opus-5-5")
        );
        // A DELIBERATE CHOICE never moves across: a launch --model the harness
        // did not put there, a /model now, a /model remembered past its
        // answers, the saved default of the live model's family (an id or an
        // alias) — and a launch alias, kept as it is.
        let remembered = ModelRecord {
            human: "claude-fable-5-1".into(),
            ..ModelRecord::default()
        };
        let chosen: [(bool, Option<&str>, Option<&str>, &ModelRecord); 5] = [
            (false, Some("claude-fable-5-1"), None, &none),
            (true, None, None, &none),
            (false, None, None, &remembered),
            (false, None, Some("claude-fable-5-1"), &none),
            (false, None, Some("fable"), &none),
        ];
        for (by_command, launch, default, record) in chosen {
            assert_eq!(
                model_due(
                    &list,
                    Some(&fable(by_command)),
                    &offer,
                    launch,
                    default,
                    record
                ),
                ModelVerdict::Keep("model-chosen-by-hand"),
                "by_command={by_command} launch={launch:?} default={default:?}"
            );
        }
        assert_eq!(
            model_due(
                &list,
                Some(&fable(false)),
                &offer,
                Some("fable"),
                None,
                &none
            ),
            ModelVerdict::Keep("model-alias")
        );
        // NEVER DOWN. The live model ranks first: nothing above it.
        assert_eq!(
            model_due(
                &list,
                Some(&live("claude-opus-5-5", false)),
                &no_opus55,
                None,
                None,
                &none
            ),
            ModelVerdict::Keep("model-ranks-higher")
        );
        // A hand list that ranks an OLDER model of the live model's own family
        // first: that is a step down, never taken.
        let older_first = Priority {
            ids: ids(&["claude-opus-5", "claude-opus-5-5", "claude-fable-5-1"]),
            history: vec![],
        };
        assert_eq!(
            model_due(
                &older_first,
                Some(&live("claude-opus-5-5", false)),
                &ids(&["claude-opus-5", "claude-fable-5-1"]),
                None,
                None,
                &none
            ),
            ModelVerdict::Keep("model-current")
        );
        // A model the list does not name is left alone.
        assert_eq!(
            model_due(
                &list,
                Some(&live("claude-sonnet-5", false)),
                &offer,
                None,
                None,
                &none
            ),
            ModelVerdict::Keep("model-off-list")
        );
        // Nothing of another family on offer: nothing moves.
        assert_eq!(
            model_due(
                &list,
                Some(&live("claude-opus-5", false)),
                &ids(&["claude-opus-5"]),
                None,
                None,
                &none
            ),
            ModelVerdict::Keep("model-current")
        );
        // Never twice, never after a failure, for the list's target too.
        for record in [
            ModelRecord {
                applied: ids(&["claude-opus-5-5"]),
                ..ModelRecord::default()
            },
            ModelRecord {
                failed: ids(&["claude-opus-5-5"]),
                ..ModelRecord::default()
            },
        ] {
            assert!(matches!(
                model_due(&list, Some(&fable(false)), &offer, None, None, &record),
                ModelVerdict::Keep("model-applied-before" | "model-failed-before")
            ));
        }
    }

    /// THE LIST'S STEP OBEYS THE SAME AVAILABILITY as the family's, because it
    /// reads the same candidates ([`offered`]): an entry the build does not
    /// know, one the account is denied, one outside `availableModels` is never
    /// its target.
    #[test]
    fn the_lists_step_reads_the_same_offered_set_as_the_familys() {
        let list = Priority::seed(0);
        let b = baked(
            &["claude-opus-5-5", "claude-fable-5-1", "claude-opus-5"],
            &[("opus", "claude-opus-5-5"), ("fable", "claude-fable-5-1")],
        );
        let opus5 = LiveModel {
            id: "claude-opus-5".into(),
            by_command: false,
        };
        let rule = |ev: &Evidence, allowed: Option<&[String]>| {
            let offer = offered(&list, "2.1.283", ev, Some(&b), allowed);
            model_due(
                &list,
                Some(&opus5),
                &offer,
                None,
                None,
                &ModelRecord::default(),
            )
        };
        let deny = |xs: &[&str]| Evidence {
            denied: ids(xs),
            ..Evidence::default()
        };
        // Opus 5.5 denied: the list's step takes Fable 5.1 ...
        assert_eq!(
            rule(&deny(&["claude-opus-5-5"]), None),
            ModelVerdict::Due {
                from: "claude-opus-5".into(),
                to: "claude-fable-5-1".into()
            }
        );
        // ... unless Fable is denied too, or not in `availableModels`.
        assert_eq!(
            rule(&deny(&["claude-opus-5-5", "claude-fable-5-1"]), None),
            ModelVerdict::Keep("model-current")
        );
        let only_opus = ids(&["claude-opus-5", "claude-opus-5-5"]);
        assert_eq!(
            rule(&deny(&["claude-opus-5-5"]), Some(&only_opus)),
            ModelVerdict::Keep("model-current")
        );
        // A listed id the build does not know is never the list's target.
        // NEGATIVE CONTROL: the build that knows it takes it.
        let hand = Priority {
            ids: ids(&["claude-sonnet-6", "claude-opus-5"]),
            history: vec![],
        };
        let only_opus5 = baked(&["claude-opus-5"], &[("opus", "claude-opus-5")]);
        let knows = baked(
            &["claude-opus-5", "claude-sonnet-6"],
            &[("opus", "claude-opus-5")],
        );
        let none = Evidence::default();
        let with = |bk: &Baked| {
            let offer = offered(&hand, "2.1.283", &none, Some(bk), None);
            model_due(
                &hand,
                Some(&opus5),
                &offer,
                None,
                None,
                &ModelRecord::default(),
            )
        };
        assert_eq!(with(&only_opus5), ModelVerdict::Keep("model-current"));
        assert_eq!(
            with(&knows),
            ModelVerdict::Due {
                from: "claude-opus-5".into(),
                to: "claude-sonnet-6".into()
            }
        );
        // The Tier-1 bind's reader IS this step's target.
        assert_eq!(
            target_allowed(
                &list,
                "2.1.283",
                &deny(&["claude-opus-5-5"]),
                Some(&b),
                None
            )
            .as_deref(),
            Some("claude-fable-5-1")
        );
    }

    /// WHY, in the notices: a move within the family is the family's, a move
    /// across it can only be the list's, and an id outside the grammar says
    /// neither.
    #[test]
    fn the_reason_a_move_is_said_with_is_read_from_the_two_ids() {
        assert_eq!(
            move_why(Some("claude-opus-5"), "claude-opus-5-5[1m]"),
            MoveWhy::Family
        );
        assert_eq!(
            move_why(Some("claude-fable-5-1"), "claude-opus-5-5"),
            MoveWhy::List
        );
        assert_eq!(move_why(None, "claude-opus-5-5"), MoveWhy::Unknown);
        assert_eq!(move_why(Some("opus"), "claude-opus-5-5"), MoveWhy::Unknown);
        assert!(MoveWhy::List.words().contains("priority list"));
        assert!(
            MoveWhy::Family
                .words()
                .contains("newest model of its family")
        );
    }

    #[test]
    fn a_model_record_round_trips_and_refuses_garbage() {
        let r = ModelRecord {
            applied: vec!["claude-opus-5-5".into()],
            set: "claude-opus-5-5".into(),
            set_at: 42,
            failed: vec!["claude-fable-5-1".into()],
            human: "claude-opus-5".into(),
            last_seq: 7,
            seq_since_s: 9,
            due_to: "claude-opus-5-5".into(),
            due_since: 11,
        };
        assert_eq!(ModelRecord::parse(&r.render()), Some(r));
        assert_eq!(ModelRecord::parse("not json"), None);
        let hostile = r#"{"applied":["a b; rm -rf ~"],"set":"x y","due_to":"$(boom)"}"#;
        let got = ModelRecord::parse(hostile).expect("parses");
        assert!(
            got.applied.is_empty() && got.set.is_empty() && got.due_to.is_empty(),
            "{got:?}"
        );
    }

    /// AN ASK THAT RAN STANDS (skeptic review of `HarnessModelSwitch`,
    /// 2026-09-27), on the pure settle step. The harness asks for
    /// `claude-opus-5-5`, the relaunch runs it, and the settle step records it
    /// APPLIED. A person then types `/model claude-fable-5-1`, and a visit
    /// comes after `MODEL_SETTLE_S`. Before the fix the failed arm did not look
    /// at `applied`: the ask that ran was recorded FAILED too (a false
    /// `model-failed` after its own `model-verified`) and cleared. NEGATIVE
    /// CONTROL: an ask that never ran is still recorded failed.
    #[test]
    fn the_settle_step_never_fails_an_ask_that_ran() {
        let asked_at = 1_000_000;
        let mut rec = ModelRecord::default();
        rec.asked("claude-opus-5-5", asked_at);
        let ran = LiveModel {
            id: "claude-opus-5-5".into(),
            by_command: false,
        };
        let verified = rec.settle(Some(&ran), asked_at + 20);
        assert_eq!(verified.verified.as_deref(), Some("claude-opus-5-5"));
        let moved = LiveModel {
            id: "claude-fable-5-1".into(),
            by_command: true,
        };
        let later = rec.settle(Some(&moved), asked_at + MODEL_SETTLE_S + 1);
        assert_eq!(later.failed, None, "an ask that ran was recorded failed");
        assert!(rec.failed.is_empty(), "{rec:?}");
        assert_eq!(
            rec.set, "claude-opus-5-5",
            "the ask stays the harness's own"
        );
        assert_eq!(
            rec.human, "claude-fable-5-1",
            "the person's choice remembered"
        );

        let mut never = ModelRecord::default();
        never.asked("claude-opus-5-5", asked_at);
        let other = LiveModel {
            id: "claude-opus-5".into(),
            by_command: false,
        };
        let failed = never.settle(Some(&other), asked_at + MODEL_SETTLE_S);
        assert_eq!(failed.failed.as_deref(), Some("claude-opus-5-5"));
        assert!(never.set.is_empty() && never.failed == ["claude-opus-5-5"]);
    }

    /// A record an OLDER harness wrote has no due clock. It must parse, with
    /// nothing due — so the first due visit of this harness starts the clock
    /// rather than reading a zero as "due since 1970" and moving at once.
    #[test]
    fn a_record_without_the_due_clock_parses_as_nothing_due() {
        let old = r#"{"applied":[],"set":"","set_at":0,"failed":[],"human":"","last_seq":3,"seq_since_s":4}"#;
        let got = ModelRecord::parse(old).expect("an older record still parses");
        assert!(got.due_to.is_empty() && got.due_since == 0, "{got:?}");
        assert_eq!(got.last_seq, 3);
    }

    /// THE MODEL LADDER's truth table — every due move lands.
    ///
    /// The NEGATIVE CONTROL is the incident itself (2026-09-25): a conversation
    /// in active use (warm), being restarted onto a newer build anyway, with a
    /// better model due for moments. The rule this replaces moved only on a cold
    /// cache, so it relaunched that session on `claude-opus-5` and would have
    /// logged `wait:model-cache-warm` for as long as the owner kept working.
    #[test]
    fn the_model_ladder_always_lands() {
        // The incident: warm, a build restart happening, due for 0 s.
        let old_rule = |cold: bool| cold;
        assert!(
            !old_rule(false),
            "control: the replaced rule really did refuse the incident's move"
        );
        assert_eq!(model_moves_now(false, true, 0), Some("build-restart"));

        // Cold is free, whatever else is true.
        assert_eq!(model_moves_now(true, false, 0), Some("cache-cold"));
        assert_eq!(model_moves_now(true, true, 0), Some("cache-cold"));

        // Warm with no restart coming: a bounded preference, not a condition.
        assert_eq!(model_moves_now(false, false, 0), None);
        assert_eq!(model_moves_now(false, false, MODEL_WARM_MAX_S - 1), None);
        assert_eq!(
            model_moves_now(false, false, MODEL_WARM_MAX_S),
            Some("warm-wait-over")
        );

        // MONOTONE: once a visit moves, every later visit (more time due) moves.
        for cold in [false, true] {
            for restart in [false, true] {
                let mut moved = false;
                for due in (0..=2 * MODEL_WARM_MAX_S).step_by(60) {
                    let now = model_moves_now(cold, restart, due).is_some();
                    assert!(
                        !moved || now,
                        "the ladder took a move back at due={due} cold={cold} restart={restart}"
                    );
                    moved |= now;
                }
                assert!(
                    moved,
                    "never landed: cold={cold} restart={restart} — an unbounded wait"
                );
            }
        }
    }

    #[test]
    fn utc_stamps_and_the_last_answer() {
        assert_eq!(parse_utc("1970-01-01T00:00:00.000Z"), Some(0));
        assert_eq!(parse_utc("2026-09-24T04:18:02.796Z"), Some(1_790_223_482));
        assert_eq!(parse_utc("2026-13-01T00:00:00Z"), None);
        assert_eq!(parse_utc("garbage"), None);
        // An offset names a different instant than its digits: refused, never
        // read as UTC (this reader's private copy took it as 04:18:02Z).
        assert_eq!(parse_utc("2026-09-24T04:18:02+05:30"), None);
        assert_eq!(parse_utc("2026-09-24T04:18:02.796"), None);
        assert_eq!(parse_utc("2026-09-24T04:18:02Z"), Some(1_790_223_482));
        let a = |ts: &str, side: bool| {
            format!(
                r#"{{"type":"assistant","isSidechain":{side},"timestamp":"{ts}","message":{{"model":"claude-opus-5-5"}}}}"#
            )
        };
        let tail = [
            a("2026-09-24T04:18:02.796Z", false),
            a("2026-09-24T05:00:00.000Z", true),
        ]
        .join("\n");
        assert_eq!(
            last_answer_at(&tail),
            Some(1_790_223_482),
            "a subagent is not the conversation"
        );
        assert_eq!(last_answer_at(""), None);
    }

    #[test]
    fn a_resumed_process_runs_its_launch_model_until_it_answers() {
        assert_eq!(
            parse_lstart("Thu Sep 24 23:21:50 2026"),
            parse_utc("2026-09-24T23:21:50Z")
        );
        assert_eq!(
            parse_lstart("Thu Sep  4 03:01:00 2026"),
            parse_utc("2026-09-04T03:01:00Z")
        );
        assert_eq!(parse_lstart("garbage"), None);
        let old = r#"{"type":"assistant","isSidechain":false,"timestamp":"2026-09-24T03:08:57.061Z","message":{"model":"claude-opus-5-5"}}"#;
        let started = parse_utc("2026-09-24T23:00:00Z");
        // Resumed with --model claude-opus-5 after the last answer: that runs.
        assert_eq!(
            live_model_at(old, None, Some("claude-opus-5"), started).map(|l| l.id),
            Some("claude-opus-5".into())
        );
        // An answer from THIS process names what runs.
        let started_before = parse_utc("2026-09-24T01:00:00Z");
        assert_eq!(
            live_model_at(old, None, Some("claude-opus-5"), started_before).map(|l| l.id),
            Some("claude-opus-5-5".into())
        );
        // An alias is not a model id: the transcript stands.
        assert_eq!(
            live_model_at(old, None, Some("opus"), started).map(|l| l.id),
            Some("claude-opus-5-5".into())
        );
    }

    /// AN EXPLICIT PIN, as designed: a person's exact id — a launch
    /// `--model`, a `/model`, the saved default Claude Code's `/model` writes —
    /// cannot be told from one made before the newer model existed, so it
    /// moves within its FAMILY like any other; the person's ANSWER is what is
    /// kept. Moved once and moved back by hand, the conversation stays where
    /// they put it — and the list's step never takes it elsewhere instead. A
    /// person's pick is never moved ACROSS families, however the list ranks
    /// it.
    #[test]
    fn a_pin_moves_within_its_family_once_and_a_move_back_by_hand_stands() {
        let list = Priority::seed(0);
        let offer = vec![
            "claude-opus-5-5".to_string(),
            "claude-fable-5-1".to_string(),
        ];
        let live = |id: &str, by_command: bool| LiveModel {
            id: id.into(),
            by_command,
        };
        let fresh = ModelRecord::default();
        assert_eq!(
            model_due(
                &list,
                Some(&live("claude-opus-5", false)),
                &offer,
                Some("claude-opus-5"),
                None,
                &fresh
            ),
            ModelVerdict::Due {
                from: "claude-opus-5".into(),
                to: "claude-opus-5-5".into()
            }
        );
        // The harness moved it once; the person then typed `/model` back to
        // Opus 5 (a `/model` newer than the last answer: `by_command`), and
        // later answered on it: neither is moved again, by either step.
        let moved = ModelRecord {
            applied: vec!["claude-opus-5-5".into()],
            set: "claude-opus-5-5".into(),
            human: "claude-opus-5".into(),
            ..ModelRecord::default()
        };
        for by_command in [true, false] {
            assert_eq!(
                model_due(
                    &list,
                    Some(&live("claude-opus-5", by_command)),
                    &offer,
                    Some("claude-opus-5-5"),
                    None,
                    &moved
                ),
                ModelVerdict::Keep("model-applied-before"),
                "by_command={by_command}"
            );
        }
        // A person's Fable, by /model or by launch, is never moved to the
        // Opus the list ranks above it.
        for (by_command, launch) in [(true, None), (false, Some("claude-fable-5-1"))] {
            assert_eq!(
                model_due(
                    &list,
                    Some(&live("claude-fable-5-1", by_command)),
                    &offer,
                    launch,
                    None,
                    &fresh
                ),
                ModelVerdict::Keep("model-chosen-by-hand")
            );
        }
    }

    #[test]
    fn the_allowlist_limits_the_target_and_the_settings_parse_fail_closed() {
        let list = Priority::seed(0);
        let b = baked(
            &["claude-opus-5-5", "claude-fable-5-1", "claude-opus-5"],
            &[],
        );
        let ev = Evidence::default();
        assert_eq!(
            target_allowed(&list, "2.1.282", &ev, Some(&b), None).as_deref(),
            Some("claude-opus-5-5")
        );
        let only_fable = ["claude-fable-5-1".to_string()];
        assert_eq!(
            target_allowed(&list, "2.1.282", &ev, Some(&b), Some(&only_fable)).as_deref(),
            Some("claude-fable-5-1")
        );
        assert_eq!(
            target_allowed(&list, "2.1.282", &ev, Some(&b), Some(&[])),
            None
        );
        let u = parse_user_settings(
            r#"{"alwaysThinkingEnabled":true,"model":"opus","availableModels":["claude-opus-5-5","a b"]}"#,
        );
        assert_eq!(u.default.as_deref(), Some("opus"));
        assert_eq!(u.allowed, Some(vec!["claude-opus-5-5".to_string()]));
        assert_eq!(parse_user_settings("nope"), UserModelSettings::default());
    }
}
