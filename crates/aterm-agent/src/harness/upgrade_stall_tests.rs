// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

//! THE STALL OF 2026-09-25/26 (tab `s-d3346b29dd236432b852`, Claude session
//! `37dffac7-361e-46be-90c2-31589cb40b5c`), replayed through the real driver:
//! the weekly limit hit at 20:50 on the 24th and Claude Code 2.1.280 parked
//! the session at `⚠ Usage limit reached · continuing automatically at 6am`;
//! the upgrade typed its notice into that parked session at 16:02, 16:32,
//! 17:02 and 17:32 on the 25th (the ledger's `announced:1..4`, a new marker
//! each time), gave up at 18:02 (`Failed("unanswered")`, waited on as
//! `failed` for ever), and at 06:00 on the 26th Claude Code delivered all four
//! notices at once. The agent stopped the work it had just relaunched and
//! answered the LAST marker at 06:02; nothing acted, and nothing told it to go
//! on. The three fixes — no notice into a limited session (F1), a late READY
//! honoured (F2), one release line for every notice abandoned (F3) — what
//! the review of 2026-09-26 found left in them (a stop after READY that kept
//! its markers, a release word that owned nothing, a stop's own word taken
//! as the last, a release typed to another process or over newer direction,
//! a late READY with no drain, a clock the window's host never held, a limit
//! held by the upgrade's ownership), and the Tier-1 bind of
//! `harness_upgrade_never_strands_model` to the real code.

use super::*;
use aterm_spec::derive::{Model, harness_upgrade_never_strands_model};
use std::collections::BTreeMap;

/// The incident's conversation, target and salt (its state file, read
/// 2026-09-26: `"salt":1790372998`, `"to":"2.1.283"`).
const INCIDENT_SESSION: &str = "37dffac7-361e-46be-90c2-31589cb40b5c";
const INCIDENT_SALT: u64 = 1_790_372_998;

/// The ledger's four announcements (`t`, marker), 2026-09-25 PDT.
const LEDGER: [(u64, &str); 4] = [
    (1_790_377_339, "ATERM-UPGRADE-READY-5182b3fa"),
    (1_790_379_148, "ATERM-UPGRADE-READY-a39848f9"),
    (1_790_380_954, "ATERM-UPGRADE-READY-e5bc1bec"),
    (1_790_382_760, "ATERM-UPGRADE-READY-8cd7f7eb"),
];

/// `gave-up`, 18:02:42 PDT; the READY answer, 06:02:01 PDT the next morning
/// (the supervisor's `EVENT idle seq=54190 ATERM-UPGRADE-READY-8cd7f7eb`).
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

/// The notice the incident's session was typed, with `marker`.
fn notice(marker: &str) -> String {
    upgrade::prepare_prompt(
        &version("2.1.280"),
        &version("2.1.283"),
        Source::Managed,
        marker,
    )
}

/// F2 over the incident's own record: the markers the ledger names are the
/// ones this build mints from the state's salt; the record the pre-fix build
/// left (four notices, gave up) hears the READY the agent gave at 06:02 — to
/// the LAST marker, and to any other of the four — and takes the restart;
/// the release it owes waits behind that READY. F1 over the same moments: the
/// notices the incident typed are never typed now, and no give-up comes.
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
    // F1: at each moment a notice was typed the session stood at its limit;
    // now each is a wait, and the give-up is one too.
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
    // NEGATIVE CONTROL: read without the limit, the same looks are the
    // incident's — a notice, and the give-up.
    assert_eq!(
        upgrade::next_step(&Phase::Pending, &idle(false), false, LEDGER[0].0),
        Step::Announce
    );
    assert_eq!(
        upgrade::next_step(&four, &idle(false), false, GAVE_UP_AT),
        Step::GiveUp
    );

    // F2: the record the pre-fix build left, replayed through this build's
    // own transitions: four notices, then the give-up.
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
    // 06:00: the four notices delivered at once, and the agent's answer.
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
    // A READY given BEFORE a later notice is no answer to it: the rule that the
    // latest notice ends any READY before it still holds.
    let early = [
        user(&notice(LEDGER[0].1)),
        said_by_agent(LEDGER[0].1),
        user(&notice(LEDGER[1].1)),
    ]
    .join("\n");
    assert!(!answered(&st, Some(&early)));
    // Once the release is typed the round's markers are forgotten: the agent
    // was told the upgrade is off for now, and none of this round's does.
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
    // A relaunch's record, and a Codex one, typed no Claude notice: nothing owed.
    let mut relaunch = announced.clone();
    relaunch.cause = crate::harness::relaunch::CAUSE_EXIT.to_string();
    relaunch.stop("no-resume", 10);
    assert!(relaunch.release.is_empty());
    let mut codex = announced.clone();
    codex.agent = upgrade::Agent::Codex;
    codex.give_up(10);
    assert!(codex.release.is_empty());
    // Once per abandonment: the first reason kept.
    let mut twice = announced.clone();
    twice.give_up(10);
    twice.stop("resumed-elsewhere", 10);
    assert_eq!(twice.release, "gave-up");
    // A new notice supersedes a release still owed.
    twice.announced("ATERM-UPGRADE-READY-feedface".to_string(), 20, 1);
    assert!(twice.release.is_empty());
    // A notice for another build abandons this one's: owed on the new state.
    let target = Candidate {
        exe: PathBuf::from("/x/claude"),
        version: version("9.9.9"),
        source: Source::Managed,
    };
    let retargeted = St::for_target(
        Some(St {
            to: "2.1.283".to_string(),
            ..announced.clone()
        }),
        &version("2.1.280"),
        &target,
        None,
        30,
    );
    assert_eq!(retargeted.phase, Phase::Pending);
    assert_eq!(retargeted.release, "retargeted");
    assert!(retargeted.marker.is_empty() && retargeted.markers.is_empty());
}

/// THE OWNER'S `--now` IS SPENT WITH THE ROUND IT ASKED TO MOVE (the second
/// review of 2026-09-26): an upgrade `--now` re-armed that gives up asking
/// clears the word, so a late READY hours later restarts under the ordinary
/// waits — a person at the tab holds the signal. NEGATIVE CONTROLS: the same
/// late READY under a `--now` still in force is signalled over the person
/// (what a kept word did), and the owner's skip — a word on a build, not on a
/// round — is kept.
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

// ---------------------------------------------------------------- the real visit

/// A limited Claude Code 2.1.280 tab as `text --json` sends it: the
/// incident's banner over an empty composer, the caret at column 2.
#[cfg(unix)]
fn limited_screen() -> String {
    let rule = "─".repeat(40);
    format!(
        r#"{{"rows":["⏺ Both workflows are relaunched.","","{AUTO_CONTINUE}","","{rule}","❯ ","{rule}","  ⏵⏵ auto mode on (shift+tab to cycle)"],"cursor":{{"row":5,"col":2}},"seq":77,"human_ms":null,"first":0}}"#
    )
}

/// A tab with a PERSON's half-typed draft in the composer.
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
    child: std::process::Child,
    sf: SessionFile,
}

