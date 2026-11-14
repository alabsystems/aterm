// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! A restart that reached its close uncommitted (`state=closed`): what the record keeps, and
//! what its watch reads from an injected kernel and session files.

use super::super::agent_image::FileId;
use super::super::upgrade::SessionFile;
use super::retirement_debt_tests::pending;
use super::*;

/// The conversation the pending record's relaunch resumes in these fixtures.
pub(in crate::harness) const CONVERSATION: &str = "bbbbbbbb-0000-0000-0000-0000000000cc";

/// An injected kernel for a closed restart's watch: who each process's parent is, the shell's
/// live children and the image each runs, each pid's kernel start and presence, whether the
/// shell holds its tab, and the machine's boot. Every other fact reads as unknown, and nothing
/// reads the machine.
#[derive(Clone, Default)]
pub(in crate::harness) struct Watched {
    pub(in crate::harness) parents: Vec<(u32, u32)>,
    pub(in crate::harness) children: Option<Vec<u32>>,
    /// The identity of the image each pid named runs; any other pid's image does not read.
    pub(in crate::harness) images: Vec<(u32, FileId)>,
    pub(in crate::harness) holds_tab: Option<bool>,
    /// The kernel start of every pid [`Self::starts`] does not name.
    pub(in crate::harness) shell_start: Option<String>,
    /// A pid's own kernel start, read before [`Self::shell_start`].
    pub(in crate::harness) starts: Vec<(u32, String)>,
    /// Whether a process holds each pid named; any other pid's presence does not read.
    pub(in crate::harness) alive: Vec<(u32, bool)>,
    pub(in crate::harness) boot: Option<native_act::BootStamp>,
}

impl upgrade_drive::Kernel for Watched {
    fn parent(&self, pid: u32) -> Option<u32> {
        self.parents
            .iter()
            .find(|(child, _)| *child == pid)
            .map(|(_, parent)| *parent)
    }
    fn terminal(&self, _: u32) -> Option<(u32, String)> {
        None
    }
    fn process_start(&self, pid: u32) -> Option<String> {
        self.starts
            .iter()
            .find(|(named, _)| *named == pid)
            .map(|(_, start)| start.clone())
            .or_else(|| self.shell_start.clone())
    }
    fn restart_background(&self, _: u32) -> Vec<String> {
        vec![upgrade_drive::UNREADABLE_TABLE.to_string()]
    }
    fn restart_held(&self, _: u32) -> Option<Vec<super::super::upgrade::Held>> {
        None
    }
    fn shell_holds_tab(&self, _: u32) -> Option<bool> {
        self.holds_tab
    }
    fn children(&self, _: u32) -> Option<Vec<u32>> {
        self.children.clone()
    }
    fn image_file(&self, pid: u32) -> Option<FileId> {
        self.images
            .iter()
            .find(|(named, _)| *named == pid)
            .map(|(_, image)| *image)
    }
    fn holds_pid(&self, pid: u32) -> Option<bool> {
        self.alive
            .iter()
            .find(|(named, _)| *named == pid)
            .map(|(_, alive)| *alive)
    }
    fn machine_boot(&self) -> Option<native_act::BootStamp> {
        self.boot.clone()
    }
}

/// A kernel that supplies no children reader: the trait's default, which reads no machine.
struct Inert(Watched);

impl upgrade_drive::Kernel for Inert {
    fn parent(&self, pid: u32) -> Option<u32> {
        self.0.parent(pid)
    }
    fn terminal(&self, _: u32) -> Option<(u32, String)> {
        None
    }
    fn process_start(&self, pid: u32) -> Option<String> {
        self.0.process_start(pid)
    }
    fn restart_background(&self, _: u32) -> Vec<String> {
        vec![upgrade_drive::UNREADABLE_TABLE.to_string()]
    }
    fn restart_held(&self, _: u32) -> Option<Vec<super::super::upgrade::Held>> {
        None
    }
}

/// The pending record's restart with the test process as the shell it typed at: a live process
/// whose kernel start both the plan and the live kernel name, on every Unix.
pub(in crate::harness) fn closing(st: &mut St) -> Box<Prepare> {
    let mut prepare = retained_prepare(st).unwrap();
    let NativePlan::Restart { shell, .. } = &mut prepare.plan else {
        unreachable!("the pending plan is a restart")
    };
    *shell = Process::observe(std::process::id()).expect("the test process");
    st.native_prepare = Request::prepare(*prepare.clone()).encode().unwrap();
    prepare
}

/// What the live kernel says of `prepare`'s shell: its kernel start, as the watch compares it.
pub(in crate::harness) fn shell_start(prepare: &Prepare) -> String {
    let NativePlan::Restart { shell, .. } = &prepare.plan else {
        unreachable!("a restart plan")
    };
    let start = vendor_start(shell);
    assert!(!start.is_empty(), "the live shell's start reads");
    start
}

