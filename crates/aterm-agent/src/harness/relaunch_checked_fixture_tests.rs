// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

use super::*;
use crate::harness::upgrade_drive::{Fact, NotRead};

#[test]
fn the_real_snapshot_reader_captures_the_original_process_lifetime() {
    let mut world = World::new(aterm_types::tab_id::TabId::from_bits(1)).unwrap();
    let kernel = SourceKernel {
        controls: world.controls(),
        process: world.process.clone(),
        shell: world.shell.clone(),
    };
    let snapshot = snapshot_with(&world.opts, &kernel).expect("the real live snapshot path");
    assert_eq!(snapshot.pid, world.process.pid);
    assert_eq!(snapshot.session.as_deref(), Some(SESSION));
    let lifetime = snapshot
        .lifetime
        .expect("captured around the live process facts");
    assert_eq!(lifetime.pid(), snapshot.pid);
    assert_eq!(lifetime.observe(), ProcessLiveness::Alive);
    world.close().unwrap();
    assert_eq!(lifetime.observe(), ProcessLiveness::Ended);
}

/// What [`Probe`] says in place of the fixture's own facts: each `None` leaves the fixture's.
#[derive(Default)]
struct Stated {
    terminal: Option<Fact<(u32, String)>>,
    image: Option<Option<PathBuf>>,
    /// Whether aterm opened the agent's terminal; read as `Ok(None)` (no) where not stated.
    opener: Option<Fact<String>>,
    /// The agent's shell is in the agent's own group: a job no shell runs unless aterm opened its
    /// terminal ([`Kernel::job`]).
    shared_group: bool,
    /// Whether the tab's shell holds it ([`Kernel::shell_holds_tab`]; `None`: not read).
    holds_tab: Option<Option<bool>>,
    /// The agent's arguments ([`Kernel::args_read`]); the fixture's own read where not stated.
    args: Option<Fact<atpkg::caller_shell::ProcArgs>>,
}

/// The fixture's kernel over its genuine process, with facts restated ([`Stated`]) and its job
/// read by the real [`Kernel::job`] from stated ids: the real [`snapshot_with`] reads every other
/// fact as it reads a live tab's.
struct Probe {
    source: SourceKernel,
    stated: Stated,
}

impl Probe {
    fn of(world: &World, stated: Stated) -> Self {
        Self {
            source: SourceKernel {
                controls: world.controls(),
                process: world.process.clone(),
                shell: world.shell.clone(),
            },
            stated,
        }
    }
}

impl Kernel for Probe {
    fn shell_dialect(
        &self,
        shell: u32,
        rows: &[crate::harness::upgrade_drive::ProcRow],
    ) -> Option<Dialect> {
        self.source.shell_dialect(shell, rows)
    }
    fn ids(&self, pid: u32) -> Option<(u32, i64, i64)> {
        // The agent leads its own group, which holds its terminal; its shell leads another, or
        // is in the agent's ([`Stated::shared_group`]).
        let agent = self.source.process.pid;
        let shell = self.source.shell.pid;
        let group = i64::from(agent);
        let shells = if self.stated.shared_group {
            group
        } else {
            i64::from(shell)
        };
        (pid == agent)
            .then_some((shell, group, group))
            .or_else(|| (pid == shell).then_some((0, shells, group)))
    }
    fn parent(&self, pid: u32) -> Option<u32> {
        self.source.parent(pid)
    }
    fn terminal(&self, pid: u32) -> Fact<(u32, String)> {
        match &self.stated.terminal {
            Some(fact) => fact.clone(),
            None => self.source.terminal(pid),
        }
    }
    fn opener(&self, _: u32, _: u32) -> Fact<String> {
        self.stated.opener.clone().unwrap_or(Ok(None))
    }
    fn shell_holds_tab(&self, shell: u32) -> Option<bool> {
        if let Some(holds) = self.stated.holds_tab {
            return holds;
        }
        let (_, pgid, tpgid) = self.ids(shell)?;
        Some(pgid == i64::from(shell) && pgid == tpgid)
    }
    fn image(&self, pid: u32) -> Option<PathBuf> {
        match &self.stated.image {
            Some(image) => image.clone(),
            None => self.source.image(pid),
        }
    }
    fn args_read(&self, pid: u32) -> Fact<atpkg::caller_shell::ProcArgs> {
        match &self.stated.args {
            Some(fact) => fact.clone(),
            None => self.source.args_read(pid),
        }
    }
    fn process_start(&self, pid: u32) -> Option<String> {
        self.source.process_start(pid)
    }
    fn process_lifetime(&self, pid: u32) -> Option<ProcessLifetime> {
        self.source.process_lifetime(pid)
    }
    fn restart_background(&self, pid: u32) -> Vec<String> {
        self.source.restart_background(pid)
    }
    fn restart_held(&self, pid: u32) -> Option<Vec<crate::harness::upgrade::Held>> {
        self.source.restart_held(pid)
    }
}

