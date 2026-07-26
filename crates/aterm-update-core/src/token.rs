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
//!
//! # Provisioning, and what happens without it
//!
//! A machine with NONE of (1)–(5) can never read the private repo, so it never
//! updates — and for a long time that state was near-invisible (one log line per
//! process). Two things close that:
//!
//! * [`provision`] writes source (3), the durable `0600` file, from a token the
//!   caller already holds. `tools/install.sh` calls the same thing through
//!   [`PROVISION_COMMAND`] so a freshly installed machine is provisioned by the
//!   install itself, not by a step nobody knew to run.
//! * [`diagnose`] reports the WHOLE chain — which sources were consulted, which
//!   were absent, and which were present-but-rejected (and why) — so the updater
//!   can say something actionable instead of going quiet. It never carries, and
//!   can never leak, the token value: [`SourceProbe`] holds a `&'static str`
//!   label and an outcome, never bytes from a credential.
//!
//! There is deliberately NO auto-adoption of an ambient token (4)/(5) into the
//! durable file. Copying a broad `gh` credential to a new place on disk, silently,
//! would outlive `gh auth logout` and rotate out of the user's control. Adoption is
//! an explicit act (the installer, or [`PROVISION_COMMAND`]).

use std::path::{Path, PathBuf};
use std::process::Command;

const MAX_TOKEN_FILE_BYTES: usize = 1024;

/// The durable per-machine token file: `<support_dir>/update-token`. This is the
/// ONE place the path is spelled — [`resolve_with_source`], [`provision`],
/// [`diagnose`] and the operator-facing [`PROVISION_COMMAND`] all agree by
/// construction.
#[must_use]
pub fn token_file(support_dir: &Path) -> PathBuf {
    support_dir.join("update-token")
}

/// The exact, copy-pasteable one-command remedy printed by every "no token"
/// surface (the app log, `status.toml`'s `outcome`, `aterm-ctl update status`,
/// `atpkg doctor`). It must work on a Mac that has NEVER run aterm — hence the
/// `mkdir -p` — and must not leave a world-readable credential behind — hence the
/// subshell `umask 077`, which applies to the redirect that CREATES the file.
///
/// Kept as one literal line so it survives being pasted out of a log.
pub const PROVISION_COMMAND: &str = concat!(
    "mkdir -p ~/Library/Application\\ Support/aterm && ",
    "(umask 077; gh auth token > ~/Library/Application\\ Support/aterm/update-token)"
);

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

/// What consulting ONE token source produced. Carries a reason, never a value —
/// nothing in this type is derived from credential bytes.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ProbeOutcome {
    /// Nothing there: the variable is unset, the keychain item does not exist, the
    /// file is missing, or `gh` is absent / not logged in.
    Absent,
    /// Something WAS there and was refused. The `&'static str` says why, in words an
    /// operator can act on; it is a fixed literal, so it cannot echo the value.
    Rejected(&'static str),
    /// A well-formed token came from this source.
    Supplied,
}

/// One consulted source and its outcome, in chain order.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SourceProbe {
    /// The same operator-facing label [`resolve_with_source`] reports.
    pub source: &'static str,
    /// What consulting it produced.
    pub outcome: ProbeOutcome,
}

/// The whole chain's verdict: which source (if any) supplied a token, plus every
/// source consulted up to and including it. Built by [`diagnose`] from the SAME
/// walk [`resolve_with_source`] uses, so the diagnosis can never describe a
/// different chain than the one that actually runs.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Diagnosis {
    /// The label of the source that supplied a token, or `None` when none did.
    pub resolved: Option<&'static str>,
    /// Every source consulted, in order. Ends at the supplying source when there is
    /// one (later sources are genuinely never reached, so claiming otherwise would
    /// be a lie).
    pub probes: Vec<SourceProbe>,
}

impl Diagnosis {
    /// Whether this machine can read the private release repo at all.
    #[must_use]
    pub fn is_provisioned(&self) -> bool {
        self.resolved.is_some()
    }

