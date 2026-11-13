// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;

#[test]
fn the_state_and_program_words_are_the_logs() {
    use aterm_core::terminal::program_state::{ProgramBlockedKind, ProgramStateWord};
    use aterm_slog::vocab::{ASK_WORDS, STATE_WORDS};
    let sessions = [
        SessionState::Spawning,
        SessionState::Alive,
        SessionState::Exited,
    ];
    for s in sessions {
        match s {
            SessionState::Spawning | SessionState::Alive | SessionState::Exited => {}
        }
        assert!(STATE_WORDS.contains(&s.as_str()), "{:?}", s.as_str());
    }
    assert!(STATE_WORDS.contains(&"closed"), "the close's row");
    let states = [
        ProgramStateWord::Idle,
        ProgramStateWord::Working,
        ProgramStateWord::Blocked,
        ProgramStateWord::Done,
        ProgramStateWord::Error,
    ];
    for s in states {
        match s {
            ProgramStateWord::Idle
            | ProgramStateWord::Working
            | ProgramStateWord::Blocked
            | ProgramStateWord::Done
            | ProgramStateWord::Error => {}
        }
        assert!(STATE_WORDS.contains(&s.as_str()), "{:?}", s.as_str());
    }
    let asks = [
        ProgramBlockedKind::Permission,
        ProgramBlockedKind::Question,
        ProgramBlockedKind::Auth,
    ];
    for k in asks {
        match k {
            ProgramBlockedKind::Permission
            | ProgramBlockedKind::Question
            | ProgramBlockedKind::Auth => {}
        }
        assert!(ASK_WORDS.contains(&k.as_str()), "{:?}", k.as_str());
    }
    assert_eq!(ASK_WORDS.len(), asks.len(), "no ask word the engine lacks");
    assert!(
        aterm_slog::Fields::new()
            .word(aterm_slog::Key::State, "teleporting")
            .is_err()
    );
}

#[test]
fn the_hold_origins_are_the_logs() {
    use aterm_slog::vocab::ORIGIN_WORDS;
    let carried = aterm_gui_fabric::parse_holds(["person a", "fleet b", "local c"]);
    let mut origins: Vec<String> = carried.standing().map(|h| h.origin).collect();
    assert_eq!(origins.len(), 3, "every carried slot stands: {origins:?}");
    origins.push(aterm_gui_fabric::REMOTE_ORIGIN.to_string());
    for o in &origins {
        assert!(ORIGIN_WORDS.contains(&o.as_str()), "{o}");
        assert!(
            aterm_slog::Fields::new()
                .word(aterm_slog::Key::Origin, o)
                .is_ok(),
            "{o}"
        );
    }
    assert_eq!(
        ORIGIN_WORDS.len(),
        origins.len(),
        "no origin word the slots lack: {origins:?}"
    );
    assert!(
        aterm_slog::Fields::new()
            .word(aterm_slog::Key::Origin, "teleport")
            .is_err()
    );
}

#[test]
fn a_spawn_wakes_instance_watchers() {
    use std::time::Duration;
    let mut store = SessionStore::default();
    let watcher = crate::session_watch::Subscription::single(&store.instance_watchers());
    let h = test_handle(3);
    store.register(h.clone());
    assert!(watcher.wait(Duration::ZERO), "a spawn rings the instance");
    store.register(h);
    assert!(!watcher.wait(Duration::ZERO), "a replace rings nobody");
    store.deregister_local_as(3, ExitReason::CtlClose, ExitActor::Unknown);
    assert!(watcher.wait(Duration::ZERO), "a close rings the instance");
}

#[test]
fn a_state_change_wakes_the_sessions_watchers_and_a_re_mark_does_not() {
    use std::time::Duration;
    let mut store = SessionStore::default();
    let h = test_handle(4);
    let watcher = crate::session_watch::Subscription::single(&h.ctx.watchers);
    store.register(h);
    while watcher.wait(Duration::ZERO) {}
    store.set_state(4, SessionState::Alive);
    assert!(!watcher.wait(Duration::ZERO), "a re-mark rings nobody");
    store.set_state(4, SessionState::Exited);
    assert!(watcher.wait(Duration::ZERO), "the exit rings the session");
}

#[test]
fn a_carried_render_debt_round_trips_and_a_bad_row_is_none() {
    let debt = |shift, pushed, cut| RenderDebt { shift, pushed, cut };
    for d in [
        debt(-1, 3, 0),
        debt(-1, 0, 2),
        debt(0, 0, 2),
        debt(4096, -4096, 4096),
    ] {
        let (row, cut) = render_debt_keys(Some(d));
        assert_eq!(parse_render_debt(row.as_deref(), cut), Some(d), "{d:?}");
    }
    assert_eq!(
        render_debt_keys(Some(debt(-1, 0, 3))),
        (Some("-1,0".to_string()), 3),
        "the rows as ever, the columns beside them"
    );
    assert_eq!(render_debt_keys(Some(debt(0, 0, 2))), (None, 2));
    assert_eq!(render_debt_keys(Some(RenderDebt::default())), (None, 0));
    assert_eq!(render_debt_keys(None), (None, 0));

    let rows = |row| parse_render_debt(Some(row), 0);
    assert_eq!(rows("-1,0"), Some(debt(-1, 0, 0)));
    assert_eq!(rows("1"), None);
    assert_eq!(rows("a,b"), None);
    assert_eq!(rows("0,0"), None, "owes nothing");
    assert_eq!(rows("4097,0"), None);
    assert_eq!(rows("-2147483648,0"), None);
    assert_eq!(rows("0,-2147483648"), None);
    assert_eq!(rows("0,0,2"), None, "three fields");
    assert_eq!(parse_render_debt(None, 0), None);
    assert_eq!(parse_render_debt(None, 4097), None);
    assert_eq!(parse_render_debt(Some("1,0"), 4097), None);
    assert_eq!(parse_render_debt(Some("bad"), 2), None, "a bad row is none");
}

#[test]
fn a_carried_journal_key_round_trips_and_a_bad_one_is_none() {
    let key = (u32::MAX, u64::MAX);
    assert_eq!(parse_journal_key(&journal_key_row(key)), Some(key));
    assert_eq!(parse_journal_key("4141:7"), Some((4141, 7)));
    for bad in ["", "4141", "0:7", "a:7", "4141:-1", "4141:7:1", " 4141:7"] {
        assert_eq!(parse_journal_key(bad), None, "{bad:?}");
    }
    let old = SessionHandoff::from_toml(
        "schema = 1\n\n[[sessions]]\nlocal_id = 0\nsid = \"s-0123456789abcdef0123\"\nstate = \
         \"alive\"\ntitle = \"zsh\"\n",
    )
    .expect("an older producer's manifest parses");
    assert_eq!(old.sessions[0].journal_key, None);
}

fn handle(local_id: u64, parent: Option<SessionId>) -> SessionHandle {
    handle_in_state(local_id, parent, SessionState::Alive)
}

fn handle_in_state(local_id: u64, parent: Option<SessionId>, state: SessionState) -> SessionHandle {
    let mut h = handle_alive(local_id, parent, -1);
    h.state = state;
    h
}