/// LS-72: ARGUMENTS WITHHELD FOR GOOD ARE AN ANSWER; ONE READ THAT FAILED IS NOT. The kernel never
/// answers another user's process's arguments to aterm (`sudo claude`: `KERN_PROCARGS2` reads only
/// a process of this one's user, or for root), a state every later read finds again: the real
/// snapshot read refuses [`ARGV_WITHHELD`], answered, so a permission click on it is refused
/// rather than left standing for a read that cannot come. CONTROL: an argument read that failed
/// (`NotRead`) is still `argv-unreadable`, unread.
#[test]
fn arguments_withheld_for_good_are_an_answer_and_a_failed_read_is_not() {
    let withheld = refused(
        60,
        Stated {
            args: Some(Ok(None)),
            ..Stated::default()
        },
        |_| {},
    );
    assert_eq!(withheld, SnapshotRefusal::Answered(ARGV_WITHHELD));
    let unread = refused(
        61,
        Stated {
            args: Some(Err(NotRead)),
            ..Stated::default()
        },
        |_| {},
    );
    assert_eq!(unread, SnapshotRefusal::Unread(ARGV_UNREAD));
}

/// The refusal the real snapshot read of a fresh fixture gives with `stated` restated, after
/// `prepare` has changed the fixture's records, classified.
fn refused(tab: u64, stated: Stated, prepare: impl FnOnce(&World)) -> SnapshotRefusal {
    let mut world = World::new(aterm_types::tab_id::TabId::from_bits(tab)).unwrap();
    prepare(&world);
    let read = snapshot_with(&world.opts, &Probe::of(&world, stated));
    world.close().unwrap();
    SnapshotRefusal::of(read.expect_err("the read refuses"))
}

/// LS-15: A TERMINAL NOT READ IS NOT "NOT ATERM'S". On macOS the terminal's owner comes from the
/// whole process table, which the kernel holds while a process execs: a table not read within its
/// bound (`Kernel::terminal`'s `NotRead`) refuses `terminal-unread`, which is unread, never
/// `terminal`, the answer that the tab holds no agent aterm can reach. CONTROLS: a terminal read
/// with no owner, or one another program opened, is the answer `terminal`.
#[test]
fn a_terminal_not_read_is_unread_and_one_read_not_ours_is_an_answer() {
    let unread = refused(
        4,
        Stated {
            terminal: Some(Err(NotRead)),
            ..Stated::default()
        },
        |_| {},
    );
    assert_eq!(unread, SnapshotRefusal::Unread("terminal-unread"));
    let none = refused(
        5,
        Stated {
            terminal: Some(Ok(None)),
            ..Stated::default()
        },
        |_| {},
    );
    assert_eq!(none, SnapshotRefusal::Answered("terminal"));
    let theirs = refused(
        6,
        Stated {
            terminal: Some(Ok(Some((4_242, "tmux".into())))),
            ..Stated::default()
        },
        |_| {},
    );
    assert_eq!(theirs, SnapshotRefusal::Answered("terminal"));
}

