// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The supervisor's policy switches, as the owner writes them: the keys of
//! aterm.toml's `[harness]` table, whose `enabled` is the Settings ▸ Harness
//! row. The in-GUI host parses the table into a [`SupervisorConfig`] with
//! [`SupervisorConfig::set`] and runs every agent session's loop under
//! [`SuperviseOpts::hosted_with`]; the defaults are the owner decisions the
//! 2026-09-23 harness audit adopted (docs: the audit's synthesis §3), so a
//! file with no `[harness]` table supervises with all of them on.
//!
//! One struct, so the host and the engine cannot disagree on a key's name or
//! its default: a key the host does not know is refused by name here, never
//! ignored.

use std::path::PathBuf;

use super::policy::approval::ApprovalToggles;
use super::run::SuperviseOpts;

/// Every `[harness]` key [`SupervisorConfig::set`] accepts, in the order the
/// help and the Settings page list them.
pub const KEYS: &[&str] = &[
    "enabled",
    "headless",
    "auto_reads",
    "rm_breaker",
    "read_outside_cwd",
    "trust_dialog",
    "trust_roots",
    "dismiss_surveys",
    "continue",
    "continue_per_hour",
    "continue_text",
    "rules_file",
    "retry_api_errors",
    "resume_limits",
    "model_fallback",
    "compact_on_context_wall",
    "upgrade",
];

/// The owner's policy for every supervised agent session.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SupervisorConfig {
    /// The master switch (Settings ▸ Harness). Off: no session is supervised.
    pub enabled: bool,
    /// Supervise sessions of a HEADLESS instance too. Off by default, so a
    /// test or agent-private instance never acts on what it hosts.
    pub headless: bool,
    /// Which approval rules may press an approving option (owner decision 1).
    pub approvals: ApprovalToggles,
    /// Folders whose trust dialog is answered "Yes, I trust this folder".
    /// A leading `~/` is the home directory; a trailing `*` matches any
    /// suffix of the last component (`~/aterm*` is every aterm worktree).
    pub trust_roots: Vec<String>,
    /// Dismiss Claude Code's session survey (`0`, never a rating).
    pub dismiss_surveys: bool,
    /// Type a continuation when a worker ends a turn after real work with no
    /// box, wall or decision asked of the human (owner decision 2).
    pub continue_policy: bool,
    /// At most this many continuations per session per hour.
    pub continue_per_hour: u32,
    /// What a continuation types when the worker offers no suggestion of its own.
    pub continue_text: String,
    /// A file whose text is appended to a continuation (standing rules).
    pub rules_file: Option<PathBuf>,
    /// Back off and continue after a retryable API error or an overload.
    pub retry_api_errors: bool,
    /// Continue once a session or weekly usage limit has reset.
    pub resume_limits: bool,
    /// The model to switch to on a model-bucket limit, and back from at its
    /// reset (owner decision 3). `None`: escalate instead.
    pub model_fallback: Option<String>,
    /// Type `/compact` when the worker stops on a full context.
    pub compact_on_context_wall: bool,
    /// The window's LIVE AGENT UPGRADE sweep (`aterm harness upgrade`,
    /// `harness::upgrade_drive::host`): restart a Claude Code session onto a
    /// newer build, cooperatively. Not a supervisor policy — the engine never
    /// reads it — but a `[harness]` key all the same, so this table stays the
    /// one policy home and the host's fail-closed parse admits it. The
    /// window gates its sweep on this parse (`enabled && upgrade`, handed to
    /// [`crate::harness::upgrade_drive::host`]); only a caller with no parsed
    /// config reads the file ([`crate::harness::upgrade_drive::host_enabled`]).
    pub upgrade: bool,
}

