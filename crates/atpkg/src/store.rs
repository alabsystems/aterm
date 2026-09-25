// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

//! The manager-owned install layout (§10): a hardened prefix under `$HOME`, the
//! per-program store, the `bin/` shim dir, per-program staging, the durable floor, and
//! the aggregate status file.
//!
//! Two trust-bearing decisions live here, both fail-closed:
//!
//! * **Prefix validation is a *chain* check, not just the leaf.**
//!   `dir_safe_for_private_write` only checks one dir's owner+mode; it does not walk the
//!   parent chain. Because `prefix` is config-controlled (§11), a prefix under a
//!   shared/attacker-writable *parent* would reintroduce a CWE-379 symlink-swap window.
//!   So [`resolve`] admits exactly TWO chain shapes, and never a mixture; anything else
//!   falls back to the trusted default prefix (mirroring the
//!   slug-fail-closed-to-default pattern). Neither shape may contain `..`.
//!
//!   1. **HOME prefix** — strictly under `$HOME`, with **every existing directory from
//!      `$HOME` down** owned-by-uid, not group/other-writable, and not a symlink.
//!   2. **SYSTEM prefix** — anywhere outside `$HOME`, with **every existing directory
//!      from `/` down** owned by ROOT, not group/other-writable, and not a symlink.
//!
//!   The system shape exists for a genuine multi-user install (one store several
//!   accounts share, which no `$HOME` prefix can be), and for anyone opting in to
//!   `TRUST_REQUIRE_SEALED_LAUNCHER=1`. A root-owned chain answers the same
//!   "no attacker-writable ancestor" question at least as strongly, and writing there
//!   requires root, which is checked rather than assumed. Both shapes are AND-checks
//!   over the FULL chain: one world-writable ancestor (`/private/tmp`, say)
//!   disqualifies the whole prefix.
//!
//!   CORRECTION 2026-08-18: this used to claim the system shape was REQUIRED for the
//!   verified lane — that a user-owned prefix "cannot carry pathname execution
//!   authority" and left `targo trust` unreachable for everything atpkg installs. That
//!   described an older Trust. Trust's default mode is now `CallerOwned`: a component
//!   owned by root **or by the invoking identity**, not group/world-writable, is
//!   authoritative — Trust's own source cites rustup's `~/.rustup` as the installation
//!   shape that demanding root would refuse. The DEFAULT `$HOME` prefix therefore
//!   proves fine, and running targo as root is REFUSED outright. Believing the old
//!   claim nearly cost this project an admin prompt and a privileged daemon;
//!   see `docs/GOLDEN-INSTALL-PATH.md` §2.
//! * **Shim names that collide with sensitive commands are refused** ([`shim_allowed`]).
//!   `bin/` is appended to the child `PATH` (never prepended, so a managed tool can't
//!   shadow a system one), but a tool honestly or maliciously named `sudo`/`ssh`/`git`/…
//!   must never get a shim at all.

use std::ffi::{OsStr, OsString};
use std::path::{Component, Path, PathBuf};

/// The resolved, validated install layout. All paths are absolute and under a prefix
/// that passed the [`resolve`] chain check.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Layout {
    /// The manager prefix, e.g. `~/Library/Application Support/aterm/pkg`.
    pub prefix: PathBuf,
}

impl Layout {
    /// Whether this is a SYSTEM prefix: the root-owned shape, as opposed to the
    /// per-user `$HOME` shape.
    ///
    /// Decided by the ACTUAL on-disk chain ([`system_chain_trusted`]), not by location.
    /// Location alone is not sound here: a `Layout` can be built directly (tests do,
    /// at 28 sites) without passing through [`vet_prefix`], and "outside `$HOME`" would
    /// then wrongly classify an ordinary user-owned temp dir as a system prefix and
    /// publish it `0755`. Asking the filesystem instead means the mode always follows
    /// the trust shape that is really there, and a prefix that is not root-owned can
    /// never be widened.
    #[must_use]
    pub fn is_system_prefix(&self) -> bool {
        system_chain_trusted(&self.prefix)
    }

    /// Create and harden a directory belonging to this layout, with the mode the
    /// PREFIX SHAPE calls for.
    ///
    /// A `$HOME` prefix is private state: `0700`, owned by us, nobody else's business.
    /// A SYSTEM prefix is the opposite — root writes it and every user must be able to
    /// traverse and execute out of it, so it is `0755`. Hardening a system prefix to
    /// `0700` installs a toolchain that only root can run, which passes every ownership
    /// check (`0700 & 0o022 == 0` satisfies even Trust's launcher predicate) and then
    /// fails at the only moment that matters — the first non-root invocation, with a
    /// bare `Permission denied`. Observed exactly that way before this existed.
    ///
    /// The write itself is still guarded by the prefix chain check: this only decides
    /// who may READ and TRAVERSE, never who may write.
    pub fn ensure_dir(&self, dir: &Path) -> std::io::Result<()> {
        let created = if self.is_system_prefix() {
            crate::platform::ensure_shared_dir(dir)
        } else {
            crate::platform::ensure_private_dir(dir)
        };
        // KEEP THE TOOLCHAIN OUT OF BACKUPS. The store holds multiple GB of
        // extracted compiler and prover binaries that are, by construction,
        // re-downloadable and signature-verifiable from the signed index — Apple's
        // own guidance is that exactly this content should be excluded. It lives
        // under Application Support rather than Caches (it must survive a purge, and
        // the verified lane needs a stable path), so nothing excludes it by default:
        // without this, every Time Machine / Backblaze / Arq run copies ~3.2 GB of
        // re-creatable bytes, and churns them again on every update pass.
        //
        // Applied to the PREFIX only, once, and best-effort: this is a storage
        // courtesy, never a correctness property, so a filesystem that will not take
        // the attribute changes nothing about the install.
        if dir == self.prefix {
            crate::platform::exclude_from_backup(dir);
        }
        created
    }

    /// `agents/` ([`Self::agents_dir`]) ENSURED as a REAL directory — the rule the `aterm`
    /// front door applies before it hands the directory to a TTY session
    /// ([`crate::reroute::AGENTS_DIR_ENV`], 2026-09-18). What a launch needs synchronously
    /// is only that the directory EXIST, so the twins atpkg lays into it later
    /// ([`crate::activate::reconcile_agents`]) are found on the next invocation: one
    /// `mkdir` through [`Self::ensure_dir`] — `0700` in a `$HOME` prefix, `0755` in a
    /// system one — never a wait, and an existing directory is left as it is. A symlink or
    /// a regular file at `agents/` is REFUSED and left alone (a pre-created link must never
    /// capture the twins) — judged by `lstat` BEFORE the `mkdir`, so the refusal is this
    /// function's one-path sentence and never the `update directory …` wording
    /// `ensure_private_dir` would give the same fact. This is the window's mkdir/mode rule
    /// (`aterm-gui::spawn::managed_agents_dir`, 2026-09-16) PLUS that refusal: the window
    /// still hands a symlinked `agents/` after warning about it (`Path::is_dir` follows the
    /// link); the front door hands nothing. One rule in two places until the window is
    /// pointed here. `Ok` is the absolute directory, real and traversable; `Err` is the
    /// sentence for the caller's one stderr line, always starting with the path — the
    /// launch then omits the directory rather than putting a nonexistent entry first on
    /// PATH.
    pub fn ensure_agents_dir(&self) -> Result<PathBuf, String> {
        let dir = self.agents_dir();
        match std::fs::symlink_metadata(&dir) {
            Ok(md) if md.file_type().is_symlink() => {
                return Err(format!("{} is a symlink; refusing", dir.display()));
            }
            Ok(md) if !md.is_dir() => {
                return Err(format!("{} exists and is not a directory", dir.display()));
            }
            _ => {}
        }
        // The bare io error names no path (`Permission denied (os error 13)`); the
        // caller's stderr line must. `ensure_private_dir`'s own refusals already start
        // with it — a link swapped in after the lstat above — and are not prefixed twice.
        self.ensure_dir(&dir).map_err(|error| {
            let text = error.to_string();
            if text.starts_with(&dir.display().to_string()) {
                text
            } else {
                format!("{}: {text}", dir.display())
            }
        })?;
        match std::fs::symlink_metadata(&dir) {
            Ok(md) if md.file_type().is_symlink() => {
                Err(format!("{} is a symlink; refusing", dir.display()))
            }
            Ok(md) if md.is_dir() => Ok(dir),
            Ok(_) => Err(format!("{} exists and is not a directory", dir.display())),
            Err(error) => Err(format!("{}: {error}", dir.display())),
        }
    }

    /// `store/<program>/<build>/` — the versioned, immutable extracted tree.
    #[must_use]
    pub fn build_dir(&self, program: &str, build: u64) -> PathBuf {
        self.prefix
            .join("store")
            .join(program)
            .join(build.to_string())
    }

    /// `bin/` — the only directory placed on the child `PATH` (shims into `current`).
    #[must_use]
    pub fn bin_dir(&self) -> PathBuf {
        self.prefix.join("bin")
    }

    /// `reroute/` — the SESSION-SCOPED directory of upstream-name stubs
    /// ([`crate::reroute`]), prepended FIRST only to the PATH aterm hands its own
    /// children; never `bin/`, never the rc hook, so [`shim_allowed`]'s deny-list
    /// keeps meaning exactly what it means for the managed `bin/`.
    #[must_use]
    pub fn reroute_dir(&self) -> PathBuf {
        crate::reroute::dir(self)
    }

    /// `agents/` — the ONE managed directory that goes FIRST on `PATH` (the shell hook
    /// prepends it, [`crate::hooks`]; the GUI's spawn seam front-inserts it beside
    /// `reroute/`). It holds ONLY the shims of the agent programs
    /// ([`crate::stub::AGENT_PROGRAMS`]), each forwarding exactly where its `bin/` twin
    /// does with the same exported environment ([`crate::activate::reconcile_agents`];
    /// the twin also carries the self-update block, [`crate::selfupdate`]),
    /// so `claude`/`codex` run the managed copy
    /// ahead of a vendor's native install or a brew cask — the rule-1 exception (owner
    /// decision 2026-09-10: aterm is the version manager for the coding agents;
    /// docs/design/DESIGN-which-copy-runs). Every other managed tool stays in `bin/`,
    /// appended LAST as before, so [`shim_allowed`]'s deny-list keeps its meaning.
    #[must_use]
    pub fn agents_dir(&self) -> PathBuf {
        self.prefix.join("agents")
    }

    /// `agents/<tool>` — an agent program's front-of-`PATH` shim ([`Self::agents_dir`]);
    /// the same [`ToolName`] gate as [`Self::shim`].
    #[must_use]
    pub fn agent_shim(&self, tool: &ToolName) -> PathBuf {
        self.agents_dir().join(tool.shim_file())
    }

    /// `landing/` — where passes from 2026-09-16 to 2026-09-22 wrote LANDING MARKERS,
    /// which a twin those clients laid still tests for ([`crate::landing`]). No pass
    /// writes one now; every pass removes the directory ([`crate::landing::sweep`]).
    #[must_use]
    pub fn landing_dir(&self) -> PathBuf {
        self.prefix.join("landing")
    }

    /// `landing/<tool>` — the path an older twin tests for ([`Self::landing_dir`]).
    #[must_use]
    pub fn landing_marker(&self, tool: &ToolName) -> PathBuf {
        self.landing_dir().join(tool.as_str())
    }

    /// `bin/<tool>` — a single shim. The concrete file name is [`ToolName::shim_file`]
    /// (`bin/ay` on Unix, `bin/ay.cmd` on Windows).
    ///
    /// Taking a [`ToolName`] rather than a `&str` is what makes the [`shim_allowed`] gate
    /// unskippable: there is no way to *name* a file in `bin/` without having gone through
    /// [`ToolName::new`], so a sensitive name cannot reach the filesystem because one call
    /// site among several forgot the deny-list.
    #[must_use]
    pub fn shim(&self, tool: &ToolName) -> PathBuf {
        self.bin_dir().join(tool.shim_file())
    }

    /// `channels/<name>/current` — the per-coherence-group active-set symlink (§10).
    ///
    /// **One symlink per CHANNEL, not per program.** Every released-tool install passes the
    /// same config-resolved channel name (`[packages].channel`, default `stable`), and every
    /// coherence-group member flips through it too, so activating `ny` overwrites the link
    /// `ay` just wrote. It therefore answers "what did this channel activate LAST", which is
    /// what `uninstall`'s dangling-link sweep needs and what a GC witness must never be built
    /// on — see [`Layout::program_current`].
    #[must_use]
    pub fn channel_current(&self, channel: &str) -> PathBuf {
        self.prefix.join("channels").join(channel).join("current")
    }

    /// `store/<program>/current` — the PER-PROGRAM active-build symlink, pointing at
    /// `store/<program>/<build>/`.
    ///
    /// This is the authority [`crate::gc::live_builds`] resolves. It exists because the
    /// channel link above cannot answer "which build of *this* program is live": with N
    /// programs on one channel it holds exactly one answer, so N−1 programs would have no
    /// witness and GC would abstain on them forever, growing the store without bound.
    ///
    /// It lives INSIDE `store/<program>/` rather than beside the channel link on purpose:
    /// there is then exactly one per program by construction (no channel can contest
    /// another's claim about the same program), `uninstall`'s `remove_dir_all` of the program
    /// tree takes it away with the builds it names, and it can never collide with a build dir
    /// (`current` does not parse as a `u64`, so [`crate::ops::list_installed`] skips it).
    #[must_use]
    pub fn program_current(&self, program: &str) -> PathBuf {
        self.prefix.join("store").join(program).join("current")
    }

    /// `staging/<program>/` — the per-program download + stage scratch.
    #[must_use]
    pub fn staging_dir(&self, program: &str) -> PathBuf {
        self.prefix.join("staging").join(program)
    }

    /// `floor` — the `0600` durable high-water `index_build` file (§8).
    ///
    /// Read together with [`Self::floor_generation`]; the pair is one value
    /// ([`crate::sig::BuildFloor`]), because `index_build` is a number a MACHINE chooses and
    /// a floor set by a machine must not outlive the roster generation that revoked it.
    #[must_use]
    pub fn floor(&self) -> PathBuf {
        self.prefix.join("floor")
    }

    /// `floor.gen` — the `0600` roster generation that recorded the current [`Self::floor`].
    ///
    /// Its own file rather than a second field inside `floor` so the format of `floor` stays
    /// what it has always been (a bare integer) and a store written by an older build reads
    /// as "floor from generation 0", which the strictly-newer rule then re-bases on first
    /// contact — the right answer, since no index that floor ever admitted can verify under
    /// the single-root chain at all.
    ///
    /// Missing or unreadable reads as `0`, which makes the floor bind at a generation no
    /// real roster carries. That is the fail-closed direction: it never waives a floor for
    /// the generation that set it.
    #[must_use]
    pub fn floor_generation(&self) -> PathBuf {
        self.prefix.join("floor.gen")
    }

    /// `roster.floor` — the `0600` durable high-water `roster_seq` file: the replay
    /// ratchet for the master-signed machine roster that authorizes this store's index.
    ///
    /// A SEPARATE file from [`Self::floor`] on purpose. The two counters move
    /// independently — minting or revoking a machine bumps `roster_seq` without re-cutting
    /// the index, and re-publishing the index bumps `index_build` without touching the
    /// roster — so folding them into one high-water would make each one's advance silently
    /// ratchet the other past documents that are still perfectly current.
    ///
    /// It is also deliberately atpkg's OWN file rather than shared with `aterm-update`'s
    /// ratchet, even though both track the same document: the two live under different
    /// prefixes with different ownership and lifetimes (uninstalling the toolchain store
    /// must not reset the app updater's replay defence, or vice versa). A client that has
    /// updated the app more recently than the toolchain simply carries a higher floor
    /// there, which costs nothing — each ratchet only ever refuses what IT has already
    /// seen superseded.
    #[must_use]
    pub fn roster_floor(&self) -> PathBuf {
        self.prefix.join("roster.floor")
    }

    /// `store.lock` — the `0600` store-wide single-writer advisory lock file
    /// ([`crate::lock`]). TRY-acquired at the CLI edge by every verb that mutates the
    /// store, so exactly one process at a time stages/activates/discards builds here.
    #[must_use]
    pub fn store_lock(&self) -> PathBuf {
        self.prefix.join("store.lock")
    }

    /// `status.toml` — the aggregate observability record.
    #[must_use]
    pub fn status(&self) -> PathBuf {
        self.prefix.join("status.toml")
    }

    /// `machine-apply.stamp` — when a launch last started `aterm pkg machine apply` (unix
    /// seconds, one line): a terminal session and a window opening claim it once a day
    /// between them ([`crate::machine::launch_apply_due`]) instead of walking `$HOME` at
    /// every launch.
    #[must_use]
    pub fn machine_apply_stamp(&self) -> PathBuf {
        self.prefix.join("machine-apply.stamp")
    }

    /// `session-pass.stamp` — when a terminal session last spawned its detached `aterm pkg
    /// update` (unix seconds, one line): claimed before the spawn, so tabs opened together
    /// start one pass, not one each.
    #[must_use]
    pub fn session_pass_stamp(&self) -> PathBuf {
        self.prefix.join("session-pass.stamp")
    }

    /// `progress.json` — the LIVE install-progress snapshot ([`crate::progress`]), the
    /// in-flight complement to [`Self::status`]'s durable per-pass record. Written only
    /// by the process holding the store flock (via `--progress-file`); read by the GUI
    /// card, the pending-program stubs (`atpkg __pending`), and anyone tailing it —
    /// all under the untrusted-reader rules the progress module documents.
    #[must_use]
    pub fn progress_file(&self) -> PathBuf {
        self.prefix.join("progress.json")
    }

    /// `bump` — the priority-queue channel ([`crate::progress`]): one program name per
    /// line, appended by pending-program stubs, consumed by the installer between
    /// items. Reorder-only by construction — the installer intersects it with the work
    /// the signed index already planned, so this file can never ADD work.
    #[must_use]
    pub fn bump_file(&self) -> PathBuf {
        self.prefix.join("bump")
    }

    /// `adopted` — the durable marker that this machine RUNS THE ALAB TOOLSET, as a set
    /// rather than as a handful of individually-chosen programs (§11).
    ///
    /// Written when the whole default set is laid down deliberately: the batteries-included
    /// first-run seed bootstrap, or an explicit `install --default-set` (the Settings
    /// "Install ALab toolset" button). NOT written by `install <program>` — asking for one
    /// tool is not adopting the suite.
    ///
    /// It exists because one config bit was once doing two unrelated jobs: a default-OFF
    /// `[packages].auto_install` answered "may atpkg pull a multi-GB toolchain onto a
    /// machine that has never had one?", and the update pass ALSO read it to answer "should
    /// a machine that already runs this toolset keep that set complete?" — so a program
    /// published to the index AFTER a user installed simply never arrived. Adoption
    /// separates the two: consent is given once, and alignment thereafter is not a new
    /// consent event. Since 2026-09-23 the consent is ONE default-on key
    /// (`auto_install`, which folded in `seed_install`), and the update pass completes the
    /// set of an ADOPTED machine while it holds (`cli::should_complete_set`).
    ///
    /// CLEARED by `uninstall` (see `crate::ops::uninstall`'s caller): removing a managed
    /// program is an explicit act, and set-completion must never fight it by reinstalling on
    /// the next pass. The durable way to drop ONE program while staying adopted is
    /// `[packages].exclude`, which the default-set planner already honours.
    ///
    /// Existence-only, and silent on who adopted: that is a separate record,
    /// [`Self::adopted_by_seed`].
    #[must_use]
    pub fn adopted(&self) -> PathBuf {
        self.prefix.join("adopted")
    }

    /// `adopted-by-seed` — who adopted: present only when `atpkg seed`, the first launch's
    /// bootstrap, created the [`Self::adopted`] marker itself. Existence only. It gates one
    /// sentence: only beside this record may an update pass say "installing aterm is the
    /// consent" and name `[packages].auto_install = false` as the switch that would have
    /// stopped it. Cleared with `adopted`; `install --default-set` never writes one, so a
    /// marker without a record claims no consent beyond the uninstall.
    #[must_use]
    pub fn adopted_by_seed(&self) -> PathBuf {
        self.prefix.join("adopted-by-seed")
    }

