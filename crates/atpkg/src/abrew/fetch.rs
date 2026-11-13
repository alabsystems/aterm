// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates
//! SHA-256-verified `curl` downloads for abrew.

use std::io::Read;
use std::path::Path;
use std::process::Output;

/// The file no configuration is read from.
const NO_CONFIG: &str = if cfg!(windows) { "NUL" } else { "/dev/null" };

/// What every git child of abrew carries, so it asks nobody anything: no prompt on a terminal
/// (`GIT_TERMINAL_PROMPT=0`), no askpass program (an empty `GIT_ASKPASS` keeps a person's
/// `core.askPass` from running; git tries it before it looks at the terminal), no window from
/// ssh's askpass or Git Credential Manager, and no system configuration (on a Mac, the Command
/// Line Tools' file names the login keychain's credential helper, whose read can raise a
/// keychain dialog). A git that would have asked fails instead, and says so on its stderr.
const GIT_ASKS_NOBODY: &[(&str, &str)] = &[
    ("GIT_TERMINAL_PROMPT", "0"),
    ("GIT_ASKPASS", ""),
    ("SSH_ASKPASS", ""),
    ("SSH_ASKPASS_REQUIRE", "never"),
    ("GCM_INTERACTIVE", "never"),
    ("GIT_CONFIG_NOSYSTEM", "1"),
    ("GIT_CONFIG_SYSTEM", NO_CONFIG),
];

/// Whether `prog` names git, by its file name.
fn is_git(prog: &str) -> bool {
    Path::new(prog)
        .file_stem()
        .is_some_and(|name| name == "git")
}

/// The one git child abrew builds ([`GIT_ASKS_NOBODY`]): a read or write of a repository on
/// this disk, under the person's own git settings.
pub(super) fn git(args: &[&str]) -> aterm_uds::spawnfd::Command {
    command(aterm_uds::os_path!("git"), args)
}

/// [`git`] for a run that reaches a tap's remote or writes its checkout (clone, fetch, pull,
/// ls-remote): as the harness's own git, it reads no global configuration either
/// (`GIT_CONFIG_GLOBAL`, which also leaves `$XDG_CONFIG_HOME/git/config` unread), runs no hook
/// and no credential helper a repository's own file names, and has no stdin to answer from. A
/// private tap is reached by an SSH remote whose key the agent already holds.
pub(super) fn git_remote(args: &[&str]) -> aterm_uds::spawnfd::Command {
    let mut command = git(&["-c", "core.hooksPath=/dev/null", "-c", "credential.helper="]);
    command
        .args(args)
        .env("GIT_CONFIG_GLOBAL", NO_CONFIG)
        .stdin(aterm_uds::spawnfd::Stdio::null());
    command
}

/// The shared child construction keeps curl's default configuration out of both fetch paths,
/// and gives every git, however its program is spelled, [`GIT_ASKS_NOBODY`].
fn command(prog: &str, args: &[&str]) -> aterm_uds::spawnfd::Command {
    let mut command = aterm_uds::spawnfd::command(prog);
    if is_git(prog) {
        for (name, value) in GIT_ASKS_NOBODY {
            command.env(name, value);
        }
    }
    if prog == "curl" {
        command
            .arg("-q")
            .env_remove("CURL_HOME")
            .env_remove("XDG_CONFIG_HOME");
    }
    command.args(args);
    command
}

/// [`run_capture`] of a command built here ([`git_remote`]).
pub(super) fn capture(mut command: aterm_uds::spawnfd::Command) -> Result<Output, String> {
    command.output().map_err(|e| {
        format!(
            "abrew: cannot run {}: {e}",
            command.get_program().to_string_lossy()
        )
    })
}

/// [`run_inherit`] of a command built here ([`git_remote`]): its stdout and stderr are the
/// caller's.
pub(super) fn inherit(mut command: aterm_uds::spawnfd::Command) -> i32 {
    match command.status() {
        Ok(s) => s.code().unwrap_or(2),
        Err(e) => {
            eprintln!(
                "abrew: cannot run {}: {e}",
                command.get_program().to_string_lossy()
            );
            2
        }
    }
}

/// Run a child to completion, capturing output.
pub fn run_capture(prog: &str, args: &[&str]) -> Result<Output, String> {
    command(prog, args)
        .output()
        .map_err(|e| format!("abrew: cannot run {prog}: {e}"))
}

