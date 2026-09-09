---
name: rust-in-aterm
description: Use whenever you are about to build, check, test, lint, format or run Rust — cargo, rustc, clippy, rustfmt, cargo test, a Cargo.toml, a rust-toolchain.toml — on a machine that has aterm installed. Rust here means the Trust toolchain (targo/trustc/tippy); this skill says how to use it, what its refusals mean, and how to tell which compiler a directory actually gets.
---
<!-- aterm skill v1 — MANAGED FILE, rewritten by `aterm agents` on every install/update; remove this marker line and aterm will leave the file alone (reported as `foreign`) -->

# Rust in aterm: the default is the Trust toolchain

aterm installs the ALab **verified** Rust toolchain beside stock Rust. On this
machine, **use it by default** — the owner's instruction, verbatim: *"USE TRUST
TOOLCHAIN NOT RUST."* Stock `cargo`/`rustc` is never blocked, but it is the
exception: if you run it, say in your reply that you did and why.

## The tools

| you would type | type this instead | what it is |
|---|---|---|
| `cargo` | `targo` | the build driver — the Trust cargo |
| `rustc` | `trustc` | the compiler; it proves as it compiles |
| `cargo clippy` | `tippy` | the linter |
| `rustfmt` / `cargo fmt` | `trustfmt` / `targo fmt` | the formatter |
| `rustdoc` | `trustdoc` | the doc tool |
| — | `ty`, `ay`, `clean` | model checker · SMT solver · theorem prover |

They live in `$ATPKG_BIN` and are on PATH in every shell. Resolve `targo`
through rustup's `trust` channel rather than by path — a channel points at a
sealed artifact, a path can point at a build tree that is being emptied:

```sh
TARGO="$(dirname "$(rustup which cargo --toolchain trust)")/targo"
"$TARGO" --unverified --version     # a real targo answers; a cargo-in-disguise rejects the flag
```

## Name the lane — a bare `targo build` is refused ON PURPOSE

`targo` will not choose a verification lane for you. Two lanes, both explicit:

```sh
targo trust <cmd> …          # VERIFIED: fail-closed by default; --allow-l0-gaps = advisory survey;
                             # writes an authenticated per-unit proof report (--report-dir)
targo --unverified <cmd> …   # UNVERIFIED: proof pipeline off, one non-suppressible notice, no proof claim
```

The refusal you get from a bare `targo build` is that rule, not a broken tool.
Do not fall back to stock `cargo` because of it — add the lane.

Inside an aterm session, typing a bare `cargo …` prints the `targo` spelling of
your exact command in both lanes and then runs upstream. That printout is the
answer; `ATERM_REROUTE_QUIET=1` silences it if you have decided.

## Before the first build in a project: measure, do not guess

```sh
aterm help rust      # which toolchain this directory gets, and why — measured now
```

It prints which toolchain won and which candidates were refused, what `rustc`
on this PATH answers to `--print sysroot`, whether `rust-toolchain.toml` pins a
channel, whether `.cargo/config.toml` switches verification off, and which
lane the project's own instructions ask for. If a project's `CLAUDE.md` or
`AGENTS.md` names a different toolchain, the project wins — say so when you
use it.

## Errors that are not what they look like

* `error: 'rustc' is not installed for the custom toolchain 'trust'` — a
  **stale rustup link**, not a blocked machine. `targo`, `trustc`, `tippy` in
  `$ATPKG_BIN` keep working. Run `aterm pkg doctor` (names the seam), then
  `aterm pkg repair`. **Never** rebuild a toolchain from source to answer it.
* `targo check refuses to create an implicitly unverified artifact` — you
  omitted the lane. Add `trust` or `--unverified`.
* `[host] rustflags cannot set -Ztrust_verify during a verified Targo
  invocation` — the repo's `.cargo/config.toml` carries a stock-cargo scoping
  posture that `targo trust` refuses by design. Targo scopes per unit itself;
  that config belongs on a measurement command line, not in the file.
* A verification run that ends in `transport:missing-json` / `0 proved` /
  `verification did not run` after an `error:` line about a **world-writable
  directory** — you pointed `CARGO_TARGET_DIR` or `--out-dir` under `/tmp`.
  Use a directory under `$HOME`. Read the `error:` lines above the summary.

## Never

* Never build in a checkout another session is working in — `git worktree
  add` your own; it gets its own `target/`.
* Never assign `RUSTFLAGS` in a Trust-pinned repo (it replaces the repo's
  policy wholesale); never pass an explicit `--target` (it strips the config
  from host units).
* Never report the machine as blocked before `aterm pkg doctor`.

Ask the compiler, never a document: `trustc -Vv`, `targo --unverified --version`.
