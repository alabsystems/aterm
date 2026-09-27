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

/// Unix seconds of an RFC 3339 UTC stamp (`2026-09-24T04:18:02.796Z`).
#[must_use]
pub fn parse_utc(s: &str) -> Option<u64> {
    let b = s.as_bytes();
    if b.len() < 20 || b[4] != b'-' || b[7] != b'-' || b[10] != b'T' || b[13] != b':' {
        return None;
    }
    let n = |r: std::ops::Range<usize>| s.get(r)?.parse::<i64>().ok();
    let (y, mo, d) = (n(0..4)?, n(5..7)?, n(8..10)?);
    let (h, mi, se) = (n(11..13)?, n(14..16)?, n(17..19)?);
    if !(1..=12).contains(&mo) || !(1..=31).contains(&d) || h > 23 || mi > 59 || se > 60 {
        return None;
    }
    // Days from civil (Howard Hinnant's algorithm).
    let y = if mo <= 2 { y - 1 } else { y };
    let era = y.div_euclid(400);
    let yoe = y - era * 400;
    let mp = (mo + 9) % 12;
    let doy = (153 * mp + 2) / 5 + d - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    let days = era * 146_097 + doe - 719_468;
    u64::try_from(days * 86_400 + h * 3600 + mi * 60 + se).ok()
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
    /// `model`: the default new sessions start on (an id or a family alias).
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

/// THE TARGET the live upgrade moves to: the first entry of the list that is
/// available ([`target`]) AND allowed by the person's `availableModels`.
#[must_use]
pub fn target_allowed(
    list: &Priority,
    cc_version: &str,
    ev: &Evidence,
    baked: Option<&Baked>,
    allowed: Option<&[String]>,
) -> Option<String> {
    let (_, verdicts) = target(list, cc_version, ev, baked);
    verdicts
        .into_iter()
        .find(|(id, a)| {
            a.ok()
                && allowed.is_none_or(|ok| {
                    ok.iter()
                        .any(|x| x == id || x.strip_suffix("[1m]") == Some(id))
                })
        })
        .map(|(id, _)| id.to_string())
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
    /// it: that choice is moved only within its family.
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
        /// The list's best available model.
        to: String,
    },
    /// Leave it, and why (one word for the report).
    Keep(&'static str),
}

/// THE MODEL RULE: move a conversation to the list's best available model
/// when that model ranks ABOVE the live one — never down, never sideways, never
/// onto a model this harness already applied or saw fail for it, and never
/// off a model not on the list (a person's own pick). A person's choice (a
/// launch `--model` this harness did not put there, or a `/model` it did not
/// type) is moved only within its FAMILY: Opus 5 → Opus 5.5, never Opus →
/// Fable.
#[must_use]
pub fn model_due(
    list: &Priority,
    live: Option<&LiveModel>,
    target: Option<&str>,
    launch_model: Option<&str>,
    default_model: Option<&str>,
    record: &ModelRecord,
) -> ModelVerdict {
    let Some(target) = target else {
        return ModelVerdict::Keep("no-model-available");
    };
    let Some(live) = live else {
        return ModelVerdict::Keep("model-unknown");
    };
    let base = |m: &str| m.strip_suffix("[1m]").unwrap_or(m).to_string();
    if base(&live.id) == base(target) {
        return ModelVerdict::Keep("model-current");
    }
    let (Some(rl), Some(rt)) = (list.rank(&live.id), list.rank(target)) else {
        return ModelVerdict::Keep("model-off-list");
    };
    if rt >= rl {
        return ModelVerdict::Keep("model-ranks-higher");
    }
    if record.applied.iter().any(|a| base(a) == base(target)) {
        return ModelVerdict::Keep("model-applied-before");
    }
    if record.failed.iter().any(|a| base(a) == base(target)) {
        return ModelVerdict::Keep("model-failed-before");
    }
    let ours = |m: &str| !record.set.is_empty() && base(m) == base(&record.set);
    let family = |m: &str| {
        family_version(&base(m))
            .map(|f| f.0)
            .or_else(|| Some(base(m).to_ascii_lowercase()))
    };
    // A PERSON's choice: a launch `--model` this harness did not put there, a
    // `/model` (now, or remembered from before its answers), or the saved
    // default in their settings — an id, or a family alias like `opus`.
    let human = launch_model.is_some_and(|m| !ours(m))
        || (live.by_command && !ours(&live.id))
        || (!record.human.is_empty() && base(&record.human) == base(&live.id))
        || default_model.is_some_and(|d| family(d) == family(&live.id));
    if human && family(&live.id) != family(target) {
        return ModelVerdict::Keep("model-chosen-by-hand");
    }
    ModelVerdict::Due {
        from: live.id.clone(),
        to: target.to_string(),
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

    #[test]
    fn the_model_rule_moves_up_the_list_only_and_respects_a_hand_choice() {
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
        // The ordinary case: Opus 5 → Opus 5.5.
        assert_eq!(
            model_due(
                &list,
                Some(&live("claude-opus-5", false)),
                Some("claude-opus-5-5"),
                None,
                None,
                &none
            ),
            due("claude-opus-5", "claude-opus-5-5")
        );
        // Across families too, when nobody chose the live one...
        assert_eq!(
            model_due(
                &list,
                Some(&live("claude-fable-5-1", false)),
                Some("claude-opus-5-5"),
                None,
                None,
                &none
            ),
            due("claude-fable-5-1", "claude-opus-5-5")
        );
        // ...but a hand choice (a /model this harness did not type, or a launch
        // --model it did not put there) moves only within its family.
        assert_eq!(
            model_due(
                &list,
                Some(&live("claude-fable-5-1", true)),
                Some("claude-opus-5-5"),
                None,
                None,
                &none
            ),
            ModelVerdict::Keep("model-chosen-by-hand")
        );
        assert_eq!(
            model_due(
                &list,
                Some(&live("claude-fable-5-1", false)),
                Some("claude-opus-5-5"),
                Some("claude-fable-5-1"),
                None,
                &none
            ),
            ModelVerdict::Keep("model-chosen-by-hand")
        );
        assert_eq!(
            model_due(
                &list,
                Some(&live("claude-opus-5", true)),
                Some("claude-opus-5-5"),
                None,
                None,
                &none
            ),
            due("claude-opus-5", "claude-opus-5-5"),
            "same family"
        );
        // The harness's own earlier /model is not a hand choice.
        let ours = ModelRecord {
            set: "claude-fable-5-1".into(),
            ..ModelRecord::default()
        };
        assert_eq!(
            model_due(
                &list,
                Some(&live("claude-fable-5-1", true)),
                Some("claude-opus-5-5"),
                None,
                None,
                &ours
            ),
            due("claude-fable-5-1", "claude-opus-5-5")
        );
        // Never down, never sideways, never off-list, never twice, never after a failure.
        assert_eq!(
            model_due(
                &list,
                Some(&live("claude-opus-5-5", false)),
                Some("claude-fable-5-1"),
                None,
                None,
                &none
            ),
            ModelVerdict::Keep("model-ranks-higher")
        );
        assert_eq!(
            model_due(
                &list,
                Some(&live("claude-opus-5-5[1m]", false)),
                Some("claude-opus-5-5"),
                None,
                None,
                &none
            ),
            ModelVerdict::Keep("model-current")
        );
        assert_eq!(
            model_due(
                &list,
                Some(&live("claude-sonnet-5", false)),
                Some("claude-opus-5-5"),
                None,
                None,
                &none
            ),
            ModelVerdict::Keep("model-off-list")
        );
        let applied = ModelRecord {
            applied: vec!["claude-opus-5-5".into()],
            ..ModelRecord::default()
        };
        assert_eq!(
            model_due(
                &list,
                Some(&live("claude-opus-5", true)),
                Some("claude-opus-5-5"),
                None,
                None,
                &applied
            ),
            ModelVerdict::Keep("model-applied-before")
        );
        let failed = ModelRecord {
            failed: vec!["claude-opus-5-5".into()],
            ..ModelRecord::default()
        };
        assert_eq!(
            model_due(
                &list,
                Some(&live("claude-opus-5", false)),
                Some("claude-opus-5-5"),
                None,
                None,
                &failed
            ),
            ModelVerdict::Keep("model-failed-before")
        );
        assert_eq!(
            model_due(&list, None, Some("claude-opus-5-5"), None, None, &none),
            ModelVerdict::Keep("model-unknown")
        );
        assert_eq!(
            model_due(
                &list,
                Some(&live("claude-opus-5", false)),
                None,
                None,
                None,
                &none
            ),
            ModelVerdict::Keep("no-model-available")
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

    #[test]
    fn a_persons_standing_choice_is_moved_only_within_its_family() {
        let list = Priority::seed(0);
        let fable = LiveModel {
            id: "claude-fable-5-1".into(),
            by_command: false,
        };
        let none = ModelRecord::default();
        // Remembered /model: the answers after it do not end the protection.
        let remembered = ModelRecord {
            human: "claude-fable-5-1".into(),
            ..ModelRecord::default()
        };
        assert_eq!(
            model_due(
                &list,
                Some(&fable),
                Some("claude-opus-5-5"),
                None,
                None,
                &remembered
            ),
            ModelVerdict::Keep("model-chosen-by-hand")
        );
        // The saved default in the person's settings, as an id or an alias.
        for default in ["claude-fable-5-1", "fable"] {
            assert_eq!(
                model_due(
                    &list,
                    Some(&fable),
                    Some("claude-opus-5-5"),
                    None,
                    Some(default),
                    &none
                ),
                ModelVerdict::Keep("model-chosen-by-hand"),
                "{default}"
            );
        }
        // A default in the TARGET's family does not hold back a move within it.
        let opus5 = LiveModel {
            id: "claude-opus-5".into(),
            by_command: false,
        };
        assert_eq!(
            model_due(
                &list,
                Some(&opus5),
                Some("claude-opus-5-5"),
                None,
                Some("opus"),
                &none
            ),
            ModelVerdict::Due {
                from: "claude-opus-5".into(),
                to: "claude-opus-5-5".into()
            }
        );
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
