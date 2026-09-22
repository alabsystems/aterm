// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! THE HARNESS'S OWN `config.toml` (design §3.7, §5.7, §5.8.7) — the keys an
//! owner may turn, read and written through ONE closed registry.
//!
//! The design's sentence for this file is literal: *"more can be handled via
//! introspection" is literally `harness config claude-harness set
//! cap.limits.actions.session-5h-limit '["wait","escalate"]'` … the per-class
//! allowed sets are enforced by the config verb … so the §11 invariants hold
//! by construction, not by default.* That is what this module is. It is not a
//! TOML library and not a settings bag:
//!
//! * every key is in [`KEYS`], with its type and its bounds. A key the
//!   registry does not name is REFUSED BY NAME, on read and on write alike —
//!   so a typo is a refusal, never a silently ignored line;
//! * every `set` is applied to a COPY, validated by
//!   [`LimitsConfig::validate`] (which runs [`limits::ActionTable::validate`],
//!   the per-class allowed sets of §5.8.7 and §11 item 7), and committed only
//!   if that passes. A refused write leaves both the value in memory and the
//!   file on disk exactly as they were;
//! * the file is re-emitted CANONICALLY from the admitted value, never
//!   patched line by line. That is safe precisely because the reader is
//!   fail-closed: a file this module admits carries no key it did not
//!   understand, so there is nothing a rewrite can lose. Comments an owner
//!   typed are the one exception, and [`HarnessConfig::to_toml`] says so in
//!   the header it writes.
//!
//! What this module does NOT do: it never reads a credential, never opens a
//! socket, and holds no clock.
//!
//! TWO keys the design prints inside a `config.toml` block are deliberately
//! ABSENT here, under §4.6.2's one-home rule:
//!
//! * `harness.enabled` — the durable master switch lives in `aterm.toml`
//!   precisely so it keeps working when the harness does not;
//! * `[accounts] enabled` — rotation's consent lives in `accounts.toml`,
//!   beside the roster it is consent ABOUT. [`super::cli::apply_accounts`]
//!   reads it from there, and a second copy here would be two separately
//!   edited values that nothing compares — the shape the update-channel
//!   pubkey was in before it was collapsed to one anchor.

use std::fmt::Write as _;
use std::path::Path;

use super::align::{Val, read_toml_dotted};
use super::limits::{self, Budget, LimitsConfig, RetryText};
use super::watch::Level;

/// The file's name inside the harness state/prefix directory.
pub const CONFIG_FILE: &str = "config.toml";

/// The most bytes a value operand may carry. Every admitted value is a
/// number, a bool, one word out of a closed set, a `n/<window>` budget or a
/// short array of action names; nothing legitimate is near this.
pub const MAX_VALUE_BYTES: usize = 512;

// ---------------------------------------------------------------------------
// The registry
// ---------------------------------------------------------------------------

/// What one key holds. The `set` parser is chosen by this and nothing else,
/// so a key can never be written through a parser its type does not name.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Kind {
    /// `true` / `false`.
    Bool,
    /// A non-negative count of seconds, hours or percent, bounded by `max`.
    Num {
        /// The inclusive upper bound; a value past it is refused by name.
        max: u64,
    },
    /// `"<count>/<n><unit>"` ([`Budget::parse`]).
    Budget,
    /// One word out of a closed set ([`RetryText`]).
    RetryText,
    /// The ladder's reach: `warn | stop | turn | escape`.
    Level,
    /// One class's ordered action row, written as a one-line array of
    /// double-quoted names — the ONE key family whose allowed set is
    /// per-class (§5.8.7).
    Actions {
        /// The class the row belongs to.
        class: limits::Class,
    },
}

/// One admitted key.
#[derive(Debug, Clone, Copy)]
pub struct KeySpec {
    /// The dotted key an owner types, e.g. `cap.limits.level`.
    pub key: &'static str,
    /// What it holds.
    pub kind: Kind,
    /// One line of help, printed by `harness config get` with no key.
    pub help: &'static str,
}

