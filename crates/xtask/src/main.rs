// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0
// Author: Andrew Yates

//! aterm build-graph tasks — the ALWAYS-RUN nodes of TRUST_VACUITY_GATE §2.
//!
//! Two subcommands, both independent of any one crate's `cargo test` binary
//! (finding 5 — "the teeth are there, the wiring isn't"):
//!
//!   * `harness-manifest` (§2.1 / finding 1a): enumerate every REAL
//!     `#[kani::proof] fn` across the workspace `crates/` and write a
//!     `HarnessManifest` JSON to `target/trust/harness-manifest.json` in the exact
//!     shape `trust-ir spec-link --harness-manifest` expects
//!     (`{"harnesses":[{"name","span"}]}`). This is the data trust-ir's L1 resolves
//!     `proof_name` against (the standalone IR has no compiler/DefId view, so the
//!     manifest must be produced HERE and handed to spec-link).
//!
//!   * `spec-link` (§2.5 / finding 5): the always-run cross-reference node. It (1)
//!     regenerates the manifest, (2) builds the anchor graph from the EMBEDDED models +
//!     external ISOLATION `.tla` + the cross-crate-collected `proof_anchor!`s
//!     (aterm-scrollback / aterm-grid / aterm-search, linked with `spec-anchors` ON in
//!     THIS binary),
//!     (3) lowers it with `aterm_spec::ir::lower_to_ir` (now emitting `proof` lines),
//!     and (4) shells `trust-ir spec-link --harness-manifest … --require-manifest`.
//!     The text-only artifact is explicitly design-only (it has no compiler
//!     `FuncId`s), so this node requires a structurally clean design-only report:
//!     S0/S1, Ob.1/Ob.4, proof-name resolution (L1), and mandatory projection
//!     labels (L2). Aterm's in-process closure remains the fail-closed Ob.3
//!     coverage gate.
//!
//! NOTE on scope: the in-SOURCE `path_confine` / `window_routing` `#[cfg(test)]`
//! anchors collect ONLY in aterm-gui's test binary (inventory sees only LINKED object
//! code), so the FULL ISOLATION + window_routing in-source set is enforced by the
//! `spec_xref_gate` there. THIS node enforces the embedded models, the external
//! ISOLATION specs, and the cross-crate PROOF anchors — i.e. the L1 teeth that the
//! manifest unlocks — independent of that test binary.

use std::path::{Path, PathBuf};
use std::process::{Command, ExitCode};

use aterm_spec::tla_check::TlaSpec;
use aterm_spec::xref::{self, SpecModule};

mod gate;
mod perf;

// Force the proof-anchor-bearing rlibs into the link graph: `inventory` only collects
// `submit!`s from LINKED object code, and a bin that references NOTHING from these
// crates would let the linker drop their rlibs (and the `spec_proof_anchors` module's
// `proof_anchor!` consts with them). The `extern crate` declarations + the
// `force_link` reference below pull them in so `xref::proof_anchors()` sees the kani
// half cross-crate (the same mechanism aterm-gui's test binary relies on).
extern crate aterm_grid;
extern crate aterm_scrollback;
extern crate aterm_search;

fn main() -> ExitCode {
    let args: Vec<String> = std::env::args().collect();
    let cmd = args.get(1).map(String::as_str);
    match cmd {
        Some("harness-manifest") => match write_harness_manifest() {
            Ok(path) => {
                eprintln!("xtask harness-manifest: wrote {}", path.display());
                ExitCode::SUCCESS
            }
            Err(e) => {
                eprintln!("xtask harness-manifest FAILED: {e}");
                ExitCode::FAILURE
            }
        },
        Some("spec-link") => spec_link(),
        Some("gate") => gate::run(
            args.get(2).map(String::as_str),
            args.get(3..).unwrap_or_default(),
        ),
        Some("verify") => verify(&args[2..]),
        _ => {
            eprintln!(
                "usage: xtask <harness-manifest|spec-link|gate <check>|verify [args…]>\n\
                 \n\
                 harness-manifest  enumerate #[kani::proof] fns -> target/trust/harness-manifest.json\n\
                 spec-link         lower the anchor graph + run `trust-ir spec-link --require-manifest`\n\
                 gate <check>      local enforcement gate (NO CI): all|drift|dormant|lint|perf\n\
                                   `gate lint [--no-fmt|--fmt-only]` — tippy + trustfmt +\n\
                                   guards; --no-fmt drops the formatter lane and\n\
                                   --fmt-only keeps only it (both passes, no compiler,\n\
                                   seconds), nothing else narrowed either way\n\
                                   see docs/EXCEED_GHOSTTY_PLAN.md\n\
                 verify [args…]    run THE gate, tools/verify.sh, forwarding every argument\n\
                                   (this is what the `cargo verify` alias dispatches to)"
            );
            ExitCode::FAILURE
        }
    }
}

