// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The `claim` record and the re-exec grace of a stream's claim, driven through the same rig as
//! the stream's own tests: a real claim on a real connection, the shipping lanes and reader,
//! records written by the wire library's codec, and the claim line a record asks for served by
//! the shipping `meta set supervisor` with the kernel's proof stood in.

use alab_wire::record::State;
use alab_wire::{ChildProcess, Scope as WireScope};
use aterm_phase::Program;

use super::super::{ClaimRecord, Standing, claim_line};
use super::*;
use crate::session_timeline::{AgentStamp, SessionTimeline, StreamedState};

/// What a test reads of `sid`'s claim: its placements, its revision, the grant's words, the
/// version, vendor and child the claim holds, the connection it is bound to and its expiry.
#[derive(Clone, Debug, PartialEq, Eq)]
struct Held {
    placements: u64,
    revision: u64,
    scope: String,
    version: Option<String>,
    vendor: Option<String>,
    child: Option<u32>,
    conn: Option<u64>,
    expiry: Option<u64>,
}

/// `sid`'s live scoped claim, or `None` where it holds none now.
fn held(rig: &Rig, sid: &str) -> Option<Held> {
    let handle = rig.service.handle(sid);
    let meta = handle
        .ctx
        .meta
        .lock()
        .unwrap_or_else(PoisonError::into_inner);
    let now = crate::metrics::now_us();
    let claim = meta.live_claim(now)?;
    let scoped = claim.scoped.as_ref()?;
    Some(Held {
        placements: meta.claim_placements,
        revision: meta.supervisor_revision,
        scope: scoped.claim.scopes.words(),
        version: scoped.claim.version.as_ref().map(|v| v.text.clone()),
        vendor: scoped.claim.vendor.clone(),
        child: scoped.child.map(|kept| kept.pid),
        conn: claim.conn,
        expiry: claim.expires_us,
    })
}

fn claim(
    child: Option<ChildProcess>,
    scope: Option<Vec<WireScope>>,
    version: Option<&str>,
    vendor: Option<&str>,
) -> Body {
    Body::Claim {
        child,
        scope,
        version: version.map(str::to_string),
        vendor: vendor.map(str::to_string),
        login: None,
    }
}

/// Run `read` on `sid`'s timeline.
fn timeline<R>(rig: &Rig, sid: &str, read: impl FnOnce(&mut SessionTimeline) -> R) -> R {
    let handle = rig.service.handle(sid);
    let mut timeline = handle
        .ctx
        .timeline
        .lock()
        .unwrap_or_else(PoisonError::into_inner);
    read(&mut timeline)
}

/// The screen reader publishes `word` for `sid`, as its sweep does; whether the published word
/// moved.
fn screen(rig: &Rig, sid: &str, word: &'static str) -> bool {
    static SEQ: AtomicU64 = AtomicU64::new(1);
    let seq = SEQ.fetch_add(1, Ordering::Relaxed);
    timeline(rig, sid, |timeline| {
        timeline.publish_agent(
            word,
            None,
            None,
            Some(Program::Claude),
            AgentStamp {
                generation: crate::control::ScreenGen { epoch: 1, seq },
                fp: seq,
            },
        )
    })
}

/// The state the stream feeds `sid`'s current-state record, while its stand-down stands.
fn fed(rig: &Rig, sid: &str) -> Option<StreamedState> {
    timeline(rig, sid, |timeline| {
        timeline.agent().stream.as_ref().map(|fed| fed.state)
    })
}

