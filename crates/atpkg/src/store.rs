// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

//! The manager-owned install layout (§10): a hardened prefix under `$HOME`, the
//! per-program store, the `bin/` shim dir, per-program staging, the durable floor, and
//! the aggregate status file.
//!
//! Two trust-bearing decisions live here, both fail-closed:
//!
//! * Prefix validation checks the whole chain, not just the leaf: `prefix` is
//!   config-controlled (§11), and a prefix under an attacker-writable parent reopens a
//!   CWE-379 symlink-swap window. [`resolve`] admits exactly two chain shapes, never a
//!   mixture and never with `..`; anything else falls back to the default prefix.
//!
//!   1. A `$HOME` prefix: strictly under `$HOME`, every existing directory from `$HOME`
//!      down owned by the uid, not group/other-writable, and not a symlink.
//!   2. A system prefix: outside `$HOME`, every existing directory from `/` down owned by
//!      root, not group/other-writable, and not a symlink.
//!
//!   The system shape is for a store several accounts share, or for
//!   `TRUST_REQUIRE_SEALED_LAUNCHER=1`; writing there requires root, which is checked. One
//!   world-writable ancestor (`/private/tmp`, say) disqualifies either shape. The verified
//!   lane does not need it: Trust's default `CallerOwned` mode accepts a component owned by
//!   root or the invoking identity, so the default `$HOME` prefix proves fine, and targo
//!   refuses to run as root (`docs/GOLDEN-INSTALL-PATH.md` §2).
//! * Shim names that collide with sensitive commands (`sudo`, `ssh`, `git`, …) never get a
//!   shim ([`shim_allowed`]), even though `bin/` is appended to the child `PATH`, never
//!   prepended.

use std::ffi::{OsStr, OsString};
use std::path::{Component, Path, PathBuf};

use crate::manifest::DigestAlg;

/// The resolved, validated install layout. All paths are absolute and under a prefix
/// that passed the [`resolve`] chain check.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Layout {
    /// The manager prefix, e.g. `~/Library/Application Support/aterm/pkg`.
    pub prefix: PathBuf,
}

impl Layout {
    /// Whether this is a system prefix (the root-owned shape) rather than a `$HOME` one,
    /// decided by the on-disk chain ([`system_chain_trusted`]), not by location: a `Layout`
    /// built directly (tests do) skips [`vet_prefix`], and "outside `$HOME`" would then
    /// publish a user-owned temp dir `0755`.
    #[must_use]
    pub fn is_system_prefix(&self) -> bool {
        #[cfg(test)]
        if system_shape::forced(&self.prefix) {
            return true;
        }
        system_chain_trusted(&self.prefix)
    }

    /// Create and harden a directory of this layout with the mode the prefix shape calls
    /// for: `0700` in a `$HOME` prefix (private state), `0755` in a system prefix, which
    /// every user must traverse and execute from (a `0700` system prefix passes every
    /// ownership check, then fails the first non-root run with `Permission denied`). This
    /// decides who may read and traverse, never who may write: the prefix chain check
    /// guards the write.
    pub fn ensure_dir(&self, dir: &Path) -> std::io::Result<()> {
        let created = if self.is_system_prefix() {
            crate::platform::ensure_shared_dir(dir)
        } else {
            crate::platform::ensure_private_dir(dir)
        };
        // Keep the toolchain out of backups: the store holds GBs of re-downloadable,
        // signature-verifiable binaries under Application Support (not Caches: it must
        // survive a purge, and the verified lane needs a stable path), so nothing excludes
        // it by default. Prefix only, best-effort: a storage courtesy, never correctness.
        if dir == self.prefix {
            crate::platform::exclude_from_backup(dir);
        }
        created
    }

    /// `agents/` ([`Self::agents_dir`]) ensured as a real directory, before the `aterm` front
    /// door hands it to a TTY session ([`crate::reroute::AGENTS_DIR_ENV`]) and the window
    /// (`aterm-gui::spawn::managed_agents_dir`) puts it on a session's PATH, so the twins
    /// atpkg lays later ([`crate::activate::reconcile_agents`]) are found. One `mkdir`
    /// through [`Self::ensure_dir`], never a wait; an existing directory is left as it is. A
    /// symlink or regular file at `agents/` is refused and left alone (a pre-created link
    /// must never capture the twins), judged by `lstat` before the `mkdir`. `Ok` is the
    /// absolute directory; `Err` is the sentence for the caller's stderr line
    /// ([`agents_dir_refusal_line`]), always starting with the path, and the launch then
    /// leaves the directory off PATH.
    pub fn ensure_agents_dir(&self) -> Result<PathBuf, String> {
        let dir = self.agents_dir();
        match std::fs::symlink_metadata(&dir) {
            Ok(md) if md.file_type().is_symlink() => {
                return Err(format!("{} is a symlink", dir.display()));
            }
            Ok(md) if !md.is_dir() => {
                return Err(format!("{} exists and is not a directory", dir.display()));
            }
            _ => {}
        }
        // The bare io error names no path; the caller's stderr line must.
        // `ensure_private_dir`'s own refusals (a link swapped in after the lstat) already
        // start with it and are not prefixed twice.
        self.ensure_dir(&dir).map_err(|error| {
            let text = error.to_string();
            if text.starts_with(&dir.display().to_string()) {
                text
            } else {
                format!("{}: {text}", dir.display())
            }
        })?;
        match std::fs::symlink_metadata(&dir) {
            Ok(md) if md.file_type().is_symlink() => Err(format!("{} is a symlink", dir.display())),
            Ok(md) if md.is_dir() => Ok(dir),
            Ok(_) => Err(format!("{} exists and is not a directory", dir.display())),
            Err(error) => Err(format!("{}: {error}", dir.display())),
        }
    }

    /// `store/<program>/<build>/` — the versioned, immutable extracted tree.
    #[inline]
    #[must_use]
    pub fn build_dir(&self, program: &str, build: u64) -> PathBuf {
        aterm_toolchain::layout::build_dir(&self.prefix, program, build)
    }

    /// `bin/` — the only directory placed on the child `PATH` (shims into `current`).
    #[inline]
    #[must_use]
    pub fn bin_dir(&self) -> PathBuf {
        self.prefix.join("bin")
    }

    /// `reroute/`: the session-scoped directory of upstream-name stubs ([`crate::reroute`]),
    /// put first only on the PATH aterm hands its own children, and by the rc hook inside an
    /// aterm session ([`crate::hooks`]). Never `bin/`, so [`shim_allowed`]'s deny-list keeps
    /// its meaning for the managed `bin/`.
    #[inline]
    #[must_use]
    pub fn reroute_dir(&self) -> PathBuf {
        crate::reroute::dir(self)
    }

    /// `agents/`: the one managed directory that goes first on `PATH` (the shell hook
    /// prepends it, [`crate::hooks`]; the GUI's spawn seam front-inserts it beside
    /// `reroute/`). It holds only the agent programs' shims
    /// ([`crate::stub::AGENT_PROGRAMS`]), each forwarding exactly where its `bin/` twin does
    /// ([`crate::activate::reconcile_agents`]), so `claude`/`codex` run the managed copy
    /// ahead of a vendor's native install or a brew cask. Every other managed tool stays in
    /// `bin/`, appended last, so [`shim_allowed`]'s deny-list keeps its meaning.
    #[inline]
    #[must_use]
    pub fn agents_dir(&self) -> PathBuf {
        self.prefix.join("agents")
    }

    /// `agents/<tool>` — an agent program's front-of-`PATH` shim ([`Self::agents_dir`]);
    /// the same [`ToolName`] gate as [`Self::shim`].
    #[inline]
    #[must_use]
    pub fn agent_shim(&self, tool: &ToolName) -> PathBuf {
        self.agents_dir().join(tool.shim_file())
    }

    /// `landing/`: where older passes wrote landing markers, which twins those clients laid
    /// still test for ([`crate::landing`]). No pass writes one now; every pass removes the
    /// directory ([`crate::landing::sweep`]).
    #[inline]
    #[must_use]
    pub fn landing_dir(&self) -> PathBuf {
        self.prefix.join("landing")
    }

    /// `bin/<tool>`: a single shim, named by [`ToolName::shim_file`] (`bin/ay` on Unix,
    /// `bin/ay.cmd` on Windows). Taking a [`ToolName`] makes the [`shim_allowed`] gate
    /// unskippable: no file in `bin/` can be named without [`ToolName::new`].
    #[inline]
    #[must_use]
    pub fn shim(&self, tool: &ToolName) -> PathBuf {
        self.bin_dir().join(tool.shim_file())
    }

    /// `store/<program>/current`: the per-program active-build symlink to
    /// `store/<program>/<build>/`, the one activation link ([`crate::gc::live_builds`]
    /// resolves it). Inside `store/<program>/`, so there is one per program, `uninstall`'s
    /// `remove_dir_all` of the program tree takes it with its builds, and it never collides
    /// with a build dir (`current` is not a `u64`; [`crate::ops::list_installed`] skips it).
    #[inline]
    #[must_use]
    pub fn program_current(&self, program: &str) -> PathBuf {
        self.prefix.join("store").join(program).join("current")
    }

    /// `staging/<program>/` — the per-program download + stage scratch.
    #[inline]
    #[must_use]
    pub fn staging_dir(&self, program: &str) -> PathBuf {
        self.prefix.join("staging").join(program)
    }

    /// `floor` — the `0600` durable high-water `index_build` file (§8).
    ///
    /// Read together with [`Self::floor_generation`]; the pair is one value
    /// ([`crate::sig::BuildFloor`]), because `index_build` is a number a machine chooses and
    /// a floor set by a machine must not outlive the roster generation that revoked it.
    #[inline]
    #[must_use]
    pub fn floor(&self) -> PathBuf {
        self.prefix.join("floor")
    }

    /// `floor.gen`: the `0600` roster generation that recorded the current [`Self::floor`].
    ///
    /// Its own file, so `floor` stays a bare integer and an older store reads as "floor from
    /// generation 0", which the strictly-newer rule re-bases on first contact. Missing or
    /// unreadable reads as `0`, a generation no real roster carries: the fail-closed
    /// direction, which never waives a floor for the generation that set it.
    #[inline]
    #[must_use]
    pub fn floor_generation(&self) -> PathBuf {
        self.prefix.join("floor.gen")
    }

    /// `roster.floor`: the `0600` durable high-water `roster_seq`, the replay ratchet for the
    /// master-signed machine roster that authorizes this store's index.
    ///
    /// Separate from [`Self::floor`] because the counters move independently (minting or
    /// revoking a machine bumps `roster_seq` without re-cutting the index, and vice versa).
    /// atpkg's own rather than shared with `aterm-update`'s ratchet: the prefixes have
    /// different ownership and lifetimes, and uninstalling the store must not reset the app
    /// updater's replay defence. Each ratchet refuses only what it has itself seen superseded.
    #[inline]
    #[must_use]
    pub fn roster_floor(&self) -> PathBuf {
        self.prefix.join("roster.floor")
    }

    /// `store.lock` — the `0600` store-wide single-writer advisory lock file
    /// ([`crate::lock`]). Try-acquired at the CLI edge by every verb that mutates the
    /// store, so exactly one process at a time stages/activates/discards builds here.
    #[inline]
    #[must_use]
    pub fn store_lock(&self) -> PathBuf {
        self.prefix.join("store.lock")
    }

    /// `status.toml` — the aggregate observability record.
    #[inline]
    #[must_use]
    pub fn status(&self) -> PathBuf {
        self.prefix.join("status.toml")
    }

    /// `machine-apply.stamp` — when a launch last started `aterm pkg machine apply` (unix
    /// seconds, one line): a terminal session and a window opening claim it once a day
    /// between them ([`crate::machine::launch_apply_due`]) instead of walking `$HOME` at
    /// every launch.
    #[inline]
    #[must_use]
    pub fn machine_apply_stamp(&self) -> PathBuf {
        self.prefix.join("machine-apply.stamp")
    }

    /// `session-pass.stamp` — when a terminal session last spawned its detached `aterm pkg
    /// update` (unix seconds, one line): claimed before the spawn, so tabs opened together
    /// start one pass, not one each.
    #[inline]
    #[must_use]
    pub fn session_pass_stamp(&self) -> PathBuf {
        self.prefix.join("session-pass.stamp")
    }

    /// `progress.json` — the live install-progress snapshot ([`crate::progress`]), the
    /// in-flight complement to [`Self::status`]'s durable per-pass record. Written only
    /// by the process holding the store flock (via `--progress-file`); read by the GUI
    /// card, the pending-program stubs (`atpkg __pending`), and anyone tailing it —
    /// all under the untrusted-reader rules the progress module documents.
    #[inline]
    #[must_use]
    pub fn progress_file(&self) -> PathBuf {
        self.prefix.join("progress.json")
    }

    /// `bump` — the priority-queue channel ([`crate::progress`]): one program name per
    /// line, appended by pending-program stubs, consumed by the installer between
    /// items. Reorder-only by construction — the installer intersects it with the work
    /// the signed index already planned, so this file can never add work.
    #[inline]
    #[must_use]
    pub fn bump_file(&self) -> PathBuf {
        self.prefix.join("bump")
    }

    /// `pass-owed.json` — the promise that a default-set pass is coming and has not
    /// planned yet ([`crate::progress::pass_owed`]): written under the store flock by the
    /// window's first-run seed and by a whole update, read by the pending-program stubs,
    /// so a stub run before `progress.json` exists waits for the pass instead of exiting.
    #[inline]
    #[must_use]
    pub fn pass_owed_file(&self) -> PathBuf {
        self.prefix.join("pass-owed.json")
    }

    /// `adopted`: the durable marker that this machine runs the ALab toolset as a set, not
    /// as a handful of individually chosen programs (§11). Written when the whole default set
    /// is laid down deliberately (the first-run seed bootstrap, or `install --default-set`),
    /// never by `install <program>`.
    ///
    /// Consent to pull a multi-GB toolchain (`[packages].auto_install`) is given once; while
    /// adopted, the update pass keeps the set complete, so a program published after install
    /// arrives (`cli::should_complete_set`). Cleared by `uninstall`, so set-completion never
    /// reinstalls what was removed; `[packages].exclude` drops one program while staying
    /// adopted. Existence-only; who adopted is [`Self::adopted_by_seed`].
    #[inline]
    #[must_use]
    pub fn adopted(&self) -> PathBuf {
        self.prefix.join("adopted")
    }

    /// `adopted-by-seed`: present only when `atpkg seed`, the first launch's bootstrap,
    /// created the [`Self::adopted`] marker itself. Existence only. Only beside it may an
    /// update pass say "installing aterm is the consent" and name `[packages].auto_install =
    /// false` as the switch that would have stopped it. Cleared with `adopted`;
    /// `install --default-set` never writes one.
    #[inline]
    #[must_use]
    pub fn adopted_by_seed(&self) -> PathBuf {
        self.prefix.join("adopted-by-seed")
    }

