// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

//! The CANONICAL per-program states — one spelling each, used verbatim by `status.toml`,
//! the pass log, `atpkg doctor`, `atpkg which`, and the Packages rows
//! (`docs/DESIGN-which-copy-runs-2026-08-27.md` §2).
//!
//! A program is always in exactly one of these:
//!
//! * `managed <build> — pinned by index <N>` — atpkg installed it from the ALab index and
//!   keeps it at the index pin;
//! * `managed <version> — <Vendor> latest` — a vendor-direct program
//!   ([`crate::vendor_direct`]) at its vendor's head; `managed <version> — <why>` while
//!   the head was not taken (a hold, a yank, a rollback). Never a store build id, never an
//!   index;
//! * `system: <path> — not managed by aterm` (`+ (managed copy retired <date>)`) — a
//!   binary the user already had satisfies the member; atpkg downloads nothing;
//! * `managed <build> — SHADOWED by <path>` — atpkg's copy is installed, but a binary
//!   earlier on `PATH` runs instead (a warning, never a fault, never "fixed"); a vendor
//!   build is named by its version here too;
//! * `extra — not installed (opt in: aterm pkg install <name>)` — listed, pinned, waiting
//!   for consent;
//! * `installed via <protocol>: <path>` — obtained through another protocol (`pkg`,
//!   `system-pm`) and proven present by one of the row's `provides` paths;
//! * `needs admin — run: aterm pkg install <name>` — the row needs elevation the
//!   unattended pass cannot supply;
//! * `unavailable on <target>: <hint>` — the pinned build carries no row for this target;
//! * `blocked by <dep>: <dep state>` — the program `requires` `<dep>`
//!   ([`crate::manifest::Program::requires`]) and `<dep>` is not installed, system-
//!   satisfied or installed through its protocol; the tail is the DEPENDENCY's own row,
//!   so the line says whose act unblocks it. Deferred, retried every pass, never a fault.
//! * `held: pinned build <N> is not published for <target>; staying on build <current>` —
//!   an installed member whose group's new pin carries no artifact for this target (a
//!   sibling's row names it: `held: <owner>'s pinned build …`). Deferred, never a fault.
//!
//! Every constructor here is the ONLY place its spelling lives; the parsers beside them
//! (`system_path`, `managed_pin`, …) read the same words back so `doctor` and `which` can
//! never drift from what the pass wrote. Strings are built by hand (no `format!`) — see
//! `lib.rs` on the strict Trust gate.

use std::path::Path;

/// The head of a managed row: `managed <build> …`.
pub const MANAGED_PREFIX: &str = "managed ";
/// The head of a system-satisfied row: `system: <path> …`.
pub const SYSTEM_PREFIX: &str = "system: ";
/// The tail every system-satisfied row carries before the optional retirement note.
pub const SYSTEM_TAIL: &str = " — not managed by aterm";
/// The head of an extra that has not been opted in to.
pub const EXTRA_PREFIX: &str = "extra — not installed";
/// The whole row of an AGENT PROGRAM ([`crate::stub::AGENT_PROGRAMS`]) the pass has not
/// installed yet: default-set on this client, so it is COMING — never `extra — not
/// installed (opt in: …)`, the row an older pass wrote under the opt-in policy.
pub const AGENT_INSTALLING: &str = "agent program — installing";
/// The head of a member obtained through another protocol.
pub const INSTALLED_VIA_PREFIX: &str = "installed via ";
/// The head of a member waiting on elevation.
pub const NEEDS_ADMIN_PREFIX: &str = "needs admin";
/// The head of a member the pinned build does not serve on this target.
pub const UNAVAILABLE_PREFIX: &str = "unavailable on ";
/// The hint an index row that names none falls back to.
pub const UNAVAILABLE_DEFAULT_HINT: &str = "no build is published for this target";
/// The whole row of a member whose channel PIN is on the `yanked` list or below
/// `min_build` ([`crate::gate::ApplyDecision::Tombstone`]): there is no safe build, so
/// nothing installs and a live copy is tombstoned. One spelling for the update lane and
/// both default-set arms, so `which`/`doctor`/Settings read the same words — a
/// `tombstoned:` prefix, which doctor counts as a FAULT (it is one: the fix is upstream).
pub const TOMBSTONED_PIN: &str = "tombstoned: pin yanked/below floor";
/// The head of a member waiting on one of its `requires`: `blocked by <dep>: <dep state>`.
/// Distinct from the `blocked:` FAULT prefix `doctor` matches (`blocked: no build for this
/// architecture`, the toolset-wide verdict): a space, not a colon, follows the word.
pub const BLOCKED_PREFIX: &str = "blocked by ";
/// The head of an installed member held on its current build because its coherence
/// group's new pin is not published for this target: `held: …`.
pub const HELD_PREFIX: &str = "held: ";
/// The `*toolset*` row on a machine the signed index serves NOTHING for — an Intel
/// Mac before x86_64 lands — as the seed and `install
/// --default-set` record it. `doctor` lists it; it is also the one fault nobody on the
/// machine can fix, so the window states it without its Packages badge.
pub const TOOLSET_UNSERVED: &str = "unavailable: no build for this architecture";
/// [`TOOLSET_UNSERVED`] as an update's set completion records it.
pub const TOOLSET_UNSERVED_BLOCKED: &str = "blocked: no build for this architecture";
/// The `*toolset*` row of an empty store the index DOES serve, emptied by
/// `[packages].exclude` alone: an `unavailable:` fault, because the fix is one line of
/// config (atpkg `cli.rs`, `Narrowing::verdict`).
pub const TOOLSET_EXCLUDED: &str = "unavailable: [packages].exclude leaves nothing to install";
/// The `*toolset*` row of an empty store the index DOES serve, where every program it
/// serves was uninstalled on purpose: a decision, so no fault prefix — `doctor` lists
/// nothing for it and the window lights no badge.
pub const TOOLSET_REMOVED: &str = "removed on this machine: nothing to install";
/// The `*toolset*` row of an update that installed nothing on an empty store because the
/// signed index could not be reached — the one exit-2 verdict a retry can change, so the
/// window's loop retries it where it lets every other exit 2 settle.
pub const TOOLSET_INDEX_UNREACHABLE: &str = "unavailable: index unreachable";