    /// `removed` — programs the user uninstalled INDIVIDUALLY, one name per line.
    ///
    /// The whole-set `declined` marker cannot express this, and without it the
    /// resumable seed lane silently undoes a targeted removal: the lane installs
    /// whatever the store LACKS (that is what lets an interrupted first run finish),
    /// so `atpkg uninstall ny` came back on the next launch. A package manager that
    /// reinstalls what you just removed is worse than one that never had the package.
    ///
    /// An explicit `install <program>` or `install --default-set` clears the relevant
    /// entries — asking for it back is unambiguous.
    #[must_use]
    pub fn removed(&self) -> PathBuf {
        self.prefix.join("removed")
    }

    /// The programs this machine removed ON PURPOSE.
    ///
    /// Lives here rather than in the CLI because the UNATTENDED lanes need it too:
    /// a coherence group is processed whenever any member is installed, and a
    /// missing sibling is pulled back in to keep the tuple locked — which silently
    /// re-downloaded programs the user had just uninstalled (`aterm pkg uninstall
    /// trust` frees ~3.2 GB; the next six-hourly tick put it straight back). The
    /// record was already durable; only the update path never read it
    /// (2026-08-20 round-8 audit).
    #[must_use]
    pub fn removed_programs(&self) -> std::collections::BTreeSet<String> {
        std::fs::read_to_string(self.removed())
            .map(|text| {
                text.lines()
                    .map(str::trim)
                    .filter(|line| !line.is_empty() && !line.starts_with('#'))
                    .map(str::to_string)
                    .collect()
            })
            .unwrap_or_default()
    }

    /// `declined` — the durable "this machine does not want the bundled toolset"
    /// marker, written by `uninstall --all`.
    ///
    /// Without it the removal does not stick. The seed lane fires on every launch and
    /// installs whatever channel-pinned members the store LACKS (that resumability is
    /// what lets an interrupted first run finish), so a user who removed the toolset
    /// would find the whole 3.2 GB back after the next launch — the manager undoing a
    /// deliberate act, which is the single most infuriating thing a package manager
    /// can do. `adopted` cannot carry this: its absence means "never adopted", which
    /// is exactly the state a first run must install from.
    ///
    /// Cleared by any explicit install (`install --default-set`, `install <program>`):
    /// asking for the toolset is unambiguous, and it must not be necessary to find and
    /// delete a marker file to undo a decline.
    #[must_use]
    pub fn declined(&self) -> PathBuf {
        self.prefix.join("declined")
    }

    /// `retired/` — the per-program RETIREMENT markers directory. One `0600` regular file
    /// per program whose managed copy atpkg retired in favour of a system install
    /// (`cli::satisfy_by_system`); its first line is the day it happened (`YYYY-MM-DD`),
    /// which the canonical `system: <path> — not managed by aterm (managed copy retired
    /// <date>)` state and `atpkg which` read back. Cleared when the program is
    /// uninstalled on purpose; a later managed reinstall simply overwrites the next
    /// retirement's date.
    #[must_use]
    pub fn retired_dir(&self) -> PathBuf {
        self.prefix.join("retired")
    }

    /// `retired/<program>` — one retirement marker, name-gated by [`ToolName`].
    #[must_use]
    pub fn retired_marker(&self, program: &str) -> PathBuf {
        self.retired_dir().join(program)
    }

    /// The `YYYY-MM-DD` day a managed copy of `program` was retired for a system
    /// install, if this machine recorded one: the first line of a REGULAR, non-symlink
    /// marker (a planted link is not a record), trimmed, and only when it reads as a
    /// date shape (ten bytes, digits and two dashes) — a scribbled-over marker answers
    /// `None` rather than a mangled date.
    #[must_use]
    pub fn retired_date(&self, program: &str) -> Option<String> {
        ToolName::new(program)?;
        let path = self.retired_marker(program);
        if !std::fs::symlink_metadata(&path).is_ok_and(|m| m.file_type().is_file()) {
            return None;
        }
        let text = crate::metadata_io::read_bounded_regular_utf8(&path, 4096).ok()?;
        let first = text.lines().next()?.trim();
        let shaped = first.len() == 10
            && first.bytes().enumerate().all(|(i, b)| {
                if i == 4 || i == 7 {
                    b == b'-'
                } else {
                    b.is_ascii_digit()
                }
            });
        shaped.then(|| first.to_string())
    }

    /// Record that `program`'s managed copy was retired on `date` (`YYYY-MM-DD`).
    /// Overwrites an earlier record: the newest retirement is the one the row names.
    ///
    /// # Errors
    /// The name is not a [`shim_allowed`] shape, the directory could not be created or
    /// hardened, the marker path is occupied by something that is not a regular file, or
    /// the write failed.
    pub fn record_retired(&self, program: &str, date: &str) -> std::io::Result<()> {
        if ToolName::new(program).is_none() {
            return Err(std::io::Error::new(
                std::io::ErrorKind::InvalidInput,
                "retirement name is not an installable program name",
            ));
        }
        let dir = self.retired_dir();
        self.ensure_dir(&dir)?;
        let path = self.retired_marker(program);
        match std::fs::symlink_metadata(&path) {
            Ok(m) if m.file_type().is_file() => {
                let _ = std::fs::remove_file(&path);
            }
            Ok(_) => {
                return Err(std::io::Error::new(
                    std::io::ErrorKind::AlreadyExists,
                    "retirement marker path is occupied by something that is not a marker",
                ));
            }
            Err(_) => {}
        }
        let mut f = crate::platform::open_create_write(&path, 0o600)?;
        use std::io::Write as _;
        f.write_all(date.as_bytes())?;
        f.write_all(
            b"
# The day atpkg retired its managed copy of this program because a system install of
# the same name appeared on PATH. Read back by `aterm pkg which` and the status row.
",
        )
    }

    /// Forget the retirement record for `program` (a no-op when none exists). Regular
    /// files only — a planted symlink is left alone.
    pub fn clear_retired(&self, program: &str) {
        if ToolName::new(program).is_none() {
            return;
        }
        let path = self.retired_marker(program);
        if std::fs::symlink_metadata(&path).is_ok_and(|m| m.file_type().is_file()) {
            let _ = std::fs::remove_file(&path);
        }
    }

    /// Forget EVERY retirement record (`uninstall --all`).
    pub fn clear_all_retired(&self) {
        let Ok(entries) = std::fs::read_dir(self.retired_dir()) else {
            return;
        };
        for entry in entries.flatten() {
            if let Some(name) = entry.file_name().to_str() {
                self.clear_retired(name);
            }
        }
    }

    /// `links/` — the per-program dev-link markers directory (§13). One `0600` marker per
    /// dev-linked program; its presence makes `update`/`apply` HARD-SKIP that program.
    #[must_use]
    pub fn links_dir(&self) -> PathBuf {
        self.prefix.join("links")
    }

    /// `links/<program>` — one dev-link marker. Only ever joined with a
    /// [`shim_allowed`]-shape program name (linkmode gates the name before calling).
    #[must_use]
    pub fn link_marker(&self, program: &str) -> PathBuf {
        self.links_dir().join(program)
    }
}

/// The per-build completeness marker: a SIBLING file `store/<program>/<build>.ready`
/// next to the `<build>/` dir. It sits OUTSIDE the build tree deliberately, so it can
/// never perturb the build's `tree_root` hash (the apply-time TOCTOU re-verify). It is
/// written LAST by `verify_and_stage`, once the extracted tree has passed sha256 +
/// tree_root re-verify; its presence is the sole "this build is fully installed"
/// signal, so a build dir left partial by a crash mid-extract (which has no marker)
/// reads as absent and is re-installed rather than mistaken for up-to-date.
///
/// `None` if `build_dir` has no final path component (never, for a real build dir).
fn ready_marker_path(build_dir: &Path) -> Option<PathBuf> {
    // `call1` routing + manual concat (no `format!`): Trust-gate lowering
    // workaround — see `lib.rs::call1`.
    let name = crate::call1(std::path::Path::file_name, build_dir)?;
    let name = crate::call1(std::ffi::OsStr::to_str, name)?;
    let mut marker = String::from(name);
    marker.push_str(".ready");
    Some(build_dir.with_file_name(marker))
}

/// The key under which the readiness marker records WHICH SLICE installed the build.
/// Its spelling is a compatibility surface, not a detail — see [`ready_text_accepts`].
const READY_PLATFORM_KEY: &str = "platform=";

/// The key under which the readiness marker records WHAT THE BUILD CONTAINED when it was
/// marked — the direct entries of its `bin/`, the files every shim and every exec root
/// forward to. Its spelling is a compatibility surface for the same reason
/// [`READY_PLATFORM_KEY`]'s is: a re-spelling silently demotes every marker this version
/// wrote back to "no record", which is accepted (see [`ready_text_accepts`]) and therefore
/// unchecked.
///
/// **WHY READINESS HAD TO BECOME A CLAIM ABOUT CONTENTS (2026-09-17).** The marker used to
/// be a file someone remembered to write, and nothing ever asked whether the tree beside it
/// still held anything. Measured on m3: `store/trust/8595/` with no `bin/` at all, 417 MB of
/// orphaned `lib/`, and `8595.ready` beside it still saying `ok` — a corpse
/// [`crate::ops::list_installed`] counted as an installed build, [`crate::gc`] refused to
/// sweep (it sweeps only marker-LESS trees), and [`crate::flow::rollback`] would have
/// selected as its rollback target, re-pointing every shim at a build with no compiler in it
/// and disarming the machine. The cause of the corpse is fixed where corpses are made
/// ([`discard_build`], [`crate::ops::uninstall`]: the marker now comes down BEFORE the tree
/// goes, so an interrupt leaves a tree that reads as incomplete rather than a lie that reads
/// as complete). This key is the other half: a marker that records what it vouched for can
/// be REFUTED by the disk, so the class cannot go unnoticed however a tree loses its
/// contents — an interrupted `rm -rf` a person typed, a volume that dropped a directory, a
/// version of this manager older than the fix.
///
/// Two values, so "recorded nothing" and "recorded that there was nothing" stay apart:
///
/// * `contents=none` — the build had no `bin/` entries when it was marked. Nothing to check.
/// * `contents=bin:<name>,<name>,…` — the sorted names of `bin/`'s direct entries. Each must
///   still be there ([`contents_still_stand`]).
///
/// The key is OMITTED — leaving the marker exactly as lenient as every marker written before
/// this version — when the inventory cannot be encoded: a `bin/` that cannot be read for a
/// reason that is not `NotFound` (a bound on what this process may know is never written
/// down as a fact about the build), or a name carrying a `,` or a newline. The writer says
/// only what it can encode, and the reader checks only what was written.
const READY_CONTENTS_KEY: &str = "contents=";

/// [`READY_CONTENTS_KEY`]'s value for a build whose `bin/` held nothing (or is absent).
/// A recorded emptiness, distinct from the absence of a record.
const READY_CONTENTS_NONE: &str = "none";

/// [`READY_CONTENTS_KEY`]'s prefix for a recorded `bin/` inventory: `bin:<name>,<name>,…`.
const READY_CONTENTS_BIN: &str = "bin:";

/// Line 1 of every readiness marker every version of atpkg has ever written — and now the
/// WELL-FORMEDNESS gate a marker must pass before [`ready_text_accepts`] reads anything
/// else out of it. Bytes that do not carry this line were not written by this writer, so
/// they are a crash artefact rather than a legacy marker. One spelling, shared by the
/// writer ([`mark_build_ready`]) and the reader, so the two cannot drift.
const READY_OK_LINE: &str = "ok";

/// `<arch>-<os>` for the atpkg slice that is RUNNING: `aarch64-macos`, `x86_64-macos`,
/// `x86_64-linux`, …
///
/// **THE HAZARD THIS EXISTS FOR, concretely.** The store path carries no architecture: a
/// build lives at `store/<program>/<build>/` and is vouched for by the sibling
/// `<build>.ready`, while `cli::current_triple()` — which decides WHICH artifact row to
/// download — is a compile-time `cfg(target_arch)`. The shipped atpkg is a UNIVERSAL
/// binary, so both of its slices can run on the same Apple Silicon Mac: `arch -x86_64`,
/// an x86_64 parent shell, or Finder ▸ Get Info ▸ "Open using Rosetta" each start the
/// x86_64 slice, which selects `x86_64-apple-darwin` rows. Without a record of who wrote
/// it, that slice installs INTEL compilers and provers into the very same
/// `store/<program>/<build>/` and writes the very same `<build>.ready` — and every later
/// NATIVE arm64 run reads that marker as "build <n> is installed", skips it, and leaves
/// the machine on Rosetta-translated solvers forever, silently, with `atpkg status`
/// reporting a correct and up-to-date toolchain. Unreachable until 2026-08-21, because no
/// `x86_64-apple-darwin` row had ever been published; reachable the moment index build 12
/// gave six programs both rows.
///
/// So the marker records this value and [`build_is_complete`] refuses one that does not
/// match: a store populated by the other slice reads as NOT installed and is re-staged
/// natively. That is a re-download, never an error — the direction that repairs itself.
///
/// WHO RE-STAGES IT, precisely — because "reads as not installed" is a claim about READERS,
/// and for a LIVE build only some of them read this marker. [`crate::ops::list_installed`],
/// `doctor` and `gc` always did. The APPLY DECISION did not: [`crate::gate::decide`] is fed
/// [`crate::ops::active_builds`], the `bin/` shim view, which resolves a shim and stats its
/// target without ever opening `<build>.ready`. So a live build carrying the other slice's
/// marker was `UpToDate` forever, and doctor's named remedy (`install <program>`) came back
/// through that same view and printed "already current" — the hazard above, left intact and
/// merely made noisy. `flow::installed_for_decide` is the input that closes it: every apply
/// path now drops a live build this predicate refuses BEFORE deciding, so the re-stage this
/// paragraph promises is the one that actually happens.
///
/// Deliberately `std::env::consts` and NOT the artifact triple (`aarch64-apple-darwin`):
/// std exposes no target triple, so spelling one here means duplicating
/// `cli::current_triple`'s `cfg` ladder in a file that cannot see it, and any later drift
/// between the two copies would read as a mismatch on EVERY machine — re-downloading a
/// ~3.2 GB toolchain because of a cosmetic edit. Both consts are per-slice at compile
/// time (a universal binary is two separately compiled binaries stitched together, so
/// each slice reports its own), which is the only property the comparison needs.
fn running_platform() -> String {
    // Manual concat (no `format!`): Trust-gate lowering workaround — see `lib.rs::dec_u64`.
    let mut s = String::new();
    s.push_str(std::env::consts::ARCH);
    s.push('-');
    s.push_str(std::env::consts::OS);
    s
}

/// The platform a marker's text records, or `None` when it records none — which covers
/// BOTH "written before this field existed" (a bare `ok\n`) and any key this version does
/// not recognise. An empty value is no record either.
fn recorded_platform(text: &str) -> Option<&str> {
    text.lines()
        .filter_map(|line| line.trim().strip_prefix(READY_PLATFORM_KEY))
        .map(str::trim)
        .find(|p| !p.is_empty())
}

/// The `contents=` record a marker's text carries, or `None` when it carries none — which
/// covers BOTH "written before this field existed" and a value this version does not
/// recognise. An empty value is no record either, exactly as for the platform key.
fn recorded_contents(text: &str) -> Option<&str> {
    text.lines()
        .filter_map(|line| line.trim().strip_prefix(READY_CONTENTS_KEY))
        .map(str::trim)
        .find(|v| !v.is_empty())
}

/// The inventory [`mark_build_ready`] is about to record for `build_dir`, or `None` when it
/// cannot be encoded (see [`READY_CONTENTS_KEY`]).
///
/// The names are the direct entries of `bin/` — read with [`std::fs::read_dir`], which does
/// not follow the entries themselves, so a `bin/clippy -> ../lib/…` link is recorded by its
/// own name and checked by its own name. Sorted, so the record is a function of the tree and
/// not of `readdir` order: two markers for the same tree are byte-identical, which is what
/// lets a test compare them and a human diff them.
fn bin_inventory(build_dir: &Path) -> Option<String> {
    let bin = build_dir.join("bin");
    let entries = match std::fs::read_dir(&bin) {
        Ok(entries) => entries,
        // A `bin/` that is not there is a FACT about the build, and one worth recording:
        // a build marked with no bin entries is never later refuted for having none.
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
            return Some(String::from(READY_CONTENTS_NONE));
        }
        // Anything else is a bound on what this process may know — EACCES on the directory,
        // EIO, macOS privacy consent. Recording `none` there would write that bound down as
        // a fact about the build and unprotect it forever; omitting the key leaves the
        // marker exactly as lenient as it was before this field existed.
        Err(_) => return None,
    };
    let mut names: Vec<String> = Vec::new();
    for entry in entries {
        // One unreadable entry is the same bound as an unreadable directory: a partial
        // inventory recorded as a whole one would refute a build that is intact.
        let entry = entry.ok()?;
        let name = entry.file_name().into_string().ok()?;
        // A name this encoding cannot round-trip. Nothing atpkg stages produces one
        // ([`ToolName`] admits neither), but `bin/` is a directory on the user's disk.
        if name.contains(',') || name.contains('\n') || name.contains('\r') || name.is_empty() {
            return None;
        }
        names.push(name);
    }
    if names.is_empty() {
        return Some(String::from(READY_CONTENTS_NONE));
    }
    names.sort();
    names.dedup();
    // Manual concat (no `format!`): Trust-gate lowering workaround — see `lib.rs::dec_u64`.
    let mut out = String::from(READY_CONTENTS_BIN);
    let mut first = true;
    for name in &names {
        if !first {
            out.push(',');
        }
        first = false;
        out.push_str(name);
    }
    Some(out)
}

/// Whether the tree at `build_dir` still holds what its marker's `contents=` record
/// vouches for.
///
/// **Only a PROVEN absence refutes.** Each recorded name is `symlink_metadata`'d — not
/// `exists()`, which would read an EACCES on a shared prefix, or macOS privacy consent, as
/// "the tool is gone" and re-stage a multi-GB toolchain over a permissions answer. The
/// three-answer rule [`presence`] exists for, applied to the one question that can
/// un-install a build. `symlink_metadata`, not `metadata`: a `bin/` entry that is a symlink
/// into the build's own `lib/` is present as itself even when what it names is not, and the
/// dangling-link case is [`crate::doctor`]'s to report, not this predicate's to re-stage on.
///
/// An unrecognised value is not a refutation either: a marker written by a LATER atpkg that
/// records something this version cannot parse must read as "no record", never as "refuted".
fn contents_still_stand(build_dir: &Path, recorded: &str) -> bool {
    if recorded == READY_CONTENTS_NONE {
        return true;
    }
    let Some(list) = recorded.strip_prefix(READY_CONTENTS_BIN) else {
        return true;
    };
    let bin = build_dir.join("bin");
    !list
        .split(',')
        .filter(|n| !n.is_empty())
        .any(|name| is_provably_absent(&bin.join(name)))
}

/// Whether `path` is PROVABLY not there, by its own link (never by its target).
/// The `symlink_metadata` twin of [`Presence::is_absent`].
fn is_provably_absent(path: &Path) -> bool {
    matches!(std::fs::symlink_metadata(path), Err(e) if e.kind() == std::io::ErrorKind::NotFound)
}

/// Whether a readiness marker with this text vouches for its build to a `running` slice.
///
/// **BACKWARD COMPATIBILITY, decided here and only here: an absent platform record means
/// ACCEPT.** Every marker written before this field existed is a bare `ok\n`, and reading
/// those as a mismatch would make the first run of this version re-download every
/// installed program on every existing machine — a multi-GB reinstall storm, to close a
/// hazard none of those stores can be in (nothing published an `x86_64-apple-darwin` row
/// before index build 12, so no marker already on disk can be the Intel-under-Rosetta
/// case). The record is acquired LAZILY instead: the next ordinary install or update of a
/// program stamps it, and that build is protected from then on. An absent record is
/// therefore "unknown, and old enough to be safe" — never "corrupt", and never a reason
/// to reinstall.
///
/// The same rule is what makes [`READY_PLATFORM_KEY`]'s spelling a compatibility surface:
/// re-spelling it silently demotes every marker this version wrote back to "no record"
/// (harmless — they are accepted) but also unprotects them, so a re-spelling has to keep
/// reading the old key rather than simply replacing it.
///
/// **AN ABSENT RECORD IS NOT AN ABSENT MARKER.** The lenience above is about a marker
/// that is WELL FORMED and merely predates the platform field — a bare `ok\n`, which is
/// what every earlier version wrote. It used to be reached by a ZERO-LENGTH file too, and
/// that is a different thing entirely: `<build>.ready` is published by a rename, and a
/// rename commits a NAME while the bytes behind it are still page cache, so a marker with
/// no contents is precisely what a power loss leaves behind. Reading that as "installed,
/// no record" vouched for a build whose tree may equally have lost its data — the state
/// nothing downstream ever repairs ([`crate::gate::decide`] answers `UpToDate` and the
/// superseded tree is already reclaimed). So [`READY_OK_LINE`] is REQUIRED: a marker with
/// no readable `ok` first line — empty, NUL-filled, a torn prefix — is refused and the
/// build re-stages, which is the direction that repairs itself. Only a well-formed marker
/// gets the legacy lenience.
fn ready_text_accepts(text: &str, running: &str) -> bool {
    if !first_line_is_ok(text) {
        return false;
    }
    match recorded_platform(text) {
        None => true,
        Some(recorded) => recorded == running,
    }
}

