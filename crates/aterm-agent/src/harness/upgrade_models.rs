// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The model priority list.

use std::path::{Path, PathBuf};

use aterm_durable::Flush;
use aterm_json::{Map, Value};

use super::upgrade::is_model_id;
use super::upgrade_catalog::{Baked, version_triple};

/// The owner's list, in priority order.
pub const SEED: [&str; 3] = ["claude-opus-5-5", "claude-fable-5-1", "claude-opus-5"];

fn base_id(id: &str) -> &str {
    id.strip_suffix("[1m]").unwrap_or(id)
}

/// `claude-<family>-<major>[-<minor>][-<yyyymmdd>]` → family and version.
#[must_use]
pub fn family_version(id: &str) -> Option<(String, (u32, u32))> {
    let base = base_id(id);
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
    /// What recommended it (`announcement:<id>`, `catalog:<build>`, `seed`, `owner`).
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
        let base = base_id(id);
        self.ids.iter().position(|x| x == base)
    }

    /// Parse the file.
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

    /// Admit a recommended id.
    pub fn admit(
        &mut self,
        id: &str,
        source: &str,
        known: &dyn Fn(&str) -> bool,
        now: i64,
    ) -> Option<Change> {
        let base = base_id(id);
        let (family, version) = family_version(base)?;
        if self.rank(base).is_some() || !known(base) {
            return None;
        }
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
/// # Errors
/// The file exists but does not parse — the caller refuses model changes.
pub fn load_or_seed(path: &Path, now: i64) -> Result<Priority, String> {
    match std::fs::read_to_string(path) {
        Ok(text) => {
            Priority::parse(&text).ok_or_else(|| format!("{} does not parse", path.display()))
        }
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
            let p = Priority::seed(now);
            write_atomic_ordered(path, &p.render())
                .map_err(|e| format!("{}: {e}", path.display()))?;
            Ok(p)
        }
        Err(e) => Err(format!("{}: {e}", path.display())),
    }
}

/// Write `text` to `path` through a sibling temp file and a rename.
/// # Errors
/// The directory could not be made or the write/rename failed.
pub fn write_atomic(path: &Path, text: &str) -> std::io::Result<()> {
    write_atomic_mode(path, text, None)
}

/// [`write_atomic`], with the replacement's permission bits (unix) where `mode` names them, for a file its owner keeps as they made it.
/// # Errors
/// The directory could not be made or the write, the mode or the rename failed.
pub fn write_atomic_mode(path: &Path, text: &str, mode: Option<u32>) -> std::io::Result<()> {
    use std::io::Write as _;
    write_atomic_using(path, text, mode, Flush::Durable, |file, bytes| {
        file.write_all(bytes)
    })
}

/// [`write_atomic`] with the stage finished by [`aterm_durable::ordered`] instead of a full flush.
/// # Errors
/// As [`write_atomic`].
pub fn write_atomic_ordered(path: &Path, text: &str) -> std::io::Result<()> {
    use std::io::Write as _;
    write_atomic_using(path, text, None, Flush::Ordered, |file, bytes| {
        file.write_all(bytes)
    })
}

fn write_atomic_using(
    path: &Path,
    text: &str,
    mode: Option<u32>,
    flush: Flush,
    write: impl FnOnce(&mut std::fs::File, &[u8]) -> std::io::Result<()>,
) -> std::io::Result<()> {
    let dir = path
        .parent()
        .filter(|p| !p.as_os_str().is_empty())
        .unwrap_or(Path::new("."));
    std::fs::create_dir_all(dir)?;
    let stage = aterm_tempfile::Builder::new()
        .prefix(".aterm-write-")
        .tempdir_in(dir)?;
    let staged = stage.path().join("data");
    let mut open = std::fs::OpenOptions::new();
    open.write(true).create_new(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt as _;
        open.mode(0o600);
    }
    let mut file = open.open(&staged)?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt as _;
        file.set_permissions(std::fs::Permissions::from_mode(mode.unwrap_or(0o600)))?;
    }
    #[cfg(not(unix))]
    let _ = mode;
    write(&mut file, text.as_bytes())?;
    crate::durable::finish(&file, flush)?;
    drop(file);
    std::fs::rename(&staged, path)
}

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
    /// The model families the account's profile bills to usage credits ([`credits_billed`]), read only under `[harness] usage_credits = false`.
    pub credits_billed: Vec<String>,
}

/// The model families taken for billed to usage credits where the account's billing cannot be read.
pub const CREDITS_BILLED_UNREAD: &[&str] = &["fable"];

/// THE MODEL FAMILIES AN ACCOUNT BILLS TO USAGE CREDITS at `now` (unix seconds), by its profile.
#[must_use]
pub fn credits_billed(profile: &super::profile::Profile, now: i64) -> Vec<String> {
    let mut billed: Vec<String> = Vec::new();
    let fable = fable_billed(&profile.fable, now).then(|| "fable".to_string());
    for family in fable
        .into_iter()
        .chain(profile.consented.iter().cloned())
        .chain(profile.credits_only.iter().flatten().cloned())
    {
        if !billed.contains(&family) {
            billed.push(family);
        }
    }
    billed
}

/// WHETHER THE PLAN BILLS FABLE TO USAGE CREDITS at `now` ([`super::profile::FablePlan`]).
#[must_use]
pub const fn fable_billed(fable: &super::profile::FablePlan, now: i64) -> bool {
    use super::profile::FablePlan;
    match fable {
        FablePlan::CreditsOnly | FablePlan::LimitsEnd(None) | FablePlan::Unknown => true,
        FablePlan::LimitsEnd(Some(at)) => now >= *at,
        FablePlan::Included => false,
    }
}