/// Whether `state` is the machine-wide no-build-for-this-architecture verdict
/// ([`TOOLSET_UNSERVED`] / [`TOOLSET_UNSERVED_BLOCKED`]).
#[must_use]
pub fn is_unserved_toolset(state: &str) -> bool {
    state == TOOLSET_UNSERVED || state == TOOLSET_UNSERVED_BLOCKED
}

/// `managed <build> — pinned by index <N>`: an index program only — a vendor-direct
/// program's row is [`vendor_managed`] / [`vendor_kept`], because no index pins it.
#[must_use]
pub fn managed(build: u64, index_build: u64) -> String {
    let mut s = String::from(MANAGED_PREFIX);
    s.push_str(&crate::dec_u64(build));
    s.push_str(" — pinned by index ");
    s.push_str(&crate::dec_u64(index_build));
    s
}

/// `managed <version> — <vendor> latest`: a vendor-direct program atpkg installed from its
/// vendor and keeps at the vendor's head. A managed row ([`is_managed`]) that names a
/// version, never a store build id and never an index.
#[must_use]
pub fn vendor_managed(version: &str, vendor: &str) -> String {
    let mut s = String::from(MANAGED_PREFIX);
    s.push_str(version);
    s.push_str(" — ");
    s.push_str(vendor);
    s.push_str(" latest");
    s
}

/// `managed <version> — <why>`: a vendor-direct program kept on `version` while the
/// vendor's head was not taken, saying why (a hold, a yank, a newer install). Managed,
/// but never a claim to be the vendor's latest.
#[must_use]
pub fn vendor_kept(version: &str, why: &str) -> String {
    let mut s = String::from(MANAGED_PREFIX);
    s.push_str(version);
    s.push_str(" — ");
    s.push_str(why);
    s
}

/// `managed <version> — updates from <vendor>`: a vendor-direct build no lane has judged
/// yet (no row, or a row an older client wrote) — its source, and no claim about the head.
#[must_use]
pub fn vendor_source(version: &str, vendor: &str) -> String {
    let mut why = String::from("updates from ");
    why.push_str(vendor);
    vendor_kept(version, &why)
}

/// `Some((version, why))` for a vendor-direct managed row — [`vendor_managed`],
/// [`vendor_kept`] or [`vendor_source`]; `None` for an index row, a SHADOWED row and every
/// other state.
#[must_use]
pub fn vendor_row(state: &str) -> Option<(&str, &str)> {
    let rest = state.strip_prefix(MANAGED_PREFIX)?;
    let (version, why) = rest.split_once(" — ")?;
    crate::vendor_direct::Version::parse(version)?;
    if why.is_empty() || why.starts_with("SHADOWED ") {
        return None;
    }
    Some((version, why))
}

/// `Some((version, vendor))` for exactly `managed <version> — <vendor> latest` where
/// `vendor` is a vendor of [`crate::vendor_direct::VENDORS`]: the row that says the build
/// IS its vendor's head. Every kept reason reads `None`.
#[must_use]
pub fn vendor_latest(state: &str) -> Option<(&str, &str)> {
    let (version, why) = vendor_row(state)?;
    let vendor = why.strip_suffix(" latest")?;
    crate::vendor_direct::VENDORS
        .iter()
        .any(|s| s.vendor == vendor)
        .then_some((version, vendor))
}

/// `system: <path> — not managed by aterm`, plus ` (managed copy retired <date>)` when
/// `retired` names the day atpkg retired its own copy in favour of this one.
#[must_use]
pub fn system(path: &Path, retired: Option<&str>) -> String {
    let mut s = String::from(SYSTEM_PREFIX);
    s.push_str(&path.display().to_string());
    s.push_str(SYSTEM_TAIL);
    if let Some(date) = retired.filter(|d| !d.is_empty()) {
        s.push_str(" (managed copy retired ");
        s.push_str(date);
        s.push(')');
    }
    s
}

/// `managed <build> — SHADOWED by <path>`; a vendor-direct build is named by its version
/// (`managed 2.1.280 — SHADOWED by …`), never its store id.
#[must_use]
pub fn shadowed(build: u64, path: &Path) -> String {
    let mut s = String::from(MANAGED_PREFIX);
    s.push_str(&crate::vendor_direct::build_label(build));
    s.push_str(" — SHADOWED by ");
    s.push_str(&path.display().to_string());
    s
}

/// The tail of an AGENT PROGRAM's shell-local shadow row: `managed <build> — SHADOWED in
/// this shell by <path>…` ([`agent_shadowed_in_shell`]). Distinct from [`shadowed`]'s
/// ` — SHADOWED by ` on purpose: [`shadowed_by`] must never read this row as the
/// machine-wide state, because it is not one.
pub const AGENT_SHADOWED_IN_SHELL: &str = " — SHADOWED in this shell by ";

