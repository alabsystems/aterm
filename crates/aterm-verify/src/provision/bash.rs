// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Provisioning builder for the pinned GNU `bash` 5.3 from source and its official patches.

use std::ffi::OsString;
use std::path::{Path, PathBuf};

use super::place::{self, Scratch};
use super::rows::Archive;
use super::{Host, Machine, Spawn, Target};

/// The OS's `patch`, by absolute path.
pub(super) const PATCH: &str = aterm_uds::os_path!("patch");

/// The Command Line Tools' `make`, by absolute path.
pub(super) const MAKE: &str = aterm_uds::os_path!("make");

/// The OS's `sh`, by absolute path: it runs `./configure`.
pub(super) const SH: &str = aterm_uds::os_path!("sh");

/// The `PATH` `./configure` and `make` search: the OS's and the Command Line Tools'
/// programs only, so the build takes no compiler, header or library from Homebrew.
pub(super) const BUILD_PATH: &str = "/usr/bin:/bin:/usr/sbin:/sbin";

/// What `./configure` takes besides the prefix: bash's own malloc off, and no
/// translations, so it links no libintl. [`BUILD_PATH`] cannot keep Homebrew's gettext
/// out on an Intel Mac, where it lives in `/usr/local`, on the compiler's own search path.
const OPTIONS: [&str; 2] = ["--without-bash-malloc", "--disable-nls"];

/// The words `./configure` runs with, for a bash that will live in `prefix`.
fn configure_args(prefix: &Path) -> Vec<OsString> {
    let mut at = OsString::from("--prefix=");
    at.push(prefix);
    [OsString::from("./configure"), at]
        .into_iter()
        .chain(OPTIONS.map(OsString::from))
        .collect()
}

/// A step of the build: `program` with `args` in `src`, on [`BUILD_PATH`], with the
/// variables that would steer a C or Rust build removed ([`super::fish::removed`]).
fn step(program: &str, args: Vec<OsString>, src: &Path) -> Spawn {
    Spawn {
        program: PathBuf::from(program),
        args,
        cwd: src.to_path_buf(),
        set: vec![("PATH".to_string(), BUILD_PATH.into())],
        remove: super::fish::removed(),
    }
}

/// Fetches the source and each patch, each held to its sha256; applies the patches in
/// order; builds, checks and installs `bash` into `t`.
pub(super) fn build(
    source: &Archive,
    patches: &[Archive],
    t: &Target,
    machine: &Machine,
    host: &dyn Host,
    say: &mut dyn FnMut(&str),
) -> Result<PathBuf, String> {
    let version = t.version.as_str();
    let url = source.url_at(version);
    let scratch = Scratch::new(&machine.tooling, "bash")?;
    let tarball = place::fetch_checked(host, &scratch, &url, source.sha256, source.bytes, say)?;
    let src = place::unpack(
        &scratch,
        &tarball,
        atpkg::extract::Container::TarGz,
        1,
        place::UNPACKED_CAP,
        "src",
    )?;
    // Settled before the patches, so what a patch changes comes out newer than what it
    // is generated into, and `make` remakes exactly that (`y.tab.c` from a patched
    // `parse.y`); a patch that changes both lays the generated file second, as 016 does
    // `configure.ac` and `configure`.
    place::settle_times(&src)?;
    for patch in patches {
        let patch_url = patch.url_at(version);
        let file =
            place::fetch_checked(host, &scratch, &patch_url, patch.sha256, patch.bytes, say)?;
        // `-f` asks nothing (a question would wait on the terminal); a hunk that does not
        // apply still fails the run.
        let words = ["-p0", "-s", "-f", "-i"].map(OsString::from);
        let apply = step(PATCH, [&words[..], &[file.into_os_string()]].concat(), &src);
        host.run(&apply)
            .map_err(|e| format!("{patch_url} did not apply: {e}"))?;
    }
    say(&format!(
        "building bash {version}: ./configure {} && make bash, a few minutes",
        OPTIONS.join(" ")
    ));
    let scratch = scratch
        .run_build(host, &step(SH, configure_args(&t.dir), &src))
        .map_err(|e| format!("bash {version}'s ./configure failed: {e}"))?;
    let scratch = scratch
        .run_build(host, &step(MAKE, vec!["bash".into()], &src))
        .map_err(|e| format!("the bash {version} build failed: {e}"))?;
    place::lay_built(
        scratch,
        host,
        t,
        place::Built {
            row: "bash",
            url,
            program: &src.join("bash"),
            licence: &src.join("COPYING"),
            answers: &|first| first.starts_with(&format!("GNU bash, version {version}(")),
        },
    )
}
