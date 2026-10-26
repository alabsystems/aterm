// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;

fn waiting_with_old_activation(keep_dock: bool) -> (aterm_tempfile::TempDir, Fake) {
    let (root, mut host) = deferred_widgets_restore();
    host.full_disk_access = Some(true);
    host.fail_restart_once = true;
    let failed = execute(&mut host, root.path(), Action::Restore).unwrap();
    assert!(failed.rows.iter().any(|row| row.name == "Reload Dock"));
    assert!(
        pending_refresh(root.path())
            .unwrap()
            .contains(&nc_widgets())
    );

    host.full_disk_access = Some(false);
    host.fail_restart_once = keep_dock;
    execute(&mut host, root.path(), Action::Restore).unwrap();
    assert_eq!(restore_waiting(root.path()).unwrap(), [nc_widgets()]);
    let pending = pending_refresh(root.path()).unwrap();
    assert!(pending.contains(&nc_widgets()));
    assert_eq!(
        pending
            .iter()
            .any(|target| target.domain == "com.apple.WindowManager"),
        keep_dock
    );
    if !keep_dock {
        assert_eq!(pending, [nc_widgets()]);
    }
    assert!(host.values[&nc_widgets()].same(&populated()));
    host.full_disk_access = Some(true);
    (root, host)
}

fn relaunches(host: &Fake, process: &str) -> usize {
    host.relaunches(process) + host.restarts.iter().filter(|name| *name == process).count()
}

#[test]
fn restore_activation_follow_up_retires_only_the_debt_it_completed() {
    for keep_dock in [false, true] {
        let (root, mut host) = waiting_with_old_activation(keep_dock);
        let _lock = HeldLock::take(root.path()).unwrap();
        let mut residual = pending_refresh(root.path()).unwrap();
        residual.retain(|target| *target != nc_widgets());
        let (writes, dock, nc) = (
            host.writes.len(),
            relaunches(&host, "Dock"),
            relaunches(&host, "NotificationCenter"),
        );
        let report = follow_up_at(root.path(), &mut host, BUILD, std::time::SystemTime::now())
            .unwrap()
            .expect("the earlier Restore is still owed");
        assert_eq!(
            report.restore.unwrap().outcome.rows,
            [(Some(Setting::Widgets), PutBack::Back)]
        );
        assert!(host.values[&nc_widgets()].same(&populated()));
        assert_eq!(
            host.writes.len(),
            writes,
            "an already-original value needs only its activation"
        );
        assert_eq!(
            relaunches(&host, "Dock"),
            dock,
            "unrelated activation is not performed"
        );
        assert_eq!(relaunches(&host, "NotificationCenter"), nc + 1);
        assert!(restore_waiting(root.path()).unwrap().is_empty());
        assert_eq!(
            pending_refresh(root.path()).unwrap(),
            residual,
            "successful FDA recovery must not leave its fulfilled activation owed"
        );
        let effects = (host.writes.len(), host.restarts.len());
        assert!(
            follow_up_at(root.path(), &mut host, BUILD, std::time::SystemTime::now())
                .unwrap()
                .is_none()
        );
        assert_eq!((host.writes.len(), host.restarts.len()), effects);
        if !keep_dock {
            // A person's later widget choice already matches setup. A stale activation
            // record would still demand --apply and relaunch Notification Center again.
            host.values.insert(nc_widgets(), cleared());
            host.select(Selection::all());
            let checked = execute(&mut host, root.path(), Action::Check).unwrap();
            assert!(
                !checked
                    .rows
                    .iter()
                    .any(|row| row.name == "Preference activation")
            );
            execute(&mut host, root.path(), Action::Apply).unwrap();
            assert_eq!(
                relaunches(&host, "NotificationCenter"),
                nc + 1,
                "no relaunch for an already-fulfilled old obligation"
            );
        }
    }
}

#[test]
fn restore_activation_failed_relaunch_keeps_its_old_debt() {
    let (root, mut host) = waiting_with_old_activation(true);
    let _lock = HeldLock::take(root.path()).unwrap();
    let pending = std::fs::read(root.path().join("pending-refresh.json")).unwrap();
    let dock = relaunches(&host, "Dock");
    host.fail_restart_once = true;
    let report = follow_up_at(root.path(), &mut host, BUILD, std::time::SystemTime::now())
        .unwrap()
        .expect("the original is saved, with activation left for login");
    assert_eq!(
        report.restore.unwrap().outcome.rows,
        [(Some(Setting::Widgets), PutBack::AtLogin)]
    );
    assert_eq!(
        std::fs::read(root.path().join("pending-refresh.json")).unwrap(),
        pending
    );
    assert_eq!(relaunches(&host, "Dock"), dock);
    assert!(
        restore_waiting(root.path()).unwrap().is_empty(),
        "a reported relaunch failure keeps the existing at-login policy"
    );
}

#[test]
fn restore_activation_unreadable_debt_refuses_before_follow_up_effects() {
    let (root, mut host) = waiting_with_old_activation(true);
    let _lock = HeldLock::take(root.path()).unwrap();
    let path = root.path().join("pending-refresh.json");
    let pending = std::fs::read(&path).unwrap();
    let waiting = std::fs::read(root.path().join(RESTORE_WAITING)).unwrap();
    std::fs::write(&path, b"unreadable activation record").unwrap();
    let effects = (host.writes.len(), host.restarts.len(), host.lines.len());
    let result = follow_up_at(root.path(), &mut host, BUILD, std::time::SystemTime::now());
    assert!(
        result.is_err(),
        "unreadable activation debt cannot be silently ignored by the follow-up"
    );
    assert_eq!(
        (host.writes.len(), host.restarts.len(), host.lines.len()),
        effects
    );
    assert_eq!(
        std::fs::read(root.path().join(RESTORE_WAITING)).unwrap(),
        waiting
    );
    std::fs::write(&path, pending).unwrap();
    let mut residual = pending_refresh(root.path()).unwrap();
    residual.retain(|target| *target != nc_widgets());
    follow_up_at(root.path(), &mut host, BUILD, std::time::SystemTime::now())
        .unwrap()
        .unwrap();
    assert_eq!(pending_refresh(root.path()).unwrap(), residual);
    assert!(restore_waiting(root.path()).unwrap().is_empty());
}