/// Whether `model` — an id (`claude-fable-5-1[1m]`) or Claude Code's alias for a family (`fable`).
#[must_use]
pub fn billed_to_credits(model: &str, billed: &[String]) -> bool {
    let base = base_id(model);
    let family = family_version(base).map_or_else(|| base.to_string(), |(family, _)| family);
    billed.contains(&family)
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

/// Parse the model-related parts of `~/.claude.json`.
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
                .map(|s| base_id(s).to_string())
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

/// Read every piece of evidence from a Claude config directory (`~/.claude`) and the `.claude.json` beside or inside it.
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
        credits_billed: Vec::new(),
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
    pub const fn ok(&self) -> bool {
        matches!(self, Availability::Yes(_))
    }
}

/// Is `id` usable for a session that will run Claude Code `cc_version` (the served catalog, the person's deny lists, the build's own catalog, a family billed to usage credits under the key off)?
#[must_use]
pub fn availability(
    id: &str,
    cc_version: &str,
    ev: &Evidence,
    baked: Option<&Baked>,
) -> Availability {
    let base = base_id(id);
    if ev.denied.iter().any(|d| d == base) {
        return Availability::No("modelAccessCache entitled:false".to_string());
    }
    if billed_to_credits(base, &ev.credits_billed) {
        return Availability::No("billed to usage credits (usage_credits off)".to_string());
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

/// The best available entry of the list, with every entry's verdict.
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

/// Claude Code's own recommendations, newest-first by source.
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

/// Apply every admissible recommendation to the list.
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

/// Seconds a model a relaunch of this harness asked for (`--model`) is given to show in the transcript before the choice is recorded as failed for that conversation.
pub const MODEL_SETTLE_S: u64 = 600;

/// A `/model` result's display name (`Opus 5.5 (default)`, `Fable 5.1 (1M context)`) mapped back to an id through the build's baked catalog.
#[must_use]
pub fn model_of_display(display: &str, baked: Option<&Baked>) -> Option<String> {
    let mut name = display.trim();
    let mut one_m = false;
    while let Some((suffix, rest)) = [" (default)", " (1M context)", " (recommended)"]
        .into_iter()
        .find_map(|suffix| name.strip_suffix(suffix).map(|rest| (suffix, rest)))
    {
        one_m |= suffix == " (1M context)";
        name = rest.trim();
    }
    if is_model_id(name) && baked.is_some_and(|b| b.model(name).is_some()) {
        return Some(name.to_string());
    }
    let m = baked?.models.iter().find(|m| m.display_name == name)?;
    Some(if one_m && !m.native_1m {
        format!("{}[1m]", m.id)
    } else {
        m.id.clone()
    })
}

/// What a session runs now, read from its transcript's tail.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LiveModel {
    /// The model id.
    pub id: String,
    /// It comes from a `/model` result newer than the last answer (someone chose it), not from the last answer itself.
    pub by_command: bool,
}

/// What one of Claude Code's model result rows says (its `<local-command-stdout>`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum ModelResultKind {
    /// `Set model to …`: the model was chosen (`/model`, the picker, a typed `/model <name>`).
    Set,
    /// `Kept model as …`: a `/model` that changed nothing.
    Kept,
    /// `<icon> Fast mode ON · model set to …`: `/fast on` moved the model to one that runs fast.
    FastOn,
}

/// A model result row, read ([`model_result_of`]).
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct ModelResult {
    pub(crate) kind: ModelResultKind,
    /// The model as Claude spelled it, quotes off (`Opus 5.5 (default)`).
    pub(crate) display: Option<String>,
    /// The effort level a `` with `<level>` effort `` clause after it names, as written.
    pub(crate) effort: Option<String>,
}

fn quoted_display(rest: &str) -> Option<(String, &str)> {
    let (display, after) = if let Some(quoted) = rest.strip_prefix('`') {
        let end = quoted.find('`')?;
        (&quoted[..end], &quoted[end + 1..])
    } else {
        let bold = rest.strip_prefix("\u{1b}[1m")?;
        let end = bold.find("\u{1b}[22m")?;
        (&bold[..end], &bold[end + "\u{1b}[22m".len()..])
    };
    Some((display.chars().take(80).collect(), after))
}

fn effort_clause(after: &str) -> Option<String> {
    let (_, rest) = after.split_once(" with `")?;
    let (level, tail) = rest.split_once('`')?;
    tail.starts_with(" effort")
        .then(|| level.chars().take(16).collect())
}

/// `text` without ANSI SGR sequences (`\x1b[1m`, `\x1b[38;2;255;106;0m`), stripped as Claude's result classifier strips them (`/\x1b\[[0-9;]*m/g`).
pub(crate) fn strip_sgr(text: &str) -> std::borrow::Cow<'_, str> {
    if !text.contains('\u{1b}') {
        return text.into();
    }
    let mut out = String::with_capacity(text.len());
    let mut rest = text;
    while let Some(at) = rest.find("\u{1b}[") {
        out.push_str(&rest[..at]);
        let seq = &rest[at + 2..];
        match seq.find(|c: char| !(c.is_ascii_digit() || c == ';')) {
            Some(end) if seq[end..].starts_with('m') => rest = &seq[end + 1..],
            _ => {
                out.push('\u{1b}');
                rest = &rest[at + 1..];
            }
        }
    }
    out.push_str(rest);
    out.into()
}

const FAST_ON_LEAD: usize = 4;

/// What a `type:user` row's content says about the model.
pub(crate) fn model_result_of(content: &str) -> Option<ModelResult> {
    let body = content
        .trim_start()
        .strip_prefix("<local-command-stdout>")?
        .trim_start();
    for (kind, id) in [
        (ModelResultKind::Set, "model.set"),
        (ModelResultKind::Kept, "model.kept"),
    ] {
        if let Some(rest) = body.strip_prefix(aterm_phase::anchor(id)) {
            let quoted = quoted_display(rest);
            return Some(ModelResult {
                kind,
                effort: quoted.as_ref().and_then(|(_, after)| effort_clause(after)),
                display: quoted.map(|(display, _)| display),
            });
        }
    }
    let plain = strip_sgr(body);
    let phrase = format!(
        "{} \u{00B7} {}",
        aterm_phase::anchor!("fast.on"),
        aterm_phase::anchor!("fast.model_set")
    );
    let at = plain
        .find(&phrase)
        .filter(|&at| plain[..at].encode_utf16().count() <= FAST_ON_LEAD)?;
    Some(ModelResult {
        kind: ModelResultKind::FastOn,
        display: quoted_display(&plain[at + phrase.len()..]).map(|(display, _)| display),
        effort: None,
    })
}

