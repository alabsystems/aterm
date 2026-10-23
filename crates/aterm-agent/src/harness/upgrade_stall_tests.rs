// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

//! A usage-limit stall replayed through the real driver: Claude Code 2.1.280 parks a limited session at `⚠ Usage limit reached · continuing automatically at 6am`, so notices typed into it all arrive at the reset and a READY to the last marker can be heard by nobody.

use super::*;
use aterm_models_harness::harness_upgrade_never_strands_model;
use aterm_spec::derive::Model;
use std::collections::BTreeMap;

/// The replayed conversation, target and salt (its state file:
/// `"salt":1790372998`, `"to":"2.1.283"`).
const INCIDENT_SESSION: &str = "37dffac7-361e-46be-90c2-31589cb40b5c";
const INCIDENT_SALT: u64 = 1_790_372_998;

/// The ledger's four announcements (`t`, marker).
const LEDGER: [(u64, &str); 4] = [
    (1_790_377_339, "ATERM-UPGRADE-READY-5182b3fa"),
    (1_790_379_148, "ATERM-UPGRADE-READY-a39848f9"),
    (1_790_380_954, "ATERM-UPGRADE-READY-e5bc1bec"),
    (1_790_382_760, "ATERM-UPGRADE-READY-8cd7f7eb"),
];

/// The give-up, and the agent's READY the next morning.
const GAVE_UP_AT: u64 = 1_790_384_562;
const READY_AT: u64 = 1_790_427_721;

/// The row the supervisor journaled as `EVENT limited` before each notice.
const AUTO_CONTINUE: &str =
    "⚠ Usage limit reached · continuing automatically at 6am · esc to cancel";

fn version(s: &str) -> Version {
    Version::parse(s).expect("a version")
}

/// The facts of an idle Claude at an empty, settled composer — at its usage
/// limit or not.
fn idle(limited: bool) -> Facts {
    Facts {
        status: "idle".to_string(),
        status_age_s: 3_600,
        composer_empty: true,
        quiet_s: 3_600,
        limited,
        ..Facts::default()
    }
}