#[cfg(unix)]
impl Agent {
    fn start(opts: &Opts, st: &St) -> Agent {
        // Its argv a launch the relaunch can carry (a positional, no flag the
        // rewrite does not know), so a plan is made as for a real agent.
        let child = Command::new(std::env::current_exe().expect("exe"))
            .arg(PARK[0])
            .env(PARK_ENV, "1")
            .env("ATERM_PARENT_SESSION_ID", TAB)
            .stdin(std::process::Stdio::null())
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
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
        let table = vec![(shell, 1, "zsh".to_string())];
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

/// The requests of `asked` that TYPED a turn saying `words`.
#[cfg(unix)]
fn typed(asked: &std::sync::Mutex<Vec<String>>, words: &str) -> usize {
    asked.lock().map_or(0, |a| {
        a.iter()
            .filter(|l| l.contains(" turn ") && l.contains(words))
            .count()
    })
}

/// F1 THROUGH THE REAL VISIT: the incident's screen — the banner over an
/// empty composer, a session file that says `idle` — types nothing: a pending
/// upgrade waits `limited`, and so does one that has asked to its bound long
/// ago, whose clock the look restarts instead of giving up. NEGATIVE CONTROL:
/// the same tab off its limit is announced to.
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

    // NEGATIVE CONTROL: off the limit, the pending upgrade announces.
    let open = scratch("open");
    let (sock, asked) = instance(&open);
    let opts = Opts {
        sock: Some(sock),
        ..drive(&open)
    };
    let agent = Agent::start(&opts, &pending);
    assert_eq!(agent.visit(&opts, shell).step, "announced:1");
    assert_eq!(turns(&asked), 1);
    drop(agent);
    let _ = std::fs::remove_dir_all(&dir);
    let _ = std::fs::remove_dir_all(&open);
}

/// F3 THROUGH THE REAL VISIT: a READY answer a person's draft held past the
/// drain's bound is void — the agent stopped for a restart that is not coming
/// is owed its release, which waits while the draft stands. The person clears
/// it: ONE release line is typed, ledgered, and never again; the re-ask waits
/// its window from the void.
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
    // A session at work when the notice came: a conversation holding only
    // the harness's own turns has no task, and is restarted afresh instead
    // of asked (D1 of the live E2E of 2026-09-26).
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

    // The person clears the draft (the same instance, now idle).
    let cleared = scratch("void-free");
    let (sock, asked) = instance(&cleared);
    let free = Opts {
        sock: Some(sock),
        ..opts.clone()
    };
    let release = upgrade::release_prompt(upgrade::Agent::Claude);
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

/// F2 AND F3 THROUGH THE REAL VISIT, on the record the incident left: gave
/// up, four markers, the notice's fence on the agent. Its READY to the LAST
/// marker reaches the restart (a dry run says so and ends nothing); with no
/// READY, the release is typed once and the markers forgotten; at the limit,
/// the release waits (`wait:release:limited`) and nothing is typed.
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
    // The real restart, to its last look before the signal (the agent
    // suspended there): every fence of an announced restart is asked, and a
    // restart that does not send its signal puts the owed release back.
    let r = agent.visit_with(&opts, shell, 3);
    assert_eq!(
        r.step, "wait:changed-before-signal",
        "the restart was reached"
    );
    let kept = load(&opts, SESSION).expect("state");
    assert_eq!(kept.phase, Phase::Failed(upgrade::GAVE_UP.to_string()));
    assert_eq!(kept.release, "gave-up", "still owed: nothing was restarted");
    assert_eq!(turns(&asked), 0);

    // No READY: the agent went on (or never answered). Released, once.
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

    // At the limit the release waits, and says so.
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

/// A STALE STATUS NEVER REACHES THE SIGNAL, AND NEVER STRANDS THE READY
/// AGENT UNDER IT (the review of 2026-09-27). Claude Code left its status at
/// `busy` over an idle screen with nothing running under the agent. At its
/// second idle look the status lags a turn that is over, and one line may
/// go there — but THE RESTART WAITS FOR CLAUDE'S OWN `idle` (the Drain
/// step): work inside the agent's own process is no process under it, and
/// the status is the one word that says it runs. So the READY's restart
/// waits `status-stale` — no last look, no signal — and once that READY has
/// stood a whole [`upgrade::REASK_S`] the agent is asked again
/// (`announced:2`), never left stopped. Before the fix the restart took the
/// lagging status for idle and went on to its last look before the signal
/// (the agent, suspended after that look, stopped it:
/// `wait:changed-before-signal`). NEGATIVE CONTROL: the same agent with
/// Claude's own `idle` is restarted — its restart reaches that last look.
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
    // NEGATIVE CONTROL: Claude's own `idle`.
    assert_eq!(
        run("idle-restart", "idle", 60, 1),
        (words(&["wait:changed-before-signal"]), 0, false),
        "an idle agent's restart reaches its last look"
    );
}

// ------------------------------------------- the review of 2026-09-26

/// A RESTART THAT STOPS AFTER READY RELEASES THE AGENT IT ASKED, through the
/// real visit. The restart's own record transitions — written before the
/// signal ([`St::signalled`]), then the kernel's refusal
/// ([`St::signal_failed`], `restart`'s `Terminated::Failed`) — stop the
/// upgrade, owe the release and FORGET the round's markers: no stopped phase
/// acts on a READY. The next visit types the release once, and the one after
/// is the upgrade's last word with nothing owed. NEGATIVE CONTROL, the review's
/// probe: the record a build before this one left — stopped, markers kept,
/// READY still the agent's last word — read `wait:release:ready` on every
/// visit, the agent neither restarted nor released; it is released at once now,
/// because a READY no phase acts on holds nothing.
#[cfg(unix)]
#[test]
fn a_restart_refused_after_ready_releases_the_agent_it_asked() {
    let marker = "ATERM-UPGRADE-READY-0badf00d";
    let base = St {
        from: "1.0.0".to_string(),
        to: "9.9.9".to_string(),
        source: "managed".to_string(),
        ..St::default()
    };
    let mut st = base.clone();
    st.announced(marker.to_string(), now_s() - 60, 1);
    let back = st.signalled(4242, 1, TAB, "claude --resume x".to_string(), now_s());
    assert!(matches!(st.phase, Phase::Exiting { .. }), "{:?}", st.phase);
    assert!(
        st.markers.is_empty(),
        "the restart consumes the READY it acts on"
    );
    st.signal_failed(back, now_s());
    assert_eq!(st.phase, Phase::Failed("signal-refused".to_string()));
    assert_eq!(st.release, "signal-refused");
    assert!(st.marker.is_empty() && st.markers.is_empty());
    // Stopped at this look (a stop older than the stamp is re-armed at
    // once: `a_stopped_round_rests_then_a_new_one_asks_again_naming_what_runs`).
    let old = St {
        phase: Phase::Failed("signal-refused".to_string()),
        marker: marker.to_string(),
        markers: vec![marker.to_string()],
        release: "signal-refused".to_string(),
        failed_at: now_s(),
        ..base
    };
    for (name, record) in [("refused-after-ready", st), ("refused-kept-markers", old)] {
        let dir = scratch(name);
        let (sock, asked) = instance(&dir);
        let opts = Opts {
            sock: Some(sock),
            ..drive(&dir)
        };
        write_transcript(
            &opts,
            &[
                user(&notice(marker)),
                said_by_agent(&format!("Saved.\n{marker}")),
            ],
        );
        let shell = dead_pid();
        let agent = Agent::start(&opts, &record);
        let first = agent.visit(&opts, shell);
        assert_eq!(first.step, "released:signal-refused", "{name}");
        assert_eq!(typed(&asked, "Upgrade off:"), 1, "{name}");
        let kept = load(&opts, SESSION).expect("state");
        assert!(kept.release.is_empty() && kept.markers.is_empty(), "{name}");
        let second = agent.visit(&opts, shell);
        assert_eq!(second.step, "wait:failed", "{name}");
        // Resting, not finished: looked at again until its next round.
        assert!(matches!(after(&second.step, 0), After::Later(_)), "{name}");
        assert_eq!(turns(&asked), 1, "{name}: typed once, and nothing else");
        drop(agent);
        let _ = std::fs::remove_dir_all(&dir);
    }
}