/// A `claim` RECORD IS A CLAIM PLACEMENT (D7): aterm serves it as `meta set supervisor <holder>
/// contract=4 …` on the stream's own connection, so the claim's grant, version and kept child
/// are the record's, the placement count moves (the restart watch reads it as the harness
/// having claimed again), and the stream goes on from the same `acked`. A changed grant or
/// revision sends the harness a `policy` record; a renewal that changes nothing sends none. A
/// field the record leaves out is the standing claim's.
/// NEGATIVE HALF: a line aterm refuses (here the proof fails) is acknowledged
/// `dropped=refused:unproven`, moves no placement, and leaves the claim as it stood.
#[test]
fn a_claim_record_renews_the_claim_and_moves_its_placements() {
    let rig = Rig::new(2);
    let mut stream = rig.claimant(ONE, "state");
    let id = stream.id();
    stream.open(ONE);
    assert_eq!(stream.record(1, Body::Beat), (1, None));
    let before = held(&rig, ONE).expect("the fixture claim");
    assert_eq!(
        (
            before.scope.as_str(),
            before.version.as_deref(),
            before.child
        ),
        ("state", Some("0.3.0"), None)
    );
    assert!(stream.heard.is_empty(), "nothing owed: {:?}", stream.heard);

    let holder = format!("claude-code-alab@{}", running().pid);
    let child = ChildProcess {
        pid: 4242,
        start: 1_792_000_000_000_000,
    };
    assert_eq!(
        stream.record(
            2,
            claim(
                Some(child),
                Some(vec![WireScope::Relaunch, WireScope::State]),
                Some("0.4.0"),
                Some("2.1.300"),
            )
        ),
        (2, None)
    );
    assert_eq!(
        rig.service.lines(),
        [format!(
            "set supervisor {holder} contract=4 scope=relaunch,state version=0.4.0 \
             vendor=2.1.300 child=4242:1792000000000000"
        )],
        "the one line the record asked for"
    );
    let renewed = held(&rig, ONE).expect("the claim stands");
    assert_eq!(renewed.placements, before.placements + 1, "a placement");
    assert_eq!(
        (
            renewed.scope.as_str(),
            renewed.version.as_deref(),
            renewed.vendor.as_deref(),
            renewed.child,
            renewed.conn,
            renewed.expiry,
        ),
        (
            "relaunch,state",
            Some("0.4.0"),
            Some("2.1.300"),
            Some(4242),
            Some(id),
            None
        ),
        "the claim is the record's, still bound to the stream's connection"
    );
    assert_ne!(renewed.revision, before.revision);
    let state = rig.state(ONE);
    assert_eq!(
        state.acked, 2,
        "the stream's state is kept across the claim"
    );
    let policy = stream.own();
    assert_eq!(
        policy.body,
        Body::Policy,
        "the grant moved: aterm sends a `policy` record"
    );
    assert!(
        policy.seq > stream.opening[1].seq,
        "numbered after the opening"
    );
    assert_eq!(
        state.applied.iter().copied().collect::<Vec<_>>(),
        ["beat"],
        "no built-in source stands down for `claim`"
    );

    // A record that names nothing renews the claim as it stands: a placement, the same grant
    // and revision, and no `policy` sent.
    assert_eq!(stream.record(3, claim(None, None, None, None)), (3, None));
    assert_eq!(
        rig.service.lines().last().map(String::as_str),
        Some(
            format!(
                "set supervisor {holder} contract=4 scope=relaunch,state version=0.4.0 \
                 vendor=2.1.300 child=4242:1792000000000000"
            )
            .as_str()
        ),
        "what the record leaves out is the standing claim's"
    );
    let again = held(&rig, ONE).expect("the claim stands");
    assert_eq!(again.placements, renewed.placements + 1);
    assert_eq!(
        Held {
            placements: renewed.placements,
            ..again.clone()
        },
        renewed,
        "nothing but the placement count moved"
    );
    assert_eq!(stream.record(4, Body::Beat), (4, None), "the stream stands");
    assert!(
        stream.heard.is_empty(),
        "nothing changed: no `policy` ({:?})",
        stream.heard
    );

    // NEGATIVE HALF: the proof fails, so the line is refused and the claim stands as it was.
    *rig.service
        .proof
        .lock()
        .unwrap_or_else(PoisonError::into_inner) = None;
    let dropped = rig.state(ONE).dropped;
    assert_eq!(
        stream.record(
            5,
            claim(None, Some(vec![WireScope::State]), Some("0.5.0"), None)
        ),
        (5, Some("refused:unproven".to_string()))
    );
    assert_eq!(
        held(&rig, ONE),
        Some(again),
        "a refused line changes nothing"
    );
    let state = rig.state(ONE);
    assert_eq!((state.acked, state.dropped), (5, dropped + 1));
    assert_eq!(stream.record(6, Body::Beat), (6, None));
    assert!(
        stream.heard.is_empty(),
        "a refused line sends no `policy` ({:?})",
        stream.heard
    );
}

