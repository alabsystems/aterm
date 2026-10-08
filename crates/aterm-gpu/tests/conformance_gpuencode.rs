// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates
//
//! Tier-1 trace conformance: binds the real frame-encode slice decision to
//! the `GpuEncode.tla` design spec.
//!
//! aterm-spec-models' `model_check.rs` checks `GpuEncode.tla` in the abstract
//! (`NeverSliceEmpty`: a bg vertex buffer is sliced only when it holds ≥1
//! instance; `Buggy=TRUE` catches an unconditional slice). Here the `Encode`
//! step's `sliced` is the real [`aterm_gpu::should_slice`] (the GPU-free gate
//! inside `VertexBuffer::upload_via` and the Metal `upload_stream`), driven
//! after a run of `Append` steps (one `BgInstance` per non-default-bg cell),
//! and each transition is checked with
//! `ty trace validate --spec` against `GpuEncode`'s `Next`.
//!
//! Each transition is a 2-step trace with `Init` pinned to `prev` via a
//! rewritten `Init` line of the committed spec (actions and invariants
//! verbatim). Two negative controls must be rejected: encoding an empty frame
//! (`bgInst = 0`) that slices anyway, and a non-empty frame that fails to
//! slice.
//!
//! Without `ty` installed the test reports that and returns.

use std::path::{Path, PathBuf};
use std::process::Command;

use aterm_gpu::should_slice;
use aterm_spec::verify::ty_escalation;

/// Spec `CONSTANT MaxCells` (from `GpuEncode.cfg`).
const MAX_CELLS: i64 = 4;
/// Bytes per `BgInstance` (`rect:[u16;4] + color:[u8;4]`), so a non-empty
/// `bgInst` maps to the non-empty byte stream `should_slice` sees in
/// `VertexBuffer::upload_via(.., aterm_bits::cast_slice(&bg))`.
const STRIDE: usize = 12;

/// Abstract spec state `<<bgInst, encoded, sliced>>`.
#[derive(Clone, Copy, PartialEq, Debug)]
struct GeState {
    bg_inst: i64,
    encoded: bool,
    sliced: bool,
}

fn spec_path(name: &str) -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .unwrap()
        .join("aterm-spec-models/specs")
        .join(name)
}

/// The committed `GpuEncode.tla` with `Init` HARDCODED to `prev`.
fn pinned_spec(committed: &str, prev: &GeState) -> String {
    let mut out = String::new();
    let mut skipping_init = false;
    for line in committed.lines() {
        if line.starts_with("Init ==") {
            skipping_init = true;
            out.push_str("Init ==\n");
            out.push_str(&format!("    /\\ bgInst = {}\n", prev.bg_inst));
            out.push_str(&format!(
                "    /\\ encoded = {}\n",
                if prev.encoded { "TRUE" } else { "FALSE" }
            ));
            out.push_str(&format!(
                "    /\\ sliced = {}\n",
                if prev.sliced { "TRUE" } else { "FALSE" }
            ));
            continue;
        }
        if skipping_init {
            if line.starts_with(char::is_whitespace) && line.trim_start().starts_with("/\\") {
                continue;
            }
            skipping_init = false;
        }
        out.push_str(line);
        out.push('\n');
    }
    out
}

fn state_json(s: &GeState) -> String {
    format!(
        "{{\"bgInst\":{{\"type\":\"int\",\"value\":{}}},\
         \"encoded\":{{\"type\":\"bool\",\"value\":{}}},\
         \"sliced\":{{\"type\":\"bool\",\"value\":{}}}}}",
        s.bg_inst, s.encoded, s.sliced
    )
}

fn transition_trace(prev: &GeState, next: &GeState, action: &str) -> String {
    format!(
        "{{\"version\":\"1\",\"module\":\"GpuEncode\",\"variables\":[\"bgInst\",\"encoded\",\"sliced\"],\
         \"steps\":[\
         {{\"index\":0,\"state\":{}}},\
         {{\"index\":1,\"state\":{},\"action\":{{\"name\":\"{}\"}}}}\
         ]}}",
        state_json(prev),
        state_json(next),
        action
    )
}

