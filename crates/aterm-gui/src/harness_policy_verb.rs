// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! `@<sid> harness policy` (the native harness contract §3): aterm's
//! `[harness]` policy for ONE session, as one reader of aterm.toml answers it,
//! so an agent that supervises itself (codex-alab) never parses the file.
//!
//! ```text
//! OK gen=<cfg_gen> effective=<on|off> why=<word> <key>=<pct value|-> … questions=<word>
//!    done_check_text=<pct> land_text=<pct> land_nudge_text=<pct> land_answer_text=<pct>
//!    needs_hint_text=<pct> needs_ack_text=<pct>
//! ```
//!
//! - `gen=` is the policy cell's generation (`0`: the launch read); the
//!   `harness-policy gen=<n>` event on `subscribe @self` names the same
//!   number when the cell moves.
//! - `effective=`/`why=` are computed by aterm for THIS session, first refusal
//!   first: `unhosted` (no control socket, nothing supervises), `disabled`
//!   (`[harness] enabled = false`), `headless` (a headless instance under
//!   `headless = false`), `agent-off` (the session's agent's own table is
//!   off), `suspended` (the host is handed off for a seamless update),
//!   `hold` (a standing halt on the session), `autopilot-off` (the session's
//!   Autopilot is `off`); `enabled` when it is on.
//! - every `[harness]` key follows, in `SupervisorConfig`'s own spelling and
//!   order ([`SupervisorConfig::texts`]), percent-encoded, `-` for an empty
//!   value; `continue_text=` already composed with the `rules_file` text the
//!   way aterm's own continuation is ([`continuation_text`]);
//! - `questions=`, resolved: the session's own `meta questions` word over
//!   `answer_questions` (off reads `ask`) and `question_priority`;
//! - then Autopilot Cruise / Land's fixed words, ADDITIVE keys after
//!   `questions=` (the codex-alab contract's §3; owner 2026-10-19, "we need
//!   this functionality in ALL HARNESSES"), each the exact text aterm's own
//!   loop types for this session ([`act_texts`]), so a self-supervising agent
//!   types aterm's words and never a copy that drifts from them:
//!   `done_check_text=` (Cruise's defend-and-find-more prompt,
//!   `continue-done-check@v1`, without the open work, repository, reversible
//!   and ack suffixes a point adds), `land_text=` (`land@v1`),
//!   `land_nudge_text=` (`land-nudge@v1`) and `land_answer_text=`
//!   (`land-answer@v1`), each composed with the `rules_file` text exactly as
//!   the loop composes the act (the rules after the words, cut to fit the
//!   composer's verbatim cap); and `needs_hint_text=` (the NEEDS YOU hint
//!   after an answer) and `needs_ack_text=` (the ack once per new set of
//!   needs), the two SUFFIXES, bare: the loop types them inside an act,
//!   before that act's one copy of the standing rules, never with rules of
//!   their own — a suffix composed with them would type the rules twice in
//!   every act that carries it. The session's Autopilot word is no key here
//!   (`autopilot=` is the `[harness]` table's default): an agent reads its
//!   own from `status autopilot=`.
//!
//! An edge token reads only `OK effective=<on|off>`: the full policy, the
//! texts included, is the owner's.

use aterm_agent::supervise::policy::needs::{NEEDS_ACK, NEEDS_HINT};
use aterm_agent::supervise::policy::turn_end::{
    continuation_text, done_check_text, land_answer_text, land_nudge_text, land_text,
};
use aterm_agent::supervise::{Autopilot, SupervisorConfig};
use aterm_phase::Program;

/// What the main thread knows of one session's policy: the facts the reply is
/// made of, all but the rules file's text (read off the main thread).
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
    /// The session's agent, where the server names one (else Codex: the
    /// agent that asks).
    pub(crate) agent: Program,
    /// The host is suspended for a seamless update's handoff.
    pub(crate) suspended: bool,
    /// A halt (local or fleet) stands on the session.
    pub(crate) held: bool,
    /// The session's own Autopilot word, when it has one.
    pub(crate) autopilot_word: Option<Autopilot>,
}

/// `effective=`/`why=` (module doc), first refusal first.
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

