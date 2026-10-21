// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! THE POLICY'S PROBE: the one door the approval policy looks at the machine
//! by, at the level of what it concluded — never the raw bytes it read.
//!
//! [`super::approval::decide`] is a pure function of the screen and its
//! context, but for a handful of boxes it must ask the machine: where a Read's
//! symbolic links lead and whether a folder on its way holds a name the box
//! may not have drawn, where an `rm` operand's links lead, what configuration a
//! git read would load and which `env` blocks Claude Code's settings carry,
//! what the worker's shell starts with and which ask rules its own settings
//! hold, where its Bash tool may stand, and which process is the worker at
//! all. Each of those looks was made directly where the policy needed it
//! (`std::fs::read_dir`, `canonicalize`, a `git config --list`, the worker's
//! environment read through the kernel), so a harness tape recorded the
//! decision's screen and its mark but not what the decision turned on.
//!
//! Now every such look is a method of [`PolicyProbe`], handed to the policy as
//! [`ApprovalCtx::probe`] (and to the rm rule as `RmScope::probe`):
//! * [`LiveProbe`] is the shipped look, its bodies the very code the policy
//!   ran before (`probe_live.rs`, or the reader it calls where that reader is
//!   shared with the window's host);
//! * `TapedProbe` is a run recorded with `--tape`: it makes the same live
//!   look and returns the same value, and hands the loop's door
//!   (`Fx::exchange`) the request and the DERIVED answer as a round trip
//!   at `probe.<name>` ([`doors`]), so the tape holds what the decision
//!   turned on, in the order it turned on it;
//! * `KeyedProbe` answers each look from a tape out of order, through
//!   astream-effects' keyed index of the tape — built once per tape and shared
//!   by every decision decided again on it — bounded at the point a recorded
//!   decision was made (`aterm drive replay --why`, `--shadow`): what the
//!   recorded loop had learned by then, never later, and never the machine.
//!
//! **Derived, not raw — the privacy line.** What crosses a probe is the
//! answer, never what was read to reach it (the replay plan's §4.7 and §7):
//! * a hidden twin's path or nothing, never a folder's listing;
//! * a path's resolved components, never the links on the way;
//! * the worker as a DIGEST and a pid ([`WorkerEnv::digest`], seeded per
//!   tape), never its environment — which holds whatever the person's shell
//!   exported, API keys and tokens among it;
//! * a git view PROJECTED to the keys [`super::git_config`]'s `hazard` reads,
//!   each value reduced to bare/true/false/set ([`GitView::project`]), never a
//!   URL, a credential helper or an `http.<url>.extraheader`;
//! * a verdict on Claude Code's settings, the shell's startup, the owner's
//!   ask rules and the Bash tool's directories, never a settings file's or an
//!   rc file's bytes;
//! * a REFUSAL (an answer's `Err`, which a tape holds as it holds a value) in
//!   the check's own words: paths, line numbers, and the NAMES it found — a
//!   variable's, a config key's, an option's, an alias's, a function's —
//!   never a line it read or a value: a `.zshenv` line is named by its number
//!   and the word of the check's that it holds
//!   ([`super::shell_startup::zshenv_hazard`]), a snapshot's option by its
//!   name, a worker bound to another session without its variable's value, a
//!   git that failed by its exit status rather than its stderr. The owner's
//!   ask rules and its hooks' matchers cross as written: they ARE the rules
//!   the decision quotes (never a hook's command, never an `env` value).
//!
//! The live bodies that see the environment and the files run INSIDE the
//! probe's live call, below the door: a door reached there runs live and is
//! taped nowhere (the replay plan's §4.4), and none is — they read the raw
//! [`WorkerEnv`] directly, which no door wraps. So the variables reach no
//! tape by construction, not by a filter that could miss one.
//!
//! **The answer IS the value.** The live probe returns the projected git view
//! too, not the full one: the policy judges in every mode exactly what a tape
//! can hold, and `hazard(v, r) == hazard(v.project(), r)` (a property test,
//! with a projection that drops a key as its negative control) is what makes
//! that no change at all.
//!
//! **No probe reads a variable by name, because nothing above one does.** The
//! replay plan's §4.7 names a `worker_var`/`worker_flag` probe for the policy's
//! reads of the worker's variables and argv, taped by access. Once every look
//! that reads the environment is its own derived probe — the worker's binding
//! to its session (`worker_proc`), git's view, the settings' `env` check, the
//! shell's startup, the owner's asks, the Bash tool's directories, Codex's
//! records — no decision-level code reads a variable at all: each read happens
//! inside a live body and reaches the tape only as that probe's answer. A
//! decision that comes to read one directly adds that probe then. Likewise the
//! plan's `canonicalize` probe for where the worker may write is no probe of
//! its own: that resolve is made only inside `git_view`'s live body
//! ([`WorkerWrites::holds`]), and the tape holds what it decided.
//!
//! **Frozen below the door.** A replay answers each probe from the tape and
//! never runs its live body, so what a probe concluded is replayed, not
//! re-derived: a counterfactual policy that would read a key the recording's
//! projection did not keep sees it absent — the [`PROJECTION`] version on each
//! recorded view is how a later build tells.

