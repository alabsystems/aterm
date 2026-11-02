---
name: rust-in-aterm
description: Use before building, checking, testing, linting, formatting or running Rust (cargo, rustc, clippy, rustfmt) on a machine with aterm installed. Rust here is the Trust toolchain; this says how to use it and what its refusals mean.
---
<!-- aterm skill v3 — MANAGED FILE, rewritten by `aterm agents` on every install/update; remove this marker line and aterm will leave the file alone (reported as `foreign`) -->

# Rust in aterm

Trust (on PATH from `$ATPKG_BIN`) is the default everywhere: `targo` is cargo, `trustc`
rustc, `tippy` (`targo tippy`) clippy, `trustfmt` (`targo fmt`) rustfmt, `trustdoc` rustdoc.
Use these names in replies. A stock `rust-toolchain.toml` pin never moves `targo`; only
`RUSTC`, `CARGO_BUILD_RUSTC` or `build.rustc` does.

## Name the lane

```sh
targo --unverified <cmd> …   # no proof claim
targo trust <cmd> …          # verified, fail-closed; --allow-l0-gaps reports gaps as warnings
```

A bare `targo build` is refused. `build`, `check` and `test` take either lane;
`run`, `doc`, `bench` and `install` only `--unverified`; `fmt` and `tippy` take none.

## Measure

- `aterm help rust` measures which compiler this directory gets. Where it or `aterm pkg
  which targo` says `unavailable on <target>`, Trust has no build here: use stock `cargo`.
- A bare `cargo` prints its `targo` spelling, then runs; in a Trust source checkout a
  non-Trust `cargo` or `rustc` exits 2. `aterm pkg lane -- <command>` checks without
  running: exit 2 and the Trust spelling for stock Rust. Pipe a line with quotes to
  `aterm pkg lane -`.
- Stock Rust for a cross target Trust lacks, or a `STOCK EXCEPTION`/`PUBLIC BOUNDARY` lane,
  runs with `ATERM_STOCK_REASON='<why>'` in front; your reply says why. A stock pin is no
  reason; a project's own lane rule wins.

## Misleading errors

- `'rustc' is not installed for the custom toolchain 'trust'`: a stale link:
  `aterm pkg repair`, never a toolchain rebuild.
- `refuses to create an implicitly unverified artifact`: name the lane.
- `rustflags cannot set -Ztrust_verify` or `Host Rust cannot supply -Ztrust_verify`: the
  flag is in `.cargo/config.toml`; remove it, as `targo --unverified` sets it itself.
- `0 proved` after a world-writable directory error: move the target dir out of `/tmp`.
- In a Trust-pinned repo never set `RUSTFLAGS` or pass a native `--target`: both bypass
  its config flags.
