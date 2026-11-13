// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! `@<sid> harness policy` control verb for one session's `[harness]` policy.

use aterm_agent::supervise::policy::land_check::land_check_text;
use aterm_agent::supervise::policy::needs::{NEEDS_ACK, NEEDS_HINT};
use aterm_agent::supervise::policy::question::LAND_TEXT;
use aterm_agent::supervise::policy::turn_end::{
    DONE_ANGLES, DONE_TAIL, continuation_text, done_check_text, land_answer_text, land_nudge_text,
    land_text,
};
use aterm_agent::supervise::{Autopilot, SupervisorConfig};
use aterm_phase::Program;

/// What the main thread knows of one session's policy: the facts the reply is made of, all but the rules file's text (read off the main thread).
#[derive(Clone, Debug)]
pub(crate) struct PolicyFacts {
    /// The policy cell's generation.
    pub(crate) generation: u64,
    /// The policy in force.
    pub(crate) policy: SupervisorConfig,
    /// Whether the policy is in effect for this session, and why.
    pub(crate) effective: (bool, &'static str),
    /// The session's own `meta questions` word, when it has one.
    pub(crate) session_questions: Option<String>,
    pub(crate) codex_rotation: bool,
}

/// The inputs of `effective=`/`why=` for one session.
#[derive(Clone, Copy, Debug)]
pub(crate) struct Posture {
    /// This window has a supervisor host (a control socket).
    pub(crate) hosted: bool,
    /// This instance is headless.
    pub(crate) headless: bool,
    /// The session's agent, where the server names one (else Codex: the agent that asks).
    pub(crate) agent: Program,
    /// The host is suspended for a seamless update's handoff.
    pub(crate) suspended: bool,
    /// A halt (local or fleet) stands on the session.
    pub(crate) held: bool,
    /// The session's own Autopilot word, when it has one.
    pub(crate) autopilot_word: Option<Autopilot>,
}

/// `effective=`/`why=`, first refusal first.
#[must_use]
pub(crate) fn effective(cfg: &SupervisorConfig, p: Posture) -> (bool, &'static str) {
    let why = if !p.hosted {
        "unhosted"
    } else if !cfg.enabled {
        "disabled"
    } else if p.headless && !cfg.headless {
        "headless"
    } else if !cfg.on_for(p.agent) {
        "agent-off"
    } else if p.suspended {
        "suspended"
    } else if p.held {
        "hold"
    } else if cfg.autopilot_for(p.autopilot_word) == Autopilot::Off {
        "autopilot-off"
    } else {
        return (true, "enabled");
    };
    (false, why)
}

/// `questions=`: the session's own word over `answer_questions` (off reads `ask`) and `question_priority`.
#[must_use]
pub(crate) fn questions(cfg: &SupervisorConfig, session: Option<&str>) -> String {
    match session {
        Some(word) => word.to_string(),
        _ if !cfg.answer_questions => "ask".to_string(),
        _ => cfg.question_priority.word().to_string(),
    }
}

/// The additive text keys after `questions=`, in their wire order. `done_check_angles` and
/// `done_check_tail` came with v6's 5a, after the rest, and `land_check_text` with 13a, after
/// them: a reader keys on names, and one that meets a reply without them keeps `done_check_text`
/// whole (the tests' `done_checks_of`, the downgrade parse such a reader writes).
/// `land_question_text`, the policy text Land writes into a question dialog's free-text row
/// (`answer-land@v1`), came after them all; `questions=` never says `land`, which a reader would
/// take for a word it does not know.
pub(crate) const ACT_TEXT_KEYS: [&str; 10] = [
    "done_check_text",
    "land_text",
    "land_nudge_text",
    "land_answer_text",
    "needs_hint_text",
    "needs_ack_text",
    "done_check_angles",
    "done_check_tail",
    "land_check_text",
    "land_question_text",
];

/// Autopilot Cruise / Land's fixed words as aterm's own loop types them for a session whose
/// standing rules read `rules`, keyed by [`ACT_TEXT_KEYS`]: the four whole acts through the
/// turn-end policy's own composers (the rules after the words; the done check is its first
/// angle's whole act), the two suffixes bare, then Cruise's six angles in order, one to a line,
/// and the words that follow each, bare; then the land check, a whole act in its plain form
/// (the hinted one needs aterm's reader of the report); last Land's dialog text, bare (a
/// free-text row takes no rules).
#[must_use]
pub(crate) fn act_texts(rules: Option<&str>) -> [(&'static str, String); 10] {
    let [
        done_check,
        land,
        nudge,
        answer,
        hint,
        ack,
        angles,
        tail,
        check,
        question,
    ] = ACT_TEXT_KEYS;
    [
        (done_check, done_check_text(rules, false)),
        (land, land_text(rules)),
        (nudge, land_nudge_text(rules)),
        (answer, land_answer_text(rules)),
        (hint, NEEDS_HINT.to_string()),
        (ack, NEEDS_ACK.to_string()),
        (angles, DONE_ANGLES.join("\n")),
        (tail, DONE_TAIL.to_string()),
        (check, land_check_text(None, false, rules)),
        (question, LAND_TEXT.to_string()),
    ]
}

/// The reply line, newline-terminated: `OK effective=<on|off>` alone for an edge token, the whole
/// policy for the owner, opening with the harness contracts this aterm serves (`contract=2,3`:
/// codex-alab's self-claim and a rostered harness's scoped claim,
/// [`aterm_agent::harness::contract::SERVED`]; none under the negative control's seam,
/// [`crate::harness_claim::served`]).
#[must_use]
pub(crate) fn reply(facts: &PolicyFacts, rules: Option<&str>, owner: bool) -> String {
    reply_serving(facts, rules, owner, crate::harness_claim::served())
}

/// [`reply`] with `contract=` saying `served`, or no `contract=` key at all for `None` (an aterm
/// predating contract 3).
#[must_use]
pub(crate) fn reply_serving(
    facts: &PolicyFacts,
    rules: Option<&str>,
    owner: bool,
    served: Option<&str>,
) -> String {
    let (on, why) = facts.effective;
    let on = if on { "on" } else { "off" };
    if !owner {
        return format!("OK effective={on}\n");
    }
    let enc = |v: &str| {
        if v.is_empty() {
            "-".to_string()
        } else {
            crate::control::pct_encode(v)
        }
    };
    let mut line = format!("OK gen={} effective={on} why={why}", facts.generation);
    if let Some(served) = served {
        line.push_str(&format!(" contract={served}"));
    }
    if facts.codex_rotation {
        // The bound a five-hour handoff is judged by (`codex_rotation::walled_window`), in
        // seconds: a codex-alab ends its turn at a five-hour wall for this host to move only
        // where that wall's reset is further away. It reads the roster itself.
        line.push_str(&format!(
            " codex_rotation=1 codex_rotation_five_hour={}",
            aterm_agent::harness::accounts::RESET_SOON_S
        ));
    }
    for (key, value) in facts.policy.texts() {
        let value = if key == "continue_text" {
            continuation_text(&facts.policy, rules)
        } else {
            value
        };
        line.push_str(&format!(" {key}={}", enc(&value)));
    }
    line.push_str(&format!(
        " questions={}",
        enc(&questions(
            &facts.policy,
            facts.session_questions.as_deref()
        ))
    ));
    for (key, value) in act_texts(rules) {
        line.push_str(&format!(" {key}={}", enc(&value)));
    }
    line.push_str(&format!(
        " land_checks={}",
        aterm_agent::supervise::policy::turn_end::LAND_CHECKS
    ));
    line.push('\n');
    line
}

/// The agent a session's policy answers for: the one with a `[harness.<agent>]` table
/// ([`crate::harness_host::agent_of`]), else the agent a rostered harness runs (Grok Build under
/// grok-alab, contract v3 §8.1 item 7: its `agent-off` follows the row's program, which no table
/// switches off), else Codex, the agent that asks for itself.
#[cfg(test)]
fn policy_agent(publication: &crate::session_timeline::AgentPublication) -> Program {
    policy_agent_in(aterm_agent::harness::contract::ROSTER, publication)
}

fn policy_agent_in(
    rows: &[aterm_agent::harness::contract::Row],
    publication: &crate::session_timeline::AgentPublication,
) -> Program {
    crate::harness_host::agent_of(publication)
        .or_else(|| {
            crate::harness_host::identified_agent(publication)
                .filter(|p| rows.iter().any(|row| row.program == *p))
        })
        .unwrap_or(Program::Codex)
}

/// Resolve configured policy through the same session posture in App and the explicit
/// display-free fixture. The session's hold and Autopilot are always read from its own state.
pub(crate) fn configured_facts(
    ctx: &crate::SessionCtx,
    policy: &SupervisorConfig,
    generation: u64,
    hosted: bool,
    headless: bool,
    suspended: bool,
) -> PolicyFacts {
    let (session_questions, autopilot_word) = {
        let meta = ctx.meta.lock().unwrap_or_else(|p| p.into_inner());
        (meta.questions.clone(), meta.autopilot_word())
    };
    let agent = {
        let timeline = ctx.timeline.lock().unwrap_or_else(|p| p.into_inner());
        policy_agent_in(ctx.native_policy.rows(), timeline.agent())
    };
    PolicyFacts {
        generation,
        codex_rotation: ctx.native_policy.quota_rotation_enabled(),
        policy: policy.clone(),
        session_questions,
        effective: effective(
            policy,
            Posture {
                hosted,
                headless,
                agent,
                suspended,
                held: ctx.fabric.hold().is_some(),
                autopilot_word,
            },
        ),
    }
}

/// The verb's control-thread half: `harness policy` (any other form is its usage line), the facts fetched by `fetch`, the rules file read here.
pub(crate) fn cmd(
    rest: &str,
    owner: bool,
    fetch: impl FnOnce() -> Result<PolicyFacts, String>,
) -> String {
    if rest.split_whitespace().collect::<Vec<_>>() != ["policy"] {
        return crate::harness_recovery_verb::USAGE.to_string();
    }
    match fetch() {
        Ok(facts) => {
            let rules = owner
                .then(|| aterm_agent::supervise::run::standing_rules(&facts.policy))
                .flatten();
            reply(&facts, rules.as_deref(), owner)
        }
        Err(e) => format!("ERR {e}\n"),
    }
}

impl crate::App {
    /// What the main thread knows of `session`'s policy ([`PolicyFacts`]).
    pub(crate) fn harness_policy_facts(&self, session: u64) -> Result<PolicyFacts, String> {
        let Some(pooled) = self.pool.get(session) else {
            return Err(format!("no such session {session}"));
        };
        let ctx = &pooled.ctx;
        Ok(configured_facts(
            ctx,
            &self.harness_policy.get().policy,
            self.harness_policy.generation(),
            self.harness.is_some(),
            self.headless,
            self.harness
                .as_ref()
                .is_some_and(crate::harness_host::HostHandle::suspended),
        ))
    }

    /// The policy cell moved: every session's `subscribe @<sid>` stream gets `EVENT <local> harness-policy gen=<n>`, so an agent that answers for itself re-reads `harness policy` on it rather than on a clock.
    pub(crate) fn announce_harness_policy(&self) {
        let generation = self.harness_policy.generation();
        for pooled in self.pool.iter() {
            record_policy_move(&pooled.ctx.timeline, generation);
        }
    }
}

/// The timeline kind (and `events` wire word) of the policy cell's move.
pub(crate) const HARNESS_POLICY_EVENT: &str = "harness-policy";

/// The policy cell's move to `generation`, on one session's timeline: the `harness-policy gen=<n>` row [`crate::App::announce_harness_policy`] writes on every session's (the wire goldens write theirs through it too), and in the same hold the session log's `mark - policy gen=<n>`, on every session as the row is.
pub(crate) fn record_policy_move(
    timeline: &std::sync::Mutex<crate::session_timeline::SessionTimeline>,
    generation: u64,
) {
    let mut tl = timeline.lock().unwrap_or_else(|p| p.into_inner());
    let at = tl.slog().now();
    tl.slog_mut().observe_policy(generation, at);
    tl.record(HARNESS_POLICY_EVENT);
    let _ = tl.slog_take_wake();
}

#[cfg(test)]
#[path = "harness_policy_verb_tests.rs"]
mod tests;