/// LS-15: AN IMAGE NOT READ IS NOT "NOT CLAUDE". `proc_pidpath` failing (`Kernel::image` `None`)
/// refuses `exe-unreadable`, unread, where it used to end in `not-claude`: nothing but the image
/// says which lane's program this is. CONTROL: an image read that no lane runs, with no session
/// record to register it, is the answer `not-claude`.
#[test]
fn an_image_not_read_is_unread_and_one_read_no_lane_runs_is_an_answer() {
    let unread = refused(
        7,
        Stated {
            image: Some(None),
            ..Stated::default()
        },
        |_| {},
    );
    assert_eq!(unread, SnapshotRefusal::Unread(EXE_UNREADABLE));
    let other = refused(
        8,
        Stated {
            image: Some(Some(PathBuf::from("/usr/bin/true"))),
            ..Stated::default()
        },
        |world| {
            std::fs::remove_file(session_record(world)).unwrap();
        },
    );
    assert_eq!(other, SnapshotRefusal::Answered("not-claude"));
}

/// LS-15: A SESSION RECORD NOT READ IS NOT "NOT CLAUDE". A program not named Claude Code's is
/// still one when its own session record registers it, so a record there and not read (a write in
/// progress) refuses `records-unread`, unread. CONTROLS: the same record read registers the
/// program (the snapshot is read), and no record at all is the answer `not-claude` (above).
#[test]
fn a_session_record_not_read_is_unread_not_another_program() {
    let other = || Stated {
        image: Some(Some(PathBuf::from("/usr/bin/true"))),
        ..Stated::default()
    };
    let unread = refused(9, other(), |world| {
        std::fs::write(session_record(world), "{\"pid\":").unwrap();
    });
    assert_eq!(unread, SnapshotRefusal::Unread(RECORDS_UNREAD));
    let mut world = World::new(aterm_types::tab_id::TabId::from_bits(10)).unwrap();
    let read = snapshot_with(&world.opts, &Probe::of(&world, other()));
    world.close().unwrap();
    let snap = read.expect("the record read registers the program");
    assert_eq!(snap.session.as_deref(), Some(SESSION));
}

/// LS-15: AN OPENER NOT READ IS NOT "NOT ATERM'S". A group leader in its parent's group is a job
/// no shell runs unless aterm opened its terminal, so where that opener was not read
/// (`kproc::record` failed) its job is not read (`ids`, unread), never `not-a-shell-job`.
/// CONTROL: an opener read that is not aterm is the answer `not-a-shell-job`.
#[test]
fn an_opener_not_read_is_unread_and_one_read_not_aterm_is_an_answer() {
    let unread = refused(
        11,
        Stated {
            opener: Some(Err(NotRead)),
            shared_group: true,
            ..Stated::default()
        },
        |_| {},
    );
    assert_eq!(unread, SnapshotRefusal::Unread("ids"));
    let read = refused(
        12,
        Stated {
            opener: Some(Ok(None)),
            shared_group: true,
            ..Stated::default()
        },
        |_| {},
    );
    assert_eq!(read, SnapshotRefusal::Answered("not-a-shell-job"));
}

fn session_record(world: &World) -> PathBuf {
    world
        .opts
        .home
        .join(format!(".claude/sessions/{}.json", world.process.pid))
}

/// LS-15: THE SELF-CLAIMED PATH TOO. The snapshot of a crashed codex-alab's copy reads its shell's
/// terminal the same way: one not read refuses `terminal-unread`, unread, and only one read and
/// not aterm's is the answer `terminal`. CONTROL: a terminal read and aterm's gives the snapshot.
#[test]
fn a_self_claimed_terminal_not_read_is_unread() {
    let read = |tab: u64, terminal: Fact<(u32, String)>| {
        self_claimed_read(
            tab,
            Stated {
                terminal: Some(terminal),
                ..Stated::default()
            },
            false,
        )
    };
    let (unread, _) = read(13, Err(NotRead));
    assert_eq!(unread, Err(SnapshotRefusal::Unread("terminal-unread")));
    let (theirs, _) = read(14, Ok(Some((4_242, "tmux".into()))));
    assert_eq!(theirs, Err(SnapshotRefusal::Answered("terminal")));
    let (ours, shell) = read(15, Ok(Some((1, "launchd".into()))));
    assert_eq!(ours, Ok(shell));
}

