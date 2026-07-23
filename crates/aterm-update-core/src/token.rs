// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

//! Resolving the GitHub token used to read the PRIVATE release repo.
//!
//! The token is **per-machine** and must NEVER be compiled into the shipped
//! (signed) binary — that would distribute one credential to every user. Because the
//! repo is private, *some* token is unavoidable (an anonymous Releases request 404s),
//! but the goal is **no *additional* secret** beyond the GitHub auth the machine may
//! already have. Resolved at runtime, first hit wins, in order:
//!
//! 1. `$ATERM_UPDATE_TOKEN` — explicit, dedicated override (CI / power users who want
//!    to provision a narrowly-scoped Contents:read PAT that beats any ambient one);
//! 2. the macOS **keychain** generic-password item `aterm-update-token`
//!    (`security find-generic-password -s aterm-update-token -w`);
//! 3. a `0600` file `…/aterm/update-token` under Application Support;
//! 4. `$GITHUB_TOKEN`, then `$GH_TOKEN` — the conventional ambient CI/tool env vars;
//! 5. `gh auth token` — the credential a developer already has after `gh auth login`.
//!
//! (4)/(5) mean a machine that is already authenticated to GitHub self-updates with
//! **no new secret**; the dedicated sources (1–3) stay highest-priority so a scoped
//! token still wins. A fine-grained PAT with read-only **Contents** permission on the
//! repo is sufficient (and is what `gh`'s token or a scoped PAT carries). Every
//! resolved token is charset-validated ([`valid_token`]) before use — a value
//! carrying a quote/backslash/whitespace is refused, so it can never break out of the
//! `curl --config` line it is fed to (see `http::curl_auth`). The token is never
//! logged and never placed on a command line.

use std::path::Path;
use std::process::Command;

/// Whether a resolved token is safe to feed to `curl --config -` as a header value.
/// GitHub tokens are `[A-Za-z0-9_]` (fine-grained `github_pat_…`, classic `ghp_…`,
/// `gh` `gho_…`); we allow `-` too for latitude and REJECT everything else —
/// crucially the `"`, `\`, and newline that could terminate/escape the quoted header
/// in the curl config stream (an injection / mangled-auth guard). Empty is invalid.
#[must_use]
pub fn valid_token(s: &str) -> bool {
    !s.is_empty()
        && s.len() <= 512
        && s.bytes()
            .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'_' | b'-'))
}

/// The SINGLE validation chokepoint every token source funnels through, so no source
/// can skip validation. (This exists because a source once did: `from_keychain`
/// returned its raw output without calling [`valid_token`], unlike its siblings —
/// exactly the class of bug a per-source re-implementation invites.) Given the raw
/// bytes a source produced, trim the surrounding whitespace a CLI/file/keychain
/// tacks on, reject empty, then reject anything that is not a well-formed token
/// (warning, naming `source`, so a set-but-malformed value is visibly skipped rather
/// than silently mangled). `.trim()` removes only LEADING/TRAILING whitespace; an
/// EMBEDDED quote/backslash/newline survives it and is rejected here, so it can never
/// reach the `format!("header = \"…{token}…\"")` fed to `curl --config -`
/// (see `http::curl_auth`) as a second directive or a broken `Authorization` header.
fn validated_token(raw: &str, source: &str) -> Option<String> {
    let s = raw.trim();
    if s.is_empty() {
        return None;
    }
    if !valid_token(s) {
        crate::warn(&format!("{source} is not a well-formed token; ignoring"));
        return None;
    }
    Some(s.to_string())
}

/// Resolve the token, or `None` when none is provisioned (the updater then stays
/// idle rather than hammering the API unauthenticated against a private repo).
/// `support_dir` is `…/Library/Application Support/aterm` (the `Updates` parent).
pub fn resolve(support_dir: &Path) -> Option<String> {
    resolve_with_source(support_dir).map(|(t, _)| t)
}

