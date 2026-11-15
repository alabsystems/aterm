// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::cmd;
use crate::restore::{RestoreManifest, RestoredSplitTree, RestoredView, TerminalLeafRestore};
use crate::session_store::{SessionHandoff, SessionStore};
use crate::{App, WindowId};

const BY: aterm_slog::Actor = aterm_slog::Actor::Ctl;

fn pinned_of(ctx: &crate::SessionCtx) -> bool {
    ctx.meta.lock().unwrap().pinned
}

/// The `pinned=` word of session `session`'s `status` record.
fn status_pinned(app: &App, session: u64) -> String {
    let record = app.session_status_record(session).expect("a live session");
    record
        .split_whitespace()
        .find_map(|word| word.strip_prefix("pinned="))
        .unwrap_or_else(|| panic!("no pinned= in {record}"))
        .to_owned()
}

/// Every terminal leaf of `manifest`'s first window.
fn leaves(manifest: &RestoreManifest) -> Vec<&TerminalLeafRestore> {
    fn walk<'a>(tree: &'a RestoredSplitTree, out: &mut Vec<&'a TerminalLeafRestore>) {
        match tree {
            RestoredSplitTree::Leaf {
                view: RestoredView::Terminal(leaf),
            } => out.push(leaf),
            RestoredSplitTree::Leaf { .. } => {}
            RestoredSplitTree::Split { first, second, .. } => {
                walk(first, out);
                walk(second, out);
            }
        }
    }
    let mut out = Vec::new();
    for tab in &manifest.windows[0].restored_tabs {
        walk(&tab.root, &mut out);
    }
    out
}

/// PIN THEN STATUS SAYS `pinned=1`; UNPIN, `pinned=0`. Each verb answers the same whether or not
/// it moved the pin, and only a move records it; a word after either is refused by its usage.
/// CONTROLS: a status that does not read the pin (says 0 after a pin), and a pin that reports a
/// move it did not make.
#[test]
fn pin_then_status_says_pinned_and_unpin_lifts_it() {
    let app = App::headless_for_test();
    let ctx = app.pool.get(0).expect("the first tab").ctx.clone();
    assert_eq!(status_pinned(&app, 0), "0", "a fresh session is unpinned");
    assert_eq!(cmd(&ctx, true, "", BY), ("OK pinned=1\n".to_owned(), true));
    assert!(pinned_of(&ctx));
    assert_eq!(status_pinned(&app, 0), "1");
    assert_eq!(
        cmd(&ctx, true, "  ", BY),
        ("OK pinned=1\n".to_owned(), false),
        "pinning a pinned session changes nothing"
    );
    assert_eq!(status_pinned(&app, 0), "1");
    assert_eq!(cmd(&ctx, false, "", BY), ("OK pinned=0\n".to_owned(), true));
    assert_eq!(status_pinned(&app, 0), "0");
    assert_eq!(
        cmd(&ctx, false, "", BY),
        ("OK pinned=0\n".to_owned(), false),
        "unpinning an unpinned session changes nothing"
    );
    for (pin, word) in [(true, "pin"), (false, "unpin")] {
        let (reply, moved) = cmd(&ctx, pin, "now", BY);
        assert_eq!(reply, format!("ERR usage: {word} (it takes no argument)\n"));
        assert!(!moved);
    }
    assert!(!pinned_of(&ctx), "a refused pin moves nothing");
}