/// Run a child with inherited stdio (git, patch).
pub fn run_inherit(prog: &str, args: &[&str]) -> i32 {
    match command(prog, args).status() {
        Ok(s) => s.code().unwrap_or(2),
        Err(e) => {
            eprintln!("abrew: cannot run {prog}: {e}");
            2
        }
    }
}

/// The URL schemes abrew will ever fetch.
fn check_url(url: &str) -> Result<(), String> {
    if url.contains(['\r', '\n', '\0']) {
        return Err("abrew: refusing download URL containing control bytes".to_string());
    }
    let scheme = url.split("://").next().unwrap_or("");
    if matches!(scheme, "https" | "http" | "file") {
        Ok(())
    } else {
        Err(format!("abrew: refusing non-http(s) download URL: {url}"))
    }
}

/// Reject control bytes that could split HTTP headers or inject `curl -K` directives.
fn check_headers(headers: &[&str]) -> Result<(), String> {
    if headers.iter().any(|h| h.contains(['\r', '\n', '\0'])) {
        Err("abrew: refusing HTTP header containing CR, LF or NUL".to_string())
    } else {
        Ok(())
    }
}

/// Redirect protocols: https never downgrades.
fn proto_args(url: &str) -> [&'static str; 4] {
    if url.starts_with("https://") {
        ["--proto", "=https", "--proto-redir", "=https"]
    } else if url.starts_with("file://") {
        ["--proto", "=file", "--proto-redir", "=file"]
    } else {
        ["--proto", "=http,https", "--proto-redir", "=http,https"]
    }
}

/// GET a URL into memory (API JSON, tokens).
pub fn curl_get(url: &str, headers: &[&str]) -> Result<Vec<u8>, String> {
    curl_get_with_status(url, headers).map_err(GetError::into_message)
}

/// A missing package requires both curl's HTTP-failure exit and the final response status.
#[derive(Debug)]
pub(super) struct GetError {
    code: Option<i32>,
    status: Option<u16>,
    message: String,
}

impl GetError {
    pub(super) const fn other(message: String) -> Self {
        Self {
            code: None,
            status: None,
            message,
        }
    }

    pub(super) fn is_not_found(&self) -> bool {
        matches!(self.code, Some(22 | 56))
            && (self.status == Some(404) || self.message.ends_with("returned error: 404"))
    }

    pub(super) fn into_message(self) -> String {
        self.message
    }
}

/// Separate the fixed write-out suffix from the response body without decoding body bytes.
pub(super) fn parse_get_response(
    url: &str,
    code: Option<i32>,
    mut stdout: Vec<u8>,
    stderr: &[u8],
) -> Result<Vec<u8>, GetError> {
    let trailer = stdout.len().checked_sub(4).and_then(|at| {
        let bytes = &stdout[at..];
        if bytes[0] != b'\n' || !bytes[1..].iter().all(u8::is_ascii_digit) {
            return None;
        }
        let status = u16::from(bytes[1] - b'0') * 100
            + u16::from(bytes[2] - b'0') * 10
            + u16::from(bytes[3] - b'0');
        (status == 0 || (100..=599).contains(&status)).then_some((at, status))
    });
    if code == Some(0) {
        let Some((at, _)) = trailer else {
            return Err(GetError::other(format!(
                "abrew: download returned invalid response status ({url})"
            )));
        };
        stdout.truncate(at);
        return Ok(stdout);
    }
    Err(GetError {
        code,
        status: trailer.map(|(_, status)| status),
        message: format!(
            "abrew: download failed ({url}): {}",
            String::from_utf8_lossy(stderr).trim()
        ),
    })
}

pub(super) fn curl_get_with_status(url: &str, headers: &[&str]) -> Result<Vec<u8>, GetError> {
    check_url(url).map_err(GetError::other)?;
    check_headers(headers).map_err(GetError::other)?;
    let mut args: Vec<&str> = vec![
        "-fsSL",
        "--max-time",
        "60",
        "--max-filesize",
        "67108864",
        "--write-out",
        "\n%{http_code}",
    ];
    args.extend(proto_args(url));
    for h in headers {
        args.push("-H");
        args.push(h);
    }
    args.extend(["--", url]);
    let out = run_capture(aterm_uds::os_path!("curl"), &args).map_err(GetError::other)?;
    parse_get_response(url, out.status.code(), out.stdout, &out.stderr)
}

