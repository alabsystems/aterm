// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;

fn pending() -> St {
    let now = now_s();
    St {
        phase: Phase::Pending,
        from: "0.9.0".into(),
        to: "1.0.0".into(),
        source: "managed".into(),
        tab: TAB.into(),
        pending_since: now - 7_200,
        salt: now - 7_200,
        wait: "limited".into(),
        wait_since: now - LIMIT_GRACE_S - 3_600,
        limit_until: now - LIMIT_GRACE_S - 600,
        looked_at: now,
        ..St::default()
    }
}

fn targets() -> Targets {
    Targets {
        managed: Some(Candidate {
            exe: PathBuf::from("/unavailable/claude"),
            version: Version::parse("1.0.0").unwrap(),
            source: Source::Managed,
        }),
        native: None,
        unanswered: false,
    }
}

fn owed(h: &Parked) -> Due {
    let opts = Opts {
        only_sid: Some(TAB.into()),
        ..h.opts.clone()
    };
    let files = session_files(&opts.home).expect("fresh session roster");
    due_among(
        &opts,
        &files,
        &the_tab(),
        targets,
        atpkg::caller_shell::process_args,
        |_| Some(101),
        |_| Some((10, 101, 101)),
    )
}

fn step(h: &Parked, dry_run: bool) -> Vec<Report> {
    let opts = Opts {
        only_sid: Some(TAB.into()),
        dry_run,
        ..h.opts.clone()
    };
    let files = session_files(&opts.home).expect("fresh session roster");
    // The injected group/terminal geometry admits the fixture's tab. The real visit still
    // checks the parked child's actual kernel birth, argv and unique conversation ownership.
    roster_reports(
        &opts,
        &files,
        &the_tab(),
        targets,
        atpkg::caller_shell::process_args,
        |_| Some(101),
        |_| Some((10, 101, 101)),
    )
}

fn rows(h: &Parked) -> Vec<Row> {
    super::super::rows(&h.opts)
}

fn untouched_agent(h: &Parked) {
    let requests = h.asked.lock().unwrap();
    assert!(
        !requests.iter().any(|line| {
            line.contains(" turn ")
                || line.contains(" send ")
                || line.contains(" key ")
                || line.contains(" signal ")
                || line.contains(" native-act ")
        }),
        "settling the record performs no agent operation: {requests:?}"
    );
    assert!(alive(h.sf.pid), "the original agent still lives");
}

#[test]
fn a_reached_pending_wait_owes_one_real_visit_and_dry_looks_leave_it_owed() {
    at_second(now_s(), || {
        let h = Parked::new(
            "pending-reached-roster",
            Answers::default(),
            pending(),
            3_600,
        );
        let before = load(&h.opts, SESSION).unwrap();
        assert!(
            rows(&h)[0].limit_passed(now_s()),
            "the aged wait is consequential"
        );
        assert_eq!(owed(&h), Due::Yes);
        let dry = step(&h, true);
        assert_eq!(dry.len(), 1);
        assert_eq!(dry[0].session, SESSION);
        assert_eq!(dry[0].step, "current");
        assert_eq!(load(&h.opts, SESSION), Some(before));
        assert!(h.details("reached:by-hand").is_empty());
        assert_eq!(owed(&h), Due::Yes, "a dry look cannot consume the close");

        let real = step(&h, false);
        assert_eq!(real.len(), 1);
        assert_eq!(real[0].step, "current");
        assert!(load(&h.opts, SESSION).is_none());
        assert!(
            rows(&h).is_empty(),
            "neither limited nor an empty pending wait ages further"
        );
        assert_eq!(h.details("reached:by-hand").len(), 1);
        assert_eq!(owed(&h), Due::No);
        assert!(step(&h, false).is_empty());
        assert_eq!(h.details("reached:by-hand").len(), 1, "closed exactly once");
        untouched_agent(&h);
    });
}

#[test]
fn a_reached_native_pending_wait_closes_once_and_its_retained_row_is_quiet() {
    at_second(now_s(), || {
        let st = St {
            native_coordination: true,
            source: "native".into(),
            ..pending()
        };
        let h = Parked::new(
            "pending-native-reached-roster",
            Answers::default(),
            st,
            3_600,
        );
        assert_eq!(owed(&h), Due::Yes);
        assert_eq!(step(&h, false).len(), 1);
        let closed = load(&h.opts, SESSION).expect("native coordination stays recorded");
        assert_eq!(closed.phase, Phase::Done);
        assert!(closed.native_coordination);
        assert_eq!(closed.done_at, now_s());
        let shown = rows(&h);
        assert_eq!(shown.len(), 1);
        assert!(matches!(shown[0].attention(now_s()), Attention::Quiet));
        assert_eq!(owed(&h), Due::No);
        assert!(step(&h, false).is_empty());
        assert_eq!(h.details("reached:by-hand").len(), 1);
        untouched_agent(&h);
    });
}

#[test]
fn pending_waits_still_behind_deferred_or_holding_native_debt_are_not_settlement_visits() {
    at_second(now_s(), || {
        let behind = St {
            to: "1.0.1".into(),
            ..pending()
        };
        let deferred = St {
            request: Request::DeferUntil(now_s() + 3_600),
            ..pending()
        };
        let invalid = St {
            to: "unreadable".into(),
            ..pending()
        };
        let mut debt = St {
            native: Some(crate::harness::native_act::Binding {
                id: "ab".repeat(16),
                capability: "cd".repeat(32),
            }),
            ..pending()
        };
        retain_bound_preparation(&mut debt);
        for (name, st) in [
            ("behind", behind),
            ("deferred", deferred),
            ("invalid", invalid),
            ("native-debt", debt),
        ] {
            let h = Parked::new(
                &format!("pending-not-settled-{name}"),
                Answers::default(),
                st,
                3_600,
            );
            let before = load(&h.opts, SESSION).expect("the control record");
            assert_eq!(owed(&h), Due::No, "{name}");
            assert!(step(&h, false).is_empty(), "{name}");
            assert_eq!(
                load(&h.opts, SESSION),
                Some(before),
                "{name}: every wait and word stays"
            );
            assert!(h.details("reached:by-hand").is_empty(), "{name}");
            untouched_agent(&h);
        }
    });
}

#[test]
fn a_reached_version_in_a_stale_process_record_cannot_close_its_pending_wait() {
    at_second(now_s(), || {
        let h = Parked::new("pending-stale-record", Answers::default(), pending(), 3_600);
        let stale = register_born(
            h.sf.claude_dir
                .as_deref()
                .expect("the fixture's records directory"),
            h.sf.pid,
            SESSION,
            "Thu Jan  1 00:00:00 1970",
        );
        assert_ne!(
            stale.proc_start, h.sf.proc_start,
            "the actual record names another birth"
        );
        let before = load(&h.opts, SESSION).unwrap();
        assert_eq!(
            owed(&h),
            Due::Yes,
            "the readonly filter owes a validated visit"
        );
        let report = step(&h, false);
        assert_eq!(report.len(), 1);
        assert_eq!(report[0].step, "wait:stale-file");
        assert_eq!(load(&h.opts, SESSION), Some(before));
        assert!(h.details("reached:by-hand").is_empty());
        untouched_agent(&h);
    });
}
