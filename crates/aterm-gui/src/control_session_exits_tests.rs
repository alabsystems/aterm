// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use aterm_session::{EdgeToken, Op};

use super::{Scope, caller_actor, caller_actor_of, cmd_exits, turn_by, typed_by};
use crate::session_store::{ExitActor, ExitReason, RosterChange, new_store, test_handle};
use crate::turn_ledger::TurnBy;

#[test]
fn no_exits_is_ok_zero() {
    let store = new_store();
    assert_eq!(cmd_exits(&store, "", &[]), "OK 0\n");
    store
        .write()
        .unwrap_or_else(|p| p.into_inner())
        .register(test_handle(1));
    assert_eq!(
        cmd_exits(&store, "", &[]),
        "OK 0\n",
        "a birth is not an exit"
    );
}

#[test]
fn formats_every_reason_actor_and_exit_code() {
    let store = new_store();
    let mut sids = Vec::new();
    {
        let mut g = store.write().unwrap_or_else(|p| p.into_inner());
        for (i, reason) in ExitReason::ALL.iter().enumerate() {
            let local = i as u64 + 10;
            let h = test_handle(local);
            sids.push(h.sid.as_str().to_string());
            g.register(h);
            let actor = match i % 3 {
                0 => ExitActor::Sid(format!("s-driver{i}")),
                1 => ExitActor::Human,
                _ => ExitActor::Unknown,
            };
            if i % 2 == 0 {
                g.note_exit_code(local, Some(i32::try_from(i).unwrap()));
            }
            g.deregister_local_as(local, *reason, actor);
        }
    }
    let reply = cmd_exits(&store, "", &[]);
    let lines: Vec<&str> = reply.lines().collect();
    assert_eq!(lines[0], format!("OK {}", ExitReason::ALL.len()));
    let ledger: Vec<(u64, u64)> = {
        let g = store.read().unwrap_or_else(|p| p.into_inner());
        g.roster_since(0)
            .filter(|r| r.change == RosterChange::Exited)
            .map(|r| (r.seq, r.t_ms))
            .collect()
    };
    for (i, reason) in ExitReason::ALL.iter().enumerate() {
        let (seq, t) = ledger[i];
        let by = match i % 3 {
            0 => format!("s-driver{i}"),
            1 => "human".to_string(),
            _ => "-".to_string(),
        };
        let code = if i % 2 == 0 {
            i.to_string()
        } else {
            "-".to_string()
        };
        assert_eq!(
            lines[i + 1],
            format!(
                "exit {seq} t={t} sid={} local={} reason={} exit_code={code} by={by} left=-",
                sids[i],
                i + 10,
                reason.as_str()
            ),
            "row for {reason:?}"
        );
    }
}

#[test]
fn exits_n_formats_only_n_rows() {
    use crate::work_counts::take_ring_rows_formatted;

    let store = new_store();
    {
        let mut g = store.write().unwrap_or_else(|p| p.into_inner());
        for local in 1..=8 {
            g.register(test_handle(local));
            if local % 2 == 0 {
                g.note_exit_code(local - 1, Some(i32::try_from(local).unwrap()));
                g.deregister_local_as(local - 1, ExitReason::UiClose, ExitActor::Human);
            }
        }
    }
    let oracle = |since: u64, n: usize| -> String {
        let g = store.read().unwrap_or_else(|p| p.into_inner());
        let mut rows: Vec<String> = g
            .closed_logs()
            .exits_since(since)
            .map(|r| {
                let exit_code = r
                    .exit_code
                    .map_or_else(|| "-".to_string(), |c| c.to_string());
                format!(
                    "exit {} t={} sid={} local={} reason={} exit_code={exit_code} by={} left=-",
                    r.seq,
                    r.t_ms,
                    r.sid.as_str(),
                    r.local_id,
                    r.reason.as_str(),
                    crate::control::pct_encode(r.actor.as_wire()),
                )
            })
            .collect();
        if n > 0 && rows.len() > n {
            rows.drain(..rows.len() - n);
        }
        let mut out = format!("OK {}\n", rows.len());
        for row in rows {
            out.push_str(&row);
            out.push('\n');
        }
        out
    };
    let exited = 4;
    take_ring_rows_formatted();
    assert_eq!(cmd_exits(&store, "", &[]), oracle(0, 0));
    assert_eq!(take_ring_rows_formatted(), exited, "negative control");
    for n in [1, 2, exited - 1, exited, exited + 1] {
        for since in [0, 3, 6, 12, u64::MAX] {
            let args = format!("{n} since={since}");
            let reply = cmd_exits(&store, &args, &[]);
            assert_eq!(reply, oracle(since, n), "{args}");
            assert_eq!(
                take_ring_rows_formatted(),
                reply.lines().count() - 1,
                "{args}"
            );
        }
    }
}