/// Download into an exclusively owned scratch beside `dest`, then rename it.
pub fn curl_to_file(url: &str, dest: &Path, headers: &[&str]) -> Result<(), String> {
    check_url(url)?;
    check_headers(headers)?;
    let parent = dest
        .parent()
        .filter(|p| !p.as_os_str().is_empty())
        .unwrap_or(Path::new("."));
    std::fs::create_dir_all(parent).map_err(|e| {
        format!(
            "abrew: cannot create download directory {}: {e}",
            parent.display()
        )
    })?;
    let work = aterm_tempfile::TempDir::new_in(parent)
        .map_err(|e| format!("abrew: cannot stage download in {}: {e}", parent.display()))?;
    let partial = work.path().join("download");
    let partial_s = partial.to_string_lossy().into_owned();
    let mut args: Vec<&str> = vec![
        "-fSL",
        "--connect-timeout",
        "30",
        "--speed-limit",
        "1024",
        "--speed-time",
        "120",
        "--max-filesize",
        "17179869184",
        "--progress-bar",
        "-o",
        &partial_s,
    ];
    // Brew passes --silent whenever stdout is not a tty, which
    // suppresses the progress bar there (scripts see clean output);
    // errors still surface via --show-error.
    {
        use std::io::IsTerminal as _;
        if !std::io::stdout().is_terminal() {
            args.push("--silent");
        }
        args.push("--show-error");
    }
    args.extend(proto_args(url));
    let header_cfg;
    if !headers.is_empty() {
        let cfg = work.path().join("curlrc");
        let mut body = String::new();
        for h in headers {
            body.push_str(&format!(
                "header = \"{}\"\n",
                h.replace('\\', "\\\\").replace('"', "\\\"")
            ));
        }
        crate::platform::open_create_new_write(&cfg, 0o600)
            .and_then(|mut out| std::io::Write::write_all(&mut out, body.as_bytes()))
            .map_err(|e| format!("abrew: cannot write {}: {e}", cfg.display()))?;
        header_cfg = cfg.to_string_lossy().into_owned();
        args.push("-K");
        args.push(&header_cfg);
    }
    args.extend(["--url", url]);
    let code = run_inherit(aterm_uds::os_path!("curl"), &args);
    if code != 0 {
        return Err(format!("abrew: download failed ({url}), curl exit {code}"));
    }
    std::fs::rename(&partial, dest)
        .map_err(|e| format!("abrew: cannot move download to {}: {e}", dest.display()))
}

/// The download size a HEAD request reports (final redirect
/// target's Content-Length), for upgrade dry-run tables. Best
/// effort: any failure simply omits the size, like brew when a
/// bottle has no recorded size. Headers ride a private curlrc,
/// as in [`curl_to_file`].
pub fn head_size(url: &str, headers: &[&str]) -> Option<u64> {
    check_url(url).ok()?;
    check_headers(headers).ok()?;
    let work = aterm_tempfile::tempdir().ok()?;
    let mut args: Vec<String> = vec![
        "-fsSIL".into(),
        "--connect-timeout".into(),
        "30".into(),
        "--max-time".into(),
        "60".into(),
    ];
    args.extend(proto_args(url).into_iter().map(str::to_owned));
    if !headers.is_empty() {
        let cfg = work.path().join("curlrc");
        let mut body = String::new();
        for h in headers {
            body.push_str(&format!(
                "header = \"{}\"\n",
                h.replace('\\', "\\\\").replace('"', "\\\"")
            ));
        }
        crate::platform::open_create_new_write(&cfg, 0o600)
            .and_then(|mut out| std::io::Write::write_all(&mut out, body.as_bytes()))
            .ok()?;
        args.push("-K".into());
        args.push(cfg.to_string_lossy().into_owned());
    }
    args.push("--url".into());
    args.push(url.to_string());
    let refs: Vec<&str> = args.iter().map(String::as_str).collect();
    let out = run_capture(aterm_uds::os_path!("curl"), &refs).ok()?;
    if !out.status.success() {
        return None;
    }
    String::from_utf8_lossy(&out.stdout)
        .lines()
        .filter_map(|l| {
            let (k, v) = l.split_once(':')?;
            let v = v.trim();
            if k.trim().eq_ignore_ascii_case("content-length") && !v.starts_with('+') {
                v.parse::<u64>().ok()
            } else {
                None
            }
        })
        .next_back()
}

