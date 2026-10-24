// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! agy's model quota: the document `agy -p /usage --output-format json` prints, read into its model
//! groups (Gemini; Claude and GPT), each with its own weekly and five-hour bucket.

use std::fmt::Write as _;
use std::time::Duration;

use aterm_json::{Map, Value};

/// The arguments that make agy print its quota and exit without a model turn.
pub const USAGE_ARGS: [&str; 4] = ["-p", "/usage", "--output-format", "json"];

/// The arguments that make agy print its default model and exit without a model turn.
pub const MODEL_ARGS: [&str; 4] = ["-p", "/model", "--output-format", "json"];

/// How long `agy -p /usage` may take before the read is abandoned.
pub const READ_BUDGET: Duration = Duration::from_secs(30);

/// The most stdout a quota read keeps.
pub const MAX_STDOUT: usize = 256 * 1024;

/// Which limit a bucket counts against.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Window {
    /// `weekly`: tied to the account's tier.
    Weekly,
    /// `5h`: the rolling five-hour limit.
    FiveHour,
    /// A window this reader does not know, by its raw name.
    Other(String),
}

impl Window {
    fn of(raw: &str) -> Self {
        match raw {
            "weekly" => Self::Weekly,
            "5h" => Self::FiveHour,
            other => Self::Other(other.to_owned()),
        }
    }

    /// The short word the HUD prints.
    #[must_use]
    pub fn word(&self) -> &str {
        match self {
            Self::Weekly => "week",
            Self::FiveHour => "5h",
            Self::Other(raw) => raw,
        }
    }

    /// The name the JSON form carries (agy's own).
    #[must_use]
    pub fn raw(&self) -> &str {
        match self {
            Self::Weekly => "weekly",
            Self::FiveHour => "5h",
            Self::Other(raw) => raw,
        }
    }
}

/// One limit of one model group.
#[derive(Debug, Clone, PartialEq)]
pub struct Bucket {
    /// agy's id (`gemini-weekly`, `3p-5h`, …).
    pub id: String,
    /// Which limit it is.
    pub window: Window,
    /// The fraction left, clamped to `0..=1`; `None` when agy gave none.
    pub remaining: Option<f64>,
    /// When it refills fully, as agy wrote it (RFC 3339, UTC).
    pub reset: Option<String>,
    /// agy says the limit does not apply now (a five-hour limit while the weekly one is spent).
    pub disabled: bool,
    /// agy's sentence about it.
    pub said: Option<String>,
}

impl Bucket {
    /// Spent: nothing left and the limit applies.
    #[must_use]
    pub fn spent(&self) -> bool {
        !self.disabled && self.remaining.is_some_and(|f| f <= 0.0)
    }

    /// The reset time as Unix seconds.
    #[must_use]
    pub fn reset_at(&self) -> Option<i64> {
        self.reset
            .as_deref()
            .and_then(aterm_types::rfc3339::parse_utc_fractional)
    }
}

/// Models that share one weekly and one five-hour limit.
#[derive(Debug, Clone, PartialEq)]
pub struct Group {
    /// agy's name (`Gemini Models`, `Claude and GPT models`).
    pub name: String,
    /// agy's line naming the models in it.
    pub models: Option<String>,
    /// Its limits, in agy's order.
    pub buckets: Vec<Bucket>,
}

impl Group {
    /// The short name the HUD prints.
    #[must_use]
    pub fn short(&self) -> String {
        let lower = self.name.to_ascii_lowercase();
        if lower.starts_with("gemini") {
            "gemini".to_owned()
        } else if lower.contains("claude") {
            "claude/gpt".to_owned()
        } else {
            lower.trim_end_matches(" models").to_owned()
        }
    }

    /// The bucket for `window`.
    #[must_use]
    pub fn bucket(&self, window: &Window) -> Option<&Bucket> {
        self.buckets.iter().find(|b| &b.window == window)
    }

    /// Any limit of this group is spent, so its models cannot run now.
    #[must_use]
    pub fn blocked(&self) -> bool {
        self.buckets.iter().any(Bucket::spent)
    }

    /// When the spent limits refill: the latest reset among them.
    #[must_use]
    pub fn unblocks_at(&self) -> Option<i64> {
        self.buckets
            .iter()
            .filter(|b| b.spent())
            .filter_map(Bucket::reset_at)
            .max()
    }
}

/// How much a pool has left, as rotation reads it ([`Group::standing`]).
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum Standing {
    /// Every applying limit is above the low mark.
    Open,
    /// Some applying limit is at or under the low mark, none spent: the pool is running out.
    Low,
    /// Some applying limit is spent: the pool's models cannot run until it refills.
    Spent,
}