/// Every key this build admits, in the order `config get` prints them.
///
/// The `cap.limits.actions.*` rows are generated below rather than listed by
/// hand, so a class added to [`limits::Class::ALL`] cannot acquire a config
/// key that no allowed set guards — the failure mode §5.8.7 exists to close.
pub const KEYS: &[KeySpec] = &[
    KeySpec {
        key: "cap.liveness.enabled",
        kind: Kind::Bool,
        help: "the stall ladder (design §5.4)",
    },
    KeySpec {
        key: "cap.liveness.level",
        kind: Kind::Level,
        help: "how far the ladder may reach on its own initiative: warn|stop|turn|escape",
    },
    KeySpec {
        key: "cap.limits.enabled",
        kind: Kind::Bool,
        help: "the failure classifier and its recovery table (design §5.8)",
    },
    KeySpec {
        key: "cap.limits.level",
        kind: Kind::Num { max: 4 },
        help: "the maximum actuator level this capability may use; 4 is needed for switch-account",
    },
    KeySpec {
        key: "cap.limits.budget",
        kind: Kind::Budget,
        help: "automatic switches, model and account together",
    },
    KeySpec {
        key: "cap.limits.settle_s",
        kind: Kind::Num { max: 3_600 },
        help: "turn-boundary wait before a switch",
    },
    KeySpec {
        key: concat!("cap.limits.", "transient_t1_s"),
        kind: Kind::Num { max: 86_400 },
        help: "transient-capacity: the first column's dwell",
    },
    KeySpec {
        key: concat!("cap.limits.", "transient_t2_s"),
        kind: Kind::Num { max: 86_400 },
        help: "transient-capacity: the second column's dwell (never below t1)",
    },
    KeySpec {
        key: "cap.limits.network_t1_s",
        kind: Kind::Num { max: 86_400 },
        help: "network-offline: the first column's dwell",
    },
    KeySpec {
        key: "cap.limits.network_t2_s",
        kind: Kind::Num { max: 86_400 },
        help: "network-offline: the second column's dwell (never below t1)",
    },
    KeySpec {
        key: "cap.limits.min_dwell_s",
        kind: Kind::Num { max: 86_400 },
        help: "the floor between two switches",
    },
    KeySpec {
        key: "cap.limits.switch_back",
        kind: Kind::Bool,
        help: "return to the primary model when its window recovers",
    },
    KeySpec {
        key: "cap.limits.max_wait_h",
        kind: Kind::Num { max: 8_760 },
        help: "beyond this, escalate instead of waiting",
    },
    KeySpec {
        key: "cap.limits.unknown_escalate_after",
        kind: Kind::Budget,
        help: "the burst of unknown classifications that escalates",
    },
    KeySpec {
        key: "cap.limits.retry_text",
        kind: Kind::RetryText,
        help: "what `retry` types: continue | last-prompt",
    },
    KeySpec {
        key: "cap.limits.allow_low_priority",
        kind: Kind::Bool,
        help: "may spend the weekly lower-priority allowance (vendor-gated)",
    },
    KeySpec {
        key: "cap.limits.allow_limit_reset",
        kind: Kind::Bool,
        help: "may open the vendor's 1/week limit-reset confirmation for the human",
    },
    KeySpec {
        key: "cap.limits.allow_spend",
        kind: Kind::Bool,
        help: "reserved: extra-usage is unreachable in ABI 1 regardless",
    },
    KeySpec {
        key: "cap.limits.own_resume",
        kind: Kind::Bool,
        help: "the harness owns the wait/retry; false leaves it to the vendor",
    },
    KeySpec {
        key: "cap.limits.relogin",
        kind: Kind::Bool,
        help: "may type /login at a turn boundary (design §5.8.10); no credential is read",
    },
    KeySpec {
        key: "cap.limits.relogin_wait_s",
        kind: Kind::Num { max: 86_400 },
        help: "how long the harness watches while a human completes the vendor's sign-in",
    },
    KeySpec {
        key: "cap.limits.relogin_budget",
        kind: Kind::Budget,
        help: "/login attempts per account",
    },
];

/// The `cap.limits.actions.<class>` key for one class.
#[must_use]
pub fn actions_key(class: limits::Class) -> String {
    format!("cap.limits.actions.{}", class.as_str())
}

