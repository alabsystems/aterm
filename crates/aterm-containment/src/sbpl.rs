// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0
// Author: Andrew Yates

//! macOS Seatbelt (SBPL) profile generation for the OS-sandbox actuator.
//!
//! The enforcing OS-level sandbox [`crate::actuator`] applies. For a resolved
//! [`Capabilities`](crate::Capabilities) set whose
//! [`NetworkCapability`](crate::NetworkCapability) is
//! [`None`](crate::NetworkCapability::None) (`Containment` mode), it returns an
//! SBPL string the spawn seam hands to `/usr/bin/sandbox-exec -p <sbpl>`, so the
//! kernel enforces it on the child shell and everything it runs.
//!
//! ## What the profile enforces (`ATERM_DESIGN` §0.1 / §5.6)
//!
//! Only a network-denied set gets a profile: `Containment`
//! ([`NetworkCapability::None`](crate::NetworkCapability::None) with
//! [`FsCapability::TmpOnly`](crate::FsCapability::TmpOnly)). Its rules, in order
//! (the later matching rule wins):
//!
//! 1. **`(allow default)`** — the base, so dyld, `path_helper`, the user's rc
//!    files, `/dev/tty` and every READ outside the deny sets keep working. A
//!    blanket `(deny file-read*)` breaks `$SHELL`, so reads are scoped by deny
//!    sets, not by an allowlist (owner, 2026-09-26).
//! 2. **`(deny network*)`** — every socket domain, outbound and inbound.
//!    **`(deny mach-priv-task-port)`**, then `(allow … (target self))`: no task
//!    port but the caller's own, so a contained `lldb`/`debugserver` cannot take
//!    control of (or read) a same-user process outside the sandbox — silently, once
//!    macOS developer mode is on (`docs/DESIGN-macos-debugger-access-2026-09-29.md`
//!    O4). The one operation covers all four flavors — `task_for_pid`,
//!    `task_read_for_pid`, `task_inspect_for_pid` and `task_name_for_pid` — and
//!    a contained process's own children too (measured on macOS 26.6).
//!    **`(deny mach-lookup)`, then `(allow mach-lookup …)` for
//!    [`MACH_LOOKUP_ALLOWLIST`] alone** — deny by default (since 2026-10-09). A
//!    mach service is a daemon outside the sandbox that does work for whoever
//!    looks it up, and the file and network rules cannot see that work: Launch
//!    Services and the Apple Events daemon start or drive a process that does
//!    not inherit the sandbox (`open`, `osascript`), the `ServiceManagement`
//!    daemon registers a login item, the preferences daemon writes
//!    `~/Library/Preferences`, the background-task agent edits login items, the
//!    pasteboard hands data to every other app. Under `(allow default)` all of
//!    them were reachable, and a denylist of the ones found left the rest open
//!    by construction, so every lookup is refused (`kr` 1100), the XPC services
//!    bundled in frameworks included, except the two directory queries a shell
//!    needs to name its user and groups. Measured on macOS 26.6 with every
//!    lookup refused (the per-tool table is the P1 entry in
//!    `docs/HARDCORE_BACKLOG.md`): a `/bin/sh` pipeline, an interactive and a
//!    login zsh with the user's rc files, bash, git (a `file://` clone too),
//!    python3 (`ssl`, `socket.getaddrinfo("localhost")`), targo, trustc and a
//!    `targo` build, make, xcrun and clang, ad-hoc `codesign` then exec, and
//!    `dscacheutil` all still work; the only loss is the user and group names,
//!    which the list restores. Refused on purpose, with what degrades: the
//!    security daemon (`security` lists no keychain certificate; the system
//!    roots stay readable as `/etc/ssl/cert.pem`), the pasteboard (`pbcopy` and
//!    `pbpaste` fail), Spotlight (`mdfind` finds nothing), the preferences
//!    daemon (`defaults read` still reads the plist files; nothing writes),
//!    `notifyd`, `logd` and `diagnosticd` (a contained program's `os_log`
//!    messages are dropped), `trustd`, `configd` and every GUI service. Each
//!    refusal is reported in the unified log (`log stream --predicate 'sender ==
//!    "Sandbox"'` prints `<program>(<pid>) deny(1) mach-lookup <name>`).
//!    launchd itself is reached through the bootstrap port, not by a lookup, so
//!    this rule does not cover it, and what a contained process may ask of it
//!    is launchd's own policy; the posture therefore claims "no mach service
//!    lookup", not "no system service". Measured on macOS 26.6: launchd refuses
//!    `launchctl bootstrap` and `submit` from inside the profile, and refuses
//!    the domain environment (`unsetenv`), the legacy `list`, `start` and
//!    `stop` to any sandboxed caller, an `(allow default)` one included, while
//!    it answers the reads (`print`, `print-disabled`, `blame`, `getenv`). The
//!    verbs that act on a real user job (`kickstart`, `bootout`,
//!    `enable`/`disable`, `setenv`) are unmeasured: the dated follow-up in the
//!    P1 entry.
//! 3. **WRITE confinement** (`TmpOnly` only) — [`PREFERENCE_WRITE_DENY`] (the
//!    preferences daemon writes `~/Library/Preferences` for its caller and
//!    consults that operation, not the file rules), then `(deny file-write*)`
//!    and `(allow file-write* …)` for [`WRITABLE_ROOTS`] (`/private/tmp`,
//!    `/private/var/tmp`, `/dev`), the user's `$TMPDIR` under
//!    [`USER_TEMP_PARENT`] ([`admitted_user_temp`]) and the shell's history
//!    files ([`HISTORY_FILES`], each a `literal` at its lexical path, held to
//!    the one file type its shell makes). Everything else, `$HOME` and the
//!    working directory included, is read-only, so the shell cannot plant
//!    `~/.zshrc`, a `LaunchAgent`, a git hook or a `$PATH` binary that would run
//!    outside the sandbox; a contained build works on a copy under `$TMPDIR`.
//! 4. `(deny file-read* file-write* …)` over the secret set ([`SECRET_SUBDIRS`],
//!    [`SECRET_LITERAL_FILES`]): credential stores cannot even be read.
//! 5. The same over the private-data set (`TmpOnly` only, [`PRIVATE_SUBDIRS`]):
//!    documents, downloads, media, and the Mail / Messages / keychain / cookies
//!    / browser-profile stores. Each is one `subpath` under `$HOME`, never `~`
//!    or all of `~/Library`, and none is sourced at shell startup.
//! 6. Ancestor-node write denies for the nested entries of 4 and 5 (see
//!    [`profile_for_home`]), closing the rename-the-parent relocation bypass.
//!
//! The actuator's enforcement-proof tests run each rule against the live kernel.
//!
//! ## Canonicalization
//!
//! Seatbelt matches the canonical path (`/tmp` is a symlink to `/private/tmp`,
//! and `$HOME` can sit behind a symlinked prefix), and a non-canonical rule
//! would silently not match. [`profile_for_home`] canonicalizes `$HOME` once and
//! joins the entries onto it. Entries need not exist: a deny matches by path and
//! fires as soon as the directory is created.
//!
//! ## Per-user profile
//!
//! Because the secret paths depend on `$HOME`, the Containment profile is now
//! per-user and built at profile-generation time; it is therefore an owned
//! `String`, not a `&'static str`. The homeless fallback
//! ([`BASE_PROFILE`]) is still a const, used verbatim when `$HOME` is unset
//! or empty (we refuse to emit denies for bogus paths).
//!
//! ## A string generator, not `sandbox_init`
//!
//! The profile is applied by exec'ing `/usr/bin/sandbox-exec -p <profile>`: the
//! spawn child between `fork` and `exec` may only make async-signal-safe calls
//! (see `aterm-pty`), and `sandbox_init(3)` is not. `sandbox-exec` does that
//! work in a fresh process image before exec'ing the target. This module only
//! produces the string, resolving `$HOME` in the parent.

use crate::capability::{FsCapability, NetworkCapability};
use crate::policy::Capabilities;
use std::path::{Path, PathBuf};

/// Absolute path to the macOS Seatbelt wrapper. A fixed absolute path (never a
/// PATH search) so the spawn seam can resolve it in the parent and the child stays
/// async-signal-safe, and so a missing wrapper is detectable (fail-closed) rather
/// than silently resolved to something else.
pub const SANDBOX_EXEC_PATH: &str = "/usr/bin/sandbox-exec";