    /// `removed`: programs the user uninstalled individually, one name per line. Without it
    /// the resumable seed lane, which installs whatever the store lacks, would reinstall a
    /// targeted removal on the next launch. An explicit `install <program>` or
    /// `install --default-set` clears the relevant entries.
    #[inline]
    #[must_use]
    pub fn removed(&self) -> PathBuf {
        self.prefix.join("removed")
    }

    /// The programs this machine removed on purpose. Here rather than in the CLI because the
    /// unattended lanes need it too: a coherence group pulls a missing sibling back in to
    /// keep the tuple locked, which would re-download a program the user just uninstalled.
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

    /// `declined`: the durable "this machine does not want the bundled toolset" marker,
    /// written by `uninstall --all`. Without it the seed lane, which installs whatever
    /// channel-pinned members the store lacks on every launch, would bring the toolset back;
    /// `adopted` cannot carry it, since its absence means "never adopted", the state a first
    /// run installs from. Cleared by any explicit install (`install --default-set`,
    /// `install <program>`).
    #[inline]
    #[must_use]
    pub fn declined(&self) -> PathBuf {
        self.prefix.join("declined")
    }

    /// `retired/`: per-program retirement markers. One `0600` regular file per program whose
    /// managed copy atpkg retired for a system install (`cli::satisfy_by_system`); its first
    /// line is the day (`YYYY-MM-DD`), which the `system: <path> — not managed by aterm
    /// (managed copy retired <date>)` state and `atpkg which` read back. Cleared when the
    /// program is uninstalled on purpose.
    #[inline]
    #[must_use]
    pub fn retired_dir(&self) -> PathBuf {
        self.prefix.join("retired")
    }

    /// `retired/<program>`: one retirement marker; its callers gate the name by [`ToolName`].
    #[inline]
    #[must_use]
    pub fn retired_marker(&self, program: &str) -> PathBuf {
        self.retired_dir().join(program)
    }

    /// The `YYYY-MM-DD` day a managed copy of `program` was retired for a system install,
    /// if recorded: the trimmed first line of a regular, non-symlink marker (a planted link
    /// is not a record), only when it has a date's shape; a scribbled-over marker is `None`.
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

    /// Forget every retirement record (`uninstall --all`).
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
    /// dev-linked program; its presence makes `update`/`apply` skip that program outright.
    #[inline]
    #[must_use]
    pub fn links_dir(&self) -> PathBuf {
        self.prefix.join("links")
    }

    /// `links/<program>` — one dev-link marker. Only ever joined with a
    /// [`shim_allowed`]-shape program name (linkmode gates the name before calling).
    #[inline]
    #[must_use]
    pub fn link_marker(&self, program: &str) -> PathBuf {
        self.links_dir().join(program)
    }
}

/// The one stderr line for a refused `agents/` ([`Layout::ensure_agents_dir`]), shared by
/// the front door and the window. `reach` is who goes without the managed agents ("this
/// session", "this window's sessions"); `error` already starts with the path. A symlink or
/// regular file at `agents/` must be removed first, since `aterm pkg repair` refuses the
/// same entry (`activate::reconcile_agents`) and naming repair alone would loop; anything
/// else is what `repair` re-lays, as root when `system` says the prefix is root-owned.
#[must_use]
pub fn agents_dir_refusal_line(dir: &Path, error: &str, system: bool, reach: &str) -> String {
    let remedy = match std::fs::symlink_metadata(dir) {
        Ok(md) if md.file_type().is_symlink() => "remove that symlink, then run `aterm pkg repair`",
        Ok(md) if !md.is_dir() => "remove that file, then run `aterm pkg repair`",
        _ if system => "run `aterm pkg repair` as root",
        _ => "run `aterm pkg repair`",
    };
    format!(
        "aterm: the managed `claude`/`codex` are not first on PATH in {reach}: {error} — {remedy}"
    )
}

/// The per-build completeness marker: a sibling `store/<program>/<build>.ready`, outside
/// the build tree so it never perturbs the build's `tree_root`. Written last by
/// `verify_and_stage`, once the extracted tree passed sha256 and `tree_root` re-verify; it
/// is the sole "fully installed" signal, so a build a crash left partial reads as absent
/// and is re-installed. `None` if `build_dir` has no final component.
fn ready_marker_path(build_dir: &Path) -> Option<PathBuf> {
    let name = build_dir.file_name()?.to_str()?;
    Some(build_dir.with_file_name(format!("{name}.ready")))
}

/// The key under which the readiness marker records which slice installed the build.
/// Its spelling is a compatibility surface, not a detail — see [`ready_text_accepts`].
const READY_PLATFORM_KEY: &str = "platform=";

/// The key under which the readiness marker records what the build contained when marked:
/// the direct entries of its `bin/`, which every shim and exec root forward to. Its
/// spelling is a compatibility surface: a re-spelling demotes every marker this version
/// wrote to "no record", which is accepted unchecked ([`ready_text_accepts`]).
///
/// A marker beside a gutted tree would be counted as installed
/// ([`crate::ops::list_installed`]), spared by [`crate::gc`] (it sweeps only marker-less
/// trees) and selected by [`crate::flow::rollback`]. [`discard_build`] and
/// [`crate::ops::uninstall`] take the marker down before the tree; this key lets the disk
/// refute a marker however a tree lost its contents (an interrupted `rm -rf`, a volume
/// that dropped a directory, an older manager).
///
/// * `contents=none`: the build had no `bin/` entries when marked; nothing to check.
/// * `contents=bin:<name>,<name>,…`: the sorted names of `bin/`'s direct entries, each of
///   which must still be there ([`contents_still_stand`]).
///
/// The key is omitted (the marker as lenient as an older one) when the inventory cannot be
/// encoded: a `bin/` unreadable for a reason other than `NotFound`, or a name carrying a
/// `,` or a newline.
const READY_CONTENTS_KEY: &str = "contents=";

/// [`READY_CONTENTS_KEY`]'s value for a build whose `bin/` held nothing (or is absent).
/// A recorded emptiness, distinct from the absence of a record.
const READY_CONTENTS_NONE: &str = "none";

/// [`READY_CONTENTS_KEY`]'s prefix for a recorded `bin/` inventory: `bin:<name>,<name>,…`.
const READY_CONTENTS_BIN: &str = "bin:";

/// Line 1 of every readiness marker atpkg has written, and the well-formedness gate a
/// marker must pass before [`ready_text_accepts`] reads anything else: bytes without it are
/// a crash artefact, not a legacy marker. Shared by the writer ([`mark_build_ready`]) and
/// the reader.
const READY_OK_LINE: &str = "ok";

/// `<arch>-<os>` for the running atpkg slice: `aarch64-macos`, `x86_64-macos`,
/// `x86_64-linux`, …
///
/// The store path carries no architecture, and the shipped atpkg is a universal binary:
/// its x86_64 slice (via `arch -x86_64`, an x86_64 parent shell, or "Open using Rosetta")
/// selects `x86_64-apple-darwin` rows (`cli::current_triple()` is compile-time). Without a
/// record of who wrote it, an Intel build in `store/<program>/<build>/` would satisfy every
/// later native run. So the marker records this value and [`build_is_complete`] refuses a
/// mismatch: the build reads as not installed and is re-staged natively (a re-download,
/// never an error). The apply decision reads the marker too: `flow::installed_for_decide`
/// drops a live build this predicate refuses before [`crate::gate::decide`] sees it.
///
/// `std::env::consts`, not the artifact triple: std exposes no triple, and a duplicated
/// copy of `cli::current_triple`'s `cfg` ladder that drifted would read as a mismatch on
/// every machine. Both consts are per-slice at compile time, which is all the comparison
/// needs.
fn running_platform() -> String {
    format!("{}-{}", std::env::consts::ARCH, std::env::consts::OS)
}

/// The platform a marker's text records, or `None` when it records none (a bare `ok\n`
/// from before the field, or an empty value).
fn recorded_platform(text: &str) -> Option<&str> {
    text.lines()
        .filter_map(|line| line.trim().strip_prefix(READY_PLATFORM_KEY))
        .map(str::trim)
        .find(|p| !p.is_empty())
}

/// The `contents=` record a marker's text carries, or `None` when it carries none (an
/// older marker, or an empty value).
fn recorded_contents(text: &str) -> Option<&str> {
    text.lines()
        .filter_map(|line| line.trim().strip_prefix(READY_CONTENTS_KEY))
        .map(str::trim)
        .find(|v| !v.is_empty())
}

/// The inventory [`mark_build_ready`] records for `build_dir`, or `None` when it cannot be
/// encoded ([`READY_CONTENTS_KEY`]). Names are `bin/`'s direct entries by their own names
/// (a `bin/clippy -> ../lib/…` link is recorded as `clippy`), sorted so the record is a
/// function of the tree, not of `readdir` order.
fn bin_inventory(build_dir: &Path) -> Option<String> {
    let bin = build_dir.join("bin");
    let entries = match std::fs::read_dir(&bin) {
        Ok(entries) => entries,
        // A missing `bin/` is a fact about the build, recorded as `none`.
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
            return Some(String::from(READY_CONTENTS_NONE));
        }
        // Anything else (EACCES, EIO, macOS privacy consent) bounds what this process may
        // know; recording `none` would unprotect the build, so the key is omitted.
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
    Some(format!("{READY_CONTENTS_BIN}{}", names.join(",")))
}

/// Whether the tree at `build_dir` still holds what its marker's `contents=` record vouches
/// for. Only a proven absence refutes: each name is `symlink_metadata`'d, not `exists()`'d,
/// so an EACCES or privacy-consent answer never re-stages a multi-GB toolchain, and a
/// `bin/` symlink into the build's own `lib/` is present as itself (a dangling link is
/// [`crate::doctor`]'s to report). An unrecognised value (a later atpkg's) reads as no
/// record, never as refuted.
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

/// Whether `path` is provably not there, by its own link (never by its target).
/// The `symlink_metadata` twin of [`Presence::is_absent`].
fn is_provably_absent(path: &Path) -> bool {
    matches!(std::fs::symlink_metadata(path), Err(e) if e.kind() == std::io::ErrorKind::NotFound)
}

/// Whether a readiness marker with this text vouches for its build to a `running` slice.
///
/// An absent platform record means accept: older markers are a bare `ok\n`, and reading
/// them as a mismatch would re-download every installed program on every machine. The
/// record is acquired lazily, by the next install or update of a program. So
/// [`READY_PLATFORM_KEY`]'s spelling is a compatibility surface: a re-spelling must keep
/// reading the old key, or it unprotects every marker written with it.
///
/// An absent record is not an absent marker: [`READY_OK_LINE`] is required.
/// `<build>.ready` is published by a rename, which commits the name while the bytes may
/// still be page cache, so a power loss can leave an empty or torn marker; vouching for it
/// would leave a build nothing downstream repairs ([`crate::gate::decide`] answers
/// `UpToDate`). Such a marker is refused and the build re-stages.
fn ready_text_accepts(text: &str, running: &str) -> bool {
    if !first_line_is_ok(text) {
        return false;
    }
    match recorded_platform(text) {
        None => true,
        Some(recorded) => recorded == running,
    }
}

/// Whether a well-formed marker's contents record (if any) is still true of `build_dir`.
/// Split out of [`ready_text_accepts`] because it is the one clause that reads the disk;
/// the rest of that predicate is a function of the text alone and its tests say so.
fn ready_contents_accept(text: &str, build_dir: &Path) -> bool {
    match recorded_contents(text) {
        None => true,
        Some(recorded) => contents_still_stand(build_dir, recorded),
    }
}

/// Whether a marker's first line is [`READY_OK_LINE`], ignoring surrounding whitespace (a
/// `\r`) and a missing trailing newline.
fn first_line_is_ok(text: &str) -> bool {
    matches!(text.lines().next(), Some(first) if first.trim() == READY_OK_LINE)
}

/// What this process could learn about whether `path` is there: three answers.
///
/// [`Path::exists`] is `fs::metadata(..).is_ok()`, so every reason a stat can fail (EACCES
/// on a parent, EPERM from macOS privacy consent, EIO, ELOOP, ENAMETOOLONG) reads as "not
/// there", turning a bound on what this process may know into a fact about the world: a
/// live program would drop out of [`crate::ops::active_builds`], doctor would call a
/// working shim broken, and [`caller_can_use_prefix`] would abandon a shared store, each
/// implying a reinstall that repairs nothing. [`Presence::Unknown`] carries the error so a
/// diagnostic can say why it cannot tell; `Absent` is only what a stat proves.
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
    /// True only when the path is provably not there. An `Unknown` answers `false`
    /// here on purpose: a caller asking "may I act as though this is gone?" must be
    /// told no when nobody looked.
    #[inline]
    #[must_use]
    pub const fn is_absent(&self) -> bool {
        matches!(self, Self::Absent)
    }
}

/// Ask the filesystem about `path`, keeping the three answers apart. Follows symlinks, as
/// [`Path::exists`] does, so a dangling link is `Absent`.
#[must_use]
pub fn presence(path: &Path) -> Presence {
    match std::fs::metadata(path) {
        Ok(_) => Presence::Present,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Presence::Absent,
        Err(e) => Presence::Unknown(e),
    }
}

/// Whether `build_dir` holds a complete install for the running slice: the tree is not
/// provably absent, its sibling marker is well formed, and the platform and `bin/` contents
/// it records, if any, still hold. A marker naming the other slice reads as not complete,
/// so the build is re-staged from the row `cli::current_triple()` selects
/// ([`running_platform`]).
#[must_use]
pub fn build_is_complete(build_dir: &Path) -> bool {
    let Some(marker) = ready_marker_path(build_dir) else {
        return false;
    };
    // The tree itself first: [`mark_build_ready`] never looks at `<build>`, so a marker can
    // exist for a tree that does not. Only a proven absence refuses ([`presence`]): an
    // EACCES on a shared prefix must never un-install a build.
    if is_provably_absent(build_dir) {
        return false;
    }
    match std::fs::read_to_string(&marker) {
        Ok(text) => {
            ready_text_accepts(&text, &running_platform())
                && ready_contents_accept(&text, build_dir)
        }
        Err(e) if e.kind() == std::io::ErrorKind::InvalidData => false,
        // `InvalidData` (not UTF-8): never written here, since every marker is ASCII, so
        // it is the crash artefact the `ok`-line rule refuses. Any other error but
        // `NotFound` (a directory planted at the marker path, a permissions oddity) says
        // nothing about content, so it reads as complete, as a plain existence check would.
        Err(e) if e.kind() != std::io::ErrorKind::NotFound => true,
        Err(_) => false,
    }
}

/// Whether `build_dir` carries a well-formed marker recording the other slice of the
/// universal binary: a finished install, complete for that slice. [`build_is_complete`]
/// answers `false` here too, and [`crate::gc`]'s interrupted-install sweep must not read
/// that as "never finished" and remove it. As strict as the acceptance rule: an `ok` first
/// line, a platform record present, and not ours.
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

