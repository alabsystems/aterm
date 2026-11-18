// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;

impl Images {
    pub(in crate::vitals_host) fn set_shell_verifier(
        &mut self,
        verifier: fn(aterm_pty::ShellIdentity) -> Option<bool>,
    ) {
        self.shell = verifier;
    }
}

#[cfg(target_os = "macos")]
impl Images {
    /// A real-PTY fixture verifies its original leader without enabling image/signature probes.
    pub(in crate::vitals_host) fn use_native_shell_verifier(&mut self) {
        self.shell = Self::NATIVE.shell;
    }
}

fn image(pid: u32) -> Option<ImageFile> {
    match pid {
        100 | 200 | 300 | 700 | 800 => Some(ImageFile {
            device: 1,
            inode: 20,
        }),
        400 => Some(ImageFile {
            device: 1,
            inode: 21,
        }),
        500 => Some(ImageFile {
            device: 2,
            inode: 20,
        }),
        _ => None,
    }
}

fn start(pid: u32) -> Option<u64> {
    Some(u64::from(pid))
}

fn process(pid: u32, parent: u32, uid: u32) -> ProcSample {
    ProcSample {
        pid,
        ppid: parent,
        uid: Some(uid),
        start: start(pid),
        name: "aterm".into(),
        ..ProcSample::default()
    }
}

#[test]
fn shared_roots_require_loaded_device_inode_and_process_generation() {
    let mut procs = [
        process(100, 1, 501),
        process(200, 1, 501),
        process(300, 1, 502),
        process(400, 1, 501),
        process(500, 1, 501),
        process(600, 1, 501),
        process(700, 100, 501),
        ProcSample {
            start: Some(799),
            ..process(800, 1, 501)
        },
    ];
    let mut probe = Images {
        image,
        start,
        ..Images::INERT
    };
    assert_eq!(
        probe.roots(100, &mut procs, &[]),
        [
            SharedRoot {
                pid: 200,
                start: Some(200),
                version: None
            },
            SharedRoot {
                pid: 300,
                start: Some(300),
                version: None
            }
        ]
    );
    let mut inert = Images::INERT;
    assert!(inert.roots(100, &mut procs, &[]).is_empty());
}

#[test]
fn a_process_execing_during_the_identity_read_is_not_a_shared_root() {
    thread_local! { static READS: std::cell::Cell<u32> = const { std::cell::Cell::new(0) }; }
    READS.set(0);
    let mut probe = Images {
        start,
        image: |pid| {
            if pid == 200 {
                READS.with(|n| {
                    let before = n.get();
                    n.set(before + 1);
                    if before == 0 { image(pid) } else { None }
                })
            } else {
                image(pid)
            }
        },
        ..Images::INERT
    };
    assert!(
        probe
            .roots(100, &mut [process(200, 1, 501)], &[])
            .is_empty()
    );
}

fn signature() -> Identity {
    Identity {
        exe: "aterm".into(),
        cs_status: Some(aterm_sysprobe::identity::CS_VALID | aterm_sysprobe::identity::CS_SIGNED),
        category: Some(aterm_sysprobe::identity::CATEGORY_DEVELOPER_ID),
        signing_id: Some("com.aterm.aterm".into()),
        team: Some("A66A9P66Z7".into()),
    }
}

#[test]
fn signed_copies_need_the_same_valid_certificate_team_and_identifier() {
    let own = signature();
    assert!(same_signature(&own, &signature()));
    for other in [
        Identity {
            team: Some("DIFFERENT1".into()),
            ..signature()
        },
        Identity {
            signing_id: Some("another-app".into()),
            ..signature()
        },
        Identity {
            cs_status: Some(aterm_sysprobe::identity::CS_SIGNED),
            ..signature()
        },
        Identity {
            category: Some(aterm_sysprobe::identity::CATEGORY_NONE),
            ..signature()
        },
        Identity {
            signing_id: None,
            ..signature()
        },
    ] {
        assert!(!same_signature(&own, &other));
    }
}

#[test]
fn a_signed_system_broker_needs_no_refused_resource_start_or_image_read() {
    let mut probe = Images {
        image: |pid| if pid == 100 { image(pid) } else { None },
        start: |pid| (pid == 100).then_some(100),
        signature: |_| Some(signature()),
        version: |pid| Some(pid as i32),
        ..Images::INERT
    };
    let mut broker = ProcSample {
        pid: 900,
        ppid: 1,
        uid: Some(0),
        version: Some(900),
        ..ProcSample::default()
    };
    assert_eq!(
        probe.roots(100, std::slice::from_mut(&mut broker), &[]),
        [SharedRoot {
            pid: 900,
            start: None,
            version: Some(900)
        }]
    );
}