    /// The sources that were present but REFUSED, as `"<source> (<why>)"`. These are
    /// the actionable ones: a `chmod 600` or a re-paste fixes them, whereas an absent
    /// source is just "not configured". Empty when nothing was rejected.
    #[must_use]
    pub fn rejections(&self) -> Vec<String> {
        self.probes
            .iter()
            .filter_map(|p| match &p.outcome {
                ProbeOutcome::Rejected(why) => Some(format!("{} ({why})", p.source)),
                _ => None,
            })
            .collect()
    }

    /// One operator-facing line explaining why there is no token and exactly what to
    /// run. Never contains a credential.
    #[must_use]
    pub fn no_token_explanation(&self) -> String {
        let rejected = self.rejections();
        let detail = if rejected.is_empty() {
            format!(
                "none of the {} token sources is configured",
                self.probes.len()
            )
        } else {
            format!("present but refused: {}", rejected.join("; "))
        };
        format!(
            "no update token is provisioned ({detail}) — this machine cannot read the \
             private release repo and will NEVER receive an update until it is fixed. \
             Run: {PROVISION_COMMAND}"
        )
    }
}

/// Consult every token source in [`resolve_with_source`]'s order and report what
/// each produced, WITHOUT returning the token. Use this for diagnostics
/// (`atpkg doctor`, the updater's "why is this machine not updating?" status);
/// use [`resolve`] to actually get a token.
///
/// Both run the identical [`walk`], so a diagnosis and a resolution can never
/// disagree about the chain.
#[must_use]
pub fn diagnose(support_dir: &Path) -> Diagnosis {
    let mut probes = Vec::new();
    let resolved = walk(support_dir, &mut probes).map(|(_, source)| source);
    Diagnosis { resolved, probes }
}

/// [`resolve_with_source`] and [`diagnose`] in ONE walk: the token on success, the
/// full diagnosis on failure.
///
/// This is what a caller that must explain a failure should use. Walking twice
/// (resolve, then diagnose because it returned `None`) would re-spawn `security`
/// and `gh` on every unprovisioned check — the exact machines that get checked
/// forever without ever succeeding.
pub fn resolve_or_diagnose(support_dir: &Path) -> Result<(String, &'static str), Diagnosis> {
    let mut probes = Vec::new();
    match walk(support_dir, &mut probes) {
        Some(hit) => Ok(hit),
        None => Err(Diagnosis {
            resolved: None,
            probes,
        }),
    }
}

/// Validate `raw` the way every source's output is validated, reporting the
/// distinction the [`Diagnosis`] needs: nothing there vs. there-but-refused.
/// `.trim()` removes only LEADING/TRAILING whitespace; an EMBEDDED
/// quote/backslash/newline survives it and is rejected here, so it can never reach
/// the `format!("header = \"…{token}…\"")` fed to `curl --config -`.
fn probe_raw(raw: &str, source: &str) -> Probe {
    let s = raw.trim();
    if s.is_empty() {
        return Probe::Absent;
    }
    if !valid_token(s) {
        crate::warn(&format!("{source} is not a well-formed token; ignoring"));
        return Probe::Rejected(
            "not a well-formed token — GitHub tokens are [A-Za-z0-9_-] only, so a value \
             carrying whitespace, a quote, or a backslash is refused",
        );
    }
    Probe::Supplied(s.to_string())
}

