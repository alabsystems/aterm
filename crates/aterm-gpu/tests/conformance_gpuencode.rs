// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates
//
//! Tier-1 trace conformance: bind the REAL frame-encode slice-precondition decision
//! to the external `GpuEncode.tla` design spec (TRUST_NATIVE_TLA Phase 2, GPU
//! FRAME-ENCODE safety).
//!
//! `GpuEncode.tla` is model-checked in the abstract by aterm-spec-models'
//! `model_check.rs` (Tier-0: it proves `NeverSliceEmpty` — a bg vertex buffer is
//! sliced/bound ONLY when it holds ≥1 instance — and catches the unconditional-slice
//! panic at `Buggy=TRUE`). This test ties that to the code that runs: the bg-instance
//! `Encode` step calls `bg_buf.slice(..)` exactly when [`aterm_gpu::should_slice`]
//! (the real gate inside `InstanceBuf::upload`, refactored to be GPU-free) returns
//! `true`. The FULL GPU encode needs a live wgpu device (so it cannot be driven
//! headlessly in CI), but the slice DECISION — which IS the modeled property — is
//! pure, so we drive it directly: build a trajectory of `Append` steps (the CPU cell
//! walk pushing one `BgInstance` per non-default-bg cell) followed by an `Encode`
//! whose `sliced` is the REAL `should_slice(bgInst * stride)`, and `ty trace validate
//! --spec` each transition against `GpuEncode`'s `Next`.
//!
//! METHOD — strict per-transition validation: each transition is a 2-step trace with
//! `Init` pinned to `prev` via a hardcoded-`Init` variant of the COMMITTED spec
//! (mechanical `Init`-line rewrite; action/invariant bodies verbatim → no drift).
//! TWO negative controls — (a) `Encode` of an EMPTY frame (`bgInst = 0`) that slices
//! anyway (the 4ab4eb9 panic), and (b) an `Encode` that fails to slice a NON-empty
//! frame — MUST be ty-REJECTED, so a pass is never vacuous.
//!
//! `ty` is located by the same fixed canonical path search; absent `ty` the test
//! FAILS (honesty ratchet, no skip path).

use std::path::{Path, PathBuf};
use std::process::Command;

use aterm_gpu::should_slice;
use aterm_spec::verify::ty_escalation;

/// Spec `CONSTANT MaxCells` (from `GpuEncode.cfg`).
const MAX_CELLS: i64 = 4;
/// Bytes per `BgInstance` (the renderer's packed `rect:[u16;4] + color:[u8;4]`),
/// so a non-empty `bgInst` count maps to a non-empty byte stream — the input
/// `should_slice` actually sees in `InstanceBuf::upload(bytemuck::cast_slice(&bg))`.
const STRIDE: usize = 12;

// VERIFICATION GATE (honesty ratchet, batteries-on) in `aterm_spec::verify`:
// verification is always required — an absent Trust `ty` FAILS the test with a build
// hint (`cargo build --release -p tla-cli` in $HOME/trust/first-party/ty).

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
    let out = Command::new(ty)
        .arg("trace")
        .arg("validate")
        .arg(&trace_f)
        .arg("--spec")
        .arg(&spec_f)
        .arg("--config")
        .arg(&cfg_f)
        .output()
        .expect("run ty trace validate");
    let combined = format!(
        "{}{}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );
    (out.status.success(), combined)
}

/// Drive a frame: `n` Appends, then one Encode whose `sliced` is the REAL
/// `should_slice` decision over the bg-instance byte stream. Returns the
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
    // TIERED (VERIFY-1): a committed hand-written `.tla` — external-tool
    // obligation; runs only where the Trust toolchain is installed.
    let Some(ty) = ty_escalation("GpuEncode conformance") else {
        return;
    };
    let committed =
        std::fs::read_to_string(spec_path("GpuEncode.tla")).expect("read GpuEncode.tla");
    let dir = std::env::temp_dir().join(format!("aterm-gpuencode-conf-{}", std::process::id()));
    std::fs::create_dir_all(&dir).expect("mk tempdir");

    let mut validated = 0usize;

    // POSITIVE — both regimes:
    //   * a ZERO-cell frame (the degenerate one that triggered the panic): 0 Appends,
    //     then Encode → should_slice(0) == false → sliced stays FALSE (the FIX).
    //   * non-empty frames (1..MaxCells fills): Encode → should_slice(>0) == true.
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

    // NEGATIVE CONTROL (a) — the 4ab4eb9 panic: Encode an EMPTY frame (bgInst=0) that
    // slices anyway. `NeverSliceEmpty` forbids it; ty MUST reject.
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

    // NEGATIVE CONTROL (b) — the dual: a NON-empty frame whose Encode fails to slice
    // (sliced=FALSE while bgInst>0). The committed `Encode` sets `sliced'=(bgInst>0)`,
    // so a held-FALSE is not an admitted transition; ty MUST reject.
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