fn model_command_display(line: &str) -> Option<String> {
    if !line.contains(aterm_phase::anchor!("model.set")) {
        return None;
    }
    let v = super::transcript::command(line).ok()?;
    if v.get("type").and_then(Value::as_str) != Some("user") {
        return None;
    }
    let content = v.get("message")?.get("content")?.as_str()?;
    model_result_of(content)
        .filter(|r| r.kind == ModelResultKind::Set)
        .and_then(|r| r.display)
}

/// The live model: the newest `/model` result ([`model_command_display`]) when it is newer than the last main-chain answer, else the last answer's model.
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
        let Ok(v) = super::transcript::metadata(line) else {
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

/// The hand-chosen model a relaunch carries in place of the launch's `--model` (`launch`), or `None` to keep the launch's flags.
#[must_use]
pub fn hand_chosen(
    live: Option<&LiveModel>,
    human: &str,
    launch: &str,
    baked: Option<&Baked>,
) -> Option<String> {
    let live = live?;
    let launch = launch_names(launch, baked);
    if live.by_command {
        return (live.id != launch).then(|| live.id.clone());
    }
    let live_base = base_id(&live.id);
    (!human.is_empty() && base_id(human) == live_base && live_base != base_id(&launch))
        .then(|| human.to_string())
}

/// The model a launch's `--model` value names: a real id as it is.
#[must_use]
pub fn launch_names(launch: &str, baked: Option<&Baked>) -> String {
    let (name, window) = launch
        .strip_suffix("[1m]")
        .map_or((launch, ""), |b| (b, "[1m]"));
    if family_version(name).is_some() {
        return launch.to_string();
    }
    baked
        .and_then(|b| b.latest(name))
        .map_or_else(|| launch.to_string(), |id| format!("{id}{window}"))
}

/// [`hand_chosen`] off a transcript `tail`, for a process started at `started_s` with launch `--model` `launch` and recorded choice `human`.
#[must_use]
pub fn hand_model(
    tail: &str,
    baked: Option<&Baked>,
    human: &str,
    launch: &str,
    started_s: Option<u64>,
) -> Option<String> {
    if started_s.is_some_and(|s| newest_row_at(tail).is_none_or(|at| at < s)) {
        return None;
    }
    hand_chosen(live_model(tail, baked).as_ref(), human, launch, baked)
}

fn newest_row_at(tail: &str) -> Option<u64> {
    tail.lines()
        .filter(|l| l.contains("\"assistant\"") || model_command_display(l).is_some())
        .filter_map(|l| super::transcript::metadata(l).ok())
        .filter(|v| v.get("isSidechain").and_then(Value::as_bool) != Some(true))
        .filter_map(|v| {
            v.get("timestamp")
                .and_then(Value::as_str)
                .and_then(parse_utc)
        })
        .max()
}

/// Seconds without an answer after which a conversation's prompt cache is cold.
pub const CACHE_COLD_S: u64 = 3600;

/// The longest a due model move waits for a cold cache, in seconds of being due.
pub const MODEL_WARM_MAX_S: u64 = CACHE_COLD_S;

/// The model ladder: whether a due move is taken on this visit, and why, or `None` to wait.
#[must_use]
pub const fn model_moves_now(
    cold: bool,
    build_restart: bool,
    due_for_s: u64,
) -> Option<&'static str> {
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

/// Unix seconds of an RFC 3339 UTC stamp (`2026-09-24T04:18:02.796Z`, as transcripts carry), via [`aterm_types::rfc3339::parse_utc_fractional`].
#[must_use]
pub fn parse_utc(s: &str) -> Option<u64> {
    u64::try_from(aterm_types::rfc3339::parse_utc_fractional(s)?).ok()
}

/// Unix seconds of the last main-chain answer in a transcript tail (`None`: no answer in the tail).
#[must_use]
pub fn last_answer_at(tail: &str) -> Option<u64> {
    tail.lines()
        .rev()
        .filter(|l| l.contains("\"assistant\""))
        .filter_map(|l| super::transcript::metadata(l).ok())
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

/// Unix seconds of a `ps -o lstart=` stamp in UTC, C locale — the session file's `procStart`.
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

/// [`live_model`] for a process started at `started_s` with the launch's own `--model` (`launch`).
#[must_use]
pub fn live_model_at(
    tail: &str,
    baked: Option<&Baked>,
    launch: Option<&str>,
    started_s: Option<u64>,
) -> Option<LiveModel> {
    let real = |m: &&str| is_model_id(m) && family_version(base_id(m)).is_some();
    if let (Some(launch), Some(started)) = (launch.filter(real), started_s)
        && newest_row_at(tail).is_none_or(|t| t < started)
    {
        return Some(LiveModel {
            id: launch.to_string(),
            by_command: false,
        });
    }
    live_model(tail, baked)
}

/// The person's own Claude settings that bear on the model (`~/.claude/settings.json`).
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct UserModelSettings {
    /// `model`: the default new sessions start on (an id or a family alias), a choice of the person's.
    pub default: Option<String>,
    /// `availableModels`: when present, the only models a session may use.
    pub allowed: Option<Vec<String>>,
}

/// Parse [`UserModelSettings`] from a settings file's text via `claude_settings::parse`.
#[must_use]
pub fn parse_user_settings(text: &str) -> UserModelSettings {
    crate::supervise::policy::claude_settings::parse(text)
        .map(|v| model_settings_of(&v))
        .unwrap_or_default()
}

/// [`UserModelSettings`] out of one parsed settings document `v`, or out of the values the settings chain gives each key.
#[must_use]
pub fn model_settings_of(v: &Value) -> UserModelSettings {
    UserModelSettings {
        default: v
            .get("model")
            .and_then(Value::as_str)
            .filter(|m| is_model_id(m))
            .map(str::to_string),
        allowed: v.get("availableModels").map(|a| {
            a.as_array()
                .map(|a| {
                    a.iter()
                        .filter_map(Value::as_str)
                        .filter(|m| is_model_id(m))
                        .map(str::to_string)
                        .collect()
                })
                .unwrap_or_default()
        }),
    }
}

/// The list's target, the rule's second step ([`model_due`]).
#[must_use]
pub fn list_target<'a>(list: &'a Priority, offered: &[String]) -> Option<&'a str> {
    list.ids.iter().map(String::as_str).find(|id| {
        let base = base_id(id);
        offered.iter().any(|o| o == base)
    })
}

