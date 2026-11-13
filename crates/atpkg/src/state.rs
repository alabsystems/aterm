// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

//! The canonical per-program states: one spelling each, used verbatim by `status.toml`, the pass log, `atpkg doctor`, `atpkg which` and the Packages rows.

use std::path::Path;

/// The head of a managed row: `managed <build> …`.
pub const MANAGED_PREFIX: &str = "managed ";
/// The head of a system-satisfied row: `system: <path> …`.
pub const SYSTEM_PREFIX: &str = "system: ";
/// The tail every system-satisfied row carries before the optional retirement note.
pub const SYSTEM_TAIL: &str = " — not managed by aterm";
/// The whole row of a wanted agent program ([`crate::stub::AGENT_PROGRAMS`]) the pass has not installed yet: it is default-set, so it is coming.
pub const AGENT_INSTALLING: &str = "agent program — installing";
/// The head of a member the pinned build does not serve on this target.
pub const UNAVAILABLE_PREFIX: &str = "unavailable on ";
/// The hint an index row that names none falls back to.
pub const UNAVAILABLE_DEFAULT_HINT: &str = "no build is published for this target";
/// The whole row of a member whose channel pin is yanked or below `min_build` ([`crate::gate::ApplyDecision::Tombstone`]): no safe build, so nothing installs and a live copy is tombstoned.
pub const TOMBSTONED_PIN: &str = "tombstoned: pin yanked/below floor";
/// The head of an installed member held on its current build because its coherence group's new pin is not published for this target: `held: …`.
pub const HELD_PREFIX: &str = "held: ";
/// The head of a Trust toolchain member whose new build is staged and whose flip waits for quiet ([`crate::quiet`]): `deferred: …`.
pub const FLIP_DEFERRED_PREFIX: &str = "deferred: ";
/// The `*toolset*` row on a machine the signed index serves nothing for (an Intel Mac before x86_64 lands), as the seed and `install --default-set` record it.
pub const TOOLSET_UNSERVED: &str = "unavailable: no build for this architecture";
/// [`TOOLSET_UNSERVED`] as an update's set completion records it.
pub const TOOLSET_UNSERVED_BLOCKED: &str = "blocked: no build for this architecture";
/// The `*toolset*` row of an empty store the index does serve, emptied by `[packages].exclude` alone: an `unavailable:` fault, because the fix is one line of config (atpkg `cli.rs`, `Narrowing::verdict`).
pub const TOOLSET_EXCLUDED: &str = "unavailable: [packages].exclude leaves nothing to install";
/// The `*toolset*` row of an empty store the index does serve, where every program it serves was uninstalled on purpose: a decision, so no fault prefix — `doctor` lists nothing for it and the window lights no badge.
pub const TOOLSET_REMOVED: &str = "removed on this machine: nothing to install";
/// The `*toolset*` row of an update that installed nothing on an empty store because the signed index could not be reached — the one exit-2 verdict a retry can change, so the window's loop retries it where it lets every other exit 2 settle.
pub const TOOLSET_INDEX_UNREACHABLE: &str = "unavailable: index unreachable";
/// The `*toolset*` row of an update that installed nothing on an empty store because the process runs under Rosetta translation, so the architecture it reports is not this Mac's: nothing is asked of the index, and running natively is the fix.
pub const TOOLSET_TRANSLATED: &str = "unavailable: running under Rosetta translation";

/// Whether `state` is the machine-wide no-build-for-this-architecture verdict ([`TOOLSET_UNSERVED`] / [`TOOLSET_UNSERVED_BLOCKED`]).
#[inline]
#[must_use]
pub fn is_unserved_toolset(state: &str) -> bool {
    state == TOOLSET_UNSERVED || state == TOOLSET_UNSERVED_BLOCKED
}

/// `managed <build> — pinned by index <N>`: an index program only — a vendor-direct program's row is [`vendor_managed`] / [`vendor_kept`], because no index pins it.
#[must_use]
pub fn managed(build: u64, index_build: u64) -> String {
    let mut s = String::from(MANAGED_PREFIX);
    s.push_str(&build.to_string());
    s.push_str(" — pinned by index ");
    s.push_str(&index_build.to_string());
    s
}

/// `managed <to> — rolled back from <from>`: an index program a person rolled back.
#[must_use]
pub fn rolled_back(to: u64, from: u64) -> String {
    let mut s = String::from(MANAGED_PREFIX);
    s.push_str(&to.to_string());
    s.push_str(" — rolled back from ");
    s.push_str(&from.to_string());
    s
}

