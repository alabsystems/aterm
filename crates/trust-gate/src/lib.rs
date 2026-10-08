// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! trust-gate — an EMPTY library. The crate exists for its build script
//! (`build.rs`), which refuses to compile this workspace with anything but the
//! Trust compiler; nothing here reaches a binary. `src/gate.rs` holds the
//! script's pure decision so it can be unit-tested (`cargo test -p trust-gate`)
//! and is compiled into the library ONLY under `cfg(test)` — the shipped rlib
//! carries no items at all.

#[cfg(test)]
#[path = "gate.rs"]
mod gate;

#[cfg(test)]
mod tests {
    use super::gate::{INSTALL_URL, TRUST_HOST, is_trust_compiler, refusal};

    /// Measured 2026-08-29 on the store build (acb08e761): the ONE line that
    /// separates a Trust `rustc -vV` from an upstream one.
    const TRUST_VV: &str = "rustc 1.99.0-dev (acb08e761 2026-08-21)\nbinary: rustc\ncommit-hash: acb08e7616e0808abb5d263a725a66366d3f8dfc\ncommit-date: 2026-08-21\nhost: aarch64-apple-darwin\nrelease: 1.99.0-dev\ntrust: 0.1.0\nLLVM version: 22.1.2\n";
    /// rustup stable the same day; Homebrew's differs only by a `(Homebrew)` suffix.
    const STABLE_VV: &str = "rustc 1.96.0 (ac68faa20 2026-05-25)\nbinary: rustc\ncommit-hash: ac68faa20c58cbccd01ee7208bf3b6e93a7d7f96\ncommit-date: 2026-05-25\nhost: aarch64-apple-darwin\nrelease: 1.96.0\nLLVM version: 22.1.2\n";
    /// An upstream NIGHTLY says `-dev` too — the release string is not a marker.
    const NIGHTLY_VV: &str = "rustc 1.99.0-nightly (0123456789 2026-08-20)\nbinary: rustc\ncommit-hash: 0123456789abcdef\ncommit-date: 2026-08-20\nhost: aarch64-apple-darwin\nrelease: 1.99.0-dev\nLLVM version: 22.1.2\n";
    const HOST: &str = "aarch64-apple-darwin";

    #[test]
    fn marker_is_the_trust_key_line_only() {
        assert!(is_trust_compiler(TRUST_VV));
        assert!(!is_trust_compiler(STABLE_VV));
        assert!(!is_trust_compiler(NIGHTLY_VV), "`-dev` alone must not pass");
        assert!(!is_trust_compiler(""), "an empty/failed -vV is not Trust");
        // The key must START the line: a commit message quoting the word does
        // not count, and neither does a `mistrust:` key.
        assert!(!is_trust_compiler("release: 1.99.0-dev (trust: fork)\n"));
        assert!(!is_trust_compiler("mistrust: 0.1.0\n"));
        assert!(is_trust_compiler("trust: 0.2.0-rc\n"));
    }

    #[test]
    fn trust_compiler_passes_silently() {
        assert_eq!(refusal(HOST, HOST, "rustc", TRUST_VV, true, None), None);
        assert_eq!(refusal(HOST, HOST, "rustc", TRUST_VV, false, None), None);
    }

    #[test]
    fn cross_build_is_never_gated() {
        // The release's x86_64-apple-darwin compat slice: upstream stable, cross
        // from the aarch64 host. TARGET != HOST ⇒ no refusal, whatever -vV says.
        assert_eq!(
            refusal(HOST, "x86_64-apple-darwin", "rustc", STABLE_VV, true, None,),
            None
        );
        assert_eq!(
            refusal(
                HOST,
                "x86_64-pc-windows-gnu",
                "rustc",
                STABLE_VV,
                false,
                None,
            ),
            None
        );
    }

    #[test]
    fn native_upstream_build_is_refused_with_targo_when_aterm_is_on_path() {
        let msg = refusal(HOST, HOST, "/opt/homebrew/bin/rustc", STABLE_VV, true, None)
            .expect("stable on the native triple must be refused");
        assert!(msg.contains("fix:  targo --unverified build"), "{msg}");
        assert!(
            msg.contains("cause     this cargo is not rustup's and ignores the pin"),
            "{msg}"
        );
        assert!(
            !msg.contains(INSTALL_URL),
            "targo, not the installer: {msg}"
        );
        assert!(
            msg.contains("/opt/homebrew/bin/rustc"),
            "names the compiler: {msg}"
        );
        assert!(
            msg.contains("rustc 1.96.0 (ac68faa20 2026-05-25)"),
            "quotes what it reported: {msg}"
        );
    }

    #[test]
    fn native_upstream_build_is_refused_with_installer_when_aterm_is_absent() {
        let msg = refusal(HOST, HOST, "rustc", NIGHTLY_VV, false, None)
            .expect("nightly on the native triple must be refused");
        assert!(msg.contains(INSTALL_URL), "{msg}");
        assert!(
            !msg.contains("targo --unverified build"),
            "installer, not targo: {msg}"
        );
    }

    #[test]
    fn without_aterm_a_host_trust_is_not_packaged_for_is_told_so() {
        for host in ["x86_64-apple-darwin", "x86_64-unknown-linux-gnu"] {
            let msg = refusal(host, host, "rustc", STABLE_VV, false, None)
                .expect("stable on the native triple must be refused");
            assert!(msg.contains(TRUST_HOST), "{msg}");
            assert!(msg.contains(host), "{msg}");
            assert!(!msg.contains(INSTALL_URL), "{msg}");
            assert!(!msg.contains("targo"), "{msg}");
            // With aterm there, a linked local seal can serve: the usual remedy stands.
            let msg = refusal(host, host, "rustc", STABLE_VV, true, None)
                .expect("stable on the native triple must be refused");
            assert!(msg.contains("targo --unverified build"), "{msg}");
        }
    }

    /// The cause comes from `RUSTUP_TOOLCHAIN`, which rustup's proxy sets for
    /// cargo and its build scripts while `RUSTC` stays a bare `rustc`: another
    /// name is an override, `trust` itself is a link that is not a Trust
    /// compiler, and unset is a cargo that is not rustup's.
    #[test]
    fn the_cause_and_its_fix_come_from_rustup_toolchain() {
        let refuse = |toolchain: Option<&str>| {
            refusal(HOST, HOST, "rustc", STABLE_VV, true, toolchain).expect("refused")
        };
        let stable = refuse(Some("stable-aarch64-apple-darwin"));
        assert!(
            stable.contains("`cargo +stable-aarch64-apple-darwin`"),
            "{stable}"
        );
        assert!(stable.contains("override"), "{stable}");
        assert!(
            stable.contains("fix:  targo --unverified build"),
            "{stable}"
        );
        let link = refuse(Some("trust"));
        assert!(link.contains("rustup's `trust` toolchain"), "{link}");
        assert!(!link.contains("override"), "{link}");
        assert!(link.contains("fix:  aterm pkg repair"), "{link}");
        assert!(!link.contains("doctor"), "{link}");
        for unset in [None, Some("")] {
            let plain = refuse(unset);
            assert!(plain.contains("not rustup's"), "{plain}");
            assert!(!plain.contains("override"), "{plain}");
        }
    }

    #[test]
    fn unrunnable_compiler_is_refused_not_excused() {
        // `-vV` could not run at all: still not a Trust compiler we could see.
        let msg = refusal(
            HOST,
            HOST,
            "rustc",
            "(could not run it: No such file)",
            true,
            None,
        )
        .expect("an unrunnable rustc is refused");
        assert!(msg.contains("could not run it"), "{msg}");
    }
}