/// The real self-claimed snapshot read of a fresh fixture with `stated` restated, its refusal
/// classified, and the fixture's shell: the copy is the tab's group's own process where
/// `copy_is_shell`, else another.
fn self_claimed_read(
    tab: u64,
    stated: Stated,
    copy_is_shell: bool,
) -> (Result<u32, SnapshotRefusal>, u32) {
    use crate::harness::self_claimed::{Copy, Resume, Seen};
    let mut world = World::new(aterm_types::tab_id::TabId::from_bits(tab)).unwrap();
    let probe = Probe::of(&world, stated);
    let resume = Resume {
        argv: vec!["/opt/codex".into()],
        session: SESSION.into(),
        codex_home: None,
        codex_auth_path: None,
        cwd: None,
    };
    let copy = Copy {
        lifetime: None,
        pid: if copy_is_shell {
            world.process.pid
        } else {
            world.process.pid.wrapping_add(1)
        },
        start: "copy-start".into(),
        home: world.opts.home.clone(),
        seen: Seen::Never,
        resume: Ok(resume.clone()),
    };
    let got = self_claimed_snapshot_with(&world.opts, &probe, &copy, &resume)
        .map(|snap| snap.shell)
        .map_err(SnapshotRefusal::of);
    let shell = world.process.pid;
    world.close().unwrap();
    (got, shell)
}

/// LS-15: A CRASH'S SHELL NOT READ IS NOT "NO SHELL", AND ONE READ IS. The real self-claimed
/// snapshot read of a crashed codex-alab's tab: the shell the roster names whose ids were not read
/// refuses `shell-unread`, unread, which the crash rule reads again; a shell read that does not
/// hold the tab, or a tab whose group is the crashed copy's own, refuses `no-shell`, the host's
/// answer, which settles the crash at once. CONTROL: a shell read holding the tab gives the
/// snapshot.
#[test]
fn a_self_claimed_shell_not_read_is_unread_and_one_read_not_holding_the_tab_is_an_answer() {
    let holds = |holds: Option<bool>| Stated {
        holds_tab: Some(holds),
        ..Stated::default()
    };
    let (unread, _) = self_claimed_read(16, holds(None), false);
    assert_eq!(unread, Err(SnapshotRefusal::Unread(SHELL_UNREAD)));
    let (none, _) = self_claimed_read(17, holds(Some(false)), false);
    assert_eq!(none, Err(SnapshotRefusal::Answered("no-shell")));
    let (own, _) = self_claimed_read(18, Stated::default(), true);
    assert_eq!(own, Err(SnapshotRefusal::Answered("no-shell")));
    let (held, shell) = self_claimed_read(19, holds(Some(true)), false);
    assert_eq!(held, Ok(shell));
}

/// LS-72: the same rule at the self-claimed read's sibling site. A crashed codex-alab's shell whose
/// arguments the kernel withholds from aterm for good (another user's shell) refuses
/// [`ARGV_WITHHELD`], answered; one whose argument read failed refuses `argv-unreadable`, unread.
#[test]
fn a_self_claimed_shells_arguments_withheld_are_an_answer_and_a_failed_read_is_not() {
    let args = |args: Fact<atpkg::caller_shell::ProcArgs>| Stated {
        holds_tab: Some(Some(true)),
        args: Some(args),
        ..Stated::default()
    };
    let (withheld, _) = self_claimed_read(62, args(Ok(None)), false);
    assert_eq!(withheld, Err(SnapshotRefusal::Answered(ARGV_WITHHELD)));
    let (unread, _) = self_claimed_read(63, args(Err(NotRead)), false);
    assert_eq!(unread, Err(SnapshotRefusal::Unread(ARGV_UNREAD)));
}