/// Atomically and durably mark `build_dir` complete (temp + fsync + rename, so a crash
/// leaves no marker rather than a half-written one). Call as the last staging step.
///
/// The fsync matters: a rename publishes the name while the bytes may sit in page cache,
/// so a filesystem that commits metadata first could expose a zero-length `<build>.ready`
/// after a power loss. (A volume that cannot flush is accepted:
/// [`sync_contents_or_accept_refusal`].) The text records which slice installed the build
/// ([`running_platform`]) and its `bin/` inventory; line 1 stays `ok`.
pub fn mark_build_ready(build_dir: &Path) -> std::io::Result<()> {
    let dest = ready_marker_path(build_dir).ok_or_else(|| {
        std::io::Error::new(std::io::ErrorKind::InvalidInput, "build dir has no name")
    })?;
    let parent = dest.parent().unwrap_or(build_dir);
    let tmp = parent.join(format!(".ready.tmp-{}", std::process::id()));
    let mut body = format!(
        "{READY_OK_LINE}\n{READY_PLATFORM_KEY}{}\n",
        running_platform()
    );
    // What this marker vouches for ([`READY_CONTENTS_KEY`]), derived from the tree here,
    // not passed in, so no caller can assert a completeness it did not measure.
    if let Some(contents) = bin_inventory(build_dir) {
        body.push_str(READY_CONTENTS_KEY);
        body.push_str(&contents);
        body.push('\n');
    }
    {
        use std::io::Write as _;
        // `0644` explicitly, not umask-dependent: under a system prefix this file is
        // root-owned and every unprivileged reader must read it.
        let mut f = crate::platform::open_create_write(&tmp, 0o644)?;
        f.write_all(body.as_bytes())?;
        // Inside the braces: on stable storage before the handle drops, and before the
        // rename below publishes the name.
        sync_contents_or_accept_refusal(&f)?;
    }
    std::fs::rename(&tmp, &dest)
}

/// Remove the completeness marker, making `build_dir` read as not installed. Called first
/// when a stage replaces a live tree, so a crash mid-swap leaves it re-installable. A
/// missing marker is already the wanted state.
pub(crate) fn clear_build_ready(build_dir: &Path) -> std::io::Result<()> {
    let Some(marker) = ready_marker_path(build_dir) else {
        return Ok(());
    };
    match std::fs::remove_file(&marker) {
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(()),
        other => other,
    }
}

/// Take down the completeness marker of every build under `prog_store`
/// (`<prefix>/store/<program>`), durably, before a caller removes the tree.
///
/// The whole-program twin of the unmark that opens [`discard_build`]:
/// [`crate::ops::uninstall`]'s `remove_dir_all` of a program's store is long (several GB
/// for `trust`) and `readdir` order decides what goes first, so an interrupt would leave
/// build dirs still vouched for by their `<n>.ready`. Unmarked, they read as incomplete
/// debris that `gc` sweeps and `list_installed`, `decide` and `rollback` ignore.
/// Best-effort (a marker that cannot be unlinked must not stop the uninstall), with one
/// `sync_dir` at the end: the ordering is "every marker gone before any tree goes".
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
        // Ordered: every marker is gone before any tree goes; a lost removal leaves a
        // marked tree for the next uninstall to unmark again.
        sync_dir_ordered(prog_store);
    }
}

/// The suffix of the declared-mode record beside a build dir: `store/<program>/<build>.modes`.
///
/// A sibling like `<build>.ready`, so it never perturbs the build's `tree_root`. It lists
/// the permission bits the stage declared for every regular file (the mode slot of each
/// signed `tree_root` line), for Windows, whose filesystem reads every mode back as `0`:
/// without it an on-disk walk there (`atpkg verify`) could agree with no signed root. On
/// Unix the inode stores the bits, so [`write_declared_modes`] is a no-op and
/// [`declared_modes`] answers empty.
pub const DECLARED_MODES_SUFFIX: &str = ".modes";

/// The record's first line, the schema gate a reader refuses anything else on.
const DECLARED_MODES_HEADER: &str = "declared-modes v1";

/// Upper bound on a declared-mode record (one line per regular file; the `trust` sysroot
/// lays about 4100): far above any real tree, and the read stays strictly finite.
const MAX_DECLARED_MODES_BYTES: usize = 16 * 1024 * 1024;

/// `store/<program>/<build>.modes` for `build_dir`, or `None` for a path with no name.
#[must_use]
pub fn declared_modes_path(build_dir: &Path) -> Option<PathBuf> {
    let name = build_dir.file_name()?;
    let name = name.to_str()?;
    let mut sidecar = String::from(name);
    sidecar.push_str(DECLARED_MODES_SUFFIX);
    Some(build_dir.with_file_name(sidecar))
}

/// Write the declared-mode record beside `build_dir` durably ([`write_sidecar_durably`]):
/// one `<relpath bytes> 0x00 <octal mode> 0x0A` line per regular file, the `tree_root`
/// line's framing minus its digest. A no-op where the filesystem stores permission bits
/// ([`crate::platform::HAS_POSIX_MODES`]). Called before the build is marked complete, so a
/// record that could not be written leaves an unmarked, re-stageable build.
///
/// # Errors
/// The record could not be written.
pub fn write_declared_modes(
    build_dir: &Path,
    modes: &crate::tree::DeclaredModes,
) -> std::io::Result<()> {
    if crate::platform::HAS_POSIX_MODES {
        return Ok(());
    }
    if let Some(parent) = build_dir.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let mut body: Vec<u8> = Vec::with_capacity(modes.len().saturating_mul(48).saturating_add(32));
    body.extend_from_slice(DECLARED_MODES_HEADER.as_bytes());
    body.push(b'\n');
    for (rel, mode) in modes {
        body.extend_from_slice(rel);
        body.push(0);
        // Octal, minimal digits — the very spelling the tree line carries (`0` for a
        // zero mode), so a record and a line never disagree on a number.
        body.extend_from_slice(crate::tree::oct_mode(*mode).as_bytes());
        body.push(b'\n');
    }
    write_sidecar_durably(build_dir, DECLARED_MODES_SUFFIX, &body)
}

/// The declared-mode record beside `build_dir`, for [`crate::tree::tree_root_declared`].
///
/// Where the filesystem stores permission bits ([`crate::platform::HAS_POSIX_MODES`]) this
/// answers empty without touching the disk. Elsewhere the record is required: a missing,
/// oversized, symlinked or malformed one is an error, never an empty map, which would fold
/// `0` for every file and report a healthy build as drifted.
///
/// # Errors
/// The record is absent (a build staged before this platform kept one; `aterm pkg install
/// <program>` re-stages it), unreadable, or malformed.
pub fn declared_modes(build_dir: &Path) -> std::io::Result<crate::tree::DeclaredModes> {
    if crate::platform::HAS_POSIX_MODES {
        return Ok(crate::tree::DeclaredModes::new());
    }
    let path = declared_modes_path(build_dir).ok_or_else(|| {
        std::io::Error::new(std::io::ErrorKind::InvalidInput, "build dir has no name")
    })?;
    let bytes =
        crate::metadata_io::read_bounded_regular(&path, MAX_DECLARED_MODES_BYTES).map_err(|e| {
            let mut m = String::from("no declared-mode record beside the build (");
            m.push_str(&e.to_string());
            m.push_str("): staged before this platform kept one; `aterm pkg install` re-stages it");
            std::io::Error::new(e.kind(), m)
        })?;
    parse_declared_modes(&bytes).ok_or_else(|| {
        std::io::Error::new(
            std::io::ErrorKind::InvalidData,
            "the declared-mode record beside the build is malformed",
        )
    })
}

/// [`declared_modes`]'s parser, split out so the schema gate and every malformed shape
/// are testable without a filesystem. Fail-closed to `None`.
fn parse_declared_modes(bytes: &[u8]) -> Option<crate::tree::DeclaredModes> {
    let mut lines = bytes.split(|&b| b == b'\n');
    if lines.next()? != DECLARED_MODES_HEADER.as_bytes() {
        return None;
    }
    let mut modes = crate::tree::DeclaredModes::new();
    for line in lines {
        if line.is_empty() {
            // The trailing newline's empty tail; an empty line anywhere else is
            // equally content-free.
            continue;
        }
        let nul = line.iter().position(|&b| b == 0)?;
        let (rel, digits) = (line.get(..nul)?, line.get(nul.saturating_add(1)..)?);
        if rel.is_empty() || digits.is_empty() || digits.len() > 4 {
            return None;
        }
        let mut mode: u32 = 0;
        for &d in digits {
            if !(b'0'..=b'7').contains(&d) {
                return None;
            }
            mode = mode.wrapping_shl(3) | u32::from(d.wrapping_sub(b'0'));
        }
        modes.insert(rel.to_vec(), mode);
    }
    Some(modes)
}

/// Remove the declared-mode record beside `build_dir`, if any.
pub fn clear_declared_modes(build_dir: &Path) {
    if let Some(sidecar) = declared_modes_path(build_dir) {
        let _ = std::fs::remove_file(sidecar);
    }
}

/// The suffix of the stage-refusal memo beside a build dir (`<build>.refused`).
pub const STAGE_REFUSAL_SUFFIX: &str = ".refused";

/// The memo's first line — the schema gate. A file that does not open with it is not
/// read at all, so a future shape can never be half-understood by this reader.
const STAGE_REFUSAL_HEADER: &str = "stage-refusal v1";

/// Bound on a memo read; it holds eight short lines at most.
const MAX_REFUSAL_BYTES: usize = 4 * 1024;

/// How long the second consecutive refusal of a build's signed digests holds the next
/// attempt off the wire (12 h, two of the GUI's six-hourly ticks), doubling per further
/// refusal up to [`REFUSAL_COOLDOWN_CAP_SECS`]. The first refusal holds nothing
/// ([`StageRefusal::cooldown_secs`]).
const REFUSAL_COOLDOWN_BASE_SECS: i64 = 12 * 60 * 60;

/// The ceiling on that doubling: one week. A pin whose signed digests do not match the
/// published asset is a publishing fact that only a new index can fix, and every attempt
/// to re-prove it costs the whole asset.
const REFUSAL_COOLDOWN_CAP_SECS: i64 = 7 * 24 * 60 * 60;

/// The longest `why` sentence a memo keeps — the stage's own words, truncated so the
/// memo cannot grow past a line or two.
const MAX_REFUSAL_WHY_CHARS: usize = 300;

/// What a deterministic stage failure recorded beside `store/<program>/<build>`: the
/// signed digests the published bytes did not match, the stage's own sentence, when, and
/// how many consecutive attempts proved the same thing. The payload digest is the row's
/// ([`crate::manifest::Artifact::digest`]), on a line named for its algorithm (`sha256=`
/// or `sha512=`).
///
/// While the pin and its digest hold still, a signed-digest mismatch keeps its answer, and
/// the failed archive (and any `.part`) is deleted, so without a memo every tick would
/// re-download the whole asset (~3.4 GB for `trust`) to the same verdict. The memo can
/// only skip a transfer: it is consulted after the signed manifest is verified, and never
/// admits bytes, relaxes a digest or keeps a build alive.
///
/// It holds a cooldown, not a verdict, because a publisher can repair the asset under the
/// same pin and a transfer can be truncated on the wire: the first is zero, then one
/// attempt per 12 h, 24, 48, … up to one a week, and any change to the pin's signed digest
/// or `tree_root` stops it binding at once.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StageRefusal {
    /// The algorithm of [`Self::digest`].
    pub alg: DigestAlg,
    /// The signed payload digest of the artifact this refusal is about.
    pub digest: String,
    /// The signed `tree_root` of that artifact (empty when the manifest carries none).
    pub tree_root: String,
    /// The stage's own sentence ([`crate::install::StageError`]'s `Display`).
    pub why: String,
    /// When it was recorded (Unix epoch second).
    pub at: i64,
    /// How many consecutive attempts have failed on these digests (>= 1).
    pub attempts: u32,
    /// The platform signer check refused bytes that matched these digests
    /// ([`crate::install::StageError::SignerRefused`]): a re-download can only fetch the
    /// same bytes, so the memo binds until the digests change, with no cooldown to lapse.
    pub signer: bool,
    /// Whether the row is held rather than cooled down: a `tree_root` mismatch. The archive
    /// passed the signed digest gate and extraction is deterministic, so no retry over the
    /// same archive and row can differ. A held memo binds until the pin's signed digests
    /// move or a person asks (`aterm pkg install <p>`, `aterm pkg update --retry`); the
    /// archive is kept beside it so that retry costs no download.
    pub held: bool,
    /// The atpkg version that reached the verdict ([`aterm_types::version::app_version`]).
    /// A memo binds only the client that wrote it: the verdict is also one by this
    /// extractor and fold, and a client-side fix ships as a new atpkg, which gets one
    /// attempt. Empty in an older memo, which binds nothing.
    pub client: String,
}

impl StageRefusal {
    /// The cooldown this record's attempt count buys: zero for a first refusal, then
    /// [`REFUSAL_COOLDOWN_BASE_SECS`] doubled per further consecutive refusal, capped at
    /// [`REFUSAL_COOLDOWN_CAP_SECS`]. The first retry is always free: a truncated transfer,
    /// a bad mirror or a meddling proxy gives the same verdict as a publishing slip and
    /// heals on the next attempt (`flow`'s
    /// `a_member_stage_failure_aborts_the_group_and_a_retry_heals_it`). Only a second
    /// identical verdict is evidence about the publication.
    #[inline]
    #[must_use]
    pub const fn cooldown_secs(&self) -> i64 {
        let Some(steps) = self.attempts.checked_sub(2) else {
            return 0;
        };
        let shift = if steps < 8 { steps } else { 8 };
        let scaled = REFUSAL_COOLDOWN_BASE_SECS.saturating_mul(1_i64 << shift);
        if scaled < REFUSAL_COOLDOWN_CAP_SECS {
            scaled
        } else {
            REFUSAL_COOLDOWN_CAP_SECS
        }
    }

    /// The epoch second at which a new attempt is due.
    #[inline]
    #[must_use]
    pub const fn retry_after(&self) -> i64 {
        self.at.saturating_add(self.cooldown_secs())
    }

    /// Whether this record still stands in the way of re-staging an artifact with these
    /// signed digests at `now_unix`. Fail-open: a memo with no digest, with digests that
    /// differ from the pin's (the publisher's repair), from another atpkg version
    /// ([`Self::client`]), or whose cooldown lapsed binds nothing, and an unreadable clock
    /// (`i64::MAX`) lapses every cooldown. [`Self::signer`] and [`Self::held`] memos have
    /// no cooldown: they bind while the digests and the client hold.
    #[inline]
    #[must_use]
    pub fn binds(&self, digest: (DigestAlg, &str), tree_root: &str, now_unix: i64) -> bool {
        self.same_digests(digest, tree_root)
            && self.client == aterm_types::version::app_version()
            && (self.signer || self.held || now_unix < self.retry_after())
    }

    /// Whether this record is about an artifact with these signed digests.
    #[inline]
    fn same_digests(&self, (alg, digest): (DigestAlg, &str), tree_root: &str) -> bool {
        !self.digest.is_empty()
            && self.alg == alg
            && self.digest.eq_ignore_ascii_case(digest)
            && self.tree_root.eq_ignore_ascii_case(tree_root)
    }
}