/// A `claim` RECORD'S TEXT NEVER BECOMES A SECOND CONTROL LINE (D7): each field is checked as
/// the claim grammar checks a word before any line is built. Over the wire, a record whose
/// `version`, `vendor` or `login` holds a space, a newline or another word, or whose scope is
/// no word list, is acknowledged `dropped=refused:usage`; no line is served, and the claim, its
/// grant and its placements stand. The line builder itself refuses each such field, so the
/// check does not rest on the codec.
/// POSITIVE CONTROL: the same builder turns well-formed fields into exactly one line.
#[test]
fn a_claims_text_never_becomes_a_second_control_line() {
    let rig = Rig::new(2);
    let mut stream = rig.claimant(ONE, "state");
    stream.open(ONE);
    let before = held(&rig, ONE).expect("the fixture claim");
    let hostile = [
        r#"{"k":"claim","seq":1,"ms":0,"version":"0.4.0 scope=accounts"}"#,
        r#"{"k":"claim","seq":2,"ms":0,"version":"0.4.0\nmeta unset supervisor"}"#,
        r#"{"k":"claim","seq":3,"ms":0,"vendor":"2.1 login=root"}"#,
        r#"{"k":"claim","seq":4,"ms":0,"login":"work\nmeta set supervisor x@1"}"#,
        r#"{"k":"claim","seq":5,"ms":0,"scope":["state accounts"]}"#,
        r#"{"k":"claim","seq":6,"ms":0,"scope":["state,accounts"]}"#,
        r#"{"k":"claim","seq":7,"ms":0,"scope":[]}"#,
        r#"{"k":"claim","seq":8,"ms":0,"child":{"pid":1,"start":5}}"#,
    ];
    for (at, line) in hostile.iter().enumerate() {
        let seq = u64::try_from(at).expect("a small index") + 1;
        assert_eq!(
            stream.raw(line),
            (seq, Some("refused:usage".to_string())),
            "{line}"
        );
    }
    assert_eq!(
        rig.service.lines(),
        Vec::<String>::new(),
        "no line was served"
    );
    assert_eq!(held(&rig, ONE), Some(before), "the claim stands untouched");
    assert_eq!(stream.record(9, Body::Beat), (9, None), "the stream stands");

    // The line builder, given the fields with no codec in front of it.
    let standing = Standing {
        holder: "claude-code-alab@4242".to_string(),
        scope: "state".to_string(),
        version: Some("0.3.0".to_string()),
        vendor: None,
        login: None,
        child: None,
    };
    let record = |edit: &dyn Fn(&mut ClaimRecord)| {
        let mut record = ClaimRecord {
            child: None,
            scope: None,
            version: None,
            vendor: None,
            login: None,
        };
        edit(&mut record);
        claim_line(&standing, &record)
    };
    let text = |text: &str| Some(text.to_string());
    assert_eq!(
        record(&|r| {
            r.scope = Some(vec![WireScope::State, WireScope::Accounts]);
            r.version = text("0.4.0+g1a2b3c");
            r.vendor = text("2.1.300");
            r.login = text("work");
            r.child = Some(ChildProcess { pid: 77, start: 9 });
        })
        .as_deref(),
        Some(
            "set supervisor claude-code-alab@4242 contract=4 scope=state,accounts \
             version=0.4.0+g1a2b3c vendor=2.1.300 child=77:9 login=work"
        ),
        "positive control: well-formed fields make one line"
    );
    assert_eq!(
        record(&|_| {}).as_deref(),
        Some("set supervisor claude-code-alab@4242 contract=4 scope=state version=0.3.0"),
        "positive control: an empty record is the standing claim again"
    );
    for hostile in [
        "0.4.0 scope=accounts",
        "0.4.0\nmeta unset supervisor",
        "0.4.0\tx",
        "0.4.1",
        "",
    ] {
        assert_eq!(record(&|r| r.version = text(hostile)), None, "{hostile:?}");
    }
    for hostile in ["2.1 login=root", "2.1\n", "2.1\u{7}", ""] {
        assert_eq!(record(&|r| r.vendor = text(hostile)), None, "{hostile:?}");
    }
    for hostile in ["work home", "work\nmeta set supervisor x@1", "Work", ""] {
        assert_eq!(record(&|r| r.login = text(hostile)), None, "{hostile:?}");
    }
    assert_eq!(
        record(&|r| r.child = Some(ChildProcess { pid: 1, start: 5 })),
        None,
        "pid 1 is no harness's child"
    );
    assert_eq!(
        record(&|r| r.scope = Some(Vec::new())),
        None,
        "an empty scope spells no `scope=`"
    );
    // The standing scope is the claim's ask, unknown words included, never the grant's `-`.
    let asked = Standing {
        scope: "turn-end,later-word".to_string(),
        ..standing.clone()
    };
    assert_eq!(
        claim_line(
            &asked,
            &ClaimRecord {
                child: None,
                scope: None,
                version: None,
                vendor: None,
                login: None,
            }
        )
        .as_deref(),
        Some(
            "set supervisor claude-code-alab@4242 contract=4 scope=turn-end,later-word \
             version=0.3.0"
        ),
        "a record that names no scope asks the claim's own words again"
    );
    let spaced = Standing {
        holder: "claude-code-alab@4242 ttl=1".to_string(),
        ..standing.clone()
    };
    assert_eq!(
        claim_line(
            &spaced,
            &ClaimRecord {
                child: None,
                scope: None,
                version: None,
                vendor: None,
                login: None,
            }
        ),
        None,
        "a holder that is no token builds no line"
    );
}