use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

use astream_effects::{EffectRecord, KeyedIndex};
use aterm_types::MutexExt;
use serde::{Deserialize, Serialize};

use super::approval::ApprovalCtx;
use super::claude_settings::{AskKind, OwnerAsk};
use super::git_config::{GitView, PROJECTION, VALUE_SET, WorkerEnv};
use super::rm_breaker::{ScratchRoot, abs_components};
use super::shell_startup::ShellStartup;
use crate::supervise::fx::Fx;
use crate::supervise::tape::codec::CodecError;
use crate::supervise::tape::core::TapeValue;
use crate::supervise::tape::doors;
use crate::supervise::tape::header::Roots;

/// How the approval policy looks at the machine ([module docs](self)): one
/// method per look, each returning what the policy concluded from it.
pub trait PolicyProbe: Send + Sync + std::fmt::Debug {
    /// A name in a folder on the way to the absolute path `comps` that differs
    /// from the drawn one only by characters the box may not draw: its full
    /// path, `None`, or `Err` for a folder that cannot be listed or is too big
    /// to list in a decision (the Read rule's `hidden_twin`).
    ///
    /// # Errors
    ///
    /// A folder on the way cannot be listed, or holds too many entries.
    fn hidden_twin(&self, comps: &[String]) -> Result<Option<String>, String> {
        self.hidden_twin_in(comps, None)
    }

    /// [`Self::hidden_twin`] with `home` supplied so the live walk never lists
    /// inside a root macOS guards with `kTCCServiceSystemPolicyAppData` or a
    /// file-provider domain (`~/Library/Containers`, `~/Library/Group Containers`,
    /// `~/Library/CloudStorage`, `~/Library/Mobile Documents`).
    ///
    /// # Errors
    ///
    /// A folder on the way cannot be listed, or holds too many entries.
    fn hidden_twin_in(
        &self,
        comps: &[String],
        home: Option<&Path>,
    ) -> Result<Option<String>, String>;

    /// `comps` with every symbolic link resolved (the Read rule's resolver);
    /// `None` for a dangling link or a refused look.
    fn real_components(&self, comps: &[String]) -> Option<Vec<String>>;

    /// Where the absolute path `comps` leads on the disk as the kernel walks
    /// it for `rm`, and whether a link on the way is not root's (the rm rule's
    /// resolver).
    ///
    /// # Errors
    ///
    /// A link climbs out of a missing directory, the links do not end, or the
    /// disk refuses the look.
    fn rm_physical(&self, comps: &[String]) -> Result<OnDisk, String>;

    /// The worker: the process leading the foreground group `group` of
    /// session `sid`, read through the kernel and bound to the session by the
    /// `ATERM_PARENT_SESSION_ID` its shell was handed.
    ///
    /// # Errors
    ///
    /// The process cannot be read, or names another session or none.
    fn worker_proc(&self, group: u32, sid: &str) -> Result<WorkerEnv, String>;

    /// What a git command the worker runs in `dir` would load, PROJECTED to
    /// what the policy reads of it ([`GitView::project`]); the git is the one
    /// the worker's `PATH` finds, refused where `writes` says the worker could
    /// have put one ([`WorkerWrites`]).
    ///
    /// # Errors
    ///
    /// No git can be named, or its configuration cannot be read.
    fn git_view(
        &self,
        worker: &WorkerEnv,
        dir: &Path,
        writes: &WorkerWrites<'_>,
    ) -> Result<GitView, String>;

    /// Whether Claude Code's settings for an agent launched in `launch` give
    /// its Bash tool a git-relevant variable the worker's process lacks
    /// ([`WorkerEnv::git_settings_env`]).
    ///
    /// # Errors
    ///
    /// Such a variable, or a settings file that cannot be read whole.
    fn settings_env(&self, worker: &WorkerEnv, launch: &Path) -> Result<(), String>;

    /// What the worker's Bash tool's shell starts with
    /// ([`super::shell_startup::read`]).
    ///
    /// # Errors
    ///
    /// What fails every line, or why the startup could not be read.
    fn shell_startup(&self, worker: &WorkerEnv, launch: &Path) -> Result<ShellStartup, String>;

    /// The ask rules and PreToolUse hooks of the agent's own Claude Code
    /// settings ([`super::claude_settings::owner_asks`]).
    ///
    /// # Errors
    ///
    /// Why they could not be read.
    fn owner_asks(&self, worker: &WorkerEnv, launch: &Path) -> Result<Vec<OwnerAsk>, String>;