/// THE PIN SURVIVES A SAVE AND A RESTORE, BOTH WAYS A SESSION COMES BACK: the layout a quit or a
/// crash journal saves carries it, a restart's respawned tab reads it back, and so does the
/// session an update's successor adopts; the handoff record carries it beside the session's
/// other user fields, so a pin between the park and the commit re-parks. A layout or a record
/// written before the pin has none, and its session reads unpinned, as it was.
/// CONTROLS: the capture dropping the pin, the respawn or the graft not putting it back, and the
/// handoff record leaving it out.
#[test]
fn the_pin_survives_a_save_and_restore_and_an_older_save_reads_unpinned() {
    // Three tabs, the first two pinned. A restore fills the first leaf with the shell the new
    // process already runs (the graft) and spawns the others again (the seed), so the pins of
    // the first two tabs go back each way, and the third stays unpinned.
    let mut old = App::headless_for_test();
    old.push_stub_tab(WindowId(0), crate::stub_session(1));
    old.push_stub_tab(WindowId(0), crate::stub_session(2));
    for tab in [0, 1] {
        let ctx = old.pool.get(tab).expect("a tab").ctx.clone();
        assert!(cmd(&ctx, true, "", BY).1);
    }
    let captured = old.capture_restore_manifest();
    let wire = captured.to_toml().expect("the layout serializes");
    assert_eq!(wire.matches("pinned = true").count(), 2, "{wire}");
    let carried = RestoreManifest::from_toml(&wire).expect("the layout reads back");
    assert_eq!(carried, captured, "the wire drops nothing");
    assert_eq!(
        leaves(&carried)
            .iter()
            .map(|l| l.pinned)
            .collect::<Vec<_>>(),
        vec![true, true, false]
    );

    // A restart: the first tab's shell takes its leaf, and the other tabs are spawned again.
    let mut again = App::headless_for_test();
    again.restore_into_window(WindowId(0), carried.windows[0].clone());
    let restored: Vec<String> = (0..again.next_session_id)
        .filter(|id| again.pool.get(*id).is_some())
        .map(|id| status_pinned(&again, id))
        .collect();
    assert_eq!(restored, ["1", "1", "0"], "the restart keeps each pin");

    // An update: the successor adopts the live session and grafts the leaf's fields onto it.
    let mut successor = App::headless_for_test();
    successor.handoff_successor = true;
    successor
        .pool
        .sessions
        .get_mut(&0)
        .expect("live session")
        .session
        .handoff_local_id = Some(0);
    assert!(!pinned_of(&successor.pool.get(0).unwrap().ctx));
    successor.restore_into_window(WindowId(0), carried.windows[0].clone());
    assert!(
        pinned_of(&successor.pool.get(0).unwrap().ctx),
        "the adopted session keeps it"
    );
    assert_eq!(status_pinned(&successor, 0), "1");

    // The handoff record.
    let mut store = SessionStore::default();
    let pinned = crate::session_store::test_handle(0);
    let plain = crate::session_store::test_handle(1);
    assert!(cmd(&pinned.ctx, true, "", BY).1);
    store.register(pinned.clone());
    store.register(plain.clone());
    let manifest = SessionHandoff::from_store(&store);
    assert_eq!(
        manifest
            .sessions
            .iter()
            .map(|r| r.pinned)
            .collect::<Vec<_>>(),
        vec![true, false]
    );
    let wire = manifest.to_toml().expect("the record serializes");
    assert_eq!(wire.matches("pinned = true").count(), 1, "{wire}");
    assert!(
        !wire.contains("pinned = false"),
        "unpinned stays off: {wire}"
    );
    assert!(manifest.roundtrips());
    #[cfg(unix)]
    {
        use crate::session_store::CarriedClaims;
        let now = crate::metrics::now_us();
        let holder = crate::harness_host::holder();
        assert_eq!(
            CarriedClaims::of_record(&manifest.sessions[0]),
            CarriedClaims::of_session(0, &pinned.ctx, now, &holder),
            "the parked record holds what the live session holds"
        );
        assert!(cmd(&pinned.ctx, false, "", BY).1);
        assert_ne!(
            CarriedClaims::of_record(&manifest.sessions[0]),
            CarriedClaims::of_session(0, &pinned.ctx, now, &holder),
            "an unpin after the park is a move the commit sees"
        );
    }

    // Saved before the pin: no `pinned` anywhere, and the session reads unpinned.
    let older = App::headless_for_test().capture_restore_manifest();
    let older_wire = older.to_toml().expect("serializes");
    assert!(!older_wire.contains("pinned"), "{older_wire}");
    let mut cold = App::headless_for_test();
    cold.restore_into_window(
        WindowId(0),
        RestoreManifest::from_toml(&older_wire)
            .expect("reads")
            .windows[0]
            .clone(),
    );
    assert!(
        (0..cold.next_session_id)
            .filter_map(|id| cold.pool.get(id))
            .all(|s| !pinned_of(&s.ctx)),
        "an older layout restores every tab unpinned"
    );
    let older_record = SessionHandoff::from_store(&{
        let mut store = SessionStore::default();
        store.register(crate::session_store::test_handle(0));
        store
    })
    .to_toml()
    .expect("serializes");
    assert!(!older_record.contains("pinned"), "{older_record}");
    assert!(
        !SessionHandoff::from_toml(&older_record)
            .expect("reads")
            .sessions[0]
            .pinned,
        "an older record reads unpinned"
    );
}

/// A DRIVE FROM ANOTHER MAC DOES NOT SERVE A PIN, AND ITS REFUSAL SAYS SO: a pin is put on the
/// Mac the session runs on. CONTROL: `status` over the same drive is served.
#[cfg(unix)]
#[test]
fn a_pin_over_a_fleet_drive_is_refused_naming_why() {
    for verb in ["pin", "unpin"] {
        let refusal =
            crate::control::drive_kind_of_line(&format!("@s-0123456789abcdef0123 {verb}"))
                .expect_err("no drive serves a pin");
        assert!(
            refusal.starts_with("a drive serves @<sid> ")
                && refusal.ends_with(&format!("not `{verb}`")),
            "{refusal}"
        );
    }
    assert!(crate::control::drive_kind_of_line("@s-0123456789abcdef0123 status").is_ok());
}