/// `store/<program>/<build>.refused` for `build_dir`: a sibling like `<build>.ready`, so it
/// never perturbs the build's `tree_root` and can outlive a build directory that never
/// came to exist (the stage it records failed).
fn stage_refusal_path(build_dir: &Path) -> Option<PathBuf> {
    let name = build_dir.file_name()?;
    let name = name.to_str()?;
    let mut marker = String::from(name);
    marker.push_str(STAGE_REFUSAL_SUFFIX);
    Some(build_dir.with_file_name(marker))
}

/// Record that staging `build_dir` failed on the signed digests (temp + rename, so a crash
/// leaves no half-written memo). A record already naming these digests has its attempt
/// count advanced, lengthening the cooldown; one naming other digests is replaced. Writing
/// one reclaims every other `*.refused` beside it, so at most one memo survives per
/// program.
///
/// # Errors
/// The memo could not be written (`store/<program>/` cannot be created, or the volume
/// refuses).
pub fn record_stage_refusal(
    build_dir: &Path,
    digest: (DigestAlg, &str),
    tree_root: &str,
    why: &str,
    now_unix: i64,
) -> std::io::Result<()> {
    write_stage_refusal(
        build_dir,
        digest,
        tree_root,
        why,
        now_unix,
        RefusalKind::Cooldown,
    )
}

/// [`record_stage_refusal`] for a platform signer refusal ([`StageRefusal::signer`]).
///
/// # Errors
/// As [`record_stage_refusal`].
pub(crate) fn record_signer_refusal(
    build_dir: &Path,
    digest: (DigestAlg, &str),
    tree_root: &str,
    why: &str,
    now_unix: i64,
) -> std::io::Result<()> {
    write_stage_refusal(
        build_dir,
        digest,
        tree_root,
        why,
        now_unix,
        RefusalKind::Signer,
    )
}

/// [`record_stage_refusal`] for a `tree_root` mismatch: a held memo
/// ([`StageRefusal::held`]) that binds from its first verdict until the pin's signed
/// digests or the client move, or a person asks (`aterm pkg install <p>`,
/// `aterm pkg update --retry`).
///
/// # Errors
/// As [`record_stage_refusal`].
pub(crate) fn record_held_refusal(
    build_dir: &Path,
    digest: (DigestAlg, &str),
    tree_root: &str,
    why: &str,
    now_unix: i64,
) -> std::io::Result<()> {
    write_stage_refusal(
        build_dir,
        digest,
        tree_root,
        why,
        now_unix,
        RefusalKind::Held,
    )
}

/// Which of the three memos [`write_stage_refusal`] writes.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum RefusalKind {
    /// A signed-digest mismatch: it cools down ([`StageRefusal::cooldown_secs`]).
    Cooldown,
    /// A platform signer refusal: `signer=1` ([`StageRefusal::signer`]).
    Signer,
    /// A `tree_root` mismatch: `hold=1` ([`StageRefusal::held`]).
    Held,
}

fn write_stage_refusal(
    build_dir: &Path,
    digest: (DigestAlg, &str),
    tree_root: &str,
    why: &str,
    now_unix: i64,
    kind: RefusalKind,
) -> std::io::Result<()> {
    let dest = stage_refusal_path(build_dir).ok_or_else(|| {
        std::io::Error::new(std::io::ErrorKind::InvalidInput, "build dir has no name")
    })?;
    let attempts = stage_refusal(build_dir)
        .filter(|prior| prior.same_digests(digest, tree_root))
        .map_or(1, |prior| prior.attempts.saturating_add(1));
    let parent = dest.parent().unwrap_or(build_dir);
    make_build_parent(build_dir)?;
    let tmp = parent.join(format!(".refused.tmp-{}", std::process::id()));
    let mut body = String::from(STAGE_REFUSAL_HEADER);
    body.push('\n');
    body.push_str(digest.0.key());
    body.push('=');
    body.push_str(digest.1);
    body.push_str("\ntree_root=");
    body.push_str(tree_root);
    body.push_str("\nattempts=");
    body.push_str(&attempts.to_string());
    body.push_str("\nat=");
    body.push_str(&u64::try_from(now_unix).unwrap_or(0).to_string());
    // Only when set: a reader that predates them sees a digest memo, which fails open,
    // and a cooled-down memo's bytes are what they were.
    match kind {
        RefusalKind::Signer => body.push_str("\nsigner=1"),
        RefusalKind::Held => body.push_str("\nhold=1"),
        RefusalKind::Cooldown => {}
    }
    body.push_str("\nclient=");
    body.push_str(aterm_types::version::app_version());
    // One line: the reader takes the `why=` line whole.
    body.push_str("\nwhy=");
    let one_line: String = why
        .replace(['\n', '\r'], " ")
        .chars()
        .take(MAX_REFUSAL_WHY_CHARS)
        .collect();
    body.push_str(&one_line);
    body.push('\n');
    write_record_temp(&tmp, body.as_bytes())?;
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
    let (mut sha256, mut sha512, mut tree_root, mut why) = (None, None, None, None);
    let (mut at, mut attempts, mut signer) = (None, None, false);
    let (mut held, mut client) = (false, String::new());
    for line in lines {
        if let Some(v) = line.strip_prefix("sha256=") {
            sha256 = Some(v);
        } else if let Some(v) = line.strip_prefix("sha512=") {
            sha512 = Some(v);
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
        } else if let Some(v) = line.strip_prefix("hold=") {
            held = v.trim() == "1";
        } else if let Some(v) = line.strip_prefix("client=") {
            client = v.trim().to_string();
        }
    }
    // Exactly one payload digest, on the line named for its algorithm.
    let (alg, digest) = match (sha256, sha512) {
        (Some(v), None) => (DigestAlg::Sha256, v),
        (None, Some(v)) => (DigestAlg::Sha512, v),
        _ => return None,
    };
    Some(StageRefusal {
        alg,
        digest: digest.to_string(),
        tree_root: tree_root?,
        why: why.unwrap_or_default(),
        at: at?,
        attempts: attempts?,
        signer,
        held,
        client,
    })
}

/// Remove the refusal beside `build_dir`, if any — a stage of this build that succeeded,
/// every discard, and the explicit install door, so the memo never outlives its cause.
pub fn clear_stage_refusal(build_dir: &Path) {
    if let Some(marker) = stage_refusal_path(build_dir) {
        let _ = std::fs::remove_file(marker);
    }
}

/// The suffix of a vendor-direct build's record (`store/<program>/<build>.vendor`): what
/// was verified, against which anchor, written by the stage before `.ready`.
pub(crate) const VENDOR_SIDECAR_SUFFIX: &str = ".vendor";

/// The sidecars a stage may write through [`crate::install::StageHooks::sidecars`] — and
/// the declared-mode record ([`DECLARED_MODES_SUFFIX`]) the stage writes itself: exactly
/// those [`discard_build`] removes and `gc` sweeps once their build is gone, so a sidecar
/// that describes a tree can never outlive it.
pub(crate) const STAGE_SIDECAR_SUFFIXES: &[&str] = &[
    VENDOR_SIDECAR_SUFFIX,
    crate::shim_env::SIDECAR_SUFFIX,
    DECLARED_MODES_SUFFIX,
];

/// `<build><suffix>` beside `build_dir`, or `None` for a path with no UTF-8 file name.
pub(crate) fn sidecar_path(build_dir: &Path, suffix: &str) -> Option<PathBuf> {
    let name = build_dir.file_name()?;
    let name = name.to_str()?;
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
    tmp_name.push_str(&std::process::id().to_string());
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

/// Forget every refusal recorded for `program`: what the explicit door (`aterm pkg install
/// <program>`) does before it installs, so a person who fixed the publish never waits out
/// an unattended loop's cooldown. A `program` that is not one directory name
/// ([`crate::ops::uninstall_name_shape`]) forgets nothing: it arrives from argv, and
/// `store/..\..\x` is outside the store.
pub fn clear_stage_refusals(layout: &Layout, program: &str) -> usize {
    if crate::ops::uninstall_name_shape(program).is_err() {
        return 0;
    }
    let dir = layout.prefix.join("store").join(program);
    let Ok(entries) = std::fs::read_dir(&dir) else {
        return 0;
    };
    let mut forgotten = 0usize;
    for entry in entries.flatten() {
        if !entry.file_type().is_ok_and(|t| t.is_file()) {
            continue;
        }
        if entry
            .file_name()
            .to_str()
            .is_some_and(|n| n.ends_with(STAGE_REFUSAL_SUFFIX))
            && std::fs::remove_file(entry.path()).is_ok()
        {
            forgotten = forgotten.saturating_add(1);
        }
    }
    forgotten
}

/// [`clear_stage_refusals`] for every program the store has a directory for — what
/// `aterm pkg update --retry` does before its pass, so a held `tree_root` verdict
/// ([`StageRefusal::held`]), which never lapses on its own, can be asked again by the
/// person who believes the publish is fixed. Returns how many memos were forgotten.
pub fn clear_all_stage_refusals(layout: &Layout) -> usize {
    let Ok(entries) = std::fs::read_dir(layout.prefix.join("store")) else {
        return 0;
    };
    let mut forgotten = 0usize;
    for entry in entries.flatten() {
        if !entry.file_type().is_ok_and(|t| t.is_dir()) {
            continue;
        }
        if let Some(program) = entry.file_name().to_str() {
            forgotten = forgotten.saturating_add(clear_stage_refusals(layout, program));
        }
    }
    forgotten
}

/// The sibling scratch directory a stage extracts into before it swaps:
/// `<build>.incoming-<pid>`. Staging never writes into the live `<build>/` tree, so a
/// Ctrl-C, crash or full disk mid-extract never leaves the user without a compiler.
pub(crate) fn incoming_dir(build_dir: &Path) -> Option<PathBuf> {
    scratch_sibling(build_dir, ".incoming-")
}

/// Write the temp of a small record renamed into place beside a build (the
/// shim-environment sidecar, the stage-refusal memo), created `0644`, which the umask can
/// only narrow: a record its group or every user could rewrite would steer what a later
/// verb believes about the build.
pub(crate) fn write_record_temp(tmp: &Path, bytes: &[u8]) -> std::io::Result<()> {
    use std::io::Write as _;
    let _ = std::fs::remove_file(tmp);
    crate::platform::open_create_write(tmp, 0o644)?.write_all(bytes)
}

/// Make the directory a build lives in (`store/<program>/`, and `store/` above it) when
/// missing, at the stage's one directory mode ([`crate::extract::safe_mode`], `0755`)
/// whatever the umask ([`crate::platform::make_dirs_born`]). Every writer that can be
/// first to need it comes here: a `store/<program>/` its group or every user could write
/// would let them swap a build for their own. One that exists is left as it is.
pub(crate) fn make_build_parent(build_dir: &Path) -> std::io::Result<()> {
    match build_dir.parent() {
        Some(parent) if !parent.as_os_str().is_empty() => {
            crate::platform::make_dirs_born(parent, crate::extract::safe_mode(0, true))
        }
        _ => Ok(()),
    }
}

/// The sibling the outgoing tree is moved to during a swap: `<build>.superseded-<pid>`.
/// It exists only between the two renames, and is deleted immediately after.
pub(crate) fn superseded_dir(build_dir: &Path) -> Option<PathBuf> {
    scratch_sibling(build_dir, ".superseded-")
}

/// `<build><suffix><pid>` beside `build_dir`. The pid keeps two stagers of the same build
/// apart even though the store lock already serializes them — a scratch name that
/// collides is a scratch name that can be deleted out from under its owner.
fn scratch_sibling(build_dir: &Path, suffix: &str) -> Option<PathBuf> {
    let name = build_dir.file_name()?.to_str()?;
    Some(build_dir.with_file_name(format!("{name}{suffix}{}", std::process::id())))
}

/// Which of the two scratch shapes a stage produced.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Scratch {
    /// `<build>.incoming-<pid>` — the tree being extracted, before it has been verified.
    Incoming,
    /// `<build>.superseded-<pid>` — the outgoing tree, parked between the two swap renames.
    Superseded,
}

/// A store build directory's name, parsed the way this manager writes one: a non-empty run
/// of ASCII digits, no leading zero unless the name is exactly `0` (`u64`'s `Display`
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

/// Recognise stage scratch by the producer's exact shape: `<build>.incoming-<pid>` or
/// `<build>.superseded-<pid>`, where `<build>` is a real build number and `<pid>` a
/// non-empty run of ASCII digits. Returns the build number and the shape. The single
/// recogniser both sweepers use ([`sweep_stage_scratch`] and [`crate::gc`]), since they
/// authorize `remove_dir_all` in a directory the user can also write:
/// `18.incoming-drafts/` is not ours to delete.
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
            // Siblings of a different build are that build's business, not ours.
            (found == build).then(|| (kind, entry.path()))
        })
        .collect()
}