pub(in crate::harness) fn registered(pid: u32, conversation: &str) -> SessionFile {
    SessionFile {
        pid,
        session_id: conversation.into(),
        cwd: "/".into(),
        version: "2.1.0".into(),
        status: "idle".into(),
        status_updated_at_ms: 0,
        proc_start: String::new(),
        kind: "interactive".into(),
        entrypoint: "cli".into(),
        claude_dir: None,
        profile: None,
    }
}

const RELAUNCHED: u32 = 4242;

fn closed_id() -> String {
    "ab".repeat(16)
}

#[test]
fn a_closed_reply_drops_the_operation_and_keeps_its_watch() {
    let mut st = pending();
    let prepare = retained_prepare(&st).unwrap();
    let before = st.clone();
    // Negative control: another operation's close changes nothing.
    assert_eq!(
        terminal_reply(&mut st, &Reply::Closed("cd".repeat(16))),
        None
    );
    assert_eq!(st, before);
    assert_eq!(
        terminal_reply(&mut st, &Reply::Closed(closed_id())),
        Some(Recovery::Closed)
    );
    assert!(st.native.is_none() && st.native_prepare.is_empty());
    assert!(!st.native_rollback, "no rollback follows a close");
    assert_eq!(st.native_closed.as_deref(), Some(&*prepare));
    assert!(
        st.native_completed_prepare.is_none() && st.native_observation.is_none(),
        "a close is never recorded completed"
    );
    assert_eq!(st.native_noexec_prepare, before.native_noexec_prepare);
    assert_eq!(
        st.phase, before.phase,
        "the watch, not the reply, moves the lane"
    );
    assert_eq!(
        st.account_intent, before.account_intent,
        "a restart that resolved no account keeps its attribution"
    );
    assert!(coordinated(&st));
    assert_eq!(closed_refusal(&st), Err(CLOSED_GRANTEE));
    assert_eq!(refusal_word(Ok(Recovery::Closed)), "native-closed");
    // A repeated answer acknowledges the same watch; another id, or a later terminal reply of
    // the dropped operation, changes nothing.
    let closed = st.clone();
    for reply in [
        Reply::Closed(closed_id()),
        Reply::Closed("cd".repeat(16)),
        Reply::Committed(native_act::ClaimOutcome::Builtin),
        Reply::Decision(Decision::Retired(native_act::Retirement::Exited)),
        Reply::Lapsed(closed_id()),
    ] {
        let acknowledged = terminal_reply(&mut st, &reply);
        assert_eq!(
            acknowledged,
            (reply == Reply::Closed(closed_id())).then_some(Recovery::Closed),
            "{reply:?}"
        );
        assert_eq!(st, closed, "{reply:?}");
    }
    // The saved record keeps the watch, and with it the coordination.
    let reloaded = St::from_json(&st.to_json()).expect("the saved record reloads");
    assert_eq!(reloaded.native_closed, st.native_closed);
    assert!(reloaded.native_coordination);
}

#[test]
fn a_closed_cold_restart_forgets_the_account_it_never_attributed() {
    let mut st = pending();
    let mut prepare = retained_prepare(&st).unwrap();
    prepare.duty = Scope::Relaunch;
    let NativePlan::Restart { cold, .. } = &mut prepare.plan else {
        unreachable!()
    };
    *cold = Some(Box::new(native_act::ColdIntent {
        login: None,
        home: None,
        auth_observed: true,
        route: super::super::accounts::AuthRoute::Stored,
        conversation: None,
        profile: None,
        mode: super::super::accounts::Mode::Wall,
        billing: native_act::ColdBilling::Keep,
    }));
    st.native_prepare = Request::prepare(*prepare).encode().unwrap();
    assert!(st.account_intent.is_some());
    assert_eq!(
        terminal_reply(&mut st, &Reply::Closed(closed_id())),
        Some(Recovery::Closed)
    );
    assert_eq!(
        st.account_intent, None,
        "only a committed cold reply names the account its helper chose"
    );
    assert_eq!(
        st.back_account_intent,
        pending().back_account_intent,
        "the rollback's own attribution is not the forward's"
    );
}

#[test]
fn a_closed_reply_never_drops_a_terminal_record_of_the_same_id_or_a_hold() {
    for done in ["completed", "noexec", "held"] {
        let mut st = pending();
        let prepare = retained_prepare(&st).unwrap();
        match done {
            "completed" => st.native_completed_prepare = Some(prepare),
            "noexec" => st.native_noexec_prepare = Some(prepare),
            _ => st.native_retired_unissued = true,
        }
        let before = st.clone();
        let outcome = terminal_reply(&mut st, &Reply::Closed(closed_id()));
        assert_ne!(outcome, Some(Recovery::Closed), "{done}");
        assert_eq!(st, before, "{done}");
    }
}

