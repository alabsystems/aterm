// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

//! The `requires` relation's ONE gate — [`unmet_requirement`] (§17.10) — shared by every
//! lane that would start a program: the set-completion pass and the OS-installed
//! reconcile (`cli.rs`), and the update pass (`flow::apply_group_txn`, so `apply_channel`
//! and `apply_program` alike). One rule, several callers: a dependent is gated the same
//! way whether it is being installed for the first time or moved to a newer pin, so a
//! dependency the user uninstalled holds its dependents' UPDATES exactly as it holds
//! their installs — the warning `uninstall` prints ("it will read `blocked by <dep>: …`
//! until it is back") is true on the six-hourly tick, not only at the next fresh install.
//!
//! Reads the store and the record under the given [`Layout`] only — never the network,
//! never the invoking user's real `~/.aterm` — so `flow.rs`'s hermetic tests hold.

use crate::Layout;
use crate::manifest::Index;

/// The first of `requires` (in order) NOT met on this machine, with the DEPENDENCY's own
/// canonical state — the two halves of a `blocked by <dep>: <dep state>` row
/// ([`crate::state::blocked`]). A requirement is met when the dependency is installed (an
/// active build), dev-linked, satisfied by a system copy whose recorded path still
/// exists, or installed through its protocol at a `provides` path that still exists.
/// Reads the store and the record only — never the network — and re-reads the active
/// builds, since the pass installs as it goes. The dependency's state is its recorded
/// row (`needs admin — …`, `unavailable on …`, `error: …`); with no usable row, an extra
/// not opted in reads `extra — not installed (opt in: …)` and anything else `not
/// installed`. `None` ⇒ every requirement is met (or there are none).
#[must_use]
pub fn unmet_requirement(
    layout: &Layout,
    index: &Index,
    requires: &[String],
) -> Option<(String, String)> {
    if requires.is_empty() {
        return None;
    }
    let active = crate::active_builds(layout);
    let status = crate::status::read(layout);
    let is_file = |p: &str| std::fs::metadata(p).is_ok_and(|m| m.is_file());
    for dep in requires {
        if active.contains_key(dep) || crate::linkmode::is_linked(layout, dep) {
            continue;
        }
        let row = status
            .as_ref()
            .and_then(|s| s.programs.get(dep))
            .map(|r| r.state.clone())
            .unwrap_or_default();
        if crate::state::system_path(&row).is_some_and(is_file) {
            continue;
        }
        if crate::state::installed_via_path(&row).is_some_and(|(_, p)| is_file(p)) {
            continue;
        }
        // A row that CLAIMS presence but is not backed by the store or the path any
        // more says nothing about the dependency's state now.
        let stale = row.is_empty()
            || crate::state::is_managed(&row)
            || crate::state::system_path(&row).is_some()
            || crate::state::installed_via_path(&row).is_some();
        let dep_state = if !stale {
            row
        } else if index.is_extra(dep) && !layout.optin_exists(dep) {
            crate::state::extra_not_installed(dep)
        } else {
            String::from("not installed")
        };
        return Some((dep.clone(), dep_state));
    }
    None
}
