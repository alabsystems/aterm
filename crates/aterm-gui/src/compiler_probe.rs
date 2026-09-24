// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

// Compiler-provenance probing, shared between BUILD time and TEST time: `build.rs`
// `include!`s this file to parse `$RUSTC -vV` and classify the compiler flavor
// ('r' = upstream Rust, 't' = the Trust fork), and `main.rs` mounts it `#[cfg(test)]`
// so the exact same parsing/classification code is unit-tested against real `-vV`
// fixtures under `cargo test`. Pure std, no I/O — the caller runs the compiler and
// reads the env; these functions only classify strings, so they are deterministic
// and testable. (Plain `//` comments, not `//!`: include! splices this file mid-
// build.rs, where inner doc comments are illegal.)

/// The `rustc -vV` fields the provenance stamp needs. Every field degrades to
/// `"unknown"` rather than failing the build (mirrors `build.rs`'s best-effort git
/// probes), so a hostile/odd toolchain can't brick compilation.
pub struct RustcVv {
    /// The full first line, e.g. `rustc 1.96.0 (ac68faa20 2026-05-25) (Homebrew)`.
    pub version_line: String,
    /// The full `commit-hash:` value (40 hex; some distro builds say "unknown").
    pub commit: String,
    /// The `host:` triple, e.g. `aarch64-apple-darwin`.
    pub host: String,
    /// Trust's OWN version — the `trust:` line the Trust toolchain prints (e.g.
    /// `0.1.0`), else the `(trustc <version>)` parenthetical the `trustc`-named
    /// entry adds to its first line. `""` when the compiler reports neither:
    /// upstream rustc, or a Trust build predating the marker. This is NOT the
    /// release token of the first line (`1.99.0-dev`): that is the RUST release
    /// Trust is compatible with, kept rustc-shaped because cargo, targo and every
    /// version-sniffing build script parse it — which is exactly why a surface
    /// that printed only that token read as "built by Rust 1.99".
    pub trust_version: String,
}

/// The `(trustc <version>)` parenthetical of a `-vV` first line, if present —
/// `rustc 1.99.0-dev (2b118046a 2026-07-29) (trustc 0.1.0)` → `0.1.0`. The
/// `trustc`-named entry prints it; the `rustc`-named entry cargo drives does not.
fn trustc_parenthetical(first_line: &str) -> Option<String> {
    let rest = first_line.split_once("(trustc ")?.1;
    let version = rest.split(')').next()?.trim();
    (!version.is_empty()).then(|| version.to_string())
}

/// Parse `rustc -vV` output. Tolerant: missing lines yield `"unknown"`, never a panic.
pub fn parse_rustc_vv(vv: &str) -> RustcVv {
    let field = |key: &str| {
        vv.lines()
            .find_map(|l| l.strip_prefix(key))
            .map(|v| v.trim().to_string())
            .filter(|v| !v.is_empty())
    };
    RustcVv {
        version_line: vv
            .lines()
            .next()
            .map(|l| l.trim().to_string())
            .filter(|l| !l.is_empty())
            .unwrap_or_else(|| "unknown".into()),
        commit: field("commit-hash:").unwrap_or_else(|| "unknown".into()),
        host: field("host:").unwrap_or_else(|| "unknown".into()),
        trust_version: field("trust:")
            .or_else(|| vv.lines().next().and_then(trustc_parenthetical))
            .unwrap_or_default(),
    }
}