impl Default for SupervisorConfig {
    fn default() -> Self {
        Self {
            enabled: true,
            headless: false,
            approvals: ApprovalToggles::default(),
            trust_roots: ["~/aterm*", "~/ay*", "$HOME/trust*", "/private/tmp/claude-*"]
                .map(String::from)
                .to_vec(),
            dismiss_surveys: true,
            continue_policy: true,
            continue_per_hour: 6,
            continue_text: "keep going".to_string(),
            rules_file: None,
            retry_api_errors: true,
            resume_limits: true,
            model_fallback: Some("opus".to_string()),
            compact_on_context_wall: true,
            upgrade: true,
        }
    }
}

impl SupervisorConfig {
    /// The policy of a host whose `aterm.toml` did not load: supervision on
    /// — the badges and notifications still reach the owner — and every
    /// switch that ACTS off (no approval rule, no survey dismissal, no
    /// continuation, retry, resume, `/model` or `/compact`, no upgrade
    /// sweep). A file that fails to parse must never mean the defaults'
    /// autonomy: an owner who wrote `continue = false` and then a typo in
    /// any table would otherwise get every act back (the safety review of
    /// 2026-09-24). A successful load or reload replaces it.
    #[must_use]
    pub fn escalate_only() -> Self {
        Self {
            approvals: ApprovalToggles::off(),
            dismiss_surveys: false,
            continue_policy: false,
            retry_api_errors: false,
            resume_limits: false,
            model_fallback: None,
            compact_on_context_wall: false,
            upgrade: false,
            ..Self::default()
        }
    }

    /// The policy `aterm drive supervise|watch` runs under: the owner's
    /// defaults for the switches its flags gate (`--auto-reads` the approval
    /// rules, `--dismiss-surveys` the survey), and every switch it has no
    /// flag for off — a CLI loop types no continuation, retries no API
    /// error and switches no model unless asked. `resume_limits` is `watch
    /// --resume`'s.
    pub fn cli(resume: bool) -> Self {
        Self {
            continue_policy: false,
            retry_api_errors: false,
            resume_limits: resume,
            model_fallback: None,
            compact_on_context_wall: false,
            ..Self::default()
        }
    }

    /// Set one `[harness]` key from its TOML value's text: `true`/`false`
    /// for a switch, a decimal for a count, the string itself for a text or
    /// a path (empty clears an optional one), and for `trust_roots` the
    /// array's elements joined by `,` (the host flattens the array). An
    /// unknown key or a malformed value is refused with the key's name.
    pub fn set(&mut self, key: &str, value: &str) -> Result<(), String> {
        let flag = |v: &str| match v.trim() {
            "true" => Ok(true),
            "false" => Ok(false),
            other => Err(format!(
                "[harness] {key}: expected true or false, got {other:?}"
            )),
        };
        let text = |v: &str| {
            let v = v.trim();
            (!v.is_empty()).then(|| v.to_string())
        };
        match key {
            "enabled" => self.enabled = flag(value)?,
            "headless" => self.headless = flag(value)?,
            "auto_reads" => self.approvals.auto_reads = flag(value)?,
            "rm_breaker" => self.approvals.rm_breaker = flag(value)?,
            "read_outside_cwd" => self.approvals.read_outside_cwd = flag(value)?,
            "trust_dialog" => self.approvals.trust_dialog = flag(value)?,
            "trust_roots" => {
                self.trust_roots = value.split(',').filter_map(text).collect();
            }
            "dismiss_surveys" => self.dismiss_surveys = flag(value)?,
            "continue" => self.continue_policy = flag(value)?,
            "continue_per_hour" => {
                self.continue_per_hour = value.trim().parse().map_err(|_| {
                    format!("[harness] continue_per_hour: expected a count, got {value:?}")
                })?;
            }
            "continue_text" => {
                self.continue_text = text(value)
                    .ok_or_else(|| "[harness] continue_text: may not be empty".to_string())?;
            }
            "rules_file" => self.rules_file = text(value).map(PathBuf::from),
            "retry_api_errors" => self.retry_api_errors = flag(value)?,
            "resume_limits" => self.resume_limits = flag(value)?,
            "model_fallback" => self.model_fallback = text(value),
            "compact_on_context_wall" => self.compact_on_context_wall = flag(value)?,
            "upgrade" => self.upgrade = flag(value)?,
            other => {
                return Err(format!(
                    "[harness] {other}: not a harness key (known: {})",
                    KEYS.join(", ")
                ));
            }
        }
        Ok(())
    }
}

