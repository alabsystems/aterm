// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0
// Author: Andrew Yates

//! Run the census walkers against any aterm checkout:
//!
//! ```text
//! targo --unverified run -p aterm-census -- <checkout-root>            # both GUI censuses; default root: .
//! targo --unverified run -p aterm-census -- <checkout-root> --mainloop # main-loop census only
//! targo --unverified run -p aterm-census -- <checkout-root> --locks    # lock-order census only
//! targo --unverified run -p aterm-census -- <checkout-root> --wasm     # wasm-process census only
//! targo --unverified run -p aterm-census -- <checkout-root> --scope    # scope-cardinality census only
//! targo --unverified run -p aterm-census -- <checkout-root> --lazy-init # lazy-init reentrancy census only
//! ```
//!
//! `--wasm` is opt-in because checkouts without `crates/aterm-wasm` would fail
//! it closed. Exit 0 = selected censuses green; 1 = obligations violated
//! (diagnostics on stderr); 2 = usage error (`-h`/`--help` prints [`USAGE`] at
//! exit 0). Running today's walker over an older tree needs no census crate
//! there. The build-blocking consumer is `tools/freeze-safety-gate/build.rs`;
//! this binary is the manual runner.

use std::path::PathBuf;
use std::process::ExitCode;

/// The grammar `main` parses: printed on `-h`/`--help` (exit 0) and, to stderr,
/// on a usage error (exit 2).
const USAGE: &str = "\
usage: aterm-census [<checkout-root>] [--mainloop | --locks | --wasm | --scope | --lazy-init]

  <checkout-root>   the aterm checkout to walk (default: .)
  --mainloop        main-loop census only
  --locks           lock-order census only (alias: --lock-order)
  --wasm            wasm-process census only (opt-in: needs crates/aterm-wasm)
  --scope           scope-cardinality census only
  --lazy-init       lazy-init reentrancy census only (alias: --lazyinit)

No selection flag runs the default GUI pair (main-loop + lock-order); when
several are given the last one wins. Exit 0 = selected census(es) GREEN,
1 = obligations violated (diagnostics on stderr), 2 = usage error.
";

fn main() -> ExitCode {
    let mut root = PathBuf::from(".");
    // A selection flag picks one census (last wins); none = the GUI pair.
    let mut selected: Option<(bool, bool, bool, bool, bool)> = None;
    for arg in std::env::args().skip(1) {
        match arg.as_str() {
            "-h" | "--help" => {
                print!("{USAGE}");
                return ExitCode::SUCCESS;
            }
            "--mainloop" => selected = Some((true, false, false, false, false)),
            "--locks" | "--lock-order" => selected = Some((false, true, false, false, false)),
            "--wasm" => selected = Some((false, false, true, false, false)),
            "--scope" => selected = Some((false, false, false, true, false)),
            "--lazy-init" | "--lazyinit" => selected = Some((false, false, false, false, true)),
            // A dash-led word is a mistyped flag, never a checkout root.
            other if other.starts_with('-') => {
                eprint!("aterm-census: unknown option {other:?}\n\n{USAGE}");
                return ExitCode::from(2);
            }
            other => root = PathBuf::from(other),
        }
    }
    let (mainloop, locks, wasm, scope, lazy_init) =
        selected.unwrap_or((true, true, false, false, false));
    let mut ok = true;
    if mainloop {
        let outcome = aterm_census::run_mainloop_census(&root);
        eprint!("{}", outcome.log);
        ok &= outcome.ok;
    }
    if locks {
        let outcome = aterm_census::run_lock_order_census(&root);
        eprint!("{}", outcome.log);
        ok &= outcome.ok;
    }
    if wasm {
        let outcome = aterm_census::run_wasm_census(&root);
        eprint!("{}", outcome.log);
        ok &= outcome.ok;
    }
    if scope {
        let outcome = aterm_census::run_scope_census(&root);
        eprint!("{}", outcome.log);
        ok &= outcome.ok;
    }
    if lazy_init {
        let outcome = aterm_census::run_lazy_init_census(&root);
        eprint!("{}", outcome.log);
        ok &= outcome.ok;
    }
    if ok {
        ExitCode::SUCCESS
    } else {
        ExitCode::FAILURE
    }
}