#[test]
fn the_frozen_path_carry_reads_absent_as_predates_and_rides_per_record() {
    assert!(
        predates_path_self_heal(None),
        "absent: the old build wrote it"
    );
    assert!(
        !predates_path_self_heal(Some(crate::running_build_number())),
        "a dev build of this tree ({}) wrote the field, so it heals",
        crate::running_build_number()
    );
    assert!(
        !predates_path_self_heal(Some(1_789_468_652)),
        "a number below the anchor with the field present still heals: presence is the test"
    );
    assert!(!predates_path_self_heal(Some(1_789_584_999)));
    assert!(!predates_path_self_heal(Some(1_789_585_000)));
    assert!(
        !predates_path_self_heal(Some(0)),
        "an unnumbered build carries the field"
    );

    let old_wire = "schema = 1

[[sessions]]
local_id = 3
sid = \"s-old\"
state = \"alive\"
title = \"zsh\"
";
    let read = SessionHandoff::from_toml(old_wire).expect("this build reads an old manifest");
    assert_eq!(read.outgoing_build, None);
    assert!(predates_path_self_heal(read.outgoing_build));
    assert!(
        !read.sessions[0].frozen_path,
        "the per-record flag defaults off"
    );

    let mut store = SessionStore::default();
    store.register(handle(0, None));
    store.register(handle(1, None));
    store.mark_frozen_path(1);
    let manifest = SessionHandoff::from_store(&store);
    assert_eq!(manifest.outgoing_build, Some(crate::running_build_number()));
    assert_eq!(
        manifest
            .sessions
            .iter()
            .map(|r| r.frozen_path)
            .collect::<Vec<_>>(),
        vec![false, true]
    );
    let wire = manifest.to_toml().expect("serializes");
    assert!(wire.contains("outgoing_build = "), "{wire}");
    assert_eq!(wire.matches("frozen_path = true").count(), 1, "{wire}");
    assert!(
        !wire.contains("frozen_path = false"),
        "false stays off the wire: {wire}"
    );
    assert!(manifest.roundtrips());

    assert_eq!(store.frozen_path_tabs(), 1);
    store.mark_frozen_path(1);
    assert_eq!(store.frozen_path_tabs(), 1, "idempotent");
    store.note_exit_code(1, Some(0));
    assert_eq!(
        store.frozen_path_tabs(),
        0,
        "an exited shell is not a tab to exec in"
    );
    assert!(store.has_frozen_path(1), "but the fact is still carried");
    store.deregister_local(1);
    assert!(!store.has_frozen_path(1));
    assert_eq!(store.frozen_path_tabs(), 0);
    store.mark_frozen_path(9);
    assert_eq!(store.frozen_path_tabs(), 0);
    store.register(handle(9, None));
    assert_eq!(store.frozen_path_tabs(), 1);
}

#[test]
fn session_handoff_carries_a_hold() {
    use crate::fabric::{HOLD_DENIED, Hold, HoldIssuer, Holds, apply_hold_for_test, cmd_hold};
    let local = Hold {
        reason: "stop%20driving".to_string(),
        origin: "local".to_string(),
    };
    let fleet = Hold {
        reason: "fabric-lost".to_string(),
        origin: "fleet".to_string(),
    };
    let mut store = SessionStore::default();
    let held: [&[&Hold]; 4] = [&[&local], &[&fleet], &[], &[&local, &fleet]];
    let mut before: Vec<Holds> = Vec::new();
    for (local_id, holds) in (0..).zip(held) {
        let h = handle(local_id, None);
        for hold in holds {
            assert!(apply_hold_for_test(&h.ctx, Some((*hold).clone())));
        }
        before.push(h.ctx.fabric.holds());
        store.register(h);
    }
    let manifest = SessionHandoff::from_store(&store);
    let rows: Vec<(Option<&str>, &[String])> = manifest
        .sessions
        .iter()
        .map(|r| (r.hold.as_deref(), r.more_holds.as_slice()))
        .collect();
    let both = ["local stop%20driving".to_string()];
    assert_eq!(
        rows,
        [
            (Some("local stop%20driving"), &[][..]),
            (Some("fleet fabric-lost"), &[][..]),
            (None, &[][..]),
            (Some("fleet fabric-lost"), &both[..]),
        ],
        "every standing hold is drawn into its record, either origin, the fleet's first"
    );
    let wire = manifest.to_toml().expect("serializes");
    assert_eq!(
        (
            wire.matches("hold = ").count(),
            wire.matches("more_holds = ").count()
        ),
        (3, 1),
        "no key for no hold, and the second only beside a first: {wire}"
    );
    assert!(manifest.roundtrips());

    let read = SessionHandoff::from_toml(&wire).expect("the successor reads it");
    let adopted_store: Store = new_store();
    for (rec, before) in read.sessions.iter().zip(&before) {
        let fresh = handle(rec.local_id, None);
        crate::spawn::seed_adopted_fabric(
            &fresh.ctx.fabric,
            &fresh.ctx.timeline,
            &rec.topics,
            &rec.hold_rows(),
            &rec.fabric,
        );
        assert_eq!(&fresh.ctx.fabric.holds(), before, "{rec:?}");
        let refused = crate::fabric::halt_refusal(&fresh.ctx, "send");
        assert_eq!(
            refused.is_some(),
            before.is_held(),
            "the dispatch gate answers exactly as it did before the update"
        );
        let carried_rows = fresh
            .ctx
            .timeline
            .lock()
            .unwrap()
            .since(None)
            .filter(|e| e.kind == "hold" && e.payload.ends_with(" carried=1"))
            .count();
        assert_eq!(carried_rows, usize::from(before.is_held()), "{rec:?}");
        adopted_store.write().unwrap().register(fresh);
    }
    let sid_of = |local_id: u64| {
        adopted_store
            .read()
            .unwrap()
            .by_local(local_id)
            .map(|h| h.sid.as_str().to_string())
            .expect("registered")
    };
    let owner_off = |local_id: u64| {
        cmd_hold(
            &adopted_store,
            &format!("{} off", sid_of(local_id)),
            HoldIssuer::Owner,
        )
    };
    assert_eq!(
        owner_off(1),
        HOLD_DENIED,
        "a carried FLEET hold is not the owner's to lift"
    );
    assert_eq!(
        owner_off(0),
        "OK hold=0\n",
        "a carried LOCAL hold is, as it was"
    );
    assert_eq!(
        owner_off(3),
        "OK hold=1\n",
        "beside a carried fleet hold the owner lifts its own, and the fleet's stands"
    );

    let old = SessionHandoff::from_toml(
        "schema = 1\n\n[[sessions]]\nlocal_id = 3\nsid = \"s-old\"\nstate = \"alive\"\ntitle = \"zsh\"\n",
    )
    .expect("an old manifest reads");
    assert_eq!(old.sessions[0].hold_rows(), Vec::<String>::new());
    let one = SessionHandoff::from_toml(
        "schema = 1\n\n[[sessions]]\nlocal_id = 3\nsid = \"s-old\"\nstate = \"alive\"\ntitle = \"zsh\"\nhold = \"local stop%20driving\"\n",
    )
    .expect("a one-hold manifest reads");
    let fresh = handle(3, None);
    crate::spawn::seed_adopted_fabric(
        &fresh.ctx.fabric,
        &fresh.ctx.timeline,
        &[],
        &one.sessions[0].hold_rows(),
        &one.sessions[0].fabric,
    );
    assert_eq!(fresh.ctx.fabric.holds(), before[0], "one hold, that origin");
}

