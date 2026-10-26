// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Session facts gathered before deciding an approval or question box.

use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use super::approval::{ApprovalCtx, QuestionPriority};
use super::claude_settings::OwnerAsk;
use super::git_config::WorkerEnv;
use super::probe::{Answer, AskWire, ShellWire};
use super::shell_startup::ShellStartup;
use crate::supervise::config::SupervisorConfig;
use crate::supervise::run::ApprovalEnv;

/// Session and probe facts gathered by the loop before deciding a box.
#[derive(Debug, Clone, Default)]
pub struct BoxFacts {
    /// Foreground program name from `status program=`, or `None` if read from screen.
    pub program: Option<String>,
    /// Session working directory (`meta cwd=`), or `None` if unknown.
    pub cwd: Option<PathBuf>,
    /// Folder trusted for this session by its starter (`meta trusted_cwd=`).
    pub trusted_cwd: Option<PathBuf>,
    /// Agent started by a worker spawn on this session (`meta worker=`).
    pub worker_agent: Option<aterm_phase::Program>,
    /// Whether the last read footer names bypass mode.
    pub bypass: bool,
    /// Whether a limit reset was passed over at this dialog's mount.
    pub limit_reset_passed: bool,
    /// Worker environment where read, or why it could not be read.
    pub worker: Option<Result<WorkerEnv, String>>,
    /// Additional directories where the worker's Bash tool may stand.
    pub shell_cwds: Vec<PathBuf>,
    /// Bash tool shell startup configuration where read.
    pub shell: Option<Result<ShellStartup, String>>,
    /// Ask rules and hooks from the agent's settings where read.
    pub owner_asks: Option<Result<Vec<OwnerAsk>, String>>,
    /// Pane width in cells (`0` uses the widest row).
    pub screen_cols: u16,
    /// Pane height in rows (`0` uses the count of rows read).
    pub screen_rows: usize,
    /// Question answering configuration when the dialog is a question.
    pub question: Option<QuestionFacts>,
    /// Tool candidate from the agent's transcript.
    pub box_call: Option<String>,
    /// Whether the `/login` picker is behind the loop's own `/login`.
    pub login_carry: bool,
    /// Whether the login came back in another tab while `/login` was carried, so the loop backs
    /// out of its picker.
    pub login_back: bool,
    /// Whether the loop's own `/login` stands over a roster row's login, which aterm's sign-in
    /// ladder signs in, so the loop backs out of its picker.
    pub login_roster: bool,
    /// Whether a followed renewal replaced the login while `/login` was carried, so the loop
    /// backs out of its picker.
    pub login_renewed: bool,
    /// Existing login method read by the host for the loop's own `/login` picker.
    pub login_method: Option<aterm_phase::LoginMethod>,
}

/// How a question dialog is to be answered.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct QuestionFacts {
    /// Whether the question is answered at all.
    pub answer: bool,
    /// Priority used to answer the question.
    pub priority: QuestionPriority,
    /// Priority whose text the loop wrote into the free-text row.
    pub own: Option<QuestionPriority>,
    /// Whether the loop already fell back to the recommended option on this tab.
    pub fell_back: bool,
}

/// Every field name carried by serialized `BoxFacts`.
pub const FIELDS: &[&str] = &[
    "program",
    "cwd",
    "trusted_cwd",
    "worker_agent",
    "bypass",
    "limit_reset_passed",
    "worker",
    "shell_cwds",
    "shell",
    "owner_asks",
    "screen_cols",
    "screen_rows",
    "question",
    "box_call",
    "login_carry",
    "login_back",
    "login_roster",
    "login_renewed",
    "login_method",
];

