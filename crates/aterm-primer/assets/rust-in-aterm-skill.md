---
name: rust-in-aterm
description: Use whenever you are about to build, check, test, lint, format or run Rust — cargo, rustc, clippy, rustfmt, cargo test, a Cargo.toml, a rust-toolchain.toml — on a machine that has aterm installed. Rust here means the Trust toolchain (targo/trustc/tippy); this skill says how to use it, what its refusals mean, and how to tell which compiler a directory actually gets.
---
<!-- aterm skill v1 — MANAGED FILE, rewritten by `aterm agents` on every install/update; remove this marker line and aterm will leave the file alone (reported as `foreign`) -->

# Rust in aterm: the default is the Trust toolchain

aterm installs the ALab **verified** Rust toolchain beside stock Rust, and on this
machine it is **the default**. Stock `cargo`/`rustc` is not blocked and still runs; a
reply that used it says so, and why.

The default holds **in every repository, including one whose `rust-toolchain.toml`
pins a stock channel**. The spellings to write — in a command, and in a prompt for
another agent — are the right column below; `cargo`, `cargo +<channel>`, clippy or
rustfmt there send that agent to stock Rust.

## A stock pin moves only rustup's proxies

A `rust-toolchain.toml` saying `channel = "1.97.1"` is read by **rustup**, so it
moves rustup's proxies — `cargo`, `rustc`, `rustfmt`, `cargo-clippy` — and nothing
else. `targo`, `tippy`, `trustfmt` and `trustdoc` are not rustup proxies: in a crate
pinning 1.97.1, `targo` still runs the store's `trustc`, `tippy-driver`, `trustfmt`
and `trustdoc`. What DOES move targo off trustc is an explicit `RUSTC` or
`build.rustc`; `aterm help rust` checks both for the directory you are in (its
`targo here` row). So in a stock-pinned
repo you build, lint and format exactly as anywhere else: `targo --unverified <cmd>` /
`targo trust <cmd>`, `targo tippy`, `targo fmt`.

## The tools

| you would type | type this instead | what it is |
|---|---|---|
| `cargo` | `targo` | the build driver — the Trust cargo |
| `rustc` | `trustc` | the compiler; it proves as it compiles |
| `cargo clippy` | `targo tippy` / `tippy` | the linter |
| `rustfmt` / `cargo fmt` | `trustfmt` / `targo fmt` | the formatter |
| `rustdoc` | `trustdoc` | the doc tool |
| — | `ty`, `ay`, `clean` | model checker · SMT solver · theorem prover |

The right column is the tool's name — use it in your replies too (`tippy`, not
"clippy"); the stock name is for a run that really used stock Rust, said with why.

They live in `$ATPKG_BIN` and are on PATH in every shell. That is the copy
`aterm pkg update` keeps current, and it keeps working even when rustup's `trust`
link is stale. `rustup which` is a weaker way to find `targo`: that entry may not be
the managed store (`aterm pkg doctor` warns when it is not), may point into a live
build tree, and yields no path when missing. Ask the tool, then ask the directory:

```sh
targo --unverified --version     # a real targo answers; a cargo-in-disguise rejects the flag
aterm help rust                  # which toolchain THIS directory gets, and why
```

## Name the lane — a bare `targo build` is refused ON PURPOSE

`targo` will not quietly pick a verification lane for a build. Two lanes, both
explicit:

```sh
targo trust <cmd> …          # VERIFIED: fail-closed by default; --allow-l0-gaps leaves verifier gaps
                             # as warnings; authenticated per-unit proof report (--report-dir)
targo --unverified <cmd> …   # UNVERIFIED: proof pipeline off; warns that the run carries no proof
                             # claim, and `-q` does not silence that
```

The refusal you get from a bare `targo build` is that rule, not a broken tool:
adding the lane is the whole fix, and stock `cargo` is not needed for it.

Which verbs take a lane is not uniform, so measure rather than assume: `build`,
`check` and `test` REFUSE a bare call; `run`, `doc`, `bench`, `rustc`, `rustdoc` and
`fix` have no verified lane at all, so a bare one proceeds unverified and says so
(*nobody was asked*), while a bare `package`, `install` or `publish` is refused —
`targo --unverified <verb>` for all of them; and `fmt`, `tippy`, `metadata`, `tree`,
`clean`, `update` take no lane — `--unverified` there is refused (*`--unverified` is
valid only for a Targo compilation command*), so run those bare.