/// A `claim` RECORD THAT NAMES NO SCOPE ASKS AGAIN FOR WHAT THE CLAIM ASKED, NOT FOR ITS GRANT
/// (D7): a claim whose words its row does not allow stands with an empty grant and opens a
/// stream. A request-only renewal replaces that set without changing the grant or revision.
/// Its harness starts a new vendor child and says so in a `claim` record that names only the
/// child. aterm serves the latest canonical ask again: the record is acknowledged with no
/// `dropped`, the placement count moves (what the restart watch reads as the harness having
/// claimed again), the child is kept, and the grant is still empty.
/// NEGATIVE HALF: nothing of the empty grant (`-`) reaches the line.
#[test]
fn a_claim_record_on_an_empty_grant_asks_its_claim_again() {
    let rig = Rig::new(2);
    let holder = format!("grok-alab@{}", running().pid);
    let mut client = rig.dial("owner");
    // grok-alab's seed row allows `relaunch,upgrade`: `turn-end` is not granted, and
    // unknown words remain part of the request rather than the grant.
    let reply = client.request(&format!(
        "meta {ONE} set supervisor {holder} contract=4 scope=turn-end,old-word version=0.9.0"
    ));
    assert!(reply.starts_with("OK scope=- revision="), "{reply}");
    let first = held(&rig, ONE).expect("the original empty grant");
    let reply = client.request(&format!(
        "meta {ONE} set supervisor {holder} contract=4 scope=walls,later-word version=0.9.0"
    ));
    assert!(reply.starts_with("OK scope=- revision="), "{reply}");
    let before = held(&rig, ONE).expect("the claim stands on an empty grant");
    assert_eq!((before.scope.as_str(), before.child), ("-", None));
    assert_eq!(before.placements, first.placements + 1);
    assert_eq!(before.revision, first.revision, "only the request changed");
    assert_eq!(
        rig.service
            .handle(ONE)
            .ctx
            .meta
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .live_scoped(crate::metrics::now_us())
            .expect("the latest claim")
            .claim
            .claimed,
        "later-word,walls",
        "the latest request keeps unknown and withheld words in canonical order"
    );
    client.open(ONE);

    let child = ChildProcess {
        pid: 4242,
        start: 1_792_000_000_000_000,
    };
    assert_eq!(
        client.record(1, claim(Some(child), None, None, None)),
        (1, None),
        "the record is served, not refused"
    );
    assert_eq!(
        rig.service.lines(),
        [format!(
            "set supervisor {holder} contract=4 scope=later-word,walls version=0.9.0 \
             child=4242:1792000000000000"
        )],
        "the claim's own ask, and nothing of the grant's `-`"
    );
    let after = held(&rig, ONE).expect("the claim stands");
    assert_eq!(after.placements, before.placements + 1, "a placement");
    assert_eq!(
        (after.scope.as_str(), after.child),
        ("-", Some(4242)),
        "the child is kept under the same empty grant"
    );
    assert_eq!(client.record(2, Body::Beat), (2, None), "the stream stands");

    // A record that names nothing more asks the same again: served, the same empty grant.
    assert_eq!(client.record(3, claim(None, None, None, None)), (3, None));
    assert_eq!(
        rig.service.lines().last(),
        rig.service.lines().first(),
        "the same line again"
    );
    let again = held(&rig, ONE).expect("the claim stands");
    assert_eq!(
        (again.placements, again.scope.as_str(), again.revision),
        (after.placements + 1, "-", after.revision)
    );
}