/// Every admitted key name, flat keys first then the per-class action rows.
#[must_use]
pub fn key_names() -> Vec<String> {
    let mut names: Vec<String> = KEYS.iter().map(|k| k.key.to_string()).collect();
    names.extend(limits::Class::ALL.into_iter().map(actions_key));
    names
}

/// The spec for `key`, or `None` for a name no key answers to.
#[must_use]
pub fn spec_of(key: &str) -> Option<KeySpec> {
    if let Some(found) = KEYS.iter().find(|k| k.key == key) {
        return Some(*found);
    }
    let class_name = key.strip_prefix("cap.limits.actions.")?;
    let class = limits::Class::parse(class_name)?;
    Some(KeySpec {
        key: "cap.limits.actions.<class>",
        kind: Kind::Actions { class },
        help: "this class's ordered candidates; the §5.8.7 allowed set is enforced on write",
    })
}

// ---------------------------------------------------------------------------
// The value
// ---------------------------------------------------------------------------

/// The whole admitted configuration.
#[derive(Debug, Clone, PartialEq)]
pub struct HarnessConfig {
    /// `[cap.limits]` and `[cap.limits.actions]`.
    pub limits: LimitsConfig,
    /// `[cap.liveness] enabled`.
    pub liveness_enabled: bool,
    /// `[cap.liveness] level`.
    pub liveness_level: Level,
}

impl Default for HarnessConfig {
    /// The SHIPPED table: `LimitsConfig::default()` (design §5.8.7 verbatim),
    /// rotation OFF, the ladder on and bounded at `stop` — the same defaults
    /// `WatchConfig::default()` carries, so a machine with no `config.toml`
    /// and a machine with an all-defaults one behave identically.
    fn default() -> HarnessConfig {
        HarnessConfig {
            limits: LimitsConfig::default(),
            liveness_enabled: true,
            liveness_level: Level::Stop,
        }
    }
}

