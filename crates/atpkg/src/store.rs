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
    /// It exists because one config bit was doing two unrelated jobs. `[packages].auto_install`
    /// answers "may atpkg pull a multi-GB toolchain onto a machine that has never had one?",
    /// which is a genuine consent question and rightly defaults FALSE. But the update pass
    /// was ALSO reading it to answer "should a machine that already runs this toolset keep
    /// that set complete?" — and with the default answer being no, a program published to
    /// the index AFTER a user installed simply never arrived. Their toolchain quietly stopped
    /// being the whole toolchain as the suite grew, which is the opposite of what a
    /// distribution channel is for. Adoption separates the two: consent is asked once, and
    /// alignment thereafter is not a new consent event.
    ///
    /// CLEARED by `uninstall` (see `crate::ops::uninstall`'s caller): removing a managed
    /// program is an explicit act, and set-completion must never fight it by reinstalling on
    /// the next pass. The durable way to drop ONE program while staying adopted is
    /// `[packages].exclude`, which the default-set planner already honours.
    #[must_use]
    pub fn adopted(&self) -> PathBuf {
        self.prefix.join("adopted")
    }

    /// `provisional` — build numbers the batteries-included seed laid down that GC
    /// must not retain as a rollback target once superseded
    /// ([`crate::provisional`]). Absent means "nothing provisional", which is the
    /// pre-existing retention behaviour.
    #[must_use]
    pub fn provisional(&self) -> PathBuf {
        self.prefix.join("provisional")
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

    /// `optin/` — the per-program EXTRAS opt-in markers directory. One `0600` regular file
    /// per extra ([`crate::manifest::Program::extra`]) this machine asked for by name —
    /// `aterm pkg install codex`, the typed-name consent stub, Settings — so the default-set
    /// pass unions it into the wanted set ([`crate::manifest::Index::installable_with_optins`])
    /// on every later pass. The marker is the consent record: it is written only by an
    /// explicit answer, and it is what ADDS work (the bump file stays reorder-only).
    ///
    /// Cleared by `uninstall <name>` and by `uninstall --all` (a decline): removing an extra
    /// is withdrawing the opt-in, and a declined machine wants none of the set.
    #[must_use]
    pub fn optin_dir(&self) -> PathBuf {
        self.prefix.join("optin")
    }

    /// `optin/<program>` — one opt-in marker. Only ever joined with a [`shim_allowed`]-shape
    /// name; [`Self::record_optin`] gates the name before creating one.
    #[must_use]
    pub fn optin_marker(&self, program: &str) -> PathBuf {
        self.optin_dir().join(program)
    }

    /// Whether an opt-in marker exists for `program`: a REGULAR, non-symlink file at
    /// [`Self::optin_marker`] (the same symlink-refusing rule the other prefix markers
    /// follow — a link planted there is not a consent this machine recorded). A name that
    /// could never be a program answers `false` without touching the filesystem.
    #[must_use]
    pub fn optin_exists(&self, program: &str) -> bool {
        if ToolName::new(program).is_none() {
            return false;
        }
        std::fs::symlink_metadata(self.optin_marker(program)).is_ok_and(|m| m.file_type().is_file())
    }

    /// Record an opt-in for `program`. Idempotent; the payload is documentation for whoever
    /// finds the file, never something read back — consent is the marker's EXISTENCE.
    ///
    /// # Errors
    /// The name is not a [`shim_allowed`] shape (never joined onto the path), the directory
    /// could not be created/hardened, or the marker could not be written.
    pub fn record_optin(&self, program: &str) -> std::io::Result<()> {
        if ToolName::new(program).is_none() {
            return Err(std::io::Error::new(
                std::io::ErrorKind::InvalidInput,
                "opt-in name is not an installable program name",
            ));
        }
        if self.optin_exists(program) {
            return Ok(());
        }
        let dir = self.optin_dir();
        self.ensure_dir(&dir)?;
        let path = self.optin_marker(program);
        // Refuse to write THROUGH a planted symlink: `open_create_write` creates the file
        // fresh; if something non-regular already sits there, leave it and fail closed.
        if std::fs::symlink_metadata(&path).is_ok() {
            return Err(std::io::Error::new(
                std::io::ErrorKind::AlreadyExists,
                "opt-in marker path is occupied by something that is not a marker",
            ));
        }
        let mut f = crate::platform::open_create_write(&path, 0o600)?;
        use std::io::Write as _;
        f.write_all(
            b"# This machine opted in to this EXTRA (not a default-set member) by name.
              # The default-set pass keeps it installed and current while this file exists.
              # Removed by `aterm pkg uninstall <name>` or `aterm pkg uninstall --all`.
",
        )
    }

    /// Forget the opt-in for `program` (a no-op when none is recorded). Only a REGULAR
    /// file is ever removed — a planted symlink at the marker path is left alone.
    pub fn clear_optin(&self, program: &str) {
        if ToolName::new(program).is_none() {
            return;
        }
        let path = self.optin_marker(program);
        if std::fs::symlink_metadata(&path).is_ok_and(|m| m.file_type().is_file()) {
            let _ = std::fs::remove_file(&path);
        }
    }

    /// Forget EVERY opt-in (a decline: `uninstall --all`). Regular files only, as above.
    pub fn clear_all_optins(&self) {
        for name in self.optins() {
            self.clear_optin(&name);
        }
    }

    /// The extras this machine opted in to: every regular-file marker under `optin/` whose
    /// name is a [`shim_allowed`] shape. Sorted (a `BTreeSet`), so the union into the wanted
    /// set is deterministic. Missing directory ⇒ empty.
    #[must_use]
    pub fn optins(&self) -> std::collections::BTreeSet<String> {
        let mut out = std::collections::BTreeSet::new();
        let Ok(entries) = std::fs::read_dir(self.optin_dir()) else {
            return out;
        };
        for entry in entries.flatten() {
            let name = entry.file_name();
            let Some(name) = name.to_str() else {
                continue;
            };
            if self.optin_exists(name) {
                out.insert(name.to_string());
            }
        }
        out
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

    /// `retired/<program>` — one retirement marker (name-gated like [`Self::optin_marker`]).
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
fn ready_text_accepts(text: &str, running: &str) -> bool {
    match recorded_platform(text) {
        None => true,
        Some(recorded) => recorded == running,
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
    match std::fs::read_to_string(&marker) {
        Ok(text) => ready_text_accepts(&text, &running_platform()),
        // Present, but not readable AS TEXT: a directory planted at the marker path, a
        // permissions oddity, a filesystem handing back non-UTF-8. `exists()` — the whole
        // of this predicate before the platform record — answered `true` for all of those,
        // so keep that answer. This check must not start reporting long-installed builds
        // as missing because of a byte it never used to read.
        Err(e) if e.kind() != std::io::ErrorKind::NotFound => true,
        Err(_) => false,
    }
}

/// Atomically mark `build_dir` complete (temp + rename, so a crash during the write
/// leaves NO marker rather than a half-written one). Call as the LAST staging step.
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
    let mut body = String::from("ok\n");
    body.push_str(READY_PLATFORM_KEY);
    body.push_str(&running_platform());
    body.push('\n');
    // `fs::write` via `call2`: Trust-gate name-matching workaround — see `lib.rs::call2`.
    crate::call2(std::fs::write, &tmp, body.as_bytes())?;
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
    let build = build.parse::<u64>().ok()?;
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
    let Ok(build) = name.parse::<u64>() else {
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
/// re-marking would promote a tree nothing can vouch for.
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
/// first recovering the swap window ([`recover_interrupted_swap`]) so a crash there does not
/// get swept as debris.
///
/// Safe to sweep unconditionally because every mutating verb holds the store-wide writer
/// lock ([`crate::lock::try_lock_store`]): if scratch exists when we get here, its owner is
/// gone. Without this sweep the scratch is INVISIBLE to reclamation — `list_installed`
/// only counts numeric, marker-bearing dirs, and GC only reclaims what `list_installed`
/// returns — so a killed install would strand its half-extracted tree on disk forever.
///
/// Non-directories at a scratch path are removed too. `remove_dir_all` fails on a regular
/// file and GC's pass filters to directories, so such an entry was reclaimed by NEITHER
/// sweeper — it leaked forever, and while it sat there it blocked every swap of that build
/// by a process holding the same pid.
pub(crate) fn sweep_stage_scratch(build_dir: &Path) {
    recover_interrupted_swap(build_dir);
    for (_, path) in scratch_siblings(build_dir) {
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

/// Discard a build entirely: remove its tree AND its sibling completeness marker (the
/// inverse of a stage + [`mark_build_ready`]). Used to clean up a build that a transaction
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
    let _ = std::fs::remove_dir_all(build_dir);
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
    Some(Layout {
        prefix: vet_prefix(configured, &home),
    })
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
    resolve(
        crate::config::load()
            .prefix_path(aterm_types::dirs::home_dir().as_deref())
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
        return if system_chain_trusted(p) {
            p.to_path_buf()
        } else {
            default
        };
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

    /// The EXTRAS opt-in markers: recorded by name (idempotent, `0600`, regular file),
    /// listed, cleared one at a time and all at once; a name that could never be a
    /// program is refused rather than joined onto the path; a planted symlink is never a
    /// consent, never written through, never removed.
    #[test]
    fn optin_markers_record_list_clear_and_refuse_links() {
        let h = temp_home("optin");
        let l = Layout { prefix: h.clone() };
        assert!(l.optins().is_empty());
        assert!(!l.optin_exists("codex"));
        l.record_optin("codex").unwrap();
        l.record_optin("codex").unwrap(); // idempotent
        assert!(l.optin_exists("codex"));
        assert_eq!(l.optin_marker("codex"), h.join("optin").join("codex"));
        assert_eq!(l.optins(), ["codex".to_string()].into_iter().collect());
        #[cfg(unix)]
        {
            let mode = std::fs::metadata(l.optin_marker("codex"))
                .unwrap()
                .permissions()
                .mode()
                & 0o777;
            assert_eq!(mode, 0o600, "a marker is private state");
        }
        for bad in ["", ".", "..", "../evil", "a/b", "a\\b", "sudo", "git"] {
            assert!(l.record_optin(bad).is_err(), "{bad:?} must be refused");
            assert!(!l.optin_exists(bad));
        }
        assert!(!h.join("optin").join("evil").exists());
        #[cfg(unix)]
        {
            let target = h.join("elsewhere");
            std::fs::write(&target, b"not a marker").unwrap();
            std::os::unix::fs::symlink(&target, l.optin_marker("claude")).unwrap();
            assert!(!l.optin_exists("claude"), "a planted link is not a consent");
            assert!(!l.optins().contains("claude"));
            assert!(
                l.record_optin("claude").is_err(),
                "never write through a planted link"
            );
            l.clear_optin("claude");
            assert!(
                std::fs::symlink_metadata(l.optin_marker("claude")).is_ok(),
                "a link is never what clear removes"
            );
            assert_eq!(std::fs::read(&target).unwrap(), b"not a marker");
        }
        l.clear_optin("codex");
        assert!(!l.optin_exists("codex"));
        l.clear_optin("codex"); // absent: a no-op
        l.record_optin("codex").unwrap();
        l.record_optin("gh").unwrap();
        assert_eq!(
            l.optins(),
            ["codex".to_string(), "gh".to_string()]
                .into_iter()
                .collect()
        );
        l.clear_all_optins();
        assert!(l.optins().is_empty());
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
        assert_eq!(
            vet_prefix(Some(prefix), &home),
            prefix.to_path_buf(),
            "a fully root-owned chain outside $HOME is a trusted system prefix"
        );
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
        assert!(ready_text_accepts("", "aarch64-macos"));
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
