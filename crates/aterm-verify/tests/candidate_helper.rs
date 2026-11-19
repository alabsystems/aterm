// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Development-only standalone atpkg ([`atpkg::cli::standalone_entry`], the whole `main` of
//! atpkg's own `[[bin]]`), owned by this package: the released-shim tests hand it to a candidate
//! shim as its helper, so they run a binary of their own package, which every compile of this
//! package's integration tests builds (`targo test`, verify's `--tests`; a `--lib` compile alone
//! does not). The `candidate-shim` check itself builds atpkg's `[[bin]]`.

fn main() -> std::process::ExitCode {
    atpkg::cli::standalone_entry()
}
