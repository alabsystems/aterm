// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! THE OUTGOING PROCESS'S ARTIFACT DIET (warm successor P5, 2026-09-29;
//! `docs/DESIGN-warm-successor-2026-09-29.md` §5).
//!
//! # What the freeze paid for
//!
//! On the launched lane the park->transfer interval is all artifacts (P0
//! stamps: 12 ms of 12). Most of that is ONE file, the attempt-bound layout
//! sidecar (`seamless-<pid>-<nonce>.layout.toml`). The restore writer
//! publishes it under a lock with a temporary sibling, then syncs the file,
//! renames it and syncs the directory. On macOS each sync is an
//! `F_FULLFSYNC`, about 4 ms on the machine P5 was measured on.
//!
//! # What is static between the launch and the park, read from the code
//!
//! * The LAYOUT SIDECAR. It is the window/tab/pane arrangement
//!   (`App::capture_handoff_layout`): frames, tabs, splits, each leaf's
//!   folder and title, Settings drafts. It moves when the person changes the
//!   desk, and not otherwise. So it is written BEFORE the park, at the
//!   attempt's own name, by [`speculate_layout`]. At the park,
//!   [`seal_layout`] reads it back with the reader the successor uses and
//!   compares it with the park's own bytes. It is kept only when it is
//!   byte-equal to them; otherwise it is discarded and written again.
//! * The two private PIPES (the proof and the Commit channels). They hold no
//!   state at all, so the launched lane makes them before the park too
//!   (`app_update_handoff`).
//!
//! # What is not static, and stays after the park
//!
//! * The WINDOW FACTS (`WindowCarry`) are a field of the manifest, not a file
//!   of their own, and the manifest is post-park.
//! * The CONTROL CARRY's sidecars are exported from the park's own fence (the
//!   archive's counters and the differ's screen-sized state, taken under the
//!   freeze). They move with every turn and every line of output, and they
//!   are written without a sync.
//! * The MANIFEST, the SCREEN sidecars and the ADOPTION PROOF are the
//!   digested state. `seamless::write_outgoing_with_held` stays their one
//!   writer.
//!
//! # The contract
//!
//! `NativeUpdateSpeculativeSidecar` (aterm-spec) is a writer/reader pair.
//! The writer is this module, before the park and at it. The reader is the
//! successor, which reads the sidecar at its name after its grant and hashes
//! those bytes into the proof it must match. The successor never reads the
//! file before its grant, and the grant waits for [`seal_layout`]. Every
//! failure path retires the attempt's files by their prefix
//! (`seamless::discard_outgoing`), and the speculative sidecar carries that
//! prefix. The Tier-1 is [`conformance`].

use std::path::{Path, PathBuf};
use std::time::Duration;

/// A layout sidecar written before the park.
///
/// Only the launched lane, macOS's alone, builds one. Elsewhere on unix no code
/// builds one, but [`seal_layout`] still takes it and reads its fields, so
/// rustc does not count it as dead code. An `expect(dead_code)` here went
/// unfulfilled on an aarch64-linux type-check.
#[derive(Debug)]
pub(crate) struct SpeculativeLayout {
    /// Where it was written: the attempt's layout sidecar.
    path: PathBuf,
    /// The exact bytes written.
    wire: String,
    /// How long the write took: the time a kept sidecar saves the freeze.
    took: Duration,
}

/// Write the attempt's layout sidecar BEFORE the park: `layout` (captured
/// with every reader live), serialized, at `path` (the attempt's own
/// `seamless-<pid>-<nonce>.layout.toml`), through the same writer the park
/// would use (`restore::write_wire_once_to`: lock, sync, rename, directory
/// sync).
///
/// A layout that does not survive its own TOML round trip is not written:
/// the park refuses such a layout anyway. A write that fails leaves nothing
/// at `path` — the park then writes the sidecar itself.
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
    /// The speculation was written under another name than this attempt's
    /// sidecar (never, as the lane derives both from one nonce; checked
    /// rather than assumed).
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

/// SEAL the attempt's layout sidecar at the park: after this returns `Ok`,
/// `path` holds exactly `wire`, the park's own serialized layout, and nothing
/// else.
///
/// A `speculative` sidecar is KEPT only when the bytes it wrote are `wire`
/// AND the file at `path` still reads back as `wire`. The read-back uses the
/// successor's own reader, so what was found equal is what the successor
/// will read. Otherwise the file is discarded and written again from `wire`,
/// and the result says why. With no speculation, the sidecar is written here,
/// as the park always did.
///
/// Correctness over speed: the read-back costs one small read, and it is
/// what makes "kept" mean "the successor reads the park's bytes" rather than
/// "this process remembers writing them" (`SealHoldsTheParksBytes`).
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
    // DISCARD, THEN REWRITE. The discard is explicit rather than left to the
    // rename below, so a rewrite that fails leaves no stale sidecar at the
    // name either (and the failure path's sweep finds nothing to miss).
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