/// Whether a well-formed marker's CONTENTS record (if any) is still true of `build_dir`.
/// Split out of [`ready_text_accepts`] because it is the one clause that reads the disk;
/// the rest of that predicate is a function of the text alone and its tests say so.
fn ready_contents_accept(text: &str, build_dir: &Path) -> bool {
    match recorded_contents(text) {
        None => true,
        Some(recorded) => contents_still_stand(build_dir, recorded),
    }
}

/// Whether a marker's FIRST line is the historical [`READY_OK_LINE`] — the
/// well-formedness gate [`ready_text_accepts`] applies before the lenient platform rule.
/// Surrounding whitespace (a `\r` from a Windows-y tool, the same tolerance
/// [`recorded_platform`] already had) is not a different line; a missing trailing newline
/// is not a torn marker either, since `ok` alone is the whole of line 1.
fn first_line_is_ok(text: &str) -> bool {
    matches!(text.lines().next(), Some(first) if first.trim() == READY_OK_LINE)
}

/// What this process could learn about whether `path` is there — THREE answers,
/// because there are three.
///
/// [`Path::exists`] has only two, and it spends them badly: it is
/// `fs::metadata(..).is_ok()`, so every reason a stat can fail — EACCES on a parent
/// directory, EPERM from macOS privacy consent, EIO, ELOOP, ENAMETOOLONG — is spent
/// on `false`, the same answer it gives for a path that genuinely is not there. That
/// collapses a bound on what THIS PROCESS MAY KNOW into a fact about THE WORLD, and
/// atpkg's consumers read it as a fact: a live program dropped out of
/// [`crate::ops::active_builds`], doctor printed `FAIL — broken bin shim` over a shim
/// that runs, and [`caller_can_use_prefix`] abandoned the shared store it exists to
/// keep. Each of those implies the same remedy — reinstall — and each repairs nothing,
/// which is the most expensive kind of wrong a package manager can be about its own
/// store.
///
/// [`Presence::Unknown`] carries the error so a diagnostic can SAY why it cannot tell,
/// rather than picking a verdict and calling it a measurement. `Absent` is reserved
/// for the one answer a stat actually proves.
#[derive(Debug)]
pub enum Presence {
    /// Stat succeeded: something is there.
    Present,
    /// Stat said `NotFound`, the only error that is evidence about the path itself.
    Absent,
    /// Stat failed for a reason that says nothing about the path.
    Unknown(std::io::Error),
}

impl Presence {
    /// True ONLY when the path is provably not there. An `Unknown` answers `false`
    /// here on purpose: a caller asking "may I act as though this is gone?" must be
    /// told no when nobody looked.
    #[must_use]
    pub fn is_absent(&self) -> bool {
        matches!(self, Self::Absent)
    }
}

/// Ask the filesystem about `path`, keeping the three answers apart.
///
/// Follows symlinks, exactly as [`Path::exists`] does, so a dangling link is `Absent`
/// (its target is what was asked about) and every existing caller keeps the behaviour
/// it was written for on the two paths a stat can actually decide.
#[must_use]
pub fn presence(path: &Path) -> Presence {
    match std::fs::metadata(path) {
        Ok(_) => Presence::Present,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Presence::Absent,
        Err(e) => Presence::Unknown(e),
    }
}

/// Whether `build_dir` holds a COMPLETE install **for the running slice**: its sibling
/// completeness marker exists, and the platform that marker records (if any) is ours.
///
/// A marker naming the OTHER slice of the universal binary reads as not-complete rather
/// than as an error, so the build is re-staged from the row `cli::current_triple()`
/// selects — see [`running_platform`] for the Rosetta hazard that motivates it.
#[must_use]
pub fn build_is_complete(build_dir: &Path) -> bool {
    let Some(marker) = ready_marker_path(build_dir) else {
        return false;
    };
    // THE TREE ITSELF, before its marker is even opened. [`mark_build_ready`] has no
    // existence precondition — it writes a temp file beside the build and renames it onto
    // `<build>.ready` without ever looking at `<build>` — and `install::restore_outgoing`'s
    // doc has named the consequence since it was written: a marker for a tree that is not
    // there is "a durable lie one refactor away from being believed". This is the refactor,
    // so the lie is refused at the door. Only a PROVEN absence refuses (the whole point of
    // [`presence`]): an EACCES on a shared prefix must never un-install a build.
    if is_provably_absent(build_dir) {
        return false;
    }
    match std::fs::read_to_string(&marker) {
        Ok(text) => {
            ready_text_accepts(&text, &running_platform())
                && ready_contents_accept(&text, build_dir)
        }
        Err(e) if e.kind() == std::io::ErrorKind::InvalidData => false,
        // Present and READ, but not text at all. Every marker this writer produces is
        // ASCII (`ok\n` plus an ASCII platform record), so bytes that are not UTF-8 were
        // never written here — they are the same crash artefact the `ok`-line rule
        // refuses, arriving as a read error instead of as a string. That is evidence
        // about CONTENT, so it decides the content question; the arm below is for the
        // errors that say nothing about content.
        // Present, but not readable AS TEXT: a directory planted at the marker path, a
        // permissions oddity, a filesystem handing back non-UTF-8. `exists()` — the whole
        // of this predicate before the platform record — answered `true` for all of those,
        // so keep that answer. This check must not start reporting long-installed builds
        // as missing because of a byte it never used to read.
        Err(e) if e.kind() != std::io::ErrorKind::NotFound => true,
        Err(_) => false,
    }
}

/// Whether `build_dir` carries a well-formed completeness marker recording the *other*
/// slice of the universal binary — a finished install, complete for that slice, not for us.
///
/// [`build_is_complete`] answers `false` here too, and a caller reading that as "no install
/// ever finished" is [`crate::gc`]'s interrupted-install sweep, whose verdict is a
/// `remove_dir_all`. As strict as the acceptance rule: an `ok` first line
/// ([`first_line_is_ok`]), a platform record present ([`recorded_platform`] — an absent one
/// is the legacy `ok\n` this slice accepts anyway), and that record not ours.
pub(crate) fn build_marks_another_slice(build_dir: &Path) -> bool {
    let Some(marker) = ready_marker_path(build_dir) else {
        return false;
    };
    let Ok(text) = std::fs::read_to_string(&marker) else {
        return false;
    };
    first_line_is_ok(&text)
        && recorded_platform(&text).is_some_and(|p| p != running_platform().as_str())
}

/// Atomically AND DURABLY mark `build_dir` complete (temp + fsync + rename, so a crash
/// during the write leaves NO marker rather than a half-written one). Call as the LAST
/// staging step.
///
/// The fsync is not ceremony. A rename publishes a NAME — metadata — and the bytes behind
/// that name stay in the page cache until something flushes them, so on any filesystem
/// that can commit metadata ahead of data a power loss could expose `<build>.ready` as a
/// correctly-named ZERO-LENGTH file. This is the commit record of an entire multi-GB
/// stage: it has to be on disk before its name is. (A refusal from a volume that cannot
/// flush is not a failure — see [`sync_contents_or_accept_refusal`].)
///
/// The text also records WHICH SLICE of the universal binary installed the build
/// ([`running_platform`]), so the other slice cannot silently inherit the verdict. Line 1
/// stays the historical `ok`, so a marker written here still reads — to a human, to
/// `cat`, and to anything that only asks whether the file is there — exactly like the
/// ones every earlier version wrote.
pub fn mark_build_ready(build_dir: &Path) -> std::io::Result<()> {
    let dest = ready_marker_path(build_dir).ok_or_else(|| {
        std::io::Error::new(std::io::ErrorKind::InvalidInput, "build dir has no name")
    })?;
    let parent = dest.parent().unwrap_or(build_dir);
    // Manual (byte-identical) render of `format!(".ready.tmp-{pid}")`: Trust-gate
    // lowering workaround — see `lib.rs::dec_u64`.
    let mut tmp_name = String::from(".ready.tmp-");
    tmp_name.push_str(&crate::dec_u64(u64::from(std::process::id())));
    let tmp = parent.join(tmp_name);
    // Manual concat (no `format!`): Trust-gate lowering workaround — see `lib.rs::dec_u64`.
    let mut body = String::from(READY_OK_LINE);
    body.push('\n');
    body.push_str(READY_PLATFORM_KEY);
    body.push_str(&running_platform());
    body.push('\n');
    // AND WHAT THIS MARKER IS VOUCHING FOR ([`READY_CONTENTS_KEY`]): the `bin/` inventory
    // as it stands right now, so a later reader can REFUTE the marker against the tree
    // instead of believing it. Derived here, from the tree, rather than passed in by the
    // stager: readiness is then a claim this function makes about what it can see, and no
    // caller can assert a completeness it did not measure.
    if let Some(contents) = bin_inventory(build_dir) {
        body.push_str(READY_CONTENTS_KEY);
        body.push_str(&contents);
        body.push('\n');
    }
    {
        use std::io::Write as _;
        // `0644` explicitly rather than `fs::write`'s umask-dependent `0666 & ~umask`:
        // under a SYSTEM prefix this file is root-owned and every unprivileged reader
        // must still be able to read the verdict it carries — the same argument the
        // durable floor settled (`sig.rs`).
        let mut f = crate::platform::open_create_write(&tmp, 0o644)?;
        f.write_all(body.as_bytes())?;
        // Inside the braces: on stable storage BEFORE the handle drops, and before the
        // rename below publishes the name.
        sync_contents_or_accept_refusal(&f)?;
    }
    std::fs::rename(&tmp, &dest)
}

/// Remove the completeness marker, making `build_dir` read as NOT installed.
///
/// Called FIRST when a stage is about to replace a live tree: for the window in which the
/// old tree is being swapped out and the new one in, the build genuinely is not complete,
/// and a crash inside that window must leave it re-installable. Best-effort on a missing
/// marker (that is already the state we want).
pub(crate) fn clear_build_ready(build_dir: &Path) -> std::io::Result<()> {
    let Some(marker) = ready_marker_path(build_dir) else {
        return Ok(());
    };
    match std::fs::remove_file(&marker) {
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(()),
        other => other,
    }
}

/// Take down the completeness marker of EVERY build under `prog_store`
/// (`<prefix>/store/<program>`), durably, before a caller removes the tree.
///
/// The whole-program twin of the unmark that opens [`discard_build`], and it exists for the
/// same reason: [`crate::ops::uninstall`]'s `remove_dir_all` of a program's whole store is
/// the longest-running delete in this manager — for `trust`, several gigabytes across tens
/// of thousands of files — and `readdir` order decides what it removes first. An interrupt
/// inside that walk leaves whichever build dirs it had not reached, each still vouched for
/// by a `<n>.ready` it also had not reached. On m3 that walk had taken `8595/bin` and left
/// `8595/lib` and `8595.ready` (2026-09-17). Unmarking first means an interrupted uninstall
/// leaves trees that read as INCOMPLETE — reclaimable debris that `gc`'s partial arm sweeps
/// and that `list_installed`, `decide` and `rollback` all ignore — rather than installed
/// builds that are not there.
///
/// Best-effort, and deliberately so: it is a narrowing of a window, not a precondition of
/// the removal, and a marker that cannot be unlinked must not stop the uninstall the user
/// asked for. One `sync_dir` at the end, not one per build: the ordering the flush
/// establishes is "every marker gone before any tree goes", which is a claim about the
/// directory, not about any one entry in it.
pub(crate) fn unmark_program_builds(prog_store: &Path) {
    let Ok(entries) = std::fs::read_dir(prog_store) else {
        return;
    };
    let mut unmarked = false;
    for entry in entries.flatten() {
        // Numeric build dirs only, by the same test every other store scan uses: `current`
        // is a symlink, and `<n>.ready` / `<n>.provenance` / `<n>.shim-env` are files.
        if !entry.file_type().is_ok_and(|t| t.is_dir()) {
            continue;
        }
        let name = entry.file_name();
        let Some(name) = name.to_str() else { continue };
        if name.parse::<u64>().is_err() {
            continue;
        }
        if clear_build_ready(&entry.path()).is_ok() {
            unmarked = true;
        }
    }
    if unmarked {
        sync_dir(prog_store);
    }
}

/// The suffix of the stage-refusal memo beside a build dir (`<build>.refused`).
pub const STAGE_REFUSAL_SUFFIX: &str = ".refused";

/// The memo's first line — the schema gate. A file that does not open with it is not
/// read at all, so a future shape can never be half-understood by this reader.
const STAGE_REFUSAL_HEADER: &str = "stage-refusal v1";

/// Bound on a memo read: it holds six short lines.
const MAX_REFUSAL_BYTES: usize = 4 * 1024;

/// How long the SECOND consecutive refusal of a build's signed digests holds the next
/// attempt off the wire — 12 h, two of the GUI's six-hourly ticks — doubling per further
/// refusal up to [`REFUSAL_COOLDOWN_CAP_SECS`]. The FIRST refusal holds nothing at all:
/// see [`StageRefusal::cooldown_secs`].
const REFUSAL_COOLDOWN_BASE_SECS: i64 = 12 * 60 * 60;

/// The ceiling on that doubling: one week. A pin whose SIGNED digests do not match the
/// published asset is a publishing fact that only a new index can fix, and every attempt
/// to re-prove it costs the whole asset.
const REFUSAL_COOLDOWN_CAP_SECS: i64 = 7 * 24 * 60 * 60;

/// The longest `why` sentence a memo keeps — the stage's own words, truncated so the
/// memo cannot grow past a line or two.
const MAX_REFUSAL_WHY_CHARS: usize = 300;

/// What a DETERMINISTIC stage failure recorded beside `store/<program>/<build>`: the
/// SIGNED digests the published bytes did not match, the stage's own sentence, when it
/// was recorded, and how many consecutive attempts have now proved the same thing.
///
/// # Why a memo exists at all
///
/// A signed-`sha256` mismatch says the published bytes are not the bytes the manifest
/// names, and while the pin and its digest hold still, so does that answer. The archive
/// is (rightly) deleted on that failure, and any `.part` with it — the bytes are wrong,
/// and a poisoned prefix must never seed the next attempt — so the NEXT attempt starts
/// from byte 0: every six-hourly tick re-downloaded the whole asset (~3.4 GB for
/// `trust`) to reach the identical verdict, on every machine in the fleet, until the
/// publisher cut a new index. This is the record that stops that, and the ONLY thing it
/// can do is skip a transfer — it is consulted after the signed manifest has been
/// fetched and verified, and it can never admit bytes, relax a digest, or keep a build
/// alive.
///
/// # Why it lapses
///
/// A publisher can repair the asset UNDER the same pin (re-upload the bytes the signed
/// digest always named), and a transfer can simply have been truncated on the wire — a
/// permanent memo would hide both. So the record holds a COOLDOWN, not a verdict, and
/// the first one is ZERO: the next pass downloads again exactly as it always did. From
/// the second identical verdict it bounds the retry to one attempt per 12 h, then 24,
/// 48, 96, up to one a week, instead of four a day — and any change to the pinned
/// build's signed `sha256`/`tree_root` makes it stop binding at once.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StageRefusal {
    /// The SIGNED `sha256` of the artifact this refusal is about.
    pub sha256: String,
    /// The SIGNED `tree_root` of that artifact (empty when the manifest carries none).
    pub tree_root: String,
    /// The stage's own sentence ([`crate::install::StageError`]'s `Display`).
    pub why: String,
    /// When it was recorded (Unix epoch second).
    pub at: i64,
    /// How many consecutive attempts have failed on these digests (>= 1).
    pub attempts: u32,
    /// The platform signer check refused bytes that MATCHED these digests
    /// ([`crate::install::StageError::SignerRefused`]): a re-download can only fetch the
    /// same bytes, so the memo binds until the digests change, with no cooldown to lapse.
    pub signer: bool,
}

impl StageRefusal {
    /// The cooldown this record's attempt count buys — ZERO for a first refusal, then
    /// [`REFUSAL_COOLDOWN_BASE_SECS`] doubled once per further consecutive refusal,
    /// capped at [`REFUSAL_COOLDOWN_CAP_SECS`].
    ///
    /// THE FIRST RETRY IS ALWAYS FREE, and that is the load-bearing half. A digest
    /// mismatch is not proof of a publishing slip: a truncated transfer, a bad mirror or
    /// a meddling proxy produces exactly the same verdict, and those heal on the very
    /// next attempt. So one failure buys nothing — the next pass downloads again, as it
    /// always did, and a publisher who repaired the asset under the same pin is found
    /// there (`flow`'s `a_member_stage_failure_aborts_the_group_and_a_retry_heals_it`
    /// pins that healing). Only a SECOND failure over the identical signed digests — two
    /// independent downloads, two identical verdicts — is evidence about the publication
    /// rather than about the wire, and that is where holding off starts to pay.
    #[must_use]
    pub fn cooldown_secs(&self) -> i64 {
        let Some(steps) = self.attempts.checked_sub(2) else {
            return 0;
        };
        REFUSAL_COOLDOWN_BASE_SECS
            .saturating_mul(1_i64 << steps.min(8))
            .min(REFUSAL_COOLDOWN_CAP_SECS)
    }

    /// The epoch second at which a new attempt is due.
    #[must_use]
    pub fn retry_after(&self) -> i64 {
        self.at.saturating_add(self.cooldown_secs())
    }

    /// Whether this record still stands in the way of re-fetching an artifact with these
    /// SIGNED digests at `now_unix`. Fail-OPEN by construction: a memo with no `sha256`,
    /// one whose digests differ from the pin's (the publisher moved them — the repair
    /// this memo must not hide), or one whose cooldown has lapsed binds nothing, and a
    /// clock that cannot be read (`i64::MAX`) makes every memo look lapsed. The only
    /// thing a memo can do is skip a download that would fail again.
    ///
    /// A [`StageRefusal::signer`] memo has no cooldown: it binds while the digests hold.
    #[must_use]
    pub fn binds(&self, sha256: &str, tree_root: &str, now_unix: i64) -> bool {
        !self.sha256.is_empty()
            && self.sha256.eq_ignore_ascii_case(sha256)
            && self.tree_root.eq_ignore_ascii_case(tree_root)
            && (self.signer || now_unix < self.retry_after())
    }
}

/// `store/<program>/<build>.refused` for `build_dir` — a SIBLING, like `<build>.ready`
/// and `<build>.shim-env`, so it never perturbs the build's `tree_root` and can outlive a
/// build directory that never came to exist (which is the point: the stage it records
/// FAILED).
fn stage_refusal_path(build_dir: &Path) -> Option<PathBuf> {
    let name = crate::call1(std::path::Path::file_name, build_dir)?;
    let name = crate::call1(std::ffi::OsStr::to_str, name)?;
    let mut marker = String::from(name);
    marker.push_str(STAGE_REFUSAL_SUFFIX);
    Some(build_dir.with_file_name(marker))
}

/// Record that staging `build_dir` failed on the SIGNED digests (temp + rename, so a
/// crash leaves no half-written memo). A record already naming these digests has its
/// attempt count ADVANCED — that is what lengthens the cooldown; a record naming other
/// digests is replaced, starting the count again.
///
/// At most ONE memo survives per program: writing one reclaims every other `*.refused`
/// beside it — the same bound `staging/` keeps for partials — so a program whose pin
/// moves repeatedly leaves one small file, never one per build it refused.
///
/// # Errors
/// The memo could not be written (its parent — `store/<program>/` — cannot be created,
/// or the volume refuses).
pub fn record_stage_refusal(
    build_dir: &Path,
    sha256: &str,
    tree_root: &str,
    why: &str,
    now_unix: i64,
) -> std::io::Result<()> {
    write_stage_refusal(build_dir, sha256, tree_root, why, now_unix, false)
}