/// [`resolve`], additionally reporting WHICH source supplied the token — a short
/// operator-facing label (NEVER the token itself), so loud diagnostic surfaces
/// (`atpkg doctor`) can say where a credential came from without ever printing
/// it. Same chain, same order, first hit wins.
pub fn resolve_with_source(support_dir: &Path) -> Option<(String, &'static str)> {
    // Dedicated sources first — a power user's scoped Contents:read PAT wins over any
    // broader ambient credential.
    if let Some(t) = from_env("ATERM_UPDATE_TOKEN") {
        return Some((t, "$ATERM_UPDATE_TOKEN"));
    }
    if let Some(t) = from_keychain() {
        return Some((t, "keychain item aterm-update-token"));
    }
    if let Some(t) = from_file(&support_dir.join("update-token")) {
        return Some((t, "0600 update-token file"));
    }
    // Fallbacks: reuse the GitHub credential the machine ALREADY has, so a developer
    // who has run `gh auth login` needs no additional secret (F3 — the private repo
    // still requires *a* token, just not a *new* one).
    if let Some(t) = from_env("GITHUB_TOKEN") {
        return Some((t, "$GITHUB_TOKEN"));
    }
    if let Some(t) = from_env("GH_TOKEN") {
        return Some((t, "$GH_TOKEN"));
    }
    from_gh_cli().map(|t| (t, "gh auth token"))
}

/// Read a token from environment variable `key`: trim, then accept only if it passes
/// [`valid_token`] (a set-but-malformed value is skipped with a warning so a stray
/// export can't produce a mangled `Authorization` header, rather than silently used).
// Skip: to_string_lossy on an env var for DISPLAY-side token validation —
// hardened byte_loss class; a lossy mangling fails `valid_token` (fail-
// closed), never corrupts stored bytes. Audited (update-atpkg).
#[cfg_attr(trust_verify, trust::skip)]
fn from_env(key: &str) -> Option<String> {
    let raw = std::env::var_os(key)?;
    validated_token(&raw.to_string_lossy(), &format!("${key}"))
}

/// Last-resort fallback: the `gh` CLI's stored token (`gh auth token`). Tries `gh` on
/// `PATH` then the two Homebrew/local install prefixes, because a Finder-launched
/// `.app` inherits a minimal `PATH` that usually lacks `/opt/homebrew/bin`. Returns
/// `None` if `gh` is absent, unauthenticated, or prints a malformed token.
// Skip: same audited display/validation byte_loss class as `from_env`.
#[cfg_attr(trust_verify, trust::skip)]
fn from_gh_cli() -> Option<String> {
    for gh in ["gh", "/opt/homebrew/bin/gh", "/usr/local/bin/gh"] {
        let Ok(out) = Command::new(gh).args(["auth", "token"]).output() else {
            continue; // gh not found at this path
        };
        if !out.status.success() {
            continue;
        }
        if let Some(t) = validated_token(&String::from_utf8_lossy(&out.stdout), "gh auth token") {
            return Some(t);
        }
    }
    None
}

/// `security find-generic-password -s aterm-update-token -w` → the secret on
/// stdout. Returns `None` if the item is absent, the tool fails, or the value is not
/// a well-formed token — the last via [`validated_token`], the shared chokepoint, so
/// a keychain item carrying an embedded newline/quote/backslash cannot inject into
/// the `curl --config` header line (this source once skipped that guard).
// Skip: same audited display/validation byte_loss class as `from_env`.
#[cfg_attr(trust_verify, trust::skip)]
fn from_keychain() -> Option<String> {
    let out = Command::new("/usr/bin/security")
        .args(["find-generic-password", "-s", "aterm-update-token", "-w"])
        .output()
        .ok()?;
    if !out.status.success() {
        return None;
    }
    validated_token(
        &String::from_utf8_lossy(&out.stdout),
        "keychain item aterm-update-token",
    )
}

/// Read a token from a `0600`-or-tighter file, refusing one that is
/// group/other-readable (a leaked credential file is worse than no updates).
// Skip: metadata+read_to_string on the 0600 token file — hardened raw_path/
// utf8_reject classes; the file is created by this crate's own private-dir
// discipline and a non-UTF-8/corrupt token fails `valid_token` (fail-
// closed). Audited (update-atpkg).
#[cfg_attr(trust_verify, trust::skip)]
fn from_file(path: &Path) -> Option<String> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt;
        let meta = std::fs::metadata(path).ok()?;
        if meta.mode() & 0o077 != 0 {
            crate::warn(&format!(
                "{} is group/other-accessible; ignoring (chmod 600 it)",
                path.display()
            ));
            return None;
        }
    }
    // Non-unix: no POSIX mode bits to check — the token file lives under the
    // per-user profile dir, whose default owner-only ACLs are the
    // confidentiality boundary. (The updater is inert off macOS; this path is
    // compile-only honesty, not a claim of parity.)
    let raw = std::fs::read_to_string(path).ok()?;
    validated_token(&raw, &path.display().to_string())
}

