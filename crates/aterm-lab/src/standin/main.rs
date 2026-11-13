// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! `aterm-lab-standin`: the stand-in vendor agent `lab harness-proof` lays into its scratch atpkg
//! store as two builds (`<prefix>/store/<program>/<build>/bin/<cmd>`), so a harness runs it as its
//! child exactly as it runs the vendor's CLI, and no vendor byte, account or server is involved
//! (`docs/DESIGN-harness-repos-2026-10-24.md` §3.7, §9 step 2). Dev-only: never packed or shipped.
//!
//! It plays the vendor its name says (`argv[0]`'s file name; `unix::Dialect`); Muse Code is the one
//! dialect so far. As Muse Code it keeps a session the way the real one does, so both the harness
//! and aterm's built-in Muse lanes read it as Muse Code: a `.session.lock` naming its pid, held open
//! for its life, beside a `session.jsonl` whose records are Muse Code's (`session.started`, the
//! process's `runtime.session.route_facts`, `runtime.session.metadata`, a run's `started` and
//! `terminal` records, and `session.end` with `exit_reason: clean` on a clean end), under
//! `$XDG_DATA_HOME/muse/sessions/YYYY/MM/DD/<uuid>/` (else `$HOME/.local/share`); `resume <uuid>`
//! reopens that session. It draws Muse Code's frame (aterm-phase's Muse reader: the `Muse Code
//! <version>` banner, a vendor-shaped composer and a footer two spaces in), whose footer says
//! what aterm's screen read judges a leg by: `standin · <version> · pid <pid> · session <uuid>
//! · start <n>`, `<n>` counting the session's starts.
//!
//! Input is read raw, only when `poll` says the terminal is readable. Enter submits the composer:
//! `/crash` kills the process with `SIGKILL` and no end record, `/quota` ends a Kimi turn on its
//! measured quota wall, `/exit <n>` records a clean end and exits `<n>`, and any other text is one
//! turn that ends at once. `SIGTERM` is a clean end (143); `SIGHUP` ends it by `SIGHUP`; `SIGINT` is said on the
//! screen and nothing more. `--help` and `--version` print and exit 0 (a harness's build probe).
//! Every start, probe, signal and end is one line in `$ATERM_LAB_STANDIN_LOG`, when set.
//!
//! It is a Unix program (a terminal's raw mode, signals, `poll`), as `lab harness-proof`, which
//! lays it, is a macOS one; off Unix it says so and exits 2.

#[cfg(unix)]
mod unix;

#[cfg(unix)]
fn main() -> std::process::ExitCode {
    unix::main()
}

#[cfg(not(unix))]
fn main() -> std::process::ExitCode {
    eprintln!("aterm-lab-standin: unsupported here (lab harness-proof runs it on macOS)");
    std::process::ExitCode::from(2)
}