/// `managed <build> — SHADOWED in this shell by <path>: its PATH has no <agents_dir> (a
/// shell that has not sourced the atpkg hook) — \`<remedy>\` here picks the managed copy
/// up; a tab opened on this build puts agents/ first on its own` — or, with `agents_on_path`, `…: its PATH has
/// <agents_dir> behind <path's dir> — \`<remedy>\` here moves it to the front; …`.
///
/// The AGENT PROGRAMS' shadow row (2026-09-16). For `claude`/`codex` aterm's copy IS what
/// runs (owner decision 2026-09-10) — through `agents/` first on every session `PATH` — so
/// a foreign copy ahead of it is not the machine's state but ONE SHELL's: a tab adopted
/// across the update that introduced `agents/`, or opened before the seed pass created it.
/// Owner, 2026-09-16: *"it is telling me to open a new tab. NO! all the latest and best
/// MUST WORK IN THE SAME TAB"* — so the remedy named is in-place, never a new tab, and
/// never "remove or reorder that copy". `remedy` is the command the CALLER has checked is
/// true for this machine (`cli::shell_remedy_command`): the hook sourced in place —
/// `. ~/.aterm/shell.d/00-atpkg.zsh` (`source …fish`, `. …ps1`) — wherever that hook
/// file exists, the same sentence the window's status row says, and a `PATH` line in
/// the shell's dialect only where it does not. NEVER `exec $SHELL`: measured the same day,
/// a re-exec'd shell inside an aterm tab loses the tab's shell integration (the zsh
/// wrapper consumes `ATERM_ORIGINAL_ZDOTDIR`; bash rides `--rcfile`), while the source
/// heals `PATH` and keeps it. The cause is stated as what is KNOWN — this shell has not run
/// the hook — not as a guess about when it was opened. NEVER RECORDED: `status.toml` and
/// the Packages row keep the machine-wide managed row (`managed <version> — <Vendor>
/// latest`) for such a program.
#[must_use]
pub fn agent_shadowed_in_shell(
    build: u64,
    path: &Path,
    agents_dir: &Path,
    agents_on_path: bool,
    remedy: &str,
) -> String {
    let mut s = String::from(MANAGED_PREFIX);
    s.push_str(&crate::vendor_direct::build_label(build));
    s.push_str(" — ");
    s.push_str(&agent_shell_shadow_tail(
        path,
        agents_dir,
        agents_on_path,
        remedy,
    ));
    s
}

/// `managed <build> — routed at exec time by <stub>: inside aterm it runs the managed copy
/// ahead of <foreign>, which this shell's PATH puts first — a shell that ran `<name>` before
/// the stub was laid keeps the path it cached until `rehash` (zsh) / `hash -r` (bash), once
/// (aterm help reroute)`.
///
/// The AGENT PROGRAMS' row for a shell whose `PATH` puts a foreign copy ahead of `agents/`
/// but whose reroute stub answers first and takes the twin (2026-09-23,
/// `reroute::agents_stub_body_sh`): what runs is the managed copy, decided when it runs —
/// the case [`agent_shadowed_in_shell`] used to report as SHADOWED, measured that day in a
/// session shell from 2026-09-10 whose `PATH` had no `agents/` at all. Not SHADOWED — but
/// not remedy-free either: this report runs as a CHILD of the shell and cannot see its
/// command cache, and zsh and bash keep the path a name was first found at, so a shell
/// that ran the program before the stub existed runs that cached path until it forgets it
/// (the 2026-09-23 review measured it in zsh and bash). The row names that one step.
/// Never recorded, like the shadow row.
#[must_use]
pub fn agent_routed_in_shell(build: u64, stub: &Path, foreign: &Path) -> String {
    let name = stub
        .file_name()
        .map_or_else(String::new, |n| n.to_string_lossy().into_owned());
    let mut s = String::from(MANAGED_PREFIX);
    s.push_str(&crate::vendor_direct::build_label(build));
    s.push_str(" — routed at exec time by ");
    s.push_str(&stub.display().to_string());
    s.push_str(": inside aterm it runs the managed copy ahead of ");
    s.push_str(&foreign.display().to_string());
    s.push_str(", which this shell's PATH puts first — a shell that ran `");
    s.push_str(&name);
    s.push_str("` before the stub was laid keeps the path it cached until `rehash` (zsh) / `hash -r` (bash), once (aterm help reroute)");
    s
}

/// `managed <build> — not what runs outside aterm: <stub> passes through to <foreign>, this
/// shell's own copy (the managed copy leads inside aterm only; aterm help reroute)`.
///
/// The AGENT PROGRAMS' row for a shell OUTSIDE aterm whose reroute stub answers first
/// (2026-09-23, `reroute::agents_stub_body_sh`): the stub runs the user's own copy there
/// by design (owner law, 03513b5d7), so this is no shadow and names no remedy. Never
/// recorded.
#[must_use]
pub fn agent_passed_through(build: u64, stub: &Path, foreign: &Path) -> String {
    let mut s = String::from(MANAGED_PREFIX);
    s.push_str(&crate::vendor_direct::build_label(build));
    s.push_str(" — not what runs outside aterm: ");
    s.push_str(&stub.display().to_string());
    s.push_str(" passes through to ");
    s.push_str(&foreign.display().to_string());
    s.push_str(
        ", this shell's own copy (the managed copy leads inside aterm only; aterm help reroute)",
    );
    s
}

/// The tail of [`agent_shadowed_in_shell`] after `managed <build> — `: `SHADOWED in this
/// shell by <path>: its PATH has …; a tab opened on this build puts agents/ first on its own`. For a shell a
/// user is in (`doctor`, `which`) — the pass's own `PATH` is no user's shell, and
/// `cli::reconcile_shadowed` prints its own words for it.
#[must_use]
pub fn agent_shell_shadow_tail(
    path: &Path,
    agents_dir: &Path,
    agents_on_path: bool,
    remedy: &str,
) -> String {
    let mut s = String::from(AGENT_SHADOWED_IN_SHELL.trim_start_matches(" — "));
    s.push_str(&path.display().to_string());
    s.push_str(": its PATH has ");
    if agents_on_path {
        s.push_str(&agents_dir.display().to_string());
        s.push_str(" behind ");
        s.push_str(
            &path
                .parent()
                .map_or_else(|| path.display().to_string(), |d| d.display().to_string()),
        );
        s.push_str(" — `");
        s.push_str(remedy);
        s.push_str("` here moves it to the front");
    } else {
        s.push_str("no ");
        s.push_str(&agents_dir.display().to_string());
        s.push_str(" (a shell that has not sourced the atpkg hook) — `");
        s.push_str(remedy);
        s.push_str("` here picks the managed copy up");
    }
    s.push_str("; a tab opened on this build puts agents/ first on its own");
    s
}