    /// Where the worker's Bash tool may stand beyond `cwd`, from Claude
    /// Code's transcripts under its directory
    /// ([`crate::harness::footer::shell_cwds`]); empty where its directory
    /// cannot be named.
    fn shell_cwds(&self, worker: &WorkerEnv, cwd: &Path) -> Vec<PathBuf>;
}

/// An absolute path as the disk resolves it for `rm`
/// ([`PolicyProbe::rm_physical`]).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OnDisk {
    /// Where it leads.
    pub comps: Vec<String>,
    /// Whether a symbolic link on the way is not root's: the worker could
    /// have made it, or re-pointed it.
    pub foreign_link: bool,
}

/// Where the worker may write unapproved, so a `git` there would be the
/// worker's to configure ([`PolicyProbe::git_view`]): the session cwd, every
/// Bash tool directory and the scratch roots. Data, not a closure, so a probe's
/// request can name it and its live body can judge by it.
#[derive(Debug, Clone, Copy)]
pub struct WorkerWrites<'a> {
    /// The session's working directory.
    pub cwd: &'a Path,
    /// Where its Bash tool may stand.
    pub shell_cwds: &'a [PathBuf],
    /// The roots an `rm` may point into.
    pub scratch_roots: &'a [ScratchRoot],
}

impl<'a> WorkerWrites<'a> {
    /// What `ctx` says the worker may write.
    #[must_use]
    pub fn of(ctx: &'a ApprovalCtx) -> Self {
        Self {
            cwd: &ctx.cwd,
            shell_cwds: &ctx.shell_cwds,
            scratch_roots: &ctx.scratch_roots,
        }
    }

    /// Whether the worker may write under `dir`: compared as written and as
    /// `resolved` leads it (the live probe's `canonicalize`), against the cwd
    /// and every Bash tool directory (each also resolved) and the scratch
    /// roots.
    pub fn holds(&self, dir: &Path, resolved: &dyn Fn(&Path) -> PathBuf) -> bool {
        let forms = [dir.to_path_buf(), resolved(dir)];
        let bases: Vec<PathBuf> = std::iter::once(self.cwd)
            .chain(self.shell_cwds.iter().map(PathBuf::as_path))
            .flat_map(|b| [b.to_path_buf(), resolved(b)])
            .collect();
        forms.iter().any(|d| {
            bases.iter().any(|b| d.starts_with(b))
                || abs_components(&d.to_string_lossy())
                    .is_some_and(|comps| self.scratch_roots.iter().any(|r| r.holds(&comps)))
        })
    }
}

/// The shipped probe: every look made on this machine, as the policy always
/// made it (`probe_live.rs`).
///
/// Named by the shipped supervisor only here, in its live arm and in `Fx`
/// (`Fx::policy_probe`, which hands it out only when nothing records): a
/// decision handed one directly would look at the machine past the door a
/// tape is kept at, so the `supervise-fx` source guard (`aterm-census`,
/// phase 3) names it anywhere else — and, for the same reason, every call of
/// a live body its arm makes (`shell_startup::read`,
/// `WorkerEnv::raw_git_view`, …) from above the door. No `Default`: a
/// probe typed in by inference would be one the guard cannot see.
#[derive(Debug, Clone, Copy)]
pub struct LiveProbe;

/// A run recorded to a harness tape: each look made live, its derived answer
/// handed to the loop's door as a round trip at `probe.<name>`
/// ([module docs](self)). The door returns the live value, so recording
/// changes no decision.
///
/// The same probe over a REPLAYING door is [`ReplayProbe`]: each request is
/// built exactly as the recording built it and checked against the recorded
/// one, and the answer is the tape's — the door never makes its live call,
/// and the probe's live arm there is [`NoLook`], which has no look to make.
#[derive(Clone)]
pub(crate) struct TapedProbe<P: PolicyProbe = LiveProbe> {
    fx: Fx,
    live: P,
    /// The tape's seed for the worker's digest ([`WorkerEnv::digest`]).
    seed: u64,
}

/// A replay's probe ([`TapedProbe`] over a replaying door): every answer is
/// the tape's.
pub(crate) type ReplayProbe = TapedProbe<NoLook>;

impl<P: PolicyProbe> std::fmt::Debug for TapedProbe<P> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "TapedProbe({:?})", self.live)
    }
}

impl TapedProbe {
    /// A probe recording through `fx`, the worker digested with `seed`.
    pub(crate) fn new(fx: Fx, seed: u64) -> Self {
        Self {
            fx,
            live: LiveProbe,
            seed,
        }
    }
}

