# Two real whole-tree lane runs, cut to what `candidate_replay.rs` reads

The Trust lane on aterm 36b2c3df3 (tree 1d62da03bc10), one 14-core, 24 GiB Mac,
`--jobs 1`, under two provers:

| side | toolchain | prover (atpkg tree_root) |
|---|---|---|
| `base/` | seal 321aaeda7 (store build 9192, run from its rustup copy) | f6de73348782… |
| `candidate/` | trust main stage2 d36c551fa | 61a997f0b92d… |

Kept because no synthetic case gives what they do: the logs targo-trust really
wrote (a check SIGTERMed partway, one whose verification did not run, the seal's
own unfinished check, complete Coverage lines), and aterm-grid's real lost and
gained obligations.

What each library shows the comparison deciding:

* LOSS: aterm-grid (986 -> 926), aterm-scrollback (318 -> 282), smol_str (137 -> 119)
* NO-RESULT: aterm-core (its trustc ended by SIGTERM), aterm-render (verification
  did not run)
* BASE-UNDECIDED: aterm-census (the seal's own check ended by SIGTERM)
* NOT-ON-HOST: aterm-cli (refused up front for memory under both)
* OK: aterm-bits, astream-cap
* skipped by policy: aterm-release (no library), winit (a queued fork)

How the data differs from what the runs left: the plans keep only those libraries'
`crate`, `refused`, `memory` and `argv` lines; their filing section names the clean
tree measured and both record `crate-budget 21600`; logs are cut to the lines the
log reader reads (every `Summary:` and `Coverage:` line, transport rows, signal and
admission lines, census lines, `timed_out=`, and `[UNKNOWN]` budget rows shortened);
home directories are `/x`.

`obligations/{base,candidate}/aterm-grid@0.101.0.log.oblig.tsv`: the 234 flat-report
rows the two aterm-grid logs disagreed on — 147 proved only under the seal, 87 only
under trust main — as per-obligation records (function rebuilt from the source at
36b2c3df3; the side that did not prove a row records it `unrecorded`).
