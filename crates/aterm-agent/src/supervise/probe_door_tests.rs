// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! THE HARNESS'S OWN USAGE PROBE NEVER STRANDS A WORKER (HP1). Seen twice on 2026-11-06 at a
//! worker's turn end right after its `Nothing left:` reply, on Claude Code 2.1.318 under aterm
//! 0.157/0.158: the probe typed `/usage`, its read gave no sample, and its Escape went out with no
//! duty, so the window harness's door refused it (`ERR denied supervisor input authority`) and the
//! loop ended on it, twice, 121 s apart; the panel stood, the held deadline stalled the task, and
//! Land's check was typed only after the panel closed. Here: the probe closes its own panel behind
//! that door whatever its read gave, and tells its host the panel stands while it does; and no
//! probe runs where Land owes its land check, which goes first.

use super::super::supervisor_attention::{HostDoor, behind_the_door};
use super::*;
use crate::harness::contract::Scope;
use crate::supervise::policy::turn_end::{LAND, LAND_CHECK, RULE_LAND, RULE_LAND_CHECK};

/// A due probe on a point, behind the window harness's door: the point decided (`seed` first
/// sets up the session under the run's options; `continues`, the policy's `continue`, lets a done
/// claim be acted on), then the probe run there, with what it said.
fn door_probe(
    door: &mut HostDoor<'_>,
    host: &Arc<ProbeHost>,
    ledger: &Path,
    continues: bool,
    seed: impl FnOnce(&mut Session<'_, HostDoor<'_>>, &SuperviseOpts),
) -> (TurnEndAction, Probed, Heard) {
    let opts = SuperviseOpts {
        idle_host: Some(Arc::clone(host) as Arc<dyn IdleHost>),
        policy: SupervisorConfig {
            continue_policy: continues,
            ..hosted(30).policy
        },
        ..hosted(30)
    };
    let mut s = behind_the_door(door);
    s.set_clock(PROBE_NOW, PDT);
    s.set_zone_lookup(test_zones);
    s.set_approval_ledger(Some(ledger.to_path_buf()));
    s.ledger = crate::supervise::approvals::Ledger::open(Some(ledger), None, &mut Vec::new());
    s.stall_host = opts.idle_host.clone();
    let ms = Duration::from_millis;
    s.set_probe_waits(ms(60), ms(200), ms(10), ms(100));
    seed(&mut s, &opts);
    let screen = s.screen().expect("the point");
    let point = s.turn_of(screen);
    let decided = s
        .turn_end_at_point(&point, Some(Duration::from_secs(182)), &opts)
        .unwrap_or_else(|e| panic!("{e:?}"));
    assert_eq!(host.said("account"), ["account:turn-end"], "advised");
    s.turn_live = true;
    let mut heard = Heard::default();
    let probed = s
        .usage_probe(&point, &decided, &opts, &mut heard)
        .unwrap_or_else(|e| panic!("the probe: {e:?}\n{:#?}", heard.said));
    (decided, probed, heard)
}

/// A PROBE WHOSE READ GIVES NO SAMPLE CLOSES ITS OWN PANEL BEHIND THE HOST'S DOOR: the panel never
/// settles, the probe gives up on its read and presses its Escape under the duty its `/usage` was
/// typed under, the panel closes and the composer is back before the probe ends, and its host
/// heard the panel stand and go. Negative control: the same Escape sent with no duty (the path
/// before HP1) is refused at the door with the words the stranded worker's journal carries, and
/// the panel stands.
#[test]
fn a_probe_whose_read_gives_no_sample_closes_its_own_panel_behind_the_hosts_door() {
    let (dir, ledger) = ledger_at("probe-door");
    let host = ProbeHost::probing(true);
    let mut m = probe_mock(
        vec![ended(PROBE_SAID)],
        &[panel_fx::USAGE_REFRESHING],
        &host,
    );
    let mut door = HostDoor {
        mock: &mut m,
        typed: Vec::new(),
        refused: Vec::new(),
    };
    let (_, probed, _) = door_probe(&mut door, &host, &ledger, false, |_, _| {});
    assert_eq!(probed, Probed::Done, "the panel closed, the probe ended");
    assert!(door.refused.is_empty(), "{:#?}", door.refused);
    let walls: Vec<&str> = door
        .typed
        .iter()
        .filter(|(_, w)| w.contains(" send ") || w.contains(" key "))
        .map(|(duty, w)| {
            assert_eq!(*duty, Scope::Walls, "{w}");
            w.as_str()
        })
        .collect();
    assert!(
        walls.last().is_some_and(|w| w.ends_with(" esc")),
        "{walls:#?}"
    );
    assert_eq!(host.said("probe"), ["probe:claimed", "probed:unsettled"]);
    assert_eq!(host.panel_told(), [true, false], "told while it stood");
    assert_eq!(probe_writes(&m), ["/usage", "esc"]);
    assert_eq!(world(&mut m).at, PanelAt::Rest, "the panel is gone");
    let _ = std::fs::remove_dir_all(&dir);

    // CONTROL: the same panel standing, its Escape sent with no duty.
    let mut m = panel_standing(panel_fx::USAGE_REFRESHING, &host);
    let mut door = HostDoor {
        mock: &mut m,
        typed: Vec::new(),
        refused: Vec::new(),
    };
    {
        let mut s = behind_the_door(&mut door);
        let grid = s.grid().expect("the panel");
        let panel = aterm_phase::settings_panel(&grid.rows).expect("the panel stands");
        let guard = panel.header_guard(&grid.rows);
        let bare = s.guarded_key(grid.seq, &guard, "esc", grid.human_seq);
        assert!(
            matches!(&bare, Err(Fail::Hard(why)) if why.contains("ERR denied supervisor input authority")),
            "{bare:?}"
        );
        assert_eq!(
            s.ctl.mock.panel.as_ref().map(|w| &w.at),
            Some(&PanelAt::Open)
        );
        let walled = s.with_duty(Scope::Walls, |s| {
            s.guarded_key(grid.seq, &guard, "esc", grid.human_seq)
        });
        assert!(matches!(walled, Ok(Dismiss::Pressed { .. })), "{walled:?}");
    }
    assert_eq!(door.refused.len(), 1, "{:#?}", door.refused);
    assert!(door.refused[0].ends_with(" esc"), "{:#?}", door.refused);
    assert_eq!(world(&mut m).at, PanelAt::Rest);
}

