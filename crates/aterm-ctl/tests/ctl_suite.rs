// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The aterm-ctl integration files as modules of ONE test binary: each file
//! keeps its path and its own `#[test]`s, so `SPECS.md` names them as before.
//!
//! Sharing a process was checked, not assumed: no file below sets or removes an
//! environment variable of this process (each strips `ATERM_*` from its child's
//! command only), installs a hook or an allocator, or re-executes itself; each
//! makes its own scratch directory (`at-inbox-`, `at-tty-`) and binds its own
//! socket in it; and every descriptor a file opens is close-on-exec (the pty
//! pair from birth), so no child of one file holds another's open.
//!
//! `autotests = false` in `Cargo.toml`: a `tests/*.rs` file that is neither a
//! `mod` line here nor a `[[test]]` of its own NEVER COMPILES.
//!
//! Run one file's tests with
//! `targo --unverified test -p aterm-ctl --test ctl_suite -- <file>::`.

mod inbox_metadata;
mod terminal_output;
mod verb_help_offline;