/// Classify the compiler flavor: `"r"` (upstream Rust) or `"t"` (the Trust fork).
///
/// EVIDENCE ONLY. There is deliberately no override — this is a provenance
/// claim the project ships in the About panel and `aterm ctl version`, and a
/// claim that can be flipped from the build shell with no diff to review is not
/// evidence. (An `ATERM_COMPILER_FLAVOR=r|t` env override used to rank ahead of
/// every signal below, so `ATERM_COMPILER_FLAVOR=t cargo build` shipped a binary
/// that reported trustc provenance it did not have. The three signals that
/// remain already covered every real lane.)
///
/// Priority order (first match wins):
///   1. the compiler's own `-vV` self-identification: a `trust: <version>` line
///      (what the toolchain prints under EVERY entry name — cargo and targo drive
///      it as `rustc`, and under that name the 2026-09 toolchain prints neither
///      of the next two markers; `trust-gate` keys on this same line), or
///      `binary: trustc`, or a `(trustc)` / `(trustc <version>)` version-line
///      parenthetical (the `trustc`-named entry stamps both). Direct evidence
///      from the probed binary, so it survives lanes where no env hint exists —
///      e.g. a bare `rustc` resolved via PATH, which sets neither RUSTC nor
///      RUSTUP_TOOLCHAIN;
///   2. the `RUSTC` path contains `/trust/` (the fork lives at `$HOME/trust/build/...`,
///      linked as `~/.rustup/toolchains/trust/` — covers pre-marker toolchains);
///   3. `RUSTUP_TOOLCHAIN == "trust"` (a `rustup toolchain link trust ...` lane).
///   4. default `"r"`.
///
/// Deliberately NOT inferred from a `-dev` release string: ANY locally built rustc
/// (upstream included) reports `-dev`, so `-dev` alone is zero evidence of Trust.
pub fn detect_flavor(vv: &str, rustc_path: &str, rustup_toolchain: Option<&str>) -> &'static str {
    let vv_says_trust = vv.lines().any(|l| {
        l.strip_prefix("binary:")
            .is_some_and(|b| b.trim() == "trustc")
            || l.strip_prefix("trust:")
                .is_some_and(|v| !v.trim().is_empty())
    }) || vv
        .lines()
        .next()
        .is_some_and(|first| first.contains("(trustc)") || first.contains("(trustc "));
    if vv_says_trust || rustc_path.contains("/trust/") || rustup_toolchain == Some("trust") {
        return "t";
    }
    "r"
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Verbatim `rustc -vV` from the upstream toolchain on this box
    /// (Homebrew/rustup-stable rustc 1.96.0, 2026-05-25).
    const UPSTREAM_VV: &str = "rustc 1.96.0 (ac68faa20 2026-05-25) (Homebrew)\n\
                               binary: rustc\n\
                               commit-hash: ac68faa20c58cbccd01ee7208bf3b6e93a7d7f96\n\
                               commit-date: 2026-05-25\n\
                               host: aarch64-apple-darwin\n\
                               release: 1.96.0\n\
                               LLVM version: 22.1.6";

    /// Verbatim `rustc -vV` from the Trust fork on this box
    /// ($HOME/trust/build/host/stage2/bin/rustc, 2026-06-27) — the PRE-marker era:
    /// no `(trustc)` parenthetical, `binary: rustc`. Only path/toolchain evidence
    /// can classify this one.
    const TRUST_VV: &str = "rustc 1.96.0-dev (58b453c80 2026-06-27)\n\
                            binary: rustc\n\
                            commit-hash: 58b453c80b3a2d5a005ecb7f98f8a2491c03e598\n\
                            commit-date: 2026-06-27\n\
                            host: aarch64-apple-darwin\n\
                            release: 1.96.0-dev\n\
                            LLVM version: 22.1.2";

    /// Verbatim `trustc -vV` from the 2026-07 toolchains, which self-identify
    /// BOTH ways: the `(trustc)` version-line parenthetical and `binary: trustc`.
    const TRUST_VV_MARKED: &str = "rustc 1.96.0-dev (7e631b2a4 2026-07-04) (trustc)\n\
                                   binary: trustc\n\
                                   commit-hash: 7e631b2a4830f36d318177dd5869ce601340dd83\n\
                                   commit-date: 2026-07-04\n\
                                   host: aarch64-apple-darwin\n\
                                   release: 1.96.0-dev\n\
                                   LLVM version: 22.1.2";

    /// Verbatim `rustc -vV` from the Trust stage2 on m17-tower (x86_64 Linux,
    /// built from trust `3a3e781fe`, 2026-09-14) — invoked under the `rustc` NAME,
    /// which is how cargo and targo drive it (`RUSTC=…/bin/rustc`). Under that
    /// name the toolchain prints NO `(trustc …)` parenthetical and `binary: rustc`;
    /// its only self-identification is the `trust:` line. The same binary invoked
    /// as `trustc` prints `(trustc 0.1.0)` and `binary: trustc` (argv0-keyed).
    const TRUST_VV_RUSTC_NAME: &str = "rustc 1.99.0-dev (3a3e781fe 2026-09-14)\n\
                                       binary: rustc\n\
                                       commit-hash: 3a3e781fe082b74d3c4de0ade65b1de3cbee7255\n\
                                       commit-date: 2026-09-14\n\
                                       host: x86_64-unknown-linux-gnu\n\
                                       release: 1.99.0-dev\n\
                                       trust: 0.1.0\n\
                                       LLVM version: 22.1.2";

    #[test]
    fn parses_upstream_vv() {
        let p = parse_rustc_vv(UPSTREAM_VV);
        assert_eq!(
            p.version_line,
            "rustc 1.96.0 (ac68faa20 2026-05-25) (Homebrew)"
        );
        assert_eq!(p.commit, "ac68faa20c58cbccd01ee7208bf3b6e93a7d7f96");
        assert_eq!(p.host, "aarch64-apple-darwin");
        assert_eq!(p.trust_version, "", "upstream rustc has no Trust version");
    }

    /// Trust's OWN version is a separate fact from the rustc-shaped release token:
    /// the `trust:` line carries it (under the `rustc` name cargo uses), the
    /// `(trustc <v>)` parenthetical is the fallback (the `trustc` name), a
    /// pre-marker Trust reports neither, and the release token stays `1.99.0-dev`
    /// — the Rust release Trust is compatible with — in every case.
    #[test]
    fn parses_trusts_own_version_apart_from_the_rust_compat_release() {
        let p = parse_rustc_vv(TRUST_VV_RUSTC_NAME);
        assert_eq!(p.trust_version, "0.1.0");
        assert_eq!(p.version_line.split_whitespace().nth(1), Some("1.99.0-dev"));
        assert_eq!(p.host, "x86_64-unknown-linux-gnu");
        // The trustc-named entry: no `trust:` line in this fixture, so the
        // parenthetical answers.
        let named = "rustc 1.99.0-dev (2b118046a 2026-07-29) (trustc 0.1.0)\nbinary: trustc";
        assert_eq!(parse_rustc_vv(named).trust_version, "0.1.0");
        // A `trust:` line outranks the parenthetical when both are present.
        let both = "rustc 1.99.0-dev (2b118046a 2026-07-29) (trustc 9.9.9)\ntrust: 0.1.0";
        assert_eq!(parse_rustc_vv(both).trust_version, "0.1.0");
        // Pre-marker Trust (`TRUST_VV`) and the bare `(trustc)` marker report none.
        assert_eq!(parse_rustc_vv(TRUST_VV).trust_version, "");
        assert_eq!(parse_rustc_vv(TRUST_VV_MARKED).trust_version, "");
        assert_eq!(
            trustc_parenthetical("rustc 1.99.0-dev (x 2026-01-01) (trustc )"),
            None
        );
    }

    /// Under the `rustc` name cargo drives — no parenthetical, `binary: rustc`,
    /// no env hint — the `trust:` line alone classifies the compiler as Trust.
    /// This is the lane every aterm build actually runs in.
    #[test]
    fn flavor_trust_from_the_trust_line_under_the_rustc_name() {
        assert_eq!(detect_flavor(TRUST_VV_RUSTC_NAME, "rustc", None), "t");
        // An empty `trust:` value is not a claim.
        let empty = "rustc 1.99.0-dev (3a3e781fe 2026-09-14)\nbinary: rustc\ntrust:";
        assert_eq!(detect_flavor(empty, "rustc", None), "r");
    }

    #[test]
    fn parses_trust_vv() {
        let p = parse_rustc_vv(TRUST_VV);
        assert_eq!(p.version_line, "rustc 1.96.0-dev (58b453c80 2026-06-27)");
        assert_eq!(p.commit, "58b453c80b3a2d5a005ecb7f98f8a2491c03e598");
        assert_eq!(p.host, "aarch64-apple-darwin");
    }

    #[test]
    fn parse_degrades_to_unknown_not_panic() {
        let p = parse_rustc_vv("");
        assert_eq!(p.version_line, "unknown");
        assert_eq!(p.commit, "unknown");
        assert_eq!(p.host, "unknown");
        assert_eq!(
            p.trust_version, "",
            "no Trust version is the empty string, not a word"
        );
    }

    #[test]
    fn flavor_defaults_to_r() {
        // The default on this box: stock cargo, no override, no trust toolchain.
        assert_eq!(
            detect_flavor(UPSTREAM_VV, "/opt/homebrew/bin/rustc", None),
            "r"
        );
        assert_eq!(detect_flavor(UPSTREAM_VV, "rustc", Some("stable")), "r");
    }

    #[test]
    fn flavor_trust_from_rustc_path_or_toolchain() {
        assert_eq!(
            detect_flavor(
                TRUST_VV,
                "/Users//example/trust/build/host/stage2/bin/rustc",
                None
            ),
            "t"
        );
        assert_eq!(detect_flavor(TRUST_VV, "rustc", Some("trust")), "t");
    }

    /// The 2026-07 trustc self-identifies in `-vV`, so it classifies 't' with NO
    /// env evidence at all — the targo ship lane (bare `rustc` via
    /// PATH, no RUSTC, no RUSTUP_TOOLCHAIN), which previously mislabeled as +r.
    #[test]
    fn flavor_trust_from_vv_self_identification() {
        assert_eq!(detect_flavor(TRUST_VV_MARKED, "rustc", None), "t");
        // Either marker alone suffices: version-line parenthetical…
        let line_only = "rustc 1.96.0-dev (7e631b2a4 2026-07-04) (trustc)\nbinary: rustc";
        assert_eq!(detect_flavor(line_only, "rustc", None), "t");
        // …or the `binary: trustc` field.
        let field_only = "rustc 1.96.0-dev (7e631b2a4 2026-07-04)\nbinary: trustc";
        assert_eq!(detect_flavor(field_only, "rustc", None), "t");
        // …or the versioned parenthetical the post-purge toolchains print
        // (`(trustc <trust-version>)` — Trust's own version, not the rust-compat
        // number).
        let versioned = "rustc 1.99.0-dev (2b118046a 2026-07-29) (trustc 0.1.0)\nbinary: rustc";
        assert_eq!(detect_flavor(versioned, "rustc", None), "t");
        // "(trustc)" anywhere PAST the first line is not the marker (defensive:
        // only the version line's parenthetical is the compiler's self-name).
        let stray = "rustc 1.96.0 (ac68faa20 2026-05-25) (Homebrew)\nbinary: rustc\nnote: (trustc)";
        assert_eq!(detect_flavor(stray, "rustc", None), "r");
    }

    #[test]
    fn sealed_linux_trust_identifies_via_its_explicit_version_field() {
        let native = "rustc 1.99.0-dev (ec2b5c59f 2026-09-01)\nbinary: rustc\n\
            commit-hash: ec2b5c59f7ae81dc57dac2a3346cb4c0ef9fad0d\n\
            host: aarch64-unknown-linux-gnu\nrelease: 1.99.0-dev\ntrust: 0.1.0\nLLVM version: 22.1.2";
        assert_eq!(detect_flavor(native, "rustc", None), "t");
        assert_eq!(
            detect_flavor("rustc 1.99.0-dev\ntrust:", "rustc", None),
            "r"
        );
        assert_eq!(
            detect_flavor("rustc 1.99.0-dev\nnote: trust: 0.1.0", "rustc", None),
            "r"
        );
    }

    /// Provenance is EVIDENCE ONLY — there is no override, in either direction.
    ///
    /// This replaces `flavor_explicit_override_wins_and_junk_falls_through`,
    /// which pinned an `ATERM_COMPILER_FLAVOR=r|t` build-environment override
    /// that ranked AHEAD of the compiler's own `-vV` self-identification. That
    /// made `ATERM_COMPILER_FLAVOR=t cargo build` produce a binary whose About
    /// panel and `aterm ctl version` claimed trustc provenance it did not have
    /// — a claim the project ships as evidence, falsifiable from a shell with
    /// no diff to review. The remaining three signals cover every real lane,
    /// including the bare-PATH `rustc` one the override was justified by.
    #[test]
    fn flavor_is_evidence_only_and_cannot_be_overridden() {
        // Trust evidence classifies 't' whatever the environment says.
        assert_eq!(
            detect_flavor(TRUST_VV_MARKED, "/x/trust/bin/rustc", Some("trust")),
            "t"
        );
        // An upstream compiler at an upstream path stays 'r'. Nothing an
        // operator can export moves it.
        assert_eq!(
            detect_flavor(UPSTREAM_VV, "/opt/homebrew/bin/rustc", None),
            "r"
        );
    }

    #[test]
    fn dev_release_alone_is_not_trust() {
        // TRUST_VV says "-dev", but with an upstream-looking path, no trust
        // toolchain, and no -vV marker the flavor stays 'r': -dev is any local
        // build, not the fork.
        let p = parse_rustc_vv(TRUST_VV);
        assert!(p.version_line.contains("-dev"));
        assert_eq!(detect_flavor(TRUST_VV, "/usr/local/bin/rustc", None), "r");
    }
}