impl HarnessConfig {
    /// Read `path`. A MISSING file is the shipped default and not an error —
    /// design §1.4's rule is that nothing about a harness may make the
    /// wrapped program un-launchable, and a first run has no file. Anything
    /// else (unreadable, outside the grammar, an unknown key, a value the
    /// registry refuses, a table §5.8.7's allowed set forbids) is a REFUSAL
    /// naming the cause, fail-closed and whole-file.
    ///
    /// # Errors
    ///
    /// The file could not be read, left the grammar, named a key or table the
    /// registry does not carry, or failed [`LimitsConfig::validate`].
    pub fn load(path: &Path) -> Result<HarnessConfig, String> {
        let text = match std::fs::read_to_string(path) {
            Ok(text) => text,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
                return Ok(HarnessConfig::default());
            }
            Err(e) => return Err(format!("{}: {e}", path.display())),
        };
        HarnessConfig::parse(&text, &path.display().to_string())
    }

    /// [`Self::load`]'s pure half, so a test never needs a file.
    ///
    /// # Errors
    ///
    /// As [`Self::load`].
    pub fn parse(text: &str, file: &str) -> Result<HarnessConfig, String> {
        let blocks = read_toml_dotted(text, file)?;
        let mut cfg = HarnessConfig::default();
        for block in &blocks {
            // The ROOT block may only be empty: a bare `level = 3` at the top
            // of the file names no table and would otherwise be read as a key
            // of whatever table happens to be first.
            let table = block.name.as_str();
            if table.is_empty() {
                if let Some((k, _)) = block.keys.first() {
                    return Err(format!(
                        "{file}: `{k}` sits before any [table]; every key in this file is under \
                         one (`{}`)",
                        key_names().join("`, `")
                    ));
                }
                continue;
            }
            if block.array {
                return Err(format!(
                    "{file}: [[{table}]] — this file carries no array tables"
                ));
            }
            for (key, val) in &block.keys {
                let dotted = format!("{table}.{key}");
                let Some(spec) = spec_of(&dotted) else {
                    return Err(unknown_key(file, &dotted));
                };
                apply_val(&mut cfg, &dotted, spec.kind, val, file)?;
            }
        }
        cfg.validate(file)?;
        Ok(cfg)
    }

    /// Every invariant this file owes, named by file.
    fn validate(&self, file: &str) -> Result<(), String> {
        self.limits
            .validate()
            .map_err(|e| format!("{file}: {e} — the table is refused whole (design §5.8.7)"))
    }

    /// One key's value, rendered exactly as `to_toml` would write it. `None`
    /// for a key the registry does not carry.
    #[must_use]
    pub fn get(&self, key: &str) -> Option<String> {
        let spec = spec_of(key)?;
        Some(match spec.kind {
            Kind::Actions { class } => render_actions(self.limits.actions.get(class)),
            _ => self.scalar(key, spec.kind)?,
        })
    }

    fn scalar(&self, key: &str, kind: Kind) -> Option<String> {
        let l = &self.limits;
        let b = |v: bool| v.to_string();
        let n = |v: u64| v.to_string();
        let text = match key {
            "cap.liveness.enabled" => b(self.liveness_enabled),
            "cap.liveness.level" => format!("\"{}\"", self.liveness_level.as_str()),
            "cap.limits.enabled" => b(l.enabled),
            "cap.limits.level" => n(u64::from(l.level)),
            "cap.limits.budget" => format!("\"{}\"", l.budget.as_string()),
            "cap.limits.settle_s" => n(l.settle_s),
            "cap.limits.transient_t1_s" => n(l.transient_t1_s),
            "cap.limits.transient_t2_s" => n(l.transient_t2_s),
            "cap.limits.network_t1_s" => n(l.network_t1_s),
            "cap.limits.network_t2_s" => n(l.network_t2_s),
            "cap.limits.min_dwell_s" => n(l.min_dwell_s),
            "cap.limits.switch_back" => b(l.switch_back),
            "cap.limits.max_wait_h" => n(l.max_wait_h),
            "cap.limits.unknown_escalate_after" => {
                format!("\"{}\"", l.unknown_escalate_after.as_string())
            }
            "cap.limits.retry_text" => format!("\"{}\"", l.retry_text.as_str()),
            "cap.limits.allow_low_priority" => b(l.allow_low_priority),
            "cap.limits.allow_limit_reset" => b(l.allow_limit_reset),
            "cap.limits.allow_spend" => b(l.allow_spend),
            "cap.limits.own_resume" => b(l.own_resume),
            "cap.limits.relogin" => b(l.relogin),
            "cap.limits.relogin_wait_s" => n(l.relogin_wait_s),
            "cap.limits.relogin_budget" => format!("\"{}\"", l.relogin_budget.as_string()),
            _ => return None,
        };
        let _ = kind;
        Some(text)
    }

    /// Write one key from an OPERAND as a person types it — `3`, `true`,
    /// `4/6h`, `continue`, `["wait","escalate"]` — with or without the
    /// surrounding quotes a TOML value would carry.
    ///
    /// The write lands on a COPY and is committed only once the whole
    /// configuration validates, so a refused `set` changes nothing: this is
    /// where §5.8.7's *"the per-class allowed sets are enforced by the config
    /// verb, so the §11 invariants hold by construction"* is discharged.
    ///
    /// # Errors
    ///
    /// An unknown key, a value outside its type or bounds, or a table the
    /// class's allowed set refuses.
    pub fn set(&mut self, key: &str, operand: &str) -> Result<(), String> {
        if operand.len() > MAX_VALUE_BYTES {
            return Err(format!(
                "{key}: a value of {} bytes, over the {MAX_VALUE_BYTES}-byte bound",
                operand.len()
            ));
        }
        let Some(spec) = spec_of(key) else {
            return Err(unknown_key("config", key));
        };
        let mut next = self.clone();
        apply_operand(&mut next, key, spec.kind, operand)?;
        next.validate("config")?;
        *self = next;
        Ok(())
    }

    /// The whole file, canonically. Round-trips: `parse(to_toml(c)) == c`.
    #[must_use]
    pub fn to_toml(&self) -> String {
        let mut s = String::from(
            "# The harness's own config.toml (design §3.7, §5.8.7).\n\
             # Written by `aterm harness config set`, which re-emits the WHOLE file from the\n\
             # values it admitted — hand-written comments below this header are not kept.\n\
             # `harness.enabled` is NOT here: the durable master switch lives in aterm.toml so\n\
             # that it keeps working when the harness does not (design §4.6.2).\n",
        );
        let mut table = String::new();
        for key in key_names() {
            let Some((head, leaf)) = key.rsplit_once('.') else {
                continue;
            };
            if head != table {
                let _ = write!(s, "\n[{head}]\n");
                head.clone_into(&mut table);
            }
            let value = self.get(&key).unwrap_or_default();
            let _ = writeln!(s, "{leaf} = {value}");
        }
        s
    }
}