/// `questions=`: the session's own word over `answer_questions` (off reads
/// `ask`) and `question_priority`.
#[must_use]
pub(crate) fn questions(cfg: &SupervisorConfig, session: Option<&str>) -> String {
    match session {
        Some(word) => word.to_string(),
        _ if !cfg.answer_questions => "ask".to_string(),
        _ => cfg.question_priority.word().to_string(),
    }
}

/// The additive text keys after `questions=` (module doc), in their wire
/// order.
pub(crate) const ACT_TEXT_KEYS: [&str; 6] = [
    "done_check_text",
    "land_text",
    "land_nudge_text",
    "land_answer_text",
    "needs_hint_text",
    "needs_ack_text",
];

/// Autopilot Cruise / Land's fixed words as aterm's own loop types them
/// for a session whose standing rules read `rules` (module doc), keyed by
/// [`ACT_TEXT_KEYS`]: the four whole acts through the turn-end policy's own
/// composers (the rules after the words), the two suffixes bare.
#[must_use]
pub(crate) fn act_texts(rules: Option<&str>) -> [(&'static str, String); 6] {
    let [done_check, land, nudge, answer, hint, ack] = ACT_TEXT_KEYS;
    [
        (done_check, done_check_text(rules, false)),
        (land, land_text(rules)),
        (nudge, land_nudge_text(rules)),
        (answer, land_answer_text(rules)),
        (hint, NEEDS_HINT.to_string()),
        (ack, NEEDS_ACK.to_string()),
    ]
}

/// The reply line (module doc), newline-terminated. `rules` is the standing
/// rules' text as the loop reads it
/// (`aterm_agent::supervise::run::standing_rules`); `owner` whether the
/// connection is owner-class.
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
    let mut line = format!("OK gen={} effective={on} why={why}", facts.generation);
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

/// The verb's control-thread half: `harness policy` (any other form is its
/// usage line), the facts fetched by `fetch`, the rules file read here.
pub(crate) fn cmd(
    rest: &str,
    owner: bool,
    fetch: impl FnOnce() -> Result<PolicyFacts, String>,
) -> String {
    if rest.split_whitespace().collect::<Vec<_>>() != ["policy"] {
        return "ERR usage: harness policy\n".to_string();
    }
    match fetch() {
        Ok(facts) => {
            // The rules file is read only for an owner's reply, off the
            // main thread.
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
            crate::harness_host::agent_of(timeline.agent()).unwrap_or(Program::Codex)
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

    /// The policy cell moved: every session's `subscribe @<sid>` stream gets
    /// `EVENT <local> harness-policy gen=<n>`, so an agent that answers for
    /// itself re-reads `harness policy` on it rather than on a clock.
    pub(crate) fn announce_harness_policy(&self) {
        let generation = self.harness_policy.generation();
        for pooled in self.pool.iter() {
            record_policy_move(&pooled.ctx.timeline, generation);
        }
    }
}

/// The timeline kind (and `events` wire word) of the policy cell's move.
pub(crate) const HARNESS_POLICY_EVENT: &str = "harness-policy";

/// The policy cell's move to `generation`, on one session's timeline: the
/// `harness-policy gen=<n>` row [`crate::App::announce_harness_policy`] writes on
/// every session's (the wire goldens write theirs through it too), and in the same
/// hold the session log's `mark - policy gen=<n>`, on every session as the row is.
pub(crate) fn record_policy_move(
    timeline: &std::sync::Mutex<crate::session_timeline::SessionTimeline>,
    generation: u64,
) {
    let mut tl = timeline.lock().unwrap_or_else(|p| p.into_inner());
    let at = tl.slog().now();
    tl.slog_mut().observe_policy(generation, at);
    tl.record(HARNESS_POLICY_EVENT, format!("gen={generation}"));
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
        // A headless instance under the default `headless = true` is on, and a
        // claude table switched off leaves codex on.
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
        // The table's own `autopilot = off` is off too; a session's `cruise`
        // word over it is on.
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

    /// The owner's reply carries every key in the config's own spelling and
    /// order, then `questions=`, then the six additive texts, each `[harness]`
    /// key read back by the config's own setter to the policy it was printed
    /// from (the one reader: nothing is respelled here).
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
            line.starts_with("OK gen=7 effective=on why=enabled "),
            "{line}"
        );
        assert!(line.ends_with('\n') && !line.trim_end().contains('\n'));
        assert_eq!(field(&line, "questions").as_deref(), Some("recommended"));
        let keys: Vec<&str> = line
            .trim_end()
            .split(' ')
            .skip(4)
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

    /// NEGATIVE CONTROL beside it: the reply's text is the composed one, so
    /// the bare `continue_text` with rules standing is not what it says.
    #[test]
    fn continue_text_carries_the_standing_rules_as_the_loops_continuation_does() {
        let cfg = SupervisorConfig::default();
        let line = reply(&facts(cfg.clone()), Some("never force-push"), true);
        let text = aterm_control::wire::pct_decode(&field(&line, "continue_text").expect("a text"));
        assert_eq!(text, continuation_text(&cfg, Some("never force-push")));
        assert_ne!(text, cfg.continue_text);
        assert!(text.ends_with("never force-push)"), "{text}");
    }

    /// The decoded value of every additive text key in `line`, in wire order.
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

    /// THE SIX ADDITIVE TEXTS ARE aterm's OWN WORDS, AS ITS LOOP TYPES THEM
    /// (the codex-alab contract's §3, Autopilot Cruise / Land): after
    /// `questions=`, `done_check_text`, `land_text`, `land_nudge_text` and
    /// `land_answer_text` are the turn-end policy's own acts — its constants
    /// with no rules standing, and with rules standing composed exactly as
    /// the loop composes them (the act's own suffix-free words, then the
    /// rules once) — and `needs_hint_text` and `needs_ack_text` are the two
    /// suffixes, bare. NEGATIVE CONTROLS: a suffix carries no rules (one
    /// composed with them would type the rules twice in every act it
    /// rides), the four acts do; Cruise's words are not Land's; and an edge
    /// reads none of them.
    #[test]
    fn the_six_texts_are_the_loops_own_words_composed_as_it_composes_them() {
        use aterm_agent::supervise::policy::turn_end::{DONE_CHECK, LAND, LAND_ANSWER, LAND_NUDGE};
        let cfg = SupervisorConfig::default();
        let bare = act_texts_of(&reply(&facts(cfg.clone()), None, true));
        assert_eq!(
            bare,
            [
                ("done_check_text", DONE_CHECK.to_string()),
                ("land_text", LAND.to_string()),
                ("land_nudge_text", LAND_NUDGE.to_string()),
                ("land_answer_text", LAND_ANSWER.to_string()),
                ("needs_hint_text", NEEDS_HINT.to_string()),
                ("needs_ack_text", NEEDS_ACK.to_string()),
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
            let suffix = matches!(*key, "needs_hint_text" | "needs_ack_text");
            assert_eq!(with.contains(rules), !suffix, "{key}: {with}");
            if suffix {
                assert_eq!(with, without, "{key}");
            } else {
                assert!(with.starts_with(without.as_str()), "{key}: {with}");
            }
        }
        assert_ne!(bare[0].1, bare[1].1, "Cruise's words are not Land's");
        // The texts stay out of an edge's reply.
        let edge = reply(&facts(cfg), Some(rules), false);
        assert!(
            ACT_TEXT_KEYS.iter().all(|k| field(&edge, k).is_none()),
            "{edge}"
        );
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
                "ERR usage: harness policy\n",
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

    /// THE REAL ADOPTION of a watcher's post moves the generation `harness
    /// policy` answers and records `harness-policy gen=<n>` on the session's
    /// timeline, which the `events` digest pushes; the facts are the real
    /// App's (a test App has no host: `unhosted`). NEGATIVE CONTROL: a stamp
    /// no newer than the one in force moves nothing and announces nothing.
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
        // Older than the one in force: nothing moves, nothing is said.
        app.adopt_harness_policy(first);
        assert_eq!(events(&app), ["gen=2"]);
        assert_eq!(
            crate::subscribe::timeline_wire_kind(HARNESS_POLICY_EVENT),
            Some("harness-policy"),
            "the digest pushes it"
        );
    }
}
