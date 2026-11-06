// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Setup Stop uses process control, with the admitted child's receipt and current image.

use super::*;
use crate::clt_install::stop::{Reach, Running};
use aterm_uds::process::{Birth, Identity};
use std::cell::RefCell;
use std::collections::VecDeque;

const PID: i32 = 2_000_000_123;
const BIRTH: u64 = 7_654_321;

thread_local! {
    static READS: RefCell<VecDeque<Option<Running>>> = const { RefCell::new(VecDeque::new()) };
    static SIGNALS: RefCell<Vec<Identity>> = const { RefCell::new(Vec::new()) };
}

fn running() -> Running {
    Running {
        identity: Identity {
            pid: PID,
            birth: Birth(BIRTH),
            idversion: Some(4),
        },
        installer: true,
    }
}

fn read(_: i32) -> Option<Running> {
    READS.with_borrow_mut(|reads| {
        if reads.len() > 1 {
            reads.pop_front().flatten()
        } else {
            reads.front().copied().flatten()
        }
    })
}

fn interrupt(identity: &Identity) -> std::io::Result<()> {
    SIGNALS.with_borrow_mut(|signals| signals.push(*identity));
    Ok(())
}

fn owned_install(mark: &std::path::Path) -> (App, u64, std::fs::File) {
    READS.with_borrow_mut(|reads| *reads = VecDeque::from([Some(running())]));
    SIGNALS.with_borrow_mut(Vec::clear);
    let (mut app, wid) = page();
    app.workstation_setup.developer_tools_mark = Mark::at(mark.to_path_buf());
    let id = book(&mut app, wid, 4);
    let (mut child, input) = registered_piped_session(id);
    child.pid = PID;
    child.shell_identity = aterm_pty::ShellIdentity::from_parts(PID, Some(BIRTH), false);
    assert!(app.install_spawned_tab(TabSpawned {
        id,
        window: wid,
        session: Ok(child),
        reader_gate: None
    }));
    assert_eq!(drain(&input), go_line());
    app.workstation_setup.developer_tools_stop = Reach {
        read: Some(read),
        interrupt: Some(interrupt),
    };
    app.workstation_setup.developer_tools_probe = Some(installing);
    (app, id, input)
}

#[test]
fn stop_interrupts_an_owned_installer_waiting_through_an_outage_once_without_reauthorization() {
    let root = aterm_tempfile::tempdir().unwrap();
    let mark = root.path().join("command-line-tools-tab");
    let (mut app, id, input) = owned_install(&mark);
    assert!(app.developer_tools_install_active());
    assert_eq!(step(&mut app), Some(ToolsInstall::Installing));
    app.stop_initial_setup_permissions();
    app.stop_initial_setup_permissions();
    assert_eq!(SIGNALS.with_borrow(Clone::clone), [running().identity]);
    assert!(
        drain(&input).is_empty(),
        "Stop injects no terminal command or input"
    );
    assert_eq!(app.workstation_setup.developer_tools_want, None);
    assert_eq!(app.workstation_setup.developer_tools, Install::Opened);
    assert!(app.workstation_setup.developer_tools_mark.opened());
    app.developer_tools_install_ended(
        id,
        Some(aterm_pty::ChildExit::Code(i32::from(
            aterm_types::macos_setup::COMMAND_LINE_TOOLS_TAB_STOPPED,
        ))),
    );
    app.want_developer_tools_install(4, true);
    assert!(
        !app.developer_tools_install_due(false),
        "a stopped install does not reopen by itself"
    );
    assert!(SIGNALS.with_borrow(|signals| signals.len()) == 1);
}

#[test]
fn stop_never_interrupts_a_replaced_child_changed_receipt_or_another_user_program() {
    for changed in ["child", "birth", "program", "exec", "receipt", "finished"] {
        let root = aterm_tempfile::tempdir().unwrap();
        let mark = root.path().join("command-line-tools-tab");
        let (mut app, id, input) = owned_install(&mark);
        match changed {
            "child" => app.pool.sessions.get_mut(&id).unwrap().session.pid += 1,
            "birth" => READS.with_borrow_mut(|reads| {
                let mut newer = running();
                newer.identity.birth = Birth(BIRTH + 1);
                *reads = VecDeque::from([Some(newer)]);
            }),
            "program" => READS.with_borrow_mut(|reads| {
                let mut other = running();
                other.installer = false;
                *reads = VecDeque::from([Some(other)]);
            }),
            "exec" => READS.with_borrow_mut(|reads| {
                let mut other = running();
                other.identity.idversion = Some(5);
                *reads = VecDeque::from([Some(running()), Some(other)]);
            }),
            "receipt" => {
                let folder = mark.with_file_name("command-line-tools-tab.children");
                let path = std::fs::read_dir(folder)
                    .unwrap()
                    .next()
                    .unwrap()
                    .unwrap()
                    .path();
                std::fs::write(path, b"changed custody\n").unwrap();
            }
            "finished" => {
                app.pool
                    .get(id)
                    .unwrap()
                    .child_exit
                    .set(aterm_pty::ChildExit::Code(0))
                    .unwrap();
            }
            _ => unreachable!(),
        }
        app.stop_initial_setup_permissions();
        assert!(SIGNALS.with_borrow(Vec::is_empty), "{changed}");
        assert!(
            drain(&input).is_empty(),
            "{changed}: no input to a replacement"
        );
    }
}