/// `claim` RECORDS HAVE THEIR OWN SMALL BUDGET: serving one proves the claimant's image in full
/// on the lane that reads the stream's beats, so a harness that sends a burst of them is served
/// the first [`CLAIM_BURST`] and the rest are acknowledged `dropped=rate` with no line served,
/// no proof made and no placement counted. A `beat` behind the burst is applied at once, and a
/// `claim` is served again once a token is back.
/// NEGATIVE HALF: the dropped records move nothing of the claim.
#[test]
fn a_burst_of_claim_records_is_held_to_its_own_budget() {
    use super::super::{CLAIM_BURST, CLAIM_REFILL_MS};
    let rig = Rig::new(2);
    let mut stream = rig.claimant(ONE, "state");
    stream.open(ONE);
    let before = held(&rig, ONE).expect("the fixture claim");
    let burst = u64::from(CLAIM_BURST);
    for seq in 1..=burst {
        assert_eq!(
            stream.record(seq, claim(None, None, None, None)),
            (seq, None),
            "inside the burst"
        );
    }
    let served = held(&rig, ONE).expect("the claim stands");
    assert_eq!(served.placements, before.placements + burst);
    let dropped = rig.state(ONE).dropped;
    for seq in burst + 1..=burst + 20 {
        assert_eq!(
            stream.record(seq, claim(None, None, None, None)),
            (seq, Some("rate".to_string())),
            "over the claim budget"
        );
    }
    assert_eq!(
        rig.service.lines().len(),
        usize::try_from(burst).expect("a small burst"),
        "no line was served, so no image was proven, past the burst"
    );
    assert_eq!(held(&rig, ONE), Some(served.clone()), "nothing moved");
    let state = rig.state(ONE);
    assert_eq!((state.acked, state.dropped), (burst + 20, dropped + 20));
    assert_eq!(
        stream.record(burst + 21, Body::Beat),
        (burst + 21, None),
        "a beat behind the burst is applied"
    );

    // One token comes back after the refill: one more claim is served, and only one.
    rig.service
        .clock
        .fetch_add(CLAIM_REFILL_MS, Ordering::AcqRel);
    assert_eq!(
        stream.record(burst + 22, claim(None, None, None, None)),
        (burst + 22, None)
    );
    assert_eq!(
        stream.record(burst + 23, claim(None, None, None, None)),
        (burst + 23, Some("rate".to_string()))
    );
    assert_eq!(
        held(&rig, ONE).map(|held| held.placements),
        Some(served.placements + 1)
    );
}