/// Defines [`MACH_LOOKUP_ALLOWLIST`] and [`BASE_PROFILE`] from one list of
/// global names, so the names the profile allows and the names its proofs read
/// are the same list by construction.
macro_rules! base_profile_allowing {
    ($($name:literal,)+) => {
        /// The only global mach service names a contained process may look up;
        /// [`BASE_PROFILE`] refuses every other (`kr` 1100). Each is a read-only
        /// directory QUERY answered by `opendirectoryd`: who a uid or gid is,
        /// which groups a user is in. Neither changes anything for its caller —
        /// no launch, no registration, no preference, keychain or file write (a
        /// directory change goes through `com.apple.system.opendirectoryd.api`,
        /// which stays refused) — and neither resolves a host name through DNS
        /// for it (`dscacheutil -q host -a name example.com` resolves nothing
        /// under the profile, measured 2026-10-09), so neither reopens the
        /// network the profile denies.
        ///
        /// - `com.apple.system.opendirectoryd.libinfo` — `getpwuid`, `getpwnam`,
        ///   `getgrgid`, `getgrouplist` for an account outside `/etc/passwd`, as
        ///   every macOS user is. Without it `whoami` prints `501`, `id` and
        ///   `ls -l` print numbers, zsh's `%n` is empty, python's `pwd` cannot
        ///   find the user and `os.getgrouplist` returns one group.
        /// - `com.apple.system.opendirectoryd.membership` — group membership and
        ///   the uid/gid ↔ UUID map: `mbr_check_membership` (whether the user is
        ///   in a group, such as `admin`), and the names in a file's ACL. Without
        ///   it every such check errors (`dsmemberutil`: memberd is not
        ///   reachable) and `ls -le` prints raw UUIDs. git, python3, make, xcrun
        ///   and clang each look it up.
        ///
        /// A name joins only with a measurement that a normal shell or a
        /// common tool fails without it, and a reason here that the daemon
        /// behind it acts for nobody. The launch brokers are refused because
        /// they are absent: `actuator`'s `enforcement_proof_launch_brokers_denied`
        /// asks bootstrap for each, `enforcement_proof_mach_lookup_denied_by_default`
        /// for a sample of other daemons that act for their caller and for
        /// every name here, and `shell_compat_tool_set_runs_under_containment`
        /// runs the tool set the list was measured on.
        pub const MACH_LOOKUP_ALLOWLIST: &[&str] = &[$($name,)+];

        /// The base SBPL profile — rules 1 and 2, and the whole profile when
        /// `$HOME` is unset/empty.
        ///
        /// `(allow default)` keeps the shell working; `(deny network*)` removes
        /// all network access; the task-port pair refuses every task port but
        /// the caller's own; `(deny mach-lookup)` refuses every mach service but
        /// [`MACH_LOOKUP_ALLOWLIST`], so no daemon outside the sandbox works for
        /// the contained process. None of it names a path, so it is emitted
        /// even with no resolvable home to scope secrets under (we emit NO file
        /// denies rather than deny bogus paths). The full Containment profile
        /// ([`profile_for_home`]) is this PLUS the write confinement and the
        /// secret/private-set file denies.
        pub const BASE_PROFILE: &str = concat!(
            "(version 1)(allow default)(deny network*)",
            "(deny mach-priv-task-port)(allow mach-priv-task-port (target self))",
            "(deny mach-lookup)(allow mach-lookup",
            $(" (global-name \"", $name, "\")",)+
            ")",
        );
    };
}

base_profile_allowing!(
    "com.apple.system.opendirectoryd.libinfo",
    "com.apple.system.opendirectoryd.membership",
);

/// The canonical roots a Containment ([`FsCapability::TmpOnly`]) shell may still
/// WRITE under, as `subpath`s, whoever the user is. Canonical on purpose — `/tmp`
/// and `/var` are symlinks into `/private` and Seatbelt matches the resolved path.
/// `/dev` carries the tty, `/dev/null` and friends. The user's own `$TMPDIR` is
/// added beside them ([`admitted_user_temp`]).
pub const WRITABLE_ROOTS: &[&str] = &["/private/tmp", "/private/var/tmp", "/dev"];

/// Where macOS keeps each user's `$TMPDIR`: `/private/var/folders/<xx>/<id>/T`.
/// Only that `T` is temp space. Its siblings are not, and a contained shell may
/// not write them: `C` is the user cache directory, whose `clang/ModuleCache`
/// every later, unsandboxed `clang -fmodules` or `swift` build reads its
/// precompiled modules from, and `0` holds per-user service state (the Launch
/// Services database among it). So a contained module build fails unless it is
/// pointed at a cache under `$TMPDIR` (`-fmodules-cache-path=$TMPDIR/…`).
pub const USER_TEMP_PARENT: &str = "/private/var/folders";

/// The user's own `$TMPDIR` as a canonical write root, admitted only in the one
/// shape macOS gives it: `<USER_TEMP_PARENT>/<xx>/<id>/T`. A `$TMPDIR` anywhere
/// else adds nothing (under `/private/tmp` it is already allowed), so a
/// misdirected one — `$HOME`, a project, the user cache directory — never widens
/// the confinement. `None` when it does not resolve.
fn admitted_user_temp(temp: &Path) -> Option<PathBuf> {
    let canon = std::fs::canonicalize(temp).ok()?;
    let bucket = canon.parent()?.parent()?;
    (canon.file_name()? == "T" && bucket.parent()? == Path::new(USER_TEMP_PARENT)).then_some(canon)
}

/// The rule that keeps a `TmpOnly` shell from writing user preferences through
/// the preferences daemon, which writes `~/Library/Preferences` on the caller's
/// behalf and asks the sandbox about this operation rather than the file (see
/// [`push_write_confinement`]).
pub const PREFERENCE_WRITE_DENY: &str = "(deny user-preference-write)";

/// The shell history files under the canonical `$HOME` a Containment shell may
/// still write, each with the siblings its shell writes beside it, as `(name,
/// vnode type)`. History is data the shell reads back, never code it runs;
/// without it zsh prints a locking error at every prompt. The first name of each
/// row is the history file itself; zsh's save-by-copy temp file (`.new`) and its
/// lock (`.LOCK`, which zsh makes with `symlink(2)`) follow it.
///
/// Each name is allowed as a `literal` at its lexical path — never resolved,
/// never a `prefix` — and only for the one vnode type its shell makes there.
/// That is what keeps the allowance from reaching anything else:
///
/// - a history file that is itself a symlink would, resolved, allow its target
///   (a contained shell that points `~/.zsh_history` at `$HOME` would hand the
///   next session all of `$HOME`); a lexical `literal` allows only the name, and
///   Seatbelt judges a write through a symlink at the file it resolves to;
/// - a contained shell may not make the history file (or `.new`) a symlink,
///   directory or anything but a regular file (`(vnode-type REGULAR-FILE)`), so
///   it cannot leave a symlink to `~/.zshenv` for a later, unsandboxed shell to
///   append its history through;
/// - a history file that is not a plain, single-link regular file when the
///   profile is generated gets no allowance at all (see [`push_history_allows`]).
///
/// A history file elsewhere is read-only to the contained shell: a relocated
/// `HISTFILE` (prezto's `~/.zhistory`, `~/.local/state/zsh/history`, fish's
/// `~/.local/share/fish/fish_history`) and bash 4.4+'s save by temp file
/// (`~/.bash_history-<pid>.tmp`; `shopt -s histappend` appends in place
/// instead).
pub const HISTORY_FILES: &[&[(&str, &str)]] = &[
    &[
        (".zsh_history", "REGULAR-FILE"),
        (".zsh_history.new", "REGULAR-FILE"),
        (".zsh_history.LOCK", "SYMLINK"),
    ],
    &[(".bash_history", "REGULAR-FILE")],
];