fn render_actions(row: &[limits::Action]) -> String {
    let inner: Vec<String> = row.iter().map(|a| format!("\"{}\"", a.as_str())).collect();
    format!("[{}]", inner.join(", "))
}

fn unknown_key(file: &str, key: &str) -> String {
    format!(
        "{file}: `{key}` is not a key this build carries. `aterm harness config get` lists every \
         one; an unknown key is refused rather than ignored, so a typo can never read as a \
         setting that took effect"
    )
}

/// Apply one parsed TOML value.
fn apply_val(
    cfg: &mut HarnessConfig,
    key: &str,
    kind: Kind,
    val: &Val,
    file: &str,
) -> Result<(), String> {
    let operand = match (kind, val) {
        (Kind::Bool, Val::Bool(b)) => b.to_string(),
        (Kind::Num { .. }, Val::Int(n)) => n.to_string(),
        (Kind::Budget | Kind::RetryText | Kind::Level, Val::Str(s)) => s.clone(),
        (Kind::Actions { .. }, Val::Arr(items)) => items.join(","),
        _ => {
            return Err(format!(
                "{file}: `{key}` has a value of the wrong shape — it wants {}",
                wants(kind)
            ));
        }
    };
    apply_operand(cfg, key, kind, &operand).map_err(|e| format!("{file}: {e}"))
}

/// What a key's type accepts, in one phrase.
fn wants(kind: Kind) -> String {
    match kind {
        Kind::Bool => "true or false".to_string(),
        Kind::Num { max } => format!("a whole number from 0 to {max}"),
        Kind::Budget => "a budget, e.g. \"4/6h\"".to_string(),
        Kind::RetryText => "continue or last-prompt".to_string(),
        Kind::Level => "warn, stop, turn or escape".to_string(),
        Kind::Actions { class } => format!(
            "an array of action names for {class}, e.g. [\"wait\", \"escalate\"]; the actions are {}",
            limits::Action::ALL
                .iter()
                .map(|a| a.as_str())
                .collect::<Vec<_>>()
                .join(", ")
        ),
    }
}

/// Apply one OPERAND as a person types it.
fn apply_operand(
    cfg: &mut HarnessConfig,
    key: &str,
    kind: Kind,
    operand: &str,
) -> Result<(), String> {
    let raw = operand.trim();
    match kind {
        Kind::Bool => {
            let v = match unquote(raw) {
                "true" => true,
                "false" => false,
                _ => return Err(format!("{key}: {raw:?} — it wants {}", wants(kind))),
            };
            set_bool(cfg, key, v)
        }
        Kind::Num { max } => {
            let v: u64 = unquote(raw)
                .parse()
                .map_err(|_| format!("{key}: {raw:?} — it wants {}", wants(kind)))?;
            if v > max {
                return Err(format!("{key}: {v} is above the bound {max}"));
            }
            set_num(cfg, key, v)
        }
        Kind::Budget => {
            let b = Budget::parse(unquote(raw))
                .ok_or_else(|| format!("{key}: {raw:?} — it wants {}", wants(kind)))?;
            set_budget(cfg, key, b)
        }
        Kind::RetryText => {
            let t = RetryText::parse(unquote(raw))
                .ok_or_else(|| format!("{key}: {raw:?} — it wants {}", wants(kind)))?;
            cfg.limits.retry_text = t;
            Ok(())
        }
        Kind::Level => {
            let l = Level::parse(unquote(raw))
                .ok_or_else(|| format!("{key}: {raw:?} — it wants {}", wants(kind)))?;
            cfg.liveness_level = l;
            Ok(())
        }
        Kind::Actions { class } => {
            let names = split_actions(raw);
            let refs: Vec<&str> = names.iter().map(String::as_str).collect();
            cfg.limits
                .actions
                .set_names(class, &refs)
                .map_err(|e| format!("{key}: {e}"))
        }
    }
}