#[test]
fn the_watch_adopts_a_relaunch_holding_the_tab_clears_a_childless_shell_and_holds_otherwise() {
    let mut st = pending();
    let prepare = closing(&mut st);
    let NativePlan::Restart { shell, process, .. } = &prepare.plan else {
        unreachable!()
    };
    let (shell, old) = (shell.pid, process.pid);
    let start = shell_start(&prepare);
    let adopting = Watched {
        parents: vec![(RELAUNCHED, shell)],
        children: Some(vec![RELAUNCHED]),
        holds_tab: Some(false),
        shell_start: Some(start.clone()),
        ..Watched::default()
    };
    let relaunched = registered(RELAUNCHED, CONVERSATION);
    let files = vec![relaunched.clone()];
    let watch = |k: &dyn upgrade_drive::Kernel, files: Option<&[SessionFile]>, fresh: bool| {
        closed_watch(&prepare, k, files, (!fresh).then_some(CONVERSATION))
    };
    assert_eq!(
        watch(&adopting, Some(&files), false),
        Watch::Adopt(Box::new(relaunched.clone()))
    );
    // A fresh start's agent registers another conversation, which only a fresh watch adopts.
    let elsewhere = vec![registered(
        RELAUNCHED,
        "cccccccc-0000-0000-0000-0000000000cc",
    )];
    assert_eq!(watch(&adopting, Some(&elsewhere), false), Watch::Hold);
    assert_eq!(
        watch(&adopting, Some(&elsewhere), true),
        Watch::Adopt(Box::new(elsewhere[0].clone()))
    );
    // Negative controls: each fact that does not prove the relaunch holds the watch.
    let holds = |k: Watched, files: Option<&[SessionFile]>, why: &str| {
        assert_eq!(watch(&k, files, false), Watch::Hold, "{why}");
    };
    holds(
        Watched {
            holds_tab: Some(true),
            ..adopting.clone()
        },
        Some(&files),
        "the shell still holds the tab",
    );
    holds(
        Watched {
            holds_tab: None,
            ..adopting.clone()
        },
        Some(&files),
        "the tab's holder did not read",
    );
    holds(
        Watched {
            parents: vec![(RELAUNCHED, shell + 1)],
            ..adopting.clone()
        },
        Some(&files),
        "another shell's child",
    );
    holds(
        Watched {
            parents: Vec::new(),
            ..adopting.clone()
        },
        Some(&files),
        "an unread parent",
    );
    holds(
        Watched {
            parents: vec![(old, shell)],
            children: Some(vec![old]),
            ..adopting.clone()
        },
        Some(&[registered(old, CONVERSATION)]),
        "the old agent itself",
    );
    holds(adopting.clone(), None, "unread files with a live child");
    holds(
        Watched {
            shell_start: None,
            starts: vec![(shell, "Thu Jan  1 00:00:01 1970".into())],
            ..adopting.clone()
        },
        Some(&files),
        "the shell's pid names another process, and the old agent's exit does not read",
    );
    holds(
        Watched {
            shell_start: None,
            children: Some(Vec::new()),
            ..adopting.clone()
        },
        None,
        "the shell's start did not read",
    );
    // A childless shell clears, files read or not; a child whose image does not read, or an
    // unread list, holds. Which children a watch rules out by their image:
    // a_watch_clears_beside_the_shells_other_children_and_holds_on_one_that_runs_a_grant_image.
    let childless = Watched {
        parents: Vec::new(),
        children: Some(Vec::new()),
        holds_tab: Some(true),
        shell_start: Some(start),
        ..Watched::default()
    };
    assert_eq!(watch(&childless, Some(&files), false), Watch::Clear);
    assert_eq!(watch(&childless, None, false), Watch::Clear);
    holds(
        Watched {
            children: Some(vec![RELAUNCHED + 1]),
            ..childless.clone()
        },
        Some(&files),
        "an unregistered child whose image does not read may be the grantee",
    );
    holds(
        Watched {
            children: None,
            ..childless.clone()
        },
        Some(&files),
        "an unread child list",
    );
    assert_eq!(
        watch(&Inert(childless), Some(&files), false),
        Watch::Hold,
        "a kernel without a children reader never clears"
    );
}

/// A child the restart's shell keeps that the restart never started: a prompt helper
/// (powerlevel10k's gitstatusd, a zsh-async worker) or a job of the person's.
const HELPER: u32 = 6161;

/// The identity of a file on the fixtures' device `dev`.
const fn file(dev: u64, ino: u64) -> FileId {
    FileId { dev, ino }
}

/// A fixture image of the file `id` names.
fn image_of(id: FileId) -> Image {
    Image {
        path: format!("/model/image-{}", id.ino).into_bytes(),
        device: id.dev,
        inode: id.ino,
    }
}

