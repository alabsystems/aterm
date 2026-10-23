// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Speculative pre-park writing and park-time sealing of the handoff layout sidecar.

use std::path::{Path, PathBuf};
use std::time::Duration;

/// A layout sidecar written before the park.
#[derive(Debug)]
pub(crate) struct SpeculativeLayout {
    /// Where it was written: the attempt's layout sidecar.
    path: PathBuf,
    /// The exact bytes written.
    wire: String,
    /// How long the write took: the time a kept sidecar saves the freeze.
    took: Duration,
}

/// Serializes and writes the attempt's layout sidecar at `path` before the park.
#[cfg(any(target_os = "macos", test))]
#[cfg_attr(
    any(test, debug_assertions),
    aterm_spec_anchor::refines(
        machine = "native_update_speculative_sidecar",
        action = "Speculate",
        project = "aterm_gui::handoff_diet::conformance::project"
    )
)]
pub(crate) fn speculate_layout(
    path: &Path,
    layout: &crate::restore::RestoreManifest,
) -> Result<SpeculativeLayout, String> {
    let started = std::time::Instant::now();
    let wire = layout.to_toml()?;
    if crate::restore::RestoreManifest::from_toml(&wire).as_ref() != Some(layout) {
        return Err("the layout does not survive its own TOML round trip".to_string());
    }
    if let Err(error) = crate::restore::write_wire_once_to(path, &wire) {
        let _ = std::fs::remove_file(path);
        return Err(error);
    }
    Ok(SpeculativeLayout {
        path: path.to_path_buf(),
        wire,
        took: started.elapsed(),
    })
}

/// Why a speculative sidecar was not kept.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Mismatch {
    /// The park's layout differs from the speculative layout.
    LayoutMoved,
    /// The speculative sidecar file no longer reads back as written.
    FileDisturbed,
    /// The speculative sidecar was written at a different path.
    OtherName,
}

impl std::fmt::Display for Mismatch {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str(match self {
            Self::LayoutMoved => "the layout changed between the launch and the park",
            Self::FileDisturbed => "the file no longer read back as it was written",
            Self::OtherName => "it was written under another name than this attempt's sidecar",
        })
    }
}

/// How the park sealed the layout sidecar.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum LayoutSeal {
    /// The speculative sidecar matched the park's layout and was kept.
    Kept { saved: Duration },
    /// The speculative sidecar mismatched and was rewritten at the park.
    Rewritten(Mismatch),
    /// Nothing was written before the park, so the park wrote it.
    WrittenAtPark,
}

impl std::fmt::Display for LayoutSeal {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Kept { saved } => write!(
                formatter,
                "kept the sidecar written before the park: it read back byte-equal to the \
                 park's layout ({:.1} ms of writing moved out of the freeze)",
                saved.as_secs_f64() * 1000.0
            ),
            Self::Rewritten(why) => write!(
                formatter,
                "discarded the sidecar written before the park and wrote it again: {why}"
            ),
            Self::WrittenAtPark => {
                formatter.write_str("wrote the sidecar at the park: none was written before it")
            }
        }
    }
}

/// Seals the attempt's layout sidecar at `path` with the park's serialized `wire`.
#[cfg(unix)]
#[cfg_attr(
    any(test, debug_assertions),
    aterm_spec_anchor::refines(
        machine = "native_update_speculative_sidecar",
        action = "KeepVerified",
        project = "aterm_gui::handoff_diet::conformance::project"
    )
)]
#[cfg_attr(
    any(test, debug_assertions),
    aterm_spec_anchor::refines(
        machine = "native_update_speculative_sidecar",
        action = "DiscardAndRewrite",
        project = "aterm_gui::handoff_diet::conformance::project"
    )
)]
#[cfg_attr(
    any(test, debug_assertions),
    aterm_spec_anchor::refines(
        machine = "native_update_speculative_sidecar",
        action = "WriteAtPark",
        project = "aterm_gui::handoff_diet::conformance::project"
    )
)]
pub(crate) fn seal_layout(
    path: &Path,
    wire: &str,
    speculative: Option<SpeculativeLayout>,
) -> Result<LayoutSeal, String> {
    let Some(speculative) = speculative else {
        crate::restore::write_wire_once_to(path, wire)?;
        return Ok(LayoutSeal::WrittenAtPark);
    };
    let why = if speculative.path != path {
        Mismatch::OtherName
    } else if speculative.wire != wire {
        Mismatch::LayoutMoved
    } else {
        let cap = u64::try_from(wire.len()).unwrap_or(u64::MAX);
        match crate::seamless::read_regular_capped(path, cap) {
            Some(bytes) if bytes == wire.as_bytes() => {
                return Ok(LayoutSeal::Kept {
                    saved: speculative.took,
                });
            }
            _ => Mismatch::FileDisturbed,
        }
    };
    for stale in [speculative.path.as_path(), path] {
        match std::fs::remove_file(stale) {
            Ok(()) => {}
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
            Err(error) => {
                return Err(format!(
                    "could not discard the layout sidecar written before the park ({error})"
                ));
            }
        }
    }
    crate::restore::write_wire_once_to(path, wire)?;
    Ok(LayoutSeal::Rewritten(why))
}