// ---------------------------------------------------------------------------
// verify — the `cargo verify` verb
// ---------------------------------------------------------------------------

/// Dispatch to `tools/verify.sh`, forwarding every argument verbatim.
///
/// This exists ONLY because a cargo alias can expand to a cargo subcommand and
/// nothing else, so the repo's one gate needs a Rust hop to become a first-class
/// verb. It deliberately implements no policy: no default flags, no argument
/// rewriting, no "helpful" mode selection. Everything the gate means lives in
/// `tools/verify.sh`, and a second place that could disagree with it would
/// reintroduce exactly the ambiguity that script's header exists to remove.
///
/// Fail-closed in both directions that matter:
///   * a missing / unrunnable `tools/verify.sh` is a FAILURE, never a silent
///     success — `cargo verify` must not be able to report "fine" without the
///     gate having run;
///   * a non-zero child status stays non-zero. A status that is non-zero but
///     not representable as a non-zero `u8` (a signal death, or an exit code
///     whose low byte is 0) maps to 1 rather than truncating to 0.
fn verify(args: &[String]) -> ExitCode {
    let script = workspace_root().join("tools").join("verify.sh");
    if !script.is_file() {
        eprintln!(
            "xtask verify: THE GATE IS MISSING — {} does not exist. Nothing was \
             verified; this is a failure, not a pass.",
            script.display()
        );
        return ExitCode::FAILURE;
    }
    let status = match Command::new(&script).args(args).status() {
        Ok(s) => s,
        Err(e) => {
            eprintln!(
                "xtask verify: could not execute {}: {e}. Nothing was verified.",
                script.display()
            );
            return ExitCode::FAILURE;
        }
    };
    match status.code() {
        Some(0) => ExitCode::SUCCESS,
        // Preserve the gate's own exit code where it fits; never let a non-zero
        // status become 0 through a `as u8` truncation.
        Some(code) => match u8::try_from(code) {
            Ok(0) => ExitCode::FAILURE,
            Ok(byte) => ExitCode::from(byte),
            Err(_) => ExitCode::FAILURE,
        },
        // Killed by a signal: no exit code at all, and emphatically not a pass.
        None => {
            eprintln!("xtask verify: {} was killed by a signal", script.display());
            ExitCode::FAILURE
        }
    }
}

// ---------------------------------------------------------------------------
// Workspace layout
// ---------------------------------------------------------------------------

/// The workspace root (the dir that holds `crates/` and `target/`). `xtask`'s
/// manifest dir is `<root>/crates/xtask`, so the root is two levels up.
pub(crate) fn workspace_root() -> PathBuf {
    let manifest = Path::new(env!("CARGO_MANIFEST_DIR"));
    // `<root>/crates/xtask` -> up two -> `<root>`. Handle the (type-reachable but
    // practically-impossible) shallow-path case without panicking: fall back to the
    // manifest dir itself rather than `.expect()`, so the function is panic-free.
    match manifest.parent().and_then(Path::parent) {
        Some(root) => root.to_path_buf(),
        None => manifest.to_path_buf(),
    }
}

// ---------------------------------------------------------------------------
// harness-manifest (finding 1a)
// ---------------------------------------------------------------------------

/// One `#[kani::proof]` harness: its fn name + a `file:line` span (opaque to L1,
/// which matches only on `name`).
struct HarnessEntry {
    name: String,
    span: String,
}

/// Enumerate every `#[kani::proof] fn <name>` under the workspace `crates/` and write
/// the `HarnessManifest` JSON. Returns the path written. The scan is a line walk:
/// a `#[kani::proof]` attribute line arms the next `fn <ident>` (allowing intervening
/// `#[kani::…]` / `#[cfg(kani)]` attribute lines), exactly as the harnesses are
/// authored. Names are de-duplicated (a harness name is the L1 key, unique per build).
fn write_harness_manifest() -> std::io::Result<PathBuf> {
    let root = workspace_root();
    let mut entries: Vec<HarnessEntry> = Vec::new();
    let mut seen = std::collections::BTreeSet::new();
    let mut files = Vec::new();
    collect_rs_files(&root.join("crates"), &mut files)?;
    files.sort();
    for file in &files {
        let text = std::fs::read_to_string(file)?;
        let rel = file
            .strip_prefix(&root)
            .unwrap_or(file)
            .to_string_lossy()
            .into_owned();
        let lines: Vec<&str> = text.lines().collect();
        let mut armed = false;
        for (i, raw) in lines.iter().enumerate() {
            let line = raw.trim_start();
            if line.starts_with("#[kani::proof") {
                armed = true;
                continue;
            }
            if armed {
                // Skip further attribute lines (#[kani::should_panic], #[cfg(kani)], …)
                // and blank/comment lines between the attr and the fn.
                if line.starts_with("#[") || line.is_empty() || line.starts_with("//") {
                    continue;
                }
                if let Some(name) = parse_fn_name(line) {
                    if seen.insert(name.clone()) {
                        entries.push(HarnessEntry {
                            name,
                            span: format!("{rel}:{}:1", i + 1),
                        });
                    }
                    armed = false;
                } else {
                    // A non-attr, non-fn line after the attr — not a harness; disarm.
                    armed = false;
                }
            }
        }
    }
    entries.sort_by(|a, b| a.name.cmp(&b.name));

    let out_dir = root.join("target").join("trust");
    std::fs::create_dir_all(&out_dir)?;
    let out_path = out_dir.join("harness-manifest.json");
    std::fs::write(&out_path, render_manifest_json(&entries))?;
    eprintln!("xtask: {} kani harness(es) enumerated", entries.len());
    Ok(out_path)
}