impl ApprovalCtx {
    /// Builds an [`ApprovalCtx`] from supervisor policy, process environment, and [`BoxFacts`].
    #[must_use]
    pub fn from(
        config: &SupervisorConfig,
        env: &ApprovalEnv,
        allow: &[String],
        facts: &BoxFacts,
        probe: std::sync::Arc<dyn super::PolicyProbe>,
    ) -> Self {
        let mut ctx = Self::new(
            facts.cwd.clone().unwrap_or_else(|| PathBuf::from("/")),
            env.home.clone(),
            env.uid,
            env.tmpdir.clone(),
            probe,
        );
        ctx.cwd_known = facts.cwd.is_some();
        ctx.set_trust_roots(&config.trust_roots, env.uid);
        ctx.trusted_cwd.clone_from(&facts.trusted_cwd);
        ctx.disk_worker = facts
            .trusted_cwd
            .as_deref()
            .is_some_and(crate::harness::disk::fence::is_worker_folder);
        ctx.worker_agent = facts.worker_agent;
        if config.withheld.rm_breaker {
            ctx.scratch_roots.clear();
        }
        ctx.rm_breaker_warn = !config.rm_breaker_ask && !config.withheld.rm_breaker;
        if config.withheld.read_outside_cwd {
            ctx.read_roots.clear();
        }
        ctx.bypass_mode = facts.bypass;
        ctx.approve = config.approve;
        ctx.answer_questions = config.answer_questions;
        ctx.model_fallback = config.model_fallback.is_some();
        ctx.limit_wait = config.limit_wait;
        ctx.autocompact_offer = config.autocompact_offer;
        ctx.limit_reset = config.limit_reset && !facts.limit_reset_passed;
        ctx.reset_kinds.clone_from(&config.limit_reset_kinds);
        ctx.usage_credits = config.usage_credits;
        ctx.python_allow = allow.to_vec();
        if let Some(worker) = &facts.worker {
            ctx.worker = worker.clone();
        }
        ctx.shell_cwds = facts.shell_cwds.clone();
        if let Some(shell) = &facts.shell {
            ctx.shell = shell.clone();
        }
        if let Some(asks) = &facts.owner_asks {
            ctx.owner_asks = asks.clone();
        }
        ctx.screen_cols = facts.screen_cols;
        ctx.screen_rows = facts.screen_rows;
        if let Some(q) = facts.question {
            ctx.answer_questions = q.answer;
            if q.answer {
                ctx.question_priority = q.priority;
                ctx.question_own = q.own;
                ctx.question_fell_back = q.fell_back;
            }
        }
        ctx.box_call = facts.box_call.clone();
        ctx.login_carry = facts.login_carry;
        ctx.login_back = facts.login_back;
        ctx.login_roster = facts.login_roster;
        ctx.login_renewed = facts.login_renewed;
        ctx.login_method = facts.login_method;
        ctx
    }
}

/// The facts as a mark holds them: every field optional.
#[derive(Serialize, Deserialize, Default)]
#[serde(default)]
struct Wire {
    program: Option<String>,
    cwd: Option<String>,
    trusted_cwd: Option<String>,
    worker_agent: Option<String>,
    bypass: bool,
    limit_reset_passed: bool,
    worker: Option<Answer<WorkerWire>>,
    shell_cwds: Vec<String>,
    shell: Option<Answer<ShellWire>>,
    owner_asks: Option<Answer<Vec<AskWire>>>,
    screen_cols: u16,
    screen_rows: u64,
    question: Option<QuestionWire>,
    box_call: Option<String>,
    login_carry: bool,
    login_back: bool,
    login_roster: bool,
    login_renewed: bool,
    login_method: Option<String>,
}

/// The worker as a tape knows it: its digest (sixteen hex digits) and pid.
#[derive(Serialize, Deserialize)]
struct WorkerWire {
    digest: String,
    pid: Option<u32>,
}

#[derive(Serialize, Deserialize)]
struct QuestionWire {
    answer: bool,
    priority: String,
    own: Option<String>,
    fell_back: bool,
}

impl BoxFacts {
    /// Serializes these facts to JSON for a decision mark using `path` and `seed`.
    #[must_use]
    pub fn to_json(&self, seed: u64, path: &dyn Fn(&Path) -> Vec<u8>) -> String {
        let text = |p: &Path| String::from_utf8_lossy(&path(p)).into_owned();
        let wire = Wire {
            program: self.program.clone(),
            cwd: self.cwd.as_deref().map(text),
            trusted_cwd: self.trusted_cwd.as_deref().map(text),
            worker_agent: self.worker_agent.map(|a| a.name().to_string()),
            bypass: self.bypass,
            limit_reset_passed: self.limit_reset_passed,
            worker: self.worker.as_ref().map(|w| match w {
                Ok(w) => Answer::Ok(WorkerWire {
                    digest: format!("{:016x}", w.digest(seed)),
                    pid: w.pid(),
                }),
                Err(why) => Answer::Err(why.clone()),
            }),
            shell_cwds: self.shell_cwds.iter().map(|p| text(p)).collect(),
            shell: self.shell.as_ref().map(|s| match s {
                Ok(s) => Answer::Ok((s.names().to_vec(), s.may_run_zsh())),
                Err(why) => Answer::Err(why.clone()),
            }),
            owner_asks: self.owner_asks.as_ref().map(|a| match a {
                Ok(asks) => Answer::Ok(asks.iter().map(AskWire::of).collect()),
                Err(why) => Answer::Err(why.clone()),
            }),
            screen_cols: self.screen_cols,
            screen_rows: self.screen_rows as u64,
            question: self.question.map(|q| QuestionWire {
                answer: q.answer,
                priority: q.priority.word().to_string(),
                own: q.own.map(|o| o.word().to_string()),
                fell_back: q.fell_back,
            }),
            box_call: self.box_call.clone(),
            login_carry: self.login_carry,
            login_back: self.login_back,
            login_roster: self.login_roster,
            login_renewed: self.login_renewed,
            login_method: self.login_method.map(|m| m.word().to_string()),
        };
        aterm_json::to_string(&wire).unwrap_or_default()
    }