/// A LATE READY WHOSE RESTART'S GATE WAITS KEEPS THE GATE'S WORD, and the
/// window's host owns the session's turn ends while it settles. The release
/// a gave-up upgrade owes waits behind the READY — and it is not the step's
/// word: the restart's gate is (the review of 2026-09-26: every such wait
/// read `wait:release:ready`, which owns nothing, so the supervisor could
/// type a continuation over the READY turn while its restart settled, and a
/// person's draft under it was looked at on the growing pause, not every
/// [`HOLD_LOOK`]). Nothing is typed either way.
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
        // The screen just moved: its quiet starts now.
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

/// WHO HAS DIRECTED THE CONVERSATION SINCE THE AGENT'S LAST ANSWER
/// ([`upgrade::directed_since_ready`]), over the user rows the owner's
/// transcripts hold (measured 2026-09-26): a person's words, a `!` command,
/// an Esc, a peer's message and a supervisor's continuation are directions;
/// the harness's own lines, tool results, `isMeta` rows (the limit's reset
/// the incident's agent answered READY after), compaction summaries, task
/// notifications and slash commands with their output are not. A direction
/// counts once the agent has answered it with a row of its own, and only
/// after the LATEST notice and the agent's latest READY to a marker of the
/// round. NEGATIVE CONTROL (the second review of 2026-09-26): a direction the
/// agent answered with READY — a peer's message, then the READY — counts for
/// nothing; the release-era reading counted it.
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
        user(&upgrade::release_prompt(upgrade::Agent::Claude)),
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
        // Not answered yet: the agent may still answer it with READY.
        assert!(!directed(&rows), "unanswered: {direction}");
        rows.push(on_it.clone());
        assert!(directed(&rows), "{direction}");
        // Answered by a row the agent did not write: still unanswered.
        let mut met = quiet.to_vec();
        met.push(direction.clone());
        met.push(said_by_agent("x").replace("claude-opus-5-5", "<synthetic>"));
        assert!(!directed(&met), "<synthetic>: {direction}");
        // Before the latest notice, it directed nothing since.
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
        // THE NEGATIVE CONTROL: the agent answered it with READY — it holds
        // for the restart, and only a release can tell it to go on.
        let then_ready = [direction.clone(), on_it.clone(), ready.clone()];
        assert!(!directed(&then_ready), "then READY: {direction}");
        assert!(
            upgrade::directed_since_ready(&with(&then_ready), &[]),
            "the READY is a marker of the round: {direction}"
        );
        // And after the READY, a direction taken up counts again.
        let mut past = then_ready.to_vec();
        past.push(direction.clone());
        past.push(on_it.clone());
        assert!(directed(&past), "after READY: {direction}");
    }
    // A subagent's own prompts are no direction of the conversation.
    let side = user("do the thing").replace(r#""isSidechain":false"#, r#""isSidechain":true"#);
    assert!(!directed(&[side, on_it]));
}

/// A RELEASE IS TYPED ONLY TO THE AGENT THE NOTICE REACHED. A restart past
/// its signal owes none (its agent was ended; the conversation is the
/// carry-on's, or the person's who resumed it). A retargeted or stopped
/// record carries the notice's fence with the release it owes.
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
        let st = St::for_target(Some(prior.clone()), &version("2.1.280"), &target, None, 30);
        assert!(!st.release.is_empty(), "{:?}", prior.phase);
        assert_eq!(
            (st.tab.as_str(), st.notice_pid, st.notice_start.as_str()),
            (TAB, 4242, "Fri Sep 25 16:00:00 2026"),
            "{:?}: the notice's fence goes with the release",
            prior.phase
        );
    }
}

/// THROUGH THE REAL VISIT, a release owed is DROPPED — said once in the
/// ledger, nothing typed — where it is no longer this upgrade's: the process
/// visited is not the one the notice reached (resumed by hand: on the old
/// build, the visit's own step; on the new one, `release_visit`), or someone
/// has directed the conversation since the notice (a person's words after
/// the notice). NEGATIVE CONTROL: the rows Claude Code writes on its own
/// after the notice (the limit's reset, a tool's result, a task
/// notification) direct nothing, and the release is typed.
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
    // (name, transcript, the notice's process is another, targets, word)
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
        let table = vec![(shell, 1, "zsh".to_string())];
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

/// A DROPPED RELEASE LEAVES NO OWED WAIT BEHIND (the review of the upgrade's
/// leftovers, 2026-09-28). A pending round owing the line (`retargeted`),
/// whose last look recorded the release's own gate (`wait=release:attended`),
/// is visited with no build to move to while the notice's process no longer
/// holds the conversation: the release is no longer the upgrade's to type and
/// is dropped (`release-dropped:other-process`). Its gate's word went with it
/// only in the ledger — the record kept `wait=release:attended`, `--status`
/// read `wait=release:attended … release=-` on one line, and the owner was
/// told the agent "is owed the line telling it to carry on", which nothing
/// owed any more, visit after visit. Now the drop is the visit's step and
/// clears the wait: the row reads what main read, how long it is behind.
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
    let table = vec![(shell, 1, "zsh".to_string())];
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
    let (rows, vetted) = upgrade_status::status_rows(&opts);
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
    assert_eq!(
        rows[0].stall_words(at).as_deref(),
        Some("behind for 7 h"),
        "{line}"
    );
}

