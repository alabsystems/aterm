---
name: rust-in-aterm
description: Use before building, checking, testing, linting, formatting or running Rust (cargo, rustc, clippy, rustfmt) on a machine with aterm installed. Rust here is the Trust toolchain; this says how to use it and what its refusals mean.
---
<!-- aterm skill v2 — MANAGED FILE, rewritten by `aterm agents` on every install/update; remove this marker line and aterm will leave the file alone (reported as `foreign`) -->

# Rust in aterm: the Trust toolchain is the default

aterm installs the ALab verified Rust toolchain in `$ATPKG_BIN`, on PATH in every shell.
It is the default in every repository, including one whose `rust-toolchain.toml` pins a
stock channel: that pin moves only rustup's proxies (`cargo`, `rustc`, `rustfmt`,
`cargo-clippy`), never `targo`. Only `RUSTC` or `build.rustc` moves targo off trustc.

| instead of | type | what it is |
|---|---|---|
| `cargo` | `targo` | the build driver |
| `rustc` | `trustc` | the compiler; it proves as it compiles |
| `cargo clippy` | `targo tippy` | the linter |
| `rustfmt` / `cargo fmt` | `trustfmt` / `targo fmt` | the formatter |
| `rustdoc` | `trustdoc` | the doc tool |
| — | `ty`, `ay`, `clean` | model checker, SMT solver, theorem prover |

Replies use these names too; a reply that used stock Rust says so, and why.

## Name the lane

```sh
targo --unverified <cmd> …   # proof pipeline off; no proof claim
targo trust <cmd> …          # verified, fail-closed; --allow-l0-gaps reports gaps as warnings
```

A bare `targo build` is refused on purpose: add the lane.

## Measure, don't assume

- `aterm help rust` says which compiler this directory gets and why.
- `aterm pkg lane -- <command>` says whether a command runs stock Rust (exit 2 and the
  Trust spelling); a command with quotes goes on stdin (`aterm pkg lane -`).
- A bare `cargo …` in an aterm session prints its `targo` spelling, then runs; in a Trust
  source checkout a `cargo` or `rustc` that is not Trust is refused.
- Stock Rust that is genuinely needed (a cross target Trust lacks, a lane labelled `STOCK
  EXCEPTION` or `PUBLIC BOUNDARY`) runs with `ATERM_STOCK_REASON='<why>'` in front. A
  stock pin alone is not a reason. A project's own lane rule wins in that project.

## Errors that are not what they look like

- `'rustc' is not installed for the custom toolchain 'trust'`: a stale rustup link, fixed
  by `aterm pkg repair` (`aterm pkg doctor` says whether the machine is really blocked).
- `refuses to create an implicitly unverified artifact`: add `trust` or `--unverified`.
- `rustflags cannot set -Ztrust_verify` or `Host Rust cannot supply -Ztrust_verify`: a
  verification flag in `.cargo/config.toml`; `targo --unverified` applies it itself.
- `0 proved` after an error about a world-writable directory: `CARGO_TARGET_DIR` is under
  `/tmp`; use a directory under `$HOME`.
- `RUSTFLAGS` replaces a Trust-pinned repo's policy flags wholesale; a native `--target`
  strips the config from host units.