    /// Deserializes [`BoxFacts`] from JSON, returning the facts and any omitted field names.
    ///
    /// # Errors
    ///
    /// Returns an error if `text` is not a valid JSON object or a field value cannot be decoded.
    pub fn from_json(
        text: &str,
        roots_decode: &dyn Fn(&[u8]) -> Result<PathBuf, String>,
    ) -> Result<(Self, Vec<&'static str>), String> {
        let value: aterm_json::Value =
            aterm_json::from_str(text).map_err(|e| format!("facts that do not read: {e}"))?;
        let object = value
            .as_object()
            .ok_or("facts that are not a JSON object")?;
        let needs: Vec<&'static str> = FIELDS
            .iter()
            .copied()
            .filter(|f| !object.contains_key(*f))
            .collect();
        let wire: Wire =
            aterm_json::from_value(value).map_err(|e| format!("facts that do not read: {e}"))?;
        let path = |t: &str| roots_decode(t.as_bytes());
        let priority = |w: &str| {
            QuestionPriority::parse(w).ok_or_else(|| format!("a question priority {w:?}"))
        };
        let facts = Self {
            program: wire.program,
            cwd: wire.cwd.as_deref().map(path).transpose()?,
            trusted_cwd: wire.trusted_cwd.as_deref().map(path).transpose()?,
            worker_agent: wire
                .worker_agent
                .as_deref()
                .map(|w| {
                    aterm_phase::Program::ALL
                        .into_iter()
                        .find(|p| p.name() == w)
                        .ok_or_else(|| format!("a worker's agent {w:?}"))
                })
                .transpose()?,
            bypass: wire.bypass,
            limit_reset_passed: wire.limit_reset_passed,
            worker: wire
                .worker
                .map(|w| match w {
                    Answer::Ok(w) => u64::from_str_radix(&w.digest, 16)
                        .map(|d| Ok(WorkerEnv::recorded(d, w.pid)))
                        .map_err(|_| format!("a worker's digest {:?} that is not hex", w.digest)),
                    Answer::Err(why) => Ok(Err(why)),
                })
                .transpose()?,
            shell_cwds: wire
                .shell_cwds
                .iter()
                .map(|p| path(p))
                .collect::<Result<_, _>>()?,
            shell: wire.shell.map(|s| match s {
                Answer::Ok((names, zsh)) => Ok(ShellStartup::of_parts(names, zsh)),
                Answer::Err(why) => Err(why),
            }),
            owner_asks: wire
                .owner_asks
                .map(|a| match a {
                    Answer::Ok(asks) => asks
                        .into_iter()
                        .map(AskWire::ask)
                        .collect::<Result<Vec<_>, _>>()
                        .map(Ok)
                        .map_err(|e| e.why.to_string()),
                    Answer::Err(why) => Ok(Err(why)),
                })
                .transpose()?,
            screen_cols: wire.screen_cols,
            screen_rows: usize::try_from(wire.screen_rows).unwrap_or(usize::MAX),
            question: wire
                .question
                .map(|q| {
                    Ok::<_, String>(QuestionFacts {
                        answer: q.answer,
                        priority: priority(&q.priority)?,
                        own: q.own.as_deref().map(priority).transpose()?,
                        fell_back: q.fell_back,
                    })
                })
                .transpose()?,
            box_call: wire.box_call,
            login_carry: wire.login_carry,
            login_back: wire.login_back,
            login_roster: wire.login_roster,
            login_renewed: wire.login_renewed,
            login_method: wire
                .login_method
                .as_deref()
                .map(|w| {
                    aterm_phase::LoginMethod::of_word(w)
                        .ok_or_else(|| format!("a login method {w:?}"))
                })
                .transpose()?,
        };
        Ok((facts, needs))
    }
}

#[cfg(test)]
#[path = "box_facts_tests.rs"]
mod tests;