/// A LATE READY THE RESTART'S GATE HOLDS PAST THE DRAIN IS VOIDED, through
/// the real visit, as an announced one is. The incident's gave-up record
/// hears the agent's READY; a person's draft that has stood [`upgrade::HOLD_S`]
/// voids it (`drain-expired:draft`) — nothing typed over the draft, the
/// release still owed, the round's markers forgotten, so the stale answer can
/// never end the agent when the draft goes (the review of 2026-09-26: the
/// gave-up arm honoured a READY of any age). Background work of the agent's
/// own, still running [`upgrade::DRAIN_S`] after the answer, voids it too, and
/// the release is typed in the same visit (`released:gave-up`, the void
/// ledgered). NEGATIVE CONTROL: the same work a minute after the answer is
/// only waited for.
#[cfg(unix)]
#[test]
fn a_late_ready_held_past_the_drain_is_voided_and_its_agent_released() {
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
    let now = now_s();

    // A person's draft, standing past HOLD_S.
    let dir = scratch("late-void-draft");
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
    write_transcript(&opts, &delivered);
    let held = St {
        hold_since_s: now - upgrade::HOLD_S - 30,
        hold_seen_s: now - 30,
        ..st.clone()
    };
    let agent = Agent::start(&opts, &held);
    assert_eq!(agent.visit(&opts, dead_pid()).step, "drain-expired:draft");
    assert_eq!(turns(&asked), 0, "nothing typed over the person's draft");
    let kept = load(&opts, SESSION).expect("state");
    assert_eq!(kept.phase, Phase::Failed(upgrade::GAVE_UP.to_string()));
    assert_eq!(kept.release, "gave-up", "still owed");
    assert!(kept.markers.is_empty(), "the stale READY can never end it");
    assert!(
        kept.failed_at >= now,
        "its rest begins again at the void: {}",
        kept.failed_at
    );
    let ledger = std::fs::read_to_string(state_dir(&opts).join("ledger.jsonl")).expect("ledger");
    assert!(
        ledger.contains("a draft nobody sent had held the restart")
            && ledger.contains("a new round asks it again in 2h"),
        "{ledger}"
    );
    drop(agent);
    let _ = std::fs::remove_dir_all(&dir);

    // The agent's own work under it: DRAIN_S after the answer, and a minute.
    for (name, heard_s, word) in [
        ("late-void-bg", upgrade::DRAIN_S, "released:gave-up"),
        ("late-wait-bg", 60, "wait:background"),
    ] {
        let dir = scratch(name);
        let (sock, asked) = instance(&dir);
        let opts = Opts {
            sock: Some(sock),
            ..drive(&dir)
        };
        write_transcript(&opts, &delivered);
        let heard = St {
            ready_since: now - heard_s,
            ..st.clone()
        };
        let agent = Agent::start(&opts, &heard);
        let shell = dead_pid();
        // A shell the agent left running, under it.
        let table = vec![
            (shell, 1, "zsh".to_string()),
            (agent.sf.pid + 100_000, agent.sf.pid, "zsh".to_string()),
        ];
        let args = atpkg::caller_shell::process_args(agent.sf.pid).expect("argv");
        let files = session_files(&opts.home);
        let r = visit_with_claim(
            &opts,
            &agent.sf,
            files.as_deref(),
            &table,
            &newer(),
            &Script::new(shell, usize::MAX, None),
            Some(&args),
            None,
        );
        assert_eq!(r.step, word, "{name}");
        let kept = load(&opts, SESSION).expect("state");
        let ledger =
            std::fs::read_to_string(state_dir(&opts).join("ledger.jsonl")).unwrap_or_default();
        if word == "released:gave-up" {
            assert_eq!(typed(&asked, "Upgrade off:"), 1, "{name}");
            assert!(kept.release.is_empty() && kept.markers.is_empty());
            assert!(
                ledger.contains(r#""step":"drain-expired:background""#)
                    && ledger.contains("work of the agent's own still running"),
                "{ledger}"
            );
        } else {
            assert_eq!(turns(&asked), 0, "{name}");
            assert_eq!(kept.markers.len(), 4, "{name}: the READY still heard");
            assert_eq!(kept.ready_since, now - heard_s, "{name}: its clock kept");
        }
        drop(agent);
        let _ = std::fs::remove_dir_all(&dir);
    }
}

/// A DIRECTION BEFORE THE READY NEVER DROPS THE RELEASE ITS VOID OWES (the
/// second review of 2026-09-26), through the real visit: the incident's
/// gave-up record; after the notices a person's message, the agent's answer
/// to it, then its READY; the agent's own work under it DRAIN_S after the
/// answer voids that READY — and the release is TYPED (`released:gave-up`).
/// Read as a direction since the notice, the message dropped it
/// (`release-dropped:directed`): no restart, no release, and nothing else
/// asks again in the gave-up phase — the incident's end state. NEGATIVE
/// CONTROL: the same message AFTER the READY, taken up by the agent, ends the
/// READY (nothing to void or restart) and drops the release — it is typed
/// over nothing the agent is doing now.
#[cfg(unix)]
#[test]
fn a_direction_before_the_ready_never_drops_the_release_its_void_owes() {
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
    let notices: Vec<String> = LEDGER.iter().map(|(_, m)| user(&notice(m))).collect();
    let message = user("[from s-d3346b29] v0.91.0 is out; the parser branch can wait");
    let answer = said_by_agent("Noted; the parser branch waits.");
    let ready = said_by_agent(&format!("Stopping.\n{}", LEDGER[3].1));
    let before = [
        notices.clone(),
        vec![message.clone(), answer.clone(), ready.clone()],
    ]
    .concat();
    let after = [notices, vec![ready, message, answer]].concat();
    let now = now_s();
    for (name, rows, word, dropped) in [
        ("dir-before-ready", &before, "released:gave-up", false),
        ("dir-after-ready", &after, "wait:failed", true),
    ] {
        let dir = scratch(name);
        let (sock, asked) = instance(&dir);
        let opts = Opts {
            sock: Some(sock),
            ..drive(&dir)
        };
        write_transcript(&opts, rows);
        let heard = St {
            ready_since: now - upgrade::DRAIN_S,
            ..st.clone()
        };
        let agent = Agent::start(&opts, &heard);
        let shell = dead_pid();
        let table = vec![
            (shell, 1, "zsh".to_string()),
            (agent.sf.pid + 100_000, agent.sf.pid, "zsh".to_string()),
        ];
        let args = atpkg::caller_shell::process_args(agent.sf.pid).expect("argv");
        let files = session_files(&opts.home);
        let r = visit_with_claim(
            &opts,
            &agent.sf,
            files.as_deref(),
            &table,
            &newer(),
            &Script::new(shell, usize::MAX, None),
            Some(&args),
            None,
        );
        assert_eq!(r.step, word, "{name}");
        let kept = load(&opts, SESSION).expect("state");
        assert!(kept.release.is_empty() && kept.markers.is_empty(), "{name}");
        let ledger =
            std::fs::read_to_string(state_dir(&opts).join("ledger.jsonl")).unwrap_or_default();
        assert_eq!(
            ledger.contains("release-dropped:directed"),
            dropped,
            "{name}: {ledger}"
        );
        assert_eq!(
            typed(&asked, "Upgrade off:"),
            usize::from(!dropped),
            "{name}"
        );
        if !dropped {
            // The ledger says what the line said ([`upgrade::release_prompt`]).
            assert!(
                ledger.contains(r#""step":"drain-expired:background""#)
                    && ledger.contains(
                        "nothing will restart the session without asking it again first, \
                         nothing the notice asked of it still applies, and to carry on as it \
                         would have without it"
                    ),
                "{name}: {ledger}"
            );
        }
        drop(agent);
        let _ = std::fs::remove_dir_all(&dir);
    }
}

/// F1 FROM THE WINDOW: THE LOOP'S LIMIT EPISODE HOLDS THE UPGRADE'S CLOCKS
/// ([`hold_clock`], what the window's host calls as its loop's episode opens
/// and closes). The host takes no step during an episode, so the upgrade's
/// own look never found the limit and never held its clock (the review of
/// 2026-09-26): a notice whose wind-down turn hit the weekly limit met its
/// first idle point after the reset hours past its window, and — asked to
/// its bound — was given up on and released at once. NEGATIVE CONTROL: that
/// record, unheld, gives up at that point; held, it waits its window. The
/// READY clock is held too; another tab's record and a pending one are not
/// touched, no clock is moved back, and a lock another sweep holds applies
/// nothing (`false`: the host tries again before its next step).
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

// ---------------------------------------------------------------- Tier-1

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
    /// The same at a break, the work INSIDE the agent's own process (a
    /// workflow it waits on — Claude's status `busy`, nothing under it; the
    /// review of 2026-09-27): judged as [`Holds::Background`]. Only at a
    /// break (`brk`): off one, `busy` over nothing is a turn still running.
    InProcess,
    /// The same work, under the same READY, looked at from A BREAK of it
    /// (`brk`, [`upgrade::Facts::background_point`]) — where the owner's tab
    /// of 2026-09-27 took nearly every step — and the step held to what a
    /// break may do ([`upgrade::break_step`]): nothing is ended; the release
    /// goes as the notice's point would. Decided for a gave-up round's late
    /// READY only ([`at_a_break`]), and judged stricter than
    /// [`Holds::Background`]: the void exactly where the model voids.
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

/// Whether the transcript a model state stands for may carry THE SECOND
/// REVIEW'S HISTORY — a direction the agent answered with READY, before what
/// the state says ([`real_of`]): wherever a READY may stand in the tail
/// without being the answer the state denies — it is the answer, its
/// markers are forgotten, or someone spoke after it.
fn history_fits(s: &S) -> bool {
    s["ready"] == 1 || s["live"] == 0 || s["told"] == 1 || s["directed"] == 1
}

/// The record, facts, clock and transcript a model state stands for: the
/// record of the agent the notice reached, its fence on that agent, the
/// round's marker typed. At a break (`brk`), the look is the window's at a
/// break of the agent's own work: Claude's status `shell`, a shell running
/// under the agent. The transcript: the notice;
/// with `history` ([`history_fits`]), a peer's message the agent answered
/// with READY; the READY where the state has one, else the agent's
/// wind-down; a direction not answered yet (`told`), or one the agent took
/// up (`directed`).
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
    // A stopped round's rest: run out (`RETRY_S` ago), or begun at this look.
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
        // A pending round that owes a release is one a re-arm started: the
        // round before it asked, and its markers stay the release's to read
        // a direction against ([`St::rearm`] keeps them; its first notice
        // clears them).
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

/// A record and a transcript projected onto the model's variables — the
/// record's RAW state, not the driver's reading of it (the review of
/// 2026-09-26: a projection that zeroed `live` for every stopped phase hid a
/// stop that kept its markers): `live`, the round's markers kept; `ready`, an
/// answer to one of them ([`answered`]); `rest`, a stopped round's rest run
/// out at `now` ([`St::failed_for`]). A pending round has asked nothing
/// ([`St::rearm`] resets the asks). The agent's, the conversation's and the
/// ghosts' are taken from `s`.
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

/// THE REAL VISIT'S DECISION at the look `s` stands for, under `holds`, over
/// the transcript [`real_of`] writes for it (`history`: the second review's
/// direction answered with READY), in the driver's order and through the
/// driver's own code: the look's clock ([`upgrade::clock_held`]); the READY
/// the step acts on ([`heard`]) and its clock ([`upgrade::ready_since`]); the
/// step ([`upgrade::requested_step`]); its record transition
/// ([`St::announced`], [`St::give_up`], [`St::void`], [`St::signalled`] —
/// refused, [`St::stop`] or [`St::signal_failed`]), at a break held to what a
/// break may do ([`upgrade::break_step`]); the release where it is the next
/// act ([`release_next`], at a break too) — DROPPED where [`release_void`] says
/// the agent took up direction since its last answer ([`St::dropped`]), else
/// typed under [`upgrade::gate_release`] ([`St::released`]) or the word that
/// owes it ([`owed_word`]); and the host's reading of the word — its LAST
/// WORD ([`after`] is Finished) at a point the agent can read is the model's
/// `Look` (the window's host takes no step at a limit). `None` where `holds`
/// cannot be met: a refusal needs a restart, or a re-ask, to refuse; work
/// inside the agent's process, and [`Holds::Break`], are decided only at a
/// break (the latter only where [`at_a_break`] says).
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
        // At a break (`brk`, and so every `Holds::Break`) [`real_of`] has
        // made the look a break's already; off one it is an idle point's.
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
    // At a break nothing is ended, whatever the plan says (the driver's rule).
    if f.background_point {
        step = upgrade::break_step(step);
    }
    let reask = step == Step::Announce && st.phase != Phase::Pending;
    if holds == Holds::Refused && !(step == Step::Terminate || reask) {
        return None;
    }
    let mut agent = s.clone();
    let mut actions = Vec::new();
    let mut word = match step {
        Step::Announce if holds == Holds::Refused => {
            // `unplanned`: the relaunch refuses the plan the re-ask asks first.
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
            // The agent reads it at once: nothing is typed at a limit. What
            // the conversation said before it is behind the notice now.
            agent.insert("holding", 1);
            agent.insert("told", 0);
            agent.insert("directed", 0);
            // Over a READY it stands for (the agent's own work outlived the
            // answer): the notice that supersedes it.
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
                // Signalled, relaunched and carried on: a new process, and
                // its conversation the carry-on's.
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
        Step::Void(why) => {
            st.void(now);
            actions.push("Void");
            format!("drain-expired:{why}")
        }
        Step::Wait(why) => format!("wait:{why}"),
        // A stopped round that has rested: a new one ([`rearm`]'s record
        // transition; its first notice is the next look's).
        Step::Rearm => {
            let why = st.rearm(now);
            actions.push("Rearm");
            format!("rearmed:{why}")
        }
        // The model's conversation has a task: never restarted afresh.
        Step::Fresh => unreachable!("a conversation with a task: {f:?}"),
    };
    if release_next(&step, &word, &st, &f, ready) {
        let text = tail.join("\n");
        if let Some(why) = release_void(&st, &sf, TAB, Some(&text)) {
            assert_eq!(why, "directed", "the model's agent is the notice's");
            // `drop_release`: nothing owed, the round over; `r` kept.
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

/// The states decided AT A BREAK ([`Holds::Break`]): a round that gave up,
/// holding a late READY to one of its markers, at a break of the agent's own
/// work (`brk`) — the owner's stall of 2026-09-27, whose tab took nearly
/// every step at one. The answer the work outlives past the drain is voided
/// there as at an idle point (`Void`, and the release it owes typed at the
/// same look, as a notice could be), never waited on for good: until that
/// day the break answered `background` before the drain's bound was ever
/// asked.
fn at_a_break(s: &S) -> bool {
    s["brk"] == 1 && s["phase"] == 2 && s["ready"] == 1 && s["live"] == 1
}

/// THE UPGRADE'S QUIET WORD — nothing it would do for the agent at this look
/// (the model's `Look`): its last word ([`After::Finished`]), or a stopped
/// round resting before its next (`wait:failed`), which the window looks at
/// again ([`after`]) but which asks, releases and restarts nothing until the
/// rest runs out.
fn quiet(word: &str) -> bool {
    word == "wait:failed" || after(word, 0) == After::Finished
}

/// The reducer's actions, the environment's aside; `Void` is a person's or
/// the agent's own work's, `Abandon` a refusal's.
const REDUCER: [&str; 7] = [
    "Announce",
    "GiveUp",
    "Restart",
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

/// Whether `m` admits what the real visit decided at `s` under `holds`: at an
/// idle look, the reducer's own step is the ONE the model's guards enable (a
/// wait where none is); held by a person, the void exactly where the model
/// voids; held by the agent's own work, the re-ask or give-up over the READY
/// where the model takes one, else its void; at a break of that work, the
/// void exactly where the model voids; refused, the round abandoned — and the
/// model, firing the same actions in the same order, lands where the real
/// record does.
fn agrees(m: &Model, s: &S, holds: Holds, d: &Decided) -> bool {
    let lands = || {
        let mut t = s.clone();
        d.actions.iter().all(|a| m.fire(a, &mut t)) && t == d.next
    };
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
        // The agent's own work under a READY past the bound: the re-ask that
        // supersedes it, or the give-up over it, where the model takes one;
        // else the void where the model voids (a gave-up upgrade's) — work
        // under the agent and work inside its own process alike.
        Holds::Background | Holds::InProcess => {
            let held = ["Supersede", "GiveUpOutlived", "Void"];
            let want = held.into_iter().find(|a| m.action_enabled(a, s));
            let took = d.actions.first().copied().filter(|a| held.contains(a));
            want == took && (want.is_none() || lands())
        }
        // At a break of that work: the void exactly where the model voids —
        // the break's old `wait:background` (or any wait) there is no step
        // the model has, and so a disagreement.
        Holds::Break => {
            let voided = d.actions.first() == Some(&"Void");
            m.action_enabled("Void", s) == voided && (!voided || lands())
        }
        Holds::Refused => {
            d.actions.first() == Some(&"Abandon") && m.action_enabled("Abandon", s) && lands()
        }
    }
}

/// THE CONVERSATION'S OWN MOVES, read by the real code
/// ([`upgrade::transcript_has_ready`] through [`answered`], and
/// [`release_void`] over [`upgrade::directed_since_ready`]): the row the
/// environment action `action` writes — a direction (`Direct`), the agent's
/// READY (`AgentReady`), its answer that is none (`AgentGoesOn`) — appended to
/// the transcript `s` stands for (`history`), and read back as the model's
/// `ready` and as whether the release's point would DROP the release (the
/// model's own `DropRelease` guard at that point). `None` where `m` does not
/// enable the action at `s`; else whether the real readings are the model's.
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

/// The model with one defect switched on: the incident's reducer (`Buggy`),
/// and each defect the fix and its reviews found.
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
            ]
            .into_iter()
            .map(|knob| (knob, aterm_spec::interp::with_consts(&model, &[(knob, 1)]))),
        )
        .collect()
}

/// TIER-1: on EVERY reachable state of `harness_upgrade_never_strands_model`
/// the real code — its reducer, its gates, its record transitions (the
/// restart's own [`St::signalled`] and [`St::signal_failed`], the stops'
/// [`St::stop`], the drop's [`St::dropped`]), the driver's READY, direction
/// and release rules, and the window's reading of each word ([`after`]) —
/// takes the step the model's guards allow and lands where the model's
/// actions land: at an idle look the one reducer step; under a person's hold
/// past the drain, the void exactly where the model voids; under the agent's
/// own work past it — a process under the agent, or at a break work inside
/// its own process (the review of 2026-09-27) — the re-ask that supersedes
/// the READY (or the give-up over it) where the model takes one, else the
/// void; AT A BREAK of that work, a gave-up round's late READY voided
/// exactly where the model voids (the no-stall review's S2: the break's old
/// `wait:background` is a disagreement, so reverting the break's void in
/// [`upgrade::next_step`] turns this red); under a refused signal or plan,
/// the round abandoned and its release typed; the release dropped exactly
/// where the model drops it; and the last word only where the model says
/// it. Each state is decided over its plain transcript and, where it fits,
/// over THE SECOND REVIEW'S HISTORY — a peer's
/// message the agent answered with READY — whose release a void owes and the
/// pre-fix reading dropped. At a BREAK (`brk`, 2026-09-27) the look is the
/// window's at a break of the agent's own work, held to [`upgrade::break_step`]:
/// no restart, and the release, its drop and a gave-up upgrade's void there
/// as at an idle point (`IdleOnlyRelease` is the defect the incident ran on).
/// No step says the host's last word over a release
/// owed. The conversation's own moves (`Direct`, `AgentReady`,
/// `AgentGoesOn`) are read back by the real transcript readers as the model
/// says ([`reads_as`]). The clocks: an announced upgrade's window runs out
/// only off the limit (`Elapse`, the look's hold), and a limit the window's
/// loop saw starts it again (`LimitResets`, [`St::hold_clock`]). NEGATIVE
/// CONTROLS: the model with any ONE defect switched on — the incident's
/// reducer, and each defect the fix and its reviews found — disagrees with
/// the real code somewhere.
#[test]
fn the_real_upgrade_conforms_to_the_never_strands_model() {
    let model = harness_upgrade_never_strands_model();
    let defective = defective();
    let states = reachable(&model);
    assert!(states.len() > 50, "{} states", states.len());
    let mut disagree: BTreeMap<&str, usize> = defective.iter().map(|(n, _)| (*n, 0)).collect();
    let mut taken = std::collections::BTreeSet::new();
    let mut histories = 0;
    // At a break: the late READYs decided there, and those the model voids.
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
                        // THE BREAK BEFORE THE FIX, as a decision: it waited
                        // `background` over the answer, for good. The model
                        // refuses it — so reverting the break's void in
                        // `upgrade::next_step` turns this bind red.
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
                for (name, m) in &defective {
                    if !agrees(m, s, holds, &d) {
                        *disagree.get_mut(name).expect("a knob") += 1;
                    }
                }
            }
            for action in ["Direct", "AgentReady", "AgentGoesOn"] {
                if let Some(reads) = reads_as(&model, s, action, history) {
                    assert!(reads, "{action} at {s:?} (history {history})");
                    taken.insert(action);
                }
                for (name, m) in &defective {
                    if reads_as(m, s, action, history) == Some(false) {
                        *disagree.get_mut(name).expect("a knob") += 1;
                    }
                }
            }
        }
        // The clocks. An announced upgrade's window runs out only off the
        // limit, because every look that finds the limit restarts it.
        let (st, f, now, _) = real_of(s, false);
        if s["phase"] == 1 && s["window"] == 0 {
            let held = upgrade::clock_held(&st.phase, &f, now + upgrade::REASK_S);
            let runs = matches!(held, Phase::Announced { at_s, .. } if at_s == T0);
            assert_eq!(runs, model.action_enabled("Elapse", s), "Elapse at {s:?}");
            for (name, m) in &defective {
                if runs != m.action_enabled("Elapse", s) {
                    *disagree.get_mut(name).expect("a knob") += 1;
                }
            }
        }
        // A stopped round's rest runs out RETRY_S after its stop, limited or
        // not (`Rests`): the look that finds it so re-arms it, or — a
        // gave-up round's late READY in hand — acts on the answer.
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
            for (name, m) in &defective {
                if runs != m.action_enabled("Rests", s)
                    || rearms != m.action_enabled("Rearm", &rested)
                {
                    *disagree.get_mut(name).expect("a knob") += 1;
                }
            }
        }
        // A limit the window's loop saw holds it too: its close starts the
        // window again, as `LimitResets` does.
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
            for (name, m) in &defective {
                let mut t = s.clone();
                if m.fire("LimitResets", &mut t) && t["window"] != window {
                    *disagree.get_mut(name).expect("a knob") += 1;
                }
            }
        }
    }
    assert!(histories > 0, "the second review's history is decided");
    assert!(
        voided_at_breaks > 0 && breaks > voided_at_breaks,
        "a late READY is decided at a break, voided there and (limited) not: \
         {voided_at_breaks} of {breaks}"
    );
    // The reducer's seven, the held three (`Void`, and the re-ask and give-up
    // over a READY the agent's work outlived), `Abandon`, and the
    // conversation's three.
    assert_eq!(
        taken.len(),
        REDUCER.len() + 7,
        "every step of the upgrade, and every move of the conversation, is taken by the real \
         code: {taken:?}"
    );
    for (name, n) in &disagree {
        assert!(
            *n > 0,
            "the real code has the `{name}` defect: {disagree:?}"
        );
    }
}

