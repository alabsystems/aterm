// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

//! Stale copies of aterm (macOS): bundles that claim aterm's identifier, signed ad hoc to
//! their own cdhash, left beside the installed release (`aterm-prev-<epoch>.app` and the
//! like, made by hand or by an agent: the updater, `tools/install.sh` and `ship dev-app`
//! leave none). Each one resets aterm's privacy grants whenever it asks macOS for one
//! (`aterm_containment::consent`, "The claimant census").
//!
//! `aterm doctor` prunes them ([`prune`]): exactly the copies the census offers for the
//! Trash ([`aterm_containment::consent::Claimants::retirable`]: ad hoc to their cdhash or
//! unsigned, no link leading to them, beside a running release whose grant survives
//! updates; never a Developer ID copy), re-checked right before the move and never while a
//! process runs from one, moved with Apple's own tool so each stays recoverable. The
//! Settings ▸ Security panel's *Move to Trash* uses the same move ([`trash`]).

use std::io::Read as _;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use aterm_containment::consent::{self, Claimants, Retired};

use crate::doctor::{Level, Row};

/// The trash tool macOS 15 and later ship.
pub const TRASH_TOOL: &str = "/usr/bin/trash";

/// How long one move may take. Within a volume it is a rename; across volumes it is a
/// copy.
const TRASH_CEILING: Duration = Duration::from_secs(120);

/// Move `path` to the Trash with [`TRASH_TOOL`], bounded by two minutes. The error is the
/// system's own reason when it gave one.
///
/// # Errors
/// A relative path, a tool that could not run or did not finish, or the tool's refusal.
pub fn trash(path: &Path) -> Result<(), String> {
    use aterm_uds::spawnfd::Stdio;
    // Census paths are absolute, so the argument can never read as an option.
    if !path.is_absolute() {
        return Err("not an absolute path".to_string());
    }
    let mut child = aterm_uds::spawnfd::command(TRASH_TOOL)
        .arg(path)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::piped())
        .spawn()
        .map_err(|e| format!("{TRASH_TOOL} could not run: {e}"))?;
    let deadline = Instant::now() + TRASH_CEILING;
    let status = loop {
        match child.try_wait() {
            Ok(Some(status)) => break status,
            Ok(None) if Instant::now() < deadline => std::thread::sleep(Duration::from_millis(50)),
            Ok(None) | Err(_) => {
                let _ = child.kill();
                let _ = child.wait();
                return Err(format!(
                    "the move did not finish within {} s",
                    TRASH_CEILING.as_secs()
                ));
            }
        }
    };
    if status.success() {
        return Ok(());
    }
    let mut stderr = String::new();
    if let Some(pipe) = child.stderr.take() {
        let _ = pipe.take(8 * 1024).read_to_string(&mut stderr);
    }
    Err(
        trash_refusal(&stderr).unwrap_or_else(|| match status.code() {
            Some(code) => format!("{TRASH_TOOL} exited {code}"),
            None => format!("{TRASH_TOOL} was ended by a signal"),
        }),
    )
}

/// The system's reason from the tool's stderr: the first quoted description after
/// `Code=<n>`, e.g. `The file “x.app” doesn’t exist.`
fn trash_refusal(stderr: &str) -> Option<String> {
    let after = &stderr[stderr.find("Code=")?..];
    let open = after.find('"')? + 1;
    let len = after[open..].find('"')?;
    let reason = after[open..open + len].trim();
    (!reason.is_empty()).then(|| reason.to_owned())
}