/// `["wait", "escalate"]`, `wait,escalate` and `wait escalate` all name the
/// same row: a person typing the verb at a shell prompt has already fought
/// one layer of quoting, and refusing the unbracketed spelling teaches
/// nothing. The NAMES are still closed — an unknown one refuses the whole
/// row in [`limits::ActionTable::set_names`].
fn split_actions(raw: &str) -> Vec<String> {
    let inner = raw
        .strip_prefix('[')
        .and_then(|r| r.strip_suffix(']'))
        .unwrap_or(raw);
    inner
        .split([',', ' ', '\t'])
        .map(|t| unquote(t.trim()).to_string())
        .filter(|t| !t.is_empty())
        .collect()
}

/// Strip ONE layer of matching double quotes, so `set … '"4/6h"'` and
/// `set … 4/6h` mean the same thing.
fn unquote(s: &str) -> &str {
    let s = s.trim();
    s.strip_prefix('"')
        .and_then(|r| r.strip_suffix('"'))
        .unwrap_or(s)
}

fn set_bool(cfg: &mut HarnessConfig, key: &str, v: bool) -> Result<(), String> {
    let l = &mut cfg.limits;
    match key {
        "cap.liveness.enabled" => cfg.liveness_enabled = v,
        "cap.limits.enabled" => l.enabled = v,
        "cap.limits.switch_back" => l.switch_back = v,
        "cap.limits.allow_low_priority" => l.allow_low_priority = v,
        "cap.limits.allow_limit_reset" => l.allow_limit_reset = v,
        "cap.limits.allow_spend" => l.allow_spend = v,
        "cap.limits.own_resume" => l.own_resume = v,
        "cap.limits.relogin" => l.relogin = v,
        _ => return Err(unknown_key("config", key)),
    }
    Ok(())
}

fn set_num(cfg: &mut HarnessConfig, key: &str, v: u64) -> Result<(), String> {
    let l = &mut cfg.limits;
    match key {
        // The narrowing cast is bounded by the registry above (`level` at
        // 4), so it cannot truncate; the fallible form is used anyway so a
        // future bound that moves cannot silently wrap.
        "cap.limits.level" => l.level = u8::try_from(v).map_err(|_| bound(key, v))?,
        "cap.limits.settle_s" => l.settle_s = v,
        "cap.limits.transient_t1_s" => l.transient_t1_s = v,
        "cap.limits.transient_t2_s" => l.transient_t2_s = v,
        "cap.limits.network_t1_s" => l.network_t1_s = v,
        "cap.limits.network_t2_s" => l.network_t2_s = v,
        "cap.limits.min_dwell_s" => l.min_dwell_s = v,
        "cap.limits.max_wait_h" => l.max_wait_h = v,
        "cap.limits.relogin_wait_s" => l.relogin_wait_s = v,
        _ => return Err(unknown_key("config", key)),
    }
    Ok(())
}

fn bound(key: &str, v: u64) -> String {
    format!("{key}: {v} is outside what this key can hold")
}

fn set_budget(cfg: &mut HarnessConfig, key: &str, b: Budget) -> Result<(), String> {
    let l = &mut cfg.limits;
    match key {
        "cap.limits.budget" => l.budget = b,
        "cap.limits.unknown_escalate_after" => l.unknown_escalate_after = b,
        "cap.limits.relogin_budget" => l.relogin_budget = b,
        _ => return Err(unknown_key("config", key)),
    }
    Ok(())
}

#[path = "config_tests.rs"]
#[cfg(test)]
mod tests;