/// TIER-1, the incident's schedule: the environment as it ran (the limit
/// hits, holds past four half-hours, resets; the agent answers READY) and the
/// REAL decisions between, each transition admitted by the model and every
/// state within its invariants — one notice after the reset, then the
/// restart. The pre-fix run (the ledger's notices at the limit, its give-up,
/// the READY heard by nobody) is admitted only by `Buggy = 1`, refused by the
/// committed model at its first notice, and ends stranded.
#[test]
fn the_incident_schedule_conforms_and_the_pre_fix_run_is_caught() {
    let model = harness_upgrade_never_strands_model();
    let admit = |m: &Model, prev: &S, next: &S| aterm_spec::interp::admits(m, prev, next);
    let env = |s: &S, action: &str| {
        let mut next = s.clone();
        assert!(model.fire(action, &mut next), "{action} at {s:?}");
        next
    };
    let look = |s: &S| decide(s, Holds::Nothing, false).expect("an idle look");
    let mut trace = vec![model.init_state()];
    let push = |trace: &mut Vec<S>, next: S| {
        let prev = trace.last().expect("a state").clone();
        assert!(
            admit(&model, &prev, &next).is_some(),
            "{prev:?} -> {next:?}"
        );
        for inv in ["NoNoticeWhileLimited", "NeverStranded"] {
            assert!(model.check_invariant(inv, &next), "{inv} at {next:?}");
        }
        trace.push(next);
    };
    let last = |trace: &Vec<S>| trace.last().expect("a state").clone();
    let next = env(&last(&trace), "LimitHits");
    push(&mut trace, next);
    // 16:02 .. 18:02: the four looks and the give-up's — every one a wait.
    for _ in 0..5 {
        assert!(look(&last(&trace)).actions.is_empty());
    }
    let next = env(&last(&trace), "LimitResets");
    push(&mut trace, next);
    let d = look(&last(&trace));
    assert_eq!(d.actions, ["Announce"]);
    push(&mut trace, d.next);
    let next = env(&last(&trace), "AgentReady");
    push(&mut trace, next);
    let d = look(&last(&trace));
    assert_eq!(d.actions, ["Restart"]);
    push(&mut trace, d.next);
    assert_eq!((last(&trace)["phase"], last(&trace)["holding"]), (3, 0));

    // THE PRE-FIX RUN, as the ledger and the journal recorded it (asks scaled
    // to the model's bound): at the limit, notice, half an hour, notice, half
    // an hour, give up; the reset; READY; the last word `failed`.
    let buggy = aterm_spec::interp::with_buggy(&model, 1);
    let ran = [
        "LimitHits",
        "Announce",
        "Elapse",
        "Announce",
        "Elapse",
        "GiveUp",
        "LimitResets",
        "AgentReady",
        "Look",
    ];
    let mut s = buggy.init_state();
    let mut refused = None;
    for action in ran {
        let prev = s.clone();
        assert!(buggy.fire(action, &mut s), "{action} at {prev:?}");
        assert_eq!(admit(&buggy, &prev, &s), Some(action));
        if refused.is_none() && admit(&model, &prev, &s).is_none() {
            refused = Some(action);
            // And the real code, at that very state, does not take it.
            assert_ne!(look(&prev).actions.first(), Some(&action), "{prev:?}");
        }
    }
    assert_eq!(refused, Some("Announce"), "the first notice at the limit");
    assert!(!buggy.check_invariant("NeverStranded", &s), "{s:?}");
    assert!(!buggy.check_invariant("NoNoticeWhileLimited", &s), "{s:?}");
}

