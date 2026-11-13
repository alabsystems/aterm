// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

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
        codex_rotation: false,
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
fn quota_capability_requires_host_support_and_is_owner_only() {
    let mut value = facts(SupervisorConfig::default());
    assert_eq!(field(&reply(&value, None, true), "codex_rotation"), None);
    value.codex_rotation = true;
    assert_eq!(
        field(&reply(&value, None, true), "codex_rotation").as_deref(),
        Some("1")
    );
    assert_eq!(field(&reply(&value, None, false), "codex_rotation"), None);
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
        .chain(["land_checks"])
        .collect();
    assert_eq!(keys, want);
    assert_eq!(field(&line, "land_checks").as_deref(), Some("git,left"));
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
/// can claim nothing, does not; and an aterm serving none (the negative control's seam,
/// `crate::harness_claim::served`) has no `contract=` key, the rest of its reply as before.
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
    let before = reply_serving(&facts(SupervisorConfig::default()), None, true, None);
    assert_eq!(field(&before, "contract"), None, "{before}");
    assert!(
        before.starts_with("OK gen=7 effective=on why=enabled "),
        "the rest of the reply stands: {before}"
    );
    assert_eq!(
        field(
            &reply_serving(&facts(SupervisorConfig::default()), None, true, Some("2,3")),
            "contract"
        )
        .as_deref(),
        Some("2,3")
    );
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
    assert_eq!(policy_agent(&named(Some("kimi"))), Program::Kimi);
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
        LAND, LAND_ANSWER, LAND_CHECK, LAND_NUDGE, done_check_words,
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
            ("land_check_text", LAND_CHECK.to_string()),
            ("land_question_text", LAND_TEXT.to_string()),
        ]
    );
    let rules = "never force-push";
    let ruled = act_texts_of(&reply(&facts(cfg.clone()), Some(rules), true));
    assert_eq!(ruled, act_texts(Some(rules)).to_vec());
    assert_eq!(ruled[0].1, done_check_text(Some(rules), false));
    assert_eq!(ruled[1].1, land_text(Some(rules)));
    assert_eq!(ruled[2].1, land_nudge_text(Some(rules)));
    assert_eq!(ruled[3].1, land_answer_text(Some(rules)));
    assert_eq!(ruled[8].1, land_check_text(None, false, Some(rules)));
    for ((key, with), (_, without)) in ruled.iter().zip(&bare) {
        let suffix = matches!(
            *key,
            "needs_hint_text"
                | "needs_ack_text"
                | "done_check_angles"
                | "done_check_tail"
                | "land_question_text"
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

/// THE LAND CHECK IS SERVED to a package that carries its own Land (codex-alab): its plain
/// form with the session's rules, as aterm's own loop composes it, never the hinted form
/// (that needs aterm's reader of the report).
#[test]
fn land_check_text_is_served_by_harness_policy() {
    use aterm_agent::supervise::policy::land_check::{
        LAND_CHECK, LAND_CHECK_HEAD, LAND_CHECK_HINT_OPEN,
    };
    let rules = "never force-push";
    for (rules, want) in [
        (None, LAND_CHECK.to_string()),
        (Some(rules), land_check_text(None, false, Some(rules))),
    ] {
        let served = act_texts_of(&reply(&facts(SupervisorConfig::default()), rules, true));
        let (_, check) = served
            .iter()
            .find(|(key, _)| *key == "land_check_text")
            .expect("served");
        assert_eq!(*check, want, "{rules:?}");
        assert!(check.starts_with(LAND_CHECK_HEAD), "{check}");
        assert!(
            !check.contains(LAND_CHECK_HINT_OPEN),
            "the plain form: {check}"
        );
    }
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
        .filter(|kv| !kv.starts_with("done_check_angles=") && !kv.starts_with("done_check_tail="))
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

/// LAND'S DIALOG TEXT IS ITS OWN KEY, AND `questions=` NEVER SAYS `land`: a session under Land
/// reads `land_question_text=` as the loop writes it into a free-text row, bare of the standing
/// rules, and `questions=` as the word configured, which a reader that knows no `land` keeps
/// answering by. NEGATIVE CONTROL: main's reply has no such key. MUTANT: `questions=` the
/// priority the loop takes under Land.
#[test]
fn lands_dialog_text_is_its_own_key_and_questions_never_says_land() {
    let land = SupervisorConfig {
        autopilot: Autopilot::Land,
        ..SupervisorConfig::default()
    };
    let line = reply(&facts(land.clone()), Some("never force-push"), true);
    assert_eq!(
        field(&line, "land_question_text"),
        Some(crate::control::pct_encode(LAND_TEXT))
    );
    assert_eq!(field(&line, "questions").as_deref(), Some("recommended"));
    let mut long = land;
    long.set("question_priority", "long-term")
        .expect("a priority");
    let line = reply(&facts(long), None, true);
    assert_eq!(field(&line, "questions").as_deref(), Some("long-term"));
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