/// What one prune found and did.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Pruned {
    /// Not looked: why (a test binary, a build outside an app bundle, no bundle id, not
    /// macOS, a read-only run).
    NotLooked(String),
    /// The census could not finish: copies may exist that it did not see.
    Incomplete {
        /// What the moves that did happen did.
        moved: Vec<(PathBuf, Retired)>,
    },
    /// A read-only look (`aterm doctor --read-only`): what a prune would move, and what
    /// it would keep, nothing moved.
    Found {
        /// The copies a prune would move to the Trash.
        offered: Vec<PathBuf>,
        /// Copies that conflict but are kept, with why.
        kept: Vec<(PathBuf, &'static str)>,
        /// Whether the census read every place a copy can be.
        complete: bool,
    },
    /// The census finished; each retirable copy and what became of it (empty: none).
    Done {
        /// Each copy offered, with its outcome.
        moved: Vec<(PathBuf, Retired)>,
        /// Copies that conflict but are kept, with why ([`consent::TrashOffer`]'s token).
        kept: Vec<(PathBuf, &'static str)>,
    },
}

/// What a prune of `bundle_id` would move and keep, moving nothing: the census alone.
pub fn look_with(bundle_id: &str, census: impl Fn(&str) -> Claimants, trash_tool: bool) -> Pruned {
    let found = census(bundle_id);
    Pruned::Found {
        offered: found
            .offered_for_trash(trash_tool)
            .into_iter()
            .map(|copy| copy.path.clone())
            .collect(),
        kept: kept_of(&found, trash_tool),
        complete: found.enumeration == consent::Enumeration::Complete,
    }
}

/// The conflicting copies the census keeps, with why ([`consent::TrashOffer`]'s token).
fn kept_of(found: &Claimants, trash_tool: bool) -> Vec<(PathBuf, &'static str)> {
    found
        .listed(trash_tool)
        .into_iter()
        .filter(|(_, offer)| *offer != consent::TrashOffer::Offered)
        .map(|(copy, offer)| (copy.path.clone(), offer.as_str()))
        .collect()
}

/// The census's moves for `bundle_id`, with every reach injected: the census, the fresh
/// identity check, whether a process runs from a copy, and the move. Pure apart from them.
pub fn prune_with(
    bundle_id: &str,
    census: impl Fn(&str) -> Claimants,
    unchanged: impl Fn(&consent::Claimant, &str) -> bool,
    in_use: impl Fn(&Path) -> Option<bool>,
    move_to_trash: impl Fn(&Path) -> Result<(), String>,
    trash_tool: bool,
) -> Pruned {
    let found = census(bundle_id);
    let plan: Vec<PathBuf> = found
        .offered_for_trash(trash_tool)
        .into_iter()
        .map(|copy| copy.path.clone())
        .collect();
    let kept = kept_of(&found, trash_tool);
    // A fresh census right before the moves, as the panel's worker takes: only a copy
    // both looks offered and is still the same bundle moves.
    let moved = if plan.is_empty() {
        Vec::new()
    } else {
        let fresh = census(bundle_id);
        consent::retire_claimants(
            &plan,
            &fresh,
            |copy| unchanged(copy, bundle_id),
            in_use,
            move_to_trash,
        )
    };
    if found.enumeration == consent::Enumeration::Complete {
        Pruned::Done { moved, kept }
    } else {
        Pruned::Incomplete { moved }
    }
}

/// Prune the stale copies of the running aterm ([`prune_with`] with the live census, the
/// fresh identity check, the process table and [`trash`]); with `dry_run`, only look
/// ([`look_with`]). Refuses, reading nothing, outside an app bundle (a test binary among
/// them) and off macOS.
#[must_use]
pub fn prune(dry_run: bool) -> Pruned {
    if !cfg!(target_os = "macos") {
        return Pruned::NotLooked("macOS only".to_string());
    }
    let Some(root) = consent::fence_exe()
        .as_deref()
        .and_then(consent::app_bundle_root)
    else {
        return Pruned::NotLooked("running outside the app bundle".to_string());
    };
    let Some(id) = consent::bundle_id_of(&root, &consent::protected_roots(&[])) else {
        return Pruned::NotLooked("this build has no readable bundle identifier".to_string());
    };
    let tool = Path::new(TRASH_TOOL).is_file();
    if dry_run {
        return look_with(&id, consent::claimants_for, tool);
    }
    prune_with(
        &id,
        consent::claimants_for,
        consent::still_claims,
        crate::gc::runs_from,
        trash,
        tool,
    )
}

/// The `copies:` row, pure over what the prune did: `ok` when nothing is left over, a
/// `note` naming each copy moved to the Trash (recoverable) or kept and why, and a
/// `warn` for a copy that should have moved and did not.
#[must_use]
pub fn row(pruned: &Pruned) -> Row {
    let name = |path: &Path| {
        path.file_name().map_or_else(
            || path.display().to_string(),
            |n| n.to_string_lossy().into_owned(),
        )
    };
    let mut lines: Vec<String> = Vec::new();
    let mut stuck = false;
    let (moved, kept, complete) = match pruned {
        Pruned::NotLooked(why) => {
            return Row {
                level: Level::Note,
                text: format!("not checked ({why})"),
            };
        }
        Pruned::Found {
            offered,
            kept,
            complete,
        } => {
            let mut lines: Vec<String> = Vec::new();
            if !offered.is_empty() {
                lines.push(format!(
                    "{} stale cop{} of aterm (`aterm doctor` moves {} to the Trash): {}",
                    offered.len(),
                    if offered.len() == 1 { "y" } else { "ies" },
                    if offered.len() == 1 { "it" } else { "them" },
                    offered
                        .iter()
                        .map(|p| p.display().to_string())
                        .collect::<Vec<_>>()
                        .join(", ")
                ));
            }
            lines.extend(
                kept.iter()
                    .map(|(path, offer)| format!("{} stays (trash={offer})", path.display())),
            );
            if !complete {
                lines.push("the census could not read every place a copy can be".to_string());
            }
            return Row {
                level: if !offered.is_empty() {
                    Level::Warn
                } else if lines.is_empty() {
                    Level::Ok
                } else {
                    Level::Note
                },
                text: if lines.is_empty() {
                    "no other copy of aterm claims its identity".to_string()
                } else {
                    lines.join("; ")
                },
            };
        }
        Pruned::Incomplete { moved } => (moved.as_slice(), &[][..], false),
        Pruned::Done { moved, kept } => (moved.as_slice(), kept.as_slice(), true),
    };
    let gone: Vec<String> = moved
        .iter()
        .filter(|(_, r)| *r == Retired::Moved)
        .map(|(p, _)| name(p))
        .collect();
    if !gone.is_empty() {
        lines.push(format!(
            "moved {} stale cop{} of aterm to the Trash (recoverable there): {}",
            gone.len(),
            if gone.len() == 1 { "y" } else { "ies" },
            gone.join(", ")
        ));
    }
    for (path, outcome) in moved {
        let why = match outcome {
            Retired::Moved | Retired::NotOffered => continue,
            Retired::InUse => "a process runs from it".to_string(),
            Retired::Failed(reason) => reason.clone(),
        };
        stuck = true;
        lines.push(format!("{} stays: {why}", path.display()));
    }
    for (path, offer) in kept {
        lines.push(format!("{} stays (trash={offer})", path.display()));
    }
    if !complete {
        lines.push("the census could not read every place a copy can be".to_string());
    }
    let level = if stuck {
        Level::Warn
    } else if lines.is_empty() {
        Level::Ok
    } else {
        Level::Note
    };
    let text = if lines.is_empty() {
        "no other copy of aterm claims its identity".to_string()
    } else {
        lines.join("; ")
    };
    Row { level, text }
}

#[cfg(test)]
mod tests {
    use super::*;
    use aterm_containment::consent::{Claimant, DrClass, Enumeration};

    const DEV_ID: &str = "identifier \"com.aterm.aterm\" and anchor apple generic and \
                          certificate leaf[subject.OU] = \"A66A9P66Z7\"";

    fn copy(path: &str, adhoc: bool, running: bool) -> Claimant {
        Claimant {
            path: PathBuf::from(path),
            dr: if adhoc {
                DrClass::Cdhash
            } else {
                DrClass::Identity
            },
            dr_text: if adhoc {
                format!("cdhash H\"{:040}\"", path.len())
            } else {
                DEV_ID.to_string()
            },
            signing: if adhoc { "adhoc" } else { "developer-id" },
            team: (!adhoc).then(|| "A66A9P66Z7".to_string()),
            running,
            links: Vec::new(),
        }
    }

    fn census_of(found: Vec<Claimant>, enumeration: Enumeration) -> Claimants {
        Claimants { found, enumeration }
    }

    /// The release running, ad hoc copies beside it, and a Developer ID rollback that is
    /// never moved.
    fn owners_mac() -> Claimants {
        census_of(
            vec![
                copy("/Applications/aterm.app", false, true),
                copy("/Applications/aterm-prev-1788285383.app", true, false),
                copy("/Applications/aterm-local-1789361388.app", true, false),
                Claimant {
                    dr_text: "identifier \"com.aterm.aterm\" and anchor apple generic".into(),
                    ..copy("/Applications/aterm.app.rollback", false, false)
                },
            ],
            Enumeration::Complete,
        )
    }

    #[test]
    fn the_prune_moves_only_the_census_offered_copies_and_names_the_rest() {
        let moved = std::cell::RefCell::new(Vec::new());
        let got = prune_with(
            "com.aterm.aterm",
            |_| owners_mac(),
            |_, _| true,
            |path| Some(path.ends_with("aterm-local-1789361388.app")),
            |path| {
                moved.borrow_mut().push(path.to_path_buf());
                Ok(())
            },
            true,
        );
        assert_eq!(
            moved.into_inner(),
            vec![PathBuf::from("/Applications/aterm-prev-1788285383.app")],
            "the copy in use stays; the Developer ID rollback is never moved"
        );
        let row = row(&got);
        assert_eq!(
            row.level,
            Level::Warn,
            "a copy in use that stays is a warning: {row:?}"
        );
    }

    #[test]
    fn nothing_moves_without_a_trash_tool_or_beside_an_unstable_running_copy() {
        let never = |_: &Path| -> Result<(), String> { panic!("nothing may move") };
        let got = prune_with(
            "x",
            |_| owners_mac(),
            |_, _| true,
            |_| Some(false),
            never,
            false,
        );
        assert!(
            matches!(&got, Pruned::Done { moved, .. } if moved.is_empty()),
            "{got:?}"
        );
        let unstable = || {
            census_of(
                vec![
                    copy("/Applications/aterm-local-1.app", true, true),
                    copy("/Applications/aterm-prev-2.app", true, false),
                ],
                Enumeration::Complete,
            )
        };
        let got = prune_with(
            "x",
            |_| unstable(),
            |_, _| true,
            |_| Some(false),
            never,
            true,
        );
        assert!(
            matches!(&got, Pruned::Done { moved, kept } if moved.is_empty()
                && kept == &[(PathBuf::from("/Applications/aterm-prev-2.app"), "running-unstable")]),
            "{got:?}"
        );
    }

    #[test]
    fn the_row_is_ok_alone_and_says_what_it_could_not_check() {
        let alone = prune_with(
            "x",
            |_| {
                census_of(
                    vec![copy("/Applications/aterm.app", false, true)],
                    Enumeration::Complete,
                )
            },
            |_, _| true,
            |_| Some(false),
            |_| Ok(()),
            true,
        );
        assert_eq!(row(&alone).level, Level::Ok);
        let partial = Pruned::Incomplete { moved: Vec::new() };
        assert_eq!(
            row(&partial).level,
            Level::Note,
            "an incomplete census is never ok"
        );
    }

    #[test]
    fn a_test_binary_prunes_nothing() {
        for dry_run in [false, true] {
            let got = prune(dry_run);
            assert!(matches!(got, Pruned::NotLooked(_)), "{got:?}");
        }
    }

    /// A read-only look names what a prune would move as a `warn`, and moves nothing.
    #[test]
    fn a_look_names_what_a_prune_would_move() {
        let got = look_with("com.aterm.aterm", |_| owners_mac(), true);
        assert_eq!(
            got,
            Pruned::Found {
                offered: vec![
                    PathBuf::from("/Applications/aterm-prev-1788285383.app"),
                    PathBuf::from("/Applications/aterm-local-1789361388.app"),
                ],
                kept: vec![(
                    PathBuf::from("/Applications/aterm.app.rollback"),
                    "signature"
                )],
                complete: true,
            }
        );
        assert_eq!(row(&got).level, Level::Warn);
    }

    #[test]
    fn the_refusal_is_the_systems_own_words() {
        let stderr = "2026-09-23 22:29:30.491 trash[79297:53920104] # Error attempting to move \
                      /x/Nope.app to the trash folder, Error Domain=NSCocoaErrorDomain Code=4 \
                      \"The file “Nope.app” doesn’t exist.\" UserInfo={NSURL=file:///x/Nope.app}";
        assert_eq!(
            trash_refusal(stderr).as_deref(),
            Some("The file “Nope.app” doesn’t exist.")
        );
        assert_eq!(trash_refusal("no code here"), None);
        assert_eq!(trash_refusal("Code=4 \"\""), None);
    }
}