/// [`ProbeOutcome`] plus the token on the success arm — the internal shape the walk
/// works in. Split from the public type so a credential is never reachable from
/// anything a diagnostic surface holds.
enum Probe {
    Absent,
    Rejected(&'static str),
    Supplied(String),
}

impl Probe {
    fn outcome(&self) -> ProbeOutcome {
        match self {
            Self::Absent => ProbeOutcome::Absent,
            Self::Rejected(why) => ProbeOutcome::Rejected(why),
            Self::Supplied(_) => ProbeOutcome::Supplied,
        }
    }
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
    walk(support_dir, &mut Vec::new())
}

/// THE resolution chain — the single walk [`resolve_with_source`] and [`diagnose`]
/// both run. Each consulted source appends a [`SourceProbe`] to `probes` before the
/// walk decides whether to stop, so the recorded chain is exactly the executed one.
///
/// Order: dedicated sources first — a power user's scoped Contents:read PAT wins over
/// any broader ambient credential — then the ambient GitHub credential the machine may
/// already have, so an already-authenticated developer needs no additional secret.
fn walk(support_dir: &Path, probes: &mut Vec<SourceProbe>) -> Option<(String, &'static str)> {
    let chain: [(&'static str, &dyn Fn() -> Probe); 6] = [
        ("$ATERM_UPDATE_TOKEN", &|| probe_env("ATERM_UPDATE_TOKEN")),
        ("keychain item aterm-update-token", &probe_keychain),
        ("0600 update-token file", &|| {
            probe_file(&token_file(support_dir))
        }),
        ("$GITHUB_TOKEN", &|| probe_env("GITHUB_TOKEN")),
        ("$GH_TOKEN", &|| probe_env("GH_TOKEN")),
        ("gh auth token", &probe_gh_cli),
    ];
    for (source, run) in chain {
        let probe = run();
        probes.push(SourceProbe {
            source,
            outcome: probe.outcome(),
        });
        if let Probe::Supplied(token) = probe {
            return Some((token, source));
        }
    }
    None
}

/// Read a token from environment variable `key`: trim, then accept only if it passes
/// [`valid_token`] (a set-but-malformed value is skipped with a warning so a stray
/// export can't produce a mangled `Authorization` header, rather than silently used).
// Skip: to_string_lossy on an env var for DISPLAY-side token validation —
// hardened byte_loss class; a lossy mangling fails `valid_token` (fail-
// closed), never corrupts stored bytes. Audited (update-atpkg).
#[cfg_attr(trust_verify, trust::skip)]
fn probe_env(key: &str) -> Probe {
    let Some(raw) = std::env::var_os(key) else {
        return Probe::Absent;
    };
    probe_raw(&raw.to_string_lossy(), &format!("${key}"))
}

/// Last-resort fallback: the `gh` CLI's stored token (`gh auth token`). Tries `gh` on
/// `PATH` then the two Homebrew/local install prefixes, because a Finder-launched
/// `.app` inherits a minimal `PATH` that usually lacks `/opt/homebrew/bin`. Returns
/// `None` if `gh` is absent, unauthenticated, or prints a malformed token.
// Skip: same audited display/validation byte_loss class as `probe_env`.
#[cfg_attr(trust_verify, trust::skip)]
fn probe_gh_cli() -> Probe {
    // Distinguish "gh isn't installed" from "gh is installed but not logged in" —
    // the two need different remedies (`brew install gh` vs `gh auth login`), and
    // only the diagnosis surface can tell the user which one applies.
    let mut found_gh = false;
    for gh in ["gh", "/opt/homebrew/bin/gh", "/usr/local/bin/gh"] {
        let Ok(out) = Command::new(gh).args(["auth", "token"]).output() else {
            continue; // gh not found at this path
        };
        found_gh = true;
        if !out.status.success() {
            continue;
        }
        match probe_raw(&String::from_utf8_lossy(&out.stdout), "gh auth token") {
            Probe::Supplied(t) => return Probe::Supplied(t),
            Probe::Rejected(why) => return Probe::Rejected(why),
            Probe::Absent => {}
        }
    }
    if found_gh {
        Probe::Rejected("`gh` is installed but has no token — run `gh auth login`")
    } else {
        Probe::Absent
    }
}

/// `security find-generic-password -s aterm-update-token -w` → the secret on
/// stdout. `Absent` if the item is missing or the tool fails; `Rejected` if the
/// value is not a well-formed token — the last via [`probe_raw`], the shared
/// validation chokepoint every source funnels through, so a keychain item carrying
/// an embedded newline/quote/backslash cannot inject into the `curl --config`
/// header line (this source once skipped that guard, which is why the chokepoint
/// exists at all).
// Skip: same audited display/validation byte_loss class as `probe_env`.
#[cfg_attr(trust_verify, trust::skip)]
fn probe_keychain() -> Probe {
    let Ok(out) = Command::new("/usr/bin/security")
        .args(["find-generic-password", "-s", "aterm-update-token", "-w"])
        .output()
    else {
        return Probe::Absent; // no `security` tool (non-macOS)
    };
    if !out.status.success() {
        return Probe::Absent; // no such keychain item
    }
    probe_raw(
        &String::from_utf8_lossy(&out.stdout),
        "keychain item aterm-update-token",
    )
}

/// Read a token from a `0600`-or-tighter file, refusing one that is
/// group/other-readable (a leaked credential file is worse than no updates).
///
/// A file that EXISTS but does not yield a token is [`Probe::Rejected`], never
/// `Absent`: "you provisioned a token and I threw it away" is the one state an
/// operator most needs told, and folding it into "not configured" is exactly how a
/// `chmod 644` turns into a machine that silently stops updating.
#[cfg_attr(trust_verify, trust::skip)]
fn probe_file(path: &Path) -> Probe {
    if std::fs::symlink_metadata(path).is_err() {
        return Probe::Absent;
    }
    let Some(raw) = read_token_file(path) else {
        return Probe::Rejected(
            "the update-token file exists but was refused — it must be a regular file \
             (not a symlink or FIFO), no larger than 1 KiB, and mode 0600 (`chmod 600` it)",
        );
    };
    probe_raw(&raw, &path.display().to_string())
}

/// Write `token` to the durable `0600` per-machine token file
/// ([`token_file`]), creating and hardening the support dir if needed. This is the
/// programmatic form of [`PROVISION_COMMAND`], and the only supported way to create
/// source (3).
///
/// Obligations, all of which exist because the alternative leaks a credential:
/// * the value is validated with [`valid_token`] FIRST, so a malformed paste is
///   refused loudly here instead of silently ignored at every later resolve;
/// * the bytes are written to a per-pid temp file created `O_EXCL` at mode `0600`
///   — never to the live path, so a reader can never observe a partial token and a
///   pre-existing symlink at the destination cannot capture the write;
/// * the temp file is `fsync`'d and then `rename`d over the destination, so the
///   file is atomically either the old token or the new one, never neither.
///
/// The error strings are operator-facing and NEVER contain the token.
#[cfg_attr(trust_verify, trust::skip)]
pub fn provision(support_dir: &Path, token: &str) -> Result<PathBuf, String> {
    let token = token.trim();
    if !valid_token(token) {
        return Err(
            "refusing to provision: the value is not a well-formed GitHub token \
             ([A-Za-z0-9_-], 1..=512 chars). Check for a stray newline, quote, or an \
             error message captured instead of a token."
                .to_string(),
        );
    }
    crate::ensure_private_dir(support_dir)
        .map_err(|e| format!("{}: {e}", support_dir.display()))?;
    let dest = token_file(support_dir);
    let tmp = support_dir.join(format!("update-token.{}.tmp", std::process::id()));
    let _ = std::fs::remove_file(&tmp);
    write_private_file(&tmp, token.as_bytes()).map_err(|e| format!("{}: {e}", tmp.display()))?;
    if let Err(e) = std::fs::rename(&tmp, &dest) {
        let _ = std::fs::remove_file(&tmp);
        return Err(format!("{}: {e}", dest.display()));
    }
    Ok(dest)
}

/// Create `path` `O_EXCL` at mode `0600`, write `bytes`, and `fsync` — the
/// credential-safe half of [`provision`], split out so the mode is applied at
/// CREATION (a `set_permissions` after the fact leaves a window where the bytes
/// are on disk under the ambient umask).
#[cfg(unix)]
fn write_private_file(path: &Path, bytes: &[u8]) -> std::io::Result<()> {
    use std::io::Write as _;
    use std::os::unix::fs::OpenOptionsExt as _;

    let mut file = std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(0o600)
        .custom_flags(libc::O_CLOEXEC | libc::O_NOFOLLOW)
        .open(path)?;
    file.write_all(bytes)?;
    file.sync_all()
}

/// Non-unix: `%LOCALAPPDATA%` ACLs are the confidentiality boundary and POSIX modes
/// have no analogue, exactly as [`crate::ensure_private_dir`] documents. The
/// `create_new` + `fsync` obligations still hold.
#[cfg(not(unix))]
fn write_private_file(path: &Path, bytes: &[u8]) -> std::io::Result<()> {
    use std::io::Write as _;

    let mut file = std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(path)?;
    file.write_all(bytes)?;
    file.sync_all()
}

#[cfg(unix)]
fn open_token_file(path: &Path) -> Option<std::fs::File> {
    use std::os::unix::fs::{MetadataExt as _, OpenOptionsExt as _};

    let file = std::fs::OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_CLOEXEC | libc::O_NONBLOCK | libc::O_NOFOLLOW)
        .open(path)
        .ok()?;
    let metadata = file.metadata().ok()?;
    if !metadata.file_type().is_file() {
        return None;
    }
    if metadata.mode() & 0o077 != 0 {
        crate::warn(&format!(
            "{} is group/other-accessible; ignoring (chmod 600 it)",
            path.display()
        ));
        return None;
    }
    Some(file)
}

#[cfg(windows)]
fn open_token_file(path: &Path) -> Option<std::fs::File> {
    use std::os::windows::fs::{MetadataExt as _, OpenOptionsExt as _};

    const FILE_ATTRIBUTE_REPARSE_POINT: u32 = 0x0000_0400;
    const FILE_FLAG_OPEN_REPARSE_POINT: u32 = 0x0020_0000;
    let file = std::fs::OpenOptions::new()
        .read(true)
        .custom_flags(FILE_FLAG_OPEN_REPARSE_POINT)
        .open(path)
        .ok()?;
    let metadata = file.metadata().ok()?;
    if !metadata.file_type().is_file()
        || metadata.file_attributes() & FILE_ATTRIBUTE_REPARSE_POINT != 0
    {
        return None;
    }
    Some(file)
}

#[cfg(not(any(unix, windows)))]
fn open_token_file(path: &Path) -> Option<std::fs::File> {
    if !std::fs::symlink_metadata(path).ok()?.file_type().is_file() {
        return None;
    }
    let file = std::fs::File::open(path).ok()?;
    file.metadata().ok()?.file_type().is_file().then_some(file)
}

fn read_token_file(path: &Path) -> Option<String> {
    use std::io::Read as _;

    let file = open_token_file(path)?;
    let metadata = file.metadata().ok()?;
    if metadata.len() > MAX_TOKEN_FILE_BYTES as u64 {
        return None;
    }
    let mut bytes = Vec::with_capacity(
        usize::try_from(metadata.len())
            .unwrap_or(MAX_TOKEN_FILE_BYTES)
            .min(MAX_TOKEN_FILE_BYTES),
    );
    file.take((MAX_TOKEN_FILE_BYTES + 1) as u64)
        .read_to_end(&mut bytes)
        .ok()?;
    if bytes.len() > MAX_TOKEN_FILE_BYTES {
        return None;
    }
    String::from_utf8(bytes).ok()
}

#[cfg(test)]
mod tests {
    use super::*;
    #[cfg(unix)]
    use std::os::unix::fs::PermissionsExt;