/// SHA-256 of a file, lowercase hex (ring, streamed).
pub fn sha256_file(path: &Path) -> Result<String, String> {
    let f = crate::metadata_io::open_regular(path)
        .map_err(|e| format!("abrew: cannot read {}: {e}", path.display()))?;
    crate::tree::sha256_opened(f).map_err(|e| format!("abrew: cannot read {}: {e}", path.display()))
}

/// SHA-256 of bytes, lowercase hex.
#[must_use]
pub fn sha256_bytes(bytes: &[u8]) -> String {
    hex(ring::digest::digest(&ring::digest::SHA256, bytes).as_ref())
}

fn hex(bytes: &[u8]) -> String {
    aterm_codec::hex::encode_lower(bytes)
}

/// Verify a regular, non-symlink cache entry.
pub fn verify_sha256(path: &Path, expected: &str) -> Result<(), String> {
    let f = crate::metadata_io::open_regular(path).map_err(|_| {
        format!(
            "abrew: cache entry is not a regular file: {}",
            path.display()
        )
    })?;
    if expected == "no_check" {
        return Ok(());
    }
    if expected.len() != 64 || !expected.bytes().all(|b| b.is_ascii_hexdigit()) {
        return Err("abrew: missing or malformed SHA-256".to_string());
    }
    let got = crate::tree::sha256_opened(f)
        .map_err(|e| format!("abrew: cannot read {}: {e}", path.display()))?;
    if got.eq_ignore_ascii_case(expected) {
        Ok(())
    } else {
        Err(format!(
            "abrew: SHA-256 mismatch for {}\n  expected: {expected}\n  actual:   {got}",
            path.display()
        ))
    }
}

/// Like [`verify_sha256`] but with no `no_check` exemption.
pub fn verify_digest_strict(path: &Path, expected: &str) -> Result<(), String> {
    if expected == "no_check" {
        return Err("abrew: no_check is not a bottle digest".to_string());
    }
    verify_sha256(path, expected)
}

/// Archives handled by the shared, bounded extractor.
pub(super) fn archive_supported(name: &str) -> bool {
    let lower = name.to_ascii_lowercase();
    [".tar", ".tar.gz", ".tgz", ".tar.zst", ".zip"]
        .iter()
        .any(|ext| lower.ends_with(ext))
}

/// Whether a download name names a macOS disk image.
pub(super) fn is_dmg(name: &str) -> bool {
    name.to_ascii_lowercase().ends_with(".dmg")
}

#[cfg(any(target_os = "macos", all(test, unix)))]
fn copy_mounted_tree(src_root: &Path, dir: &Path) -> Result<(), String> {
    use crate::install::{MAX_ENTRIES, UNSIGNED_SIZE_CAP};
    std::fs::create_dir(dir).map_err(|e| format!("abrew: {e}"))?;
    crate::platform::set_mode(dir, crate::extract::safe_mode(0, true))
        .map_err(|e| format!("abrew: {e}"))?;
    let mut entries: u64 = 0;
    let mut bytes: u64 = 0;
    let mut stack: Vec<std::path::PathBuf> = vec![std::path::PathBuf::new()];
    while let Some(rel) = stack.pop() {
        let src_dir = src_root.join(&rel);
        let read = std::fs::read_dir(&src_dir).map_err(|e| format!("abrew: {e}"))?;
        for entry in read {
            let entry = entry.map_err(|e| format!("abrew: {e}"))?;
            entries += 1;
            if entries > MAX_ENTRIES {
                return Err("abrew: dmg entry limit exceeded".to_string());
            }
            let kind = entry.file_type().map_err(|e| format!("abrew: {e}"))?;
            let rel_path = rel.join(entry.file_name());
            let dst = dir.join(&rel_path);
            if kind.is_dir() {
                std::fs::create_dir(&dst).map_err(|e| format!("abrew: {e}"))?;
                crate::platform::set_mode(&dst, crate::extract::safe_mode(0, true))
                    .map_err(|e| format!("abrew: {e}"))?;
                stack.push(rel_path);
            } else if kind.is_symlink() {
                let target = std::fs::read_link(entry.path()).map_err(|e| format!("abrew: {e}"))?;
                if matches!(
                    crate::extract::vet_symlink(dir, &rel_path, &target, 0),
                    Ok(Some(_))
                ) {
                    super::prefix::symlink_file(&target, &dst)?;
                }
            } else if kind.is_file() {
                let (source, meta) = crate::metadata_io::open_regular_with_meta(&entry.path())
                    .map_err(|e| format!("abrew: cannot read payload: {e}"))?;
                let remaining = UNSIGNED_SIZE_CAP.saturating_sub(bytes);
                if meta.len() > remaining {
                    return Err("abrew: dmg size limit exceeded".to_string());
                }
                let mut dest = crate::platform::open_create_new_write(&dst, 0o644)
                    .map_err(|e| format!("abrew: cannot stage payload: {e}"))?;
                let copied =
                    std::io::copy(&mut source.take(remaining.saturating_add(1)), &mut dest)
                        .map_err(|e| format!("abrew: cannot stage payload: {e}"))?;
                bytes = bytes.saturating_add(copied);
                if bytes > UNSIGNED_SIZE_CAP {
                    return Err("abrew: dmg size limit exceeded".to_string());
                }
                crate::platform::set_mode_on(
                    &dest,
                    crate::extract::safe_mode(crate::platform::permission_mode(&meta), false),
                )
                .map_err(|e| format!("abrew: {e}"))?;
            } else {
                return Err(format!(
                    "abrew: dmg contains a nonregular entry: {}",
                    rel_path.display()
                ));
            }
        }
    }
    Ok(())
}