/// Whether the person's `availableModels` (`None`: they set none) admits `id`, as itself or in its `[1m]` form.
pub(crate) fn allowed_by(allowed: Option<&[String]>, id: &str) -> bool {
    allowed.is_none_or(|ok| {
        ok.iter()
            .any(|x| x == id || x.strip_suffix("[1m]") == Some(id))
    })
}

/// Every model a restart on the build `cc_version` may ask for, the candidates both steps of the rule choose among.
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
        let base = base_id(id);
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

/// The same-family resolver.
#[must_use]
pub fn family_successor(current: &str, one_m: bool, offered: &[String]) -> Option<String> {
    let (family, version) = family_version(current)?;
    let (_, newest) = offered
        .iter()
        .filter_map(|id| {
            let base = base_id(id);
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

/// What the harness remembers about one conversation's model, beside the per-conversation upgrade state.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ModelRecord {
    /// Models this harness already moved the conversation to: never re-applied.
    pub applied: Vec<String>,
    /// The model the harness last asked for (a relaunch's `--model`), and when.
    pub set: String,
    /// Unix seconds.
    pub set_at: u64,
    /// Models this harness asked for (a relaunch's `--model`) that did not become what runs.
    pub failed: Vec<String>,
    /// The model a person last chose for this conversation with `/model` (the harness never types `/model`), remembered past the answers that follow it.
    pub human: String,
    /// The screen sequence last seen, and since when (the quiet gate).
    pub last_seq: u64,
    /// Unix seconds.
    pub seq_since_s: u64,
    /// The model a move has been due to, and since when (Unix seconds).
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
        /// The newest model of its family on offer ([`family_successor`], `[1m]` kept), or, with none newer, the list's target.
        to: String,
    },
    /// Leave it, and why (one word for the report).
    Keep(&'static str),
}

/// The model rule: same family first, then the priority list.
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
    let spent_on = |to: &str| spent(record, to).map(ModelVerdict::Keep);
    let tagged = |m: &str| m.ends_with("[1m]");
    let one_m = tagged(&live.id)
        || (!live.by_command
            && launch_model
                .is_some_and(|m| tagged(m) && family_version(m).is_some_and(|(f, _)| f == family)));
    if let Some(to) = family_successor(&live.id, one_m, offered) {
        return spent_on(&to).unwrap_or(ModelVerdict::Due {
            from: live.id.clone(),
            to,
        });
    }
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
    if let Some(keep) = spent_on(target) {
        return keep;
    }
    if chosen_by_hand(live, &family, launch_model, default_model, record) {
        return ModelVerdict::Keep("model-chosen-by-hand");
    }
    ModelVerdict::Due {
        from: live.id.clone(),
        to: target.to_string(),
    }
}

fn spent(record: &ModelRecord, to: &str) -> Option<&'static str> {
    if record.applied.iter().any(|a| base_id(a) == base_id(to)) {
        Some("model-applied-before")
    } else if record.failed.iter().any(|a| base_id(a) == base_id(to)) {
        Some("model-failed-before")
    } else {
        None
    }
}

fn chosen_by_hand(
    live: &LiveModel,
    family: &str,
    launch_model: Option<&str>,
    default_model: Option<&str>,
    record: &ModelRecord,
) -> bool {
    let ours = |m: &str| !record.set.is_empty() && base_id(m) == base_id(&record.set);
    let family_of = |m: &str| {
        family_version(base_id(m))
            .map(|f| f.0)
            .unwrap_or_else(|| base_id(m).to_ascii_lowercase())
    };
    launch_model.is_some_and(|m| !ours(m))
        || (live.by_command && !ours(&live.id))
        || (!record.human.is_empty() && base_id(&record.human) == base_id(&live.id))
        || default_model.is_some_and(|d| family_of(d) == family)
}

/// Whether `carried`, the model a notice of the upgrade carried, may still ride its round's restart given `live`.
#[must_use]
pub fn ride_bar(
    carried: &str,
    list: &Priority,
    live: Option<&LiveModel>,
    launch_model: Option<&str>,
    default_model: Option<&str>,
    record: &ModelRecord,
) -> Option<&'static str> {
    if let Some(word) = spent(record, carried) {
        return Some(word);
    }
    if launch_model.is_some_and(|m| family_version(m).is_none()) {
        return Some("model-alias");
    }
    let live = live?;
    let (Some((family, version)), Some((to_family, to_version))) =
        (family_version(&live.id), family_version(carried))
    else {
        return Some("model-unreadable");
    };
    if to_family == family {
        return (to_version <= version).then_some("model-current");
    }
    let (Some(rl), Some(rt)) = (list.rank(&live.id), list.rank(carried)) else {
        return Some("model-off-list");
    };
    if rt >= rl {
        return Some("model-ranks-higher");
    }
    chosen_by_hand(live, &family, launch_model, default_model, record)
        .then_some("model-chosen-by-hand")
}