/// THE LAND CHECK IS TYPED AFTER A TURN END THAT ALSO TRIGGERS THE PROBE: the host advises a probe
/// at the turn end of Land's defense, the instruction typed before it; that point's act is the
/// land check, and the probe there shuts on it (`land-check`) with no `/usage` typed and no probe
/// claimed, so the check is what the loop types at that turn end. Negative control: the same
/// advice at the same reply under Cruise, which owes no land check, claims the probe, types
/// `/usage` and closes its panel.
#[test]
fn the_land_check_is_typed_at_a_turn_end_that_also_makes_the_probe_due() {
    const DEFENSE: &str = "Nothing left: all of it is merged into the remote main branch.";
    let (dir, ledger) = ledger_at("probe-land-check");
    let host = ProbeHost::probing(true);
    let mut m = probe_mock(
        vec![ended(DEFENSE)],
        &[panel_fx::USAGE_REFRESHING, panel_fx::USAGE_SETTLED],
        &host,
    );
    m.autopilot = Some("land");
    let meta = m.meta_line();
    let mut door = HostDoor {
        mock: &mut m,
        typed: Vec::new(),
        refused: Vec::new(),
    };
    let (decided, probed, heard) = door_probe(&mut door, &host, &ledger, true, |s, opts| {
        // The session's word as the loop read it, and Land's instruction the loop typed, booked
        // as the loop books its act: this reply is its answer, never the person's own exchange.
        s.note_meta(&meta);
        let screen = s.screen().expect("the point");
        let r = s.turn_end_reread(&screen, opts);
        let instruction = TurnEndAction::Type {
            text: LAND.to_string(),
            rule_id: RULE_LAND,
        };
        s.turn_end.acted(&instruction, &r, s.fx.mono());
    });
    let _ = std::fs::remove_dir_all(&dir);
    assert_eq!(decided.rule_id(), Some(RULE_LAND_CHECK), "{decided:?}");
    assert!(
        matches!(&decided, TurnEndAction::Type { text, .. } if text.starts_with(&LAND_CHECK[..40])),
        "{decided:?}"
    );
    assert_eq!(probed, Probed::No, "{:#?}", heard.said);
    assert!(
        heard
            .said
            .iter()
            .any(|l| l.starts_with("SKIPPED ") && l.ends_with(" gate=land-check")),
        "{:#?}",
        heard.said
    );
    assert!(host.said("probe").is_empty(), "never claimed");
    assert!(host.panel_told().is_empty());
    assert!(probe_writes(&m).is_empty(), "{:#?}", m.requests);

    // CONTROL: the same advice at the same reply under Cruise claims the probe.
    let (dir, ledger) = ledger_at("probe-land-check-control");
    let host = ProbeHost::probing(true);
    let mut m = probe_mock(
        vec![ended(DEFENSE)],
        &[panel_fx::USAGE_REFRESHING, panel_fx::USAGE_SETTLED],
        &host,
    );
    let mut door = HostDoor {
        mock: &mut m,
        typed: Vec::new(),
        refused: Vec::new(),
    };
    let (decided, probed, heard) = door_probe(&mut door, &host, &ledger, true, |_, _| {});
    let _ = std::fs::remove_dir_all(&dir);
    assert_ne!(decided.rule_id(), Some(RULE_LAND_CHECK), "{decided:?}");
    assert_eq!(probed, Probed::Done, "{:#?}", heard.said);
    assert_eq!(host.said("probe"), ["probe:claimed", "probed:ok"]);
    assert_eq!(probe_writes(&m), ["/usage", "esc"]);
}