/// Recover the one crash window a swap cannot make atomic; `true` when a tree was moved
/// back.
///
/// [`crate::install::verify_and_stage`]'s swap moves the outgoing tree to
/// `<build>.superseded-<pid>`, then the verified incoming tree onto `<build>`. Between the
/// renames the only copy of the old tree is under the scratch name, so a kill or power
/// loss leaves `<build>` absent with one superseded sibling, which a sweep would destroy.
///
/// Narrow on purpose: it fires only when `<build>` is absent and exactly one superseded
/// sibling is a real directory. With `<build>` present the sibling is leftover; with two
/// there is no telling which is the outgoing tree.
///
/// It does not re-mark the recovered tree (the swap cleared the marker, so its
/// completeness is unknowable): it reads as not installed and the next apply pass
/// re-stages it, since `flow::installed_for_decide` drops an unmarked live build before
/// [`crate::gate::decide`] sees it.
pub(crate) fn recover_interrupted_swap(build_dir: &Path) -> bool {
    // `symlink_metadata`, not `exists()`: a dangling symlink at the build path is still
    // something there, and the window means nothing is.
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

/// Delete every stage-scratch sibling of `build_dir` an earlier run left, after recovering
/// the swap window ([`recover_interrupted_swap`]). The caller holds the store lock, which
/// every stager takes. Nothing else reclaims this scratch (GC sees only numeric,
/// marker-bearing dirs). A superseded directory is spared while `<build>` is absent: it is
/// the only copy of the build on disk.
pub(crate) fn sweep_stage_scratch(build_dir: &Path) {
    let recovered = recover_interrupted_swap(build_dir);
    // While nothing stands at `<build>`, a superseded sibling is the only copy of that
    // build: a swap window [`recover_interrupted_swap`] could not close. `crate::gc` holds
    // the same guard and reports an ambiguous pair as `doctor`'s `diverged` line.
    let parked_only_copy = !recovered && std::fs::symlink_metadata(build_dir).is_err();
    for (kind, path) in scratch_siblings(build_dir) {
        // Spare only a directory (what a swap parks); a file or symlink at that name is
        // nobody's only copy, and nothing else reclaims it.
        if parked_only_copy
            && kind == Scratch::Superseded
            && std::fs::symlink_metadata(&path).is_ok_and(|m| m.is_dir())
        {
            continue;
        }
        // `symlink_metadata`, not `is_dir()`: `remove_dir_all` refuses a symlink to a
        // directory, so dispatching on the followed type would leave that entry forever.
        match std::fs::symlink_metadata(&path) {
            Ok(m) if m.is_dir() => {
                let _ = remove_tree(&path);
            }
            Ok(_) => {
                let _ = std::fs::remove_file(&path);
            }
            Err(_) => {}
        }
    }
}

/// Attempt a full flush of this directory's metadata. Under the supported storage
/// contract, success persists its entries and earlier fsync-submitted I/O on the same
/// device; it does not submit a rename in another, unflushed directory. Best-effort:
/// open and flush errors are intentionally ignored, so returning from this helper is
/// not a durability receipt. Callers must account for that uncertainty in recovery.
pub(crate) fn sync_dir(dir: &Path) {
    if let Ok(handle) = std::fs::File::open(dir) {
        let _ = crate::platform::sync_durable(&handle);
    }
}

/// Attempt an ORDERED flush of `dir` ([`aterm_durable::ordered`]): on a successful
/// supported flush, its entries and other already-fsynced I/O on that device precede
/// later I/O. This does not submit publications in other, unflushed directories or prove
/// an atomic/valid prefix of the store. Callers establish the earlier-file and recovery
/// invariants (a sidecar before its marker). Best-effort, as [`sync_dir`]: open or flush
/// errors are intentionally ignored. Build discard uses the full [`sync_dir`] instead.
pub(crate) fn sync_dir_ordered(dir: &Path) {
    if let Ok(handle) = std::fs::File::open(dir) {
        let _ = aterm_durable::ordered(&handle);
    }
}

/// [`crate::platform::sync_file_contents`], with a refusal degraded to success: some
/// volumes (a network home, some FUSE mounts) answer a flush `ENOTSUP`/`EINVAL`, and
/// failing the install there buys nothing. A real error (`EIO`, `ENOSPC`) still
/// propagates.
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
/// every file and directory of a staged tree rather than some arbitrary subset of it —
/// there is no other observable the flush leaves behind.
#[derive(Debug, Default)]
pub(crate) struct Synced {
    /// Regular files whose contents were pushed to the filesystem.
    pub files: u64,
    /// Directories whose entries were pushed to the filesystem.
    pub dirs: u64,
}

/// Flush a staged tree (every regular file's contents, then every directory's entries) so
/// nothing that publishes the tree can become durable ahead of the bytes it publishes.
/// Call between the last verification and the swap.
///
/// A rename is metadata; the data behind it is page cache until flushed. Under ext4's
/// delayed allocation (`auto_da_alloc` covers only a rename over an existing file, which
/// neither the swap nor the marker is) the journal can commit `<build>/` and
/// `<build>.ready` before the payload, and after a power loss nothing repairs a store
/// that says "complete" over truncated files ([`crate::gate::decide`] answers
/// `UpToDate`). [`sync_dir`] orders the renames; it does not make the data durable.
///
/// One `fsync(2)` per file, never `fcntl(F_FULLFSYNC)`
/// ([`crate::platform::sync_file_contents`]). Symlinks are never followed or opened. A
/// file this process cannot open is skipped rather than failing the install; no lane lays
/// one (every staged file is `0644` or `0755`, `extract::safe_mode`).
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
                // Opened the way the platform's flush needs it
                // (`platform::open_for_sync`): read-only on Unix, where `fsync(2)` takes
                // any fd; with write access on Windows, where `FlushFileBuffers` refuses
                // a read-only handle with `Access is denied`.
                match crate::platform::open_for_sync(&path) {
                    Ok(f) => {
                        sync_contents_or_accept_refusal(&f)?;
                        counts.files = counts.files.saturating_add(1);
                    }
                    Err(e) if e.kind() == std::io::ErrorKind::PermissionDenied => {}
                    Err(e) => return Err(e),
                }
            }
        }
        // The directory's own entries, after the contents they name. Best-effort on the
        // open alone: Windows cannot open a directory as a file at all (NTFS journals
        // directory metadata itself), so that arm is skipped there.
        if let Ok(handle) = std::fs::File::open(&dir) {
            sync_contents_or_accept_refusal(&handle)?;
            counts.dirs = counts.dirs.saturating_add(1);
        }
    }
    Ok(counts)
}

/// The mode of a single-binary build's `bin/` once its file is laid ([`seal_bin`]): read
/// and search for everyone, write for nobody, its owner included.
pub(crate) const SEALED_BIN_MODE: u32 = 0o555;

/// Seal a staged single-binary tree's `bin/` at [`SEALED_BIN_MODE`]
/// ([`crate::install::verify_and_stage`], once every member has its final mode).
///
/// A vendor's own updater rewrites its executable in place (agy's `update`, its background
/// updater and its `install` all write beside `os.Executable()`, the store build, leaving
/// a 183 MB `agy.<nanos>.old`), and each refuses a directory it cannot write. The shim's
/// intercept is bypassed by an older atpkg answering it, an older `repair` laying a shim
/// without it, or the shim's atpkg being gone; a write bit nobody holds stops all three.
/// Directory modes are not folded into a `tree_root` ([`crate::tree`]), so the seal
/// changes no verified root. Every removal of a store tree unseals it first
/// ([`remove_tree`]).
pub(crate) fn seal_bin(tree: &Path) -> std::io::Result<()> {
    crate::platform::set_mode(&tree.join("bin"), SEALED_BIN_MODE)
}

/// Give a sealed `<tree>/bin` ([`seal_bin`]) its owner's write bit back so the tree's
/// removal can unlink what it holds. Only the seal is undone: a `bin/` at any other mode
/// or a link, or a `tree` that is not a real directory, is left as it is.
pub(crate) fn unseal_bin(tree: &Path) {
    let real_dir = |p: &Path| std::fs::symlink_metadata(p).ok().filter(|m| m.is_dir());
    let bin = tree.join("bin");
    if real_dir(tree).is_some()
        && real_dir(&bin)
            .is_some_and(|m| crate::platform::permission_mode(&m) & 0o7777 == SEALED_BIN_MODE)
    {
        let _ = crate::platform::set_mode(&bin, crate::extract::safe_mode(0, true));
    }
}

/// Remove the store tree at `dir` (a build, or a stage's `.incoming`/`.superseded`
/// scratch), unsealing its `bin/` first ([`unseal_bin`]). std's `remove_dir_all` never
/// follows a link.
pub(crate) fn remove_tree(dir: &Path) -> std::io::Result<()> {
    unseal_bin(dir);
    std::fs::remove_dir_all(dir)
}

/// Discard a build entirely: its tree, its sibling completeness marker, its sidecars, and
/// the exec root [`crate::compat`] laid for it. For a build a transaction staged but
/// aborted without activating, which `list_installed`/`decide` would otherwise mis-read as
/// active. Best-effort.
///
/// `pub(crate)` because it is an unconditional `remove_dir_all` with no notion of which
/// build is live: reclaim goes through [`crate::gc::discard_superseded`], which demands a
/// [`crate::gc::LiveBuild`] witness and refuses to delete it. The one `read_dir`-fold
/// caller, [`crate::gc::run`]'s interrupted-install sweep, has no live build to witness and
/// is guarded instead on the claim union (every `current` link plus every `bin/` shim
/// target), deleting only trees nothing on disk points into.
pub(crate) fn discard_build(build_dir: &Path) {
    // Attempt to remove and fully flush the marker before the exec root and tree: the
    // `remove_dir_all` walk can be interrupted, leaving a partial tree. When unmarked,
    // it reads as incomplete ([`build_is_complete`]) and `gc`'s partial arm reclaims it.
    // Both the unmark and `sync_dir` are best-effort; their failure supplies no persisted
    // marker-before-tree guarantee. This full flush does not flush an unpublished shim name.
    let _ = clear_build_ready(build_dir);
    if let Some(parent) = build_dir.parent() {
        sync_dir(parent);
    }
    // The build's exec root (`<prefix>/compat/<program>/<n>`, `crate::compat`): a
    // copy-on-write clone that would keep reclaimed blocks allocated and could still run
    // a discarded build's tools.
    crate::compat::discard_root_of(build_dir);
    let _ = remove_tree(build_dir);
    // The marker again: the unmark above is best-effort, and the sibling must not outlive
    // the tree.
    if let Some(marker) = ready_marker_path(build_dir) {
        let _ = std::fs::remove_file(marker);
    }
    // The source-build provenance sidecar (`<build>.provenance`), so a later signed
    // reinstall under this build number is never mis-verified as source-built.
    if let Some(name) = build_dir.file_name().and_then(|n| n.to_str()) {
        let mut prov = String::from(name);
        prov.push_str(".provenance");
        let _ = std::fs::remove_file(build_dir.with_file_name(prov));
    }
    // The shim-environment sidecar (`<build>.shim-env`): a later reinstall writes its own
    // from its signed manifest.
    crate::shim_env::remove_sidecar(build_dir);
    // The digest-refusal memo (`<build>.refused`): a verdict about bytes a pin named.
    clear_stage_refusal(build_dir);
    // The vendor record (`<build>.vendor`) vouches for a tree that is gone.
    clear_vendor_sidecar(build_dir);
    // The declared-mode record (`<build>.modes`): a stale one must never be what `verify`
    // folds.
    clear_declared_modes(build_dir);
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
/// (`None`: the default). The prefix is chain-validated against the home dir
/// ([`vet_prefix`]); any violation falls back to the default. `None` only when the home
/// directory cannot be resolved ([`aterm_types::dirs::home_dir`], not a raw `$HOME`, which
/// a native-Windows shell does not set).
#[must_use]
pub fn resolve(configured: Option<&Path>) -> Option<Layout> {
    let home = aterm_types::dirs::home_dir()?;
    let prefix = vet_prefix(configured, &home);
    // A rejected prefix falls back to the default: right in production (a typo must not
    // brick the store), catastrophic under test, where the default is the developer's own
    // live store. Asserted here so no test has to remember.
    #[cfg(test)]
    if let Some(asked) = configured {
        assert_eq!(
            prefix,
            asked.to_path_buf(),
            "vet_prefix rejected the prefix this test asked for and fell back to the \
             DEFAULT — which under test is the developer's real store. Give the test a \
             prefix the vet admits: strictly under $HOME, absolute, no `..`, and a real \
             0700 directory"
        );
    }
    Some(Layout { prefix })
}

/// Resolve the layout the user configured: `[packages].prefix` when set, the default
/// otherwise. Every caller outside `atpkg`'s own CLI should use it: `resolve(None)` looks
/// at the default prefix and misses a relocated or shared store.
#[must_use]
pub fn resolve_configured() -> Option<Layout> {
    resolve_from(&crate::config::load())
}

/// [`resolve_configured`] over a `[packages]` table already read. The terminal session
/// reads `aterm.toml` once ([`crate::config::cached`]) so complaints about the file are not
/// repeated per consumer; the window, which lives for days, keeps [`resolve_configured`]
/// so an edited prefix is seen.
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
///
/// The default comes back unvetted: there is nowhere further to fall back. Its missing
/// ancestors are made never group/other-writable ([`Layout::ensure_dir`]), and one that
/// already is (made by an older build under umask `002`) is named with its `chmod go-w` by
/// doctor's check of the installed Trust toolchain's chain, which `targo trust` refuses.
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
    // System prefix: outside $HOME, admissible only when every existing component from `/`
    // down is root-owned and not group/other-writable (see the module docs).
    if !under_home(p, home) {
        if !system_chain_trusted(p) {
            return default;
        }
        // Trusted is not usable: a non-root user with a root-owned prefix would fail on
        // `store.lock` for every verb (docs/AUDIT-nux-first-open-toolchain-2026-08-31.md).
        // Fall back to the `$HOME`-owned default, which needs no privilege, and say so
        // once per process as an unasked notice ([`crate::notice`]).
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
    // Home prefix: strictly under $HOME, compared by `under_home` (case-insensitive on
    // Windows).
    if p == home {
        return default;
    }
    // Walk every existing directory from `home` down to the leaf; each must be a real dir
    // (not a symlink or Windows junction), owned by us, and not group/other-writable. (On
    // Windows `dir_meta_is_private` is a best-effort `true`; privacy rests on the per-user
    // ACL.)
    for anc in p.ancestors().filter(|a| under_home(a, home)) {
        // A missing ancestor is the tail atpkg makes: the prefix `0700`, the directories on
        // the way `0755` whatever the umask (`create_dir_all_unshared`), so the next resolve
        // admits what this one created. `is_reparse` also catches a junction, which needs no
        // admin and reports `is_symlink() == false`.
        if let Ok(m) = std::fs::symlink_metadata(anc)
            && (crate::platform::is_reparse(&m) || !crate::platform::dir_meta_is_private(&m))
        {
            return default;
        }
    }
    p.to_path_buf()
}

#[must_use]
/// Whether the caller can use `p`: it can write the deepest existing component (where the
/// first `create_dir` lands; atpkg creates the tail), or the prefix already serves working
/// tools through its `bin/` shims.
fn caller_can_use_prefix(p: &Path) -> bool {
    // Writable: we can install here.
    if p.ancestors()
        .find(|a| a.exists())
        .is_some_and(crate::platform::dir_writable_by_caller)
    {
        return true;
    }
    // Not writable, but a prefix that already serves working tools is still usable: a
    // shared multi-user prefix is installed once by an admin and read by users who cannot
    // write it. The test is whether a shim resolves, not whether `store/` exists: a
    // populated `store/` whose shims all dangle is not a working prefix.
    let Ok(entries) = std::fs::read_dir(p.join("bin")) else {
        return false;
    };
    // Not provably gone, rather than stat succeeded: a user who may traverse and exec this
    // store but not read it gets `false` from `exists()`, which would abandon the shared
    // prefix for a private reinstall. Only a `NotFound` shows a tool is not there.
    entries.flatten().any(|e| {
        crate::platform::resolve_shim(&e.path()).is_some_and(|t| !presence(&t).is_absent())
    })
}

/// A test's own system-shaped prefix: only root can create one under a root-owned chain,
/// so a test that must tell the two shapes apart names its scratch prefix here, on its own
/// thread, and every [`Layout::is_system_prefix`] question about it answers yes. Compiled
/// into tests only.
#[cfg(test)]
pub(crate) mod system_shape {
    use std::cell::RefCell;
    use std::path::{Path, PathBuf};

    thread_local! {
        static FORCED: RefCell<Vec<PathBuf>> = const { RefCell::new(Vec::new()) };
    }

    /// Answer "system-shaped" for `prefix` on this thread from now on (the mode-bit tests
    /// that ask are unix).
    #[cfg(unix)]
    pub(crate) fn force(prefix: &Path) {
        FORCED.with(|f| f.borrow_mut().push(prefix.to_path_buf()));
    }

    pub(super) fn forced(prefix: &Path) -> bool {
        FORCED.with(|f| f.borrow().iter().any(|p| p == prefix))
    }
}

/// Whether every existing directory from `/` down to `p` is root-owned, not
/// group/other-writable, and not a symlink or reparse point: the system-prefix chain
/// check, an AND over the full chain, since one writable component reopens the CWE-379
/// swap window. A non-existent tail is what atpkg will create (as root).
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
/// case-insensitive per-component on Windows, where the filesystem is case-insensitive so
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

/// Commands a managed shim may never name, even though `bin/` is only appended to `PATH`:
/// a tool named one of these, or one of [`SHELLS`], is refused a shim, so a key compromise
/// or an honest mistake cannot intercept core or security commands. A client that meets one
/// in a signed `exposes` records it in `status.toml`'s `refused_shims`
/// ([`crate::status::RefusedShims`]), which `aterm pkg doctor`'s `base.pkg.refused` row
/// names; the producer refuses to pack one. Lower-cased.
const SENSITIVE_SHIMS: &[&str] = &[
    "sudo",
    "ssh",
    "scp",
    "sshd",
    "git",
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

/// The shells a shim may never be named for: a shim named for a shell would run inside
/// every script whose `#!` names it, on any machine where that shell is not installed ahead
/// of the managed `bin/` (most are absent from a stock macOS `PATH`). [`is_shell`] also
/// refuses each one's `-preview` and versioned spellings (`pwsh-preview`, `bash5`,
/// `pwsh-7.4`). A shell this list does not name is shimmed like any tool name. Lower-cased.
const SHELLS: &[&str] = &[
    "sh",
    "bash",
    "rbash",
    "zsh",
    "fish",
    "pwsh",
    "powershell",
    "nu",
    "dash",
    "ash",
    "ksh",
    "mksh",
    "oksh",
    "pdksh",
    "loksh",
    "yash",
    "tcsh",
    "csh",
    "elvish",
    "xonsh",
    "osh",
    "ysh",
    "ion",
    "rc",
    "es",
    "busybox",
];

/// Whether the lower-cased `lower` names a shell: one of [`SHELLS`], alone, with
/// `-preview`, or with a version (`5`, `-7.4`, `5.2.1`: an optional `-`, then a digit,
/// then digits and dots).
fn is_shell(lower: &str) -> bool {
    SHELLS.iter().any(|shell| {
        lower.strip_prefix(shell).is_some_and(|rest| {
            let version = rest.strip_prefix('-').unwrap_or(rest);
            rest.is_empty()
                || rest == "-preview"
                || (version.starts_with(|c: char| c.is_ascii_digit())
                    && version.chars().all(|c| c.is_ascii_digit() || c == '.'))
        })
    })
}

/// The prefix of an alias shim: `alab-<tool>` is laid beside every `<tool>` shim of one of
/// ALab's own programs and forwards to the same store executable
/// ([`crate::activate::Aliases`]). ALab's bare tool names collide with other software
/// (Homebrew's p11-kit installs `/opt/homebrew/bin/trust`; Homebrew core owns `ty` and
/// `clean`) and the managed `bin/` is appended to `PATH`, so `alab-trust` always names
/// ALab's. The prefix is reserved: `alab-<x>` is admissible exactly when `<x>` is
/// (`alab-sudo` is refused like `sudo`), and a tool already carrying the prefix is its own
/// alias.
pub const ALIAS_PREFIX: &str = "alab-";

/// Whether `name` may be installed as a `bin/` shim: non-empty, plain ASCII, not `.`/`..`,
/// free of `/`, `\` and NUL, not on the `SENSITIVE_SHIMS` deny-list and not a shell
/// ([`SHELLS`]), both case-insensitively.
///
/// ASCII only, because the volume `bin/` lives on folds more than ASCII case: on macOS's
/// default case-insensitive APFS a file named `pw\u{17F}h` (LATIN SMALL LETTER LONG S),
/// `\u{FB01}sh` or `\u{212A}sh` (KELVIN SIGN) answers a lookup of `pwsh`, `fish` or `ksh`,
/// so a deny-list compared after ASCII folding alone would be bypassed by a lookalike. Both
/// separators are refused: on Windows a `\` in a manifest-supplied name makes `Path::join`
/// traverse out of `bin/` (`..\..\evil`) and lets `..\git` dodge the deny-list;
/// `linkmode::safe_component`, `ops::uninstall` and the other name gates refuse `\` too.
///
/// An [`ALIAS_PREFIX`]ed name is admitted only when its base is: `alab-sudo` is refused
/// like `sudo`, a bare `alab-` has no base, and `alab-alab-x` is refused (one level of
/// aliasing).
#[must_use]
pub fn shim_allowed(name: &str) -> bool {
    if name.is_empty()
        || !name.is_ascii()
        || name == "."
        || name == ".."
        || name.contains('/')
        || name.contains('\\')
        || name.contains('\0')
    {
        return false;
    }
    let lower = name.to_ascii_lowercase();
    if SENSITIVE_SHIMS.contains(&lower.as_str()) || is_shell(&lower) {
        return false;
    }
    match lower.strip_prefix(ALIAS_PREFIX) {
        // The base must itself be a shim name, and never another alias.
        Some(base) => !base.is_empty() && !base.starts_with(ALIAS_PREFIX) && shim_allowed(base),
        None => true,
    }
}

/// A logical tool name: one entry of a manifest's `exposes` list, never a file name.
///
/// The logical name, the `bin/` shim file (`<name><SHIM_SUFFIX>`) and the executable
/// inside a build's `bin/` (`<name><EXE_SUFFIX>`) coincide on Unix (both suffixes `""`)
/// but are three files on Windows (`.cmd`, `.exe`). So the type has no
/// `Deref<Target = str>` and no `Display`: `Path::join(tool)` and `format!("{tool}")` do
/// not compile, and the author must pick [`shim_file`](Self::shim_file) or
/// [`exe_file`](Self::exe_file).
#[derive(Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Debug)]
pub struct ToolName(String);

/// `<name><suffix>`, the single rule behind both of [`ToolName`]'s renderings. The suffix
/// is a parameter so tests can drive it with `.cmd`/`.exe`: on Unix both suffixes are `""`
/// and a test against the methods would pass a wrong implementation.
fn with_suffix(name: &str, suffix: &str) -> String {
    format!("{name}{suffix}")
}

/// The inverse of [`with_suffix`], and total: a name without the suffix is returned
/// unchanged. On Windows a `bin/` entry may be `ay.cmd` (a shim this manager wrote) or `ay`
/// (a hand-made file), and both must read back as `ay` so re-shimming replaces rather than
/// writing `ay.cmd.cmd` beside it.
#[inline]
fn without_suffix<'a>(name: &'a str, suffix: &str) -> &'a str {
    name.strip_suffix(suffix).unwrap_or(name)
}