/// A CLOSED STREAM KEEPS ITS CLAIM THROUGH THE RE-EXEC GRACE (D28): the connection of a contract
/// 4 claim closes, and the claim stands for 45 s more with the grace's end as its expiry. No
/// placement is counted for the close. The stream's stand-down stands with it: the state the
/// stream fed still decides the tab's agent word, and the screen reader's reading does not. The
/// same claimant claiming again inside the grace renews the claim (no expiry) and its reopened
/// stream answers the kept `acked`. At the grace's end the lapse releases the claim and the
/// screen reader feeds the tab again.
/// NEGATIVE HALF: a contract 3 claim still ends with its connection.
#[test]
fn a_closed_stream_keeps_its_claim_through_the_reexec_grace() {
    let grace_us = alab_wire::record::REEXEC_GRACE_MS * 1000;
    let rig = Rig::new(2);
    let mut first = rig.claimant(ONE, "state");
    let id = first.id();
    let epoch = first.open(ONE).epoch;
    assert_eq!(
        first.record(
            1,
            Body::State {
                state: State::Working,
                composer: None,
            }
        ),
        (1, None)
    );
    assert_eq!(fed(&rig, ONE), Some(StreamedState::Working));
    let bound = held(&rig, ONE).expect("the claim");
    assert_eq!((bound.conn, bound.expiry), (Some(id), None));

    let closing = crate::metrics::now_us();
    drop(first);
    rig.ended(id);
    let closed = crate::metrics::now_us();
    let graced = held(&rig, ONE).expect("the claim outlives its connection");
    let expiry = graced
        .expiry
        .expect("the grace's end is the claim's expiry");
    assert!(
        (closing + grace_us..=closed + grace_us).contains(&expiry),
        "the expiry {expiry} is the close plus the grace ({closing}..{closed} + {grace_us})"
    );
    assert_eq!(
        Held {
            expiry: None,
            ..graced.clone()
        },
        bound,
        "the close placed no claim: placements, revision, grant and connection stand"
    );
    let handle = rig.service.handle(ONE);
    let applied = |now_us: u64| {
        handle
            .ctx
            .meta
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .stream_applied("state", Duty::State, &epoch, now_us)
    };
    assert!(applied(expiry - 1), "the stream feeds `state` to the end");
    assert!(!applied(expiry), "and not past it");
    // The screen reader stays stood down for the tab through the grace.
    assert_eq!(fed(&rig, ONE), Some(StreamedState::Working));
    assert!(!screen(&rig, ONE, "idle"), "the reader's word is not shown");
    assert_eq!(
        timeline(&rig, ONE, |timeline| timeline.agent().word),
        "busy",
        "the stream's state still decides the word"
    );

    // A second close of the same serial does not extend the grace.
    super::super::super::release_connection_claims(&rig.service.store, None, id);
    assert_eq!(held(&rig, ONE), Some(graced.clone()));

    // The same claimant claims again inside the grace: renewed, and `acked` is kept.
    let mut again = rig.claimant(ONE, "state");
    let again_id = again.id();
    let renewed = held(&rig, ONE).expect("the renewed claim");
    assert_eq!((renewed.conn, renewed.expiry), (Some(again_id), None));
    assert_eq!(again.open(ONE).acked, 1, "the kept acked");
    assert_eq!(
        fed(&rig, ONE),
        Some(StreamedState::Working),
        "the stand-down held across the renewal"
    );

    // It closes again and nothing claims: the grace runs out and the lapse releases the claim.
    drop(again);
    rig.ended(again_id);
    let expiry = held(&rig, ONE)
        .and_then(|held| held.expiry)
        .expect("the second grace");
    assert!(
        !crate::session_timeline::lapse_supervisor(&handle.ctx, expiry - 1),
        "inside the grace nothing lapses"
    );
    assert_eq!(fed(&rig, ONE), Some(StreamedState::Working));
    assert!(crate::session_timeline::lapse_supervisor(
        &handle.ctx,
        expiry
    ));
    assert!(
        handle
            .ctx
            .meta
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .supervisor
            .is_none(),
        "the grace's end released the claim"
    );
    assert_eq!(fed(&rig, ONE), None, "the stand-down ended with the claim");
    assert_eq!(
        timeline(&rig, ONE, |timeline| timeline.agent().word),
        "idle",
        "the screen reader feeds the tab again, from what it read"
    );
    assert!(screen(&rig, ONE, "busy"));

    // NEGATIVE HALF: a contract 3 claim has no grace.
    let mut third = rig.dial("owner");
    assert_eq!(
        third.request(&format!("claim {TWO} 3 relaunch")),
        "OK claim"
    );
    let third_id = third.id();
    assert!(held(&rig, TWO).is_some());
    drop(third);
    rig.ended(third_id);
    let two = rig.service.handle(TWO);
    assert!(
        two.ctx
            .meta
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .supervisor
            .is_none(),
        "a contract 3 claim ends with its connection"
    );
}

/// The `supervisor_version=` and `supervisor_vendor=` words `status` prints for `sid` and the
/// ones its `sessions` row prints.
fn version_words(rig: &Rig, sid: &str) -> [(String, String); 2] {
    let handle = rig.service.handle(sid);
    let pick = |text: &str| {
        let word = |key: &str| {
            text.split(' ')
                .find_map(|word| word.strip_prefix(key))
                .unwrap_or_else(|| panic!("no {key} in {text}"))
                .to_string()
        };
        (word("supervisor_version="), word("supervisor_vendor="))
    };
    let status = {
        let meta = handle
            .ctx
            .meta
            .lock()
            .unwrap_or_else(PoisonError::into_inner);
        crate::session_status::native_supervisor_fields(&meta, crate::metrics::now_us()).0
    };
    let sessions = super::super::super::control_session::sessions_lines(
        std::slice::from_ref(&handle),
        Ok(&[]),
        crate::autopilot::HarnessPosture::default(),
        |_| false,
    );
    let row = sessions.lines().nth(1).expect("the session's row");
    [pick(&status), pick(row)]
}