#[test]
fn an_unchanged_kernel_version_reuses_the_image_proof_and_exec_invalidates_it() {
    thread_local! {
        static VERSION: std::cell::Cell<i32> = const { std::cell::Cell::new(1) };
        static IMAGES: std::cell::Cell<u32> = const { std::cell::Cell::new(0) };
    }
    VERSION.set(1);
    IMAGES.set(0);
    let mut probe = Images {
        image: |pid| {
            if pid == 200 {
                IMAGES.with(|n| n.set(n.get() + 1));
            }
            image(pid)
        },
        start,
        version: |pid| Some(if pid == 200 { VERSION.get() } else { 100 }),
        ..Images::INERT
    };
    let mut p = process(200, 1, 501);
    p.version = Some(1);
    assert_eq!(probe.roots(100, std::slice::from_mut(&mut p), &[]).len(), 1);
    let read = IMAGES.get();
    assert_eq!(probe.roots(100, std::slice::from_mut(&mut p), &[]).len(), 1);
    assert_eq!(IMAGES.get(), read, "a stable pid version reuses the proof");
    VERSION.set(2);
    assert!(
        probe
            .roots(100, std::slice::from_mut(&mut p), &[])
            .is_empty(),
        "the old sweep cannot claim an exec"
    );
    p.version = Some(2);
    assert_eq!(probe.roots(100, std::slice::from_mut(&mut p), &[]).len(), 1);
    assert!(IMAGES.get() > read);
}

/// A GENERATION KNOWN UNOWNED IS READ ONCE: a row the cache holds unowned under its sweep version
/// costs no read of its own on the next `roots()` (no version, start, image or signature), while
/// a row cached owned keeps its version bracket, and the roots, every row's write-back and the
/// cache are a fresh `Images`' over the same world. NEGATIVE CONTROLS: without the skip each
/// unowned row reads its version again here; with the skip widened to rows cached owned,
/// `an_unchanged_kernel_version_reuses_the_image_proof_and_exec_invalidates_it` lets the old sweep
/// claim an exec.
#[test]
fn a_generation_known_unowned_reads_each_generation_once() {
    type Reads = std::collections::BTreeMap<(&'static str, u32), u32>;
    thread_local! {
        static READS: std::cell::RefCell<Reads> = const {
            std::cell::RefCell::new(std::collections::BTreeMap::new())
        };
    }
    fn count(kind: &'static str, pid: u32) {
        READS.with_borrow_mut(|reads| *reads.entry((kind, pid)).or_default() += 1);
    }
    let world = || Images {
        image: |pid| {
            count("image", pid);
            image(pid)
        },
        start: |pid| {
            count("start", pid);
            start(pid)
        },
        signature: |pid| {
            count("signature", pid);
            None
        },
        version: |pid| {
            count("version", pid);
            i32::try_from(pid).ok()
        },
        ..Images::INERT
    };
    // 100 is this instance and 700 its child; 200 runs its image elsewhere (shared); 400 and
    // 500 run other images and 600 none that reads: unowned.
    let mut procs = [100, 200, 400, 500, 600, 700].map(|pid| ProcSample {
        version: i32::try_from(pid).ok(),
        ..process(pid, if pid == 700 { 100 } else { 1 }, 501)
    });
    let mut probe = world();
    let first = probe.roots(100, &mut procs, &[]);
    assert_eq!(
        first.iter().map(|r| r.pid).collect::<Vec<_>>(),
        [200],
        "{first:?}"
    );
    let read_first = READS.take();
    for pid in [200, 400, 500, 600] {
        assert_eq!(
            read_first.get(&("version", pid)),
            Some(&1),
            "first sight brackets {pid}: {read_first:?}"
        );
    }
    let unchanged = procs.clone();
    let second = probe.roots(100, &mut procs, &[]);
    let read_second = READS.take();
    assert_eq!(
        read_second,
        Reads::from([
            (("image", 100), 2),
            (("signature", 100), 1),
            (("start", 100), 2),
            (("version", 100), 2),
            (("version", 200), 1),
        ]),
        "only this instance's own bracket and the owned row's version: no read of 400, 500 or 600"
    );
    let mut fresh = world();
    let mut fresh_procs = unchanged;
    let fresh_roots = fresh.roots(100, &mut fresh_procs, &[]);
    assert_eq!(second, fresh_roots, "the roots");
    assert_eq!(procs, fresh_procs, "each row's write-back");
    assert_eq!(probe.cache, fresh.cache, "the cache");
    assert_eq!(second, first);
}

fn root(key: u64, pid: u32) -> TabRoot {
    TabRoot {
        key,
        tab: key,
        tab_id: None,
        shell_pid: pid,
        label: format!("Tab {key}"),
        program: String::new(),
        agent_leader: None,
    }
}

#[test]
fn shell_generation_proofs_reject_reused_pids_and_mark_unread_roots_as_bounds() {
    let probe = Images {
        shell: |id| match id.pid {
            200 => Some(true),
            201 => Some(false),
            _ => None,
        },
        ..Images::INERT
    };
    let mut tabs = [root(1, 200), root(2, 201), root(3, 202), root(4, 203)];
    let identities = [
        (1, aterm_pty::ShellIdentity::from_parts(200, Some(1), true)),
        (2, aterm_pty::ShellIdentity::from_parts(201, Some(1), true)),
        (3, aterm_pty::ShellIdentity::from_parts(202, Some(1), false)),
        (4, aterm_pty::ShellIdentity::from_parts(203, Some(1), false)),
    ];
    let unproved = probe.tab_roots(&mut tabs, &identities, &[200, 201, 202].into(), true);
    assert_eq!(
        tabs.iter().map(|t| t.shell_pid).collect::<Vec<_>>(),
        [200, 0, 0, 0]
    );
    assert_eq!(
        unproved,
        [3],
        "absent and stale roots claim no replacement; unread roots stay bounds"
    );
    let child_of_replacement = ProcSample {
        pid: 205,
        ppid: 201,
        ..ProcSample::default()
    };
    assert_eq!(attribute(&[child_of_replacement], 100, &tabs)[0].tab, None);
}
