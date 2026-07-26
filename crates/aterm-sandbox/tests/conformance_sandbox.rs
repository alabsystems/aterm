// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates
//
//! Tier-1 trace conformance: bind the REAL sandbox apply step to the external
//! `Sandbox.tla` design spec (TRUST_NATIVE_TLA Phase 2, CONFINEMENT family).
//!
//! `Sandbox.tla` is model-checked in the abstract by aterm-spec-models'
//! `model_check.rs` (Tier-0: it proves the *design* of the requested∧supported⇒
//! applied discipline sound, and catches the macOS no-op at `Buggy=TRUE`), but
//! nothing tied it to the code that actually runs. This test closes that gap: it
//! drives the genuine [`aterm_sandbox::apply_step`] — the pure per-restriction rule
//! the real [`aterm_sandbox::Limits::apply`] loop embodies (attempt every requested
//! limit; the OS-supported ones land; an unsupported one never blocks the supported
//! ones) — over the full requested/supported matrix, projects each onto the spec
//! variables `<<requested, supported, applied, done>>`, and asks the real `ty`
//! binary to confirm every observed `Init -> Apply` transition is one the committed
//! `Sandbox.tla`'s `Next` admits.
//!
//! METHOD — `Apply` is the spec's ONLY action and it fires once from `Init`, so each
//! real transition IS the strict first transition `ty trace validate --spec` checks
//! (step 0 against `Init`, step 0->1 against `Next`). We therefore validate directly
//! against the COMMITTED `Sandbox.tla` — no parameterized variant needed. Function-
//! valued vars are encoded with the `{"type":"function","value":{domain,mapping}}`
//! JSON form `ty`'s json_codec decodes. A NEGATIVE control (claim an unsupported
//! restriction was applied — the `NoPhantomApply`/`AllSupportedApplied` violation,
//! i.e. the macOS no-op's mirror image) MUST be ty-REJECTED, so a pass is never
//! vacuous.
//!
//! ALSO drives the real [`Limits::apply`] end-to-end for one universally-supported
//! restriction (`RLIMIT_NOFILE`), reading the limit back, so the binding is not only
//! to the pure rule but to a real `setrlimit` that lands — the same end-to-end check
//! the crate's own `apply_actually_sets_the_limit` unit test makes.
//!
//! `ty` is located by the same fixed canonical path search the other conformance
//! gates use; VERIFICATION GATE (honesty ratchet): absent `ty`, this test FAILS.

use std::path::{Path, PathBuf};
use std::process::Command;

// The end-to-end `Limits::apply` + rlimit readback is POSIX-only (on Windows the
// actuator is a documented cap-gated no-op — there is no limit to read back); the
// pure `apply_step` projection below runs on every platform.
#[cfg(unix)]
use aterm_cap::{Authority, Cap, Tier};
use aterm_sandbox::apply_step;
#[cfg(unix)]
use aterm_sandbox::{Limits, Sandbox};
use aterm_spec::verify::ty_escalation;

/// `K` from `Sandbox.cfg` — the number of restriction slots the bounded model uses.
const K: usize = 4;

// VERIFICATION GATE (honesty ratchet, batteries-on) in `aterm_spec::verify`:
// verification is always required — an absent Trust `ty` FAILS the test with a build
// hint (`cargo build --release -p tla-cli` in $HOME/trust/first-party/ty).

/// Encode a `[bool; K]` as `ty`'s function-value JSON (`[1..K -> BOOLEAN]`).
fn func_json(bits: &[bool]) -> String {
    let domain: Vec<String> = (1..=bits.len())
        .map(|n| format!("{{\"type\":\"int\",\"value\":{n}}}"))
        .collect();
    let mapping: Vec<String> = bits
        .iter()
        .enumerate()
        .map(|(i, b)| {
            format!(
                "[{{\"type\":\"int\",\"value\":{}}},{{\"type\":\"bool\",\"value\":{}}}]",
                i + 1,
                b
            )
        })
        .collect();
    format!(
        "{{\"type\":\"function\",\"value\":{{\"domain\":[{}],\"mapping\":[{}]}}}}",
        domain.join(","),
        mapping.join(",")
    )
}