impl Standing {
    /// The word the HUD, the ledger and `--json` use.
    #[must_use]
    pub const fn word(self) -> &'static str {
        match self {
            Self::Open => "open",
            Self::Low => "low",
            Self::Spent => "spent",
        }
    }
}

impl Group {
    /// The least fraction left among the limits that apply now; `None` when none says.
    #[must_use]
    pub fn least_left(&self) -> Option<f64> {
        self.buckets
            .iter()
            .filter(|b| !b.disabled)
            .filter_map(|b| b.remaining)
            .reduce(f64::min)
    }

    /// Open, low (a limit at or under `low`) or spent.
    #[must_use]
    pub fn standing(&self, low: f64) -> Standing {
        if self.blocked() {
            Standing::Spent
        } else if self.least_left().is_some_and(|f| f <= low) {
            Standing::Low
        } else {
            Standing::Open
        }
    }

    /// Does this pool hold `model` (an agy id such as `claude-opus-5-5-medium` or a label such
    /// as `Gemini 4 Argon (High)`)? Read by the model's family against the pool's name and its
    /// "Models within this group" line.
    #[must_use]
    pub fn holds(&self, model: &str) -> bool {
        let family = family(model);
        !family.is_empty()
            && (self.name.to_ascii_lowercase().contains(&family)
                || self
                    .models
                    .as_deref()
                    .is_some_and(|m| m.to_ascii_lowercase().contains(&family)))
    }
}

/// A model's family: `gemini`, `claude`, `gpt-oss`, … — its first word, lowercased, `gpt-oss`
/// kept whole.
#[must_use]
pub fn family(model: &str) -> String {
    let lower = model.trim().to_ascii_lowercase();
    if lower.starts_with("gpt-oss") || lower.starts_with("gpt oss") {
        return "gpt-oss".to_owned();
    }
    lower
        .split(['-', ' ', '_'])
        .next()
        .unwrap_or_default()
        .to_owned()
}

/// agy's quota, every group.
#[derive(Debug, Clone, PartialEq, Default)]
pub struct Quota {
    /// The groups, in agy's order.
    pub groups: Vec<Group>,
    /// agy's paragraph on how the limits work.
    pub about: Option<String>,
}

impl Quota {
    /// The pool `model` draws on.
    #[must_use]
    pub fn group_of(&self, model: &str) -> Option<&Group> {
        self.groups.iter().find(|g| g.holds(model))
    }
}

/// The low mark: a pool with a limit at or under this fraction is running out ([`Standing::Low`]).
pub const DEFAULT_LOW: f64 = 0.10;

/// The models rotation tries, in order, when the one asked for draws on a pool that is low or
/// spent: the strongest of each family first, so a rotation crosses pools before it trades down.
pub const DEFAULT_ORDER: &[&str] = &[
    "claude-opus-5-5-medium",
    "gemini-4-argon-high",
    "claude-sonnet-5-5-medium",
    "gemini-3.8-flash-medium",
    "gpt-oss-120b-medium",
];

/// How rotation chooses ([`pick`]).
#[derive(Debug, Clone, PartialEq)]
pub struct Rotation {
    /// Rotation on at all (`[harness.agy] rotate`).
    pub enabled: bool,
    /// The low mark (`[harness.agy] low`, a fraction).
    pub low: f64,
    /// The candidates in order (`[harness.agy] order`).
    pub order: Vec<String>,
}

impl Default for Rotation {
    fn default() -> Self {
        Self {
            enabled: true,
            low: DEFAULT_LOW,
            order: DEFAULT_ORDER.iter().map(|m| (*m).to_owned()).collect(),
        }
    }
}

/// The `[harness.agy]` keys rotation reads ([`Rotation::from_aterm_toml`]).
pub const ROTATION_KEYS: &[&str] = &["rotate", "low", "order"];

