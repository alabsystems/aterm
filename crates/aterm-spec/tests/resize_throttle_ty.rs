// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates
//
//! Proof-Carrying Performance: the live-resize throttle (v0.5.4).
//!
//! A live window width/corner drag emits a `Resized` per ~cell-width, and each WIDTH
//! change rewraps the entire off-screen scrollback (`reflow_scrollback_lines`) on the
//! event-loop thread — so without throttling a big-scrollback drag hitches. The fix
//! (`on_resize_throttled` / `flush_pending_resize` in `app_config.rs`, `RESIZE_THROTTLE`
//! / `next_resize_settle` in `main.rs`) applies the first resize immediately, coalesces
//! the rest, and applies the latest on a trailing settle armed at `lastApply + Throttle`.
//!
//! `crates/aterm-spec-models/specs/ResizeThrottle.tla` models that state machine. The
//! EFFICIENCY THEOREM `BoundedReflowRate` — no two LIVE-RESIZE-driven reflows (the
//! `WindowEvent::Resized` path) are applied within one `Throttle` window, so the
//! live-resize reflow RATE is bounded and the drag cannot hitch on a per-cell-width
//! rewrap — is discharged here by the same `ty` explicit-state BFS that gates the
//! committed specs, under the Buggy-constant prove-and-catch convention. Honestly scoped
//! (see the spec header): out-of-band reflows (the control-socket `resize` verb, a
//! font/config re-grid, a scale-factor rebuild) bypass the throttle and are out of scope,
//! and this bounds the reflow RATE, not the per-reflow cost of a single large rewrap.
//!
//!   * `Buggy = 0` (the throttle): `ty` PROVES `BoundedReflowRate`, EXHAUSTIVELY.
//!   * `Buggy = 1` (the old code: reflow on every resize): `ty` finds a COUNTEREXAMPLE
//!     (two reflows within `Throttle`) — so the theorem is a genuine, non-vacuous catch
//!     of the unbounded-reflow bug, not a tautology.
//!
//! Verification is always required (batteries-on): an absent `ty` FAILS the test with a
//! build hint. The certificate re-reds if the modeled throttle logic drifts.

use aterm_spec::verify::ty_escalation;
use std::path::{Path, PathBuf};
use std::process::Command;

/// `crates/aterm-spec/tests/` → `crates/aterm-spec-models/specs/ResizeThrottle.tla`.
fn spec_path() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../aterm-spec-models/specs/ResizeThrottle.tla")
}

/// Run `ty check <spec> --config <cfg> [extra...]`; return (exit-0, combined output).
fn run(ty: &PathBuf, spec: &Path, cfg: &Path, extra: &[&str]) -> (bool, String) {
    let mut cmd = Command::new(ty);
    cmd.arg("check")
        .arg(spec)
        .arg("--config")
        .arg(cfg)
        .args(extra);
    // ARMED: partial-order reduction is a speed feature that does not
    // preserve coverage, and the `ty` this machine discovers has an
    // UNSOUND one (it reduced a 128-state model to 1 and still printed
    // "No errors found (exhaustive)"). Every `ty check` in this workspace
    // arms the whole space through the one shared place.
    aterm_spec::verify::arm_whole_space_check(&mut cmd);
    let out = cmd.output().expect("run ty check");
    let combined = format!(
        "{}{}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );
    (out.status.success(), combined)
}

#[test]
fn resize_throttle_proves_bounded_reflow_rate_and_catches_unbounded() {
    // TIERED (VERIFY-1): a hand-written `.tla` — external-tool obligation;
    // runs only where the Trust toolchain is installed (loud notice otherwise).
    let Some(ty) = ty_escalation("ResizeThrottle (resize throttle) spec") else {
        return;
    };
    let spec = spec_path();
    assert!(
        spec.exists(),
        "missing committed spec {} — the PCP resize-throttle certificate",
        spec.display()
    );
    let okcfg = spec.with_extension("cfg"); // ResizeThrottle.cfg (Buggy = 0)

    // Buggy = 0 (the throttle): the efficiency theorem must hold under an EXHAUSTIVE
    // (uncapped) BFS — no two reflows are ever applied within one Throttle window.
    let (ok, out) = run(&ty, &spec, &okcfg, &["--require-exhaustive"]);
    assert!(
        ok,
        "ResizeThrottle (Buggy=0, the throttle) must model-check clean (BoundedReflowRate)\n{out}"
    );
    assert!(
        out.contains("exhaustive"),
        "the prove run must be an exhaustive (uncapped) BFS, not a bounded sample\n{out}"
    );

    // Buggy = 1 (the OLD code: reflow on every resize): MUST yield a counterexample —
    // a reachable state with two reflows within Throttle. The cfg flips only Buggy.
    let dir = std::env::temp_dir().join(format!("aterm-pcp-resize-{}", std::process::id()));
    std::fs::create_dir_all(&dir).expect("mk tempdir");
    let bugcfg = dir.join("ResizeThrottle.bug.cfg");
    let oktext = std::fs::read_to_string(&okcfg).expect("read committed cfg");
    let bugtext = oktext.replace("Buggy = 0", "Buggy = 1");
    assert_ne!(oktext, bugtext, "cfg must contain `Buggy = 0` to flip");
    std::fs::write(&bugcfg, bugtext).expect("write bug cfg");
    let (bug_ok, bug_out) = run(&ty, &spec, &bugcfg, &[]);
    assert!(
        !bug_ok,
        "ResizeThrottle (Buggy=1, old code) MUST yield a counterexample (unbounded reflow)\n{bug_out}"
    );
    // The counterexample must be the EFFICIENCY invariant failing — not an unrelated one
    // (e.g. TypeOK) — so a future regression can't masquerade as a genuine catch.
    assert!(
        bug_out.contains("BoundedReflowRate"),
        "Buggy=1 must violate BoundedReflowRate specifically, not another invariant\n{bug_out}"
    );
    let _ = std::fs::remove_dir_all(&dir);

    eprintln!(
        "PCP ResizeThrottle: BoundedReflowRate proven (Buggy=0, exhaustive) and \
         unbounded reflow caught (Buggy=1 → counterexample)."
    );
}