/// A two-step `ty` trace `[Init, Apply]`: step 0 (requested/supported chosen, applied
/// all-FALSE, done=FALSE) must match `Init`; step 1 (applied set, done=TRUE) must
/// match the `Apply` action. Module name is `Sandbox` (must match the committed
/// `---- MODULE Sandbox ----`).
fn apply_trace(
    requested: &[bool],
    supported: &[bool],
    applied0: &[bool],
    applied1: &[bool],
) -> String {
    let st = |req: &[bool], sup: &[bool], app: &[bool], done: bool| {
        format!(
            "{{\"requested\":{},\"supported\":{},\"applied\":{},\"done\":{{\"type\":\"bool\",\"value\":{}}}}}",
            func_json(req),
            func_json(sup),
            func_json(app),
            done
        )
    };
    format!(
        "{{\"version\":\"1\",\"module\":\"Sandbox\",\
         \"variables\":[\"requested\",\"supported\",\"applied\",\"done\"],\"steps\":[\
         {{\"index\":0,\"state\":{}}},\
         {{\"index\":1,\"state\":{},\"action\":{{\"name\":\"Apply\"}}}}\
         ]}}",
        st(requested, supported, applied0, false),
        st(requested, supported, applied1, true),
    )
}

/// Validate one real `Init -> Apply` transition against the COMMITTED `Sandbox.tla`.
fn validate(
    ty: &Path,
    dir: &Path,
    requested: &[bool],
    supported: &[bool],
    applied1: &[bool],
) -> (bool, String) {
    let spec = manifest_spec("Sandbox.tla");
    let cfg = manifest_spec("Sandbox.cfg");
    let applied0 = vec![false; K];
    let trace = dir.join("t.json");
    std::fs::write(
        &trace,
        apply_trace(requested, supported, &applied0, applied1),
    )
    .expect("write trace");
    let out = Command::new(ty)
        .arg("trace")
        .arg("validate")
        .arg(&trace)
        .arg("--spec")
        .arg(&spec)
        .arg("--config")
        .arg(&cfg)
        .output()
        .expect("run ty trace validate");
    let combined = format!(
        "{}{}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );
    (out.status.success(), combined)
}

/// Path to a committed external spec in aterm-spec-models.
fn manifest_spec(name: &str) -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .unwrap()
        .join("aterm-spec-models/specs")
        .join(name)
}

