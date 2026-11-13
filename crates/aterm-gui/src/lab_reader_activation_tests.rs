// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;

fn restored_lab(incoming_handoff_pending: bool) -> crate::App {
    let mut app = crate::App::headless_for_test();
    let mut layout = app.capture_restore_manifest().windows.remove(0);
    layout.tabs.clear();
    layout.native_tabs.clear();
    layout.tab_order.clear();
    layout.active_item = Some(0);
    layout.workers_in_front = false;
    layout.restored_tabs = vec![crate::restore::RestoredTab {
        root: crate::restore::RestoredSplitTree::leaf(crate::restore::RestoredView::Native(
            crate::restore::NativeLeafRestore::settings("/lab".into()),
        )),
        focused_path: Vec::new(),
        zoomed: false,
        parked: false,
        ephemeral: false,
        fleet: false,
    }];
    app.incoming_handoff_pending = incoming_handoff_pending;
    app.restore_into_window(crate::WindowId(0), layout);
    assert!(app.lab_page_shown());
    assert!(app.proxy.is_none());
    app
}

fn first_read_once(app: &mut crate::App, tick: Instant) {
    assert!(app.lab_screen.next_read.is_none());
    assert!(app.lab_screen.accounts.is_none());
    assert_eq!(app.lab_state_deadline_at(tick), Some(tick));
    app.service_lab_state(tick);
    let next = app
        .lab_screen
        .next_read
        .expect("the next read is scheduled");
    assert!(app.lab_screen.pending.is_none());
    // Taking the inert publication makes another admission observable without a worker.
    assert!(app.lab_screen.accounts.take().is_some());
    assert_eq!(app.lab_state_deadline_at(tick), Some(next));
    app.service_lab_state(next - Duration::from_nanos(1));
    assert!(app.lab_screen.accounts.is_none());
    assert_eq!(app.lab_screen.next_read, Some(next));
}

#[test]
fn a_restored_lab_reads_on_its_first_service_tick() {
    let mut app = restored_lab(false);
    let tick = Instant::now() - Duration::from_secs(1);
    first_read_once(&mut app, tick);
}

#[cfg(unix)]
#[test]
fn a_successors_lab_waits_for_activation_before_its_first_read() {
    let mut app = restored_lab(true);
    let tick = Instant::now() - Duration::from_secs(1);
    let sequence = app.lab_screen.sequence;
    assert_eq!(app.lab_state_deadline_at(tick), None);
    app.service_lab_state(tick);
    app.start_lab_accounts_refresh();
    assert_eq!(app.lab_screen.sequence, sequence);
    assert!(app.lab_screen.accounts.is_none());
    assert!(app.lab_screen.next_read.is_none());
    assert!(app.lab_screen.pending.is_none());

    app.incoming_handoff_pending = false;
    app.settle_harness_reading_at_commit();
    first_read_once(&mut app, tick);
}

#[test]
fn an_occluded_lab_waits_until_shown_before_its_first_read() {
    let mut app = restored_lab(false);
    let tick = Instant::now() - Duration::from_secs(1);
    app.windows.get_mut(&crate::WindowId(0)).unwrap().occluded = true;
    assert_eq!(app.lab_state_deadline_at(tick), None);
    app.service_lab_state(tick);
    assert!(app.lab_screen.accounts.is_none());
    assert!(app.lab_screen.next_read.is_none());

    app.windows.get_mut(&crate::WindowId(0)).unwrap().occluded = false;
    first_read_once(&mut app, tick);
}