/// `extra — not installed (opt in: aterm pkg install <name>)`.
#[must_use]
pub fn extra_not_installed(name: &str) -> String {
    let mut s = String::from(EXTRA_PREFIX);
    s.push_str(" (opt in: aterm pkg install ");
    s.push_str(name);
    s.push(')');
    s
}

/// `agent program — installing` ([`AGENT_INSTALLING`]): the one row a wanted agent program
/// carries between adoption and its install.
#[must_use]
pub fn agent_installing() -> String {
    String::from(AGENT_INSTALLING)
}

/// `installed via <protocol>: <path>`.
#[must_use]
pub fn installed_via(protocol: &str, path: &Path) -> String {
    let mut s = String::from(INSTALLED_VIA_PREFIX);
    s.push_str(protocol);
    s.push_str(": ");
    s.push_str(&path.display().to_string());
    s
}

/// `needs admin — run: aterm pkg install <name>`.
#[must_use]
pub fn needs_admin(name: &str) -> String {
    let mut s = String::from(NEEDS_ADMIN_PREFIX);
    s.push_str(" — run: aterm pkg install ");
    s.push_str(name);
    s
}

/// `unavailable on <target>: <hint>` — `hint` empty ⇒ [`UNAVAILABLE_DEFAULT_HINT`].
#[must_use]
pub fn unavailable(target: &str, hint: &str) -> String {
    let mut s = String::from(UNAVAILABLE_PREFIX);
    s.push_str(target);
    s.push_str(": ");
    s.push_str(if hint.is_empty() {
        UNAVAILABLE_DEFAULT_HINT
    } else {
        hint
    });
    s
}

/// `blocked by <dep>: <dep state>` — the program requires `dep` and `dep` is not yet
/// installed, system-satisfied or installed through its protocol. `dep_state` is the
/// dependency's OWN canonical row (`needs admin — run: aterm pkg install clt`, `extra —
/// not installed (opt in: aterm pkg install codex)`, `error: …`), quoted verbatim, so the
/// blocked row names the act that unblocks it. A per-program DEFERRED state, not a fault:
/// the next pass retries, and nothing downloads for a blocked program.
#[must_use]
pub fn blocked(dep: &str, dep_state: &str) -> String {
    let mut s = String::from(BLOCKED_PREFIX);
    s.push_str(dep);
    s.push_str(": ");
    s.push_str(dep_state);
    s
}

/// `Some((dep, dep_state))` for a `blocked by <dep>: <dep state>` state; `None` for every
/// other state. The inverse of [`blocked`] (a dependency name never contains `: `).
#[must_use]
pub fn blocked_by(state: &str) -> Option<(&str, &str)> {
    let rest = state.strip_prefix(BLOCKED_PREFIX)?;
    let (dep, dep_state) = rest.split_once(": ")?;
    if dep.is_empty() || dep_state.is_empty() {
        return None;
    }
    Some((dep, dep_state))
}

/// `held: pinned build <N> is not published for <target>; staying on build <current>` —
/// or `held: <owner>'s pinned build <N> …` on a sibling's row. The update lane's row for an
/// installed member whose group cannot move on this target because `owner`'s new pin (the
/// row's own program when `owner` is `None`) carries no artifact for it. Deferred, not a
/// fault: the pass that finds the build published moves the group.
#[must_use]
pub fn held_unpublished(owner: Option<&str>, build: u64, target: &str, current: u64) -> String {
    let mut s = String::from(HELD_PREFIX);
    if let Some(owner) = owner {
        s.push_str(owner);
        s.push_str("'s ");
    }
    s.push_str("pinned build ");
    s.push_str(&crate::dec_u64(build));
    s.push_str(" is not published for ");
    s.push_str(target);
    s.push_str("; staying on build ");
    s.push_str(&crate::dec_u64(current));
    s
}

/// The `<path>` of a `system: <path> — not managed by aterm…` state, or `None` for any
/// other state. The inverse of [`system`]; the retirement note is dropped.
#[must_use]
pub fn system_path(state: &str) -> Option<&str> {
    let rest = state.strip_prefix(SYSTEM_PREFIX)?;
    let end = rest.find(SYSTEM_TAIL)?;
    Some(&rest[..end])
}

/// The `<date>` of a system state's ` (managed copy retired <date>)` note, if present.
#[must_use]
pub fn system_retired(state: &str) -> Option<&str> {
    let rest = state.strip_prefix(SYSTEM_PREFIX)?;
    let note = rest.rsplit_once(" (managed copy retired ")?.1;
    note.strip_suffix(')')
}

/// `Some((protocol, path))` for an `installed via <protocol>: <path>` state; `None` for
/// every other state. The inverse of [`installed_via`].
#[must_use]
pub fn installed_via_path(state: &str) -> Option<(&str, &str)> {
    let rest = state.strip_prefix(INSTALLED_VIA_PREFIX)?;
    let (protocol, path) = rest.split_once(": ")?;
    if protocol.is_empty() || path.is_empty() {
        return None;
    }
    Some((protocol, path))
}