/// THE HARNESS'S VERSION IS READ FROM THE VERIFIED ROW, NOT THE RECORD (D7): `status` and
/// `sessions` print `supervisor_version=` from the build the claim's image proof admitted. The
/// claim states one version, a `claim` record states another, and the proof admits a third:
/// both lines print the third. A development link prints `link`, and a build that records no
/// version prints `-`. `supervisor_vendor=` has no verified source and is the word the claim,
/// then the record, states.
/// NEGATIVE HALF: the version the claim and the record state is held by the claim and printed
/// by neither line; an unclaimed session prints `-` for both words.
#[test]
fn the_harness_version_is_read_from_the_verified_row_not_the_record() {
    let rig = Rig::new(2);
    let holder = format!("claude-code-alab@{}", running().pid);
    let both = |version: &str, vendor: &str| {
        let words = (version.to_string(), vendor.to_string());
        [words.clone(), words]
    };
    assert_eq!(version_words(&rig, ONE), both("-", "-"), "no claim");

    let mut client = rig.dial("owner");
    let reply = client.request(&format!(
        "meta {ONE} set supervisor {holder} contract=4 scope=state version=0.3.0 vendor=2.1.300"
    ));
    assert!(reply.starts_with("OK scope=state revision="), "{reply}");
    assert_eq!(
        held(&rig, ONE).and_then(|held| held.version).as_deref(),
        Some("0.3.0"),
        "what the claim states"
    );
    assert_eq!(
        version_words(&rig, ONE),
        both(ADMITTED, "2.1.300"),
        "the admitted build's version, and the claim's vendor"
    );

    // A `claim` record states another version: the claim holds it, and no line prints it.
    client.open(ONE);
    assert_eq!(
        client.record(1, claim(None, None, Some("0.4.0"), Some("2.1.301"))),
        (1, None)
    );
    assert_eq!(
        held(&rig, ONE).and_then(|held| held.version).as_deref(),
        Some("0.4.0"),
        "what the record states"
    );
    let printed = version_words(&rig, ONE);
    assert_eq!(printed, both(ADMITTED, "2.1.301"));
    for (version, _) in &printed {
        assert!(
            version != "0.3.0" && version != "0.4.0",
            "NEGATIVE HALF: neither stated version is printed: {version}"
        );
    }

    // A development link's checkout is admitted under no version: it prints `link`.
    let admit = |version: Option<String>| {
        // Each claim record below is one more than the last: let its token come back.
        rig.service
            .clock
            .fetch_add(super::super::CLAIM_REFILL_MS, Ordering::AcqRel);
        *rig.service
            .proof
            .lock()
            .unwrap_or_else(PoisonError::into_inner) = Some(crate::harness_claim::ClaimProof {
            proven: running(),
            version,
        });
    };
    let link = crate::harness_claim::Passed::from(atpkg::harness_record::Admitted::Checkout);
    admit(link.version);
    assert_eq!(client.record(2, claim(None, None, None, None)), (2, None));
    assert_eq!(version_words(&rig, ONE), both("link", "2.1.301"));

    // A store build's own version, as the proof read it from the build.
    let build = crate::harness_claim::Passed::from(atpkg::harness_record::Admitted::Build {
        build: 12,
        version: Some("0.7.0".to_string()),
    });
    admit(build.version);
    assert_eq!(client.record(3, claim(None, None, None, None)), (3, None));
    assert_eq!(version_words(&rig, ONE), both("0.7.0", "2.1.301"));

    // A build that records no version prints `-`, never the claim's own word.
    admit(None);
    assert_eq!(client.record(4, claim(None, None, None, None)), (4, None));
    assert_eq!(version_words(&rig, ONE), both("-", "2.1.301"));
    assert_eq!(version_words(&rig, TWO), both("-", "-"), "another session");
}