#[test]
fn an_unreadable_carried_hold_fails_closed() {
    use crate::fabric::{CARRIED_UNREADABLE, Hold, parse_hold, render_hold};
    let held = |reason: &str, origin: &str| Hold {
        reason: reason.to_string(),
        origin: origin.to_string(),
    };
    for (row, want) in [
        ("local stop%20now", held("stop%20now", "local")),
        ("fleet fabric-lost", held("fabric-lost", "fleet")),
        ("fleet -", held("-", "fleet")),
        (
            &*format!("local {}", "r".repeat(400)),
            held(&"r".repeat(128), "local"),
        ),
        ("local a%2", held("a", "local")),
        ("", held(CARRIED_UNREADABLE, "local")),
        ("fleet", held(CARRIED_UNREADABLE, "fleet")),
        ("fleet a b", held(CARRIED_UNREADABLE, "fleet")),
        ("mars now", held(CARRIED_UNREADABLE, "local")),
        ("local ", held(CARRIED_UNREADABLE, "local")),
        (" fleet x", held(CARRIED_UNREADABLE, "local")),
    ] {
        assert_eq!(parse_hold(row), want, "{row:?}");
    }
    for hold in [held("stop%20now", "local"), held("fabric-lost", "fleet")] {
        assert_eq!(parse_hold(&render_hold(&hold)), hold, "round trip");
    }
    let odd = SessionHandoff::from_toml(
        "schema = 1\n\n[[sessions]]\nlocal_id = 3\nsid = \"s-odd\"\nstate = \"alive\"\ntitle = \"zsh\"\nhold = 5\n",
    )
    .expect("a hold of the wrong type does not cost the manifest");
    assert_eq!(odd.sessions[0].hold.as_deref(), Some(""));
    assert_eq!(parse_hold(""), held(CARRIED_UNREADABLE, "local"));
    for (more, want) in [
        ("[\"local mine\", 5]", vec!["local mine", ""]),
        ("5", vec![""]),
        ("[]", vec![]),
    ] {
        let odd = SessionHandoff::from_toml(&format!(
            "schema = 1\n\n[[sessions]]\nlocal_id = 3\nsid = \"s-odd\"\nstate = \"alive\"\ntitle = \"zsh\"\nhold = \"fleet stop\"\nmore_holds = {more}\n",
        ))
        .expect("more_holds of the wrong type does not cost the manifest");
        assert_eq!(odd.sessions[0].more_holds, want, "{more}");
    }
}

#[test]
fn a_ttl_claim_and_keyed_attention_cross_the_handoff() {
    use crate::session_timeline::{
        MetaEdit, claim_supervisor, parse_carried_attention, parse_carried_supervisor,
        seed_carried_claims, write_attention_owned, write_server_attention,
    };
    let now = crate::metrics::now_us();
    let mut store = SessionStore::default();
    let s0 = handle(0, None);
    claim_supervisor(&s0.ctx, "watch-bob", None, Some(now + 60_000_000), now).unwrap();
    for (owner, text) in [
        ("-", "check the build"),
        ("supervisor", "claude box (Bash: rm -rf x) — needs you"),
        ("ci@host", "red 100%"),
    ] {
        write_attention_owned(&s0.ctx, owner, MetaEdit::Set(text), aterm_slog::Actor::Ctl).unwrap();
    }
    crate::session_timeline::write_attention_kind(
        &s0.ctx,
        "ci@host",
        MetaEdit::Set("red 100%"),
        Some(aterm_types::needs::AttentionKind::Wait),
        aterm_slog::Actor::Ctl,
    )
    .unwrap();
    assert!(write_server_attention(
        &s0.ctx,
        crate::input_stall::SERVER_ATTENTION_OWNER,
        Some("claude has not read 3 keys".to_string())
    ));
    store.register(s0);
    let s1 = handle(1, None);
    claim_supervisor(&s1.ctx, "relay-x", Some(7), None, now).unwrap();
    store.register(s1);
    let s2 = handle(2, None);
    claim_supervisor(
        &s2.ctx,
        &crate::harness_host::holder(),
        None,
        Some(now + 90_000_000),
        now,
    )
    .unwrap();
    store.register(s2);
    store.register(handle(3, None));
    let s4 = handle(4, None);
    claim_supervisor(&s4.ctx, "gone", None, Some(now.saturating_sub(1)), now).unwrap();
    store.register(s4);

    let manifest = SessionHandoff::from_store(&store);
    let r = &manifest.sessions;
    let (holder, ms) = r[0]
        .supervisor
        .as_deref()
        .and_then(|row| row.split_once(' '))
        .expect("s0's lease is carried");
    let ms: u64 = ms.parse().unwrap();
    assert_eq!(holder, "watch-bob");
    assert!((50_000..=60_000).contains(&ms), "{ms}");
    assert!(r[0].claim_known);
    assert_eq!(
        r[0].attention_owners,
        [
            "- check%20the%20build",
            "supervisor claude%20box%20(Bash:%20rm%20-rf%20x)%20%E2%80%94%20needs%20you",
            "ci@host kind=wait red%20100%25",
        ],
        "every owner's line in write order, with the kind its writer named, \
         the server's own left behind"
    );
    let claims: Vec<(Option<&str>, bool, bool)> = r
        .iter()
        .map(|rec| {
            (
                rec.supervisor.as_deref(),
                rec.claim_known,
                rec.claim_grace(),
            )
        })
        .collect();
    assert_eq!(
        claims[1..],
        [
            (None, false, true),
            (None, true, false),
            (None, true, false),
            (None, true, false)
        ],
        "a connection's claim is unvouched; our own host's, none and a lapsed \
         lease are vouched for"
    );
    assert!(r[0].claim_grace(), "a carried lease is waited on too");
    let wire = manifest.to_toml().expect("serializes");
    assert_eq!(wire.matches("\nsupervisor = ").count(), 1, "{wire}");
    assert_eq!(wire.matches("\nclaim_known = true").count(), 4, "{wire}");
    assert!(manifest.roundtrips());

    let read = SessionHandoff::from_toml(&wire).expect("the successor reads it");
    let later = now + 2_000_000;
    let adopted: Vec<SessionHandle> = read
        .sessions
        .iter()
        .map(|rec| {
            let fresh = handle(rec.local_id, None);
            seed_carried_claims(
                &fresh.ctx.meta,
                &fresh.ctx.timeline,
                rec.supervisor.as_deref(),
                &rec.attention_owners,
                later,
            );
            fresh
        })
        .collect();
    {
        let meta = adopted[0].ctx.meta.lock().unwrap();
        assert_eq!(meta.live_supervisor(later), Some("watch-bob"));
        let expiry = meta.supervisor_expiry().expect("a lease");
        assert_eq!(
            expiry,
            later + ms * 1000,
            "its remaining time, from the seed"
        );
        assert_eq!(meta.attention.as_deref(), Some("red 100%"));
        assert_eq!(
            meta.attention_owners.effective_owner(),
            Some("ci@host"),
            "the most recent line is the one shown, as before the update"
        );
        assert_eq!(
            meta.attention_owners.get("supervisor"),
            Some("claude box (Bash: rm -rf x) — needs you")
        );
        assert_eq!(meta.attention_owners.get("-"), Some("check the build"));
        assert_eq!(
            meta.attention_owners.kinds_field(),
            "ask:-,-:supervisor,wait:ci@host",
            "a wait is still a wait in the successor"
        );
        assert_eq!(
            meta.attention_owners
                .get(crate::input_stall::SERVER_ATTENTION_OWNER),
            None
        );
        assert_eq!(meta.driver_writes, 0, "a seed is no driver's write");
    }
    assert_eq!(
        claim_supervisor(
            &adopted[0].ctx,
            "aterm-harness@1",
            None,
            Some(later + 90_000_000),
            later
        ),
        Err("watch-bob".to_string())
    );
    assert_eq!(
        claim_supervisor(
            &adopted[0].ctx,
            "watch-bob",
            None,
            Some(later + 90_000_000),
            later
        ),
        Ok(false)
    );
    let seeded_rows: Vec<String> = adopted[0]
        .ctx
        .timeline
        .lock()
        .unwrap()
        .since(None)
        .filter(|e| e.kind == "meta-change")
        .map(|e| e.payload.to_string())
        .collect();
    assert_eq!(
        seeded_rows,
        [
            "field=attention value=red%20100%25",
            "field=supervisor value=watch-bob"
        ]
    );
    for fresh in &adopted[1..] {
        let meta = fresh.ctx.meta.lock().unwrap();
        assert_eq!(meta.live_supervisor(later), None, "nothing else is seeded");
        assert!(meta.attention_owners.is_empty());
    }

    let old = SessionHandoff::from_toml(
        "schema = 1\n\n[[sessions]]\nlocal_id = 3\nsid = \"s-old\"\nstate = \"alive\"\ntitle = \"zsh\"\n",
    )
    .expect("an old manifest reads");
    assert_eq!(old.sessions[0].supervisor, None);
    assert!(old.sessions[0].attention_owners.is_empty());
    assert!(old.sessions[0].claim_grace());

    for (row, want) in [
        ("watch-bob 5000", Some(("watch-bob", 5000))),
        ("watch-bob 99999999", Some(("watch-bob", 600_000))),
        ("watch-bob 0", None),
        ("watch-bob -5", None),
        ("watch-bob soon", None),
        ("- 5000", None),
        ("bad holder! 5000", None),
        ("", None),
        ("watch-bob", None),
        (" watch-bob 5", None),
    ] {
        assert_eq!(parse_carried_supervisor(row), want, "{row:?}");
    }
    use aterm_types::needs::AttentionKind::{Ask, Wait};
    for (row, want) in [
        ("ci red", Some(("ci", None, "red".to_string()))),
        (
            "ci x%1B%5B31mred%E2%80%AEx",
            Some(("ci", None, "x[31mredx".to_string())),
        ),
        (
            "ci kind=wait limited:%20resets%207pm",
            Some(("ci", Some(Wait), "limited: resets 7pm".to_string())),
        ),
        (
            "ci kind=ask red",
            Some(("ci", Some(Ask), "red".to_string())),
        ),
        ("ci kind=later red", Some(("ci", None, "red".to_string()))),
        ("- kind=wait red", Some(("-", None, "red".to_string()))),
        ("ci kind=wait", Some(("ci", None, "kind=wait".to_string()))),
        ("aterm stalled", None),
        ("bad%20owner text", None),
        ("ci ", None),
        ("ci %0A%09", None),
        ("ci kind=wait %0A", None),
        ("ci", None),
    ] {
        assert_eq!(parse_carried_attention(row), want, "{row:?}");
    }
    let (long, bare) = (
        format!("ci {}", "a".repeat(400)),
        format!("- {}", "a".repeat(400)),
    );
    assert_eq!(
        parse_carried_attention(&long).unwrap().2.len(),
        crate::session_timeline::META_ATTENTION_KEYED_MAX
    );
    assert_eq!(
        parse_carried_attention(&bare).unwrap().2.len(),
        crate::session_timeline::META_ATTENTION_MAX
    );
    let crowded = handle(9, None);
    let rows: Vec<String> = (0..12).map(|i| format!("owner{i} line{i}")).collect();
    seed_carried_claims(&crowded.ctx.meta, &crowded.ctx.timeline, None, &rows, later);
    assert_eq!(
        crowded.ctx.meta.lock().unwrap().attention_owners.len(),
        crate::session_timeline::ATTENTION_OWNERS_MAX - 1,
        "the bare owner's slot is kept"
    );
    let odd = SessionHandoff::from_toml(
        "schema = 1\n\n[[sessions]]\nlocal_id = 3\nsid = \"s-odd\"\nstate = \"alive\"\n\
         title = \"zsh\"\nsupervisor = 5\nclaim_known = \"yes\"\nattention_owners = [1, \"ci x\"]\n",
    )
    .expect("rows of the wrong type do not cost the manifest");
    assert_eq!(odd.sessions[0].supervisor.as_deref(), Some(""));
    assert!(!odd.sessions[0].claim_known);
    assert_eq!(odd.sessions[0].attention_owners, ["ci x"]);
    assert!(odd.sessions[0].claim_grace());
    let not_a_list = SessionHandoff::from_toml(
        "schema = 1\n\n[[sessions]]\nlocal_id = 3\nsid = \"s-odd\"\nstate = \"alive\"\n\
         title = \"zsh\"\nattention_owners = \"ci x\"\nclaim_known = true\n",
    )
    .expect("a list that is no list does not cost the manifest");
    assert!(not_a_list.sessions[0].attention_owners.is_empty());
    assert!(!not_a_list.sessions[0].claim_grace());
}