#[test]
fn window_cursor_and_usage() {
    let store = new_store();
    {
        let mut g = store.write().unwrap_or_else(|p| p.into_inner());
        for local in 1..=3 {
            g.register(test_handle(local));
            g.deregister_local_as(local, ExitReason::UiClose, ExitActor::Human);
        }
    }
    let all = cmd_exits(&store, "", &[]);
    let ids: Vec<u64> = all
        .lines()
        .skip(1)
        .map(|l| l.split(' ').nth(1).unwrap().parse().unwrap())
        .collect();
    assert_eq!(ids.len(), 3);
    let last = cmd_exits(&store, "1", &[]);
    assert!(last.starts_with("OK 1\n"), "{last:?}");
    assert!(last.contains(&format!("exit {} ", ids[2])), "{last:?}");
    let newer = cmd_exits(&store, &format!("since={}", ids[1]), &[]);
    assert!(newer.starts_with("OK 1\n"), "{newer:?}");
    assert!(newer.contains(&format!("exit {} ", ids[2])), "{newer:?}");
    assert_eq!(
        cmd_exits(&store, &format!("since={}", ids[2]), &[]),
        "OK 0\n"
    );
    let both = cmd_exits(&store, &format!("5 since={}", ids[0]), &[]);
    assert!(both.starts_with("OK 2\n"), "{both:?}");
    assert_eq!(cmd_exits(&store, "3 1", &[]), last);
    assert_eq!(
        cmd_exits(&store, &format!("since=0 since={}", ids[1]), &[]),
        newer
    );
    for bad in ["bogus", "since=x", "since=", "1 extra", "-1"] {
        assert_eq!(
            cmd_exits(&store, bad, &[]),
            "ERR usage: exits [<n>] [since=<id>]\n",
            "{bad:?}"
        );
    }
}

/// The leftover view's record of a tab's close, as its last reading counted what that tab
/// left running that `end` takes.
fn closed(sid: &str, left: usize) -> aterm_vitals::left::ClosedTab {
    aterm_vitals::left::ClosedTab {
        key: 1,
        sid: sid.to_owned(),
        shell_uniqueid: 1_000,
        horizon: 2_000,
        left,
        ..aterm_vitals::left::ClosedTab::default()
    }
}

/// AN EXIT THAT LEFT TWO PROCESSES READS `left=2`: the row joins its session to the view's
/// record of that close, the newest when the view holds two (a sid is never reused, so two
/// are one close's), and reads no table.
#[test]
fn an_exit_that_left_two_processes_reads_left_2() {
    let store = new_store();
    let sid = {
        let mut g = store.write().unwrap_or_else(|p| p.into_inner());
        let h = test_handle(1);
        let sid = h.sid.as_str().to_string();
        g.register(h);
        g.deregister_local_as(1, ExitReason::UiClose, ExitActor::Human);
        sid
    };
    let reply = cmd_exits(&store, "", &[closed(&sid, 5), closed(&sid, 2)]);
    let row = reply.lines().nth(1).expect("one exit row");
    assert!(row.ends_with(" by=human left=2"), "{row}");
}