impl Rotation {
    /// Read `[harness.agy]`'s `rotate` (bool), `low` (a fraction, or a percent above 1) and
    /// `order` (model ids or labels) from aterm.toml's text; a key that is absent or malformed
    /// keeps its default, and each malformed one is a note.
    #[must_use]
    pub fn from_aterm_toml(text: &str) -> (Self, Vec<String>) {
        let mut r = Self::default();
        let mut notes = Vec::new();
        let Ok(doc) = aterm_toml::from_str::<aterm_toml::Value>(text) else {
            return (r, notes);
        };
        let Some(agy) = doc
            .get("harness")
            .and_then(|h| h.get("agy"))
            .and_then(aterm_toml::Value::as_table)
        else {
            return (r, notes);
        };
        if let Some(v) = agy.get("rotate") {
            match v.as_bool() {
                Some(on) => r.enabled = on,
                None => notes
                    .push("harness.agy.rotate: wants true or false; rotation stays on".to_owned()),
            }
        }
        if let Some(v) = agy.get("low") {
            let f = v.as_float().or_else(|| v.as_integer().map(|i| i as f64));
            match f.map(|f| if f > 1.0 { f / 100.0 } else { f }) {
                Some(f) if (0.0..1.0).contains(&f) => r.low = f,
                _ => notes.push(format!(
                    "harness.agy.low: wants a fraction under 1 (0.1) or a percent (10); {} stays",
                    DEFAULT_LOW
                )),
            }
        }
        if let Some(v) = agy.get("order") {
            let order: Option<Vec<String>> = v.as_array().map(|a| {
                a.iter()
                    .filter_map(aterm_toml::Value::as_str)
                    .map(str::trim)
                    .filter(|m| !m.is_empty())
                    .map(str::to_owned)
                    .collect()
            });
            match order.filter(|o| !o.is_empty()) {
                Some(o) => r.order = o,
                None => notes.push(
                    "harness.agy.order: wants a list of agy model ids; the default order stays"
                        .to_owned(),
                ),
            }
        }
        (r, notes)
    }

    /// [`Self::from_aterm_toml`] over the file at `path`: no path or no file is the defaults.
    #[must_use]
    pub fn from_path(path: Option<&std::path::Path>) -> (Self, Vec<String>) {
        match path.and_then(|p| super::accounts::read_bounded(p, 256 * 1024).ok().flatten()) {
            Some(text) => Self::from_aterm_toml(&text),
            None => (Self::default(), Vec::new()),
        }
    }
}

/// What rotation decided for one launch.
#[derive(Debug, Clone, PartialEq)]
pub enum Pick {
    /// Run the model asked for: its pool is open, or it was named and its pool is only low.
    Keep {
        /// The model.
        model: String,
        /// Its pool's standing.
        standing: Standing,
    },
    /// Run another model, in a pool with more left.
    Rotate {
        /// The model asked for.
        from: String,
        /// Its pool's standing.
        from_standing: Standing,
        /// The model to run instead.
        to: String,
        /// The new model's pool's standing.
        to_standing: Standing,
    },
    /// Every candidate's pool is spent: nothing can run before `until`.
    Wait {
        /// The model asked for.
        model: String,
        /// The earliest refill of a candidate's pool.
        until: Option<i64>,
    },
    /// The quota names no pool for the model asked for; it runs unchanged.
    Unknown {
        /// The model.
        model: String,
    },
}

impl Pick {
    /// The model the launch runs, or `None` when it must wait.
    #[must_use]
    pub fn model(&self) -> Option<&str> {
        match self {
            Self::Keep { model, .. } | Self::Unknown { model } => Some(model),
            Self::Rotate { to, .. } => Some(to),
            Self::Wait { .. } => None,
        }
    }

    /// One line for a ledger or a report.
    #[must_use]
    pub fn describe(&self) -> String {
        match self {
            Self::Keep { model, standing } => {
                format!("{model} kept (its pool is {})", standing.word())
            }
            Self::Rotate {
                from,
                from_standing,
                to,
                to_standing,
            } => format!(
                "{from} rotated to {to}: {from}'s pool is {}, {to}'s is {}",
                from_standing.word(),
                to_standing.word()
            ),
            Self::Wait { model, until } => match until {
                Some(at) => format!(
                    "{model} waits: every candidate's pool is spent until {}",
                    super::usage::rfc3339_utc(*at)
                ),
                None => format!("{model} waits: every candidate's pool is spent"),
            },
            Self::Unknown { model } => format!("{model} kept (agy's quota names no pool for it)"),
        }
    }
}