// ------------------------------------------ no stop is for good (2026-09-27)

/// EVERY STOP IS STAMPED, AND A RE-ARM IS A NEW ROUND ([`St::fail`],
/// [`St::rearm`]). The give-up, a stop, a refused signal and a gave-up
/// round's void each stamp the rest's start; the new round is pending with a
/// fresh salt past every marker the old one could mint, its markers, stamp
/// and saved prompt position gone, the owner's spent `--now` gone (a skip
/// kept), the release still owed carried for the new notice to supersede, and why the old one
/// stopped kept until that notice. A state an older build wrote carries no
/// stamp — no migration: it reads as stopped long ago and is due at once.
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
    // A gave-up round's void begins its rest again.
    let mut voided = st.clone();
    voided.void(5_000);
    assert_eq!(voided.failed_at, 5_000);
    // THE NEW ROUND.
    let mut skip = st.clone();
    skip.request = Request::Skip("9.9.9".to_string());
    // The prompt a refused relaunch saved is the stopped round's, never the
    // new one's to type against.
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
    // Its first notice is ask 1, supersedes the release and clears the stop.
    assert_eq!(upgrade::announce_asks(&st.phase, false), 1);
    st.announced(marker(2), 9_100, 1);
    assert!(st.release.is_empty() && st.last_stop.is_empty());
    assert_eq!(st.markers, vec![marker(2)]);
    // The record keeps the stamp, and an older build's reads with none.
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