/// AN EXIT WITH NO RECORD READS `left=-`: a tab closed before its shell was read, or one the
/// view has dropped, is not a tab that left nothing. NEGATIVE CONTROL: default a missing
/// record to 0, and this row reads `left=0`.
#[test]
fn an_exit_with_no_record_reads_left_dash() {
    let store = new_store();
    {
        let mut g = store.write().unwrap_or_else(|p| p.into_inner());
        for local in 1..=2 {
            g.register(test_handle(local));
            g.deregister_local_as(local, ExitReason::UiClose, ExitActor::Human);
        }
    }
    let reply = cmd_exits(&store, "", &[closed("s-another", 3)]);
    let rows: Vec<&str> = reply.lines().skip(1).collect();
    assert_eq!(rows.len(), 2, "{reply}");
    for row in rows {
        assert!(row.ends_with(" left=-"), "{row}");
    }
    let empty = closed("s-another", 0);
    assert_eq!(
        super::exits_reply(&[], std::slice::from_ref(&empty)),
        "OK 0\n"
    );
}

#[test]
fn caller_actor_is_the_edge_source_never_the_target_and_the_client_kind_for_owner() {
    let store = new_store();
    let caller = test_handle(1);
    let target = test_handle(2);
    let (caller_sid, target_sid) = (
        caller.sid.as_str().to_string(),
        target.sid.as_str().to_string(),
    );
    let tok = target
        .ctx
        .edges
        .lock()
        .unwrap_or_else(|p| p.into_inner())
        .grant(
            caller.sid.clone(),
            target.sid.clone(),
            Op::WriteInput,
            target.ctx.nonce,
        );
    let by = caller_actor(Scope::Edge(tok), &target.ctx, &store);
    assert_eq!(by, ExitActor::Sid(caller_sid.clone()));
    assert_ne!(
        by.as_wire(),
        target_sid,
        "the closer is the token's source session, not the session being closed"
    );
    assert_eq!(
        caller_actor(Scope::Owner, &target.ctx, &store),
        ExitActor::Ctl,
        "an owner-token connection carries no session identity, so the row \
         names the kind of client"
    );
    assert_eq!(ExitActor::Ctl.as_wire(), "ctl");
    assert_eq!(
        caller_actor(Scope::Bridge, &target.ctx, &store),
        ExitActor::Bridge,
        "the bridge is a connection with no sid, named apart from a local client"
    );
    assert_eq!(ExitActor::Bridge.as_wire(), "bridge");
    assert_eq!(
        caller_actor(Scope::Edge(EdgeToken::generate()), &target.ctx, &store),
        ExitActor::Unknown,
        "a token the target's table never issued names nobody"
    );
    assert_eq!(caller_actor_of(Scope::Owner, None, &store), ExitActor::Ctl);
    assert_eq!(
        caller_actor_of(Scope::Bridge, None, &store),
        ExitActor::Bridge
    );
    assert_eq!(
        caller_actor_of(Scope::Edge(tok), None, &store),
        ExitActor::Unknown
    );
    assert_eq!(
        caller_actor_of(Scope::Edge(tok), Some(&target.ctx), &store),
        ExitActor::Sid(caller_sid.clone())
    );
    {
        let mut g = store.write().unwrap_or_else(|p| p.into_inner());
        g.register(caller);
        g.register(target);
        g.deregister_local_as(2, ExitReason::CtlClose, by);
    }
    let reply = cmd_exits(&store, "", &[]);
    let row = reply.lines().nth(1).expect("one exit row");
    assert!(row.contains(&format!(" sid={target_sid} ")), "{row}");
    assert!(
        row.ends_with(&format!(
            " reason=ctl-close exit_code=- by={caller_sid} left=-"
        )),
        "{row}"
    );
}