#[test]
fn the_real_claim_carry_conforms_to_the_handoff_claim_model() {
    use crate::session_timeline::{claim_supervisor, seed_carried_claims};
    let model = aterm_models_harness::harness_handoff_claim_model();
    let buggy = aterm_spec::interp::with_buggy(&model, 1);
    let grace = model.consts.iter().find(|c| c.0 == "Grace").unwrap().1;
    let new_keys = ["supervisor = ", "claim_known = ", "attention_owners = "];
    let mut driven = 0;
    for pre in ["", "ExternalLease", "ExternalConn", "HostHeld"] {
        for older in [false, true] {
            let mut state = model.init_state();
            let mut schedule: Vec<&str> = [pre].into_iter().filter(|a| !a.is_empty()).collect();
            if older {
                schedule.push("OlderParent");
            }
            schedule.extend(["Park", "Commit"]);
            for action in &schedule {
                assert!(model.fire(action, &mut state), "{action}: {state:?}");
            }

            let now = crate::metrics::now_us();
            let live = handle(0, None);
            match pre {
                "ExternalLease" => {
                    claim_supervisor(&live.ctx, "watch-bob", None, Some(now + 60_000_000), now)
                        .unwrap();
                }
                "ExternalConn" => {
                    claim_supervisor(&live.ctx, "relay-x", Some(7), None, now).unwrap();
                }
                "HostHeld" => {
                    claim_supervisor(
                        &live.ctx,
                        &crate::harness_host::holder(),
                        None,
                        Some(now + 90_000_000),
                        now,
                    )
                    .unwrap();
                }
                _ => {}
            }
            let mut store = SessionStore::default();
            store.register(live);
            let mut wire = SessionHandoff::from_store(&store)
                .to_toml()
                .expect("serializes");
            if older {
                wire = wire
                    .lines()
                    .filter(|line| !new_keys.iter().any(|key| line.starts_with(key)))
                    .map(|line| format!("{line}\n"))
                    .collect();
            }
            let read = SessionHandoff::from_toml(&wire).expect("the successor reads it");
            let rec = &read.sessions[0];
            let fresh = handle(0, None);
            let later = now + 1_000_000;
            seed_carried_claims(
                &fresh.ctx.meta,
                &fresh.ctx.timeline,
                rec.supervisor.as_deref(),
                &rec.attention_owners,
                later,
            );
            let shown = fresh
                .ctx
                .meta
                .lock()
                .unwrap()
                .live_supervisor(later)
                .map(str::to_owned);
            let server = match shown.as_deref() {
                None => 0,
                Some(holder) if holder == crate::harness_host::holder() => 2,
                Some(_) => 1,
            };
            let real_grace = if rec.claim_grace() { grace } else { 0 };
            assert_eq!(
                (server, real_grace),
                (state["server"], state["grace"]),
                "{schedule:?}: the successor's state is the model's after Commit\n{wire}"
            );
            let host_may = claim_supervisor(
                &fresh.ctx,
                "aterm-harness@2",
                None,
                Some(later + 90_000_000),
                later,
            )
            .is_ok()
                && !rec.claim_grace();
            assert_eq!(
                host_may,
                model.action_enabled("HostClaims", &state),
                "{schedule:?}"
            );

            let mut shipped = buggy.init_state();
            for action in schedule.iter().map(|a| {
                if *a == "Commit" {
                    "CommitUncarried"
                } else {
                    *a
                }
            }) {
                assert!(buggy.fire(action, &mut shipped), "{action}: {shipped:?}");
            }
            assert_eq!((shipped["server"], shipped["grace"]), (0, 0));
            assert!(buggy.fire("HostClaims", &mut shipped));
            assert_eq!(
                buggy.check_invariant("NoTakeover", &shipped),
                matches!(pre, "" | "HostHeld"),
                "{schedule:?}: the old successor takes exactly the externally held sessions"
            );
            driven += 1;
        }
    }
    assert_eq!(driven, 8, "every pre-Commit configuration of the machine");
}