impl ToolName {
    /// Admit `raw` as a tool name, or `None` when it fails [`shim_allowed`] (a sensitive
    /// command or a malformed name). Checking here rather than at each call site means no
    /// shim writer can forget the deny-list.
    #[inline]
    #[must_use]
    pub fn new(raw: &str) -> Option<Self> {
        shim_allowed(raw).then(|| Self(raw.to_string()))
    }

    /// The `bin/` **shim** file name: `<tool><SHIM_SUFFIX>` — `ay` on Unix (a bare symlink),
    /// `ay.cmd` on Windows (a batch wrapper).
    #[inline]
    #[must_use]
    pub fn shim_file(&self) -> String {
        with_suffix(&self.0, crate::platform::SHIM_SUFFIX)
    }

    /// The **executable** file name inside a build's `bin/`: `<tool><EXE_SUFFIX>` — `ay` on
    /// Unix, `ay.exe` on Windows. This is what a shim forwards to; it is never the shim's own
    /// name, and on Windows conflating the two yields `bin/ay.cmd` pointing at `bin\ay.cmd`.
    #[inline]
    #[must_use]
    pub fn exe_file(&self) -> String {
        with_suffix(&self.0, crate::platform::EXE_SUFFIX)
    }

    /// Recover the logical name from a `bin/` entry, stripping the platform
    /// [`crate::platform::SHIM_SUFFIX`] (`ay.cmd` reads back as `ay` on Windows). Callers
    /// feed it back through [`Layout::shim`] / `install_shims` / `install_tombstone_shim`,
    /// which append the suffix again. `None` for an entry this manager could never have
    /// written ([`shim_allowed`] refuses it), the fail-closed direction for the caller that
    /// deletes what it recognizes.
    #[inline]
    #[must_use]
    pub fn from_shim_file(name: &str) -> Option<Self> {
        Self::new(without_suffix(name, crate::platform::SHIM_SUFFIX))
    }

    /// The logical name, for reporting (`status.toml`, refusal lists, log lines). not for
    /// building a path — use [`shim_file`](Self::shim_file) / [`exe_file`](Self::exe_file).
    #[inline]
    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }

    /// Whether this is an [`ALIAS_PREFIX`]ed name (`alab-trust`), i.e. a shim that forwards
    /// to another tool's executable rather than to one of its own name.
    #[inline]
    #[must_use]
    pub fn is_alias(&self) -> bool {
        self.0.starts_with(ALIAS_PREFIX)
    }

    /// The `alab-<tool>` alias of this tool — `None` when the tool already carries the
    /// prefix (an alias never gets an alias; the name is unambiguous as it stands). Always
    /// `Some` otherwise: a base [`shim_allowed`] admits, the prefixed spelling admits too.
    #[inline]
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
    #[inline]
    #[must_use]
    pub fn alias_base(&self) -> Option<Self> {
        self.0.strip_prefix(ALIAS_PREFIX).and_then(Self::new)
    }
}