/// Recursive `*.rs` collection (skips `target/` + hidden dirs). The
/// implementation moved to `aterm-census` — the shared census library that
/// `gate mainloop` AND tools/freeze-safety-gate's build.rs both consume — so
/// the file-walk semantics (and its Trust-L0-hardened byte-comparison shape)
/// cannot diverge between the gates and the build-blocking census. Re-exported
/// here because every xtask scan (harness-manifest, drift, fault, counts) uses
/// the same walk.
pub(crate) use aterm_census::collect_rs_files;

/// Extract `<ident>` from a `(pub )?(unsafe )?fn <ident>…` line; `None` otherwise.
fn parse_fn_name(line: &str) -> Option<String> {
    let mut rest = line;
    for kw in [
        "pub ",
        "pub(crate) ",
        "unsafe ",
        "const ",
        "async ",
        "extern ",
    ] {
        if let Some(s) = rest.strip_prefix(kw) {
            rest = s.trim_start();
        }
    }
    let rest = rest.strip_prefix("fn ")?;
    let ident: String = rest
        .trim_start()
        .chars()
        .take_while(|c| c.is_alphanumeric() || *c == '_')
        .collect();
    if ident.is_empty() { None } else { Some(ident) }
}

/// Render the `HarnessManifest` JSON in the documented shape. Hand-rolled (no serde
/// dep): each `name`/`span` is JSON-escaped (both are plain identifiers / file paths
/// here, but escape defensively).
fn render_manifest_json(entries: &[HarnessEntry]) -> String {
    let mut s = String::from("{\n  \"harnesses\": [");
    for (i, e) in entries.iter().enumerate() {
        if i > 0 {
            s.push(',');
        }
        // Spelled as direct `push_str`s, byte-identical to the former
        // `format!("\n    {{ \"name\": {}, \"span\": {} }}", …)`:
        // `fmt::Arguments::new` is an unmodeled construct the strict gate's
        // native TrustIr lowering refuses, which failed this function's
        // panic-freedom proof outright.
        s.push_str("\n    { \"name\": ");
        s.push_str(&json_str(&e.name));
        s.push_str(", \"span\": ");
        s.push_str(&json_str(&e.span));
        s.push_str(" }");
    }
    if !entries.is_empty() {
        s.push_str("\n  ");
    }
    s.push_str("]\n}\n");
    s
}

fn json_str(s: &str) -> String {
    // Capacity is a pure allocation hint — the escaped output is identical
    // with any starting capacity (`push`/`push_str` grow on demand) — so
    // clamping it is behavior-preserving. The `len < 4096` check dominates
    // each branch-local `with_capacity` call (a joined `cap` variable would
    // lose the bound at the phi node), which discharges both the `len + 2`
    // overflow obligation and the L0 unbounded-allocation budget. Real
    // inputs are fn identifiers and `file:line` spans, far below 4 KiB, so
    // the hint stays exact for every input the callers produce.
    let len = s.len();
    let mut out = if len < 4096 {
        String::with_capacity(len + 2)
    } else {
        String::with_capacity(4096)
    };
    out.push('"');
    for c in s.chars() {
        match c {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\t' => out.push_str("\\t"),
            c => out.push(c),
        }
    }
    out.push('"');
    out
}

// ---------------------------------------------------------------------------
// spec-link (finding 5) — the always-run cross-reference node
// ---------------------------------------------------------------------------

/// Touch a symbol from each proof-anchor-bearing crate so the linker retains its rlib
/// (and the `spec_proof_anchors` `inventory::submit!` consts). `black_box` defeats
/// dead-code elimination of the reference itself.
fn force_link() {
    std::hint::black_box(aterm_scrollback::DEFAULT_LINE_LIMIT);
    std::hint::black_box(aterm_grid::MAX_GRID_ROWS);
    std::hint::black_box(aterm_search::MAX_SEARCH_MATCHES);
}