    /// The pre-probe shape of the file source: `Some(token)` or nothing. The
    /// Absent/Rejected split the shipping code needs is asserted separately
    /// (`file_probe_distinguishes_absent_from_rejected`), so these tests keep
    /// reading as "does the 0600 gate hold".
    fn from_file(path: &Path) -> Option<String> {
        match probe_file(path) {
            Probe::Supplied(t) => Some(t),
            Probe::Absent | Probe::Rejected(_) => None,
        }
    }

    /// The chokepoint's pre-probe shape, for the validation tests.
    fn validated_token(raw: &str, source: &str) -> Option<String> {
        match probe_raw(raw, source) {
            Probe::Supplied(t) => Some(t),
            Probe::Absent | Probe::Rejected(_) => None,
        }
    }

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

    #[test]
    fn oversized_sparse_token_file_is_rejected() {
        let d = std::env::temp_dir().join(format!("aterm-token-sparse-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(&d).unwrap();
        let path = d.join("update-token");
        let file = std::fs::File::create(&path).unwrap();
        file.set_len((MAX_TOKEN_FILE_BYTES + 1) as u64).unwrap();
        #[cfg(unix)]
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o600)).unwrap();
        assert!(from_file(&path).is_none());
        let _ = std::fs::remove_dir_all(d);
    }