impl SuperviseOpts {
    /// [`Self::hosted`] under the owner's `[harness]` policy.
    pub fn hosted_with(cfg: &SupervisorConfig) -> Self {
        Self {
            // Every rule off is the approval policy off: nothing presses.
            auto_reads: cfg.approvals != ApprovalToggles::off(),
            dismiss_surveys: cfg.dismiss_surveys,
            resume: if cfg.resume_limits {
                Self::hosted().resume
            } else {
                None
            },
            policy: cfg.clone(),
            ..Self::hosted()
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_key_sets_and_an_unknown_one_is_refused_by_name() {
        let mut c = SupervisorConfig::default();
        for key in KEYS {
            let value = match *key {
                "continue_per_hour" => "3",
                "continue_text" | "rules_file" | "model_fallback" => "x",
                "trust_roots" => "~/a*, /b",
                _ => "false",
            };
            c.set(key, value).unwrap_or_else(|e| panic!("{key}: {e}"));
        }
        assert!(!c.enabled && !c.approvals.auto_reads && !c.continue_policy && !c.upgrade);
        assert_eq!(c.trust_roots, vec!["~/a*".to_string(), "/b".to_string()]);
        assert_eq!(c.continue_per_hour, 3);
        let err = c.set("contine", "true").unwrap_err();
        assert!(err.contains("contine"), "{err}");
        assert!(c.set("continue", "yes").is_err());
        assert!(c.set("continue_text", "  ").is_err());
        c.set("model_fallback", "").unwrap();
        assert_eq!(c.model_fallback, None);
    }

    /// A host whose config did not load escalates and does nothing else:
    /// through the host's options nothing presses, dismisses or resumes.
    /// Negative control: the defaults act.
    #[test]
    fn escalate_only_acts_on_nothing() {
        let c = SupervisorConfig::escalate_only();
        assert!(c.enabled, "still supervised: escalations reach the owner");
        assert_eq!(c.approvals, ApprovalToggles::off());
        assert!(!c.continue_policy && !c.retry_api_errors && !c.resume_limits);
        assert!(!c.dismiss_surveys && !c.compact_on_context_wall && !c.upgrade);
        assert_eq!(c.model_fallback, None);
        let o = SuperviseOpts::hosted_with(&c);
        assert!(!o.auto_reads && !o.dismiss_surveys && o.resume.is_none());
        let d = SuperviseOpts::hosted_with(&SupervisorConfig::default());
        assert!(d.auto_reads && d.dismiss_surveys && d.resume.is_some());
    }

    #[test]
    fn the_defaults_are_the_adopted_owner_decisions() {
        let c = SupervisorConfig::default();
        assert!(c.enabled && !c.headless && c.upgrade);
        assert_eq!(c.approvals, ApprovalToggles::default());
        assert!(c.continue_policy && c.continue_per_hour == 6);
        assert_eq!(c.model_fallback.as_deref(), Some("opus"));
        let o = SuperviseOpts::hosted_with(&c);
        assert!(o.auto_reads && o.dismiss_surveys && o.resume.is_some());
        assert_eq!(o.policy, c, "the whole config rides in the options");
        assert_eq!(o.max, super::super::run::UNBOUNDED);
        // Every approval rule off is the auto-reads flag off: nothing presses.
        let mut off = c.clone();
        for k in [
            "auto_reads",
            "rm_breaker",
            "read_outside_cwd",
            "trust_dialog",
        ] {
            off.set(k, "false").unwrap();
        }
        assert!(!SuperviseOpts::hosted_with(&off).auto_reads);
    }
}