#[test]
fn a_retired_identity_field_is_read_past() {
    let old_wire = "schema = 1

[[sessions]]
local_id = 3
sid = \"s-old\"
state = \"alive\"
title = \"zsh\"
identity = \"worker\"
";
    let read = SessionHandoff::from_toml(old_wire).expect("this build reads an old manifest");
    assert_eq!(read.sessions.len(), 1);
    assert_eq!(read.sessions[0].local_id, 3);
    assert_eq!(read.sessions[0].sid, "s-old");
    let wire = read.to_toml().expect("serializes");
    assert!(!wire.contains("identity ="), "{wire}");
    let back = SessionHandoff::from_toml(&wire).expect("round-trips");
    assert_eq!(back, read);
}

#[cfg(unix)]
#[test]
fn the_task_record_rides_per_record_and_absent_reads_none() {
    use std::time::Duration;

    use crate::task_record::{
        MarkFenced, TaskMark, TaskState, mark_session_task, mark_session_task_within, seed_carried,
    };
    let old_wire = "schema = 1

[[sessions]]
local_id = 3
sid = \"s-old\"
state = \"alive\"
title = \"zsh\"
";
    let read = SessionHandoff::from_toml(old_wire).expect("this build reads an old manifest");
    assert_eq!(read.sessions[0].task, None, "absent reads None");
    let typo = SessionHandoff::from_toml(&format!("{old_wire}task = 5\n"))
        .expect("a wrong-typed row costs the row, never the manifest");
    let unread = handle(7, None);
    seed_carried(&unread.ctx, typo.sessions[0].task.as_deref());
    assert!(
        unread.ctx.timeline.lock().unwrap().task().is_none(),
        "and seeds nothing"
    );

    let mut store = SessionStore::default();
    store.register(handle(0, None));
    let worker = handle(1, None);
    mark_session_task(&worker.ctx, 1, &TaskMark::new(TaskState::Working)).expect("unfenced");
    let folder = "/Users//me/work proj";
    worker
        .ctx
        .timeline
        .lock()
        .unwrap()
        .set_task_worker(aterm_phase::Program::Agy, Some(folder.to_string()));
    let mut done = TaskMark::new(TaskState::Done);
    done.why = Some("marker");
    done.result_bytes = 5;
    let (record, _) = mark_session_task(&worker.ctx, 1, &done).expect("unfenced");
    store.register(worker.clone());
    let manifest = SessionHandoff::from_store(&store);
    assert_eq!(manifest.sessions[0].task, None);
    let row = manifest.sessions[1].task.clone().expect("the worker's row");
    assert!(row.starts_with("rev=2 state=done from=ctl since="), "{row}");
    assert!(
        row.ends_with(&format!(
            " agent=agy trusted_cwd={}",
            crate::control::pct_encode(folder)
        )),
        "a mark keeps the agent and the trusted folder: {row}"
    );
    assert_eq!(record.agent, Some(aterm_phase::Program::Agy));
    assert_eq!(record.trusted_cwd.as_deref(), Some(folder));
    let wire = manifest.to_toml().expect("serializes");
    assert_eq!(
        wire.matches("\ntask = ").count(),
        1,
        "None stays off the wire: {wire}"
    );
    assert!(manifest.roundtrips());
    let back = SessionHandoff::from_toml(&wire).expect("round-trips");
    assert_eq!(back.sessions[1].task.as_deref(), Some(row.as_str()));

    let now = crate::metrics::now_us();
    let holder = crate::harness_host::holder();
    let drawn = CarriedClaims::of_record(&manifest.sessions[1]);
    // The Commit draws again later: the record's ages have grown, its claims have not.
    std::thread::sleep(Duration::from_millis(2));
    assert_eq!(
        CarriedClaims::of_session(1, &worker.ctx, now, &holder),
        drawn,
        "a record that stood still since the park holds the Commit"
    );

    let adopted = handle(8, None);
    seed_carried(&adopted.ctx, back.sessions[1].task.as_deref());
    let seeded = adopted.ctx.timeline.lock().unwrap().task().cloned();
    let seeded = seeded.expect("seeded");
    assert_eq!(
        crate::task_record::TaskRecord {
            since: record.since,
            started: record.started,
            carried_entered: None,
            ..seeded
        },
        record,
        "the same record, its since re-anchored"
    );
    let seeded_folder = |h: &SessionHandle| {
        h.ctx
            .timeline
            .lock()
            .unwrap()
            .task()
            .and_then(|t| t.trusted_cwd.clone())
    };
    assert_eq!(seeded_folder(&adopted), Some(folder.to_string()));
    let carried = back.sessions[1].task.as_deref().expect("the row");
    let encoded = crate::control::pct_encode(folder);
    for bad in [
        "work".to_string(),
        crate::control::pct_encode("/Users//me/../etc"),
        crate::control::pct_encode("/Users//me/"),
        "/Users//me/a%0Ab".to_string(),
    ] {
        let other = handle(10, None);
        seed_carried(&other.ctx, Some(&carried.replace(&encoded, &bad)));
        let seeded = other.ctx.timeline.lock().unwrap().task().cloned();
        let seeded = seeded.unwrap_or_else(|| panic!("{bad}: the record crosses"));
        assert_eq!(seeded.trusted_cwd, None, "{bad}");
        assert_eq!(seeded.agent, Some(aterm_phase::Program::Agy), "{bad}");
    }
    let other = handle(11, None);
    seed_carried(
        &other.ctx,
        Some(&carried.replace(&format!(" trusted_cwd={encoded}"), "")),
    );
    assert!(other.ctx.timeline.lock().unwrap().task().is_some());
    assert_eq!(seeded_folder(&other), None, "a row from before the field");
    let raised = back.sessions[1]
        .task
        .as_deref()
        .expect("the row")
        .replace(" attention=0", " attention=1");
    let unclaimed = handle(9, None);
    seed_carried(&unclaimed.ctx, Some(&raised));
    let seeded = unclaimed.ctx.timeline.lock().unwrap().task().cloned();
    assert!(!seeded.expect("seeded").attention, "no entry, no flag");

    mark_session_task(&worker.ctx, 1, &TaskMark::new(TaskState::Stalled)).expect("unfenced");
    assert_ne!(
        CarriedClaims::of_session(1, &worker.ctx, now, &holder),
        drawn,
        "a mark after the park's draw stands the Commit down"
    );
    let fenced = CarriedClaims::of_session(1, &worker.ctx, now, &holder);
    let _ = worker.ctx.fabric.fence_hold(false);
    let done = TaskMark::new(TaskState::Done);
    assert_eq!(
        mark_session_task_within(&worker.ctx, 1, &done, Duration::ZERO),
        Err(MarkFenced)
    );
    assert_eq!(
        CarriedClaims::of_session(1, &worker.ctx, now, &holder),
        fenced
    );
    worker.ctx.fabric.lift_hold_fence();
    let landed = mark_session_task_within(&worker.ctx, 1, &done, Duration::ZERO);
    assert!(landed.is_ok_and(|(_, moved)| moved), "lifted, it lands");
}