/// [`record_stage_refusal`] for a platform signer refusal ([`StageRefusal::signer`]).
///
/// # Errors
/// As [`record_stage_refusal`].
pub(crate) fn record_signer_refusal(
    build_dir: &Path,
    sha256: &str,
    tree_root: &str,
    why: &str,
    now_unix: i64,
) -> std::io::Result<()> {
    write_stage_refusal(build_dir, sha256, tree_root, why, now_unix, true)
}

fn write_stage_refusal(
    build_dir: &Path,
    sha256: &str,
    tree_root: &str,
    why: &str,
    now_unix: i64,
    signer: bool,
) -> std::io::Result<()> {
    let dest = stage_refusal_path(build_dir).ok_or_else(|| {
        std::io::Error::new(std::io::ErrorKind::InvalidInput, "build dir has no name")
    })?;
    let attempts = stage_refusal(build_dir)
        .filter(|prior| {
            prior.sha256.eq_ignore_ascii_case(sha256)
                && prior.tree_root.eq_ignore_ascii_case(tree_root)
        })
        .map_or(1, |prior| prior.attempts.saturating_add(1));
    let parent = dest.parent().unwrap_or(build_dir);
    std::fs::create_dir_all(parent)?;
    // Manual (byte-identical) render, no `format!`: Trust-gate lowering workaround —
    // see `lib.rs::dec_u64`.
    let mut tmp_name = String::from(".refused.tmp-");
    tmp_name.push_str(&crate::dec_u64(u64::from(std::process::id())));
    let tmp = parent.join(tmp_name);
    let mut body = String::from(STAGE_REFUSAL_HEADER);
    body.push_str("\nsha256=");
    body.push_str(sha256);
    body.push_str("\ntree_root=");
    body.push_str(tree_root);
    body.push_str("\nattempts=");
    body.push_str(&crate::dec_u64(u64::from(attempts)));
    body.push_str("\nat=");
    body.push_str(&crate::dec_u64(u64::try_from(now_unix).unwrap_or(0)));
    // Only when set: a reader that predates it sees a digest memo, which fails open.
    if signer {
        body.push_str("\nsigner=1");
    }
    // One line: the reader takes the `why=` line whole.
    body.push_str("\nwhy=");
    let one_line: String = why
        .replace(['\n', '\r'], " ")
        .chars()
        .take(MAX_REFUSAL_WHY_CHARS)
        .collect();
    body.push_str(&one_line);
    body.push('\n');
    crate::call2(std::fs::write, &tmp, body.as_bytes())?;
    if let Err(e) = std::fs::rename(&tmp, &dest) {
        let _ = std::fs::remove_file(&tmp);
        return Err(e);
    }
    if let Some(name) = dest.file_name() {
        sweep_other_refusals(parent, name);
    }
    Ok(())
}

/// Reclaim every `*.refused` in `dir` except `keep` — the one-memo-per-program bound.
fn sweep_other_refusals(dir: &Path, keep: &OsStr) {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return;
    };
    for entry in entries.flatten() {
        let name = entry.file_name();
        if name == keep || !entry.file_type().is_ok_and(|t| t.is_file()) {
            continue;
        }
        if name
            .to_str()
            .is_some_and(|n| n.ends_with(STAGE_REFUSAL_SUFFIX))
        {
            let _ = std::fs::remove_file(entry.path());
        }
    }
}

/// The refusal recorded beside `build_dir`, if one is there and reads. `None` is "no
/// usable record" — a missing, unreadable, oversized, symlinked or malformed memo says
/// nothing, so the caller downloads, which is the behaviour without this file at all.
#[must_use]
pub fn stage_refusal(build_dir: &Path) -> Option<StageRefusal> {
    let path = stage_refusal_path(build_dir)?;
    let text = crate::metadata_io::read_bounded_regular_utf8(&path, MAX_REFUSAL_BYTES).ok()?;
    parse_stage_refusal(&text)
}

/// [`stage_refusal`]'s parser, split out so the schema gate and every malformed shape are
/// testable without a filesystem. Fail-closed to `None`.
fn parse_stage_refusal(text: &str) -> Option<StageRefusal> {
    let mut lines = text.lines();
    if lines.next()?.trim() != STAGE_REFUSAL_HEADER {
        return None;
    }
    let (mut sha256, mut tree_root, mut why) = (None, None, None);
    let (mut at, mut attempts, mut signer) = (None, None, false);
    for line in lines {
        if let Some(v) = line.strip_prefix("sha256=") {
            sha256 = Some(v.to_string());
        } else if let Some(v) = line.strip_prefix("tree_root=") {
            tree_root = Some(v.to_string());
        } else if let Some(v) = line.strip_prefix("why=") {
            why = Some(v.to_string());
        } else if let Some(v) = line.strip_prefix("at=") {
            at = v.trim().parse::<i64>().ok();
        } else if let Some(v) = line.strip_prefix("attempts=") {
            attempts = v.trim().parse::<u32>().ok().filter(|n| *n >= 1);
        } else if let Some(v) = line.strip_prefix("signer=") {
            signer = v.trim() == "1";
        }
    }
    Some(StageRefusal {
        sha256: sha256?,
        tree_root: tree_root?,
        why: why.unwrap_or_default(),
        at: at?,
        attempts: attempts?,
        signer,
    })
}

/// Remove the refusal beside `build_dir`, if any — a stage of this build that SUCCEEDED,
/// every discard, and the explicit install door, so the memo never outlives its cause.
pub fn clear_stage_refusal(build_dir: &Path) {
    if let Some(marker) = stage_refusal_path(build_dir) {
        let _ = std::fs::remove_file(marker);
    }
}

/// The suffix of a vendor-direct build's record (`store/<program>/<build>.vendor`): what
/// was verified, against which anchor, written by the stage before `.ready`.
pub(crate) const VENDOR_SIDECAR_SUFFIX: &str = ".vendor";

/// The sidecars a stage may write through [`crate::install::StageHooks::sidecars`]:
/// exactly those [`discard_build`] removes and `gc` sweeps once their build is gone, so a
/// sidecar can never outlive its tree.
pub(crate) const STAGE_SIDECAR_SUFFIXES: &[&str] =
    &[VENDOR_SIDECAR_SUFFIX, crate::shim_env::SIDECAR_SUFFIX];

/// `<build><suffix>` beside `build_dir`, or `None` for a path with no UTF-8 file name.
pub(crate) fn sidecar_path(build_dir: &Path, suffix: &str) -> Option<PathBuf> {
    let name = crate::call1(std::path::Path::file_name, build_dir)?;
    let name = crate::call1(std::ffi::OsStr::to_str, name)?;
    let mut sidecar = String::from(name);
    sidecar.push_str(suffix);
    Some(build_dir.with_file_name(sidecar))
}

/// Write `<build><suffix>` durably: temp + fsync + rename, mode `0644` like `.ready` so a
/// system prefix's readers can read it. The caller flushes the directory afterwards.
///
/// # Errors
/// The temp file cannot be written or flushed, or the rename fails; no temp is left.
pub(crate) fn write_sidecar_durably(
    build_dir: &Path,
    suffix: &str,
    bytes: &[u8],
) -> std::io::Result<()> {
    let dest = sidecar_path(build_dir, suffix).ok_or_else(|| {
        std::io::Error::new(std::io::ErrorKind::InvalidInput, "build dir has no name")
    })?;
    let parent = dest.parent().unwrap_or(build_dir);
    let mut tmp_name = String::from(suffix);
    tmp_name.push_str(".tmp-");
    tmp_name.push_str(&crate::dec_u64(u64::from(std::process::id())));
    let tmp = parent.join(tmp_name);
    let written = (|| {
        use std::io::Write as _;
        let mut f = crate::platform::open_create_write(&tmp, 0o644)?;
        f.write_all(bytes)?;
        sync_contents_or_accept_refusal(&f)?;
        drop(f);
        std::fs::rename(&tmp, &dest)
    })();
    if written.is_err() {
        let _ = std::fs::remove_file(&tmp);
    }
    written
}

/// Remove the vendor record beside `build_dir`, if any.
pub(crate) fn clear_vendor_sidecar(build_dir: &Path) {
    if let Some(sidecar) = sidecar_path(build_dir, VENDOR_SIDECAR_SUFFIX) {
        let _ = std::fs::remove_file(sidecar);
    }
}

/// Forget every refusal recorded for `program` — what the EXPLICIT door
/// (`aterm pkg install <program>`) does before it installs, so a person who has just
/// fixed the publish (or the proxy that corrupted the transfer) never waits out a
/// cooldown meant for an unattended six-hourly loop.
pub fn clear_stage_refusals(layout: &Layout, program: &str) {
    let dir = layout.prefix.join("store").join(program);
    let Ok(entries) = std::fs::read_dir(&dir) else {
        return;
    };
    for entry in entries.flatten() {
        if !entry.file_type().is_ok_and(|t| t.is_file()) {
            continue;
        }
        if entry
            .file_name()
            .to_str()
            .is_some_and(|n| n.ends_with(STAGE_REFUSAL_SUFFIX))
        {
            let _ = std::fs::remove_file(entry.path());
        }
    }
}

/// The sibling scratch directory a stage extracts INTO before it swaps: `<build>.incoming-<pid>`.
///
/// Staging never writes into the live `<build>/` tree. The old flow deleted the live tree
/// and extracted over it, so from the first byte of extraction until the final marker
/// write the installed toolchain simply did not exist — a Ctrl-C, a crash, or a
/// disk-full there left the user with no compiler AND (because the marker is a SIBLING
/// that the delete did not touch) a store that still claimed the build was installed.
pub(crate) fn incoming_dir(build_dir: &Path) -> Option<PathBuf> {
    scratch_sibling(build_dir, ".incoming-")
}

/// The sibling the OUTGOING tree is moved to during a swap: `<build>.superseded-<pid>`.
/// It exists only between the two renames, and is deleted immediately after.
pub(crate) fn superseded_dir(build_dir: &Path) -> Option<PathBuf> {
    scratch_sibling(build_dir, ".superseded-")
}

/// `<build><suffix><pid>` beside `build_dir`. The pid keeps two stagers of the same build
/// apart even though the store lock already serializes them — a scratch name that
/// collides is a scratch name that can be deleted out from under its owner.
fn scratch_sibling(build_dir: &Path, suffix: &str) -> Option<PathBuf> {
    let name = crate::call1(std::path::Path::file_name, build_dir)?;
    let name = crate::call1(std::ffi::OsStr::to_str, name)?;
    // Manual concat (no `format!`): Trust-gate lowering workaround — see `lib.rs::dec_u64`.
    let mut scratch = String::from(name);
    scratch.push_str(suffix);
    scratch.push_str(&crate::dec_u64(u64::from(std::process::id())));
    Some(build_dir.with_file_name(scratch))
}

/// Which of the two scratch shapes a stage produced.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Scratch {
    /// `<build>.incoming-<pid>` — the tree being extracted, before it has been verified.
    Incoming,
    /// `<build>.superseded-<pid>` — the OUTGOING tree, parked between the two swap renames.
    Superseded,
}

/// A store build directory's name, parsed the way this manager writes one: a non-empty run
/// of ASCII digits, no leading zero unless the name is exactly `0` ([`crate::dec_u64`]
/// renders nothing else). `u64::from_str` is looser — `+18` and `018` read back as 18 — and
/// these names authorize `remove_dir_all` inside `store/<program>/`, a directory the user
/// can also put things in.
pub(crate) fn parse_build_name(name: &str) -> Option<u64> {
    if name.is_empty() || !name.bytes().all(|b| b.is_ascii_digit()) {
        return None;
    }
    if name.len() > 1 && name.starts_with('0') {
        return None;
    }
    name.parse::<u64>().ok()
}

/// Recognise stage scratch by the PRODUCER's exact shape: `<build>.incoming-<pid>` or
/// `<build>.superseded-<pid>`, where `<build>` is a real build number and `<pid>` a
/// non-empty run of ASCII digits. Returns the build number and the shape.
///
/// This is the SINGLE recogniser both sweepers use ([`sweep_stage_scratch`] here and
/// [`crate::gc`]'s pass), because they authorize the same unguarded `remove_dir_all` inside
/// a directory the user can also put things in, and a policy that only one half enforces is
/// not a policy: `18.incoming-drafts/` is not ours to delete, whichever code path meets it.
pub(crate) fn stage_scratch_of(name: &str) -> Option<(u64, Scratch)> {
    let (build, rest) = name.split_once('.')?;
    let build = parse_build_name(build)?;
    let (kind, pid) = match rest.strip_prefix("incoming-") {
        Some(pid) => (Scratch::Incoming, pid),
        None => (Scratch::Superseded, rest.strip_prefix("superseded-")?),
    };
    if pid.is_empty() || !pid.bytes().all(|b| b.is_ascii_digit()) {
        return None;
    }
    Some((build, kind))
}

/// Every stage-scratch sibling of `build_dir`, as `(kind, path)`. Empty when `build_dir` is
/// not a numeric build under a readable parent.
fn scratch_siblings(build_dir: &Path) -> Vec<(Scratch, PathBuf)> {
    let (Some(parent), Some(name)) = (
        build_dir.parent(),
        build_dir.file_name().and_then(|n| n.to_str()),
    ) else {
        return Vec::new();
    };
    let Some(build) = parse_build_name(name) else {
        return Vec::new();
    };
    let Ok(entries) = std::fs::read_dir(parent) else {
        return Vec::new();
    };
    entries
        .flatten()
        .filter_map(|entry| {
            let entry_name = entry.file_name();
            let (found, kind) = stage_scratch_of(entry_name.to_str()?)?;
            // Siblings of a DIFFERENT build are that build's business, not ours.
            (found == build).then(|| (kind, entry.path()))
        })
        .collect()
}

/// Recover the one crash window a swap cannot make atomic. `true` when a tree was moved back.
///
/// [`crate::install::verify_and_stage`]'s swap moves the outgoing tree to
/// `<build>.superseded-<pid>` and then moves the verified incoming tree onto `<build>`.
/// Between those two renames the ONLY copy of the old tree lives under the scratch name. A
/// SIGKILL, a `^C`, or a power loss there leaves `<build>` absent with exactly one
/// superseded sibling — and routine housekeeping used to `remove_dir_all` that sibling,
/// turning a recoverable crash into a permanently deleted toolchain. This is the only place
/// a plain crash, with no I/O error anywhere, could leave neither the old build nor the new.
///
/// The recovery is deliberately NARROW, because a wrong move here is as destructive as the
/// delete it replaces. It fires only when `<build>` is absent AND exactly one superseded
/// sibling is present: that is the swap window and nothing else. With `<build>` present the
/// sibling is genuine leftover (the swap got past its second rename, or its rollback put the
/// tree back) and must still be swept; with two siblings there is no way to tell which is
/// the real outgoing tree, and guessing is exactly how live trees get deleted.
///
/// It does NOT re-mark the recovered tree. The swap clears the marker before the first
/// rename, so whether that tree was complete is unrecoverable from disk — leaving it
/// unmarked means it reads as not-installed and is re-staged, which is honest, whereas
/// re-marking would promote a tree nothing can vouch for. The re-stage is the next apply
/// pass's, shims or no shims: `flow::installed_for_decide` drops an unmarked live build
/// before [`crate::gate::decide`] sees it, so a recovered tree the shims still resolve into
/// is repaired rather than read as up to date.
pub(crate) fn recover_interrupted_swap(build_dir: &Path) -> bool {
    // `symlink_metadata`, not `exists()`: a DANGLING symlink at the build path is still
    // something being there, and "the window" means nothing at all is. Narrower is safer.
    if std::fs::symlink_metadata(build_dir).is_ok() {
        return false;
    }
    let mut superseded = scratch_siblings(build_dir).into_iter().filter(|(kind, p)| {
        // A real directory, not a symlink to one: the outgoing tree the swap parked here is
        // a directory, and anything else is not the state this recovers.
        *kind == Scratch::Superseded && std::fs::symlink_metadata(p).is_ok_and(|m| m.is_dir())
    });
    let Some((_, old)) = superseded.next() else {
        return false;
    };
    if superseded.next().is_some() {
        return false; // ambiguous — refuse rather than guess
    }
    std::fs::rename(&old, build_dir).is_ok()
}

/// Delete every stage-scratch sibling of `build_dir` left behind by an earlier run, after
/// recovering the swap window ([`recover_interrupted_swap`]) so a crash there does not get
/// swept as debris. The caller holds the store lock, which every stager takes.
///
/// Nothing else reclaims this scratch — GC only sees numeric, marker-bearing dirs — and a
/// non-directory at a scratch path is reclaimed by neither sweeper while blocking every
/// later swap of that build. A superseded *directory* is spared while `<build>` is absent
/// and the recovery above declined to move it: it is the only copy of the build on disk.
pub(crate) fn sweep_stage_scratch(build_dir: &Path) {
    let recovered = recover_interrupted_swap(build_dir);
    // Not debris: while nothing stands at `<build>`, a superseded sibling is the only copy
    // of that build on disk — a swap window [`recover_interrupted_swap`] could not close.
    // Still swept: a sibling of a live `<build>`, `.incoming-<pid>` half-extracts, and the
    // sibling itself once `<build>` is whole again. `crate::gc` holds this same guard and
    // reports an ambiguous pair as `doctor`'s `diverged` line rather than reclaiming one.
    let parked_only_copy = !recovered && std::fs::symlink_metadata(build_dir).is_err();
    for (kind, path) in scratch_siblings(build_dir) {
        // A directory, the predicate [`recover_interrupted_swap`] filters on: a swap parks a
        // tree. A file or symlink at that name is nobody's only copy, and the one entry
        // neither sweeper can otherwise reclaim — so it must still be swept.
        if parked_only_copy
            && kind == Scratch::Superseded
            && std::fs::symlink_metadata(&path).is_ok_and(|m| m.is_dir())
        {
            continue;
        }
        // `symlink_metadata`, not `is_dir()`: `remove_dir_all` refuses a SYMLINK to a
        // directory (it does not follow it — proven by the sweep tests), so dispatching on
        // the followed type would leave exactly that entry behind forever.
        match std::fs::symlink_metadata(&path) {
            Ok(m) if m.is_dir() => {
                let _ = std::fs::remove_dir_all(&path);
            }
            Ok(_) => {
                let _ = std::fs::remove_file(&path);
            }
            Err(_) => {}
        }
    }
}

/// Flush a directory's metadata so the renames that make up a swap are durable in the
/// order they were issued. Best-effort: a platform that refuses to open a directory (or an
/// exotic filesystem) is not a reason to fail an otherwise-good install, and the swap is
/// crash-CORRECT either way — this only narrows the window in which a power loss can
/// reorder the marker ahead of the tree.
pub(crate) fn sync_dir(dir: &Path) {
    if let Ok(handle) = std::fs::File::open(dir) {
        let _ = handle.sync_all();
    }
}

/// [`crate::platform::sync_file_contents`], with a REFUSAL degraded to success.
///
/// The rule the updater's boot sentinel already settled: some volumes a store can live on
/// (a network home, some FUSE mounts) answer a flush `ENOTSUP`/`EINVAL`, and failing an
/// install there would trade a durability guarantee the volume cannot give for a
/// toolchain the user cannot have. A REAL error (`EIO`, `ENOSPC`) still propagates —
/// those are the answers that say the bytes are not on disk, which is the whole question
/// being asked.
pub(crate) fn sync_contents_or_accept_refusal(f: &std::fs::File) -> std::io::Result<()> {
    match crate::platform::sync_file_contents(f) {
        Ok(()) => Ok(()),
        Err(e)
            if matches!(
                e.kind(),
                std::io::ErrorKind::Unsupported | std::io::ErrorKind::InvalidInput
            ) =>
        {
            Ok(())
        }
        Err(e) => Err(e),
    }
}

/// What [`sync_tree`] flushed. The counts exist so a test can prove the walk reached
/// EVERY file and directory of a staged tree rather than some arbitrary subset of it —
/// there is no other observable the flush leaves behind.
#[derive(Debug, Default)]
pub(crate) struct Synced {
    /// Regular files whose contents were pushed to the filesystem.
    pub files: u64,
    /// Directories whose entries were pushed to the filesystem.
    pub dirs: u64,
}