/// Choose the model for a launch of `asked` under `quota`. `named` is true when the person or
/// the caller named the model (a `model=` or `--model`), false when it is agy's default.
///
/// - Open pool: keep.
/// - Low pool: a named model is kept; the default rotates to the first candidate in an open pool.
/// - Spent pool: rotate to the first candidate in an open pool, else to the candidate in a low
///   pool with the most left, else wait for the earliest refill.
#[must_use]
pub fn pick(quota: &Quota, asked: &str, named: bool, rotation: &Rotation) -> Pick {
    let Some(group) = quota.group_of(asked) else {
        return Pick::Unknown {
            model: asked.to_owned(),
        };
    };
    let standing = group.standing(rotation.low);
    let keep = Pick::Keep {
        model: asked.to_owned(),
        standing,
    };
    if !rotation.enabled || standing == Standing::Open || (standing == Standing::Low && named) {
        return keep;
    }
    let candidates: Vec<(&str, &Group)> = rotation
        .order
        .iter()
        .filter_map(|m| quota.group_of(m).map(|g| (m.as_str(), g)))
        .filter(|(_, g)| g.name != group.name)
        .collect();
    let open = candidates
        .iter()
        .find(|(_, g)| g.standing(rotation.low) == Standing::Open);
    let low = || {
        let left = |g: &Group| g.least_left().unwrap_or(0.0);
        candidates
            .iter()
            .filter(|(_, g)| g.standing(rotation.low) == Standing::Low)
            .fold(None, |best: Option<&(&str, &Group)>, c| match best {
                Some(b) if left(b.1) >= left(c.1) => Some(b),
                _ => Some(c),
            })
    };
    let to = match standing {
        Standing::Low => open,
        _ => open.or_else(low),
    };
    match to {
        Some((to, g)) => Pick::Rotate {
            from: asked.to_owned(),
            from_standing: standing,
            to: (*to).to_owned(),
            to_standing: g.standing(rotation.low),
        },
        None if standing == Standing::Spent => Pick::Wait {
            model: asked.to_owned(),
            until: std::iter::once(group)
                .chain(candidates.iter().map(|(_, g)| *g))
                .filter_map(Group::unblocks_at)
                .min(),
        },
        None => keep,
    }
}

fn text(obj: &Map, key: &str) -> Option<String> {
    obj.get(key)
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(str::to_owned)
}

fn bucket_of(obj: &Map) -> Option<Bucket> {
    let id = text(obj, "id")?;
    let window = Window::of(&text(obj, "window").unwrap_or_default());
    let remaining = obj
        .get("remaining_fraction")
        .and_then(Value::as_f64)
        .filter(|f| f.is_finite())
        .map(|f| f.clamp(0.0, 1.0));
    Some(Bucket {
        id,
        window,
        remaining,
        reset: text(obj, "reset_time"),
        disabled: obj
            .get("disabled")
            .and_then(Value::as_bool)
            .unwrap_or(false),
        said: text(obj, "description"),
    })
}

/// Read the document `agy -p /usage --output-format json` printed.
///
/// # Errors
///
/// The text is not JSON, agy reported a failure, or the document names no `usage` command data.
pub fn parse(doc: &str) -> Result<Quota, String> {
    let root: Value = aterm_json::from_str(doc.trim())
        .map_err(|e| format!("agy's /usage output is not JSON: {e}"))?;
    let root = root
        .as_object()
        .ok_or_else(|| "agy's /usage output is not a JSON object".to_owned())?;
    if let Some(status) = text(root, "status")
        && status != "SUCCESS"
    {
        let why = text(root, "response").unwrap_or_default();
        return Err(format!("agy's /usage reported {status}: {why}"));
    }
    let command = root
        .get("command")
        .and_then(Value::as_object)
        .ok_or_else(|| {
            "agy's /usage output carries no `command` (an agy too old for -p /usage?)".to_owned()
        })?;
    if text(command, "name").as_deref() != Some("usage") {
        return Err("agy's output is not the /usage command's".to_owned());
    }
    let data = command
        .get("data")
        .and_then(Value::as_object)
        .ok_or_else(|| "agy's /usage output carries no `command.data`".to_owned())?;
    let groups = data
        .get("groups")
        .and_then(Value::as_array)
        .map(|groups| {
            groups
                .iter()
                .filter_map(Value::as_object)
                .filter_map(|g| {
                    Some(Group {
                        name: text(g, "name")?,
                        models: text(g, "description"),
                        buckets: g
                            .get("buckets")
                            .and_then(Value::as_array)
                            .map(|b| {
                                b.iter()
                                    .filter_map(Value::as_object)
                                    .filter_map(bucket_of)
                                    .collect()
                            })
                            .unwrap_or_default(),
                    })
                })
                .collect::<Vec<_>>()
        })
        .unwrap_or_default();
    if groups.is_empty() {
        return Err("agy's /usage output names no model group".to_owned());
    }
    Ok(Quota {
        groups,
        about: text(data, "description"),
    })
}

/// The model id `agy -p /model --output-format json` names (`command.data.id`).
///
/// # Errors
///
/// The text is not agy's `/model` document.
pub fn parse_model(doc: &str) -> Result<String, String> {
    let root: Value = aterm_json::from_str(doc.trim())
        .map_err(|e| format!("agy's /model output is not JSON: {e}"))?;
    root.get("command")
        .filter(|c| c.get("name").and_then(Value::as_str) == Some("model"))
        .and_then(|c| c.get("data"))
        .and_then(|d| d.get("id"))
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|id| !id.is_empty())
        .map(str::to_owned)
        .ok_or_else(|| "agy's /model output names no model id".to_owned())
}