#[test]
fn the_rekey_channel_rides_per_record_and_absent_reads_false() {
    let old_wire = "schema = 1

[[sessions]]
local_id = 3
sid = \"s-old\"
state = \"alive\"
title = \"zsh\"
";
    let read = SessionHandoff::from_toml(old_wire).expect("this build reads an old manifest");
    assert!(!read.sessions[0].rekey, "absent reads false");

    let mut store = SessionStore::default();
    store.register(handle(0, None));
    store.register(handle(1, None));
    store.mark_rekey_channel(1);
    let manifest = SessionHandoff::from_store(&store);
    assert_eq!(
        manifest
            .sessions
            .iter()
            .map(|r| r.rekey)
            .collect::<Vec<_>>(),
        vec![false, true]
    );
    let wire = manifest.to_toml().expect("serializes");
    assert_eq!(
        wire.matches("rekey").count(),
        1,
        "false stays off the wire: {wire}"
    );
    assert!(manifest.roundtrips());
    let back = SessionHandoff::from_toml(&wire).expect("round-trips");
    assert!(back.sessions[1].rekey);
    store.deregister_local(1);
    assert!(!store.has_rekey_channel(1), "a closed session forgets it");
}

#[test]
fn the_loader_rides_per_record_and_absent_reads_false() {
    let old_wire = "schema = 1

[[sessions]]
local_id = 3
sid = \"s-old\"
state = \"alive\"
title = \"zsh\"
";
    let read = SessionHandoff::from_toml(old_wire).expect("this build reads an old manifest");
    assert!(!read.sessions[0].loader, "absent reads false");

    let mut store = SessionStore::default();
    store.register(handle(0, None));
    store.register(handle(1, None));
    store.mark_body_loader(1);
    store.mark_integration_frozen(0);
    let manifest = SessionHandoff::from_store(&store);
    assert_eq!(
        manifest
            .sessions
            .iter()
            .map(|r| r.loader)
            .collect::<Vec<_>>(),
        vec![false, true]
    );
    let wire = manifest.to_toml().expect("serializes");
    assert_eq!(
        wire.matches("loader").count(),
        1,
        "false stays off the wire: {wire}"
    );
    assert!(manifest.roundtrips());
    let back = SessionHandoff::from_toml(&wire).expect("round-trips");
    assert!(back.sessions[1].loader);
    assert!(store.is_integration_frozen(0));
    store.deregister_local(1);
    store.deregister_local(0);
    assert!(!store.has_body_loader(1), "a closed session forgets it");
    assert!(!store.is_integration_frozen(0), "and the frozen mark");
}

#[test]
fn register_indexes_both_key_spaces_and_deregister_clears_both() {
    let mut store = SessionStore::default();
    let h = handle(7, None);
    let sid = h.sid.clone();
    store.register(h);

    assert_eq!(store.len(), 1);
    assert!(store.by_sid(&sid).is_some(), "resolvable by sid");
    assert!(store.by_local(7).is_some(), "resolvable by local id");
    assert_eq!(store.by_local(7).unwrap().sid, sid, "both keys agree");

    assert_eq!(store.deregister_local(7), Some(sid.clone()));
    assert!(store.by_sid(&sid).is_none(), "sid index cleared");
    assert!(store.by_local(7).is_none(), "local index cleared");
    assert!(store.is_empty());
    assert_eq!(store.deregister_local(7), None);
}

#[test]
fn snapshot_is_sorted_by_local_id_and_carries_parent() {
    let mut store = SessionStore::default();
    let root = handle(0, None);
    let root_sid = root.sid.clone();
    store.register(root);
    store.register(handle(2, Some(root_sid.clone())));
    store.register(handle(1, Some(root_sid.clone())));

    let snap = store.snapshot();
    assert_eq!(
        snap.iter().map(|h| h.local_id).collect::<Vec<_>>(),
        vec![0, 1, 2]
    );
    assert_eq!(snap[0].parent, None, "root has no parent");
    assert_eq!(
        snap[1].parent.as_ref(),
        Some(&root_sid),
        "child links to root"
    );
}

#[test]
fn session_handoff_carries_additive_resource_identity_without_changing_sessions() {
    let mut store = SessionStore::default();
    store.register(handle(0, None));
    let mut manifest = SessionHandoff::from_store(&store);
    manifest.resources = Some(aterm_json::json!({
        "boot": vec![7; 16],
        "jobs": [{ "pid": 42, "start": 123456789, "session": "s-a", "ai": "codex",
                   "label": "tree:compiler", "members": [[42, 123456789], [43, 123456790]] }]
    }));
    let parsed = SessionHandoff::from_toml(&manifest.to_toml().unwrap()).unwrap();
    assert_eq!(parsed.resources, manifest.resources);
    assert_eq!(parsed.sessions, manifest.sessions);
    manifest.resources = None;
    assert!(
        SessionHandoff::from_toml(&manifest.to_toml().unwrap())
            .unwrap()
            .resources
            .is_none()
    );
}

#[test]
fn session_handoff_roundtrips_the_whole_set() {
    let mut store = SessionStore::default();
    let root = handle(0, None);
    let root_sid = root.sid.clone();
    store.register(root);
    let peer = handle_in_state(2, Some(root_sid.clone()), SessionState::Alive);
    let peer_sid = peer.sid.clone();
    let peer_ctx = peer.ctx.clone();
    store.register(peer);
    store.register(handle_in_state(
        1,
        Some(root_sid.clone()),
        SessionState::Exited,
    ));
    store.set_title(1, "vim");
    {
        let mut edges = peer_ctx.edges.lock().unwrap();
        let minted = edges.grant_connection(
            &root_sid,
            &peer_sid,
            aterm_session::ConnectionKind::Both,
            &peer_ctx.nonce,
        );
        assert_eq!(minted.len(), 3, "both mints its exact op set");
    }

    let manifest = SessionHandoff::from_store(&store);
    assert_eq!(manifest.sessions.len(), 3, "every live session is packed");
    let carry = |op: &str| ConnectionCarry {
        src: root_sid.as_str().to_string(),
        dst: peer_sid.as_str().to_string(),
        op: op.to_string(),
    };
    assert_eq!(
        manifest.connections,
        vec![carry("read-screen"), carry("signal"), carry("write-input")],
        "the three rows one `both` mints, tokenless and sorted"
    );
    assert!(
        manifest.roundtrips(),
        "serialize -> parse must be the identity"
    );

    let text = manifest.to_toml().unwrap();
    assert!(
        !text
            .as_bytes()
            .windows(64)
            .any(|w| w.iter().all(u8::is_ascii_hexdigit)),
        "no bearer-token-width hex blob may ride the manifest: {text}"
    );
    let restored = SessionHandoff::from_toml(&text).expect("parse");
    assert_eq!(restored, manifest);
    assert_eq!(
        restored.connections, manifest.connections,
        "the tokenless triples round-trip verbatim"
    );
    assert_eq!(
        restored
            .sessions
            .iter()
            .map(|r| r.local_id)
            .collect::<Vec<_>>(),
        vec![0, 1, 2],
        "tab order preserved"
    );
    assert_eq!(
        restored.sessions[1].parent.as_deref(),
        Some(root_sid.as_str())
    );
    assert_eq!(restored.sessions[1].state, "exited");
    assert_eq!(restored.sessions[1].title, "vim");
    assert_eq!(
        SessionState::from_str(&restored.sessions[1].state),
        SessionState::Exited
    );

    assert!(SessionHandoff::from_toml("not = [valid").is_none());
    assert!(SessionHandoff::from_toml("schema = 999\nsessions = []").is_none());
}