/// A disk-image failure's origin, independent of payload names in its text.
#[derive(Debug)]
pub(super) enum DmgError {
    Attach(String),
    #[cfg(any(target_os = "macos", test))]
    Payload(String),
    #[cfg(any(target_os = "macos", test))]
    Detach,
    #[cfg(any(target_os = "macos", test))]
    Uncertain(String),
}

impl std::fmt::Display for DmgError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Attach(message) => write!(f, "abrew: dmg attach failed: {message}"),
            #[cfg(any(target_os = "macos", test))]
            Self::Payload(message) => f.write_str(message),
            #[cfg(any(target_os = "macos", test))]
            Self::Detach => f.write_str("abrew: dmg detach failed"),
            #[cfg(any(target_os = "macos", test))]
            Self::Uncertain(message) => f.write_str(message),
        }
    }
}

#[cfg(any(target_os = "macos", all(test, unix)))]
const DMG_ATTACH_LIMIT: std::time::Duration = std::time::Duration::from_secs(5 * 60);
#[cfg(any(target_os = "macos", all(test, unix)))]
const DMG_DETACH_LIMIT: std::time::Duration = std::time::Duration::from_secs(60);
#[cfg(target_os = "macos")]
const DMG_REAP_LIMIT: std::time::Duration = std::time::Duration::from_secs(5);

/// Retention is the default even if a backstop panics.
#[cfg(any(target_os = "macos", all(test, unix)))]
struct RetainedImage(Option<aterm_tempfile::TempDir>);

#[cfg(any(target_os = "macos", all(test, unix)))]
impl Drop for RetainedImage {
    fn drop(&mut self) {
        if let Some(root) = self.0.take() {
            let _ = root.keep();
        }
    }
}

#[cfg(any(target_os = "macos", all(test, unix)))]
#[derive(Clone, Copy)]
enum DmgStep {
    Attach,
    Detach,
    ForceDetach,
}

/// Own the image actually passed to attach, as well as its mountpoint.
#[cfg(any(target_os = "macos", all(test, unix)))]
struct MountGuard<'a, F>
where
    F: FnMut(
        &mut aterm_uds::spawnfd::Command,
        std::time::Duration,
    ) -> Result<Output, aterm_uds::deadline::RunError>,
{
    root: RetainedImage,
    image: std::path::PathBuf,
    point: std::path::PathBuf,
    run: &'a mut F,
    needs_detach: bool,
    uncertain: Option<&'static str>,
}