/// The write-confinement rules of a `TmpOnly` profile: deny every write, then allow
/// [`WRITABLE_ROOTS`], the user's `$TMPDIR` where [`admitted_user_temp`] admits
/// it, and, with a home, the [`HISTORY_FILES`] under it.
///
/// A file deny alone does not confine every write. A user preference is
/// written by the preferences daemon (`cfprefsd`), outside the sandbox, into
/// `~/Library/Preferences`; the daemon asks the sandbox about the
/// `user-preference-write` operation, not about the file, and `(allow default)`
/// grants it. So [`PREFERENCE_WRITE_DENY`] comes first: no preference domain
/// is writable from inside.
fn push_write_confinement(
    profile: &mut String,
    user_temp: Option<&Path>,
    canon_home: Option<&Path>,
) {
    profile.push_str(PREFERENCE_WRITE_DENY);
    profile.push_str("(deny file-write*)(allow file-write*");
    for root in WRITABLE_ROOTS {
        push_subpath_clause(profile, Path::new(root));
    }
    if let Some(temp) = user_temp.and_then(admitted_user_temp) {
        push_subpath_clause(profile, &temp);
    }
    if let Some(home) = canon_home {
        push_history_allows(profile, home);
    }
    profile.push(')');
}

/// Append the [`HISTORY_FILES`] allows under `canon_home`: for each row whose
/// history file is absent or a plain regular file with one link, every name of
/// the row as ` (require-all (literal "<home>/<name>") (vnode-type <type>))`.
/// A history file that is a symlink, a directory or a hard link gets no
/// allowance — its row is skipped whole, so nothing reached through it is ever
/// named — and one whose type cannot be read is treated the same way.
fn push_history_allows(out: &mut String, canon_home: &Path) {
    for row in HISTORY_FILES {
        let Some((file, _)) = row.first() else {
            continue;
        };
        if !plain_or_absent(&canon_home.join(file)) {
            continue;
        }
        for (name, vnode_type) in *row {
            out.push_str(" (require-all");
            push_literal_clause(out, &canon_home.join(name));
            out.push_str(" (vnode-type ");
            out.push_str(vnode_type);
            out.push_str("))");
        }
    }
}

/// Whether `path` is absent, or a regular file (not followed through a symlink)
/// with exactly one link. An unreadable type answers `false`.
fn plain_or_absent(path: &Path) -> bool {
    match std::fs::symlink_metadata(path) {
        Err(e) => e.kind() == std::io::ErrorKind::NotFound,
        Ok(meta) => meta.file_type().is_file() && single_link(&meta),
    }
}

#[cfg(unix)]
fn single_link(meta: &std::fs::Metadata) -> bool {
    use std::os::unix::fs::MetadataExt;
    meta.nlink() == 1
}

#[cfg(not(unix))]
fn single_link(_meta: &std::fs::Metadata) -> bool {
    true
}

/// Conservative secret-directory set, denied (read+write) as `subpath`s under the
/// canonical `$HOME`. Each entry is a credential/secret store an untrusted agent
/// shell has no business reading or tampering with. Kept small and fixed on
/// purpose: every entry must be both (a) genuinely secret and (b) safe to deny
/// without breaking a normal `$SHELL` (none of these are sourced by the shell at
/// startup, unlike `~/.zshrc`).
///
/// Each path component is joined onto the canonical home, so e.g. `.config/gh`
/// becomes `<canonical-home>/.config/gh` (denying gh's token without denying the
/// rest of `~/.config`).
pub const SECRET_SUBDIRS: &[&str] = &[
    ".ssh",           // SSH private keys / known_hosts / config
    ".aws",           // AWS credentials + config
    ".gnupg",         // GnuPG keyrings
    ".config/gh",     // GitHub CLI OAuth token
    ".config/aterm",  // aterm's own config/secrets
    ".kube",          // Kubernetes kubeconfig (cluster creds/tokens)
    ".docker",        // Docker registry credentials (config.json)
    ".config/gcloud", // Google Cloud SDK credentials/tokens
    ".azure",         // Azure CLI tokens (accessTokens.json)
];

/// Conservative secret FILE set, denied (read+write) as `literal`s under the
/// canonical `$HOME`. Unlike [`SECRET_SUBDIRS`] these are single files, not
/// directories, so they are emitted as `(literal …)` not `(subpath …)`.
pub const SECRET_LITERAL_FILES: &[&str] = &[
    ".netrc",           // FTP/curl/git credentials
    ".git-credentials", // git credential-store plaintext tokens
    ".npmrc",           // npm registry auth token (_authToken)
    ".pypirc",          // PyPI upload credentials
];

/// Conservative private-user-data set, denied (read+write) as `subpath`s under the
/// canonical `$HOME`. This is the Containment FS policy beyond credentials: not
/// secrets per se, but the user's private personal data and local app databases an
/// untrusted Containment agent has no legitimate need to read or alter.
///
/// Every entry must be both (a) genuinely private user data and (b) safe to deny
/// without breaking a normal `$SHELL` (none of these is sourced at shell startup,
/// and each is a specific subpath — in particular the `Library/*` entries deny
/// individual stores, never the whole `~/Library`, so the tool caches/preferences/
/// application-support state the shell and its tools actually touch keep working).
///
/// Applied only to Containment ([`FsCapability::TmpOnly`](crate::FsCapability::TmpOnly)).
/// Browser/profile and `Library/*` entries need not exist — Seatbelt matches by
/// path, so a deny pre-arms the moment such a store is created. This is the set the
/// owner extends safely by appending.
pub const PRIVATE_SUBDIRS: &[&str] = &[
    // Personal files the user keeps under $HOME.
    "Documents",
    "Desktop",
    "Downloads",
    "Pictures",
    "Movies",
    "Music",
    // Local communication / credential databases under ~/Library.
    "Library/Mail",      // local Mail.app message store
    "Library/Messages",  // iMessage database (chat.db) — highly sensitive
    "Library/Keychains", // login keychain (encrypted, but deny access anyway)
    "Library/Cookies",   // system HTTP cookie store
    "Library/Safari",    // Safari history/bookmarks/top-sites
    // Browser profile stores (cookies, history, saved logins). Need not exist.
    "Library/Application Support/Google/Chrome",
    "Library/Application Support/Chromium",
    "Library/Application Support/Firefox",
    "Library/Application Support/BraveSoftware",
    "Library/Application Support/Microsoft Edge",
];

/// Generate the SBPL profile for the given resolved capability set, or `None`
/// if no OS sandbox is required.
///
/// `Some(profile)` exactly when the set denies network
/// ([`NetworkCapability::None`](crate::NetworkCapability::None), i.e.
/// `Containment`): the profile [`profile_for_home`] builds for the current
/// `$HOME` and the set's filesystem capability. Every other set (network
/// `Full`: `Safety`/`User`) returns `None`, meaning no `sandbox-exec`
/// wrap, so the default spawn is unchanged.
///
/// `$HOME` and `$TMPDIR` ([`std::env::temp_dir`]) are read here, in the
/// parent.
#[must_use]
pub fn profile_for(caps: &Capabilities) -> Option<String> {
    profile_for_scoped(*caps, home_dir().as_deref()).map(|(profile, _)| profile)
}

/// [`profile_for`] for the given home (the caller passes [`home_dir`]), and
/// whether that profile holds the home-scoped read denies (credential stores,
/// and for `TmpOnly` private data): `false` when the home is absent, empty or
/// does not canonicalize, so a posture record never claims them.
#[must_use]
pub(crate) fn profile_for_scoped(caps: Capabilities, home: Option<&str>) -> Option<(String, bool)> {
    match caps.network {
        // Containment: network denied → the Seatbelt profile scoped under
        // `home`. Write confinement and the private-data set are gated on the
        // filesystem capability (see `profile_for_home`), so a mode pairing
        // `network == None` with a broader FS tier would get only the network
        // and credential denies. Containment is `None ∧ TmpOnly`, so the full
        // set applies.
        NetworkCapability::None => Some(profile_scoped(home, Some(&std::env::temp_dir()), caps.fs)),
        // Safety / User (Full): no OS network sandbox. Spawn unchanged.
        NetworkCapability::Full => None,
    }
}

/// Read `$HOME` from the environment, treating unset OR empty as "no home".
///
/// Empty is treated as absent so we never join secret components onto `""` (which
/// would produce absolute `/.ssh`-style denies for a bogus root-relative path).
pub(crate) fn home_dir() -> Option<String> {
    match std::env::var("HOME") {
        Ok(h) if !h.is_empty() => Some(h),
        _ => None,
    }
}