/// `managed <version> — <vendor> latest`: a vendor-direct program atpkg installed from its vendor and keeps at the vendor's head.
#[must_use]
pub fn vendor_managed(version: &str, vendor: &str) -> String {
    let mut s = String::from(MANAGED_PREFIX);
    s.push_str(version);
    s.push_str(" — ");
    s.push_str(vendor);
    s.push_str(" latest");
    s
}

/// `managed <version> — <why>`: a vendor-direct program kept on `version` while the vendor's head was not taken, saying why (a hold, a yank, a newer install).
#[must_use]
pub fn vendor_kept(version: &str, why: &str) -> String {
    let mut s = String::from(MANAGED_PREFIX);
    s.push_str(version);
    s.push_str(" — ");
    s.push_str(why);
    s
}

/// The words an ALab build's name starts with on a line ([`crate::alab::build_words`]).
pub const ALAB_BUILD_WORDS: &str = "ALab build ";

/// `managed <build> — ALab build <…>`: an ALab-built agent program's row for its active ALab store build `build`, kept or rolled back onto ([`crate::alab`]).
#[must_use]
pub fn alab_kept(build: u64, why: &str) -> String {
    let mut s = String::from(MANAGED_PREFIX);
    s.push_str(&build.to_string());
    s.push_str(" — ");
    if !why.starts_with(ALAB_BUILD_WORDS) {
        s.push_str(ALAB_BUILD_WORDS);
        s.push_str(&build.to_string());
        s.push_str("; ");
    }
    s.push_str(why);
    s
}

/// Whether `state` is an [`alab_kept`] row: a store build in the ALab window ([`crate::alab::in_store_window`]) named as ALab's.
#[must_use]
pub fn is_alab_kept(state: &str) -> bool {
    state
        .strip_prefix(MANAGED_PREFIX)
        .and_then(|rest| rest.split_once(" — "))
        .is_some_and(|(build, why)| {
            crate::store::parse_build_name(build).is_some_and(crate::alab::in_store_window)
                && why.starts_with(ALAB_BUILD_WORDS)
        })
}

/// `managed <version> — updates from <vendor>`: a vendor-direct build no lane has judged yet (no row, or a row an older client wrote) — its source, and no claim about the head.
#[must_use]
pub fn vendor_source(version: &str, vendor: &str) -> String {
    let mut why = String::from("updates from ");
    why.push_str(vendor);
    vendor_kept(version, &why)
}

/// `Some((version, why))` for a vendor-direct managed row — [`vendor_managed`], [`vendor_kept`] or [`vendor_source`]; `None` for an index row, a SHADOWED row and every other state.
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

/// `Some((version, vendor))` for exactly `managed <version> — <vendor> latest` where `vendor` is a vendor of [`crate::vendor_direct::VENDORS`]: the row that says the build is its vendor's head.
#[must_use]
pub fn vendor_latest(state: &str) -> Option<(&str, &str)> {
    let (version, why) = vendor_row(state)?;
    let vendor = why.strip_suffix(" latest")?;
    crate::vendor_direct::VENDORS
        .iter()
        .any(|s| s.vendor == vendor)
        .then_some((version, vendor))
}

/// `system: <path> — not managed by aterm`, plus ` (managed copy retired <date>)` when `retired` names the day atpkg retired its own copy in favour of this one.
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

/// `managed <build> — SHADOWED by <path>`; a vendor-direct build is named by its version (`managed 2.1.280 — SHADOWED by …`), never its store id.
#[must_use]
pub fn shadowed(build: u64, path: &Path) -> String {
    let mut s = String::from(MANAGED_PREFIX);
    s.push_str(&crate::vendor_direct::build_label(build));
    s.push_str(" — SHADOWED by ");
    s.push_str(&path.display().to_string());
    s
}

/// The head of a program built into aterm's state ([`built_in`]).
pub const BUILT_IN_PREFIX: &str = "built in: aterm ";

/// `built in: aterm <version>`: a program the one `aterm` binary serves under an argv0 alias ([`crate::builtin`]), updated with aterm and never from the store.
#[must_use]
pub fn built_in(version: &str) -> String {
    let mut s = String::from(BUILT_IN_PREFIX);
    s.push_str(version);
    s
}

/// `built in: aterm <version> — SHADOWED by <path>`: [`built_in`], with a copy earlier on `PATH` that runs instead.
#[must_use]
pub fn built_in_shadowed(version: &str, path: &Path) -> String {
    let mut s = built_in(version);
    s.push_str(" — SHADOWED by ");
    s.push_str(&path.display().to_string());
    s
}

/// The tail of an agent program's shell-local shadow row: `managed <build> — SHADOWED in this shell by <path>…` ([`agent_shadowed_in_shell`]).
pub const AGENT_SHADOWED_IN_SHELL: &str = " — SHADOWED in this shell by ";

