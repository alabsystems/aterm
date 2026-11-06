// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The release-only OpenSSL executable, built from a pinned upstream archive.

use std::ffi::OsString;
use std::path::{Path, PathBuf};

use super::place::{self, Scratch};
use super::rows::Archive;
use super::{Host, Machine, Spawn, Target};

fn step(program: &str, args: Vec<OsString>, src: &Path) -> Spawn {
    let mut cmd = super::bash::step(program, args, src);
    cmd.remove.extend([
        "OPENSSL_*".into(),
        "PERL".into(),
        "PERL5LIB".into(),
        "PERL5OPT".into(),
    ]);
    cmd
}

fn configure(src: &Path, prefix: &Path) -> Spawn {
    let mut at = OsString::from("--prefix=");
    at.push(prefix);
    step(
        aterm_uds::os_path!("perl"),
        std::iter::once(OsString::from("./Configure"))
            .chain(
                aterm_uds::host::upstream::OPENSSL_CONFIGURE_OPTIONS
                    .iter()
                    .map(OsString::from),
            )
            .chain(std::iter::once(at))
            .collect(),
        src,
    )
}

pub(super) fn build(
    source: &Archive,
    t: &Target,
    machine: &Machine,
    host: &dyn Host,
    say: &mut dyn FnMut(&str),
) -> Result<PathBuf, String> {
    let url = source.url_at(&t.version);
    let scratch = Scratch::new(&machine.tooling.join(".ubd"), "openssl-signing")?;
    let archive = place::fetch_checked(host, &scratch, &url, source.sha256, source.bytes, say)?;
    let src = scratch.fresh("src")?;
    // Upstream's git archive has a descriptive global PAX comment. The
    // extractor's explicit mode admits only descriptive keys, never global
    // path, size, link or sparse-file overrides, under the same byte caps.
    atpkg::extract::extract_tree(
        &archive,
        &src,
        atpkg::extract::Container::TarGz,
        place::UNPACKED_CAP,
        atpkg::extract::ExtractOptions {
            strip_components: 1,
            symlinks: atpkg::extract::SymlinkPolicy::InRoot,
            allow_global_pax: true,
        },
    )
    .map_err(|e| format!("{} did not unpack: {e}", archive.display()))?;
    place::settle_times(&src)?;
    say(&format!(
        "building OpenSSL {} for source signing",
        t.version
    ));
    let scratch = scratch.run_build(host, &configure(&src, &t.dir))?;
    let jobs = std::thread::available_parallelism().map_or(1, std::num::NonZeroUsize::get);
    let scratch = scratch.run_build(
        host,
        &step(
            super::bash::MAKE,
            vec![format!("-j{jobs}").into(), "build_sw".into()],
            &src,
        ),
    )?;
    place::lay_built(
        scratch,
        host,
        t,
        place::Built {
            row: "openssl-signing",
            url,
            program: &src.join("apps/openssl"),
            licence: &src.join("LICENSE.txt"),
            version_args: &["version"],
            answers: &|first| first.starts_with(&format!("OpenSSL {} ", t.version)),
        },
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_signing_build_has_no_external_modules_or_ambient_perl_configuration() {
        let cmd = configure(Path::new("/source"), Path::new("/prefix with space"));
        assert_eq!(cmd.program, Path::new(aterm_uds::os_path!("perl")));
        assert_eq!(cmd.cwd, Path::new("/source"));
        assert_eq!(
            cmd.args,
            [
                "./Configure",
                "no-shared",
                "no-dso",
                "no-module",
                "--prefix=/prefix with space"
            ]
            .map(OsString::from)
        );
        for key in [
            "OPENSSL_*",
            "PERL",
            "PERL5LIB",
            "PERL5OPT",
            "CC",
            "MAKEFLAGS",
        ] {
            assert!(cmd.remove.iter().any(|removed| removed == key), "{key}");
        }
    }
}