/// A VOID NEVER SHORTENS THE REST A REPEATED STOP EARNED (round six, F27).
/// Three give-ups in a row stretch the rest to `4 × RETRY_S`
/// ([`upgrade::rest_extension`]); a late READY the agent's own work outlives
/// is voided half an hour on, and the rest begins again at the void — as
/// long as the streak makes it, never a bare `RETRY_S` from there.
///
/// FAILS WITHOUT THE FIX: the void stamped `failed_at = now`, and the next
/// round was due `RETRY_S` after the void, six hours before the rest the
/// streak had earned ran out.
#[test]
fn a_void_never_shortens_the_rest_a_repeated_stop_earned() {
    const T: u64 = 1_790_000_000;
    let mut st = St::default();
    for _ in 0..3 {
        st.give_up(T);
    }
    let before = st.failed_at;
    assert_eq!(before, T + 3 * upgrade::RETRY_S);
    let void_at = T + 1_800;
    st.void(void_at);
    assert!(
        st.failed_at >= before,
        "a void never shortens the rest a repeated stop earned: {} < {before}",
        st.failed_at
    );
    assert_eq!(
        st.failed_at,
        void_at + 3 * upgrade::RETRY_S,
        "begun again at the void, stretched by the streak"
    );
    assert!(
        !upgrade::retry_due(&st.phase, st.failed_for(void_at + upgrade::RETRY_S), false),
        "not due a bare RETRY_S after the void"
    );
    // NEGATIVE CONTROL: a first stop's void rests a plain RETRY_S from the
    // void, as the rule has always said.
    let mut first = St::default();
    first.give_up(T);
    first.void(void_at);
    assert_eq!(first.failed_at, void_at);
    assert!(upgrade::retry_due(
        &first.phase,
        first.failed_for(void_at + upgrade::RETRY_S),
        false
    ));
}

/// THE LONGEST SILENCE, through the real record and the real reducer (the
/// no-stall review of 2026-09-27: `REASK_S + RETRY_S` was claimed as its
/// bound, but a void begins the rest again). A round's last notice goes out;
/// its window runs out and it gives up; a late READY comes a second before
/// its rest runs out and holds the rest where it is, at an idle point and at
/// a break alike; the agent's own work outlives the answer, and the void
/// comes `DRAIN_S` after it, beginning the rest again; the new round starts
/// a rest after the void, as long as the streak makes it (round six, F27).
/// Five hours today after a first stop, seventeen after one repeated to the
/// cap, as `upgrade::RETRY_S` says — and without
/// the late READY, two and a half and eight and a half.
#[test]
fn the_longest_silence_is_the_window_a_rest_a_drain_and_a_rest() {
    const H: u64 = 3_600;
    // The first second in `from..to` at which `hit` holds.
    let first = |from: u64, to: u64, hit: &dyn Fn(u64) -> bool| (from..to).find(|t| hit(*t));
    for (prior_stops, with_ready, hours) in [
        (0, false, 5 * H / 2),
        (0, true, 5 * H),
        (upgrade::RETRY_BACKOFF_MAX_SHIFT, false, 17 * H / 2),
        (upgrade::RETRY_BACKOFF_MAX_SHIFT, true, 17 * H),
    ] {
        let case = format!("{prior_stops} stop(s) before, late READY {with_ready}");
        let last_notice = T0;
        let mut st = St {
            to: "9.9.9".to_string(),
            streak_why: upgrade::GAVE_UP.to_string(),
            stop_streak: prior_stops,
            ..St::default()
        };
        st.announced(marker(1), last_notice, upgrade::MAX_ASKS);
        // The give-up: the reducer's, at the first second its window allows.
        let gave_up_at = first(last_notice, last_notice + H, &|t| {
            upgrade::next_step(&st.phase, &idle(false), false, t) == Step::GiveUp
        })
        .expect("a give-up");
        assert_eq!(gave_up_at, last_notice + upgrade::REASK_S, "{case}");
        st.give_up(gave_up_at);
        let rested =
            |st: &St, ready: bool, t: u64| upgrade::retry_due(&st.phase, st.failed_for(t), ready);
        let rest_end = first(gave_up_at, gave_up_at + 12 * H, &|t| rested(&st, false, t))
            .expect("the rest runs out");
        let new_round = if with_ready {
            // A late READY a second before the rest runs out holds it there.
            let heard_at = rest_end - 1;
            assert!(!rested(&st, true, heard_at + 7 * 24 * H), "{case}: held");
            let look = |st: &mut St, t: u64, at_break: bool| {
                let mut f = Facts {
                    background: vec!["zsh".to_string()],
                    background_point: at_break,
                    ..idle(false)
                };
                st.time_ready(true, &mut f, t);
                st.time_failed(&mut f, t);
                let step = upgrade::next_step(&st.phase, &f, true, t);
                if at_break {
                    upgrade::break_step(step)
                } else {
                    step
                }
            };
            let mut voids = Vec::new();
            for at_break in [false, true] {
                let mut probe = st.clone();
                let _ = look(&mut probe, heard_at, at_break);
                voids.push(
                    first(heard_at, heard_at + 12 * H, &|t| {
                        matches!(look(&mut probe.clone(), t, at_break), Step::Void(_))
                    })
                    .expect("the answer is voided"),
                );
            }
            assert_eq!(voids, [heard_at + upgrade::DRAIN_S; 2], "{case}");
            st.void(voids[0]);
            first(voids[0], voids[0] + 12 * H, &|t| rested(&st, false, t)).expect("a new round")
        } else {
            rest_end
        };
        assert_eq!(
            new_round - last_notice + u64::from(with_ready),
            hours,
            "{case}"
        );
    }
}

