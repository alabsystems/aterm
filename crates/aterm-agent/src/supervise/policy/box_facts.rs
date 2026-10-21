// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! THE FACTS A BOX WAS DECIDED ON: everything the approval policy's context
//! ([`ApprovalCtx`]) holds that the loop LEARNED — by a control read, a
//! look through a probe, or its own memory of the session — as opposed to
//! what the owner's policy and the supervisor's own process say.
//!
//! The loop's `approval_ctx` used to be one effectful function that read the
//! session and wrote the context field by field, so a decision's context
//! could be had only by running the loop to it. It is two now: the loop
//! gathers [`BoxFacts`] (the effectful part: the session's directory, its
//! footer, the worker, its shell, its owner's ask rules, the dialog's own
//! memory …), and [`ApprovalCtx::from`] builds the context from the policy,
//! the process's inputs and those facts — a pure function, the one the loop
//! decides through. A diagnostic reader can rebuild the context without
//! running the loop, then decide from the facts its mark carries
//! ([`BoxFacts::to_json`]), the header's policy and process inputs, and a
//! probe answering from the tape (`aterm drive replay --why`, `tape::explain`).
//!
//! **On a tape** the facts are a JSON object in the decision's mark
//! (`decide.approval`, `decide.question`), written by the recorder only:
//! captured filesystem paths as the run's roots write them (`@home/…`).
//! `OwnerAsk.source` is retained display text, outside that path projection.
//! The worker is the DIGEST and pid the tape knows it by — never a variable of its
//! environment ([`super::probe`]'s privacy line) — the shell's startup, the
//! owner's asks and the Bash tool's directories as the derived answers their
//! probes give.
//!
//! **Read by a later build**, every field is optional: a field the writing
//! build had not grown reads at its default, and is NAMED in what the read
//! says the decision needs ([`BoxFacts::from_json`]'s second value), so a
//! diagnostic reader can name the facts a re-decision lacked, rather
//! than present a default as what the recorded run knew. A field this build
//! does not know is ignored.

use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use super::approval::{ApprovalCtx, QuestionPriority};
use super::claude_settings::OwnerAsk;
use super::git_config::WorkerEnv;
use super::probe::{Answer, AskWire, ShellWire};
use super::shell_startup::ShellStartup;
use crate::supervise::config::SupervisorConfig;
use crate::supervise::run::ApprovalEnv;

/// What the loop learned about a box before deciding it ([module docs](self)).
#[derive(Debug, Clone, Default)]
pub struct BoxFacts {
    /// The session's foreground program as `status program=` named it: the
    /// reader the box was read with (`None`: the screen named it).
    pub program: Option<String>,
    /// The session's working directory (`meta cwd=`); `None`: unknown.
    pub cwd: Option<PathBuf>,
    /// The one folder the session's own starter trusted for it (`meta
    /// trusted_cwd=`, [`ApprovalCtx::trusted_cwd`]); `None`: none.
    pub trusted_cwd: Option<PathBuf>,
    /// The footer last read names bypass mode.
    pub bypass: bool,
    /// A limit reset was passed over at this dialog's mount.
    pub limit_reset_passed: bool,
    /// The worker, where it was read (a box that may run git, a Bash box, a
    /// box an ask rule may have sent), or why it could not be; `None`: not
    /// read.
    pub worker: Option<Result<WorkerEnv, String>>,
    /// Where the worker's Bash tool may stand beyond the cwd.
    pub shell_cwds: Vec<PathBuf>,
    /// What the Bash tool's shell starts with, where read; `None`: not read.
    pub shell: Option<Result<ShellStartup, String>>,
    /// The ask rules and hooks of the agent's own settings, where read.
    pub owner_asks: Option<Result<Vec<OwnerAsk>, String>>,
    /// The pane's width in cells (`0`: the widest row).
    pub screen_cols: u16,
    /// The pane's height in rows (`0`: the count of rows read).
    pub screen_rows: usize,
    /// How a question dialog is answered, for a question only.
    pub question: Option<QuestionFacts>,
    /// A tool candidate from the agent's transcript.
    pub box_call: Option<String>,
    /// The `/login` picker is behind the loop's own `/login`
    /// ([`ApprovalCtx::login_carry`]).
    pub login_carry: bool,
    /// The `/login` picker is behind the loop's own `/login`, and the login
    /// came back in another tab since ([`ApprovalCtx::login_back`]).
    pub login_back: bool,
    /// The `/login` picker is behind the loop's own `/login`, and a renewal
    /// the tab follows replaced the login it signs in
    /// ([`ApprovalCtx::login_renewed`]).
    pub login_renewed: bool,
    /// The method the session's login already is, as its host read it
    /// ([`ApprovalCtx::login_method`]), for the picker of the loop's own
    /// `/login` only; `None`: not read, or nothing says.
    pub login_method: Option<aterm_phase::LoginMethod>,
}