/// THE FIX for a watch its shell's other children held for good: the restart's shell, alive
/// and the process it typed at, keeps a child the restart never started (a prompt helper, a
/// job of the person's) after the grantee exited, its session file left behind. The watch reads
/// each child's image and rules out every child that runs none the grant may run (the
/// launcher, a command's own image or the one it settles on), so it clears and the exit lane
/// starts the conversation again; it held before, whatever the child ran. Negative controls: a
/// child that runs any image the grant may run, or whose image does not read, holds.
#[test]
fn a_watch_clears_beside_the_shells_other_children_and_holds_on_one_that_runs_a_grant_image() {
    let (mut st, mut prepare, shell, _) = watched_record(None);
    let (launcher_id, forward_id, settles_id, rollback_id) =
        (file(1, 10), file(1, 11), file(1, 12), file(1, 13));
    let NativePlan::Restart {
        launcher,
        forward,
        rollback,
        shell: typed_at,
        ..
    } = &mut prepare.plan
    else {
        unreachable!()
    };
    let shell_image = file(typed_at.image.device, typed_at.image.inode);
    *launcher = image_of(launcher_id);
    forward.image = image_of(forward_id);
    forward.settles = Some(image_of(settles_id));
    let rollback = rollback.as_mut().expect("the pending plan rolls back");
    rollback.image = image_of(rollback_id);
    rollback.settles = None;
    st.native_closed = Some(prepare.clone());
    let watched = st.clone();
    // The grantee exited; its file names a pid whose parent no longer reads.
    let files = vec![registered(RELAUNCHED, CONVERSATION)];
    let beside = |children: &[(u32, Option<FileId>)]| Watched {
        parents: children.iter().map(|(pid, _)| (*pid, shell)).collect(),
        children: Some(children.iter().map(|(pid, _)| *pid).collect()),
        images: children
            .iter()
            .filter_map(|(pid, image)| image.map(|image| (*pid, image)))
            .collect(),
        holds_tab: Some(true),
        shell_start: Some(shell_start(&prepare)),
        ..Watched::default()
    };
    let start_again = Fresh::Start {
        conversation: Some(CONVERSATION),
    };
    for (why, children) in [
        ("a prompt helper", vec![(HELPER, Some(file(1, 99)))]),
        (
            "a fork of the shell itself, a zpty worker",
            vec![(HELPER, Some(shell_image))],
        ),
        (
            "the same inode on another device, another file",
            vec![(HELPER, Some(file(2, settles_id.ino)))],
        ),
        (
            "two of them",
            vec![(HELPER, Some(file(1, 99))), (HELPER + 1, Some(shell_image))],
        ),
    ] {
        let k = beside(&children);
        for conversation in [Some(CONVERSATION), None] {
            assert_eq!(
                closed_watch(&prepare, &k, Some(&files), conversation),
                Watch::Clear,
                "{why} ({conversation:?})"
            );
        }
        assert_eq!(
            closed_watch(&prepare, &k, None, Some(CONVERSATION)),
            Watch::Clear
        );
        let mut settled = watched.clone();
        assert_eq!(
            settle_closed(&mut settled, start_again, || Some(files.clone()), &k),
            Ok(()),
            "{why}"
        );
        assert!(settled.native_closed.is_none(), "{why}");
    }
    // Negative controls: beside the helper, a child that runs an image the grant may run (the
    // grantee itself, unregistered, or one the watch cannot tell from it), or one whose image
    // does not read, may be the grantee.
    for (why, image) in [
        ("the launcher, granted or not", Some(launcher_id)),
        ("the forward command's own image", Some(forward_id)),
        ("the image the forward command settles on", Some(settles_id)),
        ("the rollback command's image", Some(rollback_id)),
        ("an image that does not read", None),
    ] {
        let k = beside(&[(HELPER, Some(file(1, 99))), (RELAUNCHED + 1, image)]);
        assert_eq!(
            closed_watch(&prepare, &k, Some(&files), Some(CONVERSATION)),
            Watch::Hold,
            "{why}"
        );
        let mut held = watched.clone();
        assert_eq!(
            settle_closed(&mut held, start_again, || Some(files.clone()), &k),
            Err(CLOSED_GRANTEE),
            "{why}"
        );
        assert_eq!(held, watched, "{why}");
    }
}

/// The live kernel reads a running image by the identity a restart's plan captures for it
/// ([`Image::at`]), so a grantee that runs a planned image always reads as one
/// ([`grant_images`]); a pid that names no process reads none.
#[test]
fn the_live_kernel_reads_a_running_image_as_the_plan_captures_it() {
    let exe = std::env::current_exe().expect("the test binary");
    let planned = Image::at(&exe).expect("the test binary's image");
    assert_eq!(
        upgrade_drive::Kernel::image_file(&upgrade_drive::Live, std::process::id()),
        Some(file(planned.device, planned.inode))
    );
    assert_eq!(
        upgrade_drive::Kernel::image_file(&upgrade_drive::Live, 0),
        None
    );
}