#[test]
fn mark_alive_is_monotonic_idempotent_and_fail_safe() {
    let mut store = SessionStore::default();
    store.register(handle_in_state(8, None, SessionState::Spawning));

    assert!(store.mark_alive(8));
    assert!(!store.mark_alive(8), "already Alive: no second transition");
    assert_eq!(store.by_local(8).unwrap().state, SessionState::Alive);

    store.register(handle_in_state(9, None, SessionState::Exited));
    assert!(!store.mark_alive(9), "Exited never flips back to Alive");
    assert_eq!(store.by_local(9).unwrap().state, SessionState::Exited);

    assert!(!store.mark_alive(404));
}

#[test]
fn store_mutators_record_ordered_timeline_events() {
    let mut store = SessionStore::default();
    let h = handle_in_state(4, None, SessionState::Spawning);
    let timeline = h.ctx.timeline.clone();
    store.register(h.clone());
    store.register(h);
    assert!(store.mark_alive(4));
    assert!(!store.mark_alive(4), "no-op re-mark records nothing");
    store.set_title(4, "vim main.rs");
    assert!(timeline.lock().unwrap().record_title(
        "vim main.rs",
        crate::session_log::observed::TitleStands::Own
    ));
    assert!(
        !timeline.lock().unwrap().record_title(
            "vim main.rs",
            crate::session_log::observed::TitleStands::Own
        ),
        "unchanged: nothing recorded"
    );
    store.set_state(4, SessionState::Exited);
    store.set_state(4, SessionState::Exited);
    store.deregister_local(4);

    let rows: Vec<_> = timeline.lock().unwrap().since(None).collect();
    let got: Vec<(u64, &str, &str)> = rows.iter().map(|e| (e.id, e.kind, &*e.payload)).collect();
    assert_eq!(
        got,
        vec![
            (1, "spawned", "state=spawning"),
            (2, "state-change", "state=alive"),
            (3, "title-change", "title=vim%20main.rs"),
            (4, "state-change", "state=exited"),
            (5, "closing", "reason=shell-exit by=-"),
            (6, "state-change", "state=closed"),
        ],
        "one ordered, monotonic-id event per ACTUAL lifecycle change"
    );
}

#[test]
fn roster_journal_mirrors_membership_exactly() {
    let mut store = SessionStore::default();
    assert_eq!(store.roster_seq(), 0, "a virgin store has no history");
    assert_eq!(store.roster_low_seq(), None);
    assert_eq!(store.roster_since(0).count(), 0);

    let replay = |st: &SessionStore| -> std::collections::HashSet<String> {
        let mut set = std::collections::HashSet::new();
        for r in st.roster_since(0) {
            match r.change {
                RosterChange::Created => {
                    set.insert(r.sid.clone());
                }
                RosterChange::Exited => {
                    set.remove(&r.sid);
                }
                RosterChange::RetireRequested | RosterChange::Bye(_) => {}
                #[cfg(unix)]
                RosterChange::QuitCloses => {}
            }
        }
        set
    };

    let a = handle(1, None);
    let b = handle(2, None);
    let (sid_a, sid_b) = (a.sid.as_str().to_string(), b.sid.as_str().to_string());
    store.register(a.clone());
    assert_eq!(replay(&store), store.live_sids());
    store.register(a);
    assert_eq!(store.roster_seq(), 1, "a replace journals nothing");
    store.register(b);
    assert_eq!(replay(&store), store.live_sids());
    store.deregister_local(1);
    assert_eq!(store.deregister_local(1), None);
    assert_eq!(
        store.roster_seq(),
        3,
        "the duplicate deregister journalled nothing"
    );
    assert_eq!(replay(&store), store.live_sids());

    let got: Vec<(u64, &str, RosterChange)> = store
        .roster_since(0)
        .map(|r| (r.seq, r.sid.as_str(), r.change))
        .collect();
    assert_eq!(
        got,
        vec![
            (1, sid_a.as_str(), RosterChange::Created),
            (2, sid_b.as_str(), RosterChange::Created),
            (3, sid_a.as_str(), RosterChange::Exited),
        ]
    );
    assert_eq!(store.roster_since(1).count(), 2);
    assert_eq!(store.roster_since(3).count(), 0);
    assert_eq!(store.roster_since(99).count(), 0, "past the high = nothing");
    assert_eq!(store.roster_low_seq(), Some(1));
}

/// `bridge_roster` emits each row's local id and nonce, so a replace that changes either moves
/// the count a bridge holds its roster by (`sessions bridge`'s and `outbox`'s `roster=`),
/// journals nothing, and the next record reads on above it.
#[test]
fn a_replace_that_changes_a_roster_row_moves_the_count() {
    let mut store = SessionStore::default();
    let mut row = handle(1, None);
    store.register(row.clone());
    assert_eq!(store.roster_seq(), 1);

    store.register(row.clone());
    assert_eq!(store.roster_seq(), 1, "the same row again moves nothing");

    row.nonce = aterm_session::LaunchNonce::generate();
    store.register(row.clone());
    assert_eq!(store.roster_seq(), 2, "a new nonce");
    assert_eq!(store.bridge_roster()[0].2, row.nonce);

    row.local_id = 7;
    store.register(row.clone());
    assert_eq!(store.roster_seq(), 3, "a new local id");
    assert_eq!(store.bridge_roster()[0].0, 7);

    assert_eq!(store.roster_since(0).count(), 1, "neither is a record");
    store.register(handle(2, None));
    assert_eq!(store.roster_seq(), 4);
    let seqs: Vec<u64> = store.roster_since(0).map(|r| r.seq).collect();
    assert_eq!(seqs, vec![1, 4], "the next record reads on above the hole");
    assert_eq!(
        store.roster_since(1).count(),
        1,
        "a reader at 1 still sees it"
    );
}

#[test]
fn roster_journal_is_bounded_and_advertises_its_low_water() {
    let mut store = SessionStore::default();
    for i in 0..(ROSTER_JOURNAL_CAP as u64) {
        store.register(handle(i, None));
        store.deregister_local(i);
    }
    let total = 2 * ROSTER_JOURNAL_CAP as u64;
    assert_eq!(
        store.roster_seq(),
        total,
        "seqs keep counting past eviction"
    );
    assert_eq!(
        store.roster_since(0).count(),
        ROSTER_JOURNAL_CAP,
        "retention is capped"
    );
    assert_eq!(
        store.roster_low_seq(),
        Some(total - ROSTER_JOURNAL_CAP as u64 + 1),
        "the low-water names the oldest retained seq"
    );
    assert!(
        store.is_empty(),
        "every session registered was deregistered"
    );
}

fn timeline_of(h: &SessionHandle) -> Vec<(&'static str, String)> {
    h.ctx
        .timeline
        .lock()
        .unwrap_or_else(|p| p.into_inner())
        .since(None)
        .map(|e| (e.kind, e.payload.to_string()))
        .collect()
}