/// Build the full Containment SBPL profile for a given (optional) `$HOME` and
/// `$TMPDIR`.
///
/// Reads no environment (the caller passes home and temp dir), but does
/// canonicalize the home and each existing secret/private entry, since
/// Seatbelt matches the resolved path (see [`push_resolved_clause`]). Absent
/// entries use the lexical join, pre-arming the deny.
///
/// - `fs == TmpOnly` ⇒ the write confinement (`(deny file-write*)` plus the
///   [`WRITABLE_ROOTS`], admitted `user_temp` and [`HISTORY_FILES`] allows)
///   follows the base profile. It needs no home to be meaningful, so it is
///   emitted even when there is none.
/// - `home == None` (or canonicalization of a present home fails) ⇒ nothing else:
///   we do NOT deny bogus paths. For a broader `fs` that is exactly
///   [`BASE_PROFILE`].
/// - `home == Some(h)` ⇒ canonicalizes `h`, joins each [`SECRET_SUBDIRS`] entry as
///   a `(subpath …)` and each [`SECRET_LITERAL_FILES`] entry as a `(literal …)`,
///   and — for `TmpOnly` — each [`PRIVATE_SUBDIRS`] entry as a `(subpath …)`, then
///   emits one `(deny file-read* file-write* …)` for the whole set. For NESTED
///   entries it ALSO emits a `(deny file-write* (literal …) …)` clause over each
///   EXISTING ancestor directory node, closing the relocate-the-writable-parent
///   bypass (see [`push_existing_ancestor_write_denies`]).
///
/// Only [`FsCapability::TmpOnly`](crate::FsCapability::TmpOnly) gets the write
/// confinement and private-data deny; broader tiers get network + credential
/// denies only.
///
/// Backslashes and double quotes in a pathological home path are escaped, so
/// the emitted SBPL literal is always well-formed.
#[must_use]
pub fn profile_for_home(home: Option<&str>, user_temp: Option<&Path>, fs: FsCapability) -> String {
    profile_scoped(home, user_temp, fs).0
}

/// [`profile_for_home`], and whether the home resolved — i.e. whether the
/// home-scoped read denies are in the profile.
fn profile_scoped(
    home: Option<&str>,
    user_temp: Option<&Path>,
    fs: FsCapability,
) -> (String, bool) {
    let tmp_only = fs == FsCapability::TmpOnly;
    // Canonicalize the home (it must exist); Seatbelt matches canonical paths.
    // No home, or one that does not canonicalize, means no home-scoped rule.
    let canon_home = home
        .filter(|h| !h.is_empty())
        .and_then(|h| std::fs::canonicalize(h).ok());

    // Assembled without `format!`: `format_args!` puts the unsafe
    // `fmt::Arguments::new` in this MIR, which the Trust lane fails closed on.
    let mut profile = String::new();
    profile.push_str(BASE_PROFILE);
    if tmp_only {
        push_write_confinement(&mut profile, user_temp, canon_home.as_deref());
    }
    let Some(canon_home) = canon_home else {
        return (profile, false);
    };

    let mut clauses = String::new();
    for sub in SECRET_SUBDIRS {
        push_resolved_clause(&mut clauses, &canon_home, sub, push_subpath_clause);
    }
    for file in SECRET_LITERAL_FILES {
        push_resolved_clause(&mut clauses, &canon_home, file, push_literal_clause);
    }
    // Containment FS policy: deny the private-user-data set too (read and write).
    if tmp_only {
        for sub in PRIVATE_SUBDIRS {
            push_resolved_clause(&mut clauses, &canon_home, sub, push_subpath_clause);
        }
    }

    // Ancestor-node rename guard.
    //
    // A leaf deny `(subpath "<home>/.config/gh")` does not cover the ancestor
    // `<home>/.config`, which `(allow default)` leaves writable, and Seatbelt
    // does not check descendants on rename, so `mv <home>/.config <home>/stolen`
    // would relocate the protected subtree past every deny. This affects nested
    // entries (`.config/*`, `Library/*`, browser profiles); a top-level entry's
    // own node is already write-denied.
    //
    // Each existing ancestor node gets a write deny as a `literal` (the node
    // only), so rename/unlink/replace is blocked while writes under it still
    // work. Only existing nodes: a deny on a missing intermediate would block
    // `mkdir -p`, and there is nothing to relocate. `file-write*` only; reads
    // are unaffected.
    let mut ancestor_clauses = String::new();
    let mut denied_ancestors: Vec<PathBuf> = Vec::new();
    let mut nested: Vec<&str> = SECRET_SUBDIRS
        .iter()
        .chain(SECRET_LITERAL_FILES.iter())
        .copied()
        .collect();
    if tmp_only {
        nested.extend(PRIVATE_SUBDIRS.iter().copied());
    }
    for entry in nested {
        push_existing_ancestor_write_denies(
            &mut ancestor_clauses,
            &canon_home,
            entry,
            &mut denied_ancestors,
        );
    }

    // The secret (+ private) set deny, then the ancestor-node write deny (when
    // any nested ancestor exists). Both follow the write confinement, so they
    // win over its allows.
    profile.push_str("(deny file-read* file-write*");
    profile.push_str(&clauses);
    profile.push(')');
    if !ancestor_clauses.is_empty() {
        profile.push_str("(deny file-write*");
        profile.push_str(&ancestor_clauses);
        profile.push(')');
    }
    (profile, true)
}

/// Emit the deny clause(s) for the secret/private entry `<canon_home>/<entry>`,
/// resolving symlinks as Seatbelt does.
///
/// Seatbelt checks the kernel-resolved path, so a store that is itself a
/// symlink (common with dotfile managers: GNU Stow, chezmoi, yadm) would escape
/// a deny written only against the lexical path.
///
/// - Entry exists ⇒ the canonical clause, plus the lexical clause when it
///   differs.
/// - Entry absent ⇒ the lexical clause (pre-arming the deny), plus, when an
///   intermediate component is a symlink (`~/.config` → `~/dotfiles/config`
///   with `gh` not yet created), the clause for the deepest existing ancestor
///   resolved with the missing tail re-appended, so the deny does not fail
///   open once the leaf appears.
fn push_resolved_clause(
    out: &mut String,
    canon_home: &Path,
    entry: &str,
    push: fn(&mut String, &Path),
) {
    let lexical = canon_home.join(entry);
    if let Ok(canonical) = std::fs::canonicalize(&lexical) {
        push(out, &canonical);
        if canonical != lexical {
            push(out, &lexical);
        }
    } else {
        push(out, &lexical);
        if let Some(resolved) = resolve_existing_prefix(&lexical)
            && resolved != lexical
        {
            push(out, &resolved);
        }
    }
}

/// Resolve symlinks in the deepest existing ancestor of `path` and re-append
/// the missing trailing components, for a path whose leaf does not exist yet.
/// `None` if no ancestor canonicalizes (impossible under a canonical home).
fn resolve_existing_prefix(path: &Path) -> Option<PathBuf> {
    // `ancestors()` yields `path` first; skip it (we already know it fails to
    // canonicalize) and find the deepest ancestor that does.
    for ancestor in path.ancestors().skip(1) {
        if let Ok(canon_ancestor) = std::fs::canonicalize(ancestor) {
            let tail = path.strip_prefix(ancestor).ok()?;
            return Some(canon_ancestor.join(tail));
        }
    }
    None
}

/// Append ` (literal "<node>")` write-deny operands for the existing ancestor
/// nodes of `entry` whose rename would relocate the protected leaf, deduped
/// against `denied` (see [`profile_for_home`]).
///
/// Ancestors of both the lexical and the resolved path are guarded: with
/// `~/.config -> ~/dotfiles/config`, denying only the lexical symlink node
/// would still allow `mv ~/dotfiles/config ~/stolen`. The resolved path is
/// computed as the leaf deny computes it ([`resolve_existing_prefix`] when the
/// leaf is absent).
///
/// Bounded to strict descendants of the canonical home (never home itself or
/// anything outside it). A store symlinked entirely outside `$HOME` keeps its
/// leaf denies but has no relocation guard, matching the `$HOME`-scoped design.
fn push_existing_ancestor_write_denies(
    out: &mut String,
    canon_home: &Path,
    entry: &str,
    denied: &mut Vec<PathBuf>,
) {
    let lexical = canon_home.join(entry);
    // Resolved as the leaf deny resolves it (`push_resolved_clause`).
    let resolved = match std::fs::canonicalize(&lexical) {
        Ok(c) => Some(c),
        Err(_) => resolve_existing_prefix(&lexical),
    };
    push_ancestor_literals(out, canon_home, &lexical, denied);
    if let Some(resolved) = resolved
        && resolved != lexical
    {
        push_ancestor_literals(out, canon_home, &resolved, denied);
    }
}