#[cfg(any(target_os = "macos", all(test, unix)))]
impl<F> MountGuard<'_, F>
where
    F: FnMut(
        &mut aterm_uds::spawnfd::Command,
        std::time::Duration,
    ) -> Result<Output, aterm_uds::deadline::RunError>,
{
    fn helper(&mut self, step: DmgStep) -> Result<Output, aterm_uds::deadline::RunError> {
        let was_uncertain = self.uncertain;
        self.uncertain = Some("helper completion was not observed");
        let mut cmd = aterm_uds::spawnfd::command("/usr/bin/hdiutil");
        let within = match step {
            DmgStep::Attach => {
                cmd.args([
                    "attach",
                    "-readonly",
                    "-nobrowse",
                    "-noautoopen",
                    "-mountpoint",
                ])
                .arg(&self.point)
                .arg(&self.image);
                DMG_ATTACH_LIMIT
            }
            DmgStep::Detach | DmgStep::ForceDetach => {
                cmd.arg("detach");
                if matches!(step, DmgStep::ForceDetach) {
                    cmd.arg("-force");
                }
                cmd.arg("-quiet").arg(&self.point);
                DMG_DETACH_LIMIT
            }
        };
        let answer = (self.run)(&mut cmd, within);
        self.uncertain = was_uncertain.or_else(|| {
            answer
                .is_err()
                .then_some("helper completion was not observed")
        });
        answer
    }

    fn cleanup(&mut self) -> bool {
        if self.uncertain.is_some() {
            return false;
        }
        if !self.needs_detach {
            return true;
        }
        if self
            .helper(DmgStep::ForceDetach)
            .is_ok_and(|out| out.status.success())
        {
            self.needs_detach = false;
            true
        } else {
            if self.uncertain.is_none() {
                self.uncertain = Some("resource cleanup was not confirmed");
            }
            false
        }
    }

    fn finish(mut self, result: Result<(), DmgError>) -> Result<(), DmgError> {
        if self.cleanup() {
            return result;
        }
        let prior = result.err().map_or_else(String::new, |e| format!("{e}; "));
        let why = self
            .uncertain
            .unwrap_or("resource cleanup was not confirmed");
        Err(DmgError::Uncertain(format!(
            "{prior}abrew: dmg {why}; retained image and mountpoint in {}",
            self.image.parent().unwrap_or(&self.image).display()
        )))
    }
}

#[cfg(any(target_os = "macos", all(test, unix)))]
impl<F> Drop for MountGuard<'_, F>
where
    F: FnMut(
        &mut aterm_uds::spawnfd::Command,
        std::time::Duration,
    ) -> Result<Output, aterm_uds::deadline::RunError>,
{
    fn drop(&mut self) {
        if self.cleanup() {
            drop(self.root.0.take());
        }
    }
}

#[cfg(target_os = "macos")]
fn run_dmg_helper(
    cmd: &mut aterm_uds::spawnfd::Command,
    within: std::time::Duration,
) -> Result<Output, aterm_uds::deadline::RunError> {
    run_dmg_helper_started(cmd, within, std::time::Instant::now())
}

#[cfg(target_os = "macos")]
fn run_dmg_helper_started(
    cmd: &mut aterm_uds::spawnfd::Command,
    within: std::time::Duration,
    started: std::time::Instant,
) -> Result<Output, aterm_uds::deadline::RunError> {
    let deadline = started
        .checked_add(within)
        .unwrap_or_else(|| started + std::time::Duration::from_secs(365 * 86_400));
    if std::time::Instant::now() >= deadline {
        return Err(aterm_uds::deadline::RunError::TimedOut);
    }
    aterm_uds::deadline::outputs_until(&mut [cmd], deadline, DMG_REAP_LIMIT)
        .pop()
        .unwrap_or(Err(aterm_uds::deadline::RunError::TimedOut))
}

/// Extract a disk image by mounting it read-only and copying the payload out with
/// [`copy_mounted_tree`].
#[cfg(target_os = "macos")]
pub(super) fn extract_dmg(dmg: &Path, dir: &Path) -> Result<(), DmgError> {
    let root = aterm_tempfile::tempdir().map_err(|e| DmgError::Attach(e.to_string()))?;
    extract_dmg_with(dmg, dir, root, run_dmg_helper)
}