/// What one visit's read of the live model did to a conversation's [`ModelRecord`].
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Settled {
    /// The model this harness asked for runs: recorded applied (the caller says `model-verified`).
    pub verified: Option<String>,
    /// The model this harness asked for is still not what runs [`MODEL_SETTLE_S`] after it was asked for.
    pub failed: Option<String>,
    /// The record changed (either of the above, or a `/model` remembered): the caller saves it.
    pub changed: bool,
}

impl ModelRecord {
    /// The settle step, taken on every visit that reads what the conversation runs (`live`, `None` when unreadable) at `now`, before [`model_due`].
    pub fn settle(&mut self, live: Option<&LiveModel>, now: u64) -> Settled {
        let mut out = Settled::default();
        let applied =
            |rec: &ModelRecord| rec.applied.iter().any(|a| base_id(a) == base_id(&rec.set));
        if !self.set.is_empty()
            && live.is_some_and(|l| base_id(&l.id) == base_id(&self.set))
            && !applied(self)
        {
            self.applied.push(self.set.clone());
            out.verified = Some(self.set.clone());
            out.changed = true;
        }
        if !self.set.is_empty()
            && live.is_none_or(|l| base_id(&l.id) != base_id(&self.set))
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

    /// Step the due clock ([`ModelRecord::due_to`], [`ModelRecord::due_since`]) by `verdict` at `now`.
    pub fn due_clock(&mut self, verdict: &ModelVerdict, now: u64) -> (u64, bool) {
        match verdict {
            ModelVerdict::Due { to, .. } => {
                let restart = base_id(&self.due_to) != base_id(to)
                    || self.due_since == 0
                    || self.due_since > now;
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

    /// Record `model` as this conversation's ask at `now`, right before the relaunch line is typed.
    pub fn asked(&mut self, model: &str, now: u64) -> Asked {
        Asked {
            model: model.to_string(),
            at: now,
            before: std::mem::replace(&mut self.set, model.to_string()),
            before_at: std::mem::replace(&mut self.set_at, now),
        }
    }

    /// Undo [`ModelRecord::asked`] when its relaunch line was not typed (a wait, a refusal).
    pub fn take_back(&mut self, asked: Asked) {
        if self.set == asked.model && self.set_at == asked.at {
            self.set = asked.before;
            self.set_at = asked.before_at;
        }
    }
}

/// An ask [`ModelRecord::asked`] recorded, and the one it replaced.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Asked {
    model: String,
    at: u64,
    before: String,
    before_at: u64,
}

/// What one visit reads a conversation's model against ([`model_read_step`]), for the build the session runs.
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
    /// The model a notice of the upgrade carried.
    pub carried: Option<&'a str>,
}

/// What one visit's read decided ([`model_read_step`]).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ModelReadStep {
    /// The model rule's verdict ([`model_due`]).
    pub verdict: ModelVerdict,
    /// The conversation's prompt cache is cold (no answer for [`CACHE_COLD_S`]).
    pub cold: bool,
    /// Seconds the current due move has been due (0 when none is).
    pub due_for_s: u64,
    /// What the settle step recorded ([`ModelRecord::settle`]).
    pub settled: Settled,
    /// The record changed (the settle step, or the clock): the caller saves it.
    pub changed: bool,
    /// The [`ride_bar`] word why the carried model ([`ModelView::carried`]) does not ride its round's restart.
    pub ride_barred: Option<&'static str>,
}

/// One visit's read of a conversation's model, over `record` (changed in place) and `view`.
pub fn model_read_step<S: AsRef<str>>(
    record: &mut ModelRecord,
    view: &ModelView<'_>,
    tail: impl FnOnce() -> S,
) -> ModelReadStep {
    if view.offered.is_empty() && record.set.is_empty() && view.carried.is_none() {
        let verdict = ModelVerdict::Keep("no-model-available");
        let (_, changed) = record.due_clock(&verdict, view.now);
        return ModelReadStep {
            verdict,
            cold: false,
            due_for_s: 0,
            settled: Settled::default(),
            changed,
            ride_barred: None,
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
    let ride_barred = view.carried.and_then(|carried| {
        ride_bar(
            carried,
            view.list,
            live.as_ref(),
            view.launch,
            view.default_model,
            record,
        )
    });
    let cold = last_answer_at(tail).is_none_or(|at| view.now.saturating_sub(at) >= CACHE_COLD_S);
    let (due_for_s, clocked) = record.due_clock(&verdict, view.now);
    let changed = settled.changed || clocked;
    ModelReadStep {
        verdict,
        cold,
        due_for_s,
        settled,
        changed,
        ride_barred,
    }
}

/// The model a restart of this conversation asks for (`--model` on its relaunch), or `None`.
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

/// The launch's `--model` a relaunch keeps.
#[must_use]
pub fn launch_model_kept<'a>(launch: Option<&'a str>, record: &ModelRecord) -> Option<&'a str> {
    launch.filter(|m| !record.failed.iter().any(|f| base_id(f) == base_id(m)))
}

/// The model a relaunch line asks for (`relaunch::Launch::model`): `asks` when the site has one.
#[must_use]
pub fn line_model<'a>(
    asks: Option<&'a str>,
    launch: Option<&str>,
    record: &ModelRecord,
) -> Option<&'a str> {
    asks.or_else(|| (launch.is_some() && launch_model_kept(launch, record).is_none()).then_some(""))
}