/// Write-deny each existing directory node strictly between `canon_home` and
/// `path`'s leaf, shallow-first, deduped against `denied`. Never home itself or
/// a node outside it.
fn push_ancestor_literals(
    out: &mut String,
    canon_home: &Path,
    path: &Path,
    denied: &mut Vec<PathBuf>,
) {
    // `ancestors()` is leaf-first. Skip the leaf (it has its own deny) and keep
    // the contiguous run of strict ancestors under home.
    let mut nodes: Vec<&Path> = path
        .ancestors()
        .skip(1)
        .take_while(|a| a.starts_with(canon_home) && *a != canon_home)
        .collect();
    nodes.reverse();
    for node in nodes {
        let node = node.to_path_buf();
        if denied.iter().any(|d| d == &node) {
            continue;
        }
        // Existing nodes only (dir or symlink; `symlink_metadata` does not
        // follow): a deny on a missing path would break `mkdir -p`.
        if std::fs::symlink_metadata(&node).is_ok() {
            push_literal_clause(out, &node);
            denied.push(node);
        }
    }
}

/// Append ` (subpath "<escaped path>")` to `out`.
fn push_subpath_clause(out: &mut String, path: &Path) {
    out.push_str(" (subpath \"");
    push_sbpl_escaped(out, path);
    out.push_str("\")");
}

/// Append ` (literal "<escaped path>")` to `out`.
fn push_literal_clause(out: &mut String, path: &Path) {
    out.push_str(" (literal \"");
    push_sbpl_escaped(out, path);
    out.push_str("\")");
}

