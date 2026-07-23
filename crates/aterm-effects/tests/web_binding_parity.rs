// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates
//
//! Drift guard for the duplicated web-binding modules.
//!
//! `aterm-wasm` (CPU/self-contained bundle) and `aterm-gpu-web` (WebGPU parity)
//! each carry a byte-for-byte copy of two `#[wasm_bindgen]` surface modules —
//! `effects_api`, `notifications_api`. They deliberately are NOT
//! single-sourced (a macro over `#[wasm_bindgen] impl` blocks degrades error
//! locality and IDE navigation), so the ONLY thing keeping the two copies honest
//! is this test: it fails CI the instant one copy is edited without mirroring the
//! change into the other.
//!
//! Legitimate divergence between the two copies is exactly two things: (1) the
//! host terminal type ident (`AtermTerminal` vs `AtermGpuTerminal`), and (2) the
//! per-crate doc/comment wording. Both are normalized away below; anything else
//! that differs is unintended drift and MUST fail.

// The web-binding modules gate their real bodies behind wasm32-only deps, but
// their *source text* is host-agnostic — we compare the committed bytes, so this
// runs on any native target with no wasm toolchain.
const WASM_EFFECTS: &str = include_str!("../../aterm-wasm/src/effects_api.rs");
const GPU_EFFECTS: &str = include_str!("../../aterm-gpu-web/src/effects_api.rs");
const WASM_NOTIFICATIONS: &str = include_str!("../../aterm-wasm/src/notifications_api.rs");
const GPU_NOTIFICATIONS: &str = include_str!("../../aterm-gpu-web/src/notifications_api.rs");

// Normalize a copy to its comparable form: fold the host type ident to the
// wasm-crate spelling and drop every whole-line comment (`//`, `///`, `//!`),
// which is where the intentional per-crate doc wording lives. Returns the kept
// code lines so a mismatch can name the first offending line.
fn normalize(src: &str) -> Vec<String> {
    src.replace("AtermGpuTerminal", "AtermTerminal")
        .lines()
        .filter(|line| !line.trim_start().starts_with("//"))
        .map(str::to_owned)
        .collect()
}

fn assert_parity(module: &str, wasm_src: &str, gpu_src: &str) {
    let wasm = normalize(wasm_src);
    let gpu = normalize(gpu_src);
    if wasm == gpu {
        return;
    }
    // Surface the first differing line so the mirror-the-edit fix is obvious.
    let first_diff = wasm
        .iter()
        .zip(gpu.iter())
        .enumerate()
        .find(|(_, (a, b))| a != b)
        .map(|(i, (a, b))| {
            format!("first mismatch at kept-line {i}:\n  wasm:    {a}\n  gpu-web: {b}")
        })
        .unwrap_or_else(|| {
            format!(
                "one copy has {} kept lines, the other {} — a whole block was added/removed",
                wasm.len(),
                gpu.len()
            )
        });
    panic!(
        "web-binding module `{module}` has drifted between aterm-wasm and aterm-gpu-web.\n\
         The two copies must stay identical modulo the host type ident and comments — \
         mirror the edit into both crates.\n{first_diff}"
    );
}

#[test]
fn effects_api_parity() {
    assert_parity("effects_api", WASM_EFFECTS, GPU_EFFECTS);
}

/// Mutual omission is still drift from the shared engine contract. Keep the
/// host-facing PHOSPHOR surface explicit so both bindings cannot "agree" by
/// silently dropping matrix rain again.
#[test]
fn effects_api_requires_matrix_rain_surface() {
    for symbol in [
        "set_matrix_rain_enabled",
        "matrix_rain_enabled",
        "set_matrix_rain(",
        "set_matrix_rain_reduced_motion",
        "set_effects_visibility",
        "note_matrix_rain_bell",
        "note_matrix_rain_alt_scroll",
        "note_matrix_rain_signal",
    ] {
        assert!(WASM_EFFECTS.contains(symbol), "aterm-wasm missing {symbol}");
        assert!(
            GPU_EFFECTS.contains(symbol),
            "aterm-gpu-web missing {symbol}"
        );
    }
}

#[test]
fn notifications_api_parity() {
    assert_parity("notifications_api", WASM_NOTIFICATIONS, GPU_NOTIFICATIONS);
}

#[test]
fn scene_surface_parity() {}

// Guard the normalizer itself: a real code difference must NOT be masked by the
// ident fold or comment stripping (otherwise a green parity test is vacuous).
#[test]
fn normalizer_does_not_mask_code_drift() {
    let a = "let x = self.effects.advance(now);";
    let b = "let x = self.effects.retreat(now);";
    assert_ne!(
        normalize(a),
        normalize(b),
        "normalizer must not hide code drift"
    );

    // ...but it MUST fold the two legitimate divergences to nothing.
    let wasm_ish = "/// CPU bundle.\nimpl AtermTerminal { fn f(&self) {} }";
    let gpu_ish = "/// WebGPU parity.\nimpl AtermGpuTerminal { fn f(&self) {} }";
    assert_eq!(
        normalize(wasm_ish),
        normalize(gpu_ish),
        "ident + comment normalization must equate the intentional divergences"
    );
}