/// THE OWNER'S STALL OF 2026-09-27, THROUGH THE REAL VISIT (tab
/// `s-d3346b29dd236432b852`: `phase=failed:unanswered pending_for=1d22h
/// wait=background … stalled=gave-up`, and `--dry-run` said
/// `step=wait:failed`). The round gave up after four notices; the agent
/// answered READY late; a shell of its own ran on under it past the drain.
/// The READY is voided and the agent released — and the round RESTS, looked
/// at again (`wait:failed` is no last word), never waited on for ever. Once
/// it has rested `RETRY_S` a new round starts (`rearmed:unanswered`, on the
/// ledger, nothing typed), and the next look types its first notice, with a
/// marker of its own, NAMING WHAT RUNS under the agent. At a BREAK of that
/// work — where the owner's tab took nearly every step — the same record,
/// as an older build wrote it (no stamp), is re-armed and asked there.
/// NEGATIVE CONTROLS: the owner's `--skip` of the build holds the stopped
/// round for good, typing nothing; a limited session is re-armed but not
/// asked.
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
    // A conversation at work when the notices came (one with no task is
    // restarted afresh, never asked).
    let mut delivered = vec![user("Tidy the parser module.")];
    delivered.extend(LEDGER.iter().map(|(_, m)| user(&notice(m))));
    delivered.push(said_by_agent(&format!("Stopping.\n{}", LEDGER[3].1)));
    let run = |opts: &Opts, agent: &Agent| {
        let shell = dead_pid();
        // A shell the agent left running, under it: its own work.
        let table = vec![
            (shell, 1, "zsh".to_string()),
            (agent.sf.pid + 100_000, agent.sf.pid, "zsh".to_string()),
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
    // The READY its own work outlived: voided, the agent released.
    assert_eq!(run(&opts, &agent).step, "released:gave-up");
    assert_eq!(typed(&asked, "Upgrade off:"), 1);
    // Resting, NOT finished: looked at again, nothing typed.
    let resting = run(&opts, &agent);
    assert_eq!(resting.step, "wait:failed");
    assert!(matches!(after(&resting.step, 9), After::Later(_)));
    assert_eq!(turns(&asked), 1);
    // The rest runs out.
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
        ledger.contains(r#""step":"rearmed:unanswered""#) && ledger.contains("no stop is for good"),
        "{ledger}"
    );
    assert!(matches!(after(&rearmed.step, 0), After::Later(_)));
    // Its first notice: a marker of its own, and what runs under the agent.
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
        typed_notice.contains(&format!(
            "Running under you now, as aterm sees it: pid {} (zsh",
            agent.sf.pid + 100_000
        )),
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

    // AT A BREAK, as an older build wrote the record: no stamp, the READY
    // long voided (its markers gone), a stale READY in the transcript.
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

/// TIER-1, THE OWNER'S STALL AS A SCHEDULE: the environment as it ran (the
/// agent's late READY, its own work outliving it, the rest running out) and
/// the REAL decisions between — the give-up, the void and the release at a
/// break, the new round, its notice, the restart on its READY — each
/// transition admitted by
/// the model and every state within `NeverStalls` and `NeverStranded`. The
/// same schedule under `Terminal = 1` (the reducer as it ran) is refused at
/// the new round the real code takes, and ends at the quiet word past the
/// rest: `NeverStalls` catches it.
#[test]
fn the_owners_stall_schedule_conforms_and_the_terminal_run_is_caught() {
    let model = harness_upgrade_never_strands_model();
    let terminal = aterm_spec::interp::with_consts(&model, &[("Terminal", 1)]);
    let admit = |m: &Model, prev: &S, next: &S| aterm_spec::interp::admits(m, prev, next);
    let env = |s: &S, action: &str| {
        let mut next = s.clone();
        assert!(model.fire(action, &mut next), "{action} at {s:?}");
        next
    };
    let look = |s: &S, holds: Holds| decide(s, holds, false).expect("a look");
    let mut trace = vec![model.init_state()];
    let push = |trace: &mut Vec<S>, next: S| {
        let prev = trace.last().expect("a state").clone();
        assert!(
            admit(&model, &prev, &next).is_some(),
            "{prev:?} -> {next:?}"
        );
        for inv in ["NeverStalls", "NeverStranded", "NoNoticeWhileLimited"] {
            assert!(model.check_invariant(inv, &next), "{inv} at {next:?}");
        }
        trace.push(next);
    };
    let last = |trace: &Vec<S>| trace.last().expect("a state").clone();
    // One real look: its actions fired in the model one by one, each state
    // admitted, landing where the real record does.
    let step = |trace: &mut Vec<S>, holds: Holds, want: &[&str]| {
        let d = look(&last(trace), holds);
        assert_eq!(d.actions, want, "{:?}", last(trace));
        for action in &d.actions {
            let mut next = last(trace);
            assert!(model.fire(action, &mut next), "{action}");
            push(trace, next);
        }
        assert_eq!(last(trace), d.next);
    };
    step(&mut trace, Holds::Nothing, &["Announce"]);
    let next = env(&last(&trace), "Elapse");
    push(&mut trace, next);
    step(&mut trace, Holds::Nothing, &["Announce"]);
    let next = env(&last(&trace), "Elapse");
    push(&mut trace, next);
    // The give-up, with a box up at the tab that holds its release back —
    // so the agent's READY can come late, as the owner's did.
    step(&mut trace, Holds::Person, &["GiveUp"]);
    let next = env(&last(&trace), "AgentReady");
    push(&mut trace, next);
    // Its own work outlives the READY past the drain: voided AT A BREAK of
    // that work, where the owner's tab took nearly every step (and waited
    // `background` on the answer for good before the fix) — and the release
    // it owes typed at the same look, as a notice could be there. The break
    // then ends.
    let next = env(&last(&trace), "BreakBegins");
    push(&mut trace, next);
    step(&mut trace, Holds::Break, &["Void", "Release"]);
    let next = env(&last(&trace), "BreakEnds");
    push(&mut trace, next);
    // Resting: the quiet word, not stranded (released), not stalled.
    step(&mut trace, Holds::Nothing, &["Look"]);
    let rested = env(&last(&trace), "Rests");
    push(&mut trace, rested.clone());
    step(&mut trace, Holds::Nothing, &["Rearm"]);
    step(&mut trace, Holds::Nothing, &["Announce"]);
    let next = env(&last(&trace), "AgentReady");
    push(&mut trace, next);
    step(&mut trace, Holds::Nothing, &["Restart"]);
    assert_eq!((last(&trace)["phase"], last(&trace)["holding"]), (3, 0));

    // THE TERMINAL RUN: the real code's new round is no move of its.
    let mut rearmed = rested.clone();
    assert!(model.fire("Rearm", &mut rearmed));
    assert_eq!(admit(&terminal, &rested, &rearmed), None, "refused");
    assert_eq!(look(&rested, Holds::Nothing).actions, ["Rearm"]);
    let mut s = rested;
    assert!(terminal.fire("Look", &mut s), "{s:?}");
    assert!(!terminal.check_invariant("NeverStalls", &s), "{s:?}");
}