/// LS-15: A CODEX WHOSE PACKAGE WAS NOT READ IS NOT "NO LANE". The real snapshot read of a tab
/// whose agent's image is a Codex: a package manifest caught mid-write (not JSON) or one whose
/// open failed is not read, so the read refuses `layout-unread`, unread, never [`NO_LANE`], the
/// answer that no lane relaunches the program (OpenAI's Codex). CONTROL: the same package read
/// with no ALab marker is that answer.
#[test]
fn a_codex_package_not_read_is_unread_and_one_read_not_alab_is_no_lane() {
    let d = crate::supervise::ScratchDir::new("codex", "snapshot-layout-unread");
    let root = d.join("pkg");
    std::fs::create_dir_all(root.join("bin")).unwrap();
    let exe = root.join("bin/codex");
    std::fs::write(&exe, "").unwrap();
    let manifest = root.join(crate::harness::codex_alab::PACKAGE_JSON);
    let codex = || Stated {
        image: Some(Some(exe.clone())),
        ..Stated::default()
    };
    std::fs::write(&manifest, r#"{"layoutVersion": 1, "version": "0.157"#).unwrap();
    assert_eq!(
        refused(20, codex(), |_| {}),
        SnapshotRefusal::Unread(LAYOUT_UNREAD)
    );
    std::fs::write(
        &manifest,
        r#"{"layoutVersion": 1, "version": "0.157.1", "entrypoint": "bin/codex"}"#,
    )
    .unwrap();
    assert_eq!(
        refused(21, codex(), |_| {}),
        SnapshotRefusal::Answered(NO_LANE)
    );
}

/// A process in the middle of an exec holds every read of a loaded Mac's process table past its
/// wait. The snapshot read then knows nothing of who opened the tab's terminal: it refuses as
/// unread, never with `terminal`, which is the host's answer that no agent aterm carries is in
/// the tab (LS-15).
#[test]
fn an_unread_process_table_is_not_a_terminal_nobody_opened() {
    let mut world = World::new(aterm_types::tab_id::TabId::from_bits(2)).unwrap();
    let table_unread = Stated {
        terminal: Some(Err(NotRead)),
        ..Stated::default()
    };
    let why = snapshot_with(&world.opts, &Probe::of(&world, table_unread))
        .expect_err("no terminal read, no snapshot");
    assert_eq!(why, TERMINAL_UNREAD);
    assert_eq!(SnapshotRefusal::of(why), SnapshotRefusal::Unread(why));

    let opened_elsewhere = Stated {
        terminal: Some(Ok(Some((4242, "tmux".into())))),
        ..Stated::default()
    };
    let why = snapshot_with(&world.opts, &Probe::of(&world, opened_elsewhere))
        .expect_err("a terminal aterm did not open");
    assert_eq!(why, "terminal", "read, and another program's");
    assert_eq!(SnapshotRefusal::of(why), SnapshotRefusal::Answered(why));
    world.close().unwrap();
}

/// The agent's own session file names the process, and its image could not be read. With no image
/// the read cannot say the program is no agent, so it refuses as unread; `not-claude` is the
/// answer only where the image was read and nothing names the program Claude Code.
#[test]
fn an_unread_image_is_not_a_program_that_is_no_agent() {
    let mut world = World::new(aterm_types::tab_id::TabId::from_bits(3)).unwrap();
    let image_unread = Stated {
        image: Some(None),
        ..Stated::default()
    };
    let why = snapshot_with(&world.opts, &Probe::of(&world, image_unread))
        .expect_err("no image read, no snapshot");
    assert_eq!(why, EXE_UNREADABLE);
    assert_eq!(SnapshotRefusal::of(why), SnapshotRefusal::Unread(why));

    // The same process once no record names it: its image is read, and it is no Claude Code.
    std::fs::remove_file(session_record(&world)).expect("the fixture's session file");
    let kernel = SourceKernel {
        controls: world.controls(),
        process: world.process.clone(),
        shell: world.shell.clone(),
    };
    let why = snapshot_with(&world.opts, &kernel).expect_err("a stand-in no record names");
    assert_eq!(why, "not-claude", "read, and no agent");
    assert_eq!(SnapshotRefusal::of(why), SnapshotRefusal::Answered(why));
    world.close().unwrap();
}

/// Set `path`'s last write an hour back: past [`crate::harness::upgrade_drive::WRITE_SETTLE`].
fn stood(path: &Path) {
    std::fs::File::options()
        .write(true)
        .open(path)
        .unwrap()
        .set_modified(std::time::SystemTime::now() - std::time::Duration::from_secs(3_600))
        .unwrap();
}

/// LS-72: A SESSION RECORD IN A LASTING STATE IS AN ANSWER, NEVER A READ NOT MADE. A plain
/// program (no lane's image) whose pid names a session record that every later read finds the
/// same is the answer `not-claude`, as on a record absent, never `records-unread`, which no read
/// could then clear (the tab would never settle): text that is not whole and has stood past its
/// write window (a Claude Code killed mid-write, the pid since reused), whole JSON that is no
/// record (another schema), a directory under the record's name, and a records folder that is a
/// regular file (`ENOTDIR`). CONTROL: the same text not whole, written just now, is a write in
/// progress and still unread.
#[test]
fn a_session_record_in_a_lasting_state_is_an_answer_not_unread() {
    let other = || Stated {
        image: Some(Some(PathBuf::from("/usr/bin/true"))),
        ..Stated::default()
    };
    let killed = refused(22, other(), |world| {
        let record = session_record(world);
        std::fs::write(&record, "{\"pid\":").unwrap();
        stood(&record);
    });
    assert_eq!(killed, SnapshotRefusal::Answered("not-claude"));
    let schema = refused(23, other(), |world| {
        std::fs::write(
            session_record(world),
            format!("{{\"pid\":{}}}", world.process.pid),
        )
        .unwrap();
    });
    assert_eq!(schema, SnapshotRefusal::Answered("not-claude"));
    let directory = refused(24, other(), |world| {
        let record = session_record(world);
        std::fs::remove_file(&record).unwrap();
        std::fs::create_dir(&record).unwrap();
    });
    assert_eq!(directory, SnapshotRefusal::Answered("not-claude"));
    let not_a_folder = refused(25, other(), |world| {
        let sessions = session_record(world).parent().unwrap().to_path_buf();
        std::fs::remove_dir_all(&sessions).unwrap();
        std::fs::write(&sessions, "").unwrap();
    });
    assert_eq!(not_a_folder, SnapshotRefusal::Answered("not-claude"));
    let writing = refused(26, other(), |world| {
        std::fs::write(session_record(world), "{\"pid\":").unwrap();
    });
    assert_eq!(writing, SnapshotRefusal::Unread(RECORDS_UNREAD));
}

/// LS-72: A CODEX MANIFEST IN A LASTING STATE IS READ. A manifest beside the image that is not
/// JSON and has stood past its write window is that root's content and names no entry, so the
/// walk reads the next root, whose manifest names the image: with no ALab marker that is the
/// answer [`NO_LANE`], never `layout-unread` on every read. CONTROL: the same manifest written
/// just now is one caught mid-write, unread.
#[test]
fn a_codex_manifest_that_has_stood_not_json_is_read_past() {
    let d = crate::supervise::ScratchDir::new("codex", "snapshot-layout-stood");
    let root = d.join("pkg");
    std::fs::create_dir_all(root.join("bin")).unwrap();
    let exe = root.join("bin/codex");
    std::fs::write(&exe, "").unwrap();
    std::fs::write(
        root.join(crate::harness::codex_alab::PACKAGE_JSON),
        r#"{"layoutVersion": 1, "version": "0.157.1", "entrypoint": "bin/codex"}"#,
    )
    .unwrap();
    let beside = root
        .join("bin")
        .join(crate::harness::codex_alab::PACKAGE_JSON);
    std::fs::write(&beside, r#"{"layoutVersion": 1, "vers"#).unwrap();
    let codex = || Stated {
        image: Some(Some(exe.clone())),
        ..Stated::default()
    };
    assert_eq!(
        refused(27, codex(), |_| {}),
        SnapshotRefusal::Unread(LAYOUT_UNREAD)
    );
    stood(&beside);
    assert_eq!(
        refused(28, codex(), |_| {}),
        SnapshotRefusal::Answered(NO_LANE)
    );
}