/// `Some((build, index))` for a `managed <build> — pinned by index <N>` state; `None`
/// for a SHADOWED managed row and for every other state.
#[must_use]
pub fn managed_pin(state: &str) -> Option<(u64, u64)> {
    let rest = state.strip_prefix(MANAGED_PREFIX)?;
    let (build, tail) = rest.split_once(" — pinned by index ")?;
    Some((build.parse().ok()?, tail.parse().ok()?))
}

/// `Some((build, path))` for a `managed <build> — SHADOWED by <path>` state; a version in
/// the build's place (a vendor-direct build, [`shadowed`]) reads back as its store id.
#[must_use]
pub fn shadowed_by(state: &str) -> Option<(u64, &str)> {
    let rest = state.strip_prefix(MANAGED_PREFIX)?;
    let (build, path) = rest.split_once(" — SHADOWED by ")?;
    let build = match build.parse() {
        Ok(b) => b,
        Err(_) => crate::vendor_direct::Version::parse(build)?.build_id(),
    };
    Some((build, path))
}

/// Whether `state` is a managed row of any spelling (pinned, vendor or SHADOWED).
#[must_use]
pub fn is_managed(state: &str) -> bool {
    state.starts_with(MANAGED_PREFIX)
}

/// The fix-line that rides AFTER a SHADOWED row on the surfaces that speak to a person
/// (the pass log, `doctor`, `which`): `type alab-<tool> for the managed one`. `alias` is
/// the alias shim that actually resolves (`crate::store::ToolName::alias`), so the
/// sentence is printed only when typing it works. NEVER part of the canonical state
/// string — [`shadowed`] stays exactly as spelled, `status.toml` and the Packages row
/// carry the state alone, and the parsers above never see this text.
#[must_use]
pub fn alias_hint(alias: &str) -> String {
    let mut s = String::from("type ");
    s.push_str(alias);
    s.push_str(" for the managed one");
    s
}

/// Where `program`'s builds come from, as the package log and Settings ▸ Packages name it
/// (Phase 4): `Anthropic latest` / `OpenAI latest` for a vendor-direct program — its
/// vendor's channel, whatever its row says this pass — and `ALab index <N>` for everything
/// else, `N` being the index its row is pinned by, else the one the last pass resolved
/// (`last_index_build`; `0` when none has), else no number at all.
#[must_use]
pub fn source_words(program: &str, state: &str, last_index_build: u64) -> String {
    if let Some(spec) = crate::vendor_direct::spec(program) {
        let mut s = String::from(spec.vendor);
        s.push_str(" latest");
        return s;
    }
    let index = managed_pin(state).map_or(last_index_build, |(_, index)| index);
    let mut s = String::from("ALab index");
    if index > 0 {
        s.push(' ');
        s.push_str(&crate::dec_u64(index));
    }
    s
}

/// Why a row is NOT current, when it says so: a one-word `result` (what the package log and
/// the Activity list call it) and the `reason` in the row's own words. `None` for a row that
/// is current or says nothing wrong — pinned by its index, its vendor's latest, a vendor
/// build no lane has judged yet, system-satisfied, installed through another protocol, an
/// extra waiting for consent, an agent on its way, a SHADOWED row (per-shell truth, never a
/// fault) — and for any spelling this build does not know. Reads the canonical spellings
/// above and the fault prefixes `doctor` matches; never a second vocabulary.
#[must_use]
pub fn not_current(state: &str) -> Option<(&'static str, &str)> {
    if let Some((_, why)) = vendor_row(state) {
        if vendor_latest(state).is_some() || why.starts_with("updates from ") {
            return None;
        }
        let result = if why.starts_with("held") {
            "held"
        } else if why.starts_with("rolled back") {
            "rolled back"
        } else if why.ends_with(" is yanked") {
            "yanked"
        } else if why.contains("not reached") {
            "not reached"
        } else {
            "kept"
        };
        return Some((result, why));
    }
    let fault = |prefix: &str| state.strip_prefix(prefix).map(str::trim_start);
    if let Some(rest) = fault("error:") {
        return Some((
            if is_refusal(rest) {
                "refused"
            } else {
                "failed"
            },
            state,
        ));
    }
    let result = if state.starts_with("aborted:") {
        "failed"
    } else if state.starts_with("rejected:") {
        "refused"
    } else if state.starts_with("tombstoned:") {
        "disabled"
    } else if state.starts_with("deferred:") {
        "deferred"
    } else if state.starts_with(HELD_PREFIX) {
        "held"
    } else if state.starts_with("unavailable:")
        || state.starts_with("blocked:")
        || state.starts_with(UNAVAILABLE_PREFIX)
    {
        "unavailable"
    } else if state.starts_with(NEEDS_ADMIN_PREFIX) || state.starts_with(BLOCKED_PREFIX) {
        "waiting"
    } else {
        return None;
    };
    Some((result, state))
}