/// A transcript user row saying `text`, as Claude Code writes a typed turn.
fn user(text: &str) -> String {
    let row = aterm_json::to_string(&Value::from(text)).expect("json");
    format!(r#"{{"type":"user","isSidechain":false,"message":{{"role":"user","content":{row}}}}}"#)
}

/// An assistant row of the session's own model saying `text`.
fn said_by_agent(text: &str) -> String {
    let row = aterm_json::to_string(&Value::from(text)).expect("json");
    format!(
        r#"{{"type":"assistant","isSidechain":false,"message":{{"model":"claude-opus-5-5","role":"assistant","content":[{{"type":"text","text":{row}}}]}},"version":"2.1.280"}}"#
    )
}

/// The notice the replayed session was typed, with `marker`.
fn notice(marker: &str) -> String {
    upgrade::prepare_prompt(
        &version("2.1.280"),
        &version("2.1.283"),
        Source::Managed,
        marker,
    )
}

/// F2 over the replayed record: the ledger's markers are the ones this build mints from the
/// salt; after four notices and a give-up the late READY (to any of the four) is heard and
/// the restart taken, the owed release waiting behind it.
#[test]
fn the_incident_replays_to_a_restart_never_a_stall() {
    let to = version("2.1.283");
    for (asks, (_, marker)) in (1u64..).zip(LEDGER) {
        assert_eq!(
            upgrade::ready_marker(INCIDENT_SESSION, &to, INCIDENT_SALT + asks),
            marker,
            "the ledger's marker {asks}"
        );
    }
    let mut pending = Phase::Pending;
    for (at, _) in LEDGER {
        assert_eq!(
            upgrade::next_step(&pending, &idle(true), false, at),
            Step::Wait("limited")
        );
        pending = upgrade::clock_held(&pending, &idle(true), at);
    }
    let four = Phase::Announced {
        at_s: LEDGER[3].0,
        asks: upgrade::MAX_ASKS,
    };
    assert_eq!(
        upgrade::next_step(&four, &idle(true), false, GAVE_UP_AT),
        Step::Wait("limited"),
        "no give-up at the limit"
    );
    assert_eq!(
        upgrade::next_step(&Phase::Pending, &idle(false), false, LEDGER[0].0),
        Step::Announce
    );
    assert_eq!(
        upgrade::next_step(&four, &idle(false), false, GAVE_UP_AT),
        Step::GiveUp
    );

    let mut st = St {
        from: "2.1.280".to_string(),
        to: "2.1.283".to_string(),
        salt: INCIDENT_SALT,
        ..St::default()
    };
    for (asks, (at, marker)) in (1u32..).zip(LEDGER) {
        st.announced(marker.to_string(), at, asks);
    }
    st.give_up(GAVE_UP_AT);
    assert_eq!(st.phase, Phase::Failed(upgrade::GAVE_UP.to_string()));
    assert_eq!(st.release, "gave-up", "the agent is owed its release");
    let mut delivered: Vec<String> = LEDGER.iter().map(|(_, m)| user(&notice(m))).collect();
    delivered.push(said_by_agent(
        "Stopping here: both workflows are saved and nothing of mine runs.\n\
         ATERM-UPGRADE-READY-8cd7f7eb",
    ));
    let tail = delivered.join("\n");
    assert!(answered(&st, Some(&tail)), "the LAST marker is heard");
    assert_eq!(
        upgrade::next_step(&st.phase, &idle(false), true, READY_AT),
        Step::Terminate,
        "and the restart is taken"
    );
    assert_eq!(
        upgrade::gate_release(&idle(false), true),
        upgrade::Gate::Wait("ready"),
        "the release waits behind the READY the restart acts on"
    );
    for (_, marker) in LEDGER {
        let mut rows = delivered.clone();
        rows.pop();
        rows.push(said_by_agent(marker));
        assert!(
            answered(&st, Some(&rows.join("\n"))),
            "{marker} is heard too"
        );
    }
    let early = [
        user(&notice(LEDGER[0].1)),
        said_by_agent(LEDGER[0].1),
        user(&notice(LEDGER[1].1)),
    ]
    .join("\n");
    assert!(!answered(&st, Some(&early)));
    st.released();
    assert!(st.release.is_empty());
    assert!(!answered(&st, Some(&tail)));
    assert_eq!(
        upgrade::next_step(&st.phase, &idle(false), false, READY_AT),
        Step::Wait("failed")
    );
}

/// The owner's word holds a gave-up upgrade's late READY too, and a hold
/// that ends an announcement releases the agent it asked (F3).
#[test]
fn the_owners_hold_releases_the_agent_and_forgets_its_answers() {
    let mut announced = St {
        phase: Phase::Announced { at_s: 10, asks: 1 },
        ..St::default()
    };
    announced.announced("ATERM-UPGRADE-READY-0badf00d".to_string(), 10, 1);
    let mut stopped = announced.clone();
    stopped.stop("signal-refused", 10);
    assert_eq!(stopped.release, "signal-refused");
    let mut relaunch = announced.clone();
    relaunch.cause = crate::harness::relaunch::CAUSE_EXIT.to_string();
    relaunch.stop("no-resume", 10);
    assert!(relaunch.release.is_empty());
    let mut codex = announced.clone();
    codex.agent = upgrade::Agent::Codex;
    codex.give_up(10);
    assert!(codex.release.is_empty());
    let mut twice = announced.clone();
    twice.give_up(10);
    twice.stop("resumed-elsewhere", 10);
    assert_eq!(twice.release, "gave-up");
    twice.announced("ATERM-UPGRADE-READY-feedface".to_string(), 20, 1);
    assert!(twice.release.is_empty());
    let target = Candidate {
        exe: PathBuf::from("/x/claude"),
        version: version("9.9.9"),
        source: Source::Managed,
    };
    let on_280 = St {
        from: "2.1.280".to_string(),
        to: "2.1.283".to_string(),
        ..announced.clone()
    };
    let in_place = St::for_target(
        Some(on_280.clone()),
        &version("2.1.280"),
        &target,
        None,
        Proc::default(),
        30,
    );
    assert_eq!(in_place.to, "9.9.9");
    assert_eq!(in_place.phase, announced.phase, "the round kept");
    assert_eq!(in_place.marker, "ATERM-UPGRADE-READY-0badf00d");
    assert!(
        in_place.release.is_empty(),
        "nothing owed: {:?}",
        in_place.release
    );
    let other_model = St::for_target(
        Some(St {
            model_list: "claude-opus-5-5".to_string(),
            ..on_280.clone()
        }),
        &version("2.1.280"),
        &target,
        None,
        Proc::default(),
        30,
    );
    let moved = St::for_target(
        Some(on_280),
        &version("2.1.281"),
        &target,
        None,
        Proc::default(),
        30,
    );
    for retargeted in [other_model, moved] {
        assert_eq!(retargeted.phase, Phase::Pending);
        assert_eq!(retargeted.release, "retargeted");
        assert!(retargeted.marker.is_empty() && retargeted.markers.is_empty());
    }
}

/// The owner's `--now` is spent with the round it asked to move: an upgrade
/// `--now` re-armed that gives up asking clears the word, so a late READY
/// hours later restarts under the ordinary waits (a person at the tab holds
/// the signal).
#[test]
fn a_give_up_spends_the_owners_now() {
    let now = READY_AT;
    let attended = Facts {
        attended: true,
        ..idle(false)
    };
    let gave_up = Phase::Failed(upgrade::GAVE_UP.to_string());
    assert_eq!(
        upgrade::requested_step(&Request::Now, &gave_up, &attended, true, now, "2.1.283"),
        Step::Terminate,
        "a --now in force waives the person at the tab"
    );
    let mut st = St {
        to: "2.1.283".to_string(),
        request: Request::Now,
        request_tab: TAB.to_string(),
        request_at: GAVE_UP_AT - 60,
        ..St::default()
    };
    for (asks, (at, marker)) in (1u32..).zip(LEDGER) {
        st.announced(marker.to_string(), at, asks);
    }
    st.give_up(GAVE_UP_AT);
    assert_eq!(
        (st.request.clone(), st.request_tab.as_str(), st.request_at),
        (Request::None, "", 0)
    );
    assert_eq!(
        upgrade::requested_step(
            &st.request_for(TAB),
            &st.phase,
            &attended,
            true,
            now,
            &st.to
        ),
        Step::Wait("attended")
    );
    let mut skipped = St {
        request: Request::Skip("2.1.284".to_string()),
        request_tab: TAB.to_string(),
        ..st.clone()
    };
    skipped.give_up(GAVE_UP_AT);
    assert_eq!(skipped.request, Request::Skip("2.1.284".to_string()));
}

/// A limited Claude Code 2.1.280 tab as `text --json` sends it: the limit
/// banner over an empty composer, the caret at column 2.
#[cfg(unix)]
fn limited_screen() -> String {
    let rule = "─".repeat(40);
    format!(
        r#"{{"rows":["⏺ Both workflows are relaunched.","","{AUTO_CONTINUE}","","{rule}","❯ ","{rule}","  ⏵⏵ auto mode on (shift+tab to cycle)"],"cursor":{{"row":5,"col":2}},"seq":77,"human_ms":null,"first":0}}"#
    )
}

/// A tab with a person's half-typed draft in the composer.
#[cfg(unix)]
fn draft_screen() -> String {
    let rule = "─".repeat(20);
    format!(
        r#"{{"rows":["{rule}","❯ half a thought","{rule}"],"cursor":{{"row":1,"col":16}},"seq":77,"human_ms":null,"first":0}}"#
    )
}

/// A stand-in agent in [`TAB`], its session file, and the upgrade record
/// `st` filed for its conversation — the notice's fence naming it.
#[cfg(unix)]
struct Agent {
    child: aterm_uds::spawnfd::Child,
    sf: SessionFile,
}

#[cfg(unix)]
impl Agent {
    fn start(opts: &Opts, st: &St) -> Agent {
        let child = Command::new(std::env::current_exe().expect("exe"))
            .arg(PARK[0])
            .env(PARK_ENV, "1")
            .env("ATERM_PARENT_SESSION_ID", TAB)
            .env_remove("HOME")
            .setsid(true)
            .stdin(aterm_uds::spawnfd::Stdio::null())
            .stdout(aterm_uds::spawnfd::Stdio::null())
            .stderr(aterm_uds::spawnfd::Stdio::null())
            .spawn()
            .expect("agent");
        wait_exec(child.id());
        let sf = register(&opts.home, child.id(), SESSION);
        let st = St {
            tab: TAB.to_string(),
            notice_pid: sf.pid,
            notice_start: squash(&sf.proc_start),
            last_seq: 77,
            seq_since_s: now_s() - 3_600,
            ..st.clone()
        };
        std::fs::create_dir_all(state_dir(opts)).expect("state");
        std::fs::write(state_path(opts, SESSION), st.to_json()).expect("write");
        Agent { child, sf }
    }

    fn visit(&self, opts: &Opts, shell: u32) -> Report {
        self.visit_with(opts, shell, usize::MAX)
    }

    /// [`Self::visit`], the agent its shell's foreground job for the first
    /// `foreground` job reads and suspended after ([`Script`]).
    fn visit_with(&self, opts: &Opts, shell: u32, foreground: usize) -> Report {
        let table = vec![(shell, 1, "zsh".into())];
        let args = atpkg::caller_shell::process_args(self.sf.pid).expect("agent argv");
        let files = session_files(&opts.home);
        visit_with_claim(
            opts,
            &self.sf,
            files.as_deref(),
            &table,
            &newer(),
            &Script::new(shell, foreground, None),
            Some(&args),
            None,
        )
    }
}

#[cfg(unix)]
impl Drop for Agent {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

/// The conversation's transcript: `rows`, under `<home>/.claude/projects`.
#[cfg(unix)]
fn write_transcript(opts: &Opts, rows: &[String]) {
    let dir = opts.home.join(".claude/projects/-stand-in");
    std::fs::create_dir_all(&dir).expect("projects");
    std::fs::write(dir.join(format!("{SESSION}.jsonl")), rows.join("\n") + "\n")
        .expect("transcript");
}

/// The requests of `asked` that typed a turn saying `words`.
#[cfg(unix)]
fn typed(asked: &std::sync::Mutex<Vec<String>>, words: &str) -> usize {
    asked.lock().map_or(0, |a| {
        a.iter()
            .filter(|l| l.contains(" turn ") && l.contains(words))
            .count()
    })
}

/// F1 through the real visit: a limited tab (the banner over an empty composer, the session
/// file `idle`) is typed nothing: a pending upgrade waits `limited`, and so does one asked to
/// its bound long ago, its clock restarted instead of a give-up.
#[cfg(unix)]
#[test]
fn a_limited_tab_is_never_typed_into_and_never_given_up_on() {
    let dir = scratch("limited");
    let (sock, asked) = instance_with(
        &dir,
        Answers {
            screen: limited_screen(),
            ..Answers::default()
        },
    );
    let opts = Opts {
        sock: Some(sock),
        ..drive(&dir)
    };
    let shell = dead_pid();
    let pending = St {
        from: "1.0.0".to_string(),
        to: "9.9.9".to_string(),
        source: "managed".to_string(),
        ..St::default()
    };
    let agent = Agent::start(&opts, &pending);
    assert_eq!(agent.visit(&opts, shell).step, "wait:limited");
    assert_eq!(turns(&asked), 0, "nothing typed into the limited tab");

    let long_ago = now_s() - 5 * upgrade::REASK_S;
    let tired = St {
        phase: Phase::Announced {
            at_s: long_ago,
            asks: upgrade::MAX_ASKS,
        },
        marker: "ATERM-UPGRADE-READY-0badf00d".to_string(),
        markers: vec!["ATERM-UPGRADE-READY-0badf00d".to_string()],
        ..pending.clone()
    };
    drop(agent);
    let agent = Agent::start(&opts, &tired);
    let before = now_s();
    assert_eq!(agent.visit(&opts, shell).step, "wait:limited");
    assert_eq!(turns(&asked), 0);
    let kept = load(&opts, SESSION).expect("state");
    assert!(
        matches!(kept.phase, Phase::Announced { at_s, asks } if at_s >= before && asks == upgrade::MAX_ASKS),
        "the clock is held, the asks unspent, nothing given up: {:?}",
        kept.phase
    );
    assert_eq!(ledger_lines(&opts), 0, "no act on the record");
    drop(agent);

    let open = scratch("open");
    let (sock, asked) = instance(&open);
    let opts = Opts {
        sock: Some(sock),
        ..drive(&open)
    };
    file_transcript_in(&opts.home.join(".claude"), "p");
    let agent = Agent::start(&opts, &pending);
    assert_eq!(agent.visit(&opts, shell).step, "announced:1");
    assert_eq!(turns(&asked), 1);
    drop(agent);
    let _ = std::fs::remove_dir_all(&dir);
    let _ = std::fs::remove_dir_all(&open);
}

/// F3 through the real visit: a READY answer a person's draft held past the
/// drain's bound is void; the agent, stopped for a restart that is not
/// coming, is owed its release, which waits while the draft stands.
#[cfg(unix)]
#[test]
fn a_voided_ready_owes_one_release_typed_once() {
    let dir = scratch("void-held");
    let (sock, asked) = instance_with(
        &dir,
        Answers {
            screen: draft_screen(),
            ..Answers::default()
        },
    );
    let opts = Opts {
        sock: Some(sock),
        ..drive(&dir)
    };
    let marker = "ATERM-UPGRADE-READY-0badf00d";
    write_transcript(
        &opts,
        &[
            user("Tidy the parser module."),
            user(&notice(marker)),
            said_by_agent(&format!("Saved.\n{marker}")),
        ],
    );
    let now = now_s();
    let asked_st = St {
        phase: Phase::Announced {
            at_s: now - upgrade::DRAIN_S - 60,
            asks: 1,
        },
        from: "1.0.0".to_string(),
        to: "9.9.9".to_string(),
        source: "managed".to_string(),
        marker: marker.to_string(),
        markers: vec![marker.to_string()],
        hold_since_s: now - upgrade::HOLD_S - 30,
        hold_seen_s: now - 30,
        ..St::default()
    };
    let shell = dead_pid();
    let agent = Agent::start(&opts, &asked_st);
    let voided = agent.visit(&opts, shell);
    assert_eq!(voided.step, "drain-expired:draft");
    assert_eq!(turns(&asked), 0, "nothing typed over the person's draft");
    let st = load(&opts, SESSION).expect("state");
    assert_eq!(st.release, "void");
    assert!(st.marker.is_empty() && st.markers.is_empty());

    let cleared = scratch("void-free");
    let (sock, asked) = instance(&cleared);
    let free = Opts {
        sock: Some(sock),
        ..opts.clone()
    };
    let release = upgrade::release_prompt(upgrade::Agent::Claude, false);
    let first = agent.visit(&free, shell);
    assert_eq!(first.step, "released:void");
    assert_eq!(typed(&asked, "Upgrade off:"), 1, "{:?}", asked.lock());
    assert!(
        asked
            .lock()
            .expect("log")
            .iter()
            .any(|l| l.contains(" turn ") && l.ends_with(&release)),
        "the release, word for word"
    );
    let second = agent.visit(&free, shell);
    assert_eq!(
        second.step, "wait:awaiting-ready",
        "the re-ask waits its window"
    );
    assert_eq!(typed(&asked, "Upgrade off:"), 1, "typed once");
    assert_eq!(turns(&asked), 1, "and nothing else");
    let ledger = std::fs::read_to_string(state_dir(&opts).join("ledger.jsonl")).expect("ledger");
    assert_eq!(
        ledger.matches(r#""step":"released:void""#).count(),
        1,
        "{ledger}"
    );
    drop(agent);
    let _ = std::fs::remove_dir_all(&dir);
    let _ = std::fs::remove_dir_all(&cleared);
}

/// F2 and F3 through the real visit, on the replayed record (gave up, four
/// markers, the notice's fence on the agent): its READY to the last marker
/// reaches the restart (a dry run says so and ends nothing); with no READY,
/// the release is typed once and the markers forgotten; at the limit, the
/// release waits (`wait:release:limited`) and nothing is typed.
#[cfg(unix)]
#[test]
fn a_gave_up_record_restarts_on_a_late_ready_or_releases_its_agent() {
    let dir = scratch("gave-up");
    let (sock, asked) = instance(&dir);
    let opts = Opts {
        sock: Some(sock),
        ..drive(&dir)
    };
    let mut st = St {
        from: "1.0.0".to_string(),
        to: "9.9.9".to_string(),
        source: "managed".to_string(),
        ..St::default()
    };
    for (asks, (at, marker)) in (1u32..).zip(LEDGER) {
        st.announced(marker.to_string(), at, asks);
    }
    st.give_up(now_s());
    let mut delivered: Vec<String> = LEDGER.iter().map(|(_, m)| user(&notice(m))).collect();
    delivered.push(said_by_agent(&format!("Stopping.\n{}", LEDGER[3].1)));
    write_transcript(&opts, &delivered);
    let shell = dead_pid();
    let agent = Agent::start(&opts, &st);
    let dry = Opts {
        dry_run: true,
        ..opts.clone()
    };
    assert_eq!(agent.visit(&dry, shell).step, "would-restart");
    assert_eq!(turns(&asked), 0);
    let r = agent.visit_with(&opts, shell, 3);
    assert_eq!(
        r.step, "wait:changed-before-signal",
        "the restart was reached"
    );
    let kept = load(&opts, SESSION).expect("state");
    assert_eq!(kept.phase, Phase::Failed(upgrade::GAVE_UP.to_string()));
    assert_eq!(kept.release, "gave-up", "still owed: nothing was restarted");
    assert_eq!(turns(&asked), 0);

    delivered.pop();
    delivered.push(said_by_agent("Still waiting on the workflows."));
    write_transcript(&opts, &delivered);
    assert_eq!(agent.visit(&opts, shell).step, "released:gave-up");
    assert_eq!(typed(&asked, "Upgrade off:"), 1);
    let after_release = load(&opts, SESSION).expect("state");
    assert!(after_release.release.is_empty());
    assert!(after_release.markers.is_empty() && after_release.marker.is_empty());
    assert_eq!(agent.visit(&opts, shell).step, "wait:failed");
    assert_eq!(turns(&asked), 1, "typed once");
    drop(agent);

    let limited = scratch("gave-up-lim");
    let (sock, asked) = instance_with(
        &limited,
        Answers {
            screen: limited_screen(),
            ..Answers::default()
        },
    );
    let opts = Opts {
        sock: Some(sock),
        ..drive(&limited)
    };
    let agent = Agent::start(&opts, &st);
    assert_eq!(agent.visit(&opts, shell).step, "wait:release:limited");
    assert_eq!(turns(&asked), 0);
    assert_eq!(load(&opts, SESSION).expect("state").release, "gave-up");
    drop(agent);
    let _ = std::fs::remove_dir_all(&dir);
    let _ = std::fs::remove_dir_all(&limited);
}

/// A stale status never reaches the signal nor strands the READY agent: with Claude Code's
/// status left `busy` over an idle screen and nothing under the agent, the restart waits
/// `status-stale` (the Drain step needs Claude's own `idle`: work inside the agent's process
/// is no process under it), and once the READY has stood a whole [`upgrade::REASK_S`] the
/// agent is asked again (`announced:2`).
#[cfg(unix)]
#[test]
fn a_ready_agent_under_a_stale_status_is_never_signalled_and_is_asked_again() {
    let marker = "ATERM-UPGRADE-READY-0badf00d";
    let run = |name: &str, status: &str, ready_age: u64, visits: usize| {
        let dir = scratch(name);
        let (sock, asked) = instance(&dir);
        let opts = Opts {
            sock: Some(sock),
            ..drive(&dir)
        };
        write_transcript(
            &opts,
            &[
                user("Tidy the parser module."),
                user(&notice(marker)),
                said_by_agent(&format!("Saved.\n{marker}")),
            ],
        );
        let st = St {
            phase: Phase::Announced {
                at_s: now_s() - ready_age,
                asks: 1,
            },
            from: "1.0.0".to_string(),
            to: "9.9.9".to_string(),
            source: "managed".to_string(),
            marker: marker.to_string(),
            markers: vec![marker.to_string()],
            ready_since: now_s() - ready_age,
            ..St::default()
        };
        let mut agent = Agent::start(&opts, &st);
        let file = opts
            .home
            .join(format!(".claude/sessions/{}.json", agent.sf.pid));
        let text = std::fs::read_to_string(&file).expect("session file");
        std::fs::write(
            &file,
            text.replace(r#""status":"idle""#, &format!(r#""status":"{status}""#)),
        )
        .expect("rewrite");
        agent.sf = session_file_of(&opts.home, agent.sf.pid).expect("parses");
        let steps: Vec<String> = (0..visits)
            .map(|_| agent.visit_with(&opts, dead_pid(), 4).step)
            .collect();
        let signalled = asked
            .lock()
            .expect("asked")
            .iter()
            .any(|l| l.contains("signal"));
        let _ = std::fs::remove_dir_all(&dir);
        (steps, turns(&asked), signalled)
    };
    let words = |w: &[&str]| w.iter().map(|s| (*s).to_string()).collect::<Vec<_>>();
    assert_eq!(
        run("stale-restart", "busy", 60, 2),
        (words(&["wait:status-stale", "wait:status-stale"]), 0, false),
        "the lag reached: still no restart on a status that is not idle"
    );
    assert_eq!(
        run("stale-reask", "busy", upgrade::REASK_S + 60, 2),
        (words(&["wait:status-stale", "announced:2"]), 1, false),
        "the READY it held a whole REASK_S: asked again, nothing ended"
    );
    assert_eq!(
        run("idle-restart", "idle", 60, 1),
        (words(&["wait:changed-before-signal"]), 0, false),
        "an idle agent's restart reaches its last look"
    );
}

/// A late READY whose restart's gate waits keeps the gate's word, not the owed release's: the
/// host owns the turn ends while the restart settles, and a draft under it is looked at every
/// [`HOLD_LOOK`].
#[cfg(unix)]
#[test]
fn a_late_ready_whose_restart_waits_keeps_its_gates_word() {
    let mut st = St {
        from: "1.0.0".to_string(),
        to: "9.9.9".to_string(),
        source: "managed".to_string(),
        ..St::default()
    };
    for (asks, (at, marker)) in (1u32..).zip(LEDGER) {
        st.announced(marker.to_string(), at, asks);
    }
    st.give_up(now_s());
    let mut delivered: Vec<String> = LEDGER.iter().map(|(_, m)| user(&notice(m))).collect();
    delivered.push(said_by_agent(&format!("Stopping.\n{}", LEDGER[3].1)));
    for (name, screen, word) in [
        ("late-settling", None, "wait:settling"),
        ("late-draft", Some(draft_screen()), "wait:draft"),
    ] {
        let dir = scratch(name);
        let (sock, asked) = instance_with(
            &dir,
            Answers {
                screen: screen.unwrap_or_else(idle_screen),
                ..Answers::default()
            },
        );
        let opts = Opts {
            sock: Some(sock),
            ..drive(&dir)
        };
        write_transcript(&opts, &delivered);
        let agent = Agent::start(&opts, &st);
        let mut moved = load(&opts, SESSION).expect("state");
        moved.last_seq = 1;
        save(&opts, SESSION, &moved);
        let r = agent.visit(&opts, dead_pid());
        assert_eq!(r.step, word, "{name}");
        assert_eq!(turns(&asked), 0, "{name}: nothing typed");
        let kept = load(&opts, SESSION).expect("state");
        assert_eq!(kept.release, "gave-up", "{name}: still owed");
        assert_eq!(kept.markers.len(), 4, "{name}: the READY still heard");
        if word == "wait:settling" {
            assert!(
                owns_turn_ends(&r.step, 0, 120),
                "the host owns the turn ends"
            );
        } else {
            assert_eq!(after(&r.step, 9), After::Later(HOLD_LOOK));
        }
        drop(agent);
        let _ = std::fs::remove_dir_all(&dir);
    }
}

/// A user row as Claude Code writes one on its own: `extra` fields beside
/// the message (`"isMeta":true`), its content the JSON `content`.
fn user_row(extra: &str, content: &str) -> String {
    format!(
        r#"{{"type":"user","isSidechain":false,{extra}"message":{{"role":"user","content":{content}}}}}"#
    )
}

/// Who has directed the conversation since the agent's last answer
/// ([`upgrade::directed_since_ready`]), over real user rows: a person's words, a `!` command,
/// an Esc, a peer's message and a continuation are directions; the harness's own lines, tool
/// results, `isMeta` rows, compaction summaries, task notifications and slash commands are
/// not.
#[test]
fn a_direction_since_the_last_answer_is_a_turn_typed_by_someone_else() {
    let notice_row = user(&notice(LEDGER[0].1));
    let markers = vec![LEDGER[0].1.to_string()];
    let quiet = [
        user_row(
            r#""isMeta":true,"#,
            r#""Your claude.ai usage limit has reset. Continue the task you were working on""#,
        ),
        user_row(
            "",
            r#"[{"type":"tool_result","tool_use_id":"t1","content":"scan-done"}]"#,
        ),
        user_row("", r#""<task-notification>\n<task-id>w89</task-id> done""#),
        user_row("", r#""<command-name>/rate-limit-options</command-name>""#),
        user_row(
            "",
            r#""<local-command-stdout>Claude Code will continue automatically at 6am""#,
        ),
        user_row(
            r#""isCompactSummary":true,"#,
            r#""This session is being continued from a previous conversation""#,
        ),
        user(&upgrade::release_prompt(upgrade::Agent::Claude, false)),
        said_by_agent("Nothing of mine is running."),
    ];
    let with = |rows: &[String]| {
        let mut all = vec![notice_row.clone()];
        all.extend_from_slice(rows);
        all.join("\n")
    };
    let directed = |rows: &[String]| upgrade::directed_since_ready(&with(rows), &markers);
    assert!(!directed(&quiet));
    let ready = said_by_agent(&format!("Saved.\n{}", LEDGER[0].1));
    let on_it = said_by_agent("On it.");
    for direction in [
        user("Actually, switch to the parser work."),
        user("keep going"),
        user("[Request interrupted by user]"),
        user("[from s-d3346b29] v0.91.0 is out"),
        user("<bash-input>git status</bash-input>"),
        user_row(
            "",
            r#"[{"type":"text","text":"look at this"},{"type":"image"}]"#,
        ),
    ] {
        let mut rows = quiet.to_vec();
        rows.push(direction.clone());
        assert!(!directed(&rows), "unanswered: {direction}");
        rows.push(on_it.clone());
        assert!(directed(&rows), "{direction}");
        let mut met = quiet.to_vec();
        met.push(direction.clone());
        met.push(said_by_agent("x").replace("claude-opus-5-5", "<synthetic>"));
        assert!(!directed(&met), "<synthetic>: {direction}");
        let before = [
            direction.clone(),
            on_it.clone(),
            notice_row.clone(),
            said_by_agent("ok"),
        ]
        .join("\n");
        assert!(
            !upgrade::directed_since_ready(&before, &markers),
            "{direction}"
        );
        let then_ready = [direction.clone(), on_it.clone(), ready.clone()];
        assert!(!directed(&then_ready), "then READY: {direction}");
        assert!(
            upgrade::directed_since_ready(&with(&then_ready), &[]),
            "the READY is a marker of the round: {direction}"
        );
        let mut past = then_ready.to_vec();
        past.push(direction.clone());
        past.push(on_it.clone());
        assert!(directed(&past), "after READY: {direction}");
    }
    let side = user("do the thing").replace(r#""isSidechain":false"#, r#""isSidechain":true"#);
    assert!(!directed(&[side, on_it]));
}

/// A release is typed only to the agent the notice reached. A restart past
/// its signal owes none (its agent was ended; the conversation is the
/// carry-on's, or the person's who resumed it).
#[test]
fn a_release_follows_only_the_process_the_notice_reached() {
    for phase in [Phase::Exiting { at_s: 10 }, Phase::Relaunched { at_s: 10 }] {
        let mut st = St {
            phase: phase.clone(),
            ..St::default()
        };
        st.stop("resumed-elsewhere", 10);
        assert!(
            st.release.is_empty(),
            "{phase:?}: nothing owed after the signal"
        );
    }
    let mut asked = St {
        to: "2.1.283".to_string(),
        tab: TAB.to_string(),
        notice_pid: 4242,
        notice_start: "Fri Sep 25 16:00:00 2026".to_string(),
        ..St::default()
    };
    asked.announced("ATERM-UPGRADE-READY-0badf00d".to_string(), 10, 1);
    let target = Candidate {
        exe: PathBuf::from("/x/claude"),
        version: version("9.9.9"),
        source: Source::Managed,
    };
    let mut stopped = asked.clone();
    stopped.stop("signal-refused", 10);
    for prior in [asked, stopped] {
        let st = St::for_target(
            Some(prior.clone()),
            &version("2.1.280"),
            &target,
            None,
            Proc::default(),
            30,
        );
        assert!(!st.release.is_empty(), "{:?}", prior.phase);
        assert_eq!(
            (st.tab.as_str(), st.notice_pid, st.notice_start.as_str()),
            (TAB, 4242, "Fri Sep 25 16:00:00 2026"),
            "{:?}: the notice's fence goes with the release",
            prior.phase
        );
    }
}

/// Through the real visit, an owed release is dropped (ledgered once, nothing typed) where it
/// is no longer this upgrade's: the process visited is not the one the notice reached, or
/// someone has directed the conversation since.
#[cfg(unix)]
#[test]
fn a_release_is_dropped_where_it_is_no_longer_the_upgrades() {
    let mut st = St {
        from: "1.0.0".to_string(),
        to: "9.9.9".to_string(),
        source: "managed".to_string(),
        ..St::default()
    };
    for (asks, (at, marker)) in (1u32..).zip(LEDGER) {
        st.announced(marker.to_string(), at, asks);
    }
    st.give_up(now_s());
    let mut after_notice: Vec<String> = LEDGER.iter().map(|(_, m)| user(&notice(m))).collect();
    after_notice.push(user_row(
        r#""isMeta":true,"#,
        r#""Your claude.ai usage limit has reset. Continue the task you were working on""#,
    ));
    after_notice.push(user_row(
        "",
        r#"[{"type":"tool_result","tool_use_id":"t1","content":"ok"}]"#,
    ));
    after_notice.push(user_row("", r#""<task-notification>\ndone""#));
    after_notice.push(said_by_agent("Still waiting on the workflows."));
    let mut directed = after_notice.clone();
    directed.push(user("Actually, switch to the parser work."));
    directed.push(said_by_agent("Switching."));
    let current = Targets {
        managed: None,
        native: None,
        unanswered: false,
    };
    for (name, rows, other, targets, word) in [
        (
            "rel-typed",
            &after_notice,
            false,
            newer(),
            "released:gave-up",
        ),
        ("rel-directed", &directed, false, newer(), "wait:failed"),
        ("rel-other", &after_notice, true, newer(), "wait:failed"),
        ("rel-other-current", &after_notice, true, current, "current"),
    ] {
        let dir = scratch(name);
        let (sock, asked) = instance(&dir);
        let opts = Opts {
            sock: Some(sock),
            ..drive(&dir)
        };
        write_transcript(&opts, rows);
        let agent = Agent::start(&opts, &st);
        if other {
            let mut moved = load(&opts, SESSION).expect("state");
            moved.notice_pid = dead_pid();
            save(&opts, SESSION, &moved);
        }
        let shell = dead_pid();
        let table = vec![(shell, 1, "zsh".into())];
        let args = atpkg::caller_shell::process_args(agent.sf.pid).expect("argv");
        let files = session_files(&opts.home);
        let r = visit_with_claim(
            &opts,
            &agent.sf,
            files.as_deref(),
            &table,
            &targets,
            &Script::new(shell, usize::MAX, None),
            Some(&args),
            None,
        );
        assert_eq!(r.step, word, "{name}");
        let kept = load(&opts, SESSION).expect("state");
        assert!(kept.release.is_empty(), "{name}: nothing owed after");
        assert!(kept.markers.is_empty(), "{name}: the round is over");
        let ledger =
            std::fs::read_to_string(state_dir(&opts).join("ledger.jsonl")).unwrap_or_default();
        if word == "released:gave-up" {
            assert_eq!(typed(&asked, "Upgrade off:"), 1, "{name}");
            assert!(!ledger.contains("release-dropped"), "{name}");
        } else {
            assert_eq!(turns(&asked), 0, "{name}: nothing typed");
            let why = if other { "other-process" } else { "directed" };
            assert_eq!(
                ledger
                    .matches(&format!(r#""step":"release-dropped:{why}""#))
                    .count(),
                1,
                "{name}: {ledger}"
            );
        }
        drop(agent);
        let _ = std::fs::remove_dir_all(&dir);
    }
}

/// A dropped release takes its gate's wait with it: a pending round owing `retargeted`, its
/// last look's wait `release:attended`, visited with no build to move to while the notice's
/// process no longer holds the conversation, drops the release (`release-dropped:other-process`)
/// and clears the wait, so `--status` never says the line is still owed.
#[cfg(unix)]
#[test]
fn a_dropped_release_leaves_no_owed_wait_behind() {
    let now = now_s();
    let st = St {
        from: "1.0.0".to_string(),
        to: "9.9.9".to_string(),
        source: "managed".to_string(),
        phase: Phase::Pending,
        pending_since: now - 7 * 3_600,
        release: "retargeted".to_string(),
        wait: "release:attended".to_string(),
        wait_since: now - 600,
        ..St::default()
    };
    let dir = scratch("rel-dropped-wait");
    let (sock, asked) = instance(&dir);
    let opts = Opts {
        sock: Some(sock),
        ..drive(&dir)
    };
    write_transcript(&opts, &[said_by_agent("Working.")]);
    let agent = Agent::start(&opts, &st);
    let mut moved = load(&opts, SESSION).expect("state");
    moved.notice_pid = dead_pid();
    save(&opts, SESSION, &moved);
    let current = Targets {
        managed: None,
        native: None,
        unanswered: false,
    };
    let shell = dead_pid();
    let table = vec![(shell, 1, "zsh".into())];
    let args = atpkg::caller_shell::process_args(agent.sf.pid).expect("argv");
    let files = session_files(&opts.home);
    let visit = || {
        visit_with_claim(
            &opts,
            &agent.sf,
            files.as_deref(),
            &table,
            &current,
            &Script::new(shell, usize::MAX, None),
            Some(&args),
            None,
        )
    };
    let first = visit();
    let kept = load(&opts, SESSION).expect("state");
    let again = visit();
    let kept_again = load(&opts, SESSION).expect("state");
    let (rows, vetted) = upgrade_status::status_rows(&opts, true);
    let at = now_s();
    let ledger = std::fs::read_to_string(state_dir(&opts).join("ledger.jsonl")).unwrap_or_default();
    drop(agent);
    let _ = std::fs::remove_dir_all(&dir);

    assert_eq!(turns(&asked), 0, "nothing typed");
    assert_eq!(
        (first.step.as_str(), again.step.as_str()),
        ("current", "current")
    );
    assert_eq!(
        ledger
            .matches(r#""step":"release-dropped:other-process""#)
            .count(),
        1,
        "{ledger}"
    );
    assert!(kept.release.is_empty(), "the release was dropped");
    assert_eq!(
        (kept.wait.as_str(), kept.wait_since),
        ("", 0),
        "the dropped release's gate goes with it"
    );
    assert_eq!(kept_again.wait, "", "and stays gone");
    assert!(vetted);
    assert_eq!(rows.len(), 1, "{rows:?}");
    let line = rows[0].line(at);
    assert!(line.contains(" wait=- "), "{line}");
    assert!(line.contains(" release=- "), "{line}");
}

/// F1 from the window: the loop's limit episode holds the upgrade's clocks ([`hold_clock`],
/// called by the host as an episode opens and closes).
#[test]
fn a_limit_episode_holds_the_upgrades_clocks_for_the_window() {
    let dir = scratch("hold-clock");
    let opts = Opts {
        only_sid: Some(TAB.to_string()),
        ..drive(&dir)
    };
    let long_ago = 1_000;
    let until = 50_000;
    let base = St {
        to: "9.9.9".to_string(),
        tab: TAB.to_string(),
        ..St::default()
    };
    let tired = St {
        phase: Phase::Announced {
            at_s: long_ago,
            asks: upgrade::MAX_ASKS,
        },
        ..base.clone()
    };
    let late = St {
        phase: Phase::Failed(upgrade::GAVE_UP.to_string()),
        ready_since: long_ago,
        ..base.clone()
    };
    let elsewhere = St {
        tab: "s-0ther".to_string(),
        ..tired.clone()
    };
    let pending = base;
    let records = [
        ("0badf00d-1111-2222-3333-000000000001", &tired),
        ("0badf00d-1111-2222-3333-000000000002", &late),
        ("0badf00d-1111-2222-3333-000000000003", &elsewhere),
        ("0badf00d-1111-2222-3333-000000000004", &pending),
    ];
    std::fs::create_dir_all(state_dir(&opts)).expect("state");
    for (session, st) in records {
        save(&opts, session, st);
    }
    let first_idle = until + 60;
    assert_eq!(
        upgrade::next_step(&tired.phase, &idle(false), false, first_idle),
        Step::GiveUp,
        "unheld, the window ran out under the limit"
    );
    {
        let _other = sweep_lock(&opts).expect("lock").expect("a lock");
        assert!(!hold_clock(&opts, until), "another sweep holds the lock");
    }
    assert_eq!(load(&opts, records[0].0).expect("state").phase, tired.phase);
    assert!(hold_clock(&opts, until));
    let held = load(&opts, records[0].0).expect("state");
    assert_eq!(
        held.phase,
        Phase::Announced {
            at_s: until,
            asks: upgrade::MAX_ASKS
        }
    );
    assert_eq!(
        upgrade::next_step(&held.phase, &idle(false), false, first_idle),
        Step::Wait("awaiting-ready"),
        "held, the agent has its whole window"
    );
    assert_eq!(load(&opts, records[1].0).expect("state").ready_since, until);
    assert_eq!(
        load(&opts, records[2].0).expect("state").phase,
        elsewhere.phase
    );
    assert_eq!(
        load(&opts, records[3].0).expect("state").phase,
        Phase::Pending
    );
    assert!(hold_clock(&opts, until - 10));
    assert_eq!(
        load(&opts, records[0].0).expect("state").phase,
        held.phase,
        "never back"
    );
    let _ = std::fs::remove_dir_all(&dir);
}

/// A model state.
type S = BTreeMap<&'static str, i64>;

/// The model's notices to its bound, and the real ones: a notice short of the
/// bound is the one before the last (`MAX_ASKS - 1`), the bound the last.
const MODEL_MAX_ASKS: i64 = 2;
const T0: u64 = 1_790_377_339;

/// The agent the notice reached, as its session file names it.
const AGENT_PID: u32 = 4242;
const AGENT_START: &str = "Thu Sep 25 23:00:00 2026";

fn real_asks(asks: i64) -> u32 {
    if asks >= MODEL_MAX_ASKS {
        upgrade::MAX_ASKS
    } else {
        upgrade::MAX_ASKS - 1
    }
}

fn model_asks(asks: u32) -> i64 {
    if asks >= upgrade::MAX_ASKS {
        MODEL_MAX_ASKS
    } else {
        1
    }
}

fn marker(k: u64) -> String {
    upgrade::ready_marker(SESSION, &version("9.9.9"), 7 + k)
}

/// The session file of the agent the notice reached.
fn agent_file() -> SessionFile {
    SessionFile {
        pid: AGENT_PID,
        session_id: SESSION.to_string(),
        cwd: "/".to_string(),
        version: "1.0.0".to_string(),
        status: "idle".to_string(),
        status_updated_at_ms: 0,
        proc_start: AGENT_START.to_string(),
        kind: "interactive".to_string(),
        entrypoint: "cli".to_string(),
        claude_dir: None,
        profile: None,
    }
}

/// What holds the look a model state stands for, beyond an idle, settled
/// session with an empty composer.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Holds {
    /// Nothing: the reducer's own step.
    Nothing,
    /// A person's box, standing [`upgrade::HOLD_S`].
    Person,
    /// The agent's own background work, under a READY heard
    /// [`upgrade::DRAIN_S`] ago.
    Background,
    /// The same at a break, the work inside the agent's own process (Claude's status `busy`,
    /// nothing under it), judged as [`Holds::Background`].
    InProcess,
    /// The same work under the same READY, looked at from a break of it (`brk`,
    /// [`upgrade::Facts::background_point`]), the step held to [`upgrade::break_step`].
    Break,
    /// The restart's signal is refused by the kernel, or a re-ask's plan
    /// by the relaunch — the round abandoned.
    Refused,
}

/// A peer's message to the agent: the direction the model's `Direct`
/// stands for (a person's words, an Esc and the supervisor's continuation
/// read the same, [`upgrade::directed_since_ready`]).
fn direction() -> String {
    user("[from s-d3346b29] v0.91.0 is out; the parser branch can wait")
}

/// The agent's READY to the round's first marker.
fn ready_row() -> String {
    said_by_agent(&format!("Saved.\n{}", marker(1)))
}

/// Whether the transcript a model state stands for may carry an earlier direction the agent
/// answered with READY ([`real_of`]): wherever a READY may stand in the tail without being an
/// answer the state denies.
fn history_fits(s: &S) -> bool {
    s["ready"] == 1 || s["live"] == 0 || s["told"] == 1 || s["directed"] == 1
}

/// The record, facts, clock and transcript a model state stands for: the record of the agent
/// the notice reached, the round's marker typed.
fn real_of(s: &S, history: bool) -> (St, Facts, u64, Vec<String>) {
    let phase = match s["phase"] {
        0 => Phase::Pending,
        1 => Phase::Announced {
            at_s: T0,
            asks: real_asks(s["asks"]),
        },
        2 => Phase::Failed(upgrade::GAVE_UP.to_string()),
        3 => Phase::Done,
        _ => Phase::Failed("signal-refused".to_string()),
    };
    let live = s["live"] == 1;
    let owed = s["owed"] == 1;
    let now = if s["window"] == 1 {
        T0 + upgrade::REASK_S
    } else {
        T0 + 60
    };
    let failed_at = match s["phase"] {
        2 | 4 if s["rest"] == 1 => now - upgrade::RETRY_S,
        2 | 4 => now,
        _ => 0,
    };
    let st = St {
        phase,
        failed_at,
        to: "9.9.9".to_string(),
        tab: TAB.to_string(),
        notice_pid: AGENT_PID,
        notice_start: squash(AGENT_START),
        marker: if live { marker(1) } else { String::new() },
        markers: if live { vec![marker(1)] } else { Vec::new() },
        release: if owed {
            "owed".to_string()
        } else {
            String::new()
        },
        asked: if s["phase"] == 0 && s["owed"] == 0 {
            Vec::new()
        } else {
            vec![marker(1)]
        },
        ..St::default()
    };
    let mut tail = vec![user(&notice(&marker(1)))];
    if history {
        tail.extend([direction(), ready_row()]);
    } else if s["ready"] == 1 {
        tail.push(ready_row());
    } else {
        tail.push(said_by_agent("Winding down."));
    }
    if s["told"] == 1 {
        tail.push(direction());
    }
    if s["directed"] == 1 {
        tail.extend([direction(), said_by_agent("On it.")]);
    }
    let mut f = idle(s["limited"] == 1);
    if s["brk"] == 1 {
        f.status = "shell".to_string();
        f.background = vec!["zsh".to_string()];
        f.background_point = true;
    }
    (st, f, now, tail)
}

/// A record and a transcript projected onto the model's variables, from the
/// record's raw state, not the driver's reading of it: `live`, the round's
/// markers kept; `ready`, an answer to one of them ([`answered`]); `rest`, a
/// stopped round's rest run out at `now` ([`St::failed_for`]).
fn project(s: &S, st: &St, tail: &[String], now: u64) -> S {
    let mut p = s.clone();
    let (phase, asks) = match &st.phase {
        Phase::Pending => (0, 0),
        Phase::Announced { asks, .. } => (1, model_asks(*asks)),
        Phase::Failed(why) if why == upgrade::GAVE_UP => (2, s["asks"]),
        Phase::Done | Phase::Exiting { .. } | Phase::Relaunched { .. } => (3, s["asks"]),
        Phase::Failed(_) => (4, s["asks"]),
    };
    p.insert("phase", phase);
    p.insert("asks", asks);
    p.insert(
        "live",
        i64::from(!(st.marker.is_empty() && st.markers.is_empty())),
    );
    p.insert("ready", i64::from(answered(st, Some(&tail.join("\n")))));
    p.insert("owed", i64::from(!st.release.is_empty()));
    p.insert("rest", i64::from(st.failed_for(now) >= upgrade::RETRY_S));
    p
}

/// What the real visit did at the look a model state stands for.
#[derive(Debug)]
struct Decided {
    /// The model's actions it took, in order (none: it waited).
    actions: Vec<&'static str>,
    /// The model state it lands at.
    next: S,
    /// The step's word, as the window's host reads it.
    word: String,
    /// A release is owed after it.
    owed: bool,
}

/// The real visit's decision at the look `s` stands for, under `holds`, over the transcript
/// [`real_of`] writes (`history`: a direction answered with READY), in the driver's order and
/// through its own code: the clock hold, the READY and its clock, the step (held to
/// [`upgrade::break_step`] at a break) and its record transition, then the release where it
/// is next ([`release_next`]): dropped where [`release_void`] says, else typed under
/// [`upgrade::gate_release`] or the word that owes it.
fn decide(s: &S, holds: Holds, history: bool) -> Option<Decided> {
    if holds == Holds::Break && !at_a_break(s) {
        return None;
    }
    let (mut st, mut f, now, mut tail) = real_of(s, history);
    let sf = agent_file();
    match holds {
        Holds::Person => {
            f.approval_box = true;
            f.hold_s = upgrade::HOLD_S;
        }
        Holds::Background | Holds::Break => {
            f.background = vec!["zsh".to_string()];
            st.ready_since = now - upgrade::DRAIN_S;
        }
        Holds::InProcess if s["brk"] == 1 => {
            f.status = "busy".to_string();
            f.background = Vec::new();
            st.ready_since = now - upgrade::DRAIN_S;
        }
        Holds::InProcess => return None,
        Holds::Nothing | Holds::Refused => {}
    }
    st.phase = upgrade::clock_held(&st.phase, &f, now);
    let ready = heard(&st, &sf, TAB, Some(&tail.join("\n")));
    st.ready_since = upgrade::ready_since(st.ready_since, ready, &f, now);
    if st.ready_since != 0 {
        f.ready_s = now - st.ready_since;
    }
    st.time_failed(&mut f, now);
    let mut step = upgrade::requested_step(&Request::None, &st.phase, &f, ready, now, &st.to);
    if f.background_point {
        step = upgrade::break_step(step);
    }
    if s["fails"] == 1
        && (upgrade::asked_for_a_move(&st.phase, ready)
            || matches!(step, Step::Announce | Step::Terminate | Step::Fresh))
    {
        step = upgrade::build_hold(step, &st.phase, ready);
    }
    let reask = step == Step::Announce && st.phase != Phase::Pending;
    if holds == Holds::Refused && !(step == Step::Terminate || reask) {
        return None;
    }
    let mut agent = s.clone();
    let mut actions = Vec::new();
    let mut word = match step {
        Step::Announce if holds == Holds::Refused => {
            st.stop("argv:--bogus", now);
            actions.push("Abandon");
            "refused:--bogus".to_string()
        }
        Step::Announce => {
            let asks = match st.phase {
                Phase::Announced { asks, .. } => asks + 1,
                _ => 1,
            };
            st.announced(marker(2), now, asks);
            tail.push(user(&notice(&marker(2))));
            agent.insert("holding", 1);
            agent.insert("told", 0);
            agent.insert("directed", 0);
            actions.push(if ready { "Supersede" } else { "Announce" });
            format!("announced:{asks}")
        }
        Step::GiveUp => {
            st.give_up(now);
            actions.push(if ready { "GiveUpOutlived" } else { "GiveUp" });
            "gave-up".to_string()
        }
        Step::Terminate => {
            let back = st.signalled(AGENT_PID, 1, TAB, "claude --resume x".to_string(), now);
            if holds == Holds::Refused {
                st.signal_failed(back, now);
                actions.push("Abandon");
                "failed:signal-refused".to_string()
            } else {
                agent.insert("holding", 0);
                if agent["told"] == 1 {
                    agent.insert("overrode", 1);
                }
                agent.insert("told", 0);
                agent.insert("directed", 0);
                actions.push("Restart");
                "adopted".to_string()
            }
        }
        Step::Void(upgrade::BUILD_FAILS) => {
            st.stop(upgrade::BUILD_FAILS, now);
            actions.push("BuildHold");
            format!("failed:{}", upgrade::BUILD_FAILS)
        }
        Step::Void(why) => {
            st.void(now);
            actions.push("Void");
            format!("drain-expired:{why}")
        }
        Step::Wait(why) => format!("wait:{why}"),
        Step::Rearm => {
            let why = st.rearm(now);
            actions.push("Rearm");
            format!("rearmed:{why}")
        }
        Step::Fresh => unreachable!("a conversation with a task: {f:?}"),
    };
    if release_next(&step, &word, &st, &f, ready) {
        let text = tail.join("\n");
        if let Some(why) = release_void(&st, &sf, TAB, Some(&text)) {
            assert_eq!(why, "directed", "the model's agent is the notice's");
            st.dropped();
            if agent["holding"] == 1 {
                agent.insert("dropheld", 1);
            }
            actions.push("DropRelease");
        } else {
            let ready = heard(&st, &sf, TAB, Some(&text));
            word = match upgrade::gate_release(&f, ready) {
                upgrade::Gate::Go => {
                    let why = st.release.clone();
                    st.released();
                    agent.insert("holding", 0);
                    actions.push("Release");
                    format!("released:{why}")
                }
                upgrade::Gate::Wait(held) => {
                    let r = Report {
                        pid: AGENT_PID,
                        tab: TAB.to_string(),
                        session: SESSION.to_string(),
                        from: "1.0.0".to_string(),
                        to: "9.9.9(managed)".to_string(),
                        step: word,
                    };
                    owed_word(r, held).step
                }
            };
        }
    }
    if actions.is_empty() && !f.limited && quiet(&word) {
        agent.insert("stuck", s["holding"]);
        if s["rest"] == 1 {
            agent.insert("stalled", 1);
        }
        actions.push("Look");
    }
    let owed = !st.release.is_empty();
    let mut next = project(&agent, &st, &tail, now);
    let window = match st.phase {
        Phase::Announced { at_s, .. } => i64::from(now.saturating_sub(at_s) >= upgrade::REASK_S),
        Phase::Pending => 0,
        _ => s["window"],
    };
    next.insert("window", window);
    Some(Decided {
        actions,
        next,
        word,
        owed,
    })
}

/// The states decided at a break ([`Holds::Break`]): a gave-up round holding a late READY to
/// one of its markers, at a break of the agent's own work (`brk`).
fn at_a_break(s: &S) -> bool {
    s["brk"] == 1 && s["phase"] == 2 && s["ready"] == 1 && s["live"] == 1
}

/// The upgrade's quiet word, nothing it would do for the agent at this look (the model's
/// `Look`): its last word ([`After::Finished`]), a stopped round resting (`wait:failed`,
/// looked at again but acting on nothing until the rest runs out), or a round held by a build
/// that fails to start (`wait:build-fails`).
fn quiet(word: &str) -> bool {
    word == "wait:failed"
        || word == format!("wait:{}", upgrade::BUILD_FAILS)
        || after(word, 0) == After::Finished
}

/// The reducer's actions, the environment's aside; `Void` is a person's or
/// the agent's own work's, `Abandon` a refusal's.
const REDUCER: [&str; 8] = [
    "Announce",
    "GiveUp",
    "Restart",
    "BuildHold",
    "Release",
    "DropRelease",
    "Rearm",
    "Look",
];

fn reachable(m: &Model) -> Vec<S> {
    let mut seen = std::collections::BTreeSet::new();
    let mut queue = std::collections::VecDeque::from([m.init_state()]);
    let mut out = Vec::new();
    while let Some(s) = queue.pop_front() {
        if !seen.insert(s.clone()) {
            continue;
        }
        for a in &m.actions {
            let mut next = s.clone();
            if m.fire(a.name, &mut next) {
                queue.push_back(next);
            }
        }
        out.push(s);
    }
    out
}

/// Whether `m` admits what the real visit decided at `s` under `holds`: at an idle look, the
/// one reducer step the model's guards enable (or a wait where none is); held by a person or
/// at a break, the void exactly where the model voids; held by the agent's own work, the
/// re-ask or give-up over the READY where the model takes one, else its void; refused, the
/// round abandoned.
fn agrees(m: &Model, s: &S, holds: Holds, d: &Decided) -> bool {
    let lands = || {
        let mut t = s.clone();
        d.actions.iter().all(|a| m.fire(a, &mut t)) && t == d.next
    };
    let held = d.actions.first() == Some(&"BuildHold");
    if holds != Holds::Nothing && (held || m.action_enabled("BuildHold", s)) {
        return held && m.action_enabled("BuildHold", s) && lands();
    }
    match holds {
        Holds::Nothing => {
            let want: Vec<&str> = REDUCER
                .into_iter()
                .filter(|a| m.action_enabled(a, s))
                .collect();
            want == d.actions.first().copied().into_iter().collect::<Vec<_>>()
                && (d.actions.is_empty() || lands())
        }
        Holds::Person => {
            let voided = d.actions.first() == Some(&"Void");
            m.action_enabled("Void", s) == voided && (!voided || lands())
        }
        Holds::Background | Holds::InProcess => {
            let held = ["Supersede", "GiveUpOutlived", "Void"];
            let want = held.into_iter().find(|a| m.action_enabled(a, s));
            let took = d.actions.first().copied().filter(|a| held.contains(a));
            want == took && (want.is_none() || lands())
        }
        Holds::Break => {
            let voided = d.actions.first() == Some(&"Void");
            m.action_enabled("Void", s) == voided && (!voided || lands())
        }
        Holds::Refused => {
            d.actions.first() == Some(&"Abandon") && m.action_enabled("Abandon", s) && lands()
        }
    }
}

/// The conversation's own moves (`Direct`, `AgentReady`, `AgentGoesOn`) read by the real code:
/// the row `action` writes, appended to the transcript `s` stands for, read back as the
/// model's `ready` ([`answered`]) and as whether the release would be dropped there
/// ([`release_void`], against the model's `DropRelease` guard).
fn reads_as(m: &Model, s: &S, action: &str, history: bool) -> Option<bool> {
    let mut t = s.clone();
    if !m.fire(action, &mut t) {
        return None;
    }
    let (st, _, _, mut tail) = real_of(s, history);
    tail.push(match action {
        "Direct" => direction(),
        "AgentReady" => ready_row(),
        _ => said_by_agent("On it."),
    });
    let text = tail.join("\n");
    let drops = release_void(&st, &agent_file(), TAB, Some(&text)).is_some();
    let mut point = t.clone();
    for (var, value) in [
        ("owed", 1),
        ("ready", 0),
        ("limited", 0),
        ("phase", 2),
        ("rest", 0),
    ] {
        point.insert(var, value);
    }
    Some(
        i64::from(answered(&st, Some(&text))) == t["ready"]
            && drops == m.action_enabled("DropRelease", &point),
    )
}

/// The model with one defect switched on: the unfixed reducer (`Buggy`), and
/// each other defect the model guards against.
fn defective() -> Vec<(&'static str, Model)> {
    let model = harness_upgrade_never_strands_model();
    std::iter::once(("Buggy", aterm_spec::interp::with_buggy(&model, 1)))
        .chain(
            [
                "Terminal",
                "NoF1",
                "NoF2",
                "NoOwe",
                "NoType",
                "KeepReady",
                "LastWhileOwed",
                "StaleDirection",
                "UnansweredDirection",
                "ReadyOverDirection",
                "IdleOnlyRelease",
                "HeldAsked",
                "HeldRelease",
            ]
            .into_iter()
            .map(|knob| (knob, aterm_spec::interp::with_consts(&model, &[(knob, 1)]))),
        )
        .collect()
}

/// Tier-1: on every reachable state of `harness_upgrade_never_strands_model` the real code
/// (reducer, gates, record transitions, the driver's READY, direction and release rules, and
/// the window's reading of each word, [`after`]) takes the step the model's guards allow and
/// lands where its actions land, under each [`Holds`].
#[test]
fn the_real_upgrade_conforms_to_the_never_strands_model() {
    let model = harness_upgrade_never_strands_model();
    let mut uncaught = defective();
    let states = reachable(&model);
    assert!(states.len() > 50, "{} states", states.len());
    let mut taken = std::collections::BTreeSet::new();
    let mut histories = 0;
    let (mut breaks, mut voided_at_breaks) = (0, 0);
    for s in &states {
        for history in [false, true] {
            if history && !history_fits(s) {
                continue;
            }
            histories += usize::from(history);
            for holds in [
                Holds::Nothing,
                Holds::Person,
                Holds::Background,
                Holds::InProcess,
                Holds::Break,
                Holds::Refused,
            ] {
                let Some(d) = decide(s, holds, history) else {
                    continue;
                };
                if holds == Holds::Break {
                    breaks += 1;
                    if model.action_enabled("Void", s) {
                        voided_at_breaks += 1;
                        let old = Decided {
                            actions: Vec::new(),
                            next: s.clone(),
                            word: "wait:background".to_string(),
                            owed: s["owed"] == 1,
                        };
                        assert!(
                            !agrees(&model, s, holds, &old),
                            "the break's old wait at {s:?} reads as the model's"
                        );
                    }
                }
                assert!(
                    !(d.owed && after(&d.word, 0) == After::Finished),
                    "{holds:?} at {s:?}: `{}` is the host's last word over a release owed",
                    d.word
                );
                assert!(
                    agrees(&model, s, holds, &d),
                    "{holds:?} at {s:?} (history {history}): the real visit took {:?} (`{}`) \
                     to {:?}",
                    d.actions,
                    d.word,
                    d.next
                );
                taken.extend(d.actions.iter().copied());
                uncaught.retain(|(_, m)| agrees(m, s, holds, &d));
            }
            for action in ["Direct", "AgentReady", "AgentGoesOn"] {
                if let Some(reads) = reads_as(&model, s, action, history) {
                    assert!(reads, "{action} at {s:?} (history {history})");
                    taken.insert(action);
                }
                uncaught.retain(|(_, m)| reads_as(m, s, action, history) != Some(false));
            }
        }
        let (st, f, now, _) = real_of(s, false);
        if s["phase"] == 1 && s["window"] == 0 {
            let held = upgrade::clock_held(&st.phase, &f, now + upgrade::REASK_S);
            let runs = matches!(held, Phase::Announced { at_s, .. } if at_s == T0);
            assert_eq!(runs, model.action_enabled("Elapse", s), "Elapse at {s:?}");
            uncaught.retain(|(_, m)| runs == m.action_enabled("Elapse", s));
        }
        if (s["phase"] == 2 || s["phase"] == 4) && s["rest"] == 0 {
            let later = now + upgrade::RETRY_S;
            let runs = st.failed_for(later) >= upgrade::RETRY_S
                && st.failed_for(later - 1) < upgrade::RETRY_S;
            assert_eq!(runs, model.action_enabled("Rests", s), "Rests at {s:?}");
            let ready = heard(
                &st,
                &agent_file(),
                TAB,
                Some(&real_of(s, false).3.join("\n")),
            );
            let mut aged = f.clone();
            st.time_failed(&mut aged, later);
            let rearms = upgrade::next_step(&st.phase, &aged, ready, later) == Step::Rearm;
            let mut rested = s.clone();
            assert!(model.fire("Rests", &mut rested));
            assert_eq!(
                rearms,
                model.action_enabled("Rearm", &rested),
                "Rearm after the rest at {rested:?}"
            );
            uncaught.retain(|(_, m)| {
                runs == m.action_enabled("Rests", s) && rearms == m.action_enabled("Rearm", &rested)
            });
        }
        if s["phase"] == 1 && s["limited"] == 1 {
            let mut st = st;
            let close = T0 + 2 * upgrade::REASK_S;
            let _ = st.hold_clock(close);
            let window = match st.phase {
                Phase::Announced { at_s, .. } => {
                    i64::from((close + 60).saturating_sub(at_s) >= upgrade::REASK_S)
                }
                _ => unreachable!("an announced record stays announced"),
            };
            let mut reset = s.clone();
            assert!(model.fire("LimitResets", &mut reset));
            assert_eq!(window, reset["window"], "LimitResets at {s:?}");
            uncaught.retain(|(_, m)| {
                let mut t = s.clone();
                !(m.fire("LimitResets", &mut t) && t["window"] != window)
            });
        }
    }
    assert!(histories > 0, "the second review's history is decided");
    assert!(
        voided_at_breaks > 0 && breaks > voided_at_breaks,
        "a late READY is decided at a break, voided there and (limited) not: \
         {voided_at_breaks} of {breaks}"
    );
    assert_eq!(
        taken.len(),
        REDUCER.len() + 7,
        "every step of the upgrade, and every move of the conversation, is taken by the real \
         code: {taken:?}"
    );
    let uncaught: Vec<&str> = uncaught.iter().map(|(name, _)| *name).collect();
    assert!(
        uncaught.is_empty(),
        "the real code has these defects: {uncaught:?}"
    );
}

/// Every stop is stamped, and a re-arm is a new round ([`St::fail`], [`St::rearm`]): the
/// give-up, a stop, a refused signal and a gave-up round's void each stamp the rest's start;
/// the new round is pending with a fresh salt, its markers, stamp, saved prompt and spent
/// `--now` gone (a skip kept), the owed release carried for its first notice to supersede.
#[test]
fn a_stop_is_stamped_and_a_rearm_is_a_new_round() {
    let mut st = St {
        to: "9.9.9".to_string(),
        salt: 1_000,
        request: Request::Now,
        request_tab: TAB.to_string(),
        request_at: 900,
        ..St::default()
    };
    st.announced(marker(1), 1_000, 1);
    st.give_up(2_000);
    assert_eq!(st.failed_at, 2_000);
    assert_eq!(st.failed_for(2_000 + upgrade::RETRY_S), upgrade::RETRY_S);
    let mut stopped = St::default();
    stopped.stop("resumed-elsewhere", 3_000);
    assert_eq!(stopped.failed_at, 3_000);
    let mut refused = St::default();
    refused.announced(marker(1), 10, 1);
    let back = refused.signalled(AGENT_PID, 1, TAB, "claude --resume x".to_string(), 20);
    refused.signal_failed(back, 30);
    assert_eq!(
        (refused.phase.clone(), refused.failed_at),
        (Phase::Failed("signal-refused".to_string()), 30)
    );
    let mut voided = st.clone();
    voided.void(5_000);
    assert_eq!(voided.failed_at, 5_000);
    let mut skip = st.clone();
    skip.request = Request::Skip("9.9.9".to_string());
    st.prompt = Some(PromptMark {
        row: 3,
        col: 9,
        left: "a, b % ".to_string(),
        right: None,
    });
    let why = st.rearm(9_000);
    assert_eq!(why, upgrade::GAVE_UP);
    assert_eq!(st.phase, Phase::Pending);
    assert!(st.marker.is_empty() && st.markers.is_empty());
    assert_eq!(st.prompt, None, "the stopped round's prompt is forgotten");
    assert!(st.salt > 1_000 + u64::from(upgrade::MAX_ASKS));
    assert_eq!(st.failed_at, 0);
    assert_eq!(
        st.failed_for(u64::MAX),
        0,
        "a pending round has not stopped"
    );
    assert_eq!(st.release, "gave-up", "carried for the new notice");
    assert_eq!(st.last_stop, upgrade::GAVE_UP);
    assert_eq!(
        (st.request.clone(), st.request_tab.as_str(), st.request_at),
        (Request::None, "", 0),
        "the owner's --now was spent with the round it hurried"
    );
    let _ = skip.rearm(9_000);
    assert_eq!(skip.request, Request::Skip("9.9.9".to_string()), "kept");
    assert_eq!(upgrade::announce_asks(&st.phase, false), 1);
    st.announced(marker(2), 9_100, 1);
    assert!(st.release.is_empty() && st.last_stop.is_empty());
    assert_eq!(st.markers, vec![marker(2)]);
    let mut again = St::default();
    again.stop("no-resume", 7_000);
    let text = again.to_json();
    assert!(text.contains(r#""failed_at":7000"#), "{text}");
    assert_eq!(St::from_json(&text).expect("reads").failed_at, 7_000);
    let older = text.replace(r#","failed_at":7000"#, "");
    assert_ne!(older, text);
    let old = St::from_json(&older).expect("an older build's state reads");
    assert_eq!(old.failed_at, 0);
    assert_eq!(old.failed_for(T0), T0, "stopped long ago");
    assert!(upgrade::retry_due(&old.phase, old.failed_for(T0), false));
}

/// A void begins the rest again by its stop's ladder: four declined rounds rest a day
/// ([`upgrade::unanswered_rest_extension`]), and a late READY voided in that rest begins a
/// day's rest again at the void, never the `4 × RETRY_S` cap of a stop repeated for another
/// reason.
#[test]
fn a_void_begins_the_rest_again_by_the_ladder_of_its_stop() {
    const T: u64 = 1_790_000_000;
    let mut declined = St::default();
    for _ in 0..4 {
        declined.give_up(T);
    }
    let earned = T + 11 * upgrade::RETRY_S;
    assert_eq!(
        declined.failed_at, earned,
        "four rounds declined rest a day"
    );
    let mut early = declined.clone();
    early.void(T + 1_800);
    assert_eq!(early.failed_at, T + 1_800 + 11 * upgrade::RETRY_S);
    let mut late = declined.clone();
    let late_at = T + 18 * 3_600;
    late.void(late_at);
    assert_eq!(late.failed_at, late_at + 11 * upgrade::RETRY_S);
    assert!(late.failed_at >= earned, "a void never shortens the rest");
    let mut refused = St::default();
    for _ in 0..4 {
        refused.stop("signal-refused", T);
    }
    let cap = T + 3 * upgrade::RETRY_S;
    assert_eq!(refused.failed_at, cap);
    refused.void(T + 1_800);
    assert_eq!(refused.failed_at, T + 1_800 + 3 * upgrade::RETRY_S);
}

/// A gave-up round whose late READY its own shell outlives: voided, the agent released, the
/// round resting (`wait:failed`, looked at again).
#[cfg(unix)]
#[test]
fn a_stopped_round_rests_then_a_new_one_asks_again_naming_what_runs() {
    let now = now_s();
    let mut st = St {
        from: "1.0.0".to_string(),
        to: "9.9.9".to_string(),
        source: "managed".to_string(),
        ..St::default()
    };
    for (asks, (at, marker)) in (1u32..).zip(LEDGER) {
        st.announced(marker.to_string(), at, asks);
    }
    let old_salt = st.salt;
    st.give_up(now - 60);
    let mut delivered = vec![user("Tidy the parser module.")];
    delivered.extend(LEDGER.iter().map(|(_, m)| user(&notice(m))));
    delivered.push(said_by_agent(&format!("Stopping.\n{}", LEDGER[3].1)));
    let run = |opts: &Opts, agent: &Agent| {
        let shell = dead_pid();
        let table = vec![
            (shell, 1, "zsh".into()),
            (agent.sf.pid + 100_000, agent.sf.pid, "zsh".into()),
        ];
        let args = atpkg::caller_shell::process_args(agent.sf.pid).expect("argv");
        let files = session_files(&opts.home);
        visit_with_claim(
            opts,
            &agent.sf,
            files.as_deref(),
            &table,
            &newer(),
            &Script::new(shell, usize::MAX, None),
            Some(&args),
            None,
        )
    };
    let dir = scratch("owners-stall");
    let (sock, asked) = instance(&dir);
    let opts = Opts {
        sock: Some(sock),
        ..drive(&dir)
    };
    write_transcript(&opts, &delivered);
    let heard = St {
        ready_since: now - upgrade::DRAIN_S,
        ..st.clone()
    };
    let agent = Agent::start(&opts, &heard);
    assert_eq!(run(&opts, &agent).step, "released:gave-up");
    assert_eq!(typed(&asked, "Upgrade off:"), 1);
    let resting = run(&opts, &agent);
    assert_eq!(resting.step, "wait:failed");
    assert!(matches!(after(&resting.step, 9), After::Later(_)));
    assert_eq!(turns(&asked), 1);
    let mut rested = load(&opts, SESSION).expect("state");
    assert!(rested.failed_at >= now, "the void began the rest again");
    rested.failed_at = now_s() - upgrade::RETRY_S;
    save(&opts, SESSION, &rested);
    let rearmed = run(&opts, &agent);
    assert_eq!(rearmed.step, "rearmed:unanswered");
    assert_eq!(turns(&asked), 1, "a new round types nothing itself");
    let fresh = load(&opts, SESSION).expect("state");
    assert_eq!(fresh.phase, Phase::Pending);
    assert!(fresh.salt > old_salt + u64::from(upgrade::MAX_ASKS));
    let ledger = std::fs::read_to_string(state_dir(&opts).join("ledger.jsonl")).expect("ledger");
    assert!(
        ledger.contains(r#""step":"rearmed:unanswered""#),
        "{ledger}"
    );
    assert!(matches!(after(&rearmed.step, 0), After::Later(_)));
    let notice = run(&opts, &agent);
    assert_eq!(notice.step, "announced:1");
    let typed_notice = asked
        .lock()
        .expect("log")
        .iter()
        .rfind(|l| l.contains(" turn "))
        .cloned()
        .expect("a notice");
    assert!(
        typed_notice.contains(&format!("pid {} (zsh", agent.sf.pid + 100_000)),
        "{typed_notice}"
    );
    let marker = load(&opts, SESSION).expect("state").marker;
    assert!(marker.starts_with(upgrade::READY_PREFIX), "{marker}");
    assert!(
        LEDGER.iter().all(|(_, m)| *m != marker),
        "a marker of its own"
    );
    drop(agent);
    let _ = std::fs::remove_dir_all(&dir);

    let mut stale = st.clone();
    stale.forget_markers();
    stale.release.clear();
    stale.failed_at = 0;
    let older = stale.to_json().replace(r#","failed_at":0"#, "");
    let stale = St::from_json(&older).expect("an older build's state");
    for (name, screen, request, words) in [
        (
            "brk-rearm",
            None,
            Request::None,
            ["rearmed:unanswered", "announced:1"],
        ),
        (
            "brk-limited",
            Some(limited_screen()),
            Request::None,
            ["rearmed:unanswered", "wait:limited"],
        ),
        (
            "brk-skip",
            None,
            Request::Skip("9.9.9".to_string()),
            ["wait:skipped", "wait:skipped"],
        ),
    ] {
        let dir = scratch(name);
        let (sock, asked) = instance_with(
            &dir,
            Answers {
                screen: screen.unwrap_or_else(idle_screen),
                ..Answers::default()
            },
        );
        let opts = Opts {
            sock: Some(sock),
            background: true,
            ..drive(&dir)
        };
        write_transcript(&opts, &delivered);
        let record = St {
            request: request.clone(),
            request_tab: if request == Request::None {
                String::new()
            } else {
                TAB.to_string()
            },
            ..stale.clone()
        };
        let agent = Agent::start(&opts, &record);
        for word in words {
            assert_eq!(run(&opts, &agent).step, word, "{name}");
        }
        let kept = load(&opts, SESSION).expect("state");
        match name {
            "brk-rearm" => {
                assert_eq!(turns(&asked), 1, "{name}: the notice, at the break");
                assert!(matches!(kept.phase, Phase::Announced { asks: 1, .. }));
            }
            "brk-limited" => {
                assert_eq!(turns(&asked), 0, "{name}: never asked at the limit");
                assert_eq!(kept.phase, Phase::Pending);
            }
            _ => {
                assert_eq!(turns(&asked), 0, "{name}: the skip holds it");
                assert_eq!(kept.phase, Phase::Failed(upgrade::GAVE_UP.to_string()));
            }
        }
        drop(agent);
        let _ = std::fs::remove_dir_all(&dir);
    }
}