#[test]
fn a_fresh_plan_settles_the_watch_only_for_its_own_subject_or_a_childless_shell() {
    let mut st = pending();
    let prepare = closing(&mut st);
    let NativePlan::Restart { shell, .. } = &prepare.plan else {
        unreachable!()
    };
    let shell = shell.pid;
    let start = shell_start(&prepare);
    let adopting = Watched {
        parents: vec![(RELAUNCHED, shell)],
        children: Some(vec![RELAUNCHED]),
        holds_tab: Some(false),
        shell_start: Some(start.clone()),
        ..Watched::default()
    };
    // The registered agent is no child of the shell: it was started elsewhere.
    let childless = Watched {
        parents: vec![(RELAUNCHED, shell + 1)],
        children: Some(Vec::new()),
        ..adopting.clone()
    };
    let files = || Some(vec![registered(RELAUNCHED, CONVERSATION)]);
    assert_eq!(
        terminal_reply(&mut st, &Reply::Closed(closed_id())),
        Some(Recovery::Closed)
    );
    let watched = st.clone();
    let signal = |pid| Fresh::Signal {
        pid,
        conversation: CONVERSATION,
    };
    let start_again = Fresh::Start {
        conversation: Some(CONVERSATION),
    };
    for (fresh, k, settles) in [
        (signal(RELAUNCHED), &adopting, true),
        (signal(RELAUNCHED + 1), &adopting, false),
        (start_again, &adopting, false),
        (signal(RELAUNCHED + 1), &childless, true),
        (start_again, &childless, true),
    ] {
        let mut st = watched.clone();
        let settled = settle_closed(&mut st, fresh, files, k);
        assert_eq!(settled.is_ok(), settles, "{fresh:?}");
        assert_eq!(st.native_closed.is_none(), settles, "{fresh:?}");
        if !settles {
            assert_eq!(settled, Err(CLOSED_GRANTEE));
            assert_eq!(st, watched);
        }
    }
    // Without a watch nothing is read.
    let mut st = pending();
    assert_eq!(
        settle_closed(
            &mut st,
            start_again,
            || panic!("no watch reads no files"),
            &adopting
        ),
        Ok(())
    );
}

/// A canonical machine-and-boot stamp, `boot` its boot id's one repeated hex digit.
fn stamp(boot: char) -> native_act::BootStamp {
    native_act::BootStamp {
        host: format!("macos:{}", "a".repeat(32)),
        boot: boot.to_string().repeat(32),
    }
}

/// The pending record's restart, its shell the live test process, answered `closed`: the
/// record keeps its watch, and the restart's old agent and shell pids.
fn watched_record(boot: Option<native_act::BootStamp>) -> (St, Box<Prepare>, u32, u32) {
    let mut st = pending();
    let mut prepare = retained_prepare(&st).unwrap();
    prepare.boot = boot;
    st.native_prepare = Request::prepare(*prepare).encode().unwrap();
    let prepare = closing(&mut st);
    assert_eq!(
        terminal_reply(&mut st, &Reply::Closed(closed_id())),
        Some(Recovery::Closed)
    );
    let NativePlan::Restart { shell, process, .. } = &prepare.plan else {
        unreachable!()
    };
    let (shell, old) = (shell.pid, process.pid);
    (st, prepare, shell, old)
}

/// The pending record's restart answered `closed`: its operation dropped, its watch kept.
pub(in crate::harness) fn closed_record() -> St {
    watched_record(None).0
}

const HOLDER_START: &str = "Mon Nov  9 10:00:00 2026";

fn holder(pid: u32) -> SessionFile {
    SessionFile {
        proc_start: HOLDER_START.into(),
        ..registered(pid, CONVERSATION)
    }
}