fn validate(
    ty: &Path,
    dir: &Path,
    committed: &str,
    prev: &GeState,
    next: &GeState,
    action: &str,
) -> (bool, String) {
    let spec_f = dir.join("GpuEncode.tla");
    let cfg_f = dir.join("GpuEncode.cfg");
    let trace_f = dir.join("t.json");
    std::fs::write(&spec_f, pinned_spec(committed, prev)).expect("write spec");
    std::fs::write(
        &cfg_f,
        format!(
            "CONSTANT MaxCells = {MAX_CELLS}\nCONSTANT Buggy = FALSE\n\
             SPECIFICATION Spec\nINVARIANT TypeOK\nINVARIANT NeverSliceEmpty\n\
             INVARIANT SliceImpliesFill\nCHECK_DEADLOCK FALSE\n"
        ),
    )
    .expect("write cfg");
    std::fs::write(&trace_f, transition_trace(prev, next, action)).expect("write trace");
    let mut cmd = Command::new(ty);
    cmd.arg("trace")
        .arg("validate")
        .arg(&trace_f)
        .arg("--spec")
        .arg(&spec_f)
        .arg("--config")
        .arg(&cfg_f);
    let out = aterm_spec::verify::ty_output(&mut cmd).expect("run ty trace validate");
    let combined = format!(
        "{}{}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );
    (out.status.success(), combined)
}

/// Drive a frame: `n` Appends, then one Encode whose `sliced` is the real
/// `should_slice` over the bg-instance bytes. Returns the
/// `(prev, next, action)` transitions.
fn drive_frame(n: i64) -> Vec<(GeState, GeState, &'static str)> {
    let mut steps = Vec::new();
    let mut st = GeState {
        bg_inst: 0,
        encoded: false,
        sliced: false,
    };
    for _ in 0..n {
        let prev = st;
        st.bg_inst += 1; // Append: bgInst' = bgInst + 1
        steps.push((prev, st, "Append"));
    }
    // Encode: the REAL slice-precondition decision over the actual byte stream.
    let prev = st;
    st.encoded = true;
    st.sliced = should_slice(st.bg_inst as usize * STRIDE);
    steps.push((prev, st, "Encode"));
    steps
}

#[test]
fn real_gpu_encode_slice_decision_conforms_to_gpuencode_spec() {
    // External-tool obligation on a committed `.tla`: runs only where `ty` is
    // installed.
    let Some(ty) = ty_escalation("GpuEncode conformance") else {
        return;
    };
    let committed =
        std::fs::read_to_string(spec_path("GpuEncode.tla")).expect("read GpuEncode.tla");
    let dir = std::env::temp_dir().join(format!("aterm-gpuencode-conf-{}", std::process::id()));
    std::fs::create_dir_all(&dir).expect("mk tempdir");

    let mut validated = 0usize;

    // Positive, both regimes:
    //   * a zero-cell frame: Encode → should_slice(0) == false → not sliced;
    //   * non-empty frames (1..MaxCells): Encode → should_slice(>0) == true.
    for n in 0..=MAX_CELLS {
        let steps = drive_frame(n);
        // Sanity: the real decision matches the modeled NeverSliceEmpty precondition.
        let encode = steps.last().unwrap();
        assert_eq!(
            encode.1.sliced,
            n > 0,
            "real should_slice must be (bgInst>0): n={n} sliced={}",
            encode.1.sliced
        );
        for (prev, next, action) in &steps {
            let (ok, out) = validate(&ty, &dir, &committed, prev, next, action);
            assert!(
                ok,
                "real {action} {prev:?} -> {next:?} (frame of {n} fills) must conform\n--- ty ---\n{out}"
            );
            validated += 1;
        }
    }

    // Negative control (a): encode an empty frame that slices anyway;
    // `NeverSliceEmpty` forbids it.
    let empty = GeState {
        bg_inst: 0,
        encoded: false,
        sliced: false,
    };
    let bad_slice_empty = GeState {
        bg_inst: 0,
        encoded: true,
        sliced: true,
    };
    let (ok, o) = validate(&ty, &dir, &committed, &empty, &bad_slice_empty, "Encode");
    assert!(
        !ok,
        "NEGATIVE CONTROL (slice an EMPTY bg buffer — the wgpu panic) MUST be rejected\n--- ty ---\n{o}"
    );

    // Negative control (b): a non-empty frame whose Encode does not slice. The
    // spec's `Encode` sets `sliced'=(bgInst>0)`, so this is not admitted.
    let two = GeState {
        bg_inst: 2,
        encoded: false,
        sliced: false,
    };
    let bad_noslice = GeState {
        bg_inst: 2,
        encoded: true,
        sliced: false,
    };
    let (ok2, o2) = validate(&ty, &dir, &committed, &two, &bad_noslice, "Encode");
    assert!(
        !ok2,
        "NEGATIVE CONTROL (fail to slice a NON-empty bg buffer) MUST be rejected\n--- ty ---\n{o2}"
    );

    let _ = std::fs::remove_dir_all(&dir);
    eprintln!(
        "GpuEncode Tier-1 conformance: {validated} real Append/Encode transitions (empty + \
         non-empty frames, slice decision = should_slice) strictly validated against committed \
         GpuEncode.tla; slice-empty and fail-to-slice negative controls both rejected."
    );
}