impl ReplayProbe {
    /// A probe answering from the tape `fx` replays, the worker digested with
    /// `seed` (the recording's).
    ///
    /// # Panics
    ///
    /// `fx` does not replay: [`NoLook`] has no look to make.
    pub(crate) fn replaying(fx: Fx, seed: u64) -> Self {
        assert!(
            fx.replaying(),
            "a replay's probe over a door that does not replay"
        );
        Self {
            fx,
            live: NoLook,
            seed,
        }
    }
}

/// A replay's live arm: no look at the machine at all. A replaying door never
/// makes the live call it is handed, so none of these is reached; each says
/// so if one ever were, rather than look at a machine the recorded run never
/// stood on.
#[derive(Debug, Clone, Copy)]
pub(crate) struct NoLook;

/// What a [`NoLook`] says if it is ever asked.
const NO_LOOK: &str =
    "a replay never looks at the machine: its door answers every probe from the tape";

impl PolicyProbe for NoLook {
    fn hidden_twin_in(&self, _: &[String], _: Option<&Path>) -> Result<Option<String>, String> {
        unreachable!("{NO_LOOK}")
    }

    fn real_components(&self, _: &[String]) -> Option<Vec<String>> {
        unreachable!("{NO_LOOK}")
    }

    fn rm_physical(&self, _: &[String]) -> Result<OnDisk, String> {
        unreachable!("{NO_LOOK}")
    }

    fn worker_proc(&self, _: u32, _: &str) -> Result<WorkerEnv, String> {
        unreachable!("{NO_LOOK}")
    }

    fn git_view(&self, _: &WorkerEnv, _: &Path, _: &WorkerWrites<'_>) -> Result<GitView, String> {
        unreachable!("{NO_LOOK}")
    }

    fn settings_env(&self, _: &WorkerEnv, _: &Path) -> Result<(), String> {
        unreachable!("{NO_LOOK}")
    }

    fn shell_startup(&self, _: &WorkerEnv, _: &Path) -> Result<ShellStartup, String> {
        unreachable!("{NO_LOOK}")
    }

    fn owner_asks(&self, _: &WorkerEnv, _: &Path) -> Result<Vec<OwnerAsk>, String> {
        unreachable!("{NO_LOOK}")
    }

    fn shell_cwds(&self, _: &WorkerEnv, _: &Path) -> Vec<PathBuf> {
        unreachable!("{NO_LOOK}")
    }
}

/// How a probe's REQUEST is written ([module docs](self)): each path as the
/// run's roots write it (`path`), the worker by its digest under the tape's
/// `seed`. One writer for the probe that records a request ([`TapedProbe`])
/// and for the one that asks a tape for it again ([`KeyedProbe`]), so the two
/// cannot spell one request two ways: a request asked otherwise than it was
/// recorded finds nothing.
struct Requests<'a> {
    path: &'a dyn Fn(&Path) -> Vec<u8>,
    seed: u64,
}