#[cfg(any(target_os = "macos", all(test, unix)))]
fn extract_dmg_with<F>(
    dmg: &Path,
    dir: &Path,
    root: aterm_tempfile::TempDir,
    mut run: F,
) -> Result<(), DmgError>
where
    F: FnMut(
        &mut aterm_uds::spawnfd::Command,
        std::time::Duration,
    ) -> Result<Output, aterm_uds::deadline::RunError>,
{
    let image = root.path().join("image.dmg");
    copy_regular(dmg, &image).map_err(DmgError::Attach)?;
    let mount_path = root.path().join("mount");
    std::fs::create_dir(&mount_path).map_err(|e| DmgError::Attach(e.to_string()))?;
    let mut guard = MountGuard {
        root: RetainedImage(Some(root)),
        image,
        point: mount_path.clone(),
        run: &mut run,
        needs_detach: true,
        uncertain: None,
    };
    let out = match guard.helper(DmgStep::Attach) {
        Ok(out) => out,
        Err(e) => return guard.finish(Err(DmgError::Attach(e.to_string()))),
    };
    if !out.status.success() {
        return guard.finish(Err(DmgError::Attach(
            String::from_utf8_lossy(&out.stderr).trim().to_string(),
        )));
    }
    let copied = copy_mounted_tree(&mount_path, dir).map_err(DmgError::Payload);
    let detached = guard.helper(DmgStep::Detach);
    // A volume another process holds for a moment (a system scanner reading the payload just
    // copied, more often on a loaded Mac) refuses the plain detach busy. The forced detach in
    // `finish` then takes it down, and only one it does not confirm leaves anything behind.
    let busy = detached
        .as_ref()
        .is_ok_and(|out| out.status.code() == Some(libc::EBUSY));
    let detached = detached.is_ok_and(|out| out.status.success());
    if detached {
        guard.needs_detach = false;
    }
    let result = copied.and(if detached || busy {
        Ok(())
    } else {
        Err(DmgError::Detach)
    });
    guard.finish(result)
}

#[cfg(not(target_os = "macos"))]
pub(super) fn extract_dmg(_dmg: &Path, _dir: &Path) -> Result<(), DmgError> {
    Err(DmgError::Attach("disk images need macOS".to_string()))
}

/// Copy one regular file, refusing a symlink or any other non-regular source.
pub(super) fn copy_regular(src: &Path, dst: &Path) -> Result<(), String> {
    let meta = std::fs::symlink_metadata(src).map_err(|e| format!("abrew: {e}"))?;
    if !meta.file_type().is_file() {
        return Err("abrew: naked payload is not a regular file".to_string());
    }
    crate::metadata_io::copy_regular_file(src, dst)
        .map(|_| ())
        .map_err(|e| format!("abrew: cannot stage payload: {e}"))
}

pub fn untar(archive: &Path, dir: &Path) -> Result<(), String> {
    extract_archive(
        archive,
        dir,
        crate::extract::ExtractOptions {
            strip_components: 0,
            symlinks: crate::extract::SymlinkPolicy::InRoot,
            allow_global_pax: true,
        },
    )
}

fn extract_archive(
    archive: &Path,
    dir: &Path,
    opts: crate::extract::ExtractOptions,
) -> Result<(), String> {
    use crate::extract::{
        FrameChecksum, extract_tar_gz_tree, extract_tar_tree, extract_tar_zst_tree,
        extract_zip_tree,
    };
    use crate::install::{MAX_ENTRIES, UNSIGNED_SIZE_CAP};
    let mut file = crate::metadata_io::open_regular(archive).map_err(|e| format!("abrew: {e}"))?;
    let mut magic = [0u8; 6];
    file.read_exact(&mut magic)
        .map_err(|e| format!("abrew: {e}"))?;
    if magic.starts_with(b"BZh") || magic == *b"\xfd7zXZ\0" {
        return Err("abrew: unsupported archive; use brew for this format".to_string());
    }
    match std::fs::symlink_metadata(dir) {
        Ok(m) if !m.file_type().is_dir() => {
            return Err(format!(
                "abrew: extraction root is not a physical directory: {}",
                dir.display()
            ));
        }
        Ok(_) => {}
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
        Err(e) => return Err(format!("abrew: cannot inspect {}: {e}", dir.display())),
    }
    let result = if magic.starts_with(&[0x1f, 0x8b]) {
        extract_tar_gz_tree(archive, dir, UNSIGNED_SIZE_CAP, MAX_ENTRIES, opts)
    } else if magic.starts_with(&[0x28, 0xb5, 0x2f, 0xfd]) {
        extract_tar_zst_tree(
            archive,
            dir,
            UNSIGNED_SIZE_CAP,
            MAX_ENTRIES,
            opts,
            FrameChecksum::IfPresent,
        )
    } else if magic.starts_with(b"PK") {
        extract_zip_tree(archive, dir, UNSIGNED_SIZE_CAP, MAX_ENTRIES, opts)
    } else {
        extract_tar_tree(archive, dir, UNSIGNED_SIZE_CAP, MAX_ENTRIES, opts)
    };
    result
        .map(|_| ())
        .map_err(|e| format!("abrew: cannot extract {}: {e}", archive.display()))
}

#[cfg(test)]
#[path = "fetch_tests.rs"]
mod tests;
