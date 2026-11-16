// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Plain lineage, marker and run observations for one leftover-process candidate.
//! Readers fill these facts; upper layers judge their custody and cleanup eligibility.

/// A candidate's lineage (flavors 17 and 18, size-checked; Linux `/proc`).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Lin {
    /// Its unique id (`p_uniqueid`; start ticks on Linux).
    pub uniqueid: u64,
    /// Its parent now: `1` once launchd or init adopted it.
    pub parent: u32,
    /// Its process group.
    pub pgid: u32,
    /// Its launcher's pid version, kept through adoption; `None` where the kernel keeps none.
    pub launcher_version: Option<i32>,
    /// Its session's controlling terminal.
    pub tty: Option<u32>,
}

/// The marker variables of one environment: the session it names and the run id it carries.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Marks {
    /// The kernel handed back no environment at all: proof of nothing, either way.
    pub withheld: bool,
    /// `ATERM_PARENT_SESSION_ID`.
    pub session: Option<String>,
    /// `ATERM_VERIFY_RUN`.
    pub run: Option<String>,
}

/// How one verify run's record ties a candidate, as the host read it against that record.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct RunTie {
    /// The record's id (`<pid>-<birth>`).
    pub id: String,
    /// W-group: born into one of its open groups whose leader check passed, inside the window.
    pub group: bool,
    /// W-run: carries its id, born inside its window.
    pub run: bool,
    /// C-tree: its executable is under the record's roots.
    pub tree: bool,
    /// The run's launcher reads dead on a direct read.
    pub dead: bool,
    /// The record's own A1–A7 verdict is proven: the next verify run ends it.
    pub auto: bool,
}

/// Held at macOS's program check, as the jobs engine names it.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Held {
    /// Since the first reading that saw it held, ms.
    pub check_ms: u64,
    /// The tab of the job it is a member of, or the tab a session-mate ties it to.
    pub tab: Option<u64>,
}

/// A live job's process waiting behind a held one.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Waiting {
    /// The held process it waits on.
    pub peer: Option<u32>,
    /// Its job's tab.
    pub tab: u64,
}

/// One candidate's facts, read by the host.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Facts {
    /// Its pid.
    pub pid: u32,
    /// Its parent as the sweep read it.
    pub ppid: u32,
    /// Its lineage; `None` where flavor 17 or 18 could not be read or failed its size check.
    pub lineage: Option<Lin>,
    /// Its identity token (`<pid>@<birth>:<idversion>`), read with the lineage.
    pub token: Option<String>,
    /// Its executable's path; `None` when gone, deleted or unread.
    pub exe: Option<String>,
    /// Its executable is under some record's roots.
    pub exe_under_root: bool,
    /// Its fork age, ms.
    pub started_ms: Option<u64>,
    /// Its CPU time, ms.
    pub cpu_ms: Option<u64>,
    /// Whether its one thread ran (`aterm_uds::process::thread_ran`), or a stand-in holds it
    /// (`Some(false)`); `None` when unread.
    pub thread_ran: Option<bool>,
    /// Held at macOS's program check, as the jobs engine names it.
    pub held: Option<Held>,
    /// Waiting behind a held process.
    pub waiting: Option<Waiting>,
    /// Its own marker variables; never read for a process whose thread has not run.
    pub marks: Option<Marks>,
    /// A group-mate's whose thread ran, read only when its own are withheld (a platform binary).
    pub mate_marks: Option<Marks>,
    /// Whether it holds a listening socket; `None` when the listener table did not read, or its
    /// descriptors could not be matched against it.
    pub listener: Option<bool>,
    /// Whether a sandbox profile holds it (a lab jail's, an agent's sandboxed tool command's);
    /// `None` when the check failed.
    pub sandboxed: Option<bool>,
    /// Whether it is an aterm program launchd keeps by design while what it serves runs: a
    /// tether's watcher or a build's claim holder, known by its executable and verb as doctor's
    /// census knows them. `Some(false)` for any other image; `None` for an aterm image whose verb
    /// could not be read.
    pub by_design: Option<bool>,
    /// Its executable is an aterm program's image.
    pub aterm: bool,
    /// An aterm instance proven the writer of its graph entry.
    pub writer: bool,
    /// It shares this aterm launch's coalition.
    pub coalition: bool,
    /// The live tab whose shell its live parent chain reaches.
    pub chain: Option<u64>,
    /// How each record of this boot ties it.
    pub runs: Vec<RunTie>,
    /// A live record claims it (it is a live run's launcher, in its open group, or carries its
    /// id; unread or withheld markers claim it while a live run has an id).
    pub claimed: bool,
    /// Whether a live process holds the master of its tty; `None` when unread.
    pub tty_master_held: Option<bool>,
}
