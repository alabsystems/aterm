// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

use super::*;

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

/// One fact [`Faulty`] reads otherwise than the world's own kernel does.
#[derive(Clone, Copy)]
enum Fault {
    /// The process table was not read: nothing says who opened the terminal.
    TableUnread,
    /// The table was read, and a program other than aterm opened the terminal.
    OpenedElsewhere,
    /// The foreground process's image was not read.
    ImageUnread,
}

/// The world's own kernel ([`SourceKernel`]) with one [`Fault`].
struct Faulty {
    source: SourceKernel,
    fault: Fault,
}

impl Faulty {
    fn of(world: &World, fault: Fault) -> Self {
        Self {
            source: SourceKernel {
                controls: world.controls(),
                process: world.process.clone(),
                shell: world.shell.clone(),
            },
            fault,
        }
    }
}

impl Kernel for Faulty {
    fn shell_dialect(
        &self,
        shell: u32,
        rows: &[crate::harness::upgrade_drive::ProcRow],
    ) -> Option<Dialect> {
        self.source.shell_dialect(shell, rows)
    }

    fn job(&self, pid: u32) -> Option<(Job, u32)> {
        self.source.job(pid)
    }

    fn parent(&self, pid: u32) -> Option<u32> {
        self.source.parent(pid)
    }

    fn terminal(&self, pid: u32) -> Option<(u32, String)> {
        match self.fault {
            Fault::TableUnread => None,
            Fault::OpenedElsewhere => Some((4242, "tmux".into())),
            Fault::ImageUnread => self.source.terminal(pid),
        }
    }

    fn terminal_read(&self, pid: u32) -> TerminalRead {
        match self.fault {
            Fault::TableUnread => TerminalRead::Unread,
            Fault::OpenedElsewhere | Fault::ImageUnread => TerminalRead::Read(self.terminal(pid)),
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

    fn image(&self, pid: u32) -> Option<PathBuf> {
        match self.fault {
            Fault::ImageUnread => None,
            Fault::TableUnread | Fault::OpenedElsewhere => self.source.image(pid),
        }
    }
}

/// A process in the middle of an exec holds every read of a loaded Mac's process table past its
/// wait. The snapshot read then knows nothing of who opened the tab's terminal: it refuses as
/// unread, never with `terminal`, which is the host's answer that no agent aterm carries is in
/// the tab (LS-15).
#[test]
fn an_unread_process_table_is_not_a_terminal_nobody_opened() {
    let mut world = World::new(aterm_types::tab_id::TabId::from_bits(2)).unwrap();
    let why = snapshot_with(&world.opts, &Faulty::of(&world, Fault::TableUnread))
        .expect_err("no terminal read, no snapshot");
    assert_eq!(why, TERMINAL_UNREAD);
    assert_eq!(SnapshotRefusal::of(why), SnapshotRefusal::Unread(why));

    let why = snapshot_with(&world.opts, &Faulty::of(&world, Fault::OpenedElsewhere))
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
    let why = snapshot_with(&world.opts, &Faulty::of(&world, Fault::ImageUnread))
        .expect_err("no image read, no snapshot");
    assert_eq!(why, EXE_UNREADABLE);
    assert_eq!(SnapshotRefusal::of(why), SnapshotRefusal::Unread(why));

    // The same process once no record names it: its image is read, and it is no Claude Code.
    let record = world
        .home()
        .join(format!(".claude/sessions/{}.json", world.process.pid));
    std::fs::remove_file(&record).expect("the fixture's session file");
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