/// Split a manifest's raw `exposes` list into the names that may be shimmed and the raw
/// names refused: the one place a `Vec<String>` off the wire becomes [`ToolName`]s. Order
/// is preserved in both halves.
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
/// managed `bin_dir` appended, never prepended, so a pinned tool's siblings resolve by bare
/// name while system commands on the inherited `PATH` are never shadowed (§10); `atpkg
/// doctor` warns when a same-named tool earlier on `PATH` wins. Idempotent; with no
/// inherited `PATH`, just `bin_dir`. The one source of the `atpkg run` / `aterm <tool>`
/// child `PATH`.
#[must_use]
pub fn append_bin_to_path(inherited: Option<&OsStr>, bin_dir: &Path) -> OsString {
    // An absent or empty inherited `PATH` means "no directories" — start empty so we never
    // emit a leading empty component (which Unix reads as the current directory).
    let mut dirs: Vec<PathBuf> = match inherited {
        Some(p) if !p.is_empty() => std::env::split_paths(p).collect(),
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

    fn temp_home(label: &str) -> crate::testdir::Dir {
        crate::testdir::dir(&format!("atpkg-store-{label}-"))
    }

    /// A fresh prefix gets a private `agents/` and its absolute path, a second call is a
    /// no-op, and a symlink (even to a real directory) or a regular file there is refused
    /// and left alone.
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
        filed.ensure_agents_dir().expect_err("a file is refused");
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
            linked.ensure_agents_dir().expect_err("a link is refused");
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

    /// The declared-mode record is written beside the build where the inode stores no
    /// permission bits, read back as the folded map, and taken by `discard_build`; on Unix
    /// nothing is written and the read answers empty.
    #[test]
    fn the_declared_mode_record_round_trips_and_goes_with_the_build() {
        let h = temp_home("declared-modes");
        let l = Layout { prefix: h.clone() };
        let build = l.build_dir("claude", 2026092201);
        std::fs::create_dir_all(build.join("bin")).unwrap();
        let mut modes = crate::tree::DeclaredModes::new();
        modes.insert(b"bin/claude.exe".to_vec(), 0o755);
        modes.insert(b"lib/data".to_vec(), 0o644);
        write_declared_modes(&build, &modes).unwrap();
        let sidecar = build.with_file_name("2026092201.modes");
        if crate::platform::HAS_POSIX_MODES {
            assert!(
                !sidecar.exists(),
                "Unix reads the inode: no record is written"
            );
            assert!(declared_modes(&build).unwrap().is_empty());
        } else {
            assert!(sidecar.is_file(), "a sibling, outside the tree");
            assert_eq!(
                std::fs::read(&sidecar).unwrap(),
                b"declared-modes v1\nbin/claude.exe\x00755\nlib/data\x00644\n".to_vec(),
                "the line framing is the tree line's, minus its digest"
            );
            assert_eq!(declared_modes(&build).unwrap(), modes);
            clear_declared_modes(&build);
            let err = declared_modes(&build).unwrap_err().to_string();
            assert!(
                err.contains("no declared-mode record") && err.contains("re-stages"),
                "{err}"
            );
            write_declared_modes(&build, &modes).unwrap();
        }
        discard_build(&build);
        assert!(
            !build.exists() && !sidecar.exists(),
            "the discard takes the record"
        );
        let _ = std::fs::remove_dir_all(&h);
    }

    /// The parser fails closed on every malformed shape and reads the tree line's octal
    /// spelling (`0` for a zero mode).
    #[test]
    fn the_declared_mode_record_parser_fails_closed() {
        let ok =
            parse_declared_modes(b"declared-modes v1\nbin/a\x00755\nb\x000\nc\x00644\n").unwrap();
        assert_eq!(ok.get(&b"bin/a"[..]), Some(&0o755));
        assert_eq!(ok.get(&b"b"[..]), Some(&0));
        assert_eq!(ok.get(&b"c"[..]), Some(&0o644));
        assert!(
            parse_declared_modes(b"declared-modes v1\n")
                .unwrap()
                .is_empty()
        );
        for bad in [
            &b"declared-modes v2\nbin/a\x00755\n"[..],
            b"bin/a\x00755\n",
            b"declared-modes v1\nbin/a 755\n",
            b"declared-modes v1\n\x00755\n",
            b"declared-modes v1\nbin/a\0\n",
            b"declared-modes v1\nbin/a\x00758\n",
            b"declared-modes v1\nbin/a\x0007555\n",
        ] {
            assert!(parse_declared_modes(bad).is_none(), "{bad:?}");
        }
        assert_eq!(crate::tree::oct_mode(0o755), "755");
    }

    /// A sha256 payload digest, as a refusal names it.
    fn sha256(hex: &str) -> (DigestAlg, &str) {
        (DigestAlg::Sha256, hex)
    }

    /// A memo names its payload digest on its algorithm's line: a sha256 memo's bytes are
    /// unchanged, a sha512 one binds nothing under the other algorithm, and a memo naming
    /// both digests or neither is no memo.
    #[test]
    fn a_refusal_memo_names_its_digest_by_algorithm() {
        let h = temp_home("refusal-alg");
        let l = Layout { prefix: h.clone() };
        let build = l.build_dir("agy", 1_000_001_000_003_000_006);
        let marker = build.with_file_name("1000001000003000006.refused");
        let client = aterm_types::version::app_version();
        // The shape an earlier client wrote.
        let earlier = format!(
            "stage-refusal v1\nsha256=aa\ntree_root=bb\nattempts=2\nat=1000\nclient={client}\n\
             why=asset sha256 mismatch: expected aa, got cc\n"
        );
        std::fs::create_dir_all(marker.parent().unwrap()).unwrap();
        std::fs::write(&marker, &earlier).unwrap();
        let m = stage_refusal(&build).unwrap();
        assert_eq!(
            (m.alg, m.digest.as_str(), m.attempts),
            (DigestAlg::Sha256, "aa", 2)
        );
        assert!(m.binds(sha256("aa"), "bb", 1_001));
        record_stage_refusal(
            &build,
            sha256("aa"),
            "bb",
            "asset sha256 mismatch: expected aa, got cc",
            1_000,
        )
        .unwrap();
        assert_eq!(
            std::fs::read_to_string(&marker).unwrap(),
            earlier.replace("attempts=2", "attempts=3"),
            "a sha256 memo is written as it always was"
        );

        let sha512 = "5".repeat(128);
        record_signer_refusal(
            &build,
            (DigestAlg::Sha512, &sha512),
            "bb",
            "signer refused: x",
            2_000,
        )
        .unwrap();
        let text = std::fs::read_to_string(&marker).unwrap();
        assert!(text.contains(&format!("\nsha512={sha512}\n")), "{text}");
        assert!(!text.contains("sha256="), "{text}");
        let m = stage_refusal(&build).unwrap();
        assert_eq!(
            (m.alg, m.digest.as_str(), m.attempts),
            (DigestAlg::Sha512, sha512.as_str(), 1)
        );
        assert!(m.binds((DigestAlg::Sha512, &sha512), "bb", i64::MAX));
        assert!(
            !m.binds(sha256(&sha512), "bb", i64::MAX),
            "the same hex under another algorithm is another question"
        );
        for bad in [
            text.replace("sha512=", "sha256=x\nsha512="),
            text.replace(&format!("sha512={sha512}\n"), ""),
        ] {
            std::fs::write(&marker, &bad).unwrap();
            assert_eq!(stage_refusal(&build), None, "{bad}");
        }
        let _ = std::fs::remove_dir_all(&h);
    }

    /// A memo binds only the digests it was recorded for, only from the second identical
    /// verdict, and only until its cooldown lapses.
    #[test]
    fn a_stage_refusal_binds_only_while_its_digests_and_cooldown_hold() {
        let memo = |attempts| StageRefusal {
            alg: DigestAlg::Sha256,
            digest: "AA".into(),
            tree_root: "bb".into(),
            why: "asset sha256 mismatch: expected aa, got cc".into(),
            at: 1_000,
            attempts,
            signer: false,
            held: false,
            client: aterm_types::version::app_version().into(),
        };
        // The first retry is free; from the second identical verdict: 12 h, doubling,
        // capped at a week.
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
            !memo(1).binds(sha256("aa"), "bb", 1_000),
            "a single failure never holds the next attempt off the wire"
        );

        let m = memo(2);
        assert_eq!(m.retry_after(), 1_000 + 12 * 3600);
        assert!(
            m.binds(sha256("aa"), "BB", 1_000),
            "case-insensitive on both digests"
        );
        assert!(
            !m.binds(sha256("aa"), "BB", m.retry_after()),
            "the cooldown lapses — the next attempt is due"
        );
        assert!(
            !m.binds(sha256("dd"), "bb", 1_000),
            "a re-cut pin's new sha256 binds nothing"
        );
        assert!(
            !m.binds(sha256("aa"), "dd", 1_000),
            "a re-cut pin's new tree_root binds nothing"
        );
        assert!(
            !m.binds(sha256("aa"), "bb", i64::MAX),
            "an unreadable clock (now_unix's fail-closed i64::MAX) reads as lapsed, \
             so it can only cost a download — never block an install"
        );
        assert!(
            !StageRefusal {
                digest: String::new(),
                ..memo(2)
            }
            .binds(sha256(""), "bb", 1_000),
            "a memo with no digest binds nothing"
        );
        // Another client's verdict binds nothing (a client-side fix ships as a new
        // atpkg), and neither does an older memo's empty client.
        let another = format!("{}+another", aterm_types::version::app_version());
        assert!(
            !StageRefusal {
                client: another.clone(),
                ..memo(2)
            }
            .binds(sha256("aa"), "bb", 1_000),
            "a memo another atpkg wrote binds nothing"
        );
        assert!(
            !StageRefusal {
                client: String::new(),
                ..memo(2)
            }
            .binds(sha256("aa"), "bb", 1_000),
            "a pre-field memo binds nothing"
        );
        // A held memo (a tree_root mismatch) has no cooldown: it binds from the first
        // verdict, at any clock, until the digests or the client move.
        let held = StageRefusal {
            held: true,
            ..memo(1)
        };
        assert!(
            held.binds(sha256("aa"), "bb", 1_000),
            "held from the first verdict"
        );
        assert!(
            held.binds(sha256("aa"), "bb", i64::MAX),
            "no cooldown to lapse"
        );
        assert!(
            !held.binds(sha256("dd"), "bb", 1_000),
            "a re-cut pin releases it"
        );
        assert!(
            !StageRefusal {
                client: another,
                ..held.clone()
            }
            .binds(sha256("aa"), "bb", 1_000),
            "a new client gets its attempt"
        );
    }

    /// A signer refusal judged bytes that matched their digests, so its memo binds from
    /// the first attempt and never lapses, but only while the digests hold, and never
    /// with no digest.
    #[test]
    fn a_signer_refusal_binds_until_the_digests_move() {
        let m = StageRefusal {
            alg: DigestAlg::Sha256,
            digest: "AA".into(),
            tree_root: "bb".into(),
            why: "signer refused: bin/claude is not signed by Developer ID team X".into(),
            at: 1_000,
            attempts: 1,
            signer: true,
            held: false,
            client: aterm_types::version::app_version().into(),
        };
        assert!(
            m.binds(sha256("aa"), "BB", 1_000),
            "the first refusal already binds"
        );
        assert!(
            m.binds(sha256("aa"), "bb", i64::MAX),
            "no cooldown to lapse"
        );
        assert!(
            !m.binds(sha256("dd"), "bb", 1_000),
            "a new sha256 is a new question"
        );
        assert!(
            !m.binds(sha256("aa"), "dd", 1_000),
            "a new tree_root is a new question"
        );
        assert!(
            !StageRefusal {
                digest: String::new(),
                ..m
            }
            .binds(sha256(""), "bb", 1_000)
        );
    }

    /// The signer kind survives the disk round trip, a digest memo stays a digest memo,
    /// and the clears take a signer memo exactly as they take any other.
    #[test]
    fn the_signer_refusal_memo_round_trips() {
        let h = temp_home("signer-refusal");
        let l = Layout { prefix: h.clone() };
        let build = l.build_dir("claude", 1_000_002_000_001_000_280);
        record_signer_refusal(&build, sha256("aa"), "bb", "signer refused: x", 1_000).unwrap();
        let m = stage_refusal(&build).unwrap();
        assert!(m.signer);
        assert!(!m.held, "a signer memo is its own kind, not a hold");
        assert_eq!(m.client, aterm_types::version::app_version());
        assert_eq!((m.attempts, m.why.as_str()), (1, "signer refused: x"));
        assert!(m.binds(sha256("aa"), "bb", i64::MAX));
        record_stage_refusal(&build, sha256("aa"), "bb", "asset sha256 mismatch", 2_000).unwrap();
        assert!(!stage_refusal(&build).unwrap().signer);
        record_signer_refusal(&build, sha256("aa"), "bb", "signer refused: x", 3_000).unwrap();
        clear_stage_refusals(&l, "claude");
        assert_eq!(stage_refusal(&build), None);
        let _ = std::fs::remove_dir_all(&h);
    }

    /// The refusal memo on disk: written beside the build, read back whole, its attempt
    /// count advanced while the digests hold and reset when they move, one per program,
    /// and taken by the clears. A malformed or foreign file reads as no memo.
    #[test]
    fn the_refusal_memo_round_trips_advances_and_is_one_per_program() {
        let h = temp_home("stage-refusal");
        let l = Layout { prefix: h.clone() };
        let build = l.build_dir("trust", 4900);
        assert_eq!(stage_refusal(&build), None, "nothing recorded yet");

        record_stage_refusal(
            &build,
            sha256("AA"),
            "bb",
            "asset sha256 mismatch:\nexpected aa",
            1_000,
        )
        .unwrap();
        let marker = build.with_file_name("4900.refused");
        assert!(marker.is_file(), "a sibling, outside the tree");
        assert!(!build.exists(), "the build it refused never came to exist");
        let m = stage_refusal(&build).unwrap();
        assert_eq!((m.digest.as_str(), m.tree_root.as_str()), ("AA", "bb"));
        assert_eq!(m.at, 1_000);
        assert_eq!(m.attempts, 1);
        assert!(!m.held, "a sha256 mismatch cools down; it is not held");
        assert_eq!(
            m.client,
            aterm_types::version::app_version(),
            "the memo names the client that wrote it"
        );
        let text = std::fs::read_to_string(&marker).unwrap();
        assert!(
            !text.contains("hold="),
            "a cooled-down memo carries no hold line: {text}"
        );
        assert_eq!(
            m.why, "asset sha256 mismatch: expected aa",
            "one line: the newline is folded"
        );

        // The same digests again: the attempt count advances, lengthening the cooldown.
        record_stage_refusal(&build, sha256("aa"), "BB", "again", 50_000).unwrap();
        let m = stage_refusal(&build).unwrap();
        assert_eq!((m.attempts, m.at), (2, 50_000));
        // Digests that moved are a different question: the count starts again.
        record_stage_refusal(&build, sha256("cc"), "bb", "new pin, new verdict", 60_000).unwrap();
        assert_eq!(stage_refusal(&build).unwrap().attempts, 1);
        // A held verdict (tree_root mismatch) round-trips its hold line and binds at
        // once, whatever the clock.
        record_held_refusal(&build, sha256("cc"), "bb", "tree_root mismatch", 70_000).unwrap();
        let m = stage_refusal(&build).unwrap();
        assert!(m.held, "the hold line reads back");
        assert_eq!(m.attempts, 2, "the same digests: the count still advances");
        assert!(
            m.binds(sha256("cc"), "bb", i64::MAX),
            "held: no cooldown to lapse"
        );
        assert!(
            std::fs::read_to_string(&marker)
                .unwrap()
                .contains("\nhold=1\n"),
            "the memo carries the hold"
        );

        // One memo per program: recording the next build's reclaims the last build's.
        let next = l.build_dir("trust", 4901);
        record_stage_refusal(&next, sha256("dd"), "ee", "and again", 70_000).unwrap();
        assert!(
            !marker.exists(),
            "the superseded build's memo is reclaimed, not left to accumulate"
        );
        assert!(stage_refusal(&next).is_some());

        // Malformed/foreign content reads as no memo — the fail-open direction.
        std::fs::write(next.with_file_name("4901.refused"), b"nonsense\nat=1\n").unwrap();
        assert_eq!(
            stage_refusal(&next),
            None,
            "a file that does not open with the schema line is not read"
        );

        // The three clears.
        record_stage_refusal(&next, sha256("dd"), "ee", "once more", 80_000).unwrap();
        clear_stage_refusal(&next);
        assert_eq!(stage_refusal(&next), None);
        clear_stage_refusal(&next); // idempotent
        record_stage_refusal(&next, sha256("dd"), "ee", "once more", 90_000).unwrap();
        clear_stage_refusals(&l, "trust");
        assert_eq!(stage_refusal(&next), None, "the explicit door forgets it");
        record_stage_refusal(&next, sha256("dd"), "ee", "once more", 95_000).unwrap();
        std::fs::create_dir_all(next.join("bin")).unwrap();
        discard_build(&next);
        assert_eq!(stage_refusal(&next), None, "the discard takes it too");
        let _ = std::fs::remove_dir_all(&h);
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

    /// The system prefix shape: outside `$HOME` is admissible when every existing ancestor
    /// is root-owned and not group/other-writable.
    #[cfg(unix)]
    #[test]
    fn root_owned_prefix_outside_home_is_accepted() {
        use std::os::unix::fs::MetadataExt as _;
        let home = temp_home("sysprefix");
        // Precondition, asserted rather than assumed: /usr/lib must really be
        // root-owned and not group/other-writable on this machine.
        let Ok(meta) = std::fs::symlink_metadata("/usr/lib") else {
            let _ = std::fs::remove_dir_all(&home);
            aterm_uds::host::could_not_run("no /usr/lib here to stand in as a system chain");
        };
        if meta.uid() != 0 || meta.mode() & 0o022 != 0 {
            let _ = std::fs::remove_dir_all(&home);
            aterm_uds::host::could_not_run(
                "/usr/lib here is not root-owned and closed to group and other writes",
            );
        }
        // A non-existent leaf is the tail the installer creates (as root).
        let prefix = Path::new("/usr/lib/aterm-pkg-system-prefix-test");
        // The trust property this test is named for holds for every caller.
        assert!(
            system_chain_trusted(prefix),
            "a fully root-owned chain outside $HOME is a trusted system prefix"
        );
        // What `vet_prefix` returns also depends on whether this caller can use it: as root
        // the prefix comes back; as an ordinary user it falls back to the default.
        let expected = if caller_can_use_prefix(prefix) {
            prefix.to_path_buf()
        } else {
            default_prefix(&home)
        };
        assert_eq!(vet_prefix(Some(prefix), &home), expected);
        let _ = std::fs::remove_dir_all(&home);
    }

    /// The system shape is an AND over the whole chain: one world-writable ancestor
    /// (the system's temporary root, root-owned and `0777` or `1777`: macOS's
    /// `/private/tmp`, elsewhere `/tmp`) fails the prefix closed to the default even
    /// though root would create the leaf.
    #[cfg(unix)]
    #[test]
    fn world_writable_system_ancestor_is_refused() {
        use std::os::unix::fs::MetadataExt as _;
        let home = temp_home("wwancestor");
        // A real directory, not macOS's `/tmp` link to `/private/tmp`.
        let Some((root, meta)) = ["/private/tmp", "/tmp"].into_iter().find_map(|root| {
            std::fs::symlink_metadata(root)
                .ok()
                .filter(std::fs::Metadata::is_dir)
                .map(|meta| (root, meta))
        }) else {
            let _ = std::fs::remove_dir_all(&home);
            aterm_uds::host::could_not_run("no /private/tmp or /tmp directory here");
        };
        if meta.uid() != 0 || meta.mode() & 0o022 == 0 {
            let _ = std::fs::remove_dir_all(&home);
            aterm_uds::host::could_not_run(format!(
                "{root} here is not the root-owned world-writable fixture this test needs"
            ));
        }
        let prefix = Path::new(root).join("aterm-pkg-should-not-be-trusted");
        let prefix = prefix.as_path();
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
        // the would-be prefix leaf.
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

    /// The env that turns [`probe_a_prefix_made_under_an_open_umask_has_no_shared_ancestor`]
    /// on: the synthetic home it creates under.
    #[cfg(unix)]
    const OPEN_UMASK_ENV: &str = "ATPKG_STORE_OPEN_UMASK_HOME";
    /// That probe's name, as `--exact` wants it.
    #[cfg(unix)]
    const OPEN_UMASK_PROBE: &str =
        "store::tests::probe_a_prefix_made_under_an_open_umask_has_no_shared_ancestor";

    /// The child half of [`a_prefix_made_under_an_open_umask_has_no_shared_ancestor`]:
    /// under umask `002`, `ensure_dir(prefix)` makes the Linux default prefix's missing
    /// ancestors (`.local`, `.local/share`, `.local/share/aterm`) `0755` and the prefix
    /// `0700`; a configured prefix's the same, and the next [`vet_prefix`] admits the
    /// prefix it made (the default is not the one to ask: `vet_prefix` falls back to it,
    /// so it comes back whatever its chain); a system-shaped prefix's missing ancestors are
    /// `0755` and the prefix `0755`. Outside that child (the env unset) it does nothing:
    /// the umask is the whole process's.
    #[cfg(unix)]
    #[test]
    fn probe_a_prefix_made_under_an_open_umask_has_no_shared_ancestor() {
        let Some(home) = std::env::var_os(OPEN_UMASK_ENV) else {
            return;
        };
        let home = PathBuf::from(home);
        // SAFETY: `umask(2)` swaps this process's file-creation mask and cannot fail; this
        // process runs this one test (`--exact`), so no other create sees the mask.
        unsafe { libc::umask(0o002) };
        // Octal text, so a failure reads `775`, not `509`.
        let mode = |rel: &str| {
            let meta = std::fs::symlink_metadata(home.join(rel)).unwrap();
            format!("{:o}", meta.permissions().mode() & 0o7777)
        };
        let default = Layout {
            prefix: home.join(".local/share/aterm/pkg"),
        };
        default.ensure_dir(&default.prefix).unwrap();
        for made in [".local", ".local/share", ".local/share/aterm"] {
            assert_eq!(mode(made), "755", "{made}: made under umask 002");
        }
        assert_eq!(mode(".local/share/aterm/pkg"), "700", "the $HOME prefix");
        let configured = Layout {
            prefix: home.join("work/aterm/pkg"),
        };
        configured.ensure_dir(&configured.prefix).unwrap();
        for made in ["work", "work/aterm"] {
            assert_eq!(mode(made), "755", "{made}: made under umask 002");
        }
        assert_eq!(mode("work/aterm/pkg"), "700", "the configured prefix");
        assert_eq!(
            vet_prefix(Some(&configured.prefix), &home),
            configured.prefix,
            "the prefix this pass made is refused by the next one"
        );
        let shared = Layout {
            prefix: home.join("opt/aterm/pkg"),
        };
        system_shape::force(&shared.prefix);
        shared.ensure_dir(&shared.prefix).unwrap();
        for made in ["opt", "opt/aterm"] {
            assert_eq!(mode(made), "755", "{made}: made under umask 002");
        }
        assert_eq!(mode("opt/aterm/pkg"), "755", "the system prefix");
    }

    /// AT3's umask finding, one level up (2026-10-17): the directories atpkg makes on the
    /// way to its prefix were `create_dir_all`'s `0777 & ~umask`, so the first pass under
    /// umask `002` (a user-private-group login's default) left `~/.local/share/aterm`
    /// `0775`. `targo trust` refuses every toolchain installed below a group-writable
    /// directory, and [`vet_prefix`] refused a configured prefix whose ancestors the
    /// previous pass had made. The umask is process-wide, so the creates run in a child of
    /// this test binary that runs only
    /// [`probe_a_prefix_made_under_an_open_umask_has_no_shared_ancestor`].
    #[cfg(unix)]
    #[test]
    fn a_prefix_made_under_an_open_umask_has_no_shared_ancestor() {
        let home = temp_home("open-umask");
        let out = aterm_uds::spawnfd::command(std::env::current_exe().unwrap())
            .args(["--exact", "--nocapture", OPEN_UMASK_PROBE])
            .env(OPEN_UMASK_ENV, home.as_os_str())
            .stdin(aterm_uds::spawnfd::Stdio::null())
            .output()
            .unwrap();
        let stdout = String::from_utf8_lossy(&out.stdout);
        assert!(
            out.status.success() && stdout.contains("1 passed"),
            "the prefix under umask 002: {}\n{stdout}\n{}",
            out.status,
            String::from_utf8_lossy(&out.stderr)
        );
    }

    /// A trusted system prefix this user cannot write falls back to the default rather
    /// than failing every verb on `store.lock`
    /// (docs/AUDIT-nux-first-open-toolchain-2026-08-31.md). Runs only as a non-root user.
    #[cfg(unix)]
    #[test]
    fn trusted_system_prefix_that_we_cannot_write_falls_back_to_default() {
        aterm_uds::host::could_not_run_as_root("a prefix this user cannot write");
        let home = temp_home("unwritable-system");
        // `/usr/lib` is outside $HOME, root-owned and not group/other-writable (trusted),
        // and not writable by an ordinary user (`/usr/local` is a Homebrew user's own on an
        // Intel Mac).
        let prefix = PathBuf::from("/usr/lib/aterm-nux-probe/pkg");
        if !system_chain_trusted(&prefix) {
            let _ = std::fs::remove_dir_all(&home);
            aterm_uds::host::could_not_run(
                "/usr/lib here is not a trusted system chain (root-owned, closed to writes)",
            );
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

    /// A prefix the caller cannot write is still usable when its shims serve, even where
    /// the user may exec but not stat the store (`exists()` would answer `false` there).
    /// Only the permission to look is withdrawn; the shim and its target would still exec.
    #[cfg(unix)]
    #[test]
    fn an_unwritable_prefix_whose_shims_cannot_be_stat_ed_is_still_usable() {
        use std::os::unix::fs::PermissionsExt as _;
        aterm_uds::host::could_not_run_as_root("a mode-0 prefix");
        let prefix = crate::testdir::dir("atpkg-shared-");
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

    #[test]
    fn shim_names_collision_and_shape_policy() {
        // Sensitive commands are refused (case-insensitively).
        for bad in ["sudo", "SSH", "git", "sh", "env", "rustc", "Codesign"] {
            assert!(!shim_allowed(bad), "{bad} must be refused a shim");
        }
        // A shell is never shimmed, under any spelling, as an alias's base, or in its preview
        // and versioned forms (design S13).
        for shell in SHELLS {
            for name in [
                (*shell).to_string(),
                shell.to_ascii_uppercase(),
                format!("{ALIAS_PREFIX}{shell}"),
                format!("{shell}-preview"),
                format!("{shell}5"),
                format!("{shell}-7.4"),
            ] {
                assert!(
                    !shim_allowed(&name),
                    "the shell {name} must be refused a shim"
                );
            }
        }
        for named in ["pwsh", "elvish", "xonsh", "mksh", "powershell", "busybox"] {
            assert!(SHELLS.contains(&named), "{named} is on the list");
        }
        // A lookalike the case-insensitive volume answers for a shell's name (LONG S folds
        // to `s`, the `fi` ligature to `fi`, KELVIN SIGN to `k`) is no tool name either.
        for lookalike in [
            "pw\u{17F}h",
            "fi\u{17F}h",
            "\u{FB01}sh",
            "xon\u{17F}h",
            "elvi\u{17F}h",
            "\u{212A}sh",
            "\u{17F}h",
            "ba\u{17F}h",
            "alab-pw\u{17F}h",
            "caf\u{E9}",
        ] {
            assert!(
                !shim_allowed(lookalike),
                "{lookalike:?} is not plain ASCII and must be refused a shim"
            );
        }
        // The control: a name that only begins or ends like a shell, or carries a version
        // that is no shell's, is a tool name.
        for ok in [
            "nushell-lsp",
            "dashboard",
            "pwsh-fmt",
            "fisher",
            "kshrc-lint",
            "nuget",
            "ashes",
            "rcfile",
            "bash-lsp",
            "zsh-5x",
            "ion-lint",
            "trust2",
        ] {
            assert!(shim_allowed(ok), "{ok} is no shell and should be allowed");
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

    /// `alab-<x>` is admitted exactly when `<x>` is: the deny-list applies to the base,
    /// the bare prefix has no base, and the prefix nests once, never twice.
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
        // Hyphenated tools alias like any other; the renderings carry the whole alias.
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

    /// The suffix rule with Windows' real suffixes, which no Unix runner reaches through
    /// `SHIM_SUFFIX` (`""`).
    #[test]
    fn the_suffix_rule_round_trips_and_never_doubles_on_a_suffixed_platform() {
        // Windows' real pair: a shim and the executable it forwards to are different files.
        assert_eq!(with_suffix("ay", ".cmd"), "ay.cmd");
        assert_eq!(with_suffix("ay", ".exe"), "ay.exe");
        assert_ne!(with_suffix("ay", ".cmd"), with_suffix("ay", ".exe"));

        // The round trip a `bin/` scan performs: file name -> logical -> file name.
        assert_eq!(without_suffix(&with_suffix("ay", ".cmd"), ".cmd"), "ay");
        // Strip first, or re-appending writes `bin/ay.cmd.cmd` beside the live shim.
        assert_eq!(
            with_suffix(&with_suffix("ay", ".cmd"), ".cmd"),
            "ay.cmd.cmd"
        );

        // Total, not fallible: an unsuffixed entry reads back unchanged …
        assert_eq!(without_suffix("ay", ".cmd"), "ay");
        // … and the shim suffix is not the exe suffix: a `.exe` in `bin/` keeps its name.
        assert_eq!(without_suffix("ay.exe", ".cmd"), "ay.exe");

        // The empty suffix (Unix) is the identity in both directions.
        assert_eq!(with_suffix("ay", ""), "ay");
        assert_eq!(without_suffix("ay", ""), "ay");
    }

    /// A marker written by the other slice of the universal binary does not vouch for the
    /// build to this one, and a native re-install makes it vouch again.
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

    /// A marker beside a build whose `bin/` is gone (the rest of the tree intact) is
    /// refuted by the `bin/` inventory it recorded.
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

        // Non-vacuity: the record is there and names the tools.
        let text = std::fs::read_to_string(ready_marker_path(&build).unwrap()).unwrap();
        assert_eq!(
            recorded_contents(&text),
            Some("bin:targo,trustc"),
            "sorted, comma-joined, so the record is a function of the tree: {text:?}"
        );

        // Gut it as an interrupted `remove_dir_all` does: `bin` goes, `lib` stays, the
        // marker is untouched.
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

    /// A `bin/` entry that is a symlink is recorded and checked by its own name: a dangling
    /// link still stands (doctor reports it); the link itself going refutes.
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
        // The link's target goes; the link itself stands. Still complete.
        std::fs::remove_file(build.join("bin").join("tippy")).unwrap();
        assert!(
            !build_is_complete(&build),
            "`tippy` was recorded in its own right and is gone"
        );
        std::fs::write(build.join("bin").join("tippy"), b"x").unwrap();
        assert!(build_is_complete(&build));
        // …and the link itself going is a refutation.
        std::fs::remove_file(build.join("bin").join("clippy")).unwrap();
        assert!(!build_is_complete(&build));
        let _ = std::fs::remove_dir_all(&home);
    }

    /// A build marked with no `bin/` records `contents=none`: never refuted for having no
    /// `bin/`, and distinct from no record.
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

    /// A marker for a build dir that is not there does not vouch: `mark_build_ready` has
    /// no existence precondition, so the predicate refuses it.
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

    /// The contents rule, driven directly, so it cannot pass without comparing anything.
    #[test]
    fn contents_still_stand_refutes_only_a_proven_absence() {
        let home = temp_home("readycontents");
        let build = home.join("b");
        std::fs::create_dir_all(build.join("bin")).unwrap();
        std::fs::write(build.join("bin").join("ay"), b"x").unwrap();
        assert!(contents_still_stand(&build, READY_CONTENTS_NONE));
        assert!(contents_still_stand(&build, "bin:ay"));
        assert!(!contents_still_stand(&build, "bin:ay,ny"));
        // A value a later atpkg might write is not a refutation: unknown is not refuted.
        assert!(contents_still_stand(&build, "tree-root:deadbeef"));
        // …and an unrecognised value does not accidentally match the `bin:` branch.
        assert!(contents_still_stand(&build, "bin"));
        let _ = std::fs::remove_dir_all(&home);
    }

    /// `discard_build` takes the marker down first: with the tree made unremovable (its
    /// own permissions, its parent still writable), the build reads as incomplete when the
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
        // No write permission on `bin/`: `remove_dir_all` fails inside the tree, the
        // interrupt in a form a test can arrange.
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

    /// `unmark_program_builds` takes down every build's marker and touches nothing else.
    #[test]
    fn unmark_program_builds_unmarks_every_build_and_only_builds() {
        let home = temp_home("unmarkall");
        let prog = home.join("store").join("trust");
        for n in [8590u64, 8595] {
            let b = prog.join(n.to_string());
            std::fs::create_dir_all(b.join("bin")).unwrap();
            std::fs::write(b.join("bin").join("trustc"), b"x").unwrap();
            mark_build_ready(&b).unwrap();
        }
        // A non-numeric sibling directory, and a sidecar that is not a readiness marker.
        std::fs::create_dir_all(prog.join("8595.incoming-7")).unwrap();
        std::fs::write(prog.join("8595.provenance"), b"src\n").unwrap();
        let recording = aterm_durable::seam::Recording::start();
        unmark_program_builds(&prog);
        // Windows opens no directory to flush (`sync_dir_ordered` is best-effort there).
        let flushed: &[aterm_durable::Flush] = if cfg!(windows) {
            &[]
        } else {
            &[aterm_durable::Flush::Ordered]
        };
        assert_eq!(
            recording.take(),
            flushed,
            "every marker down before any tree goes: one ordered directory flush \
             (aterm-durable's counting seam)"
        );
        drop(recording);
        for n in [8590u64, 8595] {
            let b = prog.join(n.to_string());
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

    /// The accept rule, driven directly: the tests above see only the platform this test
    /// binary was compiled for.
    #[test]
    fn ready_text_accepts_only_an_absent_or_matching_record() {
        assert!(ready_text_accepts("ok\n", "aarch64-macos"));
        // A marker with no `ok` line is a crash artefact, never a legacy marker: empty after
        // a rename without fsync, a NUL run from a committed but unwritten inode.
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
        // The hazard case: the Intel slice's marker, read by the native arm64 slice.
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
        // An empty value or an unrecognised key is no record, which is accept.
        assert!(ready_text_accepts("ok\nplatform=\n", "aarch64-macos"));
        assert!(ready_text_accepts(
            "ok\narch=x86_64-macos\n",
            "aarch64-macos"
        ));
    }

    /// A marker a crash left without a readable `ok` line (empty, NUL-filled, non-UTF-8,
    /// torn) does not vouch for its build, through the real predicate.
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

        // Non-vacuity: the real writer's marker vouches, and so does a legacy bare `ok`.
        mark_build_ready(&build).unwrap();
        assert!(build_is_complete(&build));
        std::fs::write(&marker, b"ok\n").unwrap();
        assert!(build_is_complete(&build));
        let _ = std::fs::remove_dir_all(&home);
    }

    /// The flush reaches every regular file and every directory of the staged tree and
    /// never follows a symlink out of it.
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
        // Windows cannot open a directory as a file, so the directory arm flushes none
        // there (NTFS journals directory metadata itself).
        let dirs_expected = if cfg!(windows) { 0 } else { 4 };
        assert_eq!(
            synced.dirs, dirs_expected,
            "the root, bin, lib and lib/nested — where a directory can be opened"
        );

        // A tree that is not there is an error: "nothing to flush" must never read as
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
        // bin_dir lands at the end (so it can't shadow system commands earlier on PATH).
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