/// Whether an `error:` row's reason is a REFUSAL — atpkg declining bytes or a document it
/// could not trust — as its writers spell one, at the HEAD of the reason: the vendor lane's
/// `<program> [<version>] refused: <why>` (`vendor_direct::lane`'s outcome line, which
/// `record_vendor_status` records), the flow's `manifest refused: `, `artifact row
/// refused: ` and `the signed row for <program> was refused: `, and a stage's `stage:
/// signer refused: `. Never a substring: an error that
/// quotes the OS's `Connection refused (os error 61)` is a network failure, and naming it a
/// refusal told a person a digest or signature had not checked out when a host was down
/// (review of Phase 4, 2026-09-23).
fn is_refusal(reason: &str) -> bool {
    const HEADS: [&str; 3] = [
        "manifest refused: ",
        "artifact row refused: ",
        "stage: signer refused: ",
    ];
    if HEADS.iter().any(|head| reason.starts_with(head))
        || reason
            .strip_prefix("the signed row for ")
            .and_then(|rest| rest.split_once(" was refused: "))
            .is_some_and(|(program, _)| !program.contains(' '))
    {
        return true;
    }
    let Some((subject, _)) = reason.split_once(" refused: ") else {
        return false;
    };
    let mut words = subject.split(' ');
    let program = words.next().unwrap_or_default();
    let version = words.next();
    crate::vendor_direct::spec(program).is_some()
        && words.next().is_none()
        && version.is_none_or(|v| crate::vendor_direct::Version::parse(v).is_some())
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Phase 4's two words per row: where the builds come from, and why a row is not
    /// current — read off the canonical spellings, so the package log, the Activity list
    /// and the program rows say what `doctor` says.
    #[test]
    fn a_rows_source_and_why_it_is_not_current_come_from_its_canonical_words() {
        assert_eq!(
            source_words("claude", &vendor_managed("2.1.280", "Anthropic"), 44),
            "Anthropic latest"
        );
        assert_eq!(source_words("codex", "error: x", 0), "OpenAI latest");
        assert_eq!(source_words("ay", &managed(1971, 41), 44), "ALab index 41");
        assert_eq!(
            source_words("ay", "error: stage failed", 44),
            "ALab index 44"
        );
        assert_eq!(source_words("ay", "active", 0), "ALab index");

        for current in [
            managed(1971, 41),
            vendor_managed("2.1.280", "Anthropic"),
            vendor_source("0.156.0", "OpenAI"),
            shadowed(1971, Path::new("/opt/homebrew/bin/ay")),
            system(Path::new("/usr/bin/git"), None),
            extra_not_installed("vendorx"),
            agent_installing(),
            String::from("active"),
            String::new(),
        ] {
            assert_eq!(not_current(&current), None, "{current}");
        }
        let held = vendor_kept("2.1.280", "held by local pin");
        assert_eq!(not_current(&held), Some(("held", "held by local pin")));
        let yanked = vendor_kept("2.1.279", "Anthropic's latest (2.1.281) is yanked");
        assert_eq!(not_current(&yanked).map(|n| n.0), Some("yanked"));
        let unreached = vendor_kept(
            "2.1.280",
            "Anthropic's release channel not reached since 2026-09-20",
        );
        assert_eq!(not_current(&unreached).map(|n| n.0), Some("not reached"));
        let rolled = vendor_kept("2.1.279", "rolled back from yanked 2.1.280");
        assert_eq!(not_current(&rolled).map(|n| n.0), Some("rolled back"));
        let refused = "error: claude 2.1.281 refused: digest mismatch — keeping 2.1.280";
        assert_eq!(not_current(refused), Some(("refused", refused)));
        for refusal in [
            "error: codex refused: no release for this target",
            "error: manifest refused: retired kind",
            "error: artifact row refused: url host is evil",
            "error: stage: signer refused: team id differs",
            "error: the signed row for ay was refused: url host is evil",
        ] {
            assert_eq!(
                not_current(refusal).map(|n| n.0),
                Some("refused"),
                "{refusal}"
            );
        }
        // A network error that quotes the OS's words is a failure, never a refusal.
        for failure in [
            "error: fetch https://downloads.claude.ai/x: Connection refused (os error 61)",
            "error: download: proxy connection refused: tunnel closed",
            "error: claude 2.1.281: connection refused",
            "error: someprogram 1.2.3 refused: not a vendor lane line",
        ] {
            assert_eq!(
                not_current(failure).map(|n| n.0),
                Some("failed"),
                "{failure}"
            );
        }
        assert_eq!(
            not_current("error: stage: disk full").map(|n| n.0),
            Some("failed")
        );
        assert_eq!(not_current(TOMBSTONED_PIN).map(|n| n.0), Some("disabled"));
        assert_eq!(
            not_current(&held_unpublished(None, 12, "x86_64-apple-darwin", 11)).map(|n| n.0),
            Some("held")
        );
        assert_eq!(
            not_current(&needs_admin("clt")).map(|n| n.0),
            Some("waiting")
        );
        assert_eq!(
            not_current(TOOLSET_INDEX_UNREACHABLE).map(|n| n.0),
            Some("unavailable")
        );
    }

    /// A vendor row is managed, names the version and the vendor, and is no index pin.
    #[test]
    fn a_vendor_row_is_managed_by_version_and_vendor() {
        let row = vendor_managed("2.1.280", "Anthropic");
        assert_eq!(row, "managed 2.1.280 — Anthropic latest");
        assert!(is_managed(&row));
        assert_eq!(managed_pin(&row), None);
        assert_eq!(shadowed_by(&row), None);
        assert_eq!(vendor_row(&row), Some(("2.1.280", "Anthropic latest")));
        assert_eq!(vendor_latest(&row), Some(("2.1.280", "Anthropic")));
        let kept = vendor_kept("2.1.280", "held by local pin");
        assert_eq!(kept, "managed 2.1.280 — held by local pin");
        assert!(is_managed(&kept) && !kept.contains("latest"));
        assert_eq!(managed_pin(&kept), None);
        assert_eq!(vendor_row(&kept), Some(("2.1.280", "held by local pin")));
        assert_eq!(vendor_latest(&kept), None);
        let source = vendor_source("0.156.0", "OpenAI");
        assert_eq!(source, "managed 0.156.0 — updates from OpenAI");
        assert_eq!(vendor_latest(&source), None);
        // A kept reason that merely mentions the latest is no claim to be it.
        let newer = vendor_kept("2.1.281", "newer than Anthropic's latest (2.1.280)");
        assert_eq!(vendor_latest(&newer), None);
        assert_eq!(vendor_latest("managed 2.1.280 — Nobody latest"), None);
        // No index row, SHADOWED row or non-version reads as a vendor row.
        for other in [
            managed(6808, 41),
            shadowed(6808, Path::new("/p")),
            shadowed(
                crate::vendor_direct::Version::parse("2.1.280")
                    .unwrap()
                    .build_id(),
                Path::new("/p"),
            ),
            String::from("managed 2.1 — Anthropic latest"),
            String::from("managed 2.1.280 — "),
        ] {
            assert_eq!(vendor_row(&other), None, "{other}");
            assert_eq!(vendor_latest(&other), None, "{other}");
        }
    }

    /// A vendor build's SHADOWED rows name its version, never its 19-digit store id, and
    /// the machine-wide one reads back as that id.
    #[test]
    fn a_vendor_shadow_row_names_the_version_and_reads_back_the_build() {
        let build = crate::vendor_direct::Version::parse("2.1.280")
            .unwrap()
            .build_id();
        let row = shadowed(build, Path::new("/Users//dev/.local/bin/claude"));
        assert_eq!(
            row,
            "managed 2.1.280 — SHADOWED by /Users//dev/.local/bin/claude"
        );
        assert_eq!(
            shadowed_by(&row),
            Some((build, "/Users//dev/.local/bin/claude"))
        );
        let shell = agent_shadowed_in_shell(
            build,
            Path::new("/Users//dev/.local/bin/claude"),
            Path::new("/p/agents"),
            false,
            ". ~/.aterm/shell.d/00-atpkg.zsh",
        );
        assert!(shell.starts_with("managed 2.1.280 — SHADOWED in this shell by "));
        for s in [&row, &shell] {
            assert!(!s.contains(&build.to_string()), "{s}");
        }
        assert_eq!(shadowed_by("managed 2.1 — SHADOWED by /p"), None);
    }

    #[test]
    fn every_canonical_spelling_is_exact() {
        assert_eq!(managed(6808, 41), "managed 6808 — pinned by index 41");
        assert_eq!(
            system(Path::new("/opt/homebrew/bin/gh"), None),
            "system: /opt/homebrew/bin/gh — not managed by aterm"
        );
        assert_eq!(
            system(Path::new("/opt/homebrew/bin/gh"), Some("2026-08-27")),
            "system: /opt/homebrew/bin/gh — not managed by aterm (managed copy retired 2026-08-27)"
        );
        assert_eq!(
            system(Path::new("/opt/homebrew/bin/gh"), Some("")),
            "system: /opt/homebrew/bin/gh — not managed by aterm",
            "an empty date is no note"
        );
        assert_eq!(
            shadowed(6808, Path::new("/Users//dev/.local/bin/trust")),
            "managed 6808 — SHADOWED by /Users//dev/.local/bin/trust"
        );
        assert_eq!(
            extra_not_installed("codex"),
            "extra — not installed (opt in: aterm pkg install codex)"
        );
        assert_eq!(
            installed_via("pkg", Path::new("/opt/homebrew/bin/brew")),
            "installed via pkg: /opt/homebrew/bin/brew"
        );
        assert_eq!(
            needs_admin("homebrew"),
            "needs admin — run: aterm pkg install homebrew"
        );
        assert_eq!(
            unavailable("x86_64-unknown-linux-gnu", "Emacs is a macOS-only member"),
            "unavailable on x86_64-unknown-linux-gnu: Emacs is a macOS-only member"
        );
        assert_eq!(
            unavailable("aarch64-pc-windows-msvc", ""),
            "unavailable on aarch64-pc-windows-msvc: no build is published for this target"
        );
        assert_eq!(
            blocked("clt", &needs_admin("clt")),
            "blocked by clt: needs admin — run: aterm pkg install clt"
        );
        assert_eq!(
            blocked("codex", &extra_not_installed("codex")),
            "blocked by codex: extra — not installed (opt in: aterm pkg install codex)"
        );
        // The alias fix-line is a SEPARATE sentence: the SHADOWED state never carries it,
        // so a row that was written and read back stays byte-identical.
        assert_eq!(
            alias_hint("alab-trust"),
            "type alab-trust for the managed one"
        );
        let row = shadowed(6808, Path::new("/opt/homebrew/bin/trust"));
        assert!(!row.contains("alab-"));
        assert_eq!(
            shadowed_by(&row),
            Some((6808, "/opt/homebrew/bin/trust")),
            "the parser reads the bare row"
        );
    }

    /// The agent programs' shell-local shadow row (2026-09-16): the in-place remedy,
    /// never a new tab, never "remove that copy"; and NOT a `SHADOWED by` row to the
    /// parser, because it is one shell's state, not the machine's.
    #[test]
    fn the_agent_shadow_row_names_the_in_place_remedy_and_is_never_the_machine_state() {
        let agents = Path::new("/Users//dev/Library/Application Support/aterm/pkg/agents");
        let absent = agent_shadowed_in_shell(
            2_026_091_601,
            Path::new("/Users//dev/.local/bin/claude"),
            agents,
            false,
            ". ~/.aterm/shell.d/00-atpkg.zsh",
        );
        assert_eq!(
            absent,
            "managed 2026091601 — SHADOWED in this shell by /Users//dev/.local/bin/claude: its \
             PATH has no /Users//dev/Library/Application Support/aterm/pkg/agents (a shell \
             that has not sourced the atpkg hook) — `. ~/.aterm/shell.d/00-atpkg.zsh` here \
             picks the managed copy up; a tab opened on this build puts agents/ first on its own"
        );
        let behind = agent_shadowed_in_shell(
            2_026_091_601,
            Path::new("/opt/homebrew/bin/codex"),
            agents,
            true,
            "source ~/.aterm/shell.d/00-atpkg.fish",
        );
        assert_eq!(
            behind,
            "managed 2026091601 — SHADOWED in this shell by /opt/homebrew/bin/codex: its PATH \
             has /Users//dev/Library/Application Support/aterm/pkg/agents behind \
             /opt/homebrew/bin — `source ~/.aterm/shell.d/00-atpkg.fish` here moves it to \
             the front; a tab opened on this build puts agents/ first on its own"
        );
        // The cause is what is known, never a guess about when the shell was opened.
        assert!(!absent.contains("before the install"), "{absent}");
        for row in [&absent, &behind] {
            assert!(is_managed(row), "{row}");
            assert_eq!(shadowed_by(row), None, "not the machine-wide row: {row}");
            assert_eq!(managed_pin(row), None, "{row}");
            assert!(!row.contains("new tab"), "never a new tab: {row}");
            assert!(
                !row.contains("remove"),
                "never the user's copy to remove: {row}"
            );
            for fault in [
                "error:",
                "unavailable:",
                "blocked:",
                "aborted:",
                "tombstoned:",
            ] {
                assert!(!row.starts_with(fault), "{row}");
            }
        }
    }

    #[test]
    fn the_parsers_read_back_exactly_what_the_constructors_wrote() {
        let plain = system(Path::new("/opt/homebrew/bin/gh"), None);
        assert_eq!(system_path(&plain), Some("/opt/homebrew/bin/gh"));
        assert_eq!(system_retired(&plain), None);
        let noted = system(Path::new("/opt/homebrew/bin/gh"), Some("2026-08-27"));
        assert_eq!(system_path(&noted), Some("/opt/homebrew/bin/gh"));
        assert_eq!(system_retired(&noted), Some("2026-08-27"));
        // A path that itself contains the tail's words still parses at the FIRST tail.
        let odd = system(Path::new("/x — not managed by aterm/gh"), None);
        assert_eq!(system_path(&odd), Some("/x"));
        assert_eq!(managed_pin(&managed(6808, 41)), Some((6808, 41)));
        assert_eq!(managed_pin(&shadowed(6808, Path::new("/x/trust"))), None);
        assert_eq!(
            shadowed_by(&shadowed(6808, Path::new("/x/trust"))),
            Some((6808, "/x/trust"))
        );
        assert_eq!(shadowed_by(&managed(6808, 41)), None);
        assert_eq!(
            installed_via_path(&installed_via("pkg", Path::new("/opt/homebrew/bin/brew"))),
            Some(("pkg", "/opt/homebrew/bin/brew"))
        );
        assert_eq!(
            installed_via_path(&installed_via(
                "softwareupdate",
                Path::new("/Library/Developer/CommandLineTools/usr/bin/git")
            )),
            Some((
                "softwareupdate",
                "/Library/Developer/CommandLineTools/usr/bin/git"
            ))
        );
        assert_eq!(installed_via_path(&needs_admin("brew")), None);
        assert_eq!(installed_via_path("installed via pkg: "), None);
        // The blocked row quotes the dependency's row VERBATIM, colons and all, and the
        // parser gives it back whole.
        let b = blocked("clt", &needs_admin("clt"));
        assert_eq!(
            blocked_by(&b),
            Some(("clt", "needs admin — run: aterm pkg install clt"))
        );
        let nested = blocked("brew", &blocked("clt", "error: x: y"));
        assert_eq!(
            blocked_by(&nested),
            Some(("brew", "blocked by clt: error: x: y"))
        );
        assert_eq!(blocked_by(&needs_admin("brew")), None);
        assert_eq!(blocked_by("blocked by clt: "), None);
        assert_eq!(blocked_by("blocked: no build for this architecture"), None);
        assert!(is_managed(&managed(1, 1)) && is_managed(&shadowed(1, Path::new("/p"))));
        for other in [
            "active",
            "error: x",
            &extra_not_installed("codex"),
            &needs_admin("brew"),
            &unavailable("t", ""),
            &installed_via("pkg", Path::new("/p")),
            &blocked("clt", &needs_admin("clt")),
            &held_unpublished(None, 6, "t", 5),
            &held_unpublished(Some("tb"), 6, "t", 3),
        ] {
            assert_eq!(system_path(other), None, "{other}");
            assert_eq!(managed_pin(other), None, "{other}");
            assert!(!is_managed(other), "{other}");
            if !other.starts_with(INSTALLED_VIA_PREFIX) {
                assert_eq!(installed_via_path(other), None, "{other}");
            }
        }
    }

    /// The states doctor treats as FAULTS are prefix-matched (`error:`, `unavailable:`,
    /// `blocked:`, `aborted:`, `tombstoned:`); none of the canonical spellings may start
    /// with one of those, or a normal row would read as a problem.
    #[test]
    fn no_canonical_state_reads_as_a_doctor_fault() {
        for s in [
            managed(1, 1),
            vendor_managed("2.1.280", "Anthropic"),
            vendor_kept("2.1.280", "held by local pin"),
            vendor_source("0.156.0", "OpenAI"),
            system(Path::new("/p"), Some("2026-01-01")),
            shadowed(1, Path::new("/p")),
            extra_not_installed("codex"),
            installed_via("pkg", Path::new("/p")),
            needs_admin("brew"),
            unavailable("t", "h"),
            blocked("clt", &needs_admin("clt")),
            held_unpublished(None, 6, "t", 5),
            held_unpublished(Some("tb"), 6, "t", 3),
        ] {
            for fault in [
                "error:",
                "unavailable:",
                "blocked:",
                "aborted:",
                "tombstoned:",
            ] {
                assert!(!s.starts_with(fault), "{s} would read as a {fault} fault");
            }
        }
    }
}