/// THE FIX for a watch whose shell is gone (ATERM-KI-1112-151's follow-up): the restart's shell
/// exited or was reaped, its pid free or holding a process born at another time, so no child of
/// it can be read. The watch then adopts the conversation's one live registered holder, and
/// clears once nothing holds the conversation and the old agent is gone by its start, so the
/// conversation is relaunched and moved again; it holds only while one of those facts does not
/// read.
#[test]
fn a_watch_whose_shell_is_gone_adopts_the_conversations_holder_or_clears_once_none_holds_it() {
    let (st, prepare, shell, old) = watched_record(None);
    for reused in [false, true] {
        let gone = Watched {
            starts: if reused {
                vec![(shell, "Thu Jan  1 00:00:01 1970".into())]
            } else {
                Vec::new()
            },
            alive: vec![(shell, reused), (old, false), (RELAUNCHED, true)],
            ..Watched::default()
        };
        let live = Watched {
            starts: [gone.starts.clone(), vec![(RELAUNCHED, HOLDER_START.into())]].concat(),
            ..gone.clone()
        };
        let watch = |k: &Watched, files: Option<&[SessionFile]>, conversation: Option<&str>| {
            closed_watch(&prepare, k, files, conversation)
        };
        let files = vec![holder(RELAUNCHED)];
        assert_eq!(
            watch(&live, Some(&files), Some(CONVERSATION)),
            Watch::Adopt(Box::new(files[0].clone())),
            "reused={reused}"
        );
        // Nothing holds the conversation: no file, a file whose process is gone, or one whose
        // pid now names a process born at another time.
        for (why, k, files) in [
            ("no file", live.clone(), Vec::new()),
            (
                "its process is gone",
                Watched {
                    alive: vec![(shell, reused), (old, false), (RELAUNCHED, false)],
                    ..live.clone()
                },
                files.clone(),
            ),
            (
                "its pid was reused",
                Watched {
                    starts: [
                        gone.starts.clone(),
                        vec![(RELAUNCHED, "Tue Nov 10 10:00:00 2026".into())],
                    ]
                    .concat(),
                    ..live.clone()
                },
                files.clone(),
            ),
            (
                "another conversation's",
                live.clone(),
                vec![SessionFile {
                    session_id: "cccccccc-0000-0000-0000-0000000000cc".into(),
                    ..holder(RELAUNCHED)
                }],
            ),
        ] {
            assert_eq!(
                watch(&k, Some(&files), Some(CONVERSATION)),
                Watch::Clear,
                "{why} (reused={reused})"
            );
        }
        // A fresh start's watch reads no conversation: its old agent gone, it clears.
        assert_eq!(watch(&live, Some(&files), None), Watch::Clear);
        // Negative controls: a fact that does not read, or two holders, holds.
        for (why, k, files) in [
            ("the files did not read", live.clone(), None),
            (
                "the holder's start did not read",
                gone.clone(),
                Some(files.clone()),
            ),
            (
                "the old agent runs by its start",
                Watched {
                    alive: vec![(shell, reused), (old, true), (RELAUNCHED, true)],
                    starts: [
                        live.starts.clone(),
                        vec![(old, vendor_start(&prepare_old(&prepare)))],
                    ]
                    .concat(),
                    ..live.clone()
                },
                Some(files.clone()),
            ),
            (
                "the old agent's exit did not read",
                Watched {
                    alive: vec![(shell, reused), (RELAUNCHED, true)],
                    ..live.clone()
                },
                Some(files.clone()),
            ),
            (
                "two processes hold it",
                Watched {
                    alive: vec![
                        (shell, reused),
                        (old, false),
                        (RELAUNCHED, true),
                        (RELAUNCHED + 1, true),
                    ],
                    starts: [
                        live.starts.clone(),
                        vec![(RELAUNCHED + 1, HOLDER_START.into())],
                    ]
                    .concat(),
                    ..live.clone()
                },
                Some(vec![holder(RELAUNCHED), holder(RELAUNCHED + 1)]),
            ),
        ] {
            assert_eq!(
                watch(&k, files.as_deref(), Some(CONVERSATION)),
                Watch::Hold,
                "{why} (reused={reused})"
            );
        }
        // A move of the adopted holder settles the watch and goes on; a start beside it, or a
        // move of another process, is refused; with nothing holding it, either goes on.
        for (fresh, files, settles) in [
            (
                Fresh::Signal {
                    pid: RELAUNCHED,
                    conversation: CONVERSATION,
                },
                files.clone(),
                true,
            ),
            (
                Fresh::Signal {
                    pid: RELAUNCHED + 1,
                    conversation: CONVERSATION,
                },
                files.clone(),
                false,
            ),
            (
                Fresh::Start {
                    conversation: Some(CONVERSATION),
                },
                files.clone(),
                false,
            ),
            (
                Fresh::Start {
                    conversation: Some(CONVERSATION),
                },
                Vec::new(),
                true,
            ),
        ] {
            let mut settled = st.clone();
            let outcome = settle_closed(&mut settled, fresh, || Some(files.clone()), &live);
            assert_eq!(outcome.is_ok(), settles, "{fresh:?} (reused={reused})");
            assert_eq!(settled.native_closed.is_none(), settles, "{fresh:?}");
        }
    }
}

fn prepare_old(prepare: &Prepare) -> Process {
    let NativePlan::Restart { process, .. } = &prepare.plan else {
        unreachable!()
    };
    process.clone()
}

/// A later boot than the one the closed restart's prepare names ended every process of it: the
/// watch clears whatever else reads, so a reboot never leaves a conversation refused for good.
#[test]
fn a_watch_of_an_earlier_boot_clears() {
    let (st, prepare, _, _) = watched_record(Some(stamp('1')));
    let unread = Watched::default();
    let watch = |k: &Watched| closed_watch(&prepare, k, None, Some(CONVERSATION));
    assert_eq!(
        watch(&Watched {
            boot: Some(stamp('2')),
            ..unread.clone()
        }),
        Watch::Clear
    );
    let mut settled = st.clone();
    assert_eq!(
        settle_closed(
            &mut settled,
            Fresh::Start {
                conversation: Some(CONVERSATION)
            },
            || None,
            &Watched {
                boot: Some(stamp('2')),
                ..unread.clone()
            }
        ),
        Ok(())
    );
    assert!(settled.native_closed.is_none());
    // Negative controls: the same boot, or a boot that does not read, decides nothing.
    for k in [
        Watched {
            boot: Some(stamp('1')),
            ..unread.clone()
        },
        unread.clone(),
    ] {
        assert_eq!(watch(&k), Watch::Hold);
    }
    // A prepare with no boot stamp names no boot to compare.
    let (_, unstamped, _, _) = watched_record(None);
    assert_eq!(
        closed_watch(
            &unstamped,
            &Watched {
                boot: Some(stamp('2')),
                ..unread
            },
            None,
            Some(CONVERSATION)
        ),
        Watch::Hold
    );
}