/// Flush a STAGED TREE — every regular file's contents, then every directory's entries —
/// so that nothing which PUBLISHES the tree can become durable ahead of the bytes it
/// publishes. Call between the last verification and the swap.
///
/// **The hazard, concretely.** Staging writes gigabytes through a plain write loop and
/// then publishes them with renames. A rename is metadata; the data behind it is page
/// cache until something flushes it. Under ext4's default delayed allocation — whose
/// `auto_da_alloc` heuristic covers a rename OVER AN EXISTING FILE, which neither the
/// swap nor the marker is — the journal can commit `store/<program>/<build>/` and its
/// sibling `<build>.ready` while the payload is still unwritten. After a power loss the
/// store then says "build N is complete" over files that are zero-length or truncated,
/// and NOTHING repairs that state: [`crate::gate::decide`] answers `UpToDate`, the apply
/// path never re-fetches, and the superseded tree was already reclaimed by the swap that
/// installed this one. Only a hand-run `atpkg verify` would ever walk the tree again.
/// [`sync_dir`] orders the RENAMES against each other; it never made the data durable.
///
/// **The cost, deliberately bounded.** One `fsync(2)` per file, never
/// `fcntl(F_FULLFSYNC)` — see [`crate::platform::sync_file_contents`]. The work is
/// dominated by writeback the install already owes the filesystem; what this gives up is
/// batching it, and what it buys is that no crash can mark a build installed over data
/// that never landed.
///
/// Symlinks are never followed and never opened: a link has no contents of its own, and
/// the directory entry naming it is flushed with its directory. A file this process
/// cannot OPEN is one it cannot flush — the `dmg` lane copies a vendor bundle with its
/// modes preserved, so an unreadable member is possible, and failing a whole install over
/// one would be strictly worse than the gap this closes.
pub(crate) fn sync_tree(root: &Path) -> std::io::Result<Synced> {
    let mut counts = Synced::default();
    let mut stack = vec![root.to_path_buf()];
    while let Some(dir) = stack.pop() {
        for child in std::fs::read_dir(&dir)? {
            let path = child?.path();
            // `symlink_metadata`: a link is never followed (the staged tree admits in-root
            // links, and following one could leave the tree or walk a cycle).
            let ft = std::fs::symlink_metadata(&path)?.file_type();
            if ft.is_dir() {
                stack.push(path);
            } else if ft.is_file() {
                match std::fs::File::open(&path) {
                    Ok(f) => {
                        sync_contents_or_accept_refusal(&f)?;
                        counts.files = counts.files.saturating_add(1);
                    }
                    Err(e) if e.kind() == std::io::ErrorKind::PermissionDenied => {}
                    Err(e) => return Err(e),
                }
            }
        }
        // The directory's own entries, AFTER the contents they name. Best-effort on the
        // open alone: Windows cannot open a directory as a file at all, and that platform
        // has no store to strand.
        if let Ok(handle) = std::fs::File::open(&dir) {
            sync_contents_or_accept_refusal(&handle)?;
            counts.dirs = counts.dirs.saturating_add(1);
        }
    }
    Ok(counts)
}

/// Discard a build entirely: remove its tree AND its sibling completeness marker (the
/// inverse of a stage + [`mark_build_ready`]), its sidecars, and the exec root
/// [`crate::compat`] laid for it. Used to clean up a build that a transaction
/// STAGED but then ABORTED without activating — leaving it complete-but-inactive would make
/// `list_installed`/`decide` mis-read it as the active build on the next run. Best-effort.
///
/// **`pub(crate)` deliberately.** This is an unconditional `remove_dir_all` with no notion of
/// which build is live: handed to a caller that got its build number from a `read_dir` fold,
/// it will happily delete the running toolchain — which is precisely how GC came to brick a
/// prefix. Reclaim therefore goes through [`crate::gc::discard_superseded`], which demands a
/// [`crate::gc::LiveBuild`] witness and refuses to delete it; making the raw form
/// crate-private is what stops that call from being *writable* elsewhere. The remaining
/// in-crate caller ([`crate::flow`]) is the staged-but-ABORTED path, where the build was
/// never activated and so no witness can name it.
///
/// ONE caller IS a `read_dir` fold, and it is the exception that states the rule:
/// [`crate::gc::run`]'s interrupted-install sweep. A witness is impossible there — an
/// interrupted FRESH install has no live build for one to be about — so it is guarded on
/// the CLAIM UNION instead: every authoritative `current` link plus every `bin/` shim
/// target. That is strictly stronger evidence of not-live than supersession, because it
/// deletes only trees that NOTHING on disk points into, which is what earns it the right
/// to name a build number it read out of a directory listing.
pub(crate) fn discard_build(build_dir: &Path) {
    // THE MARKER COMES DOWN FIRST, AND DURABLY — before the exec root, before the tree.
    //
    // This used to be the LAST act, after `remove_dir_all`, and that ordering is how a
    // corpse is made: `remove_dir_all` walks a multi-gigabyte toolchain for tens of
    // seconds, and anything that ends the process inside that window — ^C, a SIGTERM at
    // logout, the machine going down, an EACCES that aborts the walk partway — leaves a
    // gutted tree with a marker beside it still saying `ok`. Measured on m3, 2026-09-17:
    // `store/trust/8595/` with no `bin/` at all, 417 MB of orphaned `lib/`, and
    // `8595.ready` intact. `list_installed` counted it as installed, `gc` would not sweep
    // it (it sweeps only marker-LESS trees), and `flow::rollback` selects the highest
    // retained build below current — that one — whose `bin/` no longer holds a single tool,
    // so `rollback_member`'s "the prior build lacks this tool" arm would have REMOVED every
    // shim on the machine.
    //
    // Unmarking first makes the whole window safe instead of merely narrower: from the
    // instant the marker is gone the tree reads as incomplete to every reader
    // ([`build_is_complete`]), which is the truth for the rest of this function and the
    // truth for any interrupt inside it — and `gc`'s partial arm reclaims exactly such a
    // tree on the next pass, under its claim guard. The `sync_dir` is what makes the
    // ordering survive a power loss and not merely a kill: an unlink is metadata, and
    // without a directory flush the marker's removal may reach the platter AFTER the
    // tree's, which is the ordering this fix exists to forbid.
    let _ = clear_build_ready(build_dir);
    if let Some(parent) = build_dir.parent() {
        sync_dir(parent);
    }
    // The build's EXEC ROOT (`<prefix>/compat/<program>/<n>`, `crate::compat`) next: it is
    // a copy-on-write clone of this build's files, so left behind it would keep every
    // reclaimed block allocated and could still run the tools of a build the store no
    // longer has. Derived from this path's own `store/<program>/<n>` chain only.
    crate::compat::discard_root_of(build_dir);
    let _ = std::fs::remove_dir_all(build_dir);
    // And the marker again, for the one case the unmark above could not settle: it is
    // best-effort, so a failure there (a read-only parent that a later act made writable,
    // an EIO) must not leave the sibling behind once the tree really is gone.
    if let Some(marker) = ready_marker_path(build_dir) {
        let _ = std::fs::remove_file(marker);
    }
    // Also remove the source-build provenance sidecar (a sibling `<build>.provenance`), so a
    // later SIGNED reinstall reusing this build number can never be mis-verified as
    // source-built by a stale sidecar. Mirrors the `.ready` sibling naming.
    if let Some(name) = build_dir.file_name().and_then(|n| n.to_str()) {
        let mut prov = String::from(name);
        prov.push_str(".provenance");
        let _ = std::fs::remove_file(build_dir.with_file_name(prov));
    }
    // And the shim-environment sidecar (`<build>.shim-env`, design S7): a later
    // reinstall under this build number writes its own from its own signed manifest,
    // and must never re-lay shims with an environment a discarded build declared.
    crate::shim_env::remove_sidecar(build_dir);
    // And the digest-refusal memo (`<build>.refused`): it is a statement about the bytes
    // a pin named, and a later reinstall under this build number must start from the
    // signed manifest, never from a verdict about a tree that is gone.
    clear_stage_refusal(build_dir);
    // And the vendor record (`<build>.vendor`): it vouches for a verified tree, and the
    // tree is gone.
    clear_vendor_sidecar(build_dir);
}

/// The default prefix under `home`. On macOS `…/Library/Application Support/aterm/pkg`
/// (a sibling of the updater's `Updates` dir, sharing the hardened support root); on
/// other Unix `…/.local/share/aterm/pkg`; on Windows `%LOCALAPPDATA%\aterm\pkg`. The
/// OS-specific base lives in [`crate::platform::default_prefix`].
#[must_use]
pub fn default_prefix(home: &Path) -> PathBuf {
    crate::platform::default_prefix(home)
}

/// Resolve the install layout. `configured` is the optional `[packages].prefix` override
/// (`None` ⇒ the default). The chosen prefix is **chain-validated** against the home dir
/// ([`vet_prefix`]); any violation falls back to the default. Returns `None` only when the
/// home directory can't be resolved (`$HOME` / `/etc/passwd` on Unix, `%USERPROFILE%` on
/// Windows) — the same fail-closed posture the updater takes. Uses the platform-aware
/// [`aterm_types::dirs::home_dir`], NOT a raw `$HOME` read: a native-Windows shell does not
/// set `HOME`, so a raw read left every prefix-dependent verb dead with "HOME is unset".
#[must_use]
pub fn resolve(configured: Option<&Path>) -> Option<Layout> {
    let home = aterm_types::dirs::home_dir()?;
    let prefix = vet_prefix(configured, &home);
    // A REJECTED prefix falls back to the default — correct in production (a typo in
    // `[packages].prefix` must not brick the store) and catastrophic under test, where
    // the default prefix is the developer's OWN live store. That is not theoretical: a
    // test here once aimed at the process temp dir, which `vet_prefix` refuses (not
    // under $HOME), and so wrote `trust` into the real machine's removed ledger —
    // uninstalling that machine's compiler and, through the coherence rule, its whole
    // `rustc` tuple. It went unnoticed for eight days because `doctor` had no
    // completeness check to notice it with.
    //
    // The test that caused it was fixed by hand-rolling this assertion at its call site.
    // It belongs HERE instead: one test remembering to check is a fix for one test,
    // while the next author of the next test inherits the same trap. Costs nothing in a
    // release build.
    #[cfg(test)]
    if let Some(asked) = configured {
        assert_eq!(
            prefix,
            asked.to_path_buf(),
            "vet_prefix rejected the prefix this test asked for and fell back to the \
             DEFAULT — which under test is the developer's real store. Give the test a \
             prefix the vet admits: strictly under $HOME, absolute, no `..`, and a real \
             0700 directory (see `a_removed_program_is_read_from_the_layout_by_every_lane`)"
        );
    }
    Some(Layout { prefix })
}

/// Resolve the layout THE USER CONFIGURED — `[packages].prefix` when set, the
/// default otherwise.
///
/// The one edge every caller outside `atpkg`'s own CLI should use. Both of the
/// others hardcoded `resolve(None)`, so on a machine with a relocated or shared lab
/// store the `aterm <tool>` front door reported the ten programs as an unknown
/// aterm option, and Settings ▸ Packages — the page every seed notice points at —
/// reported a fully installed toolset as "No package activity yet". The store was
/// correct the whole time; only the two readers were looking somewhere else
/// (2026-08-20 round-8 audit).
#[must_use]
pub fn resolve_configured() -> Option<Layout> {
    resolve_from(&crate::config::load())
}

/// [`resolve_configured`] over a `[packages]` table already read. The terminal session
/// reads `aterm.toml` ONCE ([`crate::config::cached`]) and resolves from that: re-reading
/// it per consumer repeated every complaint about the file once per read. The window keeps
/// [`resolve_configured`]: it lives for days, and re-reads so an edited prefix is seen.
#[must_use]
pub fn resolve_from(cfg: &crate::config::PackagesConfig) -> Option<Layout> {
    resolve(
        cfg.prefix_path(aterm_types::dirs::home_dir().as_deref())
            .as_deref(),
    )
}

/// Validate a configured prefix against `home`, returning it if safe or the trusted
/// [`default_prefix`] otherwise. Pure w.r.t. config but reads directory metadata; `home`
/// is a parameter so the chain check is testable against a synthetic tree.
#[must_use]
pub fn vet_prefix(configured: Option<&Path>, home: &Path) -> PathBuf {
    let default = default_prefix(home);
    let Some(p) = configured else {
        return default;
    };
    // No `..` escape components, ever, in either shape.
    if !p.is_absolute() || p.components().any(|c| matches!(c, Component::ParentDir)) {
        return default;
    }
    // SYSTEM PREFIX — the second trusted shape. A prefix OUTSIDE $HOME is admissible
    // only when every existing component from `/` down is root-owned and not
    // group/other-writable. That answers the same question the $HOME chain answers
    // (no attacker-writable ancestor can swap a component) at least as strongly.
    // Installing here needs root; that is the point, and it is checked rather than
    // assumed. It is for a genuinely shared multi-user store, NOT a precondition of
    // the verified lane — Trust's default `CallerOwned` mode admits a component owned
    // by the invoking identity, so the $HOME shape proves fine (see the module docs).
    if !under_home(p, home) {
        if !system_chain_trusted(p) {
            return default;
        }
        // TRUSTED IS NOT THE SAME AS USABLE. The chain check above proves no
        // attacker-writable ancestor can swap a component; it says nothing about
        // whether the CALLER can write here. A non-root user with a root-owned
        // prefix configured used to pass this check and then fail on `store.lock`
        // for every verb, forever — while the GUI's launch install reported the
        // failure only as a WARN in a log file. That combination left a machine
        // with no toolchain for three weeks, with a one-line stale config as the
        // whole cause (docs/AUDIT-nux-first-open-toolchain-2026-08-31.md).
        //
        // Falling back is the posture this function already takes for every other
        // violation, and it is the safe direction: the default prefix is
        // `$HOME`-owned and needs no privilege, so a config that would otherwise
        // brick the install degrades to a working one. Say so out loud — silence
        // is what made the original failure survive — as an unasked notice: a typed
        // verb's stderr, a host's log ([`crate::notice`]) — once per process, since every
        // layout resolution vets the prefix again.
        if !caller_can_use_prefix(p) {
            crate::notice::say_once(&format!(
                "configured prefix {} is not writable by this user — using the default {} \
                 instead (set [packages].prefix only for a shared multi-user store this user \
                 can write)",
                p.display(),
                default.display()
            ));
            return default;
        }
        return p.to_path_buf();
    }
    // HOME PREFIX — must be strictly under $HOME. Containment is compared with
    // `under_home` (case-insensitively on Windows, whose filesystem is
    // case-insensitive — else a validly-configured prefix that differs only in case
    // from %USERPROFILE% is wrongly rejected and silently ignored).
    if p == home {
        return default;
    }
    // Walk every EXISTING directory from `home` down to the leaf (the not-yet-created
    // tail that atpkg will make `0700` is fine). Each must be a real dir (NOT a symlink OR a
    // Windows junction), owned by us, and not group/other-writable — else fall back
    // fail-closed. (On Windows `dir_meta_is_private` is a best-effort `true`; privacy rests
    // on the per-user ACL.)
    for anc in p.ancestors().filter(|a| under_home(a, home)) {
        // A non-existent ancestor is the not-yet-created tail atpkg makes 0700 — skip it.
        // `is_reparse` disqualifies a symlink OR a directory junction: a junction (needs no
        // admin) reports is_symlink()==false, so without the reparse-bit check an attacker-
        // pre-created junction ancestor would reintroduce the CWE-379 reparse-swap window.
        if let Ok(m) = std::fs::symlink_metadata(anc)
            && (crate::platform::is_reparse(&m) || !crate::platform::dir_meta_is_private(&m))
        {
            return default;
        }
    }
    p.to_path_buf()
}

/// Whether EVERY existing directory from `/` down to `p` is root-owned and not
/// group/other-writable — the system-prefix chain check.
///
/// Same fail-closed shape as the `$HOME` walk in [`vet_prefix`]: a non-existent tail is
/// the part atpkg will create (as root, since it must already be root to write here);
/// any existing component that is a symlink/reparse point, or is not root-owned, or is
/// group/other-writable, disqualifies the whole prefix. One writable component anywhere
/// in the chain is enough to reintroduce the CWE-379 swap window this exists to close,
/// so this is an AND over the full chain, not a leaf check.
#[must_use]
/// Can the caller actually install into `p`?
///
/// The tail of a configured prefix need not exist yet — atpkg creates it — so the
/// question is asked of the deepest component that *does* exist, which is the
/// directory the first `create_dir` would land in. A prefix whose every component is
/// missing answers `false` only if even `/` is unwritable, which is the correct
/// answer for a non-root caller.
fn caller_can_use_prefix(p: &Path) -> bool {
    // WRITABLE — we can install here. The tail of a configured prefix need not exist
    // yet (atpkg creates it), so the question is asked of the deepest component that
    // does, which is where the first `create_dir` lands.
    if p.ancestors()
        .find(|a| a.exists())
        .is_some_and(crate::platform::dir_writable_by_caller)
    {
        return true;
    }
    // NOT writable — but a prefix that already SERVES WORKING TOOLS is still usable. A
    // shared multi-user prefix is installed once by an admin and then read by users who
    // cannot write it; degrading those users to a private default would cut them off
    // from the very store they are meant to share.
    //
    // The test is whether a shim RESOLVES, not whether a `store/` directory exists.
    // Those differ exactly where it matters: the machine this fix comes from had a
    // fully populated `store/` and 22 shims in `bin/` every one of which dangled into
    // a `/Library/aterm` that had been removed. A directory count called that healthy;
    // a resolve call calls it what it is.
    let Ok(entries) = std::fs::read_dir(p.join("bin")) else {
        return false;
    };
    //
    // "Not PROVABLY gone", not "stat succeeded". The machine this branch serves is the
    // one whose store it may traverse and exec but not read, and `t.exists()` answered
    // `false` for exactly those tools — abandoning the shared prefix, printing "not
    // writable by this user", and installing the whole toolchain a second time into a
    // private default. Only a `NotFound` is evidence that a tool is not there.
    entries.flatten().any(|e| {
        crate::platform::resolve_shim(&e.path()).is_some_and(|t| !presence(&t).is_absent())
    })
}

fn system_chain_trusted(p: &Path) -> bool {
    p.ancestors().all(|anc| {
        // A component that does not exist yet is the tail we will create; skip it. An
        // existing one must be a real dir, root-owned, and not group/other-writable.
        std::fs::symlink_metadata(anc).is_ok_and(|m| {
            !crate::platform::is_reparse(&m) && crate::platform::dir_meta_is_system(&m)
        }) || std::fs::symlink_metadata(anc).is_err()
    })
}

/// Containment check `p` is at/under `home`. Case-sensitive on Unix (`starts_with`);
/// case-INSENSITIVE per-component on Windows, where the filesystem is case-insensitive so
/// `c:\users\me\pkg` is genuinely under `C:\Users\Me` and must not be rejected.
#[must_use]
fn under_home(p: &Path, home: &Path) -> bool {
    #[cfg(windows)]
    {
        let mut hc = home.components();
        let mut pc = p.components();
        loop {
            match hc.next() {
                None => return true, // consumed all of home's components ⇒ p is under home
                Some(h) => match pc.next() {
                    Some(q) if h.as_os_str().eq_ignore_ascii_case(q.as_os_str()) => continue,
                    _ => return false,
                },
            }
        }
    }
    #[cfg(not(windows))]
    {
        p.starts_with(home)
    }
}

/// Commands a managed shim must NEVER be allowed to name, even though `bin/` is only
/// *appended* to `PATH`. A tool honestly or maliciously named one of these is refused a
/// shim outright (and the refusal is surfaced in `status.toml`), so a key-compromise (or
/// an honest mistake) can't quietly intercept core/security commands. Lower-cased.
const SENSITIVE_SHIMS: &[&str] = &[
    "sudo",
    "ssh",
    "scp",
    "sshd",
    "git",
    "sh",
    "bash",
    "zsh",
    "fish",
    "env",
    "sudo_askpass",
    "doas",
    "su",
    "login",
    "passwd",
    "gpg",
    "gpg2",
    "curl",
    "wget",
    "rm",
    "mv",
    "cp",
    "ln",
    "chmod",
    "chown",
    "kill",
    "launchctl",
    "osascript",
    "security",
    "codesign",
    "spctl",
    "cargo",
    "rustc",
    "rustup",
    "python",
    "python3",
    "node",
    "ls",
    "cat",
];

