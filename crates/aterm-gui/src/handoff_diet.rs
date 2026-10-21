// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The outgoing process's artifact diet (`docs/DESIGN-warm-successor-2026-09-29.md` §5):
//! write before the park what does not change between the launch and the park.
//!
//! On the launched lane the park-to-transfer interval is all artifacts, and most of it is one
//! file, the attempt-bound layout sidecar (`seamless-<pid>-<nonce>.layout.toml`): the restore
//! writer publishes it under a lock with a temporary sibling, syncs, renames and syncs the
//! directory, and on macOS each sync is an `F_FULLFSYNC` (about 4 ms).
//!
//! Static between the launch and the park:
//!
//! * The layout sidecar (`App::capture_handoff_layout`: frames, tabs, splits, each leaf's
//!   folder and title, Settings drafts) moves only when the person changes the desk. It is
//!   written before the park by [`speculate_layout`]; at the park [`seal_layout`] reads it
//!   back with the successor's reader and keeps it only when it is byte-equal to the park's
//!   bytes, else writes it again.
//! * The two private pipes (the proof and Commit channels) hold no state, so the launched lane
//!   makes them before the park too (`app_update_handoff`).
//!
//! Not static, and written after the park: the window facts (`WindowCarry`, a field of the
//! manifest); the control carry's sidecars (exported from the park's own fence, unsynced);
//! and the manifest, screen sidecars and adoption proof, the digested state, whose one writer
//! is `seamless::write_outgoing_with_held`.
//!
//! The contract, `NativeUpdateSpeculativeSidecar` (aterm-spec), is a writer/reader pair: this
//! module writes, and the successor reads the sidecar after its grant and hashes those bytes
//! into the proof it must match. The grant waits for [`seal_layout`]. Every failure path
//! retires the attempt's files by their prefix (`seamless::discard_outgoing`), which the
//! speculative sidecar carries. The Tier-1 is [`conformance`].

use std::path::{Path, PathBuf};
use std::time::Duration;

/// A layout sidecar written before the park.
///
/// Only the launched lane (macOS) builds one. Elsewhere on unix [`seal_layout`] still takes
/// and reads it, so rustc does not count it dead; an `expect(dead_code)` here goes unfulfilled
/// on aarch64-linux.
#[derive(Debug)]
pub(crate) struct SpeculativeLayout {
    /// Where it was written: the attempt's layout sidecar.
    path: PathBuf,
    /// The exact bytes written.
    wire: String,
    /// How long the write took: the time a kept sidecar saves the freeze.
    took: Duration,
}

/// Write the attempt's layout sidecar before the park: `layout` (captured with every reader
/// live), serialized, at `path` (the attempt's `seamless-<pid>-<nonce>.layout.toml`), through
/// the writer the park would use (`restore::write_wire_once_to`: lock, sync, rename,
/// directory sync).
///
/// A layout that does not survive its own TOML round trip is not written (the park refuses it
/// anyway). A failed write leaves nothing at `path`, and the park writes the sidecar itself.
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
        // The rename may have landed before a failed directory sync: a
        // refused speculation leaves nothing at the name.
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
    /// The park's layout is not the one the speculation wrote: the desk moved
    /// while the successor booted.
    LayoutMoved,
    /// The file no longer reads back as what was written (replaced,
    /// truncated, removed or unreadable).
    FileDisturbed,
    /// The speculation was written under another name than this attempt's sidecar (the lane
    /// derives both from one nonce; checked rather than assumed).
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
    /// The sidecar written before the park read back byte-equal to the park's
    /// layout, and was kept. `saved` is what writing it cost before the park.
    Kept { saved: Duration },
    /// It did not, so it was discarded and written again from the park's
    /// layout.
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

/// Seal the attempt's layout sidecar at the park: after `Ok`, `path` holds exactly `wire`,
/// the park's own serialized layout.
///
/// A `speculative` sidecar is kept only when the bytes it wrote are `wire` and the file at
/// `path` still reads back as `wire`, through the successor's own reader, so "kept" means the
/// successor reads the park's bytes, not that this process remembers writing them
/// (`SealHoldsTheParksBytes`). Otherwise the file is discarded and written again, and the
/// result says why. With no speculation the sidecar is written here.
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
        // Capped at the park's own length: a longer file is not equal.
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
    // Discard, then rewrite. The discard is explicit rather than left to the rename, so a
    // rewrite that fails leaves no stale sidecar at the name either.
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