fn spec_link() -> ExitCode {
    force_link();
    // (1) Regenerate the manifest the L1 resolution needs.
    let manifest = match write_harness_manifest() {
        Ok(p) => p,
        Err(e) => {
            eprintln!("xtask spec-link: could not write harness manifest: {e}");
            return ExitCode::FAILURE;
        }
    };

    // (2) Build the anchor graph from THIS binary's linked object code: every embedded
    // model + every external ISOLATION `.tla` + the cross-crate `proof_anchor!`s.
    let mut modules: Vec<SpecModule> = xref::model_registry()
        .into_iter()
        .map(SpecModule::Embedded)
        .collect();
    let dir = aterm_spec_models::specs_dir();
    let mut external = 0usize;
    for entry in std::fs::read_dir(&dir).expect("read aterm-spec-models specs/") {
        let path = entry.expect("dir entry").path();
        if path.is_dir() || path.extension().and_then(|e| e.to_str()) != Some("tla") {
            continue;
        }
        let spec = TlaSpec::parse_file(&path)
            .unwrap_or_else(|e| panic!("failed to parse external spec {path:?}: {e}"));
        modules.push(SpecModule::External(spec));
        external += 1;
    }

    let refs: Vec<_> = xref::refinements().collect();
    let waivers: Vec<_> = xref::waivers().collect();
    let proofs: Vec<_> = xref::proof_anchors().collect();
    eprintln!(
        "xtask spec-link: anchor graph — {} module(s) ({} external ISOLATION), {} refinement(s), \
         {} waiver(s), {} proof anchor(s)",
        modules.len(),
        external,
        refs.len(),
        waivers.len(),
        proofs.len()
    );
    assert!(
        !proofs.is_empty(),
        "xtask spec-link: ZERO proof anchors collected — the cross-crate `proof_anchor!` \
         inventory (aterm-scrollback / aterm-grid / aterm-search with `spec-anchors`) did not \
         link. The L1 proof-name teeth would be untested."
    );

    // (3) Lower to a byte-conforming `.trust_irtxt` (now emitting `proof` lines).
    let module_txt =
        aterm_spec::ir::lower_to_ir("aterm_xtask_spec_link", &modules, &refs, &waivers, &proofs);
    let out_dir = workspace_root().join("target").join("trust");
    std::fs::create_dir_all(&out_dir).expect("mk target/trust");
    let ir_path = out_dir.join("xtask-spec-link.trust_irtxt");
    std::fs::write(&ir_path, &module_txt).expect("write .trust_irtxt");

    // (4) Shell `trust-ir spec-link --harness-manifest … --require-manifest`.
    let trust_ir = match aterm_spec::verify::find_trust_ir() {
        Some(p) => p,
        None => {
            eprintln!(
                "xtask spec-link: VERIFICATION GATE — `trust-ir` not found; build it at \
                 $HOME/trust/first-party/trust-ir/target/release/trust-ir (or put it on PATH). The \
                 always-run spec-link node FAILS rather than silently skipping."
            );
            return ExitCode::FAILURE;
        }
    };
    let out = Command::new(&trust_ir)
        .arg("spec-link")
        // aterm emits TEXT (`lower_to_ir`); trust-ir 0.2.0 maps the `.trust_ir`
        // extension to BINARY, so pin the format explicitly.
        .arg("--format")
        .arg("text")
        .arg(&ir_path)
        .arg("--harness-manifest")
        .arg(&manifest)
        .arg("--require-manifest")
        .output()
        .unwrap_or_else(|e| panic!("failed to run {trust_ir:?} spec-link: {e}"));
    let report = format!(
        "{}{}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );
    eprintln!("--- trust-ir spec-link (xtask always-run node) ---\n{report}");
    let structurally_clean =
        aterm_spec::ir::spec_link_report_is_clean(out.status.success(), &report);
    let proof_evidence = report.contains("harness manifest")
        && report.contains(&format!("checked {} proof binding", proofs.len()));
    if structurally_clean && proof_evidence {
        eprintln!(
            "xtask spec-link: GREEN (STRUCTURAL, DESIGN-ONLY) — trust-ir checked S0/S1 + \
             Ob.1/Ob.4 + L2 and resolved every proof_name against the manifest (L1) over the \
             embedded models, external ISOLATION specs, and {} proof anchor(s). The artifact is \
             explicitly non-certifying because it carries no compiler FuncIds; aterm's in-process \
             closure separately enforces Ob.3 coverage.",
            proofs.len()
        );
        ExitCode::SUCCESS
    } else {
        eprintln!(
            "xtask spec-link: FAILED — trust-ir reported a structural violation, an unexpected \
             non-certification reason, or did not prove manifest use (see above)."
        );
        ExitCode::FAILURE
    }
}