/// How a question dialog was to be answered, as the session's word, the
/// table and the loop's memory of the dialog said.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct QuestionFacts {
    /// It is answered at all.
    pub answer: bool,
    /// By which priority (the recommended option where a text priority's
    /// fences are missing).
    pub priority: QuestionPriority,
    /// The priority whose text the loop wrote into its free-text row.
    pub own: Option<QuestionPriority>,
    /// The loop already fell back to the recommended option on this tab.
    pub fell_back: bool,
}

/// Every field a mark's facts may carry: what a read that lacks one says the
/// decision needs ([`BoxFacts::from_json`]).
pub const FIELDS: &[&str] = &[
    "program",
    "cwd",
    "trusted_cwd",
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
    "login_renewed",
    "login_method",
];

impl ApprovalCtx {
    /// THE CONTEXT A BOX IS DECIDED IN, built from the owner's policy
    /// `config`, the supervisor's process inputs `env`, the `python3` globs
    /// the classifier reads as reads (`allow`), and what the loop learned of
    /// the box (`facts`) — the pure half of the loop's context
    /// ([module docs](self)): the loop decides through it, and a reader of a
    /// tape decides again through it from the facts a mark carries. `probe`
    /// is how the decision looks at the machine (the loop's door, or a probe
    /// answering from a tape).
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
        // A safe rule a withdrawn key took away (`config::Withheld`): its roots.
        if config.withheld.rm_breaker {
            ctx.scratch_roots.clear();
        }
        // An unproven rm breaker is approved with a warning under `safe`
        // unless the owner asked for it (`rm_breaker_ask`), or the withdrawn
        // `rm_breaker = false` said no rm is answered at all.
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
        // The grant row's rule, under `[harness] limit_reset` — and never
        // again at a dialog mount the reset was passed over for.
        ctx.limit_reset = config.limit_reset && !facts.limit_reset_passed;
        ctx.reset_kinds.clone_from(&config.limit_reset_kinds);
        // A credits consent, under `[harness] usage_credits` alone: policy,
        // no fact of the box.
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
        ctx.login_renewed = facts.login_renewed;
        ctx.login_method = facts.login_method;
        ctx
    }
}

// -- the facts on a tape ------------------------------------------------------

/// The facts as a mark holds them: every field optional ([module docs](self)).
#[derive(Serialize, Deserialize, Default)]
#[serde(default)]
struct Wire {
    program: Option<String>,
    cwd: Option<String>,
    trusted_cwd: Option<String>,
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
    /// The facts as a decision's mark holds them ([module docs](self)): each
    /// captured filesystem path as `path` writes it (the run's roots), the
    /// worker by its digest under the tape's `seed` and its pid. Path bytes use
    /// the existing lossy UTF-8 text convention, so arbitrary Unix byte paths
    /// may not round-trip.
    #[must_use]
    pub fn to_json(&self, seed: u64, path: &dyn Fn(&Path) -> Vec<u8>) -> String {
        let text = |p: &Path| String::from_utf8_lossy(&path(p)).into_owned();
        let wire = Wire {
            program: self.program.clone(),
            cwd: self.cwd.as_deref().map(text),
            trusted_cwd: self.trusted_cwd.as_deref().map(text),
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
            login_renewed: self.login_renewed,
            login_method: self.login_method.map(|m| m.word().to_string()),
        };
        // The wire holds strings, numbers, booleans and lists of them, which
        // always serialize.
        aterm_json::to_string(&wire).unwrap_or_default()
    }

    /// A mark's facts read back, recorded filesystem path text decoded by
    /// `roots_decode` (the tape's roots): the facts, and the fields not carried —
    /// read at their defaults, which a re-decision NEEDS and did not have
    /// ([module docs](self)).
    ///
    /// # Errors
    ///
    /// Text that is no JSON object, or a field whose value this build cannot
    /// read (a worker's digest that is not hex, a priority it does not
    /// know, a path under a root the header does not name).
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
