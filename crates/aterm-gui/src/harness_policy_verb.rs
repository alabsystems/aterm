// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! `@<sid> harness policy` control verb for one session's `[harness]` policy.

use aterm_agent::supervise::policy::needs::{NEEDS_ACK, NEEDS_HINT};
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
/// `done_check_tail` came with v6's 5a, after the rest: a reader keys on names, and one that
/// meets a reply without them keeps `done_check_text` whole (the tests' `done_checks_of`, the
/// downgrade parse such a reader writes).
pub(crate) const ACT_TEXT_KEYS: [&str; 8] = [
    "done_check_text",
    "land_text",
    "land_nudge_text",
    "land_answer_text",
    "needs_hint_text",
    "needs_ack_text",
    "done_check_angles",
    "done_check_tail",
];

/// Autopilot Cruise / Land's fixed words as aterm's own loop types them for a session whose
/// standing rules read `rules`, keyed by [`ACT_TEXT_KEYS`]: the four whole acts through the
/// turn-end policy's own composers (the rules after the words; the done check is its first
/// angle's whole act), the two suffixes bare, then Cruise's six angles in order, one to a line,
/// and the words that follow each, bare.
#[must_use]
pub(crate) fn act_texts(rules: Option<&str>) -> [(&'static str, String); 8] {
    let [done_check, land, nudge, answer, hint, ack, angles, tail] = ACT_TEXT_KEYS;
    [
        (done_check, done_check_text(rules, false)),
        (land, land_text(rules)),
        (nudge, land_nudge_text(rules)),
        (answer, land_answer_text(rules)),
        (hint, NEEDS_HINT.to_string()),
        (ack, NEEDS_ACK.to_string()),
        (angles, DONE_ANGLES.join("\n")),
        (tail, DONE_TAIL.to_string()),
    ]
}

/// The reply line, newline-terminated: `OK effective=<on|off>` alone for an edge token, the whole
/// policy for the owner, opening with the harness contracts this aterm serves (`contract=2,3`:
/// codex-alab's self-claim and a rostered harness's scoped claim,
/// [`aterm_agent::harness::contract::SERVED`]).
#[must_use]
pub(crate) fn reply(facts: &PolicyFacts, rules: Option<&str>, owner: bool) -> String {
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
    let mut line = format!(
        "OK gen={} effective={on} why={why} contract={}",
        facts.generation,
        aterm_agent::harness::contract::served_word()
    );
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
    line.push('\n');
    line
}

/// The agent a session's policy answers for: the one with a `[harness.<agent>]` table
/// ([`crate::harness_host::agent_of`]), else the agent a rostered harness runs (Grok Build under
/// grok-alab, contract v3 §8.1 item 7: its `agent-off` follows the row's program, which no table
/// switches off), else Codex, the agent that asks for itself.
fn policy_agent(publication: &crate::session_timeline::AgentPublication) -> Program {
    crate::harness_host::agent_of(publication)
        .or_else(|| {
            crate::harness_host::identified_agent(publication)
                .filter(|p| aterm_agent::harness::contract::row_for(*p).is_some())
        })
        .unwrap_or(Program::Codex)
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
        let policy = self.harness_policy.get().policy.clone();
        let (session_questions, autopilot_word) = {
            let meta = ctx.meta.lock().unwrap_or_else(|p| p.into_inner());
            (meta.questions.clone(), meta.autopilot_word())
        };
        let agent = {
            let timeline = ctx.timeline.lock().unwrap_or_else(|p| p.into_inner());
            policy_agent(timeline.agent())
        };
        let posture = Posture {
            hosted: self.harness.is_some(),
            headless: self.headless,
            agent,
            suspended: self
                .harness
                .as_ref()
                .is_some_and(crate::harness_host::HostHandle::suspended),
            held: ctx.fabric.hold().is_some(),
            autopilot_word,
        };
        Ok(PolicyFacts {
            generation: self.harness_policy.generation(),
            effective: effective(&policy, posture),
            policy,
            session_questions,
        })
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
mod tests {
    use super::*;

    fn posture() -> Posture {
        Posture {
            hosted: true,
            headless: false,
            agent: Program::Codex,
            suspended: false,
            held: false,
            autopilot_word: None,
        }
    }

    fn facts(policy: SupervisorConfig) -> PolicyFacts {
        PolicyFacts {
            generation: 7,
            effective: effective(&policy, posture()),
            policy,
            session_questions: None,
        }
    }

    fn field(line: &str, key: &str) -> Option<String> {
        line.trim_end()
            .split(' ')
            .find_map(|kv| kv.strip_prefix(key)?.strip_prefix('='))
            .map(str::to_string)
    }

    #[test]
    fn every_refusal_reads_its_own_word_first_refusal_first() {
        let full = SupervisorConfig::default();
        assert_eq!(effective(&full, posture()), (true, "enabled"));
        let cases: [(Posture, SupervisorConfig, &str); 7] = [
            (
                Posture {
                    hosted: false,
                    ..posture()
                },
                SupervisorConfig {
                    enabled: false,
                    ..full.clone()
                },
                "unhosted",
            ),
            (
                posture(),
                SupervisorConfig {
                    enabled: false,
                    ..full.clone()
                },
                "disabled",
            ),
            (
                Posture {
                    headless: true,
                    held: true,
                    ..posture()
                },
                SupervisorConfig {
                    headless: false,
                    ..full.clone()
                },
                "headless",
            ),
            (
                Posture {
                    held: true,
                    ..posture()
                },
                SupervisorConfig {
                    off: vec![Program::Codex],
                    ..full.clone()
                },
                "agent-off",
            ),
            (
                Posture {
                    suspended: true,
                    held: true,
                    ..posture()
                },
                full.clone(),
                "suspended",
            ),
            (
                Posture {
                    held: true,
                    autopilot_word: Some(Autopilot::Off),
                    ..posture()
                },
                full.clone(),
                "hold",
            ),
            (
                Posture {
                    autopilot_word: Some(Autopilot::Off),
                    ..posture()
                },
                full.clone(),
                "autopilot-off",
            ),
        ];
        for (p, cfg, why) in cases {
            assert_eq!(effective(&cfg, p), (false, why), "{why}");
        }
        let claude_off = SupervisorConfig {
            off: vec![Program::Claude],
            ..full.clone()
        };
        assert_eq!(
            effective(
                &claude_off,
                Posture {
                    headless: true,
                    ..posture()
                }
            ),
            (true, "enabled")
        );
        let table_off = SupervisorConfig {
            autopilot: Autopilot::Off,
            ..full
        };
        assert_eq!(effective(&table_off, posture()), (false, "autopilot-off"));
        assert_eq!(
            effective(
                &table_off,
                Posture {
                    autopilot_word: Some(Autopilot::Cruise),
                    ..posture()
                }
            ),
            (true, "enabled")
        );
    }

    #[test]
    fn the_owners_reply_is_every_key_in_its_own_spelling_and_reads_back() {
        let mut cfg = SupervisorConfig::default();
        cfg.set("continue_per_hour", "4").expect("a count");
        cfg.set("answer_text", "Decide; prefer reversible steps.")
            .expect("a text");
        cfg.set("trust_roots", "~/src/app*,/abs path")
            .expect("a list");
        let line = reply(&facts(cfg.clone()), None, true);
        assert!(
            line.starts_with("OK gen=7 effective=on why=enabled contract=2,3 "),
            "{line}"
        );
        assert!(line.ends_with('\n') && !line.trim_end().contains('\n'));
        assert_eq!(field(&line, "questions").as_deref(), Some("recommended"));
        let keys: Vec<&str> = line
            .trim_end()
            .split(' ')
            .skip(5)
            .filter_map(|kv| kv.split_once('=').map(|(k, _)| k))
            .collect();
        let want: Vec<&str> = aterm_agent::supervise::config::KEYS
            .iter()
            .copied()
            .chain(["questions"])
            .chain(ACT_TEXT_KEYS)
            .collect();
        assert_eq!(keys, want);
        let mut back = SupervisorConfig::default();
        for (key, _) in cfg.texts() {
            let raw = field(&line, key).expect(key);
            let value = if raw == "-" {
                String::new()
            } else {
                aterm_control::wire::pct_decode(&raw)
            };
            if !value.is_empty() {
                back.set(key, &value)
                    .unwrap_or_else(|e| panic!("{key}: {e}"));
            }
        }
        assert_eq!(back.texts(), cfg.texts());
        assert_eq!(
            field(&line, "continue_text").map(|v| aterm_control::wire::pct_decode(&v)),
            Some(cfg.continue_text.clone())
        );
    }

    /// THE HANDSHAKE (contract v3 §8.1 item 1): the owner's reply lists the contracts this aterm
    /// serves, codex-alab's 2 and the roster's 3, effective or not; an edge token's reply, which
    /// can claim nothing, does not.
    #[test]
    fn the_owners_reply_lists_the_served_contracts() {
        let on = reply(&facts(SupervisorConfig::default()), None, true);
        assert_eq!(field(&on, "contract").as_deref(), Some("2,3"), "{on}");
        let off = PolicyFacts {
            effective: (false, "autopilot-off"),
            ..facts(SupervisorConfig::default())
        };
        assert_eq!(
            field(&reply(&off, None, true), "contract").as_deref(),
            Some("2,3")
        );
        assert_eq!(field(&reply(&off, None, false), "contract"), None);
    }

    /// The policy answers for the agent a rostered harness runs (Grok Build under grok-alab, whose
    /// program name maps to Grok), for an agent with a table by its own name, and for Codex when
    /// nothing is identified.
    #[test]
    fn the_policy_answers_for_the_agent_the_harness_runs() {
        let named = |program: Option<&str>| crate::session_timeline::AgentPublication {
            program: program.map(str::to_string),
            ..crate::session_timeline::AgentPublication::default()
        };
        assert_eq!(policy_agent(&named(Some("grok-alab"))), Program::Grok);
        assert_eq!(policy_agent(&named(Some("grok"))), Program::Grok);
        assert_eq!(policy_agent(&named(Some("claude"))), Program::Claude);
        assert_eq!(policy_agent(&named(Some("kimi"))), Program::Codex);
        assert_eq!(policy_agent(&named(Some("zsh"))), Program::Codex);
        assert_eq!(policy_agent(&named(None)), Program::Codex);
    }

    #[test]
    fn continue_text_carries_the_standing_rules_as_the_loops_continuation_does() {
        let cfg = SupervisorConfig::default();
        let line = reply(&facts(cfg.clone()), Some("never force-push"), true);
        let text = aterm_control::wire::pct_decode(&field(&line, "continue_text").expect("a text"));
        assert_eq!(text, continuation_text(&cfg, Some("never force-push")));
        assert_ne!(text, cfg.continue_text);
        assert!(text.ends_with("never force-push)"), "{text}");
    }

    /// A READER'S DONE CHECKS FROM THE REPLY `line`: the acts its rotation types at each claim in
    /// turn, as aterm's own loop types them. Each is `done_check_text` with its first angle given
    /// way to the claim's (the base before it and the tail and standing rules after it kept). A
    /// reply from a build before v6's 5a carries no `done_check_angles=`, or one whose first angle
    /// `done_check_text` does not hold: then the one fixed text, whole, at every claim (the
    /// downgrade). `None` where the reply holds no `done_check_text=` at all.
    fn done_checks_of(line: &str) -> Option<Vec<String>> {
        let field = |key: &str| {
            line.trim_end()
                .split(' ')
                .find_map(|kv| kv.strip_prefix(key)?.strip_prefix('='))
                .filter(|v| *v != "-")
                .map(aterm_control::wire::pct_decode)
        };
        let whole = field("done_check_text")?;
        let angles = field("done_check_angles").unwrap_or_default();
        let angles: Vec<&str> = angles.lines().filter(|a| !a.is_empty()).collect();
        let Some((before, after)) = angles.first().and_then(|first| whole.split_once(first)) else {
            return Some(vec![whole]);
        };
        Some(
            angles
                .iter()
                .map(|angle| format!("{before}{angle}{after}"))
                .collect(),
        )
    }

    fn act_texts_of(line: &str) -> Vec<(&'static str, String)> {
        ACT_TEXT_KEYS
            .iter()
            .map(|key| {
                let raw = field(line, key).unwrap_or_else(|| panic!("{key}: {line}"));
                assert_ne!(raw, "-", "{key} is never empty");
                (*key, aterm_control::wire::pct_decode(&raw))
            })
            .collect()
    }

    #[test]
    fn the_texts_are_the_loops_own_words_composed_as_it_composes_them() {
        use aterm_agent::supervise::policy::turn_end::{
            LAND, LAND_ANSWER, LAND_NUDGE, done_check_words,
        };
        let cfg = SupervisorConfig::default();
        let bare = act_texts_of(&reply(&facts(cfg.clone()), None, true));
        assert_eq!(
            bare,
            [
                ("done_check_text", done_check_words(0)),
                ("land_text", LAND.to_string()),
                ("land_nudge_text", LAND_NUDGE.to_string()),
                ("land_answer_text", LAND_ANSWER.to_string()),
                ("needs_hint_text", NEEDS_HINT.to_string()),
                ("needs_ack_text", NEEDS_ACK.to_string()),
                ("done_check_angles", DONE_ANGLES.join("\n")),
                ("done_check_tail", DONE_TAIL.to_string()),
            ]
        );
        let rules = "never force-push";
        let ruled = act_texts_of(&reply(&facts(cfg.clone()), Some(rules), true));
        assert_eq!(ruled, act_texts(Some(rules)).to_vec());
        assert_eq!(ruled[0].1, done_check_text(Some(rules), false));
        assert_eq!(ruled[1].1, land_text(Some(rules)));
        assert_eq!(ruled[2].1, land_nudge_text(Some(rules)));
        assert_eq!(ruled[3].1, land_answer_text(Some(rules)));
        for ((key, with), (_, without)) in ruled.iter().zip(&bare) {
            let suffix = matches!(
                *key,
                "needs_hint_text" | "needs_ack_text" | "done_check_angles" | "done_check_tail"
            );
            assert_eq!(with.contains(rules), !suffix, "{key}: {with}");
            if suffix {
                assert_eq!(with, without, "{key}");
            } else {
                assert!(with.starts_with(without.as_str()), "{key}: {with}");
            }
        }
        assert_ne!(bare[0].1, bare[1].1, "Cruise's words are not Land's");
        let edge = reply(&facts(cfg), Some(rules), false);
        assert!(
            ACT_TEXT_KEYS.iter().all(|k| field(&edge, k).is_none()),
            "{edge}"
        );
    }

    /// THE ANGLES ROUND-TRIP: a reader of the reply rebuilds each of the six done checks the loop
    /// types in turn, the standing rules after each, from `done_check_text`, `done_check_angles`
    /// and `done_check_tail`; and `done_check_text` stays one whole act, the first angle's.
    /// NEGATIVE CONTROL: main's reply, which has neither new key, reads as its one fixed text at
    /// every claim (the downgrade), and so does a reply whose angles `done_check_text` does not
    /// hold.
    #[test]
    fn the_angles_round_trip_and_a_reply_without_them_reads_as_one_text() {
        use aterm_agent::supervise::policy::turn_end::{
            DONE_CHECK, DONE_CHECK_FIXED_BEFORE, done_check_act,
        };
        let cfg = SupervisorConfig::default();
        let rules = "never force-push";
        let line = reply(&facts(cfg), Some(rules), true);
        let raw = field(&line, "done_check_angles").expect("the angles");
        assert!(!raw.contains('\n'), "one line on the wire: {raw}");
        let angles = aterm_control::wire::pct_decode(&raw);
        assert_eq!(angles.lines().collect::<Vec<_>>(), DONE_ANGLES);
        assert_eq!(
            field(&line, "done_check_tail").map(|t| aterm_control::wire::pct_decode(&t)),
            Some(DONE_TAIL.to_string())
        );
        let checks = done_checks_of(&line).expect("a done check");
        let typed: Vec<String> = (0..DONE_ANGLES.len())
            .map(|n| done_check_act(n, Some(rules), false))
            .collect();
        assert_eq!(checks, typed);
        assert_eq!(checks[0], done_check_text(Some(rules), false));
        for (check, angle) in checks.iter().zip(DONE_ANGLES) {
            assert!(check.starts_with(DONE_CHECK), "{check}");
            assert!(
                check.contains(angle) && check.contains(DONE_TAIL),
                "{check}"
            );
            assert!(check.ends_with(&format!("{rules})")), "{check}");
        }
        let mains: String = line
            .trim_end()
            .split(' ')
            .filter(|kv| {
                !kv.starts_with("done_check_angles=") && !kv.starts_with("done_check_tail=")
            })
            .map(|kv| match kv.strip_prefix("done_check_text=") {
                Some(_) => format!(
                    "done_check_text={}",
                    crate::control::pct_encode(DONE_CHECK_FIXED_BEFORE)
                ),
                None => kv.to_string(),
            })
            .collect::<Vec<_>>()
            .join(" ");
        assert!(field(&mains, "done_check_angles").is_none(), "{mains}");
        assert_eq!(
            done_checks_of(&mains),
            Some(vec![DONE_CHECK_FIXED_BEFORE.to_string()])
        );
        let foreign = format!(
            "{mains} done_check_angles={}",
            crate::control::pct_encode("Another angle.\nAnd another.")
        );
        assert_eq!(
            done_checks_of(&foreign),
            Some(vec![DONE_CHECK_FIXED_BEFORE.to_string()])
        );
        assert_eq!(done_checks_of("OK effective=on"), None);
    }

    #[test]
    fn questions_is_the_sessions_word_over_the_switch_and_the_priority() {
        let mut cfg = SupervisorConfig::default();
        assert_eq!(questions(&cfg, None), "recommended");
        assert_eq!(questions(&cfg, Some("ask")), "ask");
        cfg.set("question_priority", "long-term")
            .expect("a priority");
        assert_eq!(questions(&cfg, None), "long-term");
        cfg.answer_questions = false;
        assert_eq!(questions(&cfg, None), "ask");
        assert_eq!(questions(&cfg, Some("short-term")), "short-term");
    }

    #[test]
    fn an_edge_reads_only_effective_and_a_wrong_form_is_its_usage() {
        let f = facts(SupervisorConfig::default());
        assert_eq!(reply(&f, Some("secret rules"), false), "OK effective=on\n");
        let off = PolicyFacts {
            effective: (false, "hold"),
            ..f.clone()
        };
        assert_eq!(reply(&off, None, false), "OK effective=off\n");
        assert!(reply(&off, None, true).starts_with("OK gen=7 effective=off why=hold "));
        for rest in ["", "policy now", "show"] {
            assert_eq!(
                cmd(rest, true, || Ok(f.clone())),
                crate::harness_recovery_verb::USAGE,
                "{rest:?}"
            );
        }
        assert_eq!(
            cmd(" policy ", false, || Ok(f.clone())),
            "OK effective=on\n"
        );
        assert_eq!(
            cmd("policy", true, || Err("no such session 9".into())),
            "ERR no such session 9\n"
        );
    }

    #[test]
    fn the_policys_move_is_announced_on_every_sessions_stream() {
        let mut app = crate::App::headless_for_test();
        let session = app
            .front_terminal(crate::WindowId(0))
            .expect("a front terminal")
            .session;
        let events = |app: &crate::App| {
            let ctx = &app.pool.get(session).expect("pooled").ctx;
            let timeline = ctx.timeline.lock().unwrap_or_else(|p| p.into_inner());
            timeline
                .since(None)
                .filter(|e| e.kind == HARNESS_POLICY_EVENT)
                .map(|e| e.payload.to_string())
                .collect::<Vec<_>>()
        };
        let line = |app: &crate::App| {
            reply(
                &app.harness_policy_facts(session).expect("facts"),
                None,
                true,
            )
        };
        assert!(
            line(&app).starts_with("OK gen=0 effective=off why=unhosted "),
            "{}",
            line(&app)
        );
        assert!(events(&app).is_empty());
        let mut stamps = crate::harness_policy_cell::HarnessStampSource::new();
        let first = stamps.stamp(crate::app_config::HarnessPolicy::read(
            "[harness]\ncontinue_text = \"go on\"\n",
        ));
        let second = stamps.stamp(crate::app_config::HarnessPolicy::read(
            "[harness]\ncontinue_text = \"carry on\"\n",
        ));
        app.adopt_harness_policy(second);
        assert_eq!(events(&app), ["gen=2"]);
        let now = line(&app);
        assert!(now.starts_with("OK gen=2 "), "{now}");
        assert!(now.contains(" continue_text=carry%20on "), "{now}");
        app.adopt_harness_policy(first);
        assert_eq!(events(&app), ["gen=2"]);
        assert_eq!(
            crate::subscribe::timeline_wire_kind(HARNESS_POLICY_EVENT),
            Some("harness-policy"),
            "the digest pushes it"
        );
    }
}