impl Requests<'_> {
    /// The absolute path `comps` as the run's roots write it.
    fn comps(&self, comps: &[String]) -> Vec<u8> {
        (self.path)(Path::new(&format!("/{}", comps.join("/"))))
    }

    /// `key=value` lines, each path as the run's roots write it.
    fn lines(&self, fields: &[(&str, Field<'_>)]) -> Vec<u8> {
        let mut out = Vec::new();
        for (key, value) in fields {
            out.extend_from_slice(key.as_bytes());
            out.push(b'=');
            match value {
                Field::Text(t) => out.extend_from_slice(t.as_bytes()),
                Field::Path(p) => out.extend((self.path)(p)),
            }
            out.push(b'\n');
        }
        out
    }

    /// The worker's digest as a request names it.
    fn worker(&self, worker: &WorkerEnv) -> String {
        format!("{:016x}", worker.digest(self.seed))
    }

    /// A request about the worker launched in `at` (`launch=`, `cwd=`).
    fn worker_at(&self, worker: &WorkerEnv, key: &str, at: &Path) -> Vec<u8> {
        self.lines(&[
            ("worker", Field::Text(&self.worker(worker))),
            (key, Field::Path(at)),
        ])
    }

    /// The worker asked for by its foreground group and session.
    fn worker_proc(&self, group: u32, sid: &str) -> Vec<u8> {
        self.lines(&[
            ("group", Field::Text(&group.to_string())),
            ("sid", Field::Text(sid)),
        ])
    }

    /// A git view asked for: the worker, the directory, and where the worker
    /// may write.
    fn git_view(&self, worker: &WorkerEnv, dir: &Path, writes: &WorkerWrites<'_>) -> Vec<u8> {
        let worker = self.worker(worker);
        let mut fields = vec![
            ("worker", Field::Text(&worker)),
            ("dir", Field::Path(dir)),
            ("cwd", Field::Path(writes.cwd)),
        ];
        fields.extend(
            writes
                .shell_cwds
                .iter()
                .map(|d| ("shell_cwd", Field::Path(d))),
        );
        fields.extend(
            writes
                .scratch_roots
                .iter()
                .map(|r| ("root", Field::Text(r.label()))),
        );
        self.lines(&fields)
    }
}

impl<P: PolicyProbe> TapedProbe<P> {
    /// A request written by [`Requests`] through this probe's door.
    fn request(&self, write: impl FnOnce(&Requests<'_>) -> Vec<u8>) -> Vec<u8> {
        let path = |p: &Path| self.fx.path_bytes(p);
        write(&Requests {
            path: &path,
            seed: self.seed,
        })
    }
}

/// One value of a probe's request.
enum Field<'a> {
    Text(&'a str),
    Path(&'a Path),
}

impl<P: PolicyProbe> PolicyProbe for TapedProbe<P> {
    fn hidden_twin_in(
        &self,
        comps: &[String],
        home: Option<&Path>,
    ) -> Result<Option<String>, String> {
        self.fx.exchange(
            doors::PROBE_HIDDEN_TWIN,
            || self.request(|r| r.comps(comps)),
            || self.live.hidden_twin_in(comps, home),
        )
    }

    fn real_components(&self, comps: &[String]) -> Option<Vec<String>> {
        self.fx.exchange(
            doors::PROBE_REAL_COMPONENTS,
            || self.request(|r| r.comps(comps)),
            || self.live.real_components(comps),
        )
    }

    fn rm_physical(&self, comps: &[String]) -> Result<OnDisk, String> {
        self.fx.exchange(
            doors::PROBE_RM_PHYSICAL,
            || self.request(|r| r.comps(comps)),
            || self.live.rm_physical(comps),
        )
    }

    fn worker_proc(&self, group: u32, sid: &str) -> Result<WorkerEnv, String> {
        self.fx.exchange(
            doors::PROBE_WORKER_PROC,
            || self.request(|r| r.worker_proc(group, sid)),
            // Digested where it crosses: every later request names the
            // worker as this record does.
            || {
                self.live
                    .worker_proc(group, sid)
                    .map(|w| w.digested(self.seed))
            },
        )
    }

    fn git_view(
        &self,
        worker: &WorkerEnv,
        dir: &Path,
        writes: &WorkerWrites<'_>,
    ) -> Result<GitView, String> {
        self.fx.exchange(
            doors::PROBE_GIT_VIEW,
            || self.request(|r| r.git_view(worker, dir, writes)),
            || self.live.git_view(worker, dir, writes),
        )
    }

    fn settings_env(&self, worker: &WorkerEnv, launch: &Path) -> Result<(), String> {
        self.fx.exchange(
            doors::PROBE_SETTINGS_ENV,
            || self.request(|r| r.worker_at(worker, "launch", launch)),
            || self.live.settings_env(worker, launch),
        )
    }

    fn shell_startup(&self, worker: &WorkerEnv, launch: &Path) -> Result<ShellStartup, String> {
        self.fx.exchange(
            doors::PROBE_SHELL_STARTUP,
            || self.request(|r| r.worker_at(worker, "launch", launch)),
            || self.live.shell_startup(worker, launch),
        )
    }

    fn owner_asks(&self, worker: &WorkerEnv, launch: &Path) -> Result<Vec<OwnerAsk>, String> {
        self.fx.exchange(
            doors::PROBE_OWNER_ASKS,
            || self.request(|r| r.worker_at(worker, "launch", launch)),
            || self.live.owner_asks(worker, launch),
        )
    }

    fn shell_cwds(&self, worker: &WorkerEnv, cwd: &Path) -> Vec<PathBuf> {
        self.fx.exchange(
            doors::PROBE_SHELL_CWDS,
            || self.request(|r| r.worker_at(worker, "cwd", cwd)),
            || self.live.shell_cwds(worker, cwd),
        )
    }
}

/// The tape's effects, by ordinal, and astream-effects' keyed index of their
/// round trips ([`KeyedIndex`]): built once per tape and shared, behind an
/// `Arc`, by the keyed probe of every decision decided again on it.
pub(crate) type TapeIndex = KeyedIndex<Arc<[EffectRecord]>>;

/// A probe that answers from a TAPE, asked out of order: what `aterm drive
/// replay --why` and `--shadow` decide a recorded box again with, on any
/// build. Each look is asked of the tape's round trips through
/// astream-effects' keyed index ([`TapeIndex`]) by its door and its request,
/// written as the recording wrote it ([`Requests`], under the tape's RECORDED
/// roots and seed), and answered by the LATEST such round trip among the
/// records the recorded loop had read when it decided — `seen`, the index's
/// own bound, a count of records — never one from after: what the loop had
/// learned by then, never what it learned later. Nothing is consumed and the
/// machine is never looked at.
///
/// A look costs one lookup in the shared index. It once built a keyed VIEW of
/// the whole tape at every look (a view borrows the records, and a probe must
/// own what it reads): on a tape grown to 100,049 effects from the corpus's
/// `c4-read`, two looks a box, a shadow took 2.59 s, quadratic in the tape
/// (`benches/tape_shadow_cost.rs`, T12).
///
/// A look the tape does not hold before the point — a build that asks what
/// the recording's build never asked, or asks it otherwise — is answered as
/// a refused look ("not on the tape"), so the decision goes the cautious way
/// it goes for any unreadable look, and the door and request are kept
/// ([`Self::misses`]): what the re-decision NEEDED and did not have, said
/// beside it, never a guess presented as the recorded world.
#[derive(Debug)]
pub(crate) struct KeyedProbe {
    /// The tape's records, by ordinal, and their keyed index, shared.
    index: Arc<TapeIndex>,
    /// How many of them the recorded loop had read at the point.
    seen: usize,
    /// The tape's roots as recorded: paths are asked as the recording wrote
    /// them.
    roots: Roots,
    /// The tape's seed for a worker's digest ([`WorkerEnv::digest`]).
    seed: u64,
    misses: Mutex<Vec<String>>,
}

impl KeyedProbe {
    /// A probe standing where the recorded loop had read the first `seen` of
    /// the records `index` holds (the tape's effects, by ordinal), its
    /// requests written under `roots` and `seed` as the recording wrote them.
    pub(crate) fn at(index: Arc<TapeIndex>, seen: usize, roots: Roots, seed: u64) -> Self {
        Self {
            index,
            seen,
            roots,
            seed,
            misses: Mutex::new(Vec::new()),
        }
    }

    /// Every look asked that the tape did not answer before the point: its
    /// door, and what it asked as the tape writes it.
    pub(crate) fn misses(&self) -> Vec<String> {
        self.misses.lock_or_recover().clone()
    }

    /// The answer to the request `write` writes at `door`, or `unrecorded`
    /// with why.
    fn answer<A: TapeValue>(
        &self,
        door: &str,
        write: impl FnOnce(&Requests<'_>) -> Vec<u8>,
        unrecorded: impl FnOnce(String) -> A,
    ) -> A {
        // Lossy as the recorder writes a path (`TapeCore::path`).
        let path = |p: &Path| {
            String::from_utf8_lossy(&self.roots.encode(p))
                .into_owned()
                .into_bytes()
        };
        let request = write(&Requests {
            path: &path,
            seed: self.seed,
        });
        let found = self
            .index
            .lookup(door, &request, self.seen)
            .map_err(|_| "not on the tape before this point")
            .and_then(|hit| {
                A::from_tape(hit.reply).map_err(|_| "on the tape in a form this build cannot read")
            });
        found.unwrap_or_else(|why| {
            self.misses.lock_or_recover().push(format!(
                "{door} {}",
                String::from_utf8_lossy(&request)
                    .trim_end()
                    .replace('\n', " ")
            ));
            unrecorded(format!("{door}: {why}"))
        })
    }
}

impl PolicyProbe for KeyedProbe {
    fn hidden_twin_in(&self, comps: &[String], _: Option<&Path>) -> Result<Option<String>, String> {
        self.answer(doors::PROBE_HIDDEN_TWIN, |r| r.comps(comps), Err)
    }

    fn real_components(&self, comps: &[String]) -> Option<Vec<String>> {
        self.answer(doors::PROBE_REAL_COMPONENTS, |r| r.comps(comps), |_| None)
    }

    fn rm_physical(&self, comps: &[String]) -> Result<OnDisk, String> {
        self.answer(doors::PROBE_RM_PHYSICAL, |r| r.comps(comps), Err)
    }

    fn worker_proc(&self, group: u32, sid: &str) -> Result<WorkerEnv, String> {
        self.answer(doors::PROBE_WORKER_PROC, |r| r.worker_proc(group, sid), Err)
    }

    fn git_view(
        &self,
        worker: &WorkerEnv,
        dir: &Path,
        writes: &WorkerWrites<'_>,
    ) -> Result<GitView, String> {
        self.answer(
            doors::PROBE_GIT_VIEW,
            |r| r.git_view(worker, dir, writes),
            Err,
        )
    }

    fn settings_env(&self, worker: &WorkerEnv, launch: &Path) -> Result<(), String> {
        self.answer(
            doors::PROBE_SETTINGS_ENV,
            |r| r.worker_at(worker, "launch", launch),
            Err,
        )
    }

    fn shell_startup(&self, worker: &WorkerEnv, launch: &Path) -> Result<ShellStartup, String> {
        self.answer(
            doors::PROBE_SHELL_STARTUP,
            |r| r.worker_at(worker, "launch", launch),
            Err,
        )
    }

    fn owner_asks(&self, worker: &WorkerEnv, launch: &Path) -> Result<Vec<OwnerAsk>, String> {
        self.answer(
            doors::PROBE_OWNER_ASKS,
            |r| r.worker_at(worker, "launch", launch),
            Err,
        )
    }

    fn shell_cwds(&self, worker: &WorkerEnv, cwd: &Path) -> Vec<PathBuf> {
        self.answer(
            doors::PROBE_SHELL_CWDS,
            |r| r.worker_at(worker, "cwd", cwd),
            |_| Vec::new(),
        )
    }
}

// -- what a probe's answer is on a tape --------------------------------------
//
// Each answer is written as JSON (`aterm_json`, the workspace's one writer)
// through a small mirror of its type, so the tape's text form reads as what
// the policy concluded. A path is written as text (`to_string_lossy`: a byte
// that is not UTF-8 is replaced, the documented loss of every path a tape
// holds). A read of bytes this build did not write is a `CodecError`, never a
// guess.

/// A probe's answer, or why it has none, as a tape holds it: `{"ok":…}` or
/// `{"err":"…"}` (the workspace's serde writes no `Result`).
#[derive(Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub(crate) enum Answer<T> {
    Ok(T),
    Err(String),
}

/// `value` as JSON bytes. The mirrors below hold strings, numbers, booleans and
/// lists of them, which always serialize.
fn json<T: Serialize>(value: &T) -> Vec<u8> {
    aterm_json::to_vec(value).unwrap_or_default()
}

/// JSON bytes read back as `T`.
fn of_json<T: for<'de> Deserialize<'de>>(bytes: &[u8]) -> Result<T, CodecError> {
    aterm_json::from_slice(bytes).map_err(|_| CodecError {
        at: 0,
        why: "a probe's answer this build did not write",
    })
}

/// `answer` as a tape holds it, its value written through `wire`.
fn answer<U, T: Serialize>(answer: &Result<U, String>, wire: impl FnOnce(&U) -> T) -> Vec<u8> {
    json(&match answer {
        Ok(value) => Answer::Ok(wire(value)),
        Err(why) => Answer::Err(why.clone()),
    })
}

/// An answer read back, its value made from its mirror by `value`.
fn answer_of<U, T: for<'de> Deserialize<'de>>(
    bytes: &[u8],
    value: impl FnOnce(T) -> Result<U, CodecError>,
) -> Result<Result<U, String>, CodecError> {
    match of_json::<Answer<T>>(bytes)? {
        Answer::Ok(wire) => value(wire).map(Ok),
        Answer::Err(why) => Ok(Err(why)),
    }
}

/// A path as a probe's answer writes it.
fn path_text(p: &Path) -> String {
    p.to_string_lossy().into_owned()
}

impl TapeValue for Result<Option<String>, String> {
    fn to_tape(&self) -> Vec<u8> {
        answer(self, Clone::clone)
    }

    fn from_tape(bytes: &[u8]) -> Result<Self, CodecError> {
        answer_of(bytes, Ok)
    }
}

impl TapeValue for Option<Vec<String>> {
    fn to_tape(&self) -> Vec<u8> {
        json(self)
    }

    fn from_tape(bytes: &[u8]) -> Result<Self, CodecError> {
        of_json(bytes)
    }
}

impl TapeValue for Result<OnDisk, String> {
    fn to_tape(&self) -> Vec<u8> {
        answer(self, |d| (d.comps.clone(), d.foreign_link))
    }

    fn from_tape(bytes: &[u8]) -> Result<Self, CodecError> {
        answer_of(bytes, |(comps, foreign_link): (Vec<String>, bool)| {
            Ok(OnDisk {
                comps,
                foreign_link,
            })
        })
    }
}

/// The worker as a tape holds it: its digest (sixteen hex digits) and pid,
/// never a variable. The digest is the one fixed where the worker crossed the
/// probe ([`WorkerEnv::digested`], which [`TapedProbe`] always makes).
impl TapeValue for Result<WorkerEnv, String> {
    fn to_tape(&self) -> Vec<u8> {
        answer(self, |w| (format!("{:016x}", w.digest(0)), w.pid()))
    }

    fn from_tape(bytes: &[u8]) -> Result<Self, CodecError> {
        answer_of(bytes, |(digest, pid): (String, Option<u32>)| {
            u64::from_str_radix(&digest, 16)
                .map(|d| WorkerEnv::recorded(d, pid))
                .map_err(|_| CodecError {
                    at: 0,
                    why: "a worker's digest that is not hex",
                })
        })
    }
}

/// A projected git view as a tape holds it ([`GitView::project`]).
#[derive(Serialize, Deserialize)]
struct ViewWire {
    /// [`PROJECTION`], the rule it was projected by.
    projection: u32,
    config: Vec<(String, String, Option<String>)>,
    index_hook: Option<String>,
    filters_selected: Option<Vec<String>>,
    submodules: Vec<(String, ViewWire)>,
    /// Whether the worker's `GIT_EXTERNAL_DIFF` is set.
    external_diff: bool,
}

impl ViewWire {
    /// `view` projected — again, whatever handed it here: a tape never holds
    /// more of a view than the projection keeps.
    fn of(view: &GitView) -> Self {
        let v = view.project();
        Self {
            projection: PROJECTION,
            config: v.config,
            index_hook: v.index_hook.as_deref().map(path_text),
            filters_selected: v.filters_selected,
            submodules: v
                .submodules
                .iter()
                .map(|(p, sub)| (path_text(p), Self::of(sub)))
                .collect(),
            external_diff: v.external_diff.is_some(),
        }
    }

    fn view(self) -> Result<GitView, CodecError> {
        if self.projection != PROJECTION {
            return Err(CodecError {
                at: 0,
                why: "a git view projected by a rule this build does not have",
            });
        }
        Ok(GitView {
            config: self.config,
            index_hook: self.index_hook.map(PathBuf::from),
            filters_selected: self.filters_selected,
            submodules: self
                .submodules
                .into_iter()
                .map(|(p, sub)| Ok((PathBuf::from(p), sub.view()?)))
                .collect::<Result<_, CodecError>>()?,
            external_diff: self.external_diff.then(|| VALUE_SET.to_string()),
        })
    }
}

impl TapeValue for Result<GitView, String> {
    fn to_tape(&self) -> Vec<u8> {
        answer(self, ViewWire::of)
    }

    fn from_tape(bytes: &[u8]) -> Result<Self, CodecError> {
        answer_of(bytes, ViewWire::view)
    }
}

/// A shell's startup as a tape holds it ([`ShellStartup::names`] and
/// [`ShellStartup::may_run_zsh`]): the probe's answer and a decision's facts
/// (`box_facts`) write it alike.
pub(crate) type ShellWire = (Vec<(String, String, bool)>, bool);

impl TapeValue for Result<ShellStartup, String> {
    fn to_tape(&self) -> Vec<u8> {
        answer(self, |s| (s.names().to_vec(), s.may_run_zsh()))
    }

    fn from_tape(bytes: &[u8]) -> Result<Self, CodecError> {
        answer_of(bytes, |(names, zsh): ShellWire| {
            Ok(ShellStartup::of_parts(names, zsh))
        })
    }
}

/// An ask rule or hook as a tape holds it ([`OwnerAsk`]).
#[derive(Serialize, Deserialize)]
pub(crate) struct AskWire {
    /// `rule` or `hook`.
    kind: String,
    /// A rule's tool; a hook's matcher, if it has one.
    tool: Option<String>,
    /// A rule with no specifier.
    bare: bool,
    text: String,
    source: String,
}

impl AskWire {
    pub(crate) fn of(ask: &OwnerAsk) -> Self {
        let (kind, tool, bare) = match &ask.kind {
            AskKind::Rule { tool, bare } => ("rule", Some(tool.clone()), *bare),
            AskKind::Hook { matcher } => ("hook", matcher.clone(), false),
        };
        Self {
            kind: kind.to_string(),
            tool,
            bare,
            text: ask.text.clone(),
            source: ask.source.clone(),
        }
    }

    pub(crate) fn ask(self) -> Result<OwnerAsk, CodecError> {
        let kind = match (self.kind.as_str(), self.tool) {
            ("rule", Some(tool)) => AskKind::Rule {
                tool,
                bare: self.bare,
            },
            ("hook", matcher) => AskKind::Hook { matcher },
            _ => {
                return Err(CodecError {
                    at: 0,
                    why: "an ask this build does not know",
                });
            }
        };
        Ok(OwnerAsk {
            kind,
            text: self.text,
            source: self.source,
        })
    }
}

impl TapeValue for Result<Vec<OwnerAsk>, String> {
    fn to_tape(&self) -> Vec<u8> {
        answer(self, |asks| {
            asks.iter().map(AskWire::of).collect::<Vec<_>>()
        })
    }

    fn from_tape(bytes: &[u8]) -> Result<Self, CodecError> {
        answer_of(bytes, |asks: Vec<AskWire>| {
            asks.into_iter().map(AskWire::ask).collect()
        })
    }
}

impl TapeValue for Vec<PathBuf> {
    fn to_tape(&self) -> Vec<u8> {
        json(&self.iter().map(|p| path_text(p)).collect::<Vec<_>>())
    }

    fn from_tape(bytes: &[u8]) -> Result<Self, CodecError> {
        let wire: Vec<String> = of_json(bytes)?;
        Ok(wire.into_iter().map(PathBuf::from).collect())
    }
}

#[cfg(test)]
#[path = "probe_tests.rs"]
pub(crate) mod tests;