/// A fraction left as a whole percent: never `0%` while some is left, never `100%` while some is used.
#[must_use]
pub fn percent(fraction: f64) -> String {
    let p = (fraction * 100.0).round();
    if fraction > 0.0 && p < 1.0 {
        "<1%".to_owned()
    } else if fraction < 1.0 && p >= 100.0 {
        "99%".to_owned()
    } else {
        format!("{p:.0}%")
    }
}

/// Seconds until a moment as a short span (`3d23h`, `4h54m`, `12m`); `now` when it has passed.
#[must_use]
pub fn span(secs: i64) -> String {
    if secs <= 0 {
        return "now".to_owned();
    }
    let (d, h, m) = (secs / 86_400, secs % 86_400 / 3_600, secs % 3_600 / 60);
    match (d, h) {
        (0, 0) => format!("{}m", m.max(1)),
        (0, _) => format!("{h}h{m:02}m"),
        _ => format!("{d}d{h}h"),
    }
}

fn bucket_text(b: &Bucket, now: i64) -> String {
    let mut s = format!("{} ", b.window.word());
    if b.disabled {
        s.push_str("off");
        return s;
    }
    match b.remaining {
        Some(f) => {
            let _ = write!(s, "{} left", percent(f));
        }
        None => s.push('?'),
    }
    if let Some(at) = b.reset_at()
        && b.remaining.is_none_or(|f| f < 1.0)
    {
        let _ = write!(s, " (refills in {})", span(at - now));
    }
    s
}

/// One line for the terminal: every group's limits, a spent one marked `SPENT`.
#[must_use]
pub fn hud_line(quota: &Quota, now: i64) -> String {
    let groups: Vec<String> = quota
        .groups
        .iter()
        .map(|g| {
            let limits: Vec<String> = g.buckets.iter().map(|b| bucket_text(b, now)).collect();
            let mark = if g.blocked() { " SPENT" } else { "" };
            format!("{}{mark}: {}", g.short(), limits.join(", "))
        })
        .collect();
    format!("agy quota \u{b7} {}", groups.join(" | "))
}

/// The `harness usage --json` body for agy (`kind: "agy-quota"`).
#[must_use]
pub fn to_json(quota: &Quota, now: i64) -> Value {
    let groups: Vec<Value> = quota
        .groups
        .iter()
        .map(|g| {
            let mut o = Map::new();
            o.insert("name".to_owned(), Value::from(g.name.clone()));
            o.insert("short".to_owned(), Value::from(g.short()));
            o.insert("models".to_owned(), Value::from(g.models.clone()));
            o.insert("blocked".to_owned(), Value::from(g.blocked()));
            o.insert("unblocks_at".to_owned(), Value::from(g.unblocks_at()));
            let buckets: Vec<Value> = g
                .buckets
                .iter()
                .map(|b| {
                    let mut o = Map::new();
                    o.insert("id".to_owned(), Value::from(b.id.clone()));
                    o.insert("window".to_owned(), Value::from(b.window.raw()));
                    o.insert("remaining_fraction".to_owned(), Value::from(b.remaining));
                    o.insert("reset_time".to_owned(), Value::from(b.reset.clone()));
                    o.insert(
                        "reset_in_s".to_owned(),
                        Value::from(b.reset_at().map(|at| (at - now).max(0))),
                    );
                    o.insert("disabled".to_owned(), Value::from(b.disabled));
                    o.insert("spent".to_owned(), Value::from(b.spent()));
                    o.insert("said".to_owned(), Value::from(b.said.clone()));
                    Value::Object(o)
                })
                .collect();
            o.insert("buckets".to_owned(), Value::from(buckets));
            Value::Object(o)
        })
        .collect();
    let mut o = Map::new();
    o.insert("schema".to_owned(), Value::from(1u64));
    o.insert("kind".to_owned(), Value::from("agy-quota"));
    o.insert("source".to_owned(), Value::from("agy -p /usage"));
    o.insert(
        "as_of".to_owned(),
        Value::from(super::usage::rfc3339_utc(now)),
    );
    o.insert("groups".to_owned(), Value::from(groups));
    o.insert("about".to_owned(), Value::from(quota.about.clone()));
    Value::Object(o)
}

#[cfg(test)]
#[path = "agy_quota_tests.rs"]
mod tests;