    #[cfg(unix)]
    #[test]
    fn fifo_and_symlink_token_files_return_without_blocking() {
        use std::os::unix::ffi::OsStrExt as _;

        let path = tmp("special");
        let path_c = std::ffi::CString::new(path.as_os_str().as_bytes()).unwrap();
        // SAFETY: `path_c` is a live NUL-terminated path in our private fixture.
        assert_eq!(unsafe { libc::mkfifo(path_c.as_ptr(), 0o600) }, 0);
        assert!(from_file(&path).is_none());
        std::fs::remove_file(&path).unwrap();
        let target = path.with_file_name("token-target");
        std::fs::write(&target, "github_pat_secret\n").unwrap();
        std::fs::set_permissions(&target, std::fs::Permissions::from_mode(0o600)).unwrap();
        std::os::unix::fs::symlink(&target, &path).unwrap();
        assert!(from_file(&path).is_none(), "credential links are refused");
        let _ = std::fs::remove_dir_all(path.parent().unwrap());
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

    fn support(name: &str) -> std::path::PathBuf {
        let d = std::env::temp_dir().join(format!(
            "aterm-tok-support-{}-{name}",
            std::process::id()
        ));
        let _ = std::fs::remove_dir_all(&d);
        d
    }

    #[test]
    fn provision_creates_the_support_dir_on_a_machine_that_never_ran_aterm() {
        // The fresh-Mac case: Application Support/aterm does not exist yet. Provision
        // must create it (0700) and land the token, with no prior aterm run.
        let dir = support("fresh");
        assert!(!dir.exists(), "precondition: nothing has ever run here");
        let token = concat!("gh", "p_freshmachine0123456789ABCDEF");
        let path = provision(&dir, token).expect("provision on a fresh machine");
        assert_eq!(path, token_file(&dir));
        assert_eq!(from_file(&path).as_deref(), Some(token));
        // …and the chain now resolves from the durable file source. `$ATERM_UPDATE_TOKEN`
        // and the keychain outrank it, so assert the FILE probe rather than the whole
        // walk (the developer running this test may well have both).
        assert!(matches!(probe_file(&path), Probe::Supplied(_)));
        #[cfg(unix)]
        {
            let mode = std::fs::metadata(&path).unwrap().permissions().mode();
            assert_eq!(mode & 0o777, 0o600, "token file must be 0600, got {mode:o}");
            let dir_mode = std::fs::metadata(&dir).unwrap().permissions().mode();
            assert_eq!(dir_mode & 0o777, 0o700, "support dir must be 0700");
        }
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn provision_is_idempotent_and_leaves_no_temp_behind() {
        let dir = support("idem");
        let a = concat!("gh", "p_aaaaaaaaaaaaaaaaaaaaaaaa");
        let b = concat!("gh", "p_bbbbbbbbbbbbbbbbbbbbbbbb");
        provision(&dir, a).unwrap();
        provision(&dir, a).unwrap();
        assert_eq!(from_file(&token_file(&dir)).as_deref(), Some(a));
        // Re-provisioning with a rotated token replaces it atomically.
        provision(&dir, b).unwrap();
        assert_eq!(from_file(&token_file(&dir)).as_deref(), Some(b));
        let leftovers: Vec<_> = std::fs::read_dir(&dir)
            .unwrap()
            .filter_map(|e| e.ok())
            .map(|e| e.file_name().to_string_lossy().into_owned())
            .filter(|n| n.ends_with(".tmp"))
            .collect();
        assert!(leftovers.is_empty(), "temp files left behind: {leftovers:?}");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn provision_refuses_a_malformed_value_without_echoing_it() {
        let dir = support("bad");
        for bad in ["", "   ", "not a token", "tok\"en", "gh auth: not logged in"] {
            let err = provision(&dir, bad).expect_err("{bad:?} must be refused");
            assert!(
                !err.contains(bad.trim()) || bad.trim().is_empty(),
                "the error must not echo the rejected value: {err}"
            );
            assert!(!token_file(&dir).exists(), "nothing may be written");
        }
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[cfg(unix)]
    #[test]
    fn file_probe_distinguishes_absent_from_rejected() {
        // The whole point of the Absent/Rejected split: "you never configured one"
        // and "you configured one and I threw it away" need different remedies, and
        // collapsing them is how a chmod 644 becomes a silently-never-updating Mac.
        let dir = support("probe");
        std::fs::create_dir_all(&dir).unwrap();
        let path = token_file(&dir);
        assert_eq!(probe_file(&path).outcome(), ProbeOutcome::Absent);

        std::fs::write(&path, concat!("gh", "p_0123456789abcdefABCDEF")).unwrap();
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o644)).unwrap();
        assert!(
            matches!(probe_file(&path).outcome(), ProbeOutcome::Rejected(_)),
            "a group/other-readable token file is REJECTED, not absent"
        );

        std::fs::write(&path, "not a token").unwrap();
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o600)).unwrap();
        assert!(
            matches!(probe_file(&path).outcome(), ProbeOutcome::Rejected(_)),
            "a malformed token file is REJECTED, not absent"
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn diagnose_walks_the_same_chain_resolve_does() {
        // Both go through `walk`, so they cannot disagree about which source wins or
        // how far the chain ran. This is the property that makes the diagnostic
        // trustworthy: it describes the executed chain, not a second copy of it.
        let dir = support("diag");
        let d = diagnose(&dir);
        let r = resolve_with_source(&dir);
        assert_eq!(d.resolved, r.as_ref().map(|(_, s)| *s));
        assert_eq!(d.is_provisioned(), r.is_some());
        // …and the single-walk combinator agrees with both, so the caller that must
        // explain a failure never has to walk (and re-spawn `security`/`gh`) twice.
        match (resolve_or_diagnose(&dir), &r) {
            (Ok((_, source)), Some((_, expected))) => assert_eq!(source, *expected),
            (Err(diagnosis), None) => {
                assert_eq!(diagnosis.probes, d.probes);
                assert!(!diagnosis.is_provisioned());
            }
            (got, want) => panic!(
                "resolve_or_diagnose disagreed with resolve_with_source: {:?} vs {:?}",
                got.is_ok(),
                want.is_some()
            ),
        }
        // The walk stops at the supplying source, so the probe list ends there.
        match d.resolved {
            Some(source) => {
                assert_eq!(d.probes.last().map(|p| p.source), Some(source));
                assert_eq!(d.probes.last().map(|p| &p.outcome), Some(&ProbeOutcome::Supplied));
            }
            None => {
                assert_eq!(d.probes.len(), 6, "an unprovisioned machine consults all 6");
                assert!(d.probes.iter().all(|p| p.outcome != ProbeOutcome::Supplied));
            }
        }
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn no_token_explanation_is_actionable_and_leaks_nothing() {
        let d = Diagnosis {
            resolved: None,
            probes: vec![
                SourceProbe {
                    source: "$ATERM_UPDATE_TOKEN",
                    outcome: ProbeOutcome::Absent,
                },
                SourceProbe {
                    source: "0600 update-token file",
                    outcome: ProbeOutcome::Rejected("chmod 600 it"),
                },
            ],
        };
        let text = d.no_token_explanation();
        assert!(text.contains("NEVER receive an update"), "{text}");
        assert!(text.contains(PROVISION_COMMAND), "{text}");
        assert!(
            text.contains("0600 update-token file (chmod 600 it)"),
            "the actionable rejection must be named: {text}"
        );
        assert_eq!(d.rejections().len(), 1, "absent sources are not rejections");
    }
}