/// The `NativeClosedWatch` projection: a model state realised as a world the injected kernel
/// reads, and the record a watch or a fresh plan leaves projected back onto the model.
pub(in crate::harness) mod watch_model {
    use super::*;
    use aterm_spec::interp::State;

    /// A process the lane's move signals that the closed restart never started.
    pub(in crate::harness) const OTHER: u32 = 5151;

    /// The world `state` names, around the closed restart of `prepare` (stamped on boot `1`):
    /// its shell the live test process, gone by its pid freed or, where `reused`, by a process
    /// born at another time on it; its old agent gone; its grantee [`RELAUNCHED`], the shell's
    /// child running the image its forward command settles on, registered on [`CONVERSATION`]
    /// where it holds it, outliving the shell only then; the shell's other child [`HELPER`],
    /// unregistered, running an image no grant of the restart's runs or, where it is one the
    /// watch cannot tell from the grantee, the restart's launcher.
    pub(in crate::harness) fn world(
        state: &State,
        prepare: &Prepare,
        reused: bool,
    ) -> (Watched, Vec<SessionFile>) {
        let NativePlan::Restart {
            shell,
            process,
            launcher,
            forward,
            ..
        } = &prepare.plan
        else {
            unreachable!("a restart plan")
        };
        let (shell_pid, old) = (shell.pid, process.pid);
        let shell_runs = state["shell"] == 1;
        let grantee = state["grantee"] == 1;
        let id = |image: &Image| file(image.device, image.inode);
        let other = match state["other"] {
            0 => None,
            1 => Some(file(launcher.device + 1, 99)),
            _ => Some(id(launcher)),
        };
        let mut k = Watched {
            boot: Some(stamp(if state["boot"] == 1 { '2' } else { '1' })),
            holds_tab: Some(state["tab"] == 1),
            children: Some(
                (shell_runs && grantee)
                    .then_some(RELAUNCHED)
                    .into_iter()
                    .chain(other.map(|_| HELPER))
                    .collect(),
            ),
            images: grantee
                .then(|| (RELAUNCHED, id(forward.settled())))
                .into_iter()
                .chain(other.map(|image| (HELPER, image)))
                .collect(),
            // A gone process has no parent to read; an orphan's is init.
            parents: grantee
                .then_some((RELAUNCHED, if shell_runs { shell_pid } else { 1 }))
                .into_iter()
                .chain(other.map(|_| (HELPER, shell_pid)))
                .collect(),
            alive: vec![
                (shell_pid, shell_runs || reused),
                (old, false),
                (RELAUNCHED, grantee),
            ],
            starts: vec![(RELAUNCHED, HOLDER_START.into())],
            shell_start: None,
        };
        k.starts.push((
            shell_pid,
            if shell_runs {
                shell_start(prepare)
            } else {
                "Thu Jan  1 00:00:01 1970".into()
            },
        ));
        let files = if state["holds"] == 1 {
            vec![holder(RELAUNCHED)]
        } else if state["grantee"] == 2 {
            // An exited grantee's file may stay behind; it names no live process.
            vec![holder(RELAUNCHED)]
        } else {
            Vec::new()
        };
        (k, files)
    }

    /// Project `record`, the closed restart's record after a watch read or a fresh plan in the
    /// world `world`, onto the model: the world's variables are the realised ones, `watch` the
    /// record's own (kept or cleared), and `adopted`, `start` and `signal` what the plan went on
    /// to do (`went_on`), the move's subject the adopted grantee or another process.
    pub(in crate::harness) fn project(
        world: &State,
        record: &St,
        adopted: bool,
        went_on: Option<Fresh<'_>>,
    ) -> State {
        let mut state = world.clone();
        state.insert("watch", i64::from(record.native_closed.is_some()));
        state.insert("adopted", i64::from(adopted));
        let (start, signal) = match went_on {
            None => (0, 0),
            Some(Fresh::Start { .. }) => (1, 0),
            Some(Fresh::Signal { .. }) if adopted => (0, 1),
            Some(Fresh::Signal { .. }) => (0, 2),
        };
        state.insert("start", start);
        state.insert("signal", signal);
        state
    }
}