/// Why a relaunch moves from `before` (the model the conversation ran) to `to`, read back from the two ids.
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
    pub const fn words(self) -> &'static str {
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
    fn an_id_outside_the_grammar_or_the_list_is_never_admitted() {
        let mut p = Priority::seed(0);
        assert_eq!(p.admit("claude-opus-6;ls", "x", &|_| true, 1), None);
        assert_eq!(p.admit("claude-mythos-5", "x", &|_| true, 1), None);
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
        let (best, verdicts) = target(&p, "2.1.278", &ev, None);
        assert_eq!(best, Some("claude-fable-5-1"));
        assert_eq!(
            verdicts[0].1,
            Availability::No("needs Claude Code 2.1.280".into())
        );
        ev.denied = vec!["claude-opus-5-5".into(), "claude-fable-5-1".into()];
        assert_eq!(target(&p, "2.1.280", &ev, None).0, Some("claude-opus-5"));
        assert!(!availability("claude-opus-4-1", "2.1.280", &ev, None).ok());
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
        let tail = [answer("claude-opus-5"), set_model("claude-fable-5-1")].join("\n");
        assert_eq!(
            live_model(&tail, Some(&b)),
            Some(LiveModel {
                id: "claude-fable-5-1".into(),
                by_command: true
            })
        );
        let tail = [set_model("claude-fable-5-1"), answer("claude-fable-5-1")].join("\n");
        assert!(!live_model(&tail, Some(&b)).expect("an answer").by_command);
        let tail = [answer("claude-opus-5"), set_model("Mystery 9")].join("\n");
        assert_eq!(live_model(&tail, Some(&b)), None);
    }

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

    #[test]
    fn the_resolver_moves_to_the_newest_of_the_same_family_and_nowhere_else() {
        let offered = ids(&[
            "claude-opus-5-5",
            "claude-fable-5-1",
            "claude-sonnet-5-5",
            "claude-opus-5",
        ]);
        let next = |current: &str, one_m: bool| family_successor(current, one_m, &offered);
        assert_eq!(
            next("claude-opus-5", false).as_deref(),
            Some("claude-opus-5-5")
        );
        assert_eq!(
            next("claude-opus-5[1m]", true).as_deref(),
            Some("claude-opus-5-5[1m]")
        );
        assert_eq!(
            family_successor("claude-opus-5", false, &ids(&["claude-opus-5-5[1m]"])).as_deref(),
            Some("claude-opus-5-5")
        );
        assert_eq!(
            next("claude-opus-4-1", false).as_deref(),
            Some("claude-opus-5-5")
        );
        assert_eq!(
            next("claude-sonnet-4-20250514", false).as_deref(),
            Some("claude-sonnet-5-5")
        );
        assert_eq!(
            family_successor(
                "claude-opus-5",
                false,
                &ids(&["claude-opus-5-5", "claude-opus-5-6", "claude-opus-5-1"])
            )
            .as_deref(),
            Some("claude-opus-5-6")
        );
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
        let hand = Priority {
            ids: ids(&["claude-opus-5-6", "claude-opus-5-5"]),
            history: vec![],
        };
        assert!(
            !offered(&hand, "2.1.283", &none, Some(&b), None).contains(&"claude-opus-5-6".into())
        );
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
        assert!(offered(&list, "2.1.283", &none, None, None).is_empty());
    }

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
        assert_eq!(
            rule("claude-opus-5", false, None),
            due("claude-opus-5", "claude-opus-5-5")
        );
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
        assert_eq!(
            rule("claude-sonnet-5", false, None),
            due("claude-sonnet-5", "claude-sonnet-5-5")
        );
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
        assert_eq!(
            rule("claude-opus-5-5[1m]", false, None),
            ModelVerdict::Keep("model-current")
        );
        for alias in ["opus", "sonnet[1m]", "us.anthropic.claude-opus-5-v1:0"] {
            assert_eq!(
                rule("claude-opus-5", false, Some(alias)),
                ModelVerdict::Keep("model-alias"),
                "{alias}"
            );
        }
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
        let offer = ids(&["claude-opus-5-5", "claude-fable-5-1", "claude-opus-5"]);
        let fable = |by_command: bool| live("claude-fable-5-1", by_command);
        assert_eq!(
            model_due(&list, Some(&fable(false)), &offer, None, None, &none),
            due("claude-fable-5-1", "claude-opus-5-5")
        );
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

    #[test]
    fn a_record_without_the_due_clock_parses_as_nothing_due() {
        let old = r#"{"applied":[],"set":"","set_at":0,"failed":[],"human":"","last_seq":3,"seq_since_s":4}"#;
        let got = ModelRecord::parse(old).expect("an older record still parses");
        assert!(got.due_to.is_empty() && got.due_since == 0, "{got:?}");
        assert_eq!(got.last_seq, 3);
    }

    #[test]
    fn utc_stamps_and_the_last_answer() {
        assert_eq!(parse_utc("1970-01-01T00:00:00.000Z"), Some(0));
        assert_eq!(parse_utc("2026-09-24T04:18:02.796Z"), Some(1_790_223_482));
        assert_eq!(parse_utc("2026-13-01T00:00:00Z"), None);
        assert_eq!(parse_utc("garbage"), None);
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
    fn a_model_chosen_by_hand_is_carried_only_where_it_is_not_the_launchs() {
        let live = |id: &str, by_command: bool| LiveModel {
            id: id.to_string(),
            by_command,
        };
        let sonnet = "claude-sonnet-5-5";
        let opus = "claude-opus-5-5";
        let opus_1m = "claude-opus-5-5[1m]";
        assert_eq!(
            hand_chosen(Some(&live(opus, true)), "", sonnet, None).as_deref(),
            Some(opus)
        );
        assert_eq!(
            hand_chosen(Some(&live(opus_1m, true)), "", opus, None).as_deref(),
            Some(opus_1m),
            "the window chosen by hand"
        );
        assert_eq!(
            hand_chosen(Some(&live(opus, false)), opus_1m, sonnet, None).as_deref(),
            Some(opus_1m),
            "answered since: the choice as recorded"
        );
        assert_eq!(
            hand_chosen(Some(&live(sonnet, true)), "", sonnet, None),
            None
        );
        assert_eq!(
            hand_chosen(Some(&live(opus, false)), "", sonnet, None),
            None
        );
        assert_eq!(
            hand_chosen(Some(&live(opus, false)), opus, opus_1m, None),
            None
        );
        assert_eq!(
            hand_chosen(Some(&live(sonnet, false)), opus, sonnet, None),
            None
        );
        assert_eq!(hand_chosen(None, opus, sonnet, None), None);
    }

    #[test]
    fn a_pick_of_what_the_launch_alias_names_is_no_hand_choice() {
        let live = |id: &str, by_command: bool| LiveModel {
            id: id.to_string(),
            by_command,
        };
        let b = baked(
            &["claude-opus-5-5", "claude-opus-5", "claude-sonnet-5"],
            &[("opus", "claude-opus-5-5"), ("sonnet", "claude-sonnet-5")],
        );
        let opus = "claude-opus-5-5";
        assert_eq!(
            hand_chosen(Some(&live(opus, true)), "", "opus", Some(&b)),
            None
        );
        assert_eq!(
            hand_chosen(
                Some(&live("claude-opus-5-5[1m]", true)),
                "",
                "opus[1m]",
                Some(&b)
            ),
            None,
            "the alias's window kept"
        );
        assert_eq!(
            hand_chosen(Some(&live(opus, false)), opus, "opus", Some(&b)),
            None,
            "answered since: the recorded pick is the alias's"
        );
        assert_eq!(
            hand_chosen(Some(&live("claude-opus-5", true)), "", "opus", Some(&b)).as_deref(),
            Some("claude-opus-5")
        );
        assert_eq!(
            hand_chosen(
                Some(&live("claude-opus-5-5[1m]", true)),
                "",
                "opus",
                Some(&b)
            )
            .as_deref(),
            Some("claude-opus-5-5[1m]")
        );
        assert_eq!(
            hand_chosen(Some(&live("claude-sonnet-5", true)), "", "opus", Some(&b)).as_deref(),
            Some("claude-sonnet-5")
        );
        assert_eq!(
            hand_chosen(Some(&live(opus, true)), "", "opus", None).as_deref(),
            Some(opus)
        );
        assert_eq!(launch_names("opus[1m]", Some(&b)), "claude-opus-5-5[1m]");
        assert_eq!(launch_names("claude-opus-5", Some(&b)), "claude-opus-5");
        assert_eq!(launch_names("opusplan", Some(&b)), "opusplan");
    }

    #[test]
    fn a_model_command_older_than_the_process_is_no_hand_choice() {
        let b = baked(
            &["claude-opus-5-5", "claude-sonnet-5"],
            &[("opus", "claude-opus-5-5")],
        );
        let set_at = |display: &str, at: &str| {
            format!(
                r#"{{"type":"user","timestamp":"{at}","message":{{"content":"<local-command-stdout>Set model to `{display}`</local-command-stdout>"}}}}"#
            )
        };
        let started = parse_utc("2026-09-24T12:00:00Z");
        let before = set_at("claude-sonnet-5", "2026-09-23T00:00:00Z");
        let after = set_at("claude-sonnet-5", "2026-09-25T00:00:00Z");
        assert_eq!(hand_model(&before, Some(&b), "", "opus", started), None);
        assert_eq!(
            hand_model(&after, Some(&b), "", "opus", started).as_deref(),
            Some("claude-sonnet-5")
        );
        assert_eq!(
            hand_model(&before, Some(&b), "", "opus", None).as_deref(),
            Some("claude-sonnet-5")
        );
        let answered_before = r#"{"type":"assistant","isSidechain":false,"timestamp":"2026-09-23T00:00:00Z","message":{"model":"claude-sonnet-5","content":[]}}"#;
        assert_eq!(
            hand_model(
                answered_before,
                Some(&b),
                "claude-sonnet-5",
                "opus",
                started
            ),
            None,
            "the recorded choice, answered before the process"
        );
        assert_eq!(
            hand_model(answered_before, Some(&b), "claude-sonnet-5", "opus", None).as_deref(),
            Some("claude-sonnet-5")
        );
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
        assert_eq!(
            live_model_at(old, None, Some("claude-opus-5"), started).map(|l| l.id),
            Some("claude-opus-5".into())
        );
        let started_before = parse_utc("2026-09-24T01:00:00Z");
        assert_eq!(
            live_model_at(old, None, Some("claude-opus-5"), started_before).map(|l| l.id),
            Some("claude-opus-5-5".into())
        );
        assert_eq!(
            live_model_at(old, None, Some("opus"), started).map(|l| l.id),
            Some("claude-opus-5-5".into())
        );
    }

    #[test]
    fn a_marked_settings_file_keeps_its_allowlist_and_default() {
        let u = parse_user_settings(
            "\u{feff}{\"model\":\"opus\",\"availableModels\":[\"claude-opus-5-5\"]}",
        );
        assert_eq!(u.default.as_deref(), Some("opus"));
        assert_eq!(u.allowed, Some(vec!["claude-opus-5-5".to_string()]));
        for blank in ["", " \n\t", "\u{feff}"] {
            assert_eq!(
                parse_user_settings(blank),
                UserModelSettings::default(),
                "{blank:?}"
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
        let target = |allowed: Option<&[String]>| {
            list_target(&list, &offered(&list, "2.1.282", &ev, Some(&b), allowed))
        };
        assert_eq!(target(None), Some("claude-opus-5-5"));
        let only_fable = ["claude-fable-5-1".to_string()];
        assert_eq!(target(Some(&only_fable)), Some("claude-fable-5-1"));
        assert_eq!(target(Some(&[])), None);
        let u = parse_user_settings(
            r#"{"alwaysThinkingEnabled":true,"model":"opus","availableModels":["claude-opus-5-5","a b"]}"#,
        );
        assert_eq!(u.default.as_deref(), Some("opus"));
        assert_eq!(u.allowed, Some(vec!["claude-opus-5-5".to_string()]));
        assert_eq!(parse_user_settings("nope"), UserModelSettings::default());
    }

    #[test]
    fn a_plan_bills_fable_to_usage_credits_only_where_its_profile_says() {
        use super::super::profile::{FablePlan, parse};
        let now = 1_792_000_000;
        let fable = vec!["fable".to_string()];
        for billed in [
            FablePlan::CreditsOnly,
            FablePlan::LimitsEnd(Some(now)),
            FablePlan::LimitsEnd(None),
            FablePlan::Unknown,
        ] {
            assert!(fable_billed(&billed, now), "{billed:?}");
        }
        for none in [FablePlan::LimitsEnd(Some(now + 1)), FablePlan::Included] {
            assert!(!fable_billed(&none, now), "{none:?}");
        }
        let profile = |extra: &str| {
            parse(
                format!(
                    r#"{{"oauthAccount":{{"organizationType":"claude_max"}},
                    "cachedGrowthBookFeatures":{{"tengu_saffron_credits_only_tiers":[],
                        "tengu_saffron_lattice":{{"enabled":false}}{extra}}}"#
                )
                .as_bytes(),
            )
            .expect("a profile")
        };
        let included = profile("}");
        assert_eq!(included.fable, FablePlan::Included);
        assert!(credits_billed(&included, now).is_empty());
        let consented = profile(
            r#"},"fableOverageConsentV2":{"acct:0b6c":true},
            "usageCreditsOnlyModelConsent":{"Nimbus":{"org-1":true},"Opus":{"org-1":false}}"#,
        );
        assert_eq!(credits_billed(&consented, now), ["fable", "nimbus"]);
        let sealed = profile(r#","tengu_calm_seal":["Nimbus","Lyra"]}"#);
        assert_eq!(credits_billed(&sealed, now), ["nimbus", "lyra"]);
        for m in ["fable", "claude-fable-5-1", "claude-fable-5-1[1m]"] {
            assert!(billed_to_credits(m, &fable), "{m}");
            assert!(!billed_to_credits(m, &[]), "{m}");
        }
        for m in ["opus", "claude-opus-5-5", "sonnet"] {
            assert!(!billed_to_credits(m, &fable), "{m}");
        }
    }
}

#[cfg(test)]
mod atomic_write_tests {
    use super::*;

    #[test]
    fn an_atomic_write_leaves_an_unowned_old_stage_untouched() {
        let dir = aterm_tempfile::tempdir().expect("private directory");
        let dest = dir.path().join("record.json");
        let unrelated = dest.with_extension(format!("tmp{}", std::process::id()));
        std::fs::write(&dest, "old").expect("destination");
        std::fs::write(&unrelated, "irreplaceable fixture").expect("unowned former stage");
        write_atomic(&dest, "new").expect("actual public replacement");
        assert_eq!(std::fs::read_to_string(&dest).expect("destination"), "new");
        assert_eq!(
            std::fs::read_to_string(&unrelated).expect("unrelated"),
            "irreplaceable fixture"
        );
    }

    #[test]
    fn the_atomic_writes_name_their_flush() {
        use aterm_durable::{Flush, seam::Recording};
        let dir = aterm_tempfile::tempdir().expect("private directory");
        let dest = dir.path().join("record.json");
        let recording = Recording::start();
        write_atomic(&dest, "durable").expect("durable write");
        assert_eq!(recording.take(), [Flush::Durable]);
        write_atomic_ordered(&dest, "ordered").expect("ordered write");
        assert_eq!(recording.take(), [Flush::Ordered]);
        assert_eq!(std::fs::read_to_string(&dest).expect("body"), "ordered");
        let list = dir.path().join("models.json");
        load_or_seed(&list, 1_000).expect("seeded");
        assert_eq!(recording.take(), [Flush::Ordered], "a seed is only ordered");
        load_or_seed(&list, 2_000).expect("read");
        assert_eq!(recording.take(), [], "a present list is not written");
    }

    #[test]
    fn a_failed_atomic_write_preserves_the_destination_and_cleans_only_its_stage() {
        use std::io::Write as _;
        let dir = aterm_tempfile::tempdir().expect("private directory");
        let dest = dir.path().join("record.json");
        let unrelated = dir.path().join("notes");
        std::fs::write(&dest, "old good snapshot").expect("destination");
        std::fs::write(&unrelated, "keep me").expect("unrelated");
        let result =
            write_atomic_using(&dest, "replacement", None, Flush::Durable, |file, bytes| {
                file.write_all(&bytes[..1])?;
                Err(std::io::Error::other("injected owned-stage write failure"))
            });
        assert!(result.is_err());
        assert_eq!(
            std::fs::read_to_string(&dest).expect("destination"),
            "old good snapshot"
        );
        assert_eq!(
            std::fs::read_to_string(&unrelated).expect("unrelated"),
            "keep me"
        );
        let mut kept: Vec<_> = std::fs::read_dir(dir.path())
            .expect("directory")
            .map(|e| e.expect("entry").file_name())
            .collect();
        kept.sort();
        assert_eq!(
            kept,
            [
                std::ffi::OsString::from("notes"),
                std::ffi::OsString::from("record.json")
            ]
        );
    }

    #[cfg(unix)]
    #[test]
    fn an_atomic_replacement_publishes_its_requested_mode() {
        use std::os::unix::fs::PermissionsExt as _;
        let dir = aterm_tempfile::tempdir().expect("private directory");
        let dest = dir.path().join("record");
        write_atomic_mode(&dest, "public signature", Some(0o644)).expect("public replacement");
        assert_eq!(
            std::fs::metadata(&dest)
                .expect("metadata")
                .permissions()
                .mode()
                & 0o777,
            0o644
        );
        write_atomic(&dest, "private replacement").expect("private replacement");
        assert_eq!(
            std::fs::read_to_string(&dest).expect("body"),
            "private replacement"
        );
        assert_eq!(
            std::fs::metadata(&dest)
                .expect("metadata")
                .permissions()
                .mode()
                & 0o777,
            0o600
        );
    }
}
