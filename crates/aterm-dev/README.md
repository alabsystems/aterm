<!-- SPDX-License-Identifier: Apache-2.0 -->
<!-- Copyright 2026 Andrew Yates -->

# aterm-dev

One discoverable, AI-friendly CLI front door to **all** of the aterm project's
dev/ops utility scripts.

`aterm-dev` does not reimplement any of the underlying logic (cargo-deny, Kani,
codex, the release cutter, …). Each subcommand resolves the workspace root and
execs the existing, battle-tested tool — `ship` forwards to the `cargo ship`
alias (crate `aterm-release`), the rest to their repo scripts — forwarding
every extra argument and propagating the exit code. The value it adds is a
single, grouped, polished `--help` so a human or an AI can discover the
available operational levers at a glance.

## Usage

```text
aterm-dev <command> [args...]
aterm-dev --help        # grouped overview of every command
aterm-dev --version     # workspace version
aterm-dev <command> --help   # forwards to that tool's own help
```

An unknown subcommand prints `aterm-dev: unknown command <x> (try --help)` to
stderr and exits `2`. A missing or non-executable script prints a clear error
and exits `1`.

## Commands

### Package & Release

| Command | Wraps | Description |
| --- | --- | --- |
| `ship` | `cargo ship` (crate `aterm-release`) | Release cutter passthrough: `cut` / `status` / `verify` / `yank` — the whole build → sign → publish → verify pipeline is `aterm-dev ship cut` (see `docs/RELEASING.md`) |

### Quality & Verify

| Command | Wraps | Description |
| --- | --- | --- |
| `visual-judge`  | `tools/visual-judge/visual-judge.sh` | LLM-as-Judge visual loop over aterm introspection |
| `audit`         | `scripts/audit-supply-chain.sh`      | Supply-chain audit via cargo-deny |
| `verify-proofs` | `scripts/verify-kani-proofs.sh`      | Opt-in Kani formal-proof verification |

### Setup

| Command | Wraps | Description |
| --- | --- | --- |
| `setup-trust` | `scripts/setup-trust-mc.sh` | Stand up the trust-mc checker |

## Examples

```sh
aterm-dev visual-judge --judges claude
aterm-dev ship cut --dry-run
aterm-dev audit --help
```