Inside an aterm session, typing a bare `cargo …` prints the `targo` spelling targo
accepts for your exact command — both lanes for `build`/`check`/`test`, `targo
--unverified …` for `run`/`doc`/`install`…, plain `targo …` for `metadata`/`tree`/
`fmt`…, `targo tippy …` for `cargo clippy` — and then runs upstream. `cargo +<stock
channel> …` (and `rustfmt +1.97.1 …`, `rustc +stable …`) prints the same spelling
and that the channel moves only rustup's proxies, then runs as named. That
printout is the answer; `[reroute] announce = false` in aterm.toml silences it if
you have decided.

If a guard in your setup **refuses a stock command before it runs**, the refusal
carries the Trust spelling of it: retype it that way. Only when stock Rust is
genuinely required — a cross target the Trust sysroot does not carry, a check that
exists to test the public stock-Rust build — put `ATERM_STOCK_REASON='<why>'` in
front of the command and say why in your reply. A stock pin alone is not such a
reason. To check a command before you run it or write it into another agent's
prompt, `aterm pkg lane -- <command>` reads it the way the shell would: exit 2 and
the Trust spelling when it runs stock Rust, exit 0 when it does not. `--` joins its
words with spaces, so a command with quotes goes on stdin instead:
`aterm pkg lane - <<'EOF'`, the command, `EOF`.

## Before the first build in a project: `aterm help rust` measures it

```sh
aterm help rust      # which toolchain this directory gets, and why — measured now
```

It prints which toolchain aterm's own gates pick and which candidates were
refused, where a bare `targo` on this PATH really runs (followed through an
atpkg shim) and whether that is a different directory or build from the gates'
pick, which compiler that `targo` runs HERE and whether anything (`RUSTC`,
`build.rustc`) moves it, what `targo fmt` and `targo tippy` answer here, what
`rustc` on this PATH answers to `--print sysroot`, whether `rust-toolchain.toml`
pins a channel, and whether `.cargo/config.toml` switches verification off.

It reads no instruction file. A project's `CLAUDE.md` or `AGENTS.md` may name a
lane (`targo --unverified` versus `targo trust`) — follow that. If it names stock
Rust for a specific job, that job is a reason you can state (above); if it only
says "the repo pins 1.97.1", it is describing rustup's proxies, and you still
build with targo.

## Errors that are not what they look like

* `error: 'rustc' is not installed for the custom toolchain 'trust'` — a
  **stale rustup link**, not a blocked machine. `targo`, `trustc`, `tippy` in
  `$ATPKG_BIN` keep working. `aterm pkg repair` fixes it; a toolchain rebuild
  from source is not needed for it.
* `targo check refuses to create an implicitly unverified artifact` — you
  omitted the lane. Add `trust` or `--unverified`.
* `[host] rustflags cannot set -Ztrust_verify during a verified Targo
  invocation` — the repo's `.cargo/config.toml` carries a stock-cargo scoping
  posture that `targo trust` refuses by design. Targo scopes per unit itself;
  that config belongs on a measurement command line, not in the file.
* `Host Rust cannot supply -Ztrust_verify: project configuration supplies
  requirements, not activation, …` — a `.cargo/config.toml` carries
  `-Ztrust-verify=off`, which a targo from Trust main refuses in any table.
  Move the off-switch onto the command line of the build that needs it
  (`targo --unverified` applies it itself).
* A verification run that ends in `transport:missing-json` / `0 proved` /
  `verification did not run` after an `error:` line about a **world-writable
  directory** — you pointed `CARGO_TARGET_DIR` or `--out-dir` under `/tmp`.
  Use a directory under `$HOME`. Read the `error:` lines above the summary.

## Things that bite

* A checkout another session is working in shares its `target/` and working
  tree with you; `git worktree add` gives you your own, with its own `target/`.
* `RUSTFLAGS` in a Trust-pinned repo replaces the repo's policy flags wholesale,
  and an explicit `--target` strips the config from host units.
* `aterm pkg doctor` says whether the machine is actually blocked, and names the
  fix when it is.

The compiler answers for itself: `trustc -Vv`, `targo --unverified --version`.