/// `closed_watch` and `settle_closed` refine `NativeClosedWatch`: from every reachable state
/// that keeps a watch, realised on the injected kernel (the shell alive, or gone by a freed or a
/// reused pid; the shell's other child absent, running an unrelated image, or running the
/// restart's launcher), a watch read is the model's Adopt, its Clear or no step, and a fresh plan
/// is no step or the model's Adopt or Clear followed by its Start, SignalAdopted or SignalOther.
/// Every fact reads here, so a watch that holds is one the model can neither adopt nor clear.
/// Negative controls: a settle that went on over a watch it holds is a step the model refuses,
/// and beside the grantee gone a child that runs the launcher holds where a helper clears.
#[test]
fn the_watch_and_its_settle_refine_the_closed_watch_model() {
    let model = aterm_models_harness::native_closed_watch_model();
    let (record, prepare, _, _) = watched_record(Some(stamp('1')));
    let mut checked = 0;
    for world in aterm_spec::interp::reachable_states(&model) {
        if world["watch"] != 1 || world["start"] != 0 || world["signal"] != 0 {
            continue;
        }
        for reused in [false, true] {
            if reused && world["shell"] == 1 {
                continue;
            }
            checked += 1;
            let (k, files) = watch_model::world(&world, &prepare, reused);
            let read = closed_watch(&prepare, &k, Some(&files), Some(CONVERSATION));
            let (step, adopts) = match &read {
                Watch::Adopt(sf) => {
                    assert_eq!(sf.pid, RELAUNCHED);
                    (Some("Adopt"), true)
                }
                Watch::Clear => (Some("Clear"), false),
                Watch::Hold => (None, false),
            };
            let mut cleared = record.clone();
            if step.is_some() {
                cleared.native_closed = None;
            }
            let after = watch_model::project(&world, &cleared, adopts, None);
            match step {
                Some(action) => assert!(
                    model.successors(action, &world).contains(&after),
                    "{world:?} reused={reused}: {read:?}"
                ),
                None => {
                    assert_eq!(after, world);
                    for action in ["Adopt", "Clear"] {
                        assert!(
                            !model.action_enabled(action, &world),
                            "{world:?} reused={reused}: a watch held where {action} is a step"
                        );
                    }
                }
            }
            for fresh in [
                Fresh::Start {
                    conversation: Some(CONVERSATION),
                },
                Fresh::Signal {
                    pid: RELAUNCHED,
                    conversation: CONVERSATION,
                },
                Fresh::Signal {
                    pid: watch_model::OTHER,
                    conversation: CONVERSATION,
                },
            ] {
                let mut st = record.clone();
                let went_on = settle_closed(&mut st, fresh, || Some(files.clone()), &k).is_ok();
                let subject = matches!(fresh, Fresh::Signal { pid, .. } if pid == RELAUNCHED);
                let adopted = went_on && adopts && subject;
                let after = watch_model::project(&world, &st, adopted, went_on.then_some(fresh));
                let paths: &[[&str; 2]] = &[
                    ["Adopt", "SignalAdopted"],
                    ["Clear", "Start"],
                    ["Clear", "SignalOther"],
                ];
                let modelled = !went_on && after == world
                    || paths.iter().any(|[first, then]| {
                        model
                            .successors(first, &world)
                            .iter()
                            .any(|mid| model.successors(then, mid).contains(&after))
                    });
                assert!(modelled, "{world:?} reused={reused} {fresh:?}: {after:?}");
            }
        }
    }
    assert!(checked > 0);
    // Negative control: in a world whose unregistered grantee runs, the watch holds; a settle
    // that went on over it, starting beside the grantee, is no path of the model.
    let mut world = model.init_state();
    for action in ["Launch", "Close"] {
        assert!(model.fire(action, &mut world));
    }
    let (k, files) = watch_model::world(&world, &prepare, false);
    assert_eq!(
        closed_watch(&prepare, &k, Some(&files), Some(CONVERSATION)),
        Watch::Hold
    );
    let mut st = record.clone();
    st.native_closed = None;
    let bad = watch_model::project(
        &world,
        &st,
        false,
        Some(Fresh::Start {
            conversation: Some(CONVERSATION),
        }),
    );
    assert!(!model.check_invariant("NoStartBesideTheGrantee", &bad));
    assert!(["Adopt", "Clear"].iter().all(|first| {
        model
            .successors(first, &world)
            .iter()
            .all(|mid| !model.successors("Start", mid).contains(&bad))
    }));
    // The shell's other child, the grantee gone: a helper never keeps the watch, where a child
    // that runs the restart's launcher does, the model's Clear neither.
    for (other, read) in [("Spawn", Watch::Clear), ("SpawnAlike", Watch::Hold)] {
        let mut world = model.init_state();
        for action in [other, "Launch", "Close", "Exit"] {
            assert!(model.fire(action, &mut world), "{other}: {action}");
        }
        let (k, files) = watch_model::world(&world, &prepare, false);
        assert_eq!(
            closed_watch(&prepare, &k, Some(&files), Some(CONVERSATION)),
            read,
            "{other}"
        );
        assert_eq!(
            model.action_enabled("Clear", &world),
            read == Watch::Clear,
            "{other}"
        );
    }
}
