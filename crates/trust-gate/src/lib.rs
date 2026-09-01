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
    use super::gate::{INSTALL_URL, is_trust_compiler, refusal, tree_pins_trust};

    /// Measured 2026-08-29 on the store build (acb08e761): the ONE line that
    /// separates a Trust `rustc -vV` from an upstream one.
    const TRUST_VV: &str = "rustc 1.99.0-dev (acb08e761 2026-08-21)\nbinary: rustc\ncommit-hash: acb08e7616e0808abb5d263a725a66366d3f8dfc\ncommit-date: 2026-08-21\nhost: aarch64-apple-darwin\nrelease: 1.99.0-dev\ntrust: 0.1.0\nLLVM version: 22.1.2\n";
    /// rustup stable the same day; Homebrew's differs only by a `(Homebrew)` suffix.
    const STABLE_VV: &str = "rustc 1.96.0 (ac68faa20 2026-05-25)\nbinary: rustc\ncommit-hash: ac68faa20c58cbccd01ee7208bf3b6e93a7d7f96\ncommit-date: 2026-05-25\nhost: aarch64-apple-darwin\nrelease: 1.96.0\nLLVM version: 22.1.2\n";
    /// An upstream NIGHTLY says `-dev` too — the release string is not a marker.
    const NIGHTLY_VV: &str = "rustc 1.99.0-nightly (0123456789 2026-08-20)\nbinary: rustc\ncommit-hash: 0123456789abcdef\ncommit-date: 2026-08-20\nhost: aarch64-apple-darwin\nrelease: 1.99.0-dev\nLLVM version: 22.1.2\n";
    const HOST: &str = "aarch64-apple-darwin";
    /// The dev workspace's pin (rust-toolchain.toml) — the gate enforces here.
    const TRUST_PIN: &str = "[toolchain]\nchannel = \"trust\"\n";
    /// What publish/transforms.sh swaps in for the public snapshot.
    const STOCK_PIN: &str = "[toolchain]\nchannel = \"1.97.1\"\nprofile = \"minimal\"\n";

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
        assert_eq!(
            refusal(HOST, HOST, "rustc", TRUST_VV, true, Some(TRUST_PIN)),
            None
        );
        assert_eq!(
            refusal(HOST, HOST, "rustc", TRUST_VV, false, Some(TRUST_PIN)),
            None
        );
    }

    #[test]
    fn cross_build_is_never_gated() {
        // The release's x86_64-apple-darwin compat slice: upstream stable, cross
        // from the aarch64 host. TARGET != HOST ⇒ no refusal, whatever -vV says.
        assert_eq!(
            refusal(
                HOST,
                "x86_64-apple-darwin",
                "rustc",
                STABLE_VV,
                true,
                Some(TRUST_PIN)
            ),
            None
        );
        assert_eq!(
            refusal(
                HOST,
                "x86_64-pc-windows-gnu",
                "rustc",
                STABLE_VV,
                false,
                Some(TRUST_PIN)
            ),
            None
        );
    }

    #[test]
    fn native_upstream_build_is_refused_with_doctor_when_aterm_is_on_path() {
        let msg = refusal(
            HOST,
            HOST,
            "/opt/homebrew/bin/rustc",
            STABLE_VV,
            true,
            Some(TRUST_PIN),
        )
        .expect("stable on the native triple must be refused");
        assert!(msg.contains("aterm pkg doctor"), "{msg}");
        assert!(
            !msg.contains(INSTALL_URL),
            "doctor, not the installer: {msg}"
        );
        assert!(
            msg.contains("/opt/homebrew/bin/rustc"),
            "names the compiler: {msg}"
        );
        assert!(
            msg.contains("rustc 1.96.0 (ac68faa20 2026-05-25)"),
            "quotes what it reported: {msg}"
        );
        assert!(msg.contains("trust:"), "names the missing marker: {msg}");
    }

    #[test]
    fn native_upstream_build_is_refused_with_installer_when_aterm_is_absent() {
        let msg = refusal(HOST, HOST, "rustc", NIGHTLY_VV, false, Some(TRUST_PIN))
            .expect("nightly on the native triple must be refused");
        assert!(msg.contains(INSTALL_URL), "{msg}");
        assert!(
            !msg.contains("aterm pkg doctor"),
            "installer, not doctor: {msg}"
        );
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
            Some(TRUST_PIN),
        )
        .expect("an unrunnable rustc is refused");
        assert!(msg.contains("could not run it"), "{msg}");
    }

    /// THE PUBLIC SNAPSHOT'S LANE. publish/transforms.sh swaps the tree's pin
    /// to stock 1.97.1 and publish/DECISIONS.md's public stock-Rust gate then
    /// builds it with upstream rustc under anonymous git — the gate must stand
    /// down THERE and only there, on the committed pin, never on an env var.
    #[test]
    fn a_tree_pinned_to_stock_rust_is_the_public_snapshot_and_is_not_gated() {
        assert!(tree_pins_trust(Some(TRUST_PIN)));
        assert!(!tree_pins_trust(Some(STOCK_PIN)));
        assert_eq!(
            refusal(HOST, HOST, "rustc", STABLE_VV, true, Some(STOCK_PIN)),
            None
        );
        // the dev tree still refuses exactly as before
        assert!(refusal(HOST, HOST, "rustc", STABLE_VV, true, Some(TRUST_PIN)).is_some());
    }

    /// FAIL-CLOSED: no pin file, an unreadable one, or one with no channel
    /// line enforces — only an explicit non-trust channel stands the gate
    /// down. Comments and quoting do not confuse the read.
    #[test]
    fn a_missing_or_channelless_pin_enforces_and_comments_do_not_confuse_it() {
        assert!(tree_pins_trust(None));
        assert!(tree_pins_trust(Some("")));
        assert!(tree_pins_trust(Some(
            "[toolchain]\nprofile = \"minimal\"\n"
        )));
        assert!(tree_pins_trust(Some(
            "# channel = \"1.97.1\"\nchannel = \"trust\"\n"
        )));
        assert!(!tree_pins_trust(Some("channel = \"1.97.1\" # was trust\n")));
        assert!(refusal(HOST, HOST, "rustc", STABLE_VV, false, None).is_some());
    }
}