/// THE TURN LEDGER NAMES A CALLER AS THE EXITS DO, but for an owner client run from a tab:
/// the exits name that tab's session, the turns `ctl` (their readers name the process their
/// own way, `typed_by`), and only the harness's hand makes a turn the harness's.
#[test]
fn a_turns_by_is_the_callers_and_only_the_harness_hand_makes_it_harness() {
    let store = new_store();
    let caller = test_handle(1);
    let mut target = test_handle(2);
    target.shell_identity.pid = 6999;
    let target_sid = target.sid.as_str().to_string();
    let caller_sid = caller.sid.as_str().to_string();
    let tok = target
        .ctx
        .edges
        .lock()
        .unwrap_or_else(|p| p.into_inner())
        .grant(
            caller.sid.clone(),
            target.sid.clone(),
            Op::WriteInput,
            target.ctx.nonce,
        );
    let by = |scope: Scope| {
        let driver = crate::control::turn_driver(scope, &target.ctx);
        let by = turn_by(scope, driver.as_ref());
        let named = match caller_actor_of(scope, Some(&target.ctx), &store) {
            ExitActor::Sid(sid) => TurnBy::Sid(sid),
            ExitActor::Ctl => TurnBy::Ctl,
            ExitActor::Bridge => TurnBy::Bridge,
            ExitActor::Harness => TurnBy::Harness,
            ExitActor::Human | ExitActor::Fleet { .. } | ExitActor::Unknown => TurnBy::Unknown,
        };
        assert_eq!(by, named, "{scope:?}");
        by
    };
    assert_eq!(by(Scope::Owner), TurnBy::Ctl);
    assert_eq!(by(Scope::Bridge), TurnBy::Bridge);
    assert_eq!(by(Scope::Edge(tok)), TurnBy::Sid(caller_sid.clone()));
    assert_eq!(
        by(Scope::Edge(EdgeToken::generate())),
        TurnBy::Unknown,
        "a token the target's table never issued names nobody"
    );
    let ctx = std::sync::Arc::clone(&target.ctx);
    store
        .write()
        .unwrap_or_else(|p| p.into_inner())
        .register(target);
    let in_tab = || {
        crate::typed_by::Who::client_with(
            Some(7001),
            1,
            &|pid| match pid {
                7001 => Some(6999),
                _ => None,
            },
            &|_| None,
            &|_| Some("aterm".to_string()),
        )
    };
    assert_eq!(
        super::caller_actor_as(Scope::Owner, Some(&ctx), &store, in_tab),
        ExitActor::Sid(target_sid),
        "the exits name an owner client run from a tab by that tab's session"
    );
    assert_eq!(
        turn_by(
            Scope::Owner,
            crate::control::turn_driver(Scope::Owner, &ctx).as_ref()
        ),
        TurnBy::Ctl,
        "the turns name it the owner token's client"
    );
    let elsewhere =
        || crate::typed_by::Who::client_with(Some(8001), 1, &|_| None, &|_| None, &|_| None);
    assert_eq!(
        super::caller_actor_as(Scope::Owner, Some(&ctx), &store, elsewhere),
        ExitActor::Ctl,
        "CONTROL: a client in no tab is `ctl` in both"
    );

    let drive = |holder: &str| crate::Lease::Drive {
        holder: holder.to_string(),
        expires_us: u64::MAX,
        conn: Some(1),
        hard: true,
        here: false,
        holder_pid: None,
    };
    let hand = drive(&crate::harness_host::holder());
    assert_eq!(typed_by(&TurnBy::Ctl, Some(&hand)), TurnBy::Harness);
    assert_eq!(TurnBy::Harness.as_wire(), "harness");
    for under in [
        None,
        Some(drive("aterm-harness@1")),
        Some(drive("manager")),
        Some(crate::Lease::Turn {
            id: 9,
            driver: None,
            typing: true,
        }),
    ] {
        assert_eq!(
            typed_by(&TurnBy::Ctl, under.as_ref()),
            TurnBy::Ctl,
            "{under:?}"
        );
    }
    for caller in [TurnBy::Sid(caller_sid), TurnBy::Bridge, TurnBy::Unknown] {
        assert_eq!(typed_by(&caller, Some(&hand)), caller, "{caller:?}");
    }
}