/// `managed <build> — SHADOWED in this shell by <path>: …`, the tail [`agent_shell_shadow_tail`] writes: the agent programs' shadow row.
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

/// ``managed <build> — routed at exec time by <stub>: inside aterm it runs the managed copy
/// ahead of <foreign>, which this shell's PATH puts first — a shell that ran `<name>` before
/// the stub was laid keeps the path it cached until `rehash` (zsh) / `hash -r` (bash), once
/// (aterm help reroute)``.
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

/// `managed <build> — not what runs outside aterm: <stub> passes through to <foreign>, this shell's own copy (the managed copy leads inside aterm only; aterm help reroute)`.
#[must_use]
pub fn agent_passed_through(build: u64, stub: &Path, foreign: &Path) -> String {
    let mut s = String::from(MANAGED_PREFIX);
    s.push_str(&crate::vendor_direct::build_label(build));
    s.push_str(" — not what runs outside aterm: ");
    s.push_str(&stub.display().to_string());
    s.push_str(" passes through to ");
    s.push_str(&foreign.display().to_string());
    s.push_str(", ");
    s.push_str(OWN_COPY_OUTSIDE_ATERM);
    s
}

/// `managed <build> — not what runs outside aterm: <foreign> runs here, this shell's own copy (the managed copy leads inside aterm only; aterm help reroute)`.
#[must_use]
pub fn agent_own_copy_outside_aterm(build: u64, foreign: &Path) -> String {
    let mut s = String::from(MANAGED_PREFIX);
    s.push_str(&crate::vendor_direct::build_label(build));
    s.push_str(" — not what runs outside aterm: ");
    s.push_str(&foreign.display().to_string());
    s.push_str(" runs here, ");
    s.push_str(OWN_COPY_OUTSIDE_ATERM);
    s
}

/// The shared tail of the two outside-aterm rows.
const OWN_COPY_OUTSIDE_ATERM: &str =
    "this shell's own copy (the managed copy leads inside aterm only; aterm help reroute)";

/// The tail of [`agent_shadowed_in_shell`] after `managed <build> — `: ``SHADOWED in this
/// shell by <path>: its PATH has no <agents_dir> (a shell that has not sourced the atpkg
/// hook) — `<remedy>` here picks the managed copy up``, or with `agents_on_path` ``…: its
/// PATH has <agents_dir> behind <path's dir> — `<remedy>` here moves it to the front``,
/// then `; a tab opened on this build puts agents/ first on its own`.
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