/// Append `path` to `out` as the inside of an SBPL string literal, backslash-
/// escaping `\` and `"` so the emitted `"…"` is always well-formed regardless of
/// the home path. Non-UTF-8 path bytes are emitted lossily (a non-UTF-8 home is not
/// a supported credential location; the worst case is a deny that does not match).
fn push_sbpl_escaped(out: &mut String, path: &Path) {
    for ch in path.to_string_lossy().chars() {
        match ch {
            '\\' | '"' => {
                out.push('\\');
                out.push(ch);
            }
            other => out.push(other),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::capability::NetworkCapability;
    use crate::mode::ContainmentMode;
    use crate::policy::ContainmentPolicy;

    /// Expected-marker helper: render `path` exactly as the emitter's SBPL
    /// string escaping does ([`push_sbpl_escaped`]). Identity for POSIX paths;
    /// on Windows the canonical `\\?\C:\…` shape gets its backslashes escaped,
    /// so markers match the emitted profile on every host.
    fn sbpl_str(path: &Path) -> String {
        let mut s = String::new();
        push_sbpl_escaped(&mut s, path);
        s
    }

    /// The TmpOnly write confinement with no home-scoped allow in it.
    fn homeless_write_confinement() -> String {
        let mut s = String::new();
        push_write_confinement(&mut s, None, None);
        s
    }

    #[test]
    fn home_unset_yields_no_home_scoped_rule() {
        // No resolvable home → no file deny or allow for a bogus path. A broader FS
        // tier gets the base profile; TmpOnly keeps its write confinement, which names
        // no home path and so is still meaningful (and strictly tighter).
        let tmp_only = [BASE_PROFILE, &homeless_write_confinement()].concat();
        for home in [None, Some("")] {
            assert_eq!(
                profile_for_home(home, None, FsCapability::HomeReadWrite),
                BASE_PROFILE
            );
            assert_eq!(
                profile_for_home(home, None, FsCapability::TmpOnly),
                tmp_only
            );
            // …and says so, so the posture record does not claim the read denies.
            assert!(!profile_scoped(home, None, FsCapability::TmpOnly).1);
        }
        let home = aterm_tempfile::tempdir().unwrap();
        assert!(profile_scoped(home.path().to_str(), None, FsCapability::TmpOnly).1);
    }

    /// `TmpOnly` confines writes: every write denied, the temp roots, `/dev`
    /// and the shell's history files allowed back, and nothing of `$HOME`
    /// beyond those files (owner, 2026-09-26).
    #[test]
    fn tmp_only_confines_writes_to_the_writable_roots() {
        let home = aterm_tempfile::tempdir().unwrap();
        let canon_home = std::fs::canonicalize(home.path()).unwrap();
        let profile = profile_for_home(canon_home.to_str(), None, FsCapability::TmpOnly);

        // Deny preference writes and every file write, then allow the roots — in
        // that order, right after the base profile, so every later deny
        // (secrets, private data) still wins.
        let confinement = [
            BASE_PROFILE,
            PREFERENCE_WRITE_DENY,
            "(deny file-write*)(allow file-write*",
        ]
        .concat();
        assert!(
            profile.starts_with(&confinement),
            "TmpOnly must open with the write confinement; got {profile}"
        );
        for root in WRITABLE_ROOTS {
            let expect = format!("(subpath \"{root}\")");
            assert!(
                profile.contains(&expect),
                "missing writable root {expect}; got {profile}"
            );
            // Written in its canonical form where the profile is applied:
            // Seatbelt matches the resolved path, so a symlinked spelling
            // (`/tmp`, `/var/…`) would allow nothing.
            #[cfg(target_os = "macos")]
            assert_eq!(
                std::fs::canonicalize(root).ok().as_deref(),
                Some(Path::new(root)),
                "{root} must be written in its canonical form"
            );
        }
        for row in HISTORY_FILES {
            for (name, vnode_type) in *row {
                let expect = history_allow(&canon_home, name, vnode_type);
                assert!(
                    profile.contains(&expect),
                    "missing history allow {expect}; got {profile}"
                );
            }
        }
        assert!(
            !profile.contains("(prefix "),
            "no allow is an open prefix; got {profile}"
        );
        // The confinement precedes the secret deny (later rules win in SBPL).
        let allow_at = profile.find("(allow file-write*").unwrap();
        let secret_at = profile.find("(deny file-read* file-write*").unwrap();
        assert!(
            allow_at < secret_at,
            "secret deny must come after the write allows"
        );
        // Neither the home nor any part of it but the history files is writable.
        let allows = &profile[allow_at..secret_at];
        for forbidden in [&canon_home, &canon_home.join("Library")] {
            for shape in ["subpath", "prefix", "literal"] {
                let allow = format!("({shape} \"{}\")", sbpl_str(forbidden));
                assert!(
                    !allows.contains(&allow),
                    "{allow} must not be writable; got {profile}"
                );
            }
        }
    }

    /// The history allow for `<home>/<name>` exactly as the generator writes it.
    fn history_allow(canon_home: &Path, name: &str, vnode_type: &str) -> String {
        format!(
            "(require-all (literal \"{}\") (vnode-type {vnode_type}))",
            sbpl_str(&canon_home.join(name))
        )
    }

    /// The `(allow file-write* …)` clause of a `TmpOnly` profile.
    fn write_allows(profile: &str) -> &str {
        let at = profile
            .find("(allow file-write*")
            .expect("the write allows");
        let end = at + profile[at..].find(")(").expect("the allow clause ends");
        &profile[at..end]
    }

    /// A history file that is not a plain single-link file gets no allowance
    /// (a `~/.zsh_history` symlink to `$HOME` would otherwise allow all of
    /// `$HOME`): its row is dropped whole; the other shell's row is untouched.
    #[cfg(unix)]
    #[test]
    fn a_history_file_that_is_not_a_plain_file_gets_no_allowance() {
        let zsh_row = HISTORY_FILES[0];
        let bash_row = HISTORY_FILES[1];
        assert_eq!(zsh_row[0].0, ".zsh_history", "precondition");
        let plant: [(&str, fn(&Path, &Path)); 4] = [
            ("symlink to $HOME", |home, hist| {
                std::os::unix::fs::symlink(home, hist).unwrap();
            }),
            ("symlink to ~/.zshrc", |home, hist| {
                std::os::unix::fs::symlink(home.join(".zshrc"), hist).unwrap();
            }),
            ("directory", |_, hist| std::fs::create_dir(hist).unwrap()),
            ("hard link to ~/.zshrc", |home, hist| {
                std::fs::hard_link(home.join(".zshrc"), hist).unwrap();
            }),
        ];
        for (shape, plant) in plant {
            let home = aterm_tempfile::tempdir().unwrap();
            let canon_home = std::fs::canonicalize(home.path()).unwrap();
            std::fs::write(canon_home.join(".zshrc"), "echo original\n").unwrap();
            plant(&canon_home, &canon_home.join(".zsh_history"));

            let profile = profile_for_home(canon_home.to_str(), None, FsCapability::TmpOnly);
            let allows = write_allows(&profile);
            for (name, _) in zsh_row {
                let named = format!("\"{}\"", sbpl_str(&canon_home.join(name)));
                assert!(
                    !allows.contains(&named),
                    "{shape}: {name} must get no allowance; got {allows}"
                );
            }
            for shape_of_home in [&canon_home, &canon_home.join(".zshrc")] {
                let named = format!("\"{}\")", sbpl_str(shape_of_home));
                assert!(
                    !allows.contains(&named),
                    "{shape}: {} must not be writable; got {allows}",
                    shape_of_home.display()
                );
            }
            // Negative control: the untouched row is still allowed, so the
            // probe reads a real allow clause.
            for (name, vnode_type) in bash_row {
                let expect = history_allow(&canon_home, name, vnode_type);
                assert!(
                    allows.contains(&expect),
                    "{shape}: {expect} must stay allowed; got {allows}"
                );
            }
        }
        // A plain, single-link history file keeps its allowance.
        let home = aterm_tempfile::tempdir().unwrap();
        let canon_home = std::fs::canonicalize(home.path()).unwrap();
        std::fs::write(canon_home.join(".zsh_history"), ": 0:0;ls\n").unwrap();
        let profile = profile_for_home(canon_home.to_str(), None, FsCapability::TmpOnly);
        let expect = history_allow(&canon_home, ".zsh_history", "REGULAR-FILE");
        assert!(
            write_allows(&profile).contains(&expect),
            "a plain history file keeps {expect}; got {profile}"
        );
        // A type that cannot be read (the path runs through a file) is not plain.
        assert!(!plain_or_absent(&canon_home.join(".zsh_history").join("x")));
    }

    /// Only the user's own `$TMPDIR` (`/private/var/folders/<xx>/<id>/T`) is
    /// added to the write roots — never its siblings (`C`, which holds clang's
    /// module cache that later unsandboxed builds read; `0`), never the folders
    /// above it, never a `$TMPDIR` pointed somewhere else.
    #[test]
    fn only_the_users_own_tmpdir_joins_the_write_roots() {
        let home = aterm_tempfile::tempdir().unwrap();
        let canon_home = std::fs::canonicalize(home.path()).unwrap();
        // Named `T` at the right depth, but not under the per-user folders.
        let lookalike = canon_home.join("a/b/T");
        std::fs::create_dir_all(&lookalike).unwrap();
        for elsewhere in [
            canon_home.as_path(),
            Path::new(USER_TEMP_PARENT),
            Path::new("/"),
            lookalike.as_path(),
        ] {
            assert_eq!(
                admitted_user_temp(elsewhere),
                None,
                "{} is not a user's $TMPDIR",
                elsewhere.display()
            );
        }
        let profile = profile_for_home(
            canon_home.to_str(),
            Some(&canon_home),
            FsCapability::TmpOnly,
        );
        let home_allow = format!("(subpath \"{}\")", sbpl_str(&canon_home));
        assert!(
            !write_allows(&profile).contains(&home_allow),
            "a $TMPDIR at $HOME must not make $HOME writable; got {profile}"
        );

        // The real per-user temp directory, where this host has one.
        #[cfg(target_os = "macos")]
        {
            let Some(temp) = std::fs::canonicalize(std::env::temp_dir())
                .ok()
                .filter(|t| admitted_user_temp(t).is_some())
            else {
                aterm_spec::host::could_not_run(
                    "$TMPDIR is not a per-user /private/var/folders/…/T here",
                );
            };
            let per_user = temp.parent().unwrap();
            for sibling in ["C", "0"] {
                let sibling = per_user.join(sibling);
                if sibling.exists() {
                    assert_eq!(admitted_user_temp(&sibling), None, "{}", sibling.display());
                }
            }
            assert_eq!(admitted_user_temp(per_user), None, "{}", per_user.display());
            let profile = profile_for_home(canon_home.to_str(), Some(&temp), FsCapability::TmpOnly);
            let allows = write_allows(&profile);
            let expect = format!("(subpath \"{}\")", sbpl_str(&temp));
            assert!(allows.contains(&expect), "missing {expect}; got {allows}");
            let broad = format!("(subpath \"{USER_TEMP_PARENT}\")");
            assert!(
                !allows.contains(&broad),
                "{broad} is too broad; got {allows}"
            );
        }
    }

    #[test]
    fn broader_fs_tiers_get_no_write_confinement() {
        let home = aterm_tempfile::tempdir().unwrap();
        let canon_home = std::fs::canonicalize(home.path()).unwrap();
        // `Library` holds private stores only `TmpOnly` guards; its node is not write-denied.
        std::fs::create_dir_all(canon_home.join("Library")).unwrap();
        let library = format!("(literal \"{}\")", sbpl_str(&canon_home.join("Library")));
        for fs in [FsCapability::ProjectReadWrite, FsCapability::HomeReadWrite] {
            let profile = profile_for_home(canon_home.to_str(), None, fs);
            assert!(
                !profile.contains("(allow file-write*") && !profile.contains("(deny file-write*)"),
                "{fs:?} must not be write-confined; got {profile}"
            );
            assert!(
                !profile.contains(&library),
                "{fs:?} guards no private store; got {profile}"
            );
            // Negative control: the probe reads a real generated profile.
            assert!(
                profile.contains("(deny file-read* file-write*"),
                "the credential deny is emitted; got {profile}"
            );
        }
    }

    #[test]
    fn full_profile_starts_with_the_base_profile_and_denies_each_secret() {
        // Use a real, canonicalizable home so the path-joins resolve. The crate's
        // own temp dir is canonical on the test box. Credential-only (a broader FS tier
        // than TmpOnly): this test pins the secret set independently of the private set.
        let tmp = std::env::temp_dir();
        // Canonicalize so our expected literals match what the generator emits.
        let canon = std::fs::canonicalize(&tmp).expect("canonicalize temp dir");
        let profile = profile_for_home(canon.to_str(), None, FsCapability::HomeReadWrite);

        // (1) It is the base profile PLUS a file deny — never less. (A broader FS
        // tier: no write confinement, so the file deny follows directly.)
        assert!(
            profile.starts_with(BASE_PROFILE),
            "full profile must begin with the exact base profile; got {profile}"
        );
        assert!(
            profile.contains("(deny file-read* file-write*"),
            "full profile must deny read AND write of the secret set; got {profile}"
        );

        // (2) Every secret subdir appears as a canonical subpath.
        for sub in SECRET_SUBDIRS {
            let expect = format!("(subpath \"{}\")", sbpl_str(&canon.join(sub)));
            assert!(
                profile.contains(&expect),
                "profile must deny secret subdir {sub} as canonical subpath {expect}; got {profile}"
            );
        }
        // (3) Every secret literal file appears as a canonical literal.
        for file in SECRET_LITERAL_FILES {
            let expect = format!("(literal \"{}\")", sbpl_str(&canon.join(file)));
            assert!(
                profile.contains(&expect),
                "profile must deny secret file {file} as canonical literal {expect}; got {profile}"
            );
        }
        // (4) Below TmpOnly, NO private-data path leaks in.
        for sub in PRIVATE_SUBDIRS {
            let leaked = format!("(subpath \"{}\")", sbpl_str(&canon.join(sub)));
            assert!(
                !profile.contains(&leaked),
                "credential-only profile must NOT contain private-data path {sub}; got {profile}"
            );
        }
    }

    #[test]
    fn private_data_set_present_only_when_included() {
        // The Containment FS-policy gate: TmpOnly adds the PRIVATE_SUBDIRS clauses
        // (and the write confinement) on top of the credential deny.
        let tmp = std::env::temp_dir();
        let canon = std::fs::canonicalize(&tmp).expect("canonicalize temp dir");
        let off = profile_for_home(canon.to_str(), None, FsCapability::HomeReadWrite);
        let on = profile_for_home(canon.to_str(), None, FsCapability::TmpOnly);

        // Every private dir is in ON, none in OFF.
        for sub in PRIVATE_SUBDIRS {
            let expect = format!("(subpath \"{}\")", sbpl_str(&canon.join(sub)));
            assert!(
                on.contains(&expect),
                "TmpOnly profile must deny private dir {sub} as {expect}; got {on}"
            );
            assert!(
                !off.contains(&expect),
                "credential-only profile must NOT deny private dir {sub}; got {off}"
            );
        }
        // The secret set is present in both (private is purely additive).
        for sub in SECRET_SUBDIRS {
            let expect = format!("(subpath \"{}\")", sbpl_str(&canon.join(sub)));
            assert!(on.contains(&expect) && off.contains(&expect));
        }
        // And ON is strictly longer (private clauses were appended, not substituted).
        assert!(on.len() > off.len(), "TmpOnly must add clauses");
    }

    #[test]
    fn full_profile_does_not_deny_the_whole_home() {
        // The home itself (and so ~/.zshrc) is never denied, only the
        // secret/private entries: no bare-home clause, checked against the
        // full TmpOnly profile.
        let tmp = std::env::temp_dir();
        let canon = std::fs::canonicalize(&tmp).expect("canonicalize temp dir");
        let profile = profile_for_home(canon.to_str(), None, FsCapability::TmpOnly);
        let bare_subpath = format!("(subpath \"{}\")", sbpl_str(&canon));
        let bare_literal = format!("(literal \"{}\")", sbpl_str(&canon));
        assert!(
            !profile.contains(&bare_subpath) && !profile.contains(&bare_literal),
            "profile must NOT deny the entire home (would break ~/.zshrc etc.); got {profile}"
        );
        // Likewise the bare ~/Library must never be denied wholesale — only specific
        // Library/* stores — or it would break tool caches/preferences/app-support.
        let bare_library = format!("(subpath \"{}\")", sbpl_str(&canon.join("Library")));
        assert!(
            !profile.contains(&bare_library),
            "profile must NOT deny all of ~/Library (only specific stores); got {profile}"
        );
    }

    #[test]
    fn secret_paths_are_canonicalized_not_raw() {
        // A home reached via /tmp canonicalizes to /private/tmp; the generator
        // must emit the canonical prefix. COULD NOT RUN where /tmp is not a symlink.
        let raw = std::path::Path::new("/tmp");
        let canon = std::fs::canonicalize(raw).unwrap_or_else(|_| raw.to_path_buf());
        if canon == raw {
            aterm_spec::host::could_not_run("/tmp is no symlink here to canonicalize");
        }
        let profile = profile_for_home(raw.to_str(), None, FsCapability::TmpOnly);
        let canon_marker = format!("(subpath \"{}/.ssh\")", canon.display());
        let raw_marker = "(subpath \"/tmp/.ssh\")";
        assert!(
            profile.contains(&canon_marker),
            "must emit CANONICAL secret path {canon_marker}; got {profile}"
        );
        assert!(
            !profile.contains(raw_marker),
            "must NOT emit the non-canonical raw path (Seatbelt would not match it); got {profile}"
        );
    }

    #[cfg(unix)]
    #[test]
    fn symlinked_secret_store_is_denied_at_its_canonical_target() {
        use std::os::unix::fs::symlink;
        // A credential store that is itself a symlink (dotfile managers) is
        // denied at its resolved target, or the deny would fail open.
        let home = aterm_tempfile::tempdir().unwrap();
        let store = aterm_tempfile::tempdir().unwrap();
        let canon_home = std::fs::canonicalize(home.path()).unwrap();
        let canon_store = std::fs::canonicalize(store.path()).unwrap();
        // Symlink <home>/.aws -> <store> (the real credential location).
        symlink(store.path(), canon_home.join(".aws")).unwrap();

        let profile = profile_for_home(canon_home.to_str(), None, FsCapability::HomeReadWrite);

        // The deny must cover the real target the kernel resolves ~/.aws to.
        let target_marker = format!("(subpath \"{}\")", canon_store.display());
        assert!(
            profile.contains(&target_marker),
            "symlinked ~/.aws must be denied at its canonical target {target_marker}; got {profile}"
        );
        // Belt-and-suspenders: the lexical path is still emitted too.
        let lexical_marker = format!("(subpath \"{}/.aws\")", canon_home.display());
        assert!(
            profile.contains(&lexical_marker),
            "lexical ~/.aws deny retained; got {profile}"
        );
    }

    #[cfg(unix)]
    #[test]
    fn symlinked_ancestor_with_absent_leaf_is_denied_at_resolved_target() {
        use std::os::unix::fs::symlink;
        // `~/.config` is a symlink and `~/.config/gh` does not exist yet: the
        // deny must still cover the target reached through the symlink.
        let home = aterm_tempfile::tempdir().unwrap();
        let store = aterm_tempfile::tempdir().unwrap();
        let canon_home = std::fs::canonicalize(home.path()).unwrap();
        let canon_store = std::fs::canonicalize(store.path()).unwrap();
        // ~/.config -> <store> (the tracked dir exists); leaves under it do not.
        symlink(store.path(), canon_home.join(".config")).unwrap();

        let profile = profile_for_home(canon_home.to_str(), None, FsCapability::HomeReadWrite);

        // SECRET_SUBDIRS has entries under `.config` (`.config/gh`, …); their
        // resolved target under the symlinked ancestor must appear as a deny.
        let resolved_prefix = format!("(subpath \"{}/", canon_store.display());
        assert!(
            profile.contains(&resolved_prefix),
            "a secret path under symlinked ~/.config must be denied at its resolved target ({resolved_prefix}); got {profile}"
        );
        // A store outside `$HOME` keeps its leaf denies but no node of it, or above it, is
        // write-denied: the relocation guard stays inside the home.
        let outside = format!("(literal \"{}\")", sbpl_str(&canon_store));
        assert!(
            !profile.contains(&outside),
            "{outside} is outside the home; got {profile}"
        );
    }

    #[test]
    fn nonexistent_home_falls_back_to_the_base_profile() {
        // A home that cannot be canonicalized (does not exist) → the base profile, not
        // a deny on a possibly-wrong path.
        let bogus = "/nonexistent/aterm-no-such-home-xyz";
        assert_eq!(
            profile_for_home(Some(bogus), None, FsCapability::HomeReadWrite),
            BASE_PROFILE
        );
        assert_eq!(
            profile_for_home(Some(bogus), None, FsCapability::TmpOnly),
            [BASE_PROFILE, &homeless_write_confinement()].concat()
        );
    }

    #[test]
    fn nested_secret_ancestor_nodes_are_write_denied_to_block_rename_relocation() {
        // Every existing nested ancestor node gets a
        // `(deny file-write* (literal …))`, so `mv ~/.config ~/stolen` cannot
        // move the subtree past the leaf deny.
        let home = aterm_tempfile::tempdir().unwrap();
        let canon_home = std::fs::canonicalize(home.path()).unwrap();
        // Create the ancestor chains for one secret and one private (deep) entry.
        std::fs::create_dir_all(canon_home.join(".config/gh")).unwrap();
        std::fs::create_dir_all(canon_home.join("Library/Application Support/Google/Chrome"))
            .unwrap();

        let profile = profile_for_home(canon_home.to_str(), None, FsCapability::TmpOnly);

        // A dedicated write-only deny clause must be present (distinct from the
        // read+write leaf clause and from the write confinement's bare deny).
        assert!(
            profile.contains("(deny file-write* (literal"),
            "must emit an ancestor write-deny clause; got {profile}"
        );
        // The immediate parent `.config` is a literal NODE write-deny.
        let cfg = format!("(literal \"{}\")", sbpl_str(&canon_home.join(".config")));
        assert!(
            profile.contains(&cfg),
            "must write-deny the .config ancestor node {cfg}; got {profile}"
        );
        // The FULL chain for the browser profile — every intermediate node, not just
        // the immediate parent (a higher rename relocates the subtree just as well).
        for anc in [
            "Library",
            "Library/Application Support",
            "Library/Application Support/Google",
        ] {
            let lit = format!("(literal \"{}\")", sbpl_str(&canon_home.join(anc)));
            assert!(
                profile.contains(&lit),
                "must write-deny ancestor node {anc} as {lit}; got {profile}"
            );
        }
        // The home NODE itself must never be write-denied (would break the shell).
        let home_lit = format!("(literal \"{}\")", sbpl_str(&canon_home));
        assert!(
            !profile.contains(&home_lit),
            "must never write-deny the home node itself; got {profile}"
        );
        // Top-level secret leaves (`.ssh`) contribute no ancestor node beyond home,
        // so the Chrome leaf itself is not re-emitted as a bare ancestor literal.
        let chrome_lit = format!(
            "(literal \"{}\")",
            sbpl_str(&canon_home.join("Library/Application Support/Google/Chrome"))
        );
        assert!(
            !profile.contains(&chrome_lit),
            "the leaf itself is covered by its subpath deny, not re-emitted as an ancestor literal; got {profile}"
        );
    }

    #[test]
    fn nonexistent_nested_ancestor_is_not_write_denied() {
        // Only existing ancestor nodes are denied: a fresh home with no nested
        // dirs emits no ancestor clause.
        let home = aterm_tempfile::tempdir().unwrap();
        let canon_home = std::fs::canonicalize(home.path()).unwrap();
        let profile = profile_for_home(canon_home.to_str(), None, FsCapability::TmpOnly);
        assert!(
            !profile.contains("(deny file-write* (literal"),
            "no existing nested ancestor ⇒ no ancestor write-deny clause (would break mkdir -p); got {profile}"
        );
    }

    #[cfg(unix)]
    #[test]
    fn symlinked_ancestor_store_is_write_denied_at_its_resolved_nodes() {
        use std::os::unix::fs::symlink;
        // `~/.config -> ~/dotfiles/config`: the resolved store node and its
        // parent must be write-denied too, not just the lexical symlink node.
        let home = aterm_tempfile::tempdir().unwrap();
        let canon_home = std::fs::canonicalize(home.path()).unwrap();
        // Real store under home, reached via the ~/.config symlink: the secret inside it absent
        // (it may be made later in the session), then present.
        let store = canon_home.join("dotfiles/config");
        std::fs::create_dir_all(&store).unwrap();
        symlink(&store, canon_home.join(".config")).unwrap();

        // (1) The lexical symlink node is write-denied, (2) and the resolved real store + its
        // parent — closing the rename-the-real-ancestor bypass codex flagged.
        let lex = format!("(literal \"{}\")", canon_home.join(".config").display());
        let real_store = format!("(literal \"{}\")", store.display());
        let real_parent = format!("(literal \"{}\")", canon_home.join("dotfiles").display());
        for present in [false, true] {
            if present {
                std::fs::create_dir_all(store.join("gh")).unwrap();
            }
            let profile = profile_for_home(canon_home.to_str(), None, FsCapability::HomeReadWrite);
            for node in [&lex, &real_store, &real_parent] {
                assert!(
                    profile.contains(node.as_str()),
                    "{node} must be write-denied (secret present: {present}); got {profile}"
                );
            }
        }
    }

    #[test]
    fn ancestor_write_deny_dedups_shared_parents() {
        // Shared ancestors (`Library`, `Library/Application Support`) are
        // emitted exactly once.
        let home = aterm_tempfile::tempdir().unwrap();
        let canon_home = std::fs::canonicalize(home.path()).unwrap();
        std::fs::create_dir_all(canon_home.join("Library/Application Support/Google/Chrome"))
            .unwrap();
        std::fs::create_dir_all(canon_home.join("Library/Messages")).unwrap();

        let profile = profile_for_home(canon_home.to_str(), None, FsCapability::TmpOnly);
        let lib_lit = format!("(literal \"{}\")", sbpl_str(&canon_home.join("Library")));
        assert_eq!(
            profile.matches(&lib_lit).count(),
            1,
            "the shared Library ancestor node must be write-denied exactly once; got {profile}"
        );
        // A leaf whose path needs no resolving is denied once, present or not.
        std::fs::create_dir_all(canon_home.join(".ssh")).unwrap();
        let profile = profile_for_home(canon_home.to_str(), None, FsCapability::TmpOnly);
        for leaf in [".ssh", ".aws"] {
            let clause = format!("(subpath \"{}\")", sbpl_str(&canon_home.join(leaf)));
            assert_eq!(
                profile.matches(&clause).count(),
                1,
                "{clause}; got {profile}"
            );
        }
    }

    #[test]
    fn sbpl_escaping_quotes_and_backslashes() {
        // A pathological home with a quote/backslash must stay a well-formed SBPL
        // string literal (escaped), so the profile never breaks out of the "…".
        let mut out = String::new();
        push_sbpl_escaped(&mut out, std::path::Path::new("/a\"b\\c"));
        assert_eq!(out, "/a\\\"b\\\\c");
    }

    /// Unix-only: the profile is scoped under `$HOME` ([`home_dir`]), which a native
    /// Windows process does not have, and only macOS ever applies one.
    #[cfg(unix)]
    #[test]
    fn private_data_is_gated_on_the_fs_capability_not_network() {
        // The private-data deny is keyed on `fs == TmpOnly`, not on
        // `network == None`: a hypothetical set with network denied and a
        // broader FS tier gets the credential deny only. Needs a resolvable
        // $HOME: COULD NOT RUN otherwise, never a pass.
        let home = home_dir().unwrap_or_else(|| {
            aterm_spec::host::could_not_run("no HOME: no private-data gate check")
        });
        let canon = std::fs::canonicalize(&home).unwrap_or_else(|e| {
            aterm_spec::host::could_not_run(format!(
                "HOME {home} does not resolve ({e}): no private-data gate check"
            ))
        });
        // A representative private path that must never appear unless fs == TmpOnly.
        let private_marker = format!("(subpath \"{}\")", sbpl_str(&canon.join("Documents")));
        // A representative secret path that must always appear when network == None.
        let secret_marker = format!("(subpath \"{}\")", sbpl_str(&canon.join(".ssh")));

        let base = ContainmentPolicy::capabilities(ContainmentMode::Containment);

        // (a) Real Containment (network None ∧ fs TmpOnly): both present.
        let full = profile_for(&base).expect("Containment yields a profile");
        assert!(
            full.contains(&secret_marker),
            "secret must be denied; got {full}"
        );
        assert!(
            full.contains(&private_marker),
            "Containment (fs TmpOnly) must deny private data; got {full}"
        );

        // (b) network None but fs broadened to HomeReadWrite: secret YES, private NO.
        let mut broadened = base;
        broadened.fs = FsCapability::HomeReadWrite;
        let prof = profile_for(&broadened).expect("network None still yields a profile");
        assert!(
            prof.contains(&secret_marker),
            "network-denied spawn must still deny credentials; got {prof}"
        );
        assert!(
            !prof.contains(&private_marker),
            "private-data deny must NOT apply when fs != TmpOnly; got {prof}"
        );
    }

    #[test]
    fn non_containment_modes_get_no_sandbox() {
        // User and Safety have full network and are never sandboxed.
        for mode in [ContainmentMode::User, ContainmentMode::Safety] {
            let caps = ContainmentPolicy::capabilities(mode);
            assert_eq!(
                profile_for(&caps),
                None,
                "{mode} must NOT be OS-sandboxed (network not denied)"
            );
        }
    }

    #[test]
    fn profile_selection_keys_only_on_network_capability_none() {
        // "network == None ⇒ Some(profile)": the network field alone decides.
        let base = ContainmentPolicy::capabilities(ContainmentMode::Containment);
        let mut none = base;
        none.network = NetworkCapability::None;
        assert!(profile_for(&none).is_some());

        let mut full = base;
        full.network = NetworkCapability::Full;
        assert!(profile_for(&full).is_none());
    }
}