#[cfg(all(test, unix))]
#[path = "handoff_diet_conformance.rs"]
pub(crate) mod conformance;

/// The model's actions no shipping function here performs, each waived with where it is
/// bound instead.
#[cfg(any(test, debug_assertions))]
#[aterm_spec_anchor::spec_unmodeled(
    machine = "native_update_speculative_sidecar",
    action = "RefuseWrite",
    reason = "The environment: a file system that refuses the speculative write. The \
              conformance realizes it as a directory the writer may not create in."
)]
#[aterm_spec_anchor::spec_unmodeled(
    machine = "native_update_speculative_sidecar",
    action = "LayoutMoves",
    reason = "The environment: the person changes the desk while the successor boots. The \
              conformance realizes it as a second park layout (the negative control)."
)]
#[aterm_spec_anchor::spec_unmodeled(
    machine = "native_update_speculative_sidecar",
    action = "DiskDisturbed",
    reason = "The environment: something else rewrites the sidecar in the private control \
              directory. The conformance realizes it as an in-place overwrite."
)]
#[aterm_spec_anchor::spec_unmodeled(
    machine = "native_update_speculative_sidecar",
    action = "Park",
    reason = "The main thread's park (`App::park_and_transfer_to_prelaunched_successor`), \
              bound by NativeUpdateSeamlessHandoffOwnership's Tier-1; P5 does not change it."
)]
#[aterm_spec_anchor::spec_unmodeled(
    machine = "native_update_speculative_sidecar",
    action = "Grant",
    reason = "The rendezvous transfer in `run_prelaunched_handoff`, which runs only after \
              `prepare_outgoing_artifacts` (and so `seal_layout`) returned: the order is \
              that function's straight line, and the rendezvous suites drive the transfer."
)]
#[aterm_spec_anchor::spec_unmodeled(
    machine = "native_update_speculative_sidecar",
    action = "SuccessorReads",
    reason = "The successor's `take_incoming_from` layout read. The conformance drives its \
              halves, `seamless::take_regular_capped` and `layout_wire_digest`, against \
              the park's committed `layout_digest`."
)]
#[aterm_spec_anchor::spec_unmodeled(
    machine = "native_update_speculative_sidecar",
    action = "SealUnverified",
    reason = "The Buggy=1 mutant: a speculation kept without the read-back; no shipping \
              code, replayed on the real files by the negative control."
)]
#[aterm_spec_anchor::spec_unmodeled(
    machine = "native_update_speculative_sidecar",
    action = "VerifyTheMemoryCopy",
    reason = "The Buggy=1 mutant: the bytes meant to be written compared instead of the \
              file; no shipping code, replayed on the real files by the negative control."
)]
#[aterm_spec_anchor::spec_unmodeled(
    machine = "native_update_speculative_sidecar",
    action = "GrantBeforeSeal",
    reason = "The Buggy=1 mutant: a grant with the speculation undecided; no shipping code, \
              replayed on the real files by the negative control."
)]
#[aterm_spec_anchor::spec_unmodeled(
    machine = "native_update_speculative_sidecar",
    action = "RetireKeepsSpeculation",
    reason = "The Buggy=1 mutant: a retire without the prefix sweep; no shipping code, \
              replayed on the real files by the negative control."
)]
#[aterm_spec_anchor::spec_unmodeled(
    machine = "native_update_speculative_sidecar",
    action = "AwaitSpeculation",
    reason = "The Buggy=1 liveness mutant: a park that waits on a refused speculation; no \
              shipping code (`seal_layout` writes at the park when there is none)."
)]
#[allow(
    dead_code,
    reason = "carrier for the `spec_unmodeled` waivers above; nothing calls it. An allow, \
              not an expect: rustc does not lint a fn the declarative anchors rebuild"
)]
fn speculative_sidecar_waivers() {}

/// The speculation is the launched lane's, macOS's alone: elsewhere on unix
/// [`speculate_layout`] is built only for tests, and its anchor with it, while
/// [`seal_layout`] binds the machine's other actions. Without this a Linux build's
/// registry left `Speculate` neither bound nor waived (aterm-models'
/// `spec_xref_closure`, obligation 3, in the Linux lane on 2026-10-20).
#[cfg(all(debug_assertions, not(test), unix, not(target_os = "macos")))]
#[aterm_spec_anchor::spec_unmodeled(
    machine = "native_update_speculative_sidecar",
    action = "Speculate",
    reason = "The speculative write is the launched lane's, macOS's alone; here no code \
              writes the sidecar before the park, and `seal_layout` writes it at the park."
)]
#[allow(
    dead_code,
    reason = "carrier for the `spec_unmodeled` waiver above; nothing calls it. An allow, \
              not an expect: rustc does not lint a fn the declarative anchors rebuild"
)]
fn speculate_is_macos_only() {}