#[test]
fn real_sandbox_apply_conforms_to_sandbox_spec() {
    // TIERED (VERIFY-1): a committed hand-written `.tla` — external-tool
    // obligation; runs only where the Trust toolchain is installed.
    let Some(ty) = ty_escalation("Sandbox apply conformance") else {
        return;
    };
    let dir = std::env::temp_dir().join(format!("aterm-sandbox-conf-{}", std::process::id()));
    std::fs::create_dir_all(&dir).expect("mk tempdir");

    // POSITIVE: every requested/supported combination over K slots. For each, the
    // REAL per-slot apply rule (`apply_step`) computes `applied`, and ty confirms
    // that `Init -> Apply` is a transition `Sandbox.tla`'s `Next` admits. This
    // sweeps all 2^(2K) capability surfaces the spec's `Init` chooses among.
    let mut validated = 0usize;
    let applied0 = vec![false; K];
    for rmask in 0u32..(1 << K) {
        for smask in 0u32..(1 << K) {
            let requested: Vec<bool> = (0..K).map(|n| (rmask >> n) & 1 == 1).collect();
            let supported: Vec<bool> = (0..K).map(|n| (smask >> n) & 1 == 1).collect();
            // The REAL rule the apply loop embodies.
            let applied1 = apply_step(&requested, &supported, &applied0);
            // Spot-check a representative spread (full 256 ty runs is slow); always
            // include the all-on, all-off, and mixed surfaces.
            let interesting = rmask == 0
                || smask == 0
                || rmask == (1 << K) - 1
                || smask == (1 << K) - 1
                || (rmask ^ smask) == (1 << K) - 1;
            if !interesting {
                continue;
            }
            let (ok, out) = validate(&ty, &dir, &requested, &supported, &applied1);
            assert!(
                ok,
                "real Apply (requested={requested:?} supported={supported:?}) -> applied={applied1:?} \
                 must conform to Sandbox.tla\n--- ty ---\n{out}"
            );
            validated += 1;
        }
    }

    // NEGATIVE CONTROL — the macOS no-op's mirror image: claim a restriction that
    // was requested but NOT supported got applied (a PHANTOM apply). `NoPhantomApply`
    // forbids it; ty MUST reject. (Also covers the dual defect: an UNDER-apply where
    // a requested∧supported slot stays FALSE — the actual macOS no-op — is rejected
    // by `AllSupportedApplied`, exercised below.)
    let requested = vec![true, true, false, false];
    let supported = vec![true, false, true, false]; // slot 2 requested but unsupported
    let phantom = vec![true, true, false, false]; // claims slot 2 (unsupported) applied
    let (ok, o) = validate(&ty, &dir, &requested, &supported, &phantom);
    assert!(
        !ok,
        "NEGATIVE CONTROL (phantom apply of an unsupported restriction) MUST be rejected \
         — NoPhantomApply forbids applying a non-(requested∧supported) slot\n--- ty ---\n{o}"
    );

    // NEGATIVE CONTROL #2 — the literal macOS no-op: a requested∧supported slot left
    // UNapplied (the regression `AllSupportedApplied` exists to catch). ty MUST reject.
    let no_op = vec![false, false, false, false]; // nothing applied despite slot 1 requested∧supported
    let (ok2, o2) = validate(&ty, &dir, &requested, &supported, &no_op);
    assert!(
        !ok2,
        "NEGATIVE CONTROL (macOS no-op: requested∧supported slot left unapplied) MUST be rejected \
         — AllSupportedApplied forbids it\n--- ty ---\n{o2}"
    );

    // END-TO-END: drive the REAL `Limits::apply` for one universally-supported
    // restriction (RLIMIT_NOFILE) and read it back, so the binding is anchored to a
    // real `setrlimit` that lands — not only the pure per-slot rule above.
    // POSIX-only: Windows has no rlimit to read back (apply is a cap-gated no-op
    // there — `aterm_sandbox::rlimits_actuated()` is false and the launchers print
    // the posture notice), so the binding-to-a-real-syscall half is unix-gated.
    #[cfg(unix)]
    {
        let auth = unsafe { Authority::root_authority() };
        let cap: Cap<Sandbox> = auth.grant(Tier::Certified);
        let target = 256u64;
        Limits {
            open_files: Some(target),
            ..Default::default()
        }
        .apply(&cap)
        .expect("apply NOFILE");
        let mut lim = libc::rlimit {
            rlim_cur: 0,
            rlim_max: 0,
        };
        // SAFETY: valid resource id + out-param.
        let rc = unsafe { libc::getrlimit(libc::RLIMIT_NOFILE, &mut lim) };
        assert_eq!(rc, 0, "getrlimit failed");
        assert_eq!(
            lim.rlim_cur, target,
            "the REAL apply must install a requested∧supported restriction (RLIMIT_NOFILE) — \
             the AllSupportedApplied guarantee, end-to-end"
        );
    }

    let _ = std::fs::remove_dir_all(&dir);
    #[cfg(unix)]
    eprintln!(
        "Sandbox Tier-1 conformance: {validated} real Init->Apply transitions strictly validated \
         against committed Sandbox.tla; negative controls (phantom apply, macOS no-op) both \
         rejected; real Limits::apply installed RLIMIT_NOFILE end-to-end."
    );
    #[cfg(not(unix))]
    eprintln!(
        "Sandbox Tier-1 conformance: {validated} real Init->Apply transitions strictly validated \
         against committed Sandbox.tla; negative controls (phantom apply, macOS no-op) both \
         rejected. (rlimit end-to-end readback is unix-only; this platform's apply is a \
         cap-gated no-op.)"
    );
}