/// The prefix of an ALIAS shim: `alab-<tool>` is laid beside every `<tool>` shim of one
/// of ALab's own programs and forwards to the same store executable
/// ([`crate::activate::Aliases`]). The alias exists because ALab's bare tool names collide
/// with other software (verified 2026-08-27: Homebrew's p11-kit installs a certificate
/// tool at `/opt/homebrew/bin/trust`; Homebrew core owns the formula names `ty` and
/// `clean`) and the managed `bin/` is deliberately APPENDED to `PATH` — so `trust` may run
/// someone else's copy, while `alab-trust` always names ALab's. The prefix is RESERVED:
/// `alab-<x>` is admissible exactly when `<x>` is (the sensitive-name refusal applies to
/// the base name, so `alab-sudo` is refused like `sudo`), and an alias of an alias
/// (`alab-alab-x`) never exists — a tool that already carries the prefix is its own alias.
pub const ALIAS_PREFIX: &str = "alab-";

/// Whether `name` may be installed as a `bin/` shim: a non-empty, path-separator-free
/// name that is not on the `SENSITIVE_SHIMS` deny-list (case-insensitive). Fail-closed:
/// an empty name, a name containing `/`, `\` or `\0`, or `.`/`..` is also refused.
/// BOTH separators are rejected: on Windows `Layout::shim` does `bin_dir().join(name)`, and
/// a `\` in an (untrusted, manifest-supplied) name makes `Path::join` traverse OUT of `bin/`
/// (e.g. `..\..\evil` → a `.cmd` written outside the managed tree) and also lets a name like
/// `..\git` dodge the sensitive-name deny-list. This matches `linkmode::safe_component`,
/// `ops::uninstall`, and the other name gates, which all reject `\` too.
///
/// An [`ALIAS_PREFIX`]ed name is admitted only when its BASE is: `alab-sudo` is refused
/// exactly like `sudo` (the deny-list is checked on both spellings, case-insensitively),
/// a bare `alab-` has no base and is refused, and a nested `alab-alab-x` is refused — the
/// prefix is reserved for one level of aliasing.
#[must_use]
pub fn shim_allowed(name: &str) -> bool {
    if name.is_empty()
        || name == "."
        || name == ".."
        || name.contains('/')
        || name.contains('\\')
        || name.contains('\0')
    {
        return false;
    }
    let lower = name.to_ascii_lowercase();
    if SENSITIVE_SHIMS.contains(&lower.as_str()) {
        return false;
    }
    match lower.strip_prefix(ALIAS_PREFIX) {
        // The base must itself be a shim name, and never another alias.
        Some(base) => !base.is_empty() && !base.starts_with(ALIAS_PREFIX) && shim_allowed(base),
        None => true,
    }
}

/// A **logical** tool name — one entry of a manifest's `exposes` list. Never a file name.
///
/// One `String` used to carry three different things at once: the logical name, the `bin/`
/// shim file (`<name><SHIM_SUFFIX>`), and the executable inside a build's `bin/`
/// (`<name><EXE_SUFFIX>`). On Unix both suffixes are `""`, so all three coincide and every
/// confusion between them is invisible; on Windows they are `.cmd` and `.exe` and the three
/// are three distinct files. Three live defects came out of that conflation — a rollback
/// probing `prior_dir/bin/<tool>` with no `.exe`, the same omission in the sysroot resolve
/// check, and a prune guard comparing a logical name against the on-disk `ay.cmd` (so on
/// Windows it never matched and the guard never fired) — plus nine hand-written
/// `format!("{tool}{EXE_SUFFIX}")` re-derivations of the one rule.
///
/// So this type deliberately has **no `Deref<Target = str>` and no `Display`**:
/// `Path::join(tool)` and `format!("{tool}")` do not compile, and the author must say which
/// rendering they mean — [`shim_file`](Self::shim_file) or [`exe_file`](Self::exe_file).
/// Choosing between those two is exactly the decision that was silently wrong at all three
/// sites; making it unavoidable is the whole point of the type.
#[derive(Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Debug)]
pub struct ToolName(String);

/// `<name><suffix>` — the single rule behind both of [`ToolName`]'s renderings.
///
/// Split out from the two methods, and taking the suffix as a PARAMETER, purely so the rule
/// is testable: `SHIM_SUFFIX` and `EXE_SUFFIX` are both `""` on Unix, so every assertion
/// written against the methods degenerates to the identity on the hosts this repo is
/// developed on and would hold for a wrong implementation too. The tests drive this with
/// literal `.cmd`/`.exe`.
///
/// Built with `push_str`, not `format!` (byte-identical) — Trust-gate lowering
/// workaround, see `lib.rs`.
fn with_suffix(name: &str, suffix: &str) -> String {
    let mut s = String::new();
    s.push_str(name);
    s.push_str(suffix);
    s
}

/// The inverse of [`with_suffix`], and total: a name that does NOT carry the suffix is
/// returned unchanged rather than rejected.
///
/// Totality is the load-bearing part. `strip_suffix("")` is `Some`, so on Unix this is the
/// identity for every input; on Windows a `bin/` entry may legitimately be either `ay.cmd`
/// (a shim this manager wrote) or `ay` (a hand-made file), and both must read back as the
/// logical `ay` so that re-shimming REPLACES rather than writing `ay.cmd.cmd` beside it.
fn without_suffix<'a>(name: &'a str, suffix: &str) -> &'a str {
    name.strip_suffix(suffix).unwrap_or(name)
}

impl ToolName {
    /// Admit `raw` as a tool name, or `None` when it fails [`shim_allowed`] — a sensitive
    /// command (`sudo`/`ssh`/`git`/…) or a malformed name (empty, `.`/`..`, a path separator
    /// or NUL).
    ///
    /// Running the deny-list HERE rather than at each call site is the second half of the
    /// type's job: `install_shims`, the tombstone writer, the transaction rollback, the seed
    /// "still installing" shims and the dev-link lane each used to repeat the check, and any
    /// one of them forgetting it would have put a shadowing shim on the user's `PATH`.
    #[must_use]
    pub fn new(raw: &str) -> Option<Self> {
        shim_allowed(raw).then(|| Self(raw.to_string()))
    }

    /// The `bin/` **shim** file name: `<tool><SHIM_SUFFIX>` — `ay` on Unix (a bare symlink),
    /// `ay.cmd` on Windows (a batch wrapper).
    #[must_use]
    pub fn shim_file(&self) -> String {
        with_suffix(&self.0, crate::platform::SHIM_SUFFIX)
    }

    /// The **executable** file name inside a build's `bin/`: `<tool><EXE_SUFFIX>` — `ay` on
    /// Unix, `ay.exe` on Windows. This is what a shim FORWARDS to; it is never the shim's own
    /// name, and on Windows conflating the two yields `bin/ay.cmd` pointing at `bin\ay.cmd`.
    #[must_use]
    pub fn exe_file(&self) -> String {
        with_suffix(&self.0, crate::platform::EXE_SUFFIX)
    }

    /// Recover the logical name from a `bin/` directory entry, stripping the platform
    /// [`crate::platform::SHIM_SUFFIX`] (so `ay.cmd` reads back as `ay` on Windows; on Unix
    /// the suffix is `""` and `strip_suffix("")` is the identity, so the name is unchanged).
    ///
    /// Callers feed the result back through [`Layout::shim`] / `install_shims` /
    /// `install_tombstone_shim`, which append the suffix again — returning the raw file name
    /// would double it (`bin/ay.cmd.cmd`), writing tombstones and rollback shims BESIDE the
    /// live shim instead of replacing it.
    ///
    /// `None` for a `bin/` entry that this manager could never have written (a name
    /// [`shim_allowed`] refuses), which is the fail-closed direction for the one caller that
    /// DELETES what it recognizes.
    #[must_use]
    pub fn from_shim_file(name: &str) -> Option<Self> {
        Self::new(without_suffix(name, crate::platform::SHIM_SUFFIX))
    }

    /// The logical name, for reporting (`status.toml`, refusal lists, log lines). NOT for
    /// building a path — use [`shim_file`](Self::shim_file) / [`exe_file`](Self::exe_file).
    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }

    /// Whether this is an [`ALIAS_PREFIX`]ed name (`alab-trust`), i.e. a shim that forwards
    /// to another tool's executable rather than to one of its own name.
    #[must_use]
    pub fn is_alias(&self) -> bool {
        self.0.starts_with(ALIAS_PREFIX)
    }

    /// The `alab-<tool>` alias of this tool — `None` when the tool already carries the
    /// prefix (an alias never gets an alias; the name is unambiguous as it stands). Always
    /// `Some` otherwise: a base [`shim_allowed`] admits, the prefixed spelling admits too.
    #[must_use]
    pub fn alias(&self) -> Option<Self> {
        if self.is_alias() {
            return None;
        }
        let mut s = String::from(ALIAS_PREFIX);
        s.push_str(&self.0);
        Self::new(&s)
    }

    /// The tool an alias names: `alab-trust` → `trust`; `None` for a plain name.
    #[must_use]
    pub fn alias_base(&self) -> Option<Self> {
        self.0.strip_prefix(ALIAS_PREFIX).and_then(Self::new)
    }
}

/// Split a manifest's raw `exposes` list into the names that may be shimmed and the ones
/// refused. This is the ONE place a `Vec<String>` off the wire becomes [`ToolName`]s, so the
/// refusal list stays honest (it reports the raw name the manifest actually asked for) while
/// everything downstream of it holds the validated type. Order is preserved in both halves.
#[must_use]
pub fn split_exposed(exposes: &[String]) -> (Vec<ToolName>, Vec<String>) {
    let mut tools = Vec::new();
    let mut refused = Vec::new();
    for raw in exposes {
        match ToolName::new(raw) {
            Some(t) => tools.push(t),
            None => refused.push(raw.clone()),
        }
    }
    (tools, refused)
}

/// Compose the child `PATH` for running a managed tool: the inherited `PATH` with the
/// managed `bin_dir` **appended** — never prepended, so a pinned tool that calls a sibling
/// by bare name resolves the pinned sibling, while system commands (`sudo`/`ssh`/…) on the
/// inherited `PATH` are never shadowed (§10). Idempotent: if `bin_dir` is already present
/// the inherited value is returned unchanged. With no inherited `PATH`, returns just
/// `bin_dir`. This is the single source of truth for the `atpkg run` / `aterm <tool>`
/// child environment; keeping it pure makes the append-not-prepend policy unit-testable.
#[must_use]
pub fn append_bin_to_path(inherited: Option<&OsStr>, bin_dir: &Path) -> OsString {
    // An absent OR empty inherited `PATH` means "no directories" — start empty so we never
    // emit a leading empty component (which Unix reads as the current directory).
    // `OsStr::is_empty` via `call1`: Trust-gate span-attribution workaround — see
    // `lib.rs::call1`.
    let mut dirs: Vec<PathBuf> = match inherited {
        Some(p) if !crate::call1(std::ffi::OsStr::is_empty, p) => {
            std::env::split_paths(p).collect()
        }
        _ => Vec::new(),
    };
    if !dirs.iter().any(|d| d == bin_dir) {
        dirs.push(bin_dir.to_path_buf());
    }
    // `join_paths` only fails if a component itself contains the platform separator; in that
    // (pathological) case fall back to the inherited value rather than corrupting `PATH`.
    std::env::join_paths(&dirs)
        .unwrap_or_else(|_| inherited.map(OsStr::to_os_string).unwrap_or_default())
}

#[cfg(test)]
mod tests {
    use super::*;
    #[cfg(unix)]
    use std::os::unix::fs::PermissionsExt;