#[test]
fn attributed_deregister_records_reason_actor_clock_and_exit_code() {
    let mut store = SessionStore::default();
    let h = handle(7, None);
    let sid = h.sid.as_str().to_string();
    store.register(h.clone());
    let before = crate::turn_ledger::now_ms();
    store.note_exit_code(7, Some(3));
    assert_eq!(
        store.deregister_local_as(7, ExitReason::CtlClose, ExitActor::Sid("s-caller".into())),
        Some(h.sid.clone())
    );
    let rec = store
        .roster_since(0)
        .find(|r| r.change == RosterChange::Exited)
        .expect("the exit was journalled")
        .clone();
    assert_eq!(rec.sid, sid);
    assert_eq!(rec.local_id, 7);
    assert_eq!(rec.reason, ExitReason::CtlClose);
    // The closer and the code are the closed index's (6G-9).
    let exit = store
        .closed_logs()
        .exits_since(0)
        .next()
        .expect("the close was indexed");
    assert_eq!((exit.seq, exit.t_ms), (rec.seq, rec.t_ms));
    assert_eq!(exit.actor, ExitActor::Sid("s-caller".into()));
    assert_eq!(exit.exit_code, Some(3));
    assert!(
        rec.t_ms >= before && rec.t_ms <= crate::turn_ledger::now_ms(),
        "stamped on the process-epoch clock: {} not in [{before}, now]",
        rec.t_ms
    );
    let created = store
        .roster_since(0)
        .find(|r| r.change == RosterChange::Created)
        .expect("the birth was journalled");
    assert_eq!(created.reason, ExitReason::Unknown);
    let tl = timeline_of(&h);
    assert_eq!(tl[0].0, "spawned");
    assert_eq!(
        &tl[1..],
        &[
            ("closing", "reason=ctl-close by=s-caller".to_string()),
            ("state-change", "state=closed".to_string()),
        ]
    );
}

#[test]
fn unattributed_deregister_infers_shell_exit_from_the_exited_state_only() {
    let mut store = SessionStore::default();
    let exited = handle_in_state(1, None, SessionState::Exited);
    let alive = handle(2, None);
    store.register(exited.clone());
    store.register(alive.clone());
    store.deregister_local(1);
    store.deregister_local(2);
    let reasons: Vec<(String, ExitReason, ExitActor)> = store
        .closed_logs()
        .exits_since(0)
        .map(|e| (e.sid.as_str().to_string(), e.reason, e.actor.clone()))
        .collect();
    assert_eq!(
        reasons,
        vec![
            (
                exited.sid.as_str().to_string(),
                ExitReason::ShellExit,
                ExitActor::Unknown
            ),
            (
                alive.sid.as_str().to_string(),
                ExitReason::Unknown,
                ExitActor::Unknown
            ),
        ]
    );
    assert!(
        timeline_of(&exited)
            .iter()
            .any(|(k, p)| *k == "closing" && p == "reason=shell-exit by=-"),
        "the inferred shell exit is a known reason: {:?}",
        timeline_of(&exited)
    );
    assert!(
        !timeline_of(&alive).iter().any(|(k, _)| *k == "closing"),
        "an unknown reason records no closing event: {:?}",
        timeline_of(&alive)
    );
}

#[test]
fn an_explicit_reason_outranks_the_exited_state() {
    let mut store = SessionStore::default();
    store.register(handle_in_state(4, None, SessionState::Exited));
    store.deregister_local_as(4, ExitReason::UiClose, ExitActor::Human);
    let rec = store.closed_logs().exits_since(0).next().unwrap();
    assert_eq!(
        (rec.reason, &rec.actor),
        (ExitReason::UiClose, &ExitActor::Human)
    );
}

#[test]
fn a_noted_exit_code_is_consumed_exactly_once() {
    let mut store = SessionStore::default();
    store.register(handle(9, None));
    store.note_exit_code(9, None);
    store.deregister_local(9);
    store.register(handle(9, None));
    store.note_exit_code(9, Some(7));
    store.deregister_local(9);
    store.register(handle(9, None));
    store.deregister_local(9);
    let codes: Vec<Option<i32>> = store
        .closed_logs()
        .exits_since(0)
        .map(|e| e.exit_code)
        .collect();
    assert_eq!(codes, vec![None, Some(7), None]);
}

#[test]
fn attributed_exits_stay_bounded_and_page_by_seq() {
    let mut store = SessionStore::default();
    let n = ROSTER_JOURNAL_CAP as u64 + 5;
    for i in 0..n {
        store.register(handle(i, None));
        store.note_exit_code(i, Some(i32::try_from(i % 256).unwrap()));
        store.deregister_local_as(i, ExitReason::CtlClose, ExitActor::Sid(format!("s-{i}")));
    }
    let exits: Vec<&RosterRecord> = store
        .roster_since(0)
        .filter(|r| r.change == RosterChange::Exited)
        .collect();
    assert_eq!(exits.len(), ROSTER_JOURNAL_CAP / 2);
    let newest = exits.last().unwrap();
    assert_eq!(newest.seq, store.roster_seq());
    let indexed = store
        .closed_logs()
        .exits_since(0)
        .next_back()
        .expect("the newest close is indexed");
    assert_eq!(indexed.seq, newest.seq);
    assert_eq!(indexed.actor, ExitActor::Sid(format!("s-{}", n - 1)));
    assert_eq!(
        indexed.exit_code,
        Some(i32::try_from((n - 1) % 256).unwrap())
    );
    let third = exits[exits.len() - 3].seq;
    let newer: Vec<u64> = store
        .roster_since(third)
        .filter(|r| r.change == RosterChange::Exited)
        .map(|r| r.seq)
        .collect();
    assert_eq!(newer, vec![exits[exits.len() - 2].seq, newest.seq]);
    assert_eq!(store.roster_since(newest.seq).count(), 0, "caught up");
}

#[test]
fn close_attribution_scope_is_outermost_wins_and_restores() {
    assert_eq!(
        current_close_attribution(),
        (ExitReason::Unknown, ExitActor::Unknown)
    );
    {
        let _outer = CloseAttribution::enter(ExitReason::CtlClose, ExitActor::Sid("s-a".into()));
        {
            let _inner = CloseAttribution::enter(ExitReason::UiClose, ExitActor::Human);
            assert_eq!(
                current_close_attribution(),
                (ExitReason::CtlClose, ExitActor::Sid("s-a".into())),
                "the inner claim does not override the initiator"
            );
        }
        assert_eq!(
            current_close_attribution().0,
            ExitReason::CtlClose,
            "dropping the inner (non-opener) guard leaves the scope open"
        );
    }
    assert_eq!(
        current_close_attribution(),
        (ExitReason::Unknown, ExitActor::Unknown),
        "dropping the opener closes the scope"
    );
}

#[test]
fn exit_reason_and_actor_wire_tokens() {
    let tokens: Vec<&str> = ExitReason::ALL.iter().map(|r| r.as_str()).collect();
    assert_eq!(
        tokens,
        [
            "shell-exit",
            "ctl-close",
            "ui-close",
            "window-close",
            "app-quit",
            "reaper",
            "unknown"
        ]
    );
    assert_eq!(ExitActor::Sid("s-z".into()).as_wire(), "s-z");
    assert_eq!(ExitActor::Human.as_wire(), "human");
    assert_eq!(ExitActor::Ctl.as_wire(), "ctl");
    assert_eq!(ExitActor::Harness.as_wire(), "harness");
    assert_eq!(ExitActor::Bridge.as_wire(), "bridge");
    assert_eq!(ExitActor::Unknown.as_wire(), "-");
}