/// `agent program — installing` ([`AGENT_INSTALLING`]): the one row a wanted agent program carries between adoption and its install.
#[must_use]
pub fn agent_installing() -> String {
    String::from(AGENT_INSTALLING)
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

/// The head of a member held after a failed stage: `held: last attempt failed with <why>; <how to retry>` ([`held_failed`]).
pub const HELD_FAILED_PREFIX: &str = "held: last attempt failed with ";

/// `held: last attempt failed with <why>; <how>`: `why` is the stage's own sentence (the `StageError` Display, digests and all) and `how` names the one retry, `aterm pkg install <p>`.
#[must_use]
pub fn held_failed(why: &str, how: &str) -> String {
    let mut s = String::from(HELD_FAILED_PREFIX);
    s.push_str(&why.replace("; ", ", "));
    s.push_str("; ");
    s.push_str(how);
    s
}

/// `deferred: build <N> is staged and installs when nothing is using the toolchain, by <time> at the latest while an aterm window or terminal session is open; staying on build <current>`: the row of a Trust toolchain member whose flip waits for quiet ([`FLIP_DEFERRED_PREFIX`], [`crate::quiet`]).
#[must_use]
pub fn flip_deferred(to: Option<u64>, current: Option<u64>, until: &str) -> String {
    let mut s = String::from(FLIP_DEFERRED_PREFIX);
    match to {
        Some(to) => {
            s.push_str("build ");
            s.push_str(&to.to_string());
            s.push_str(" is staged");
        }
        None => s.push_str("the update is staged"),
    }
    s.push_str(" and installs when nothing is using the toolchain, by ");
    s.push_str(until);
    s.push_str(" at the latest while an aterm window or terminal session is open");
    if let Some(current) = current {
        s.push_str("; staying on build ");
        s.push_str(&current.to_string());
    }
    s
}

/// `held: pinned build <N> is not published for <target>; staying on build <current>` — or `held: <owner>'s pinned build <N> …` on a sibling's row.
#[must_use]
pub fn held_unpublished(owner: Option<&str>, build: u64, target: &str, current: u64) -> String {
    let mut s = String::from(HELD_PREFIX);
    if let Some(owner) = owner {
        s.push_str(owner);
        s.push_str("'s ");
    }
    s.push_str("pinned build ");
    s.push_str(&build.to_string());
    s.push_str(" is not published for ");
    s.push_str(target);
    s.push_str("; staying on build ");
    s.push_str(&current.to_string());
    s
}

/// The `<path>` of a `system: <path> — not managed by aterm…` state, or `None` for any other state.
#[inline]
#[must_use]
pub fn system_path(state: &str) -> Option<&str> {
    let rest = state.strip_prefix(SYSTEM_PREFIX)?;
    let end = rest.find(SYSTEM_TAIL)?;
    Some(&rest[..end])
}

/// The `<date>` of a system state's ` (managed copy retired <date>)` note, if present.
#[inline]
#[must_use]
pub fn system_retired(state: &str) -> Option<&str> {
    let rest = state.strip_prefix(SYSTEM_PREFIX)?;
    let note = rest.rsplit_once(" (managed copy retired ")?.1;
    note.strip_suffix(')')
}

/// `Some((build, index))` for a `managed <build> — pinned by index <N>` state; `None` for a SHADOWED managed row and for every other state.
#[inline]
#[must_use]
pub fn managed_pin(state: &str) -> Option<(u64, u64)> {
    let rest = state.strip_prefix(MANAGED_PREFIX)?;
    let (build, tail) = rest.split_once(" — pinned by index ")?;
    Some((
        crate::store::parse_build_name(build)?,
        crate::store::parse_build_name(tail)?,
    ))
}

/// `Some((build, path))` for a `managed <build> — SHADOWED by <path>` state; a version in the build's place (a vendor-direct build, [`shadowed`]) reads back as its store id.
#[must_use]
pub fn shadowed_by(state: &str) -> Option<(u64, &str)> {
    let rest = state.strip_prefix(MANAGED_PREFIX)?;
    let (build, path) = rest.split_once(" — SHADOWED by ")?;
    let build = match crate::store::parse_build_name(build) {
        Some(b) => b,
        None => crate::vendor_direct::Version::parse(build)?.build_id(),
    };
    Some((build, path))
}

/// Whether `state` is a managed row of any spelling (pinned, vendor or SHADOWED).
#[inline]
#[must_use]
pub fn is_managed(state: &str) -> bool {
    state.starts_with(MANAGED_PREFIX)
}

/// The fix-line after a SHADOWED row on the surfaces that speak to a person (the pass log, `doctor`, `which`): `type alab-<tool> for the managed one`.
#[must_use]
pub fn alias_hint(alias: &str) -> String {
    let mut s = String::from("type ");
    s.push_str(alias);
    s.push_str(" for the managed one");
    s
}

/// Where `program`'s builds come from, as the package log and Settings ▸ Packages name it: `ALab build (index <N>)` / `ALab build` for an ALab build of an ALab-built agent program (pinned by the index, or kept: [`crate::alab`]), `Anthropic latest` / `OpenAI latest` for a vendor-direct program (its vendor's channel, whatever else its row says) and `ALab index <N>` for everything else, `N` being the index its row is pinned by, else the one the last pass resolved (`last_index_build`), else none.
#[must_use]
pub fn source_words(program: &str, state: &str, last_index_build: u64) -> String {
    // An ALab-built agent program's row from the index lane, or the one its kept ALab build
    // carries ([`crate::alab`]): ALab's build, never its vendor's latest.
    if crate::alab::agent(program).is_some() {
        if let Some((_, index)) =
            managed_pin(state).filter(|(build, _)| crate::alab::in_store_window(*build))
        {
            let mut s = String::from("ALab build (index ");
            s.push_str(&index.to_string());
            s.push(')');
            return s;
        }
        if is_alab_kept(state) {
            return String::from("ALab build");
        }
    }
    if let Some(spec) = crate::vendor_direct::spec(program) {
        let mut s = String::from(spec.vendor);
        s.push_str(" latest");
        return s;
    }
    let index = managed_pin(state).map_or(last_index_build, |(_, index)| index);
    let mut s = String::from("ALab index");
    if index > 0 {
        s.push(' ');
        s.push_str(&index.to_string());
    }
    s
}

/// Why a row is not current, when it says so: a one-word `result` (what the package log and the Activity list call it) and the `reason` in the row's own words.
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
    } else {
        return None;
    };
    Some((result, state))
}

/// Whether an `error:` row's reason is a refusal (atpkg declining bytes or a document it
/// could not trust) as its writers spell one at the head of the reason: the vendor lane's
/// `<program> [<version>] refused: <why>` (recorded by `record_vendor_status`), the flow's
/// `manifest refused: `, `artifact row refused: ` and `the signed row for <program> was
/// refused: `, and a stage's `stage: signer refused: `.
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
#[path = "state_tests.rs"]
mod tests;