    fn temp_home(label: &str) -> PathBuf {
        let h = std::env::temp_dir().join(format!("atpkg-store-{label}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&h);
        std::fs::create_dir_all(&h).unwrap();
        #[cfg(unix)]
        std::fs::set_permissions(&h, std::fs::Permissions::from_mode(0o700)).unwrap();
        h
    }

    /// `Layout::ensure_agents_dir` — the front door's rule (2026-09-18; the window's
    /// mkdir/mode rule plus a symlink refusal the window does not yet make):
    /// a fresh prefix gets `agents/` created (private, `0700`, the `$HOME` shape) and
    /// the absolute directory back; a second call is a no-op with the same answer; a
    /// symlink at `agents/` — even one that resolves to a real directory — is REFUSED
    /// and left alone; a regular file there is refused and left alone.
    #[test]
    fn ensure_agents_dir_creates_a_real_directory_once_and_refuses_a_link_or_a_file() {
        let h = temp_home("ensure-agents");
        let l = Layout {
            prefix: h.join("pkg"),
        };
        assert!(!l.agents_dir().exists(), "a fresh prefix has no agents/");
        let dir = l.ensure_agents_dir().expect("created");
        assert_eq!(dir, l.agents_dir());
        assert!(dir.is_absolute());
        assert!(
            std::fs::symlink_metadata(&dir).unwrap().is_dir(),
            "a real directory"
        );
        #[cfg(unix)]
        assert_eq!(
            std::fs::metadata(&dir).unwrap().permissions().mode() & 0o777,
            0o700,
            "a $HOME prefix's directory is private"
        );
        assert_eq!(l.ensure_agents_dir().as_deref(), Ok(dir.as_path()));
        // A regular file at agents/: refused, left alone.
        let filed = Layout {
            prefix: h.join("filed"),
        };
        std::fs::create_dir_all(&filed.prefix).unwrap();
        std::fs::write(filed.agents_dir(), b"not a dir").unwrap();
        let err = filed.ensure_agents_dir().expect_err("a file is refused");
        assert_eq!(
            err,
            format!(
                "{} exists and is not a directory",
                filed.agents_dir().display()
            ),
            "one path, one sentence"
        );
        assert!(filed.agents_dir().is_file(), "left alone");
        #[cfg(unix)]
        {
            // A symlink at agents/ — even one that points at a real directory: refused.
            let linked = Layout {
                prefix: h.join("linked"),
            };
            std::fs::create_dir_all(&linked.prefix).unwrap();
            std::os::unix::fs::symlink(&dir, linked.agents_dir()).unwrap();
            assert!(linked.agents_dir().is_dir(), "the link resolves");
            let err = linked.ensure_agents_dir().expect_err("a link is refused");
            assert_eq!(
                err,
                format!("{} is a symlink; refusing", linked.agents_dir().display()),
                "one path, one sentence — never `ensure_private_dir`'s `update directory` wording twice over"
            );
            assert_eq!(
                err.matches("linked").count(),
                1,
                "the path is named once: {err}"
            );
            assert!(
                std::fs::symlink_metadata(linked.agents_dir())
                    .unwrap()
                    .file_type()
                    .is_symlink(),
                "left alone"
            );
        }
        let _ = std::fs::remove_dir_all(&h);
    }

    /// The refusal's PURE half: what it binds, and for how long. A memo binds only the
    /// digests it was recorded for, only from the SECOND identical verdict, and only
    /// until its cooldown lapses — the three escapes that keep a bandwidth cooldown from
    /// ever becoming a verdict (a truncated transfer heals on the next attempt, a
    /// publisher may repair the asset under the same pin, and a re-cut pin moves the
    /// digests).
    #[test]
    fn a_stage_refusal_binds_only_while_its_digests_and_cooldown_hold() {
        let memo = |attempts| StageRefusal {
            sha256: "AA".into(),
            tree_root: "bb".into(),
            why: "asset sha256 mismatch: expected aa, got cc".into(),
            at: 1_000,
            attempts,
            signer: false,
        };
        // THE FIRST RETRY IS FREE: one mismatch can be a truncated transfer, and the next
        // pass must still be able to find the publisher's repair. From the second
        // identical verdict: 12 h, doubling, capped at a week.
        assert_eq!(memo(1).cooldown_secs(), 0, "one failure holds nothing");
        assert_eq!(memo(2).cooldown_secs(), 12 * 3600);
        assert_eq!(memo(3).cooldown_secs(), 24 * 3600);
        assert_eq!(memo(4).cooldown_secs(), 48 * 3600);
        assert_eq!(
            memo(10).cooldown_secs(),
            7 * 24 * 3600,
            "capped at one week"
        );
        assert_eq!(memo(1_000).cooldown_secs(), 7 * 24 * 3600, "no overflow");
        assert!(
            !memo(1).binds("aa", "bb", 1_000),
            "a single failure never holds the next attempt off the wire"
        );

        let m = memo(2);
        assert_eq!(m.retry_after(), 1_000 + 12 * 3600);
        assert!(
            m.binds("aa", "BB", 1_000),
            "case-insensitive on both digests"
        );
        assert!(
            !m.binds("aa", "BB", m.retry_after()),
            "the cooldown lapses — the next attempt is due"
        );
        assert!(
            !m.binds("dd", "bb", 1_000),
            "a re-cut pin's new sha256 binds nothing"
        );
        assert!(
            !m.binds("aa", "dd", 1_000),
            "a re-cut pin's new tree_root binds nothing"
        );
        assert!(
            !m.binds("aa", "bb", i64::MAX),
            "an unreadable clock (now_unix's fail-closed i64::MAX) reads as lapsed, \
             so it can only cost a download — never block an install"
        );
        assert!(
            !StageRefusal {
                sha256: String::new(),
                ..memo(2)
            }
            .binds("", "bb", 1_000),
            "a memo with no digest binds nothing"
        );
    }

    /// A SIGNER refusal judged bytes that matched their digests, so a re-download can only
    /// fetch the same verdict: its memo binds from the first attempt and never lapses —
    /// but only while the digests hold, and never with no digest at all.
    #[test]
    fn a_signer_refusal_binds_until_the_digests_move() {
        let m = StageRefusal {
            sha256: "AA".into(),
            tree_root: "bb".into(),
            why: "signer refused: bin/claude is not signed by Developer ID team X".into(),
            at: 1_000,
            attempts: 1,
            signer: true,
        };
        assert!(
            m.binds("aa", "BB", 1_000),
            "the first refusal already binds"
        );
        assert!(m.binds("aa", "bb", i64::MAX), "no cooldown to lapse");
        assert!(
            !m.binds("dd", "bb", 1_000),
            "a new sha256 is a new question"
        );
        assert!(
            !m.binds("aa", "dd", 1_000),
            "a new tree_root is a new question"
        );
        assert!(
            !StageRefusal {
                sha256: String::new(),
                ..m
            }
            .binds("", "bb", 1_000)
        );
    }

    /// The signer kind survives the disk round trip, a digest memo stays a digest memo,
    /// and the clears take a signer memo exactly as they take any other.
    #[test]
    fn the_signer_refusal_memo_round_trips() {
        let h = temp_home("signer-refusal");
        let l = Layout { prefix: h.clone() };
        let build = l.build_dir("claude", 1_000_002_000_001_000_280);
        record_signer_refusal(&build, "aa", "bb", "signer refused: x", 1_000).unwrap();
        let m = stage_refusal(&build).unwrap();
        assert!(m.signer);
        assert_eq!((m.attempts, m.why.as_str()), (1, "signer refused: x"));
        assert!(m.binds("aa", "bb", i64::MAX));
        record_stage_refusal(&build, "aa", "bb", "asset sha256 mismatch", 2_000).unwrap();
        assert!(!stage_refusal(&build).unwrap().signer);
        record_signer_refusal(&build, "aa", "bb", "signer refused: x", 3_000).unwrap();
        clear_stage_refusals(&l, "claude");
        assert_eq!(stage_refusal(&build), None);
        let _ = std::fs::remove_dir_all(&h);
    }

    /// The refusal memo on disk: written beside the build like `.ready`/`.shim-env`, read
    /// back whole, its attempt count ADVANCED while the digests hold and reset when they
    /// move, one memo per program at most, and taken away by the success/discard/explicit
    /// -door clears. A malformed or foreign file reads as no memo at all.
    #[test]
    fn the_refusal_memo_round_trips_advances_and_is_one_per_program() {
        let h = temp_home("stage-refusal");
        let l = Layout { prefix: h.clone() };
        let build = l.build_dir("trust", 4900);
        assert_eq!(stage_refusal(&build), None, "nothing recorded yet");

        record_stage_refusal(
            &build,
            "AA",
            "bb",
            "asset sha256 mismatch:\nexpected aa",
            1_000,
        )
        .unwrap();
        let marker = build.with_file_name("4900.refused");
        assert!(marker.is_file(), "a sibling, outside the tree");
        assert!(!build.exists(), "the build it refused never came to exist");
        let m = stage_refusal(&build).unwrap();
        assert_eq!((m.sha256.as_str(), m.tree_root.as_str()), ("AA", "bb"));
        assert_eq!(m.at, 1_000);
        assert_eq!(m.attempts, 1);
        assert_eq!(
            m.why, "asset sha256 mismatch: expected aa",
            "one line: the newline is folded"
        );

        // The same digests again: the attempt count advances, which is what lengthens the
        // cooldown (12 h, then 24, …) instead of retrying every six-hourly tick.
        record_stage_refusal(&build, "aa", "BB", "again", 50_000).unwrap();
        let m = stage_refusal(&build).unwrap();
        assert_eq!((m.attempts, m.at), (2, 50_000));
        // Digests that MOVED are a different question: the count starts again.
        record_stage_refusal(&build, "cc", "bb", "new pin, new verdict", 60_000).unwrap();
        assert_eq!(stage_refusal(&build).unwrap().attempts, 1);

        // One memo per program: recording the next build's reclaims the last build's.
        let next = l.build_dir("trust", 4901);
        record_stage_refusal(&next, "dd", "ee", "and again", 70_000).unwrap();
        assert!(
            !marker.exists(),
            "the superseded build's memo is reclaimed, not left to accumulate"
        );
        assert!(stage_refusal(&next).is_some());

        // Malformed/foreign content reads as NO memo — the fail-open direction.
        std::fs::write(next.with_file_name("4901.refused"), b"nonsense\nat=1\n").unwrap();
        assert_eq!(
            stage_refusal(&next),
            None,
            "a file that does not open with the schema line is not read"
        );

        // The three clears.
        record_stage_refusal(&next, "dd", "ee", "once more", 80_000).unwrap();
        clear_stage_refusal(&next);
        assert_eq!(stage_refusal(&next), None);
        clear_stage_refusal(&next); // idempotent
        record_stage_refusal(&next, "dd", "ee", "once more", 90_000).unwrap();
        clear_stage_refusals(&l, "trust");
        assert_eq!(stage_refusal(&next), None, "the explicit door forgets it");
        record_stage_refusal(&next, "dd", "ee", "once more", 95_000).unwrap();
        std::fs::create_dir_all(next.join("bin")).unwrap();
        discard_build(&next);
        assert_eq!(stage_refusal(&next), None, "the discard takes it too");
        let _ = std::fs::remove_dir_all(&h);
    }

    #[test]
    fn layout_paths_are_under_prefix() {
        let l = Layout {
            prefix: PathBuf::from("/p"),
        };
        assert_eq!(l.build_dir("ay", 18), PathBuf::from("/p/store/ay/18"));
        // The shim file name carries the concrete platform suffix (`.cmd` on Windows).
        assert_eq!(
            l.shim(&ToolName::new("ay").unwrap()),
            PathBuf::from(format!("/p/bin/ay{}", crate::platform::SHIM_SUFFIX))
        );
        assert_eq!(
            l.channel_current("stable"),
            PathBuf::from("/p/channels/stable/current")
        );
        assert_eq!(l.staging_dir("ay"), PathBuf::from("/p/staging/ay"));
        assert_eq!(l.floor(), PathBuf::from("/p/floor"));
        assert_eq!(l.store_lock(), PathBuf::from("/p/store.lock"));
    }

    #[test]
    fn unset_or_default_prefix_uses_default() {
        let home = temp_home("default");
        // No config ⇒ default prefix under home.
        assert_eq!(vet_prefix(None, &home), default_prefix(&home));
        let _ = std::fs::remove_dir_all(&home);
    }

    #[test]
    fn prefix_outside_home_or_with_traversal_falls_back() {
        let home = temp_home("outside");
        // Not under home.
        assert_eq!(
            vet_prefix(Some(Path::new("/tmp/evil")), &home),
            default_prefix(&home)
        );
        // A `..` escape component, even if it textually starts under home.
        let sneaky = home.join("../somewhere/pkg");
        assert_eq!(vet_prefix(Some(&sneaky), &home), default_prefix(&home));
        // home itself is not a valid prefix (the manager must own a subdir).
        assert_eq!(vet_prefix(Some(&home), &home), default_prefix(&home));
        let _ = std::fs::remove_dir_all(&home);
    }

    /// The SYSTEM prefix shape: outside `$HOME` is admissible when every existing
    /// ancestor is root-owned and not group/other-writable. This is what makes the
    /// verified Trust lane reachable at all — its launcher refuses a user-owned path
    /// component, so an atpkg that can only install under `$HOME` can never provide a
    /// toolchain with pathname execution authority.
    #[cfg(unix)]
    #[test]
    fn root_owned_prefix_outside_home_is_accepted() {
        use std::os::unix::fs::MetadataExt as _;
        let home = temp_home("sysprefix");
        // Precondition, asserted rather than assumed: /usr/lib must really be
        // root-owned and not group/other-writable on this machine.
        let Ok(meta) = std::fs::symlink_metadata("/usr/lib") else {
            let _ = std::fs::remove_dir_all(&home);
            return;
        };
        if meta.uid() != 0 || meta.mode() & 0o022 != 0 {
            let _ = std::fs::remove_dir_all(&home);
            return;
        }
        // A non-existent leaf is the tail the installer creates (as root).
        let prefix = Path::new("/usr/lib/aterm-pkg-system-prefix-test");
        // The TRUST property this test is named for holds for every caller.
        assert!(
            system_chain_trusted(prefix),
            "a fully root-owned chain outside $HOME is a trusted system prefix"
        );
        // What `vet_prefix` RETURNS now also depends on whether this caller could
        // ever use it — trusted is not the same as usable. As root (the installer
        // this test's comment describes) the tail is creatable and the prefix is
        // returned; as an ordinary user it is not, and returning it would hand back
        // a prefix that fails on `store.lock` for every verb, forever. That was a
        // real three-week outage, so the fallback is deliberate
        // (docs/AUDIT-nux-first-open-toolchain-2026-08-31.md).
        let expected = if caller_can_use_prefix(prefix) {
            prefix.to_path_buf()
        } else {
            default_prefix(&home)
        };
        assert_eq!(vet_prefix(Some(prefix), &home), expected);
        let _ = std::fs::remove_dir_all(&home);
    }

    /// The system shape is an AND over the WHOLE chain, not a leaf check: one
    /// world-writable ancestor reintroduces the CWE-379 swap window, so the prefix must
    /// fail closed to the default even though the leaf itself would be created by root.
    /// `/private/tmp` is mode 0777 and root-owned — exactly that trap.
    #[cfg(unix)]
    #[test]
    fn world_writable_system_ancestor_is_refused() {
        use std::os::unix::fs::MetadataExt as _;
        let home = temp_home("wwancestor");
        let Ok(meta) = std::fs::symlink_metadata("/private/tmp") else {
            let _ = std::fs::remove_dir_all(&home);
            return;
        };
        if meta.mode() & 0o022 == 0 {
            let _ = std::fs::remove_dir_all(&home);
            return; // not the world-writable fixture this test needs
        }
        let prefix = Path::new("/private/tmp/aterm-pkg-should-not-be-trusted");
        assert_eq!(
            vet_prefix(Some(prefix), &home),
            default_prefix(&home),
            "a world-writable ancestor must fail closed even when root owns it"
        );
        let _ = std::fs::remove_dir_all(&home);
    }

    #[cfg(unix)] // group-writable chmod fixture — Unix-only
    #[test]
    fn group_writable_intermediate_parent_is_rejected() {
        let home = temp_home("gwparent");
        // A safe (0700) intermediate, then a group/other-writable one beneath it, then
        // the would-be prefix leaf — the design's exact "intermediate parent rejected" case.
        let mid = home.join("Library");
        std::fs::create_dir_all(&mid).unwrap();
        std::fs::set_permissions(&mid, std::fs::Permissions::from_mode(0o700)).unwrap();
        let bad = mid.join("shared");
        std::fs::create_dir_all(&bad).unwrap();
        std::fs::set_permissions(&bad, std::fs::Permissions::from_mode(0o777)).unwrap(); // group/other-writable
        let prefix = bad.join("pkg");
        assert_eq!(
            vet_prefix(Some(&prefix), &home),
            default_prefix(&home),
            "a group/other-writable intermediate parent must fail closed to the default"
        );
        let _ = std::fs::remove_dir_all(&home);
    }

    #[test]
    fn safe_under_home_prefix_is_accepted() {
        let home = temp_home("safe");
        // Build a fully-safe chain home/a/b (0700 each); the not-yet-existing leaf is fine.
        let a = home.join("a");
        std::fs::create_dir_all(&a).unwrap();
        #[cfg(unix)]
        std::fs::set_permissions(&a, std::fs::Permissions::from_mode(0o700)).unwrap();
        let prefix = a.join("b").join("pkg"); // b + pkg do not exist yet
        assert_eq!(vet_prefix(Some(&prefix), &home), prefix);
        let _ = std::fs::remove_dir_all(&home);
    }

    /// A SYSTEM prefix that is trusted but that this user cannot write must fall back
    /// to the default, not be handed back to be failed on later.
    ///
    /// This is the defect that left a machine with no toolchain for three weeks: the
    /// chain check proved `/usr/local/aterm/pkg` was root-owned and therefore
    /// *trusted*, `vet_prefix` returned it, and every verb then died on `store.lock`
    /// with `Operation not permitted` — while the GUI logged the failure and showed
    /// nothing (docs/AUDIT-nux-first-open-toolchain-2026-08-31.md).
    ///
    /// Runs only as a non-root user: as root every path is writable and there is
    /// nothing to assert.
    #[cfg(unix)]
    #[test]
    fn trusted_system_prefix_that_we_cannot_write_falls_back_to_default() {
        if crate::platform::our_uid() == 0 {
            return; // root can write anywhere; the case does not exist.
        }
        let home = temp_home("unwritable-system");
        // `/usr` is the real thing this guards: outside $HOME, root-owned, not
        // group/other-writable — so `system_chain_trusted` accepts it — and not
        // writable by an ordinary user.
        let prefix = PathBuf::from("/usr/local/aterm-nux-probe/pkg");
        if !system_chain_trusted(&prefix) {
            return; // a machine with a non-standard /usr chain proves nothing here.
        }
        assert!(
            !caller_can_use_prefix(&prefix),
            "precondition: a non-root user must not be able to write under /usr"
        );
        assert_eq!(
            vet_prefix(Some(&prefix), &home),
            default_prefix(&home),
            "a trusted-but-unwritable system prefix must degrade to the default"
        );
        let _ = std::fs::remove_dir_all(&home);
    }

    /// THE SHARED-PREFIX USER THIS CHECK EXISTS FOR, CUT OFF BY THE CHECK ITSELF.
    ///
    /// A prefix the caller cannot write is still usable when its shims SERVE — that is
    /// the whole second half of `caller_can_use_prefix`, and it is the admin-installed
    /// multi-user store the comment above it describes. But the serving test was
    /// `t.exists()`, which is `fs::metadata(..).is_ok()`: on the very machine shape it
    /// protects — a store an admin laid down and a user may execute but not stat — it
    /// answers `false` for tools that run perfectly. `vet_prefix` then prints "not
    /// writable by this user", silently redirects the whole store to a private default,
    /// and installs the entire toolchain a second time. Losing the shared store is the
    /// exact outcome this branch was written to prevent.
    ///
    /// Only the permission to LOOK is withdrawn here; the shim and its target are
    /// untouched and would still exec.
    #[cfg(unix)]
    #[test]
    fn an_unwritable_prefix_whose_shims_cannot_be_stat_ed_is_still_usable() {
        use std::os::unix::fs::PermissionsExt as _;
        if crate::platform::our_uid() == 0 {
            return; // root reads and writes through mode 0; the case does not exist.
        }
        let prefix = std::env::temp_dir().join(format!("atpkg-shared-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&prefix);
        let holder = prefix.join("store/ay/17/bin");
        std::fs::create_dir_all(&holder).unwrap();
        std::fs::create_dir_all(prefix.join("bin")).unwrap();
        let ay = ToolName::new("ay").unwrap();
        std::fs::write(holder.join(ay.exe_file()), b"#!/bin/true\n").unwrap();
        crate::platform::install_shim(&holder, &ay, &prefix.join("bin").join(ay.shim_file()))
            .unwrap();
        assert!(
            caller_can_use_prefix(&prefix),
            "precondition: a writable prefix serving a real shim is usable"
        );
        // The admin-installed shape: the user may traverse and exec, not write or stat.
        std::fs::set_permissions(&holder, std::fs::Permissions::from_mode(0o000)).unwrap();
        std::fs::set_permissions(&prefix, std::fs::Permissions::from_mode(0o555)).unwrap();
        let target = prefix.join("store/ay/17/bin").join(ay.exe_file());
        let armed = std::fs::metadata(&target).is_err();
        let usable = caller_can_use_prefix(&prefix);
        std::fs::set_permissions(&prefix, std::fs::Permissions::from_mode(0o700)).unwrap();
        std::fs::set_permissions(&holder, std::fs::Permissions::from_mode(0o700)).unwrap();
        let _ = std::fs::remove_dir_all(&prefix);
        assert!(
            armed,
            "the fixture must make the stat fail, or it proves nothing"
        );
        assert!(
            usable,
            "a shared prefix whose tools we may run but not stat must not be abandoned"
        );
    }

    /// The negative half: a writable directory outside `$HOME` must still be usable,
    /// so the check above cannot be satisfied by simply refusing every system prefix.
    #[cfg(unix)]
    #[test]
    fn writable_prefix_is_still_accepted_by_the_usability_check() {
        let home = temp_home("writable-check");
        let writable = std::env::temp_dir().join(format!("atpkg-writable-{}", std::process::id()));
        std::fs::create_dir_all(&writable).unwrap();
        assert!(
            caller_can_use_prefix(&writable.join("pkg")),
            "a directory we just created must read as usable, including its absent tail"
        );
        let _ = std::fs::remove_dir_all(&writable);
        let _ = std::fs::remove_dir_all(&home);
    }

    #[test]
    fn shim_names_collision_and_shape_policy() {
        // Sensitive commands are refused (case-insensitively).
        for bad in ["sudo", "SSH", "git", "sh", "env", "rustc", "Codesign"] {
            assert!(!shim_allowed(bad), "{bad} must be refused a shim");
        }
        // Malformed shapes are refused.
        for bad in ["", ".", "..", "a/b", "x\0y"] {
            assert!(!shim_allowed(bad), "{bad:?} is not a valid shim name");
        }
        // Ordinary tool names are allowed.
        for ok in ["ay", "ny", "trust-mc", "clean-certify"] {
            assert!(shim_allowed(ok), "{ok} should be allowed");
        }
    }

    /// The `alab-` alias prefix: admitted exactly when the BASE is — the sensitive-name
    /// refusal applies to the base (`alab-sudo` is `sudo` wearing a hat), the bare prefix
    /// has no base, and the prefix nests once, never twice.
    #[test]
    fn alias_names_admit_on_their_base_and_refuse_sensitive_bases() {
        for ok in ["alab-ay", "alab-trust", "alab-trust-cg"] {
            assert!(shim_allowed(ok), "{ok} should be allowed");
        }
        for bad in [
            "alab-sudo",
            "alab-SSH",
            "alab-git",
            "alab-cargo",
            "alab-",
            "alab-alab-ay",
            "alab-a/b",
            "alab-..",
        ] {
            assert!(!shim_allowed(bad), "{bad:?} must be refused a shim");
        }
        let trust = ToolName::new("trust").unwrap();
        assert!(!trust.is_alias());
        let alias = trust.alias().expect("a plain admissible name has an alias");
        assert_eq!(alias.as_str(), "alab-trust");
        assert!(alias.is_alias());
        assert_eq!(alias.alias_base(), Some(trust.clone()));
        assert_eq!(trust.alias_base(), None, "a plain name has no base");
        assert_eq!(alias.alias(), None, "an alias never gets an alias");
        // Hyphenated tools alias like any other — and the shim/exe renderings carry the
        // whole alias, never the base.
        let cg = ToolName::new("trust-cg").unwrap();
        let cg_alias = cg.alias().unwrap();
        assert_eq!(cg_alias.as_str(), "alab-trust-cg");
        assert_eq!(cg_alias.alias_base(), Some(cg));
        assert_eq!(
            cg_alias.shim_file(),
            format!("alab-trust-cg{}", crate::platform::SHIM_SUFFIX)
        );
        assert_eq!(ToolName::new("alab-sudo"), None);
        assert_eq!(ToolName::from_shim_file("alab-git"), None);
    }

    #[test]
    fn tool_name_admits_through_the_deny_list_and_renders_both_file_names() {
        // Construction IS the deny-list check — a refused name has no ToolName at all, so it
        // cannot be handed to `Layout::shim`, `install_shim`, or anything else that writes.
        assert!(ToolName::new("sudo").is_none());
        assert!(ToolName::new("../git").is_none());
        assert!(ToolName::new("").is_none());
        let ay = ToolName::new("ay").unwrap();
        assert_eq!(ay.as_str(), "ay");
        // The two renderings are the shim's own name and the binary it forwards to. They are
        // the same string on Unix and different files on Windows; the point of the type is
        // that a caller must pick one, not that they differ on the host that runs this test.
        assert_eq!(
            ay.shim_file(),
            format!("ay{}", crate::platform::SHIM_SUFFIX)
        );
        assert_eq!(ay.exe_file(), format!("ay{}", crate::platform::EXE_SUFFIX));
    }

    #[test]
    fn from_shim_file_round_trips_and_never_doubles_the_suffix() {
        let ay = ToolName::new("ay").unwrap();
        // The exact inverse of `shim_file` — this is what makes `bin/ay.cmd` read back as the
        // logical `ay`, so re-shimming it replaces the live shim instead of writing
        // `bin/ay.cmd.cmd` beside it.
        assert_eq!(ToolName::from_shim_file(&ay.shim_file()), Some(ay.clone()));
        assert_eq!(ToolName::from_shim_file("ay"), Some(ay));
        // A `bin/` entry this manager could never have written is not recognized, so the
        // one caller that DELETES what it recognizes leaves it alone.
        assert_eq!(ToolName::from_shim_file("sudo"), None);
    }

    /// The two assertions above are, on Unix, `""`-suffixed identities: they hold for ANY
    /// implementation, including the conflated `String` this type replaced. The defect the
    /// type exists to close is a WINDOWS one (`.cmd` vs `.exe` vs the logical name), and no
    /// macOS/Linux runner can reach it through `SHIM_SUFFIX`. So drive the rule directly.
    #[test]
    fn the_suffix_rule_round_trips_and_never_doubles_on_a_suffixed_platform() {
        // Windows' real pair: a shim and the executable it forwards to are DIFFERENT files.
        assert_eq!(with_suffix("ay", ".cmd"), "ay.cmd");
        assert_eq!(with_suffix("ay", ".exe"), "ay.exe");
        assert_ne!(with_suffix("ay", ".cmd"), with_suffix("ay", ".exe"));

        // The round trip a `bin/` scan performs: file name -> logical -> file name.
        assert_eq!(without_suffix(&with_suffix("ay", ".cmd"), ".cmd"), "ay");
        // THE trap: strip first, or re-appending writes `bin/ay.cmd.cmd` BESIDE the live
        // shim instead of replacing it — silently disabling nothing and tombstoning nothing.
        assert_eq!(
            with_suffix(&with_suffix("ay", ".cmd"), ".cmd"),
            "ay.cmd.cmd"
        );

        // Total, not fallible: an unsuffixed entry reads back unchanged …
        assert_eq!(without_suffix("ay", ".cmd"), "ay");
        // … and the shim suffix is NOT the exe suffix, so a `.exe` in `bin/` keeps its name
        // rather than being mistaken for a shim of `ay`.
        assert_eq!(without_suffix("ay.exe", ".cmd"), "ay.exe");

        // The empty suffix (Unix) is the identity in both directions — which is exactly why
        // every assertion phrased in terms of `SHIM_SUFFIX` is vacuous here.
        assert_eq!(with_suffix("ay", ""), "ay");
        assert_eq!(without_suffix("ay", ""), "ay");
    }

    #[test]
    fn split_exposed_keeps_the_raw_name_in_the_refusal_list() {
        let raw = vec!["ay".to_string(), "sudo".to_string(), "trust-mc".to_string()];
        let (tools, refused) = split_exposed(&raw);
        assert_eq!(
            tools.iter().map(ToolName::as_str).collect::<Vec<_>>(),
            vec!["ay", "trust-mc"]
        );
        // Refusals are reported with the name the manifest actually asked for.
        assert_eq!(refused, vec!["sudo".to_string()]);
    }

    /// The Rosetta hazard, end to end: a readiness marker written by the OTHER slice of
    /// the universal binary must not vouch for the build to this one, and an ordinary
    /// native re-install must make it vouch again (the store repairs, it does not wedge).
    #[test]
    fn a_marker_from_the_other_slice_reads_as_not_installed() {
        let home = temp_home("readyslice");
        let build = home.join("store").join("ay").join("18");
        std::fs::create_dir_all(&build).unwrap();
        mark_build_ready(&build).unwrap();
        assert!(build_is_complete(&build));

        let marker = ready_marker_path(&build).unwrap();
        let text = std::fs::read_to_string(&marker).unwrap();
        // Non-vacuity: the record is really written, so the assertions below mean something.
        assert!(
            text.starts_with("ok\n"),
            "line 1 stays what every earlier version wrote: {text:?}"
        );
        assert_eq!(recorded_platform(&text), Some(running_platform().as_str()));

        // Forge the marker the x86_64 slice would have left behind on this machine.
        let mut foreign = String::from("ok\n");
        foreign.push_str(READY_PLATFORM_KEY);
        foreign.push_str("some-other-arch-macos\n");
        std::fs::write(&marker, foreign).unwrap();
        assert!(
            !build_is_complete(&build),
            "a build installed by the other architecture is not installed for us"
        );

        mark_build_ready(&build).unwrap();
        assert!(
            build_is_complete(&build),
            "re-staging natively must clear the mismatch, not leave the build unusable"
        );
        let _ = std::fs::remove_dir_all(&home);
    }

    /// THE m3 CORPSE, exactly as it was found (2026-09-17): `store/trust/8595/` with no
    /// `bin/` at all, 417 MB of orphaned `lib/`, and `8595.ready` beside it still saying
    /// `ok`. `ops::list_installed` counted it as an installed build, `gc` would not sweep it
    /// (it sweeps only marker-LESS trees) and `flow::rollback` would have switched onto it,
    /// removing every shim on the machine.
    ///
    /// A readiness marker is now a claim ABOUT CONTENTS: it records the `bin/` it was
    /// written over, and the disk can refute it.
    #[test]
    fn a_marker_is_refuted_when_the_bin_it_vouched_for_is_gone() {
        let home = temp_home("readygutted");
        let build = home.join("store").join("trust").join("8595");
        std::fs::create_dir_all(build.join("bin")).unwrap();
        std::fs::create_dir_all(build.join("lib")).unwrap();
        std::fs::write(build.join("bin").join("trustc"), b"#!/bin/true\n").unwrap();
        std::fs::write(build.join("bin").join("targo"), b"#!/bin/true\n").unwrap();
        std::fs::write(build.join("lib").join("libtrust.dylib"), b"payload").unwrap();
        mark_build_ready(&build).unwrap();
        assert!(build_is_complete(&build), "precondition: a whole build");

        // The record is really there, and it really names the tools — otherwise the
        // refutation below could be produced by a check that reads nothing.
        let text = std::fs::read_to_string(ready_marker_path(&build).unwrap()).unwrap();
        assert_eq!(
            recorded_contents(&text),
            Some("bin:targo,trustc"),
            "sorted, comma-joined, so the record is a function of the tree: {text:?}"
        );

        // Now gut it the way an interrupted `remove_dir_all` does: `bin` goes, `lib` stays,
        // the marker is untouched.
        std::fs::remove_dir_all(build.join("bin")).unwrap();
        assert!(
            ready_marker_path(&build).unwrap().is_file(),
            "the fixture must leave the marker in place, or it proves nothing"
        );
        assert!(
            build.join("lib").join("libtrust.dylib").is_file(),
            "and the orphaned payload, exactly as on m3"
        );
        assert!(
            !build_is_complete(&build),
            "a marker whose `bin/` is gone vouches for nothing"
        );
        let _ = std::fs::remove_dir_all(&home);
    }

    /// The other half of the same corpse: ONE tool removed out of a `bin/` that is
    /// otherwise intact. A predicate that only asked "is there a `bin/`?" would miss it.
    #[test]
    fn a_marker_is_refuted_when_one_recorded_tool_is_gone() {
        let home = temp_home("readyonetool");
        let build = home.join("store").join("trust").join("8595");
        std::fs::create_dir_all(build.join("bin")).unwrap();
        std::fs::write(build.join("bin").join("trustc"), b"x").unwrap();
        std::fs::write(build.join("bin").join("targo"), b"x").unwrap();
        mark_build_ready(&build).unwrap();
        std::fs::remove_file(build.join("bin").join("trustc")).unwrap();
        assert!(
            !build_is_complete(&build),
            "the marker named `trustc`; `trustc` is not there"
        );
    }

    /// A `bin/` ENTRY that is a symlink is recorded and checked BY ITS OWN NAME: the link
    /// standing is what the record claims, and a dangling link is doctor's to report, not
    /// this predicate's to re-stage on. Otherwise every build whose `bin/clippy` points at a
    /// tool it ships would re-download on a machine where that target moved.
    #[cfg(unix)]
    #[test]
    fn a_bin_symlink_is_recorded_and_checked_by_its_own_name() {
        let home = temp_home("readylink");
        let build = home.join("store").join("trust").join("8595");
        std::fs::create_dir_all(build.join("bin")).unwrap();
        std::fs::write(build.join("bin").join("tippy"), b"x").unwrap();
        std::os::unix::fs::symlink("tippy", build.join("bin").join("clippy")).unwrap();
        mark_build_ready(&build).unwrap();
        let text = std::fs::read_to_string(ready_marker_path(&build).unwrap()).unwrap();
        assert_eq!(recorded_contents(&text), Some("bin:clippy,tippy"));
        // The link's TARGET goes; the link itself stands. Still complete.
        std::fs::remove_file(build.join("bin").join("tippy")).unwrap();
        assert!(
            !build_is_complete(&build),
            "`tippy` was recorded in its own right and is gone"
        );
        std::fs::write(build.join("bin").join("tippy"), b"x").unwrap();
        assert!(build_is_complete(&build));
        // …and the link itself going IS a refutation.
        std::fs::remove_file(build.join("bin").join("clippy")).unwrap();
        assert!(!build_is_complete(&build));
        let _ = std::fs::remove_dir_all(&home);
    }

    /// A build with no `bin/` at all when it was marked records a RECORDED EMPTINESS, which
    /// is not the same thing as no record: it is never refuted for having no `bin/`, and it
    /// never silently inherits the legacy lenience either.
    #[test]
    fn a_build_with_no_bin_records_none_and_stays_complete() {
        let home = temp_home("readynone");
        let build = home.join("store").join("clt").join("3");
        std::fs::create_dir_all(&build).unwrap();
        mark_build_ready(&build).unwrap();
        let text = std::fs::read_to_string(ready_marker_path(&build).unwrap()).unwrap();
        assert_eq!(recorded_contents(&text), Some(READY_CONTENTS_NONE));
        assert!(build_is_complete(&build));
        let _ = std::fs::remove_dir_all(&home);
    }

    /// A marker for a build dir that IS NOT THERE. `mark_build_ready` has no existence
    /// precondition, so `install::restore_outgoing` can write one after a restore that
    /// failed — a state that function's own doc called "a durable lie one refactor away
    /// from being believed". The predicate refuses it at the door.
    #[test]
    fn a_marker_over_an_absent_build_dir_is_not_complete() {
        let home = temp_home("readyghost");
        let build = home.join("store").join("ay").join("18");
        std::fs::create_dir_all(&build).unwrap();
        mark_build_ready(&build).unwrap();
        std::fs::remove_dir_all(&build).unwrap();
        assert!(
            ready_marker_path(&build).unwrap().is_file(),
            "the fixture keeps the sibling marker, which is the whole point"
        );
        assert!(!build_is_complete(&build));
        let _ = std::fs::remove_dir_all(&home);
    }

    /// The contents rule, driven directly — the tests above all run on one machine with one
    /// real `bin/`, so they would hold for an implementation that never compared anything.
    #[test]
    fn contents_still_stand_refutes_only_a_proven_absence() {
        let home = temp_home("readycontents");
        let build = home.join("b");
        std::fs::create_dir_all(build.join("bin")).unwrap();
        std::fs::write(build.join("bin").join("ay"), b"x").unwrap();
        assert!(contents_still_stand(&build, READY_CONTENTS_NONE));
        assert!(contents_still_stand(&build, "bin:ay"));
        assert!(!contents_still_stand(&build, "bin:ay,ny"));
        // A value a LATER atpkg might write is not a refutation: unknown is not refuted.
        assert!(contents_still_stand(&build, "tree-root:deadbeef"));
        // …and an unrecognised value does not accidentally match the `bin:` branch.
        assert!(contents_still_stand(&build, "bin"));
        let _ = std::fs::remove_dir_all(&home);
    }

    /// `discard_build` takes the MARKER DOWN FIRST. The corpse on m3 was made by the old
    /// order — tree first, marker last — where anything ending the process inside a
    /// multi-gigabyte `remove_dir_all` leaves a gutted tree still vouched for.
    ///
    /// The interrupt is simulated the only way a test can: the tree is made UNREMOVABLE
    /// (its own permissions, so its writable parent can still lose the marker), which is one
    /// of the real ways that walk aborts partway. Under the old order the marker would
    /// survive untouched; under this one the build reads as incomplete the moment the
    /// function returns, whatever happened to the tree.
    #[cfg(unix)]
    #[test]
    fn discard_build_unmarks_before_it_removes_so_an_interrupted_discard_leaves_no_lie() {
        let home = temp_home("discardorder");
        let build = home.join("store").join("trust").join("8595");
        std::fs::create_dir_all(build.join("bin")).unwrap();
        std::fs::write(build.join("bin").join("trustc"), b"x").unwrap();
        mark_build_ready(&build).unwrap();
        assert!(build_is_complete(&build), "precondition");
        // No write permission on `bin/`: its entries cannot be unlinked, so `remove_dir_all`
        // aborts inside the tree — the interrupt, in a form a test can arrange.
        std::fs::set_permissions(build.join("bin"), std::fs::Permissions::from_mode(0o500))
            .unwrap();
        discard_build(&build);
        let survived = build.join("bin").join("trustc").is_file();
        std::fs::set_permissions(build.join("bin"), std::fs::Permissions::from_mode(0o700))
            .unwrap();
        assert!(
            survived,
            "the fixture must make the removal fail, or it proves nothing"
        );
        assert!(
            !ready_marker_path(&build).unwrap().exists(),
            "the marker comes down FIRST, so a failed removal cannot leave one behind"
        );
        assert!(
            !build_is_complete(&build),
            "and the surviving tree reads as incomplete — reclaimable debris, not an install"
        );
        let _ = std::fs::remove_dir_all(&home);
    }

    /// The whole-program twin: `unmark_program_builds` takes down every build's marker and
    /// touches nothing else, so `ops::uninstall` can run its `remove_dir_all` knowing an
    /// interrupt inside it leaves incomplete trees rather than installed ones.
    #[test]
    fn unmark_program_builds_unmarks_every_build_and_only_builds() {
        let home = temp_home("unmarkall");
        let prog = home.join("store").join("trust");
        for n in [8590u64, 8595] {
            let b = prog.join(crate::dec_u64(n));
            std::fs::create_dir_all(b.join("bin")).unwrap();
            std::fs::write(b.join("bin").join("trustc"), b"x").unwrap();
            mark_build_ready(&b).unwrap();
        }
        // A non-numeric sibling directory, and a sidecar that is not a readiness marker.
        std::fs::create_dir_all(prog.join("8595.incoming-7")).unwrap();
        std::fs::write(prog.join("8595.provenance"), b"src\n").unwrap();
        unmark_program_builds(&prog);
        for n in [8590u64, 8595] {
            let b = prog.join(crate::dec_u64(n));
            assert!(!build_is_complete(&b), "build {n} must read as incomplete");
            assert!(
                b.join("bin").join("trustc").is_file(),
                "and nothing but the marker is touched"
            );
        }
        assert!(
            prog.join("8595.provenance").is_file(),
            "other sidecars stay"
        );
        assert!(prog.join("8595.incoming-7").is_dir());
        let _ = std::fs::remove_dir_all(&home);
    }

    /// BACKWARD COMPATIBILITY: a store written before the platform record existed holds a
    /// bare `ok\n`. It must keep reading as installed — treating it as a mismatch would
    /// re-download the whole toolchain on every machine that already has one.
    #[test]
    fn a_legacy_marker_without_a_platform_record_still_reads_as_installed() {
        let home = temp_home("readylegacy");
        let build = home.join("store").join("ay").join("18");
        std::fs::create_dir_all(&build).unwrap();
        std::fs::write(ready_marker_path(&build).unwrap(), b"ok\n").unwrap();
        assert!(build_is_complete(&build));
        // And an absent marker is still the "partial install" answer it always was.
        let _ = std::fs::remove_file(ready_marker_path(&build).unwrap());
        assert!(!build_is_complete(&build));
        let _ = std::fs::remove_dir_all(&home);
    }

    /// The accept rule itself, driven directly: the two tests above can only ever see the
    /// ONE platform this test binary was compiled for, so on any single runner they would
    /// hold for an implementation that never compared anything.
    #[test]
    fn ready_text_accepts_only_an_absent_or_matching_record() {
        assert!(ready_text_accepts("ok\n", "aarch64-macos"));
        // A marker with no `ok` line is a CRASH ARTEFACT, never a legacy marker: empty is
        // what a rename-without-fsync exposes after a power loss, and a NUL run is how a
        // committed-but-unwritten inode reads back.
        assert!(!ready_text_accepts("", "aarch64-macos"));
        assert!(!ready_text_accepts("\0\0\0\0", "aarch64-macos"));
        assert!(!ready_text_accepts("o", "aarch64-macos"));
        assert!(!ready_text_accepts(
            "platform=aarch64-macos\n",
            "aarch64-macos"
        ));
        // …while a well-formed marker that never grew a trailing newline is well formed.
        assert!(ready_text_accepts("ok", "aarch64-macos"));
        assert!(ready_text_accepts(
            "ok\nplatform=aarch64-macos\n",
            "aarch64-macos"
        ));
        // THE hazard case: the Intel slice's marker, read by the native arm64 slice.
        assert!(!ready_text_accepts(
            "ok\nplatform=x86_64-macos\n",
            "aarch64-macos"
        ));
        // …and symmetrically, so neither direction inherits the other's install.
        assert!(!ready_text_accepts(
            "ok\nplatform=aarch64-macos\n",
            "x86_64-macos"
        ));
        // A stray `\r` (a marker copied through a Windows-y tool) is whitespace, not a
        // different architecture.
        assert!(ready_text_accepts(
            "ok\r\nplatform=aarch64-macos\r\n",
            "aarch64-macos"
        ));
        // An empty value carries no information, and an unrecognised key is not a record:
        // both are "absent", which is accept — never a reinstall.
        assert!(ready_text_accepts("ok\nplatform=\n", "aarch64-macos"));
        assert!(ready_text_accepts(
            "ok\narch=x86_64-macos\n",
            "aarch64-macos"
        ));
    }

    /// THE CRASH MARKER, through the real predicate. `<build>.ready` is published by a
    /// rename — a directory entry — while the bytes behind it are page cache, so a power
    /// loss in that window leaves the NAME with no contents. Until the `ok` line was
    /// required, such a marker took the "no platform record" branch and vouched for the
    /// build: the store reported it installed, `decide` answered `UpToDate`, and nothing
    /// short of a hand-run `atpkg verify` ever looked at the tree again — while the tree's
    /// own data, flushed by nothing either, may have been just as lost.
    #[test]
    fn a_marker_left_empty_or_torn_by_a_crash_reads_as_incomplete() {
        let home = temp_home("readytorn");
        let build = home.join("store").join("ay").join("18");
        std::fs::create_dir_all(&build).unwrap();
        let marker = ready_marker_path(&build).unwrap();

        // Exactly what a rename of unflushed bytes exposes after a power loss.
        std::fs::write(&marker, b"").unwrap();
        assert!(
            !build_is_complete(&build),
            "a zero-length marker must never vouch for a build"
        );
        // The same crash's other shape: the inode committed, its blocks read back as zeros.
        std::fs::write(&marker, vec![0u8; 4096]).unwrap();
        assert!(
            !build_is_complete(&build),
            "a NUL-filled marker is not `ok`"
        );
        // Bytes that are not text at all — never something this writer produced.
        std::fs::write(&marker, [0xff_u8, 0xfe, 0xff]).unwrap();
        assert!(!build_is_complete(&build), "a non-UTF-8 marker is not `ok`");
        // A marker torn mid-word is still honest about line 1.
        std::fs::write(&marker, b"o").unwrap();
        assert!(!build_is_complete(&build), "a torn marker is not `ok`");

        // NON-VACUITY, both directions: the real writer's marker vouches, and so does the
        // legacy bare `ok` a pre-platform-record version left behind.
        mark_build_ready(&build).unwrap();
        assert!(build_is_complete(&build));
        std::fs::write(&marker, b"ok\n").unwrap();
        assert!(build_is_complete(&build));
        let _ = std::fs::remove_dir_all(&home);
    }

    /// THE STAGED TREE IS FLUSHED BEFORE IT IS PUBLISHED. The walk has to reach every
    /// regular file and every directory — a subset would leave exactly the files the
    /// swap's renames then vouch for unflushed — and must never follow a symlink out of
    /// the tree it was handed.
    #[test]
    fn sync_tree_flushes_every_file_and_directory_it_walks() {
        let home = temp_home("synctree");
        let root = home.join("18.incoming-1");
        std::fs::create_dir_all(root.join("bin")).unwrap();
        std::fs::create_dir_all(root.join("lib").join("nested")).unwrap();
        std::fs::write(root.join("bin").join("ay"), b"#!/bin/true\n").unwrap();
        std::fs::write(root.join("lib").join("a.rlib"), b"payload").unwrap();
        std::fs::write(root.join("lib").join("nested").join("b.rlib"), b"deep").unwrap();
        #[cfg(unix)]
        std::os::unix::fs::symlink("../lib/a.rlib", root.join("bin").join("link")).unwrap();

        let synced = sync_tree(&root).unwrap();
        assert_eq!(
            synced.files, 3,
            "every regular file, and the symlink is not one"
        );
        assert_eq!(synced.dirs, 4, "the root, bin, lib and lib/nested");

        // A tree that is not there is an ERROR, not a quiet success: this runs on the path
        // an install is about to publish, so "nothing to flush" must never read as
        // "flushed".
        assert!(sync_tree(&home.join("absent")).is_err());
        let _ = std::fs::remove_dir_all(&home);
    }

    #[test]
    fn append_bin_to_path_appends_never_prepends() {
        let bin = Path::new("/p/bin");
        // Inputs/expectations built with join_paths so the platform separator (':' Unix,
        // ';' Windows) is exercised, not hard-coded.
        let inherited = std::env::join_paths([Path::new("/usr/bin"), Path::new("/bin")]).unwrap();
        // bin_dir lands at the END (so it can't shadow system commands earlier on PATH).
        let out = append_bin_to_path(Some(&inherited), bin);
        assert_eq!(
            out,
            std::env::join_paths([Path::new("/usr/bin"), Path::new("/bin"), bin]).unwrap()
        );
        // No inherited PATH → just the managed bin.
        assert_eq!(append_bin_to_path(None, bin), OsString::from("/p/bin"));
        assert_eq!(
            append_bin_to_path(Some(OsStr::new("")), bin),
            OsString::from("/p/bin")
        );
    }

    #[test]
    fn append_bin_to_path_is_idempotent() {
        let bin = Path::new("/p/bin");
        // Already present (anywhere) → returned unchanged, never duplicated.
        let bin_first = std::env::join_paths([Path::new("/p/bin"), Path::new("/usr/bin")]).unwrap();
        assert_eq!(append_bin_to_path(Some(&bin_first), bin), bin_first);
        let bin_last = std::env::join_paths([Path::new("/usr/bin"), Path::new("/p/bin")]).unwrap();
        assert_eq!(append_bin_to_path(Some(&bin_last), bin), bin_last);
    }
}