#[cfg(test)]
mod tests {
    use super::*;
    #[cfg(unix)]
    use std::os::unix::fs::PermissionsExt;

    // The mode-bit tests exercise the POSIX 0600 gate and are unix-only; the
    // validation-chokepoint tests below are platform-shared.
    #[cfg(unix)]
    fn tmp(name: &str) -> std::path::PathBuf {
        let d = std::env::temp_dir().join(format!("aterm-tok-{}-{name}", std::process::id()));
        std::fs::create_dir_all(&d).unwrap();
        d.join("update-token")
    }

    #[cfg(unix)]
    #[test]
    fn file_token_accepts_0600() {
        let p = tmp("ok");
        std::fs::write(&p, "github_pat_secret\n").unwrap();
        std::fs::set_permissions(&p, std::fs::Permissions::from_mode(0o600)).unwrap();
        assert_eq!(from_file(&p).as_deref(), Some("github_pat_secret"));
        let _ = std::fs::remove_dir_all(p.parent().unwrap());
    }

    #[cfg(unix)]
    #[test]
    fn file_token_refuses_group_or_other_readable() {
        for mode in [0o644u32, 0o640, 0o604, 0o660] {
            let p = tmp(&format!("m{mode:o}"));
            std::fs::write(&p, "leakable").unwrap();
            std::fs::set_permissions(&p, std::fs::Permissions::from_mode(mode)).unwrap();
            assert!(from_file(&p).is_none(), "mode {mode:o} must be refused");
            let _ = std::fs::remove_dir_all(p.parent().unwrap());
        }
    }

    #[test]
    fn missing_file_is_none() {
        assert!(from_file(std::path::Path::new("/nonexistent/aterm/update-token")).is_none());
    }

    #[cfg(unix)]
    #[test]
    fn file_token_refuses_curl_config_injection() {
        // A token carrying a quote/backslash/newline could break out of the
        // `header = "Authorization: Bearer …"` line in curl's --config stream; such a
        // file must be refused even at 0600 (F20).
        for bad in [
            "abc\"def",        // quote — terminates the header value
            "abc\\def",        // backslash — escape introducer
            "line1\ninjected", // embedded newline — second directive
            "has space",       // whitespace
        ] {
            let p = tmp("inj");
            std::fs::write(&p, bad).unwrap();
            std::fs::set_permissions(&p, std::fs::Permissions::from_mode(0o600)).unwrap();
            assert!(from_file(&p).is_none(), "{bad:?} must be refused");
            let _ = std::fs::remove_dir_all(p.parent().unwrap());
        }
    }

    #[test]
    fn validated_token_is_the_single_validation_chokepoint() {
        // Every source (env, keychain, file, gh) funnels through validated_token, so
        // this pins the shared contract that closes the from_keychain bypass: trim
        // surrounding whitespace, keep a well-formed token, reject empty and every
        // curl-config-injection shape (embedded quote/backslash/newline/whitespace).
        assert_eq!(
            validated_token(concat!("  gh", "p_abc123DEF_-  \n"), "test").as_deref(),
            Some(concat!("gh", "p_abc123DEF_-")),
            "surrounding whitespace is trimmed; a well-formed token is kept"
        );
        assert!(validated_token("", "test").is_none(), "empty is refused");
        assert!(
            validated_token("   \n\t ", "test").is_none(),
            "all-whitespace is refused"
        );
        for bad in [
            "abc\"def",        // quote — terminates the header value
            "abc\\def",        // backslash — escape introducer
            "line1\ninjected", // EMBEDDED newline — survives trim, second directive
            "has space",       // interior whitespace
            "tok;en",          // shell/metacharacter
        ] {
            assert!(
                validated_token(bad, "test").is_none(),
                "{bad:?} must be refused by the chokepoint"
            );
        }
    }

    #[test]
    fn valid_token_accepts_real_shapes_rejects_injection() {
        for ok in [
            concat!("github", "_pat_11ABCDEF0_xYz123"),
            concat!("gh", "p_0123456789abcdefABCDEF"),
            "gho_tokenlikevalue",
        ] {
            assert!(valid_token(ok), "{ok:?} should be valid");
        }
        for bad in [
            "", "a\"b", "a\\b", "a b", "a\nb", "a\tb", "tok;en", "tok`en",
        ] {
            assert!(!valid_token(bad), "{bad:?} should be rejected");
        }
    }
}
