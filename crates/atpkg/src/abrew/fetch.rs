// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates
//! Fetching: `curl` is abrew's one downloader (design §3), always
//! SHA-256-verified before installation unless a cask explicitly says `no_check`.
//! Children are built with
//! `aterm_uds::spawnfd::command` like every child in this crate.

use std::io::Read;
use std::path::Path;
use std::process::Output;

/// The shared child construction keeps curl's default configuration out of
/// both fetch paths. `-q` must be its first argument; the explicit private
/// header config remains an ordinary later option. Other tools are unchanged.
fn command(prog: &str, args: &[&str]) -> aterm_uds::spawnfd::Command {
    let mut command = aterm_uds::spawnfd::command(prog);
    if prog == "curl" {
        command
            .arg("-q")
            .env_remove("CURL_HOME")
            .env_remove("XDG_CONFIG_HOME");
    }
    command.args(args);
    command
}

/// Run a child to completion, capturing output. The command is named in
/// every error so a failure says what to run by hand.
pub fn run_capture(prog: &str, args: &[&str]) -> Result<Output, String> {
    command(prog, args)
        .output()
        .map_err(|e| format!("abrew: cannot run {prog}: {e}"))
}

/// Run a child with inherited stdio (git, patch); returns
/// its exit code (2 when it could not be started or was signalled).
pub fn run_inherit(prog: &str, args: &[&str]) -> i32 {
    match command(prog, args).status() {
        Ok(s) => s.code().unwrap_or(2),
        Err(e) => {
            eprintln!("abrew: cannot run {prog}: {e}");
            2
        }
    }
}

/// The URL schemes abrew will ever fetch. `file` exists for tests and
/// local mirrors; everything else (ftp, ext, dict, …) is refused here,
/// at the one fetcher, before curl runs.
fn check_url(url: &str) -> Result<(), String> {
    let scheme = url.split("://").next().unwrap_or("");
    if matches!(scheme, "https" | "http" | "file") {
        Ok(())
    } else {
        Err(format!("abrew: refusing non-http(s) download URL: {url}"))
    }
}

/// Redirect protocols: https never downgrades; http/file stay themselves.
fn proto_args(url: &str) -> [&'static str; 4] {
    if url.starts_with("https://") {
        ["--proto", "=https", "--proto-redir", "=https"]
    } else if url.starts_with("file://") {
        ["--proto", "=file", "--proto-redir", "=file"]
    } else {
        ["--proto", "=http,https", "--proto-redir", "=http,https"]
    }
}

/// GET a URL into memory (API JSON, tokens). Size-capped by curl
/// itself (64 MiB — the API ceiling api.rs enforces again); a 60 s
/// ceiling is fine at that size.
pub fn curl_get(url: &str, headers: &[&str]) -> Result<Vec<u8>, String> {
    curl_get_with_status(url, headers).map_err(GetError::into_message)
}

/// A missing package requires both curl's HTTP-failure exit and the final
/// response status. URLs and diagnostic text never establish that status.
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
        self.code == Some(22) && self.status == Some(404)
    }

    pub(super) fn into_message(self) -> String {
        self.message
    }
}

/// Separate the fixed write-out suffix from the response body without decoding
/// body bytes. Missing or malformed metadata cannot certify an HTTP absence.
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
    let out = run_capture("curl", &args).map_err(GetError::other)?;
    parse_get_response(url, out.status.code(), out.stdout, &out.stderr)
}

/// Download into an exclusively owned scratch beside `dest`, then rename it.
/// Failed or concurrent downloads never remove another operation's partial file.
pub fn curl_to_file(url: &str, dest: &Path, headers: &[&str]) -> Result<(), String> {
    check_url(url)?;
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
    // No total time ceiling (large bottles on slow links are legit);
    // a stall ceiling and the extractor's 16 GiB size cap bound it.
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
    args.extend(proto_args(url));
    // Headers travel in a private config file, never argv: a bearer
    // token in `-H` is ps-visible for the whole download.
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
        std::fs::write(&cfg, body)
            .map_err(|e| format!("abrew: cannot write {}: {e}", cfg.display()))?;
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let _ = std::fs::set_permissions(&cfg, std::fs::Permissions::from_mode(0o600));
        }
        header_cfg = cfg.to_string_lossy().into_owned();
        args.push("-K");
        args.push(&header_cfg);
    }
    args.extend(["--url", url]);
    let code = run_inherit("curl", &args);
    if code != 0 {
        return Err(format!("abrew: download failed ({url}), curl exit {code}"));
    }
    std::fs::rename(&partial, dest)
        .map_err(|e| format!("abrew: cannot move download to {}: {e}", dest.display()))
}

/// SHA-256 of a file, lowercase hex (ring, streamed).
pub fn sha256_file(path: &Path) -> Result<String, String> {
    let mut f = std::fs::File::open(path)
        .map_err(|e| format!("abrew: cannot read {}: {e}", path.display()))?;
    let mut ctx = ring::digest::Context::new(&ring::digest::SHA256);
    let mut buf = [0u8; 65536];
    loop {
        let n = f
            .read(&mut buf)
            .map_err(|e| format!("abrew: cannot read {}: {e}", path.display()))?;
        if n == 0 {
            break;
        }
        if let Some(chunk) = buf.get(..n) {
            ctx.update(chunk);
        }
    }
    Ok(hex(ctx.finish().as_ref()))
}

/// SHA-256 of bytes, lowercase hex.
#[must_use]
pub fn sha256_bytes(bytes: &[u8]) -> String {
    hex(ring::digest::digest(&ring::digest::SHA256, bytes).as_ref())
}

fn hex(bytes: &[u8]) -> String {
    let mut s = String::with_capacity(bytes.len() * 2);
    for b in bytes {
        s.push_str(&format!("{b:02x}"));
    }
    s
}

/// Verify a regular, non-symlink cache entry. Only explicit `no_check`
/// bypasses the digest comparison; it still requires an actual file.
pub fn verify_sha256(path: &Path, expected: &str) -> Result<(), String> {
    if !std::fs::symlink_metadata(path).is_ok_and(|m| m.file_type().is_file()) {
        return Err(format!(
            "abrew: cache entry is not a regular file: {}",
            path.display()
        ));
    }
    if expected == "no_check" {
        return Ok(());
    }
    if expected.len() != 64 || !expected.bytes().all(|b| b.is_ascii_hexdigit()) {
        return Err("abrew: missing or malformed SHA-256".to_string());
    }
    let got = sha256_file(path)?;
    if got.eq_ignore_ascii_case(expected) {
        Ok(())
    } else {
        Err(format!(
            "abrew: SHA-256 mismatch for {}\n  expected: {expected}\n  actual:   {got}",
            path.display()
        ))
    }
}

/// Like [`verify_sha256`] but with no `no_check` exemption: bottle and
/// fetch paths must carry a real 64-hex digest. The exemption is a
/// cask-only spelling and must never pour a bottle unverified.
pub fn verify_digest_strict(path: &Path, expected: &str) -> Result<(), String> {
    if expected == "no_check" {
        return Err("abrew: no_check is not a bottle digest".to_string());
    }
    verify_sha256(path, expected)
}

/// Archives handled by the shared, bounded extractor. Other formats need brew.
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

/// Bounded copy of a mounted image's tree into `dir`, under the
/// same entry/byte ceilings and in-root symlink vetting as archive
/// extraction. A symlink the vetting refuses (the customary
/// `Applications -> /Applications` Finder alias) is skipped, not
/// followed and not fatal; the cask artifact census still judges
/// what must be present afterwards. Anything that is not a
/// directory, regular file or symlink is a hard refusal.
#[cfg(any(target_os = "macos", all(test, unix)))]
fn copy_mounted_tree(src_root: &Path, dir: &Path) -> Result<(), String> {
    use crate::install::{MAX_ENTRIES, UNSIGNED_SIZE_CAP};
    // The stage and every entry are exclusive: a case-sensitive image may
    // contain names that alias on the destination filesystem. Reusing an
    // earlier symlink as a directory would invalidate the lexical link check.
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
                let meta = entry.metadata().map_err(|e| format!("abrew: {e}"))?;
                let remaining = UNSIGNED_SIZE_CAP.saturating_sub(bytes);
                if meta.len() > remaining {
                    return Err("abrew: dmg size limit exceeded".to_string());
                }
                let source = std::fs::File::open(entry.path())
                    .map_err(|e| format!("abrew: cannot read payload: {e}"))?;
                let mut dest = std::fs::OpenOptions::new()
                    .write(true)
                    .create_new(true)
                    .open(&dst)
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

// An image's verification and mount may take substantially longer than a small
// tool probe. Detach has no payload to process. These bound helper waits, not
// filesystem copies, spawn itself, or work in macOS's disk-image service.
#[cfg(any(target_os = "macos", all(test, unix)))]
const DMG_ATTACH_LIMIT: std::time::Duration = std::time::Duration::from_secs(5 * 60);
#[cfg(any(target_os = "macos", all(test, unix)))]
const DMG_DETACH_LIMIT: std::time::Duration = std::time::Duration::from_secs(30);
#[cfg(target_os = "macos")]
const DMG_REAP_LIMIT: std::time::Duration = std::time::Duration::from_secs(5);

/// Retention is the default even if a backstop panics. Only confirmed cleanup
/// takes the TempDir out of this wrapper and permits its recursive deletion.
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

/// Own the image actually passed to attach, as well as its mountpoint. A cache
/// eviction or a caller's TempDir drop cannot remove an uncertain helper's input.
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
        // Arm before command construction or the runner: unwinding is not a
        // completed helper, and outputs_until's error is not a reap certificate.
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
            // A late attach can follow a successful detach. Another helper
            // cannot turn that unknown operation into permission to delete.
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
            // No second cleanup attempt in Drop and no delegation while the
            // mount's cleanup is unconfirmed, even after a completed refusal.
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
    // Unix's poll-based runner starts no reader threads, caps both output
    // tails, and bounds its SIGKILL/reap reserve. An error remains unknown:
    // a child may be handed to the reaper, and OS services are not its group.
    aterm_uds::deadline::outputs_until(
        &mut [cmd],
        std::time::Instant::now() + within,
        DMG_REAP_LIMIT,
    )
    .pop()
    .unwrap_or(Err(aterm_uds::deadline::RunError::TimedOut))
}

/// Extract a disk image by mounting it read-only and copying the
/// payload out under the bounded, vetted copy above. No license is
/// ever agreed to (stdin is /dev/null: an image that demands one
/// fails the attach), and nothing on the image runs. Detach
/// is bounded on every known exit. Unknown helpers retain their actual image
/// input and mount root; they never delegate or race another cleanup helper.
/// Once attach starts, only completed refusals with completed cleanup delegate.
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
    let detached = detached.is_ok_and(|out| out.status.success());
    if detached {
        guard.needs_detach = false;
    }
    let result = copied.and(if detached {
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

/// Extract through atpkg's entry-by-entry path and symlink admission. The
/// unsigned-payload byte and entry ceilings are shared with vendor installs.
/// Copy one already-verified regular cache file (naked cask payloads).
pub(super) fn copy_regular(src: &Path, dst: &Path) -> Result<(), String> {
    let meta = std::fs::symlink_metadata(src).map_err(|e| format!("abrew: {e}"))?;
    if !meta.file_type().is_file() {
        return Err("abrew: naked payload is not a regular file".to_string());
    }
    std::fs::copy(src, dst).map_err(|e| format!("abrew: cannot stage payload: {e}"))?;
    Ok(())
}

pub fn untar(archive: &Path, dir: &Path) -> Result<(), String> {
    extract_archive(
        archive,
        dir,
        crate::extract::ExtractOptions {
            strip_components: 0,
            symlinks: crate::extract::SymlinkPolicy::InRoot,
            // Descriptive global PAX metadata is common in real cask
            // tarballs (GitHub source archives); the extractor validates
            // it as metadata-only before skipping it.
            allow_global_pax: true,
        },
    )
}

/// Homebrew's checksum-verified source archive carries descriptive global PAX
/// metadata and intentional external links. Preserve them as leaves; the same
/// extractor refuses any later write on or through a link.
pub(super) fn untar_homebrew_bundle(archive: &Path, dir: &Path) -> Result<(), String> {
    extract_archive(
        archive,
        dir,
        crate::extract::ExtractOptions {
            strip_components: 0,
            symlinks: crate::extract::SymlinkPolicy::ExternalLeaf,
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
    let mut file = std::fs::File::open(archive).map_err(|e| format!("abrew: {e}"))?;
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
mod tests {
    use super::*;

    #[test]
    fn download_setup_errors_name_the_path() {
        let tmp = aterm_tempfile::tempdir().unwrap();
        // A dest whose parent is a regular file fails before any
        // network use; the error must name that parent, not just
        // relay a bare OS error.
        let blocker = tmp.path().join("blocker");
        std::fs::write(&blocker, b"file").unwrap();
        let dest = blocker.join("bottle.tar.gz");
        let error = curl_to_file("https://example.invalid/bottle.tar.gz", &dest, &[])
            .expect_err("setup must fail");
        assert!(
            error.contains(&blocker.display().to_string()),
            "error names the path: {error}"
        );
    }

    #[test]
    fn sha256_of_known_bytes() {
        assert_eq!(
            sha256_bytes(b"abc"),
            "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad"
        );
    }

    #[test]
    #[cfg(unix)]
    fn unchecked_cache_still_requires_a_file_and_downloads_keep_foreign_partials() {
        let tmp = aterm_tempfile::tempdir().unwrap();
        let dest = tmp.path().join("download");
        assert!(verify_sha256(&dest, "no_check").is_err());
        std::fs::write(&dest, b"old").unwrap();
        assert!(verify_sha256(&dest, "no_check").is_ok());
        assert!(verify_sha256(&dest, "").is_err());
        let partial = tmp.path().join("download.partial");
        std::fs::write(&partial, b"another operation").unwrap();
        let source = tmp.path().join("source");
        std::fs::write(&source, b"new").unwrap();
        curl_to_file(&format!("file://{}", source.display()), &dest, &[]).unwrap();
        assert_eq!(std::fs::read(&dest).unwrap(), b"new");
        assert_eq!(std::fs::read(&partial).unwrap(), b"another operation");
        assert!(
            curl_to_file(
                &format!("file://{}", tmp.path().join("absent").display()),
                &dest,
                &[]
            )
            .is_err()
        );
        assert_eq!(std::fs::read(&dest).unwrap(), b"new");
        assert_eq!(std::fs::read(&partial).unwrap(), b"another operation");
        assert_eq!(std::fs::read_dir(tmp.path()).unwrap().count(), 3);
    }

    #[cfg(unix)]
    #[test]
    fn mounted_tree_copy_skips_escaping_alias_and_keeps_in_root_links() {
        use std::os::unix::fs::PermissionsExt as _;
        let tmp = aterm_tempfile::tempdir().unwrap();
        let src = tmp.path().join("mnt");
        let dst = tmp.path().join("out");
        std::fs::create_dir_all(src.join("Demo.app/Contents")).unwrap();
        std::fs::write(src.join("Demo.app/Contents/Info.plist"), b"plist").unwrap();
        std::fs::write(src.join("Demo.app/Contents/tool"), b"executable payload").unwrap();
        std::fs::set_permissions(
            src.join("Demo.app/Contents/Info.plist"),
            std::fs::Permissions::from_mode(0o666),
        )
        .unwrap();
        std::fs::set_permissions(
            src.join("Demo.app/Contents/tool"),
            std::fs::Permissions::from_mode(0o777),
        )
        .unwrap();
        std::fs::set_permissions(
            src.join("Demo.app/Contents"),
            std::fs::Permissions::from_mode(0o555),
        )
        .unwrap();
        super::super::prefix::symlink_dir(Path::new("/Applications"), &src.join("Applications"))
            .unwrap();
        super::super::prefix::symlink_file(
            Path::new("Demo.app/Contents/Info.plist"),
            &src.join("alias"),
        )
        .unwrap();
        let copied = copy_mounted_tree(&src, &dst);
        std::fs::set_permissions(
            src.join("Demo.app/Contents"),
            std::fs::Permissions::from_mode(0o755),
        )
        .unwrap();
        copied.unwrap();
        for (path, mode) in [
            ("", 0o755),
            ("Demo.app/Contents", 0o755),
            ("Demo.app/Contents/Info.plist", 0o644),
            ("Demo.app/Contents/tool", 0o755),
        ] {
            assert_eq!(
                std::fs::metadata(dst.join(path))
                    .unwrap()
                    .permissions()
                    .mode()
                    & 0o7777,
                mode,
                "{path} uses the staged tree's safe mode"
            );
        }
        assert_eq!(
            std::fs::read(dst.join("Demo.app/Contents/Info.plist")).unwrap(),
            b"plist"
        );
        assert!(
            std::fs::symlink_metadata(dst.join("Applications")).is_err(),
            "escaping Finder alias not copied"
        );
        assert_eq!(std::fs::read(dst.join("alias")).unwrap(), b"plist");
    }

    #[cfg(unix)]
    #[test]
    fn dmg_helpers_retain_uncertain_inputs_until_cleanup_is_confirmed() {
        use std::os::unix::process::ExitStatusExt as _;
        use std::panic::{AssertUnwindSafe, catch_unwind};

        fn answer(code: i32) -> Output {
            Output {
                status: std::process::ExitStatus::from_raw(code << 8),
                stdout: Vec::new(),
                stderr: b"unsupported image".to_vec(),
            }
        }

        let tmp = aterm_tempfile::tempdir().unwrap();
        let original = tmp.path().join("caller.dmg");
        for scenario in [
            "copied",
            "refused",
            "attach-unknown",
            "detach-unknown",
            "backstop-unknown",
            "backstop-refused",
            "detach-refused",
            "attach-panic",
        ] {
            std::fs::write(&original, b"owned image bytes").unwrap();
            let out = tmp.path().join(scenario);
            // The outer guard owns every retained ordinary-file root even
            // if an assertion unwinds. This injected runner never mounts or
            // starts a real helper, so its roots can safely be reclaimed.
            let scratch = aterm_tempfile::TempDir::new_in(tmp.path()).unwrap();
            let root = scratch.path().to_path_buf();
            let mut calls = Vec::new();
            // Only the helper boundary is injected. The production command,
            // guard, private image copy and mounted-tree copier run unchanged.
            let mut run = |cmd: &mut aterm_uds::spawnfd::Command, _within| {
                assert_eq!(cmd.get_program(), "/usr/bin/hdiutil");
                let args: Vec<_> = cmd.get_args().map(std::ffi::OsStr::to_os_string).collect();
                if args[0] == "attach" {
                    let point = std::path::PathBuf::from(&args[5]);
                    let image = std::path::PathBuf::from(&args[6]);
                    assert_ne!(image, original, "the caller's path is never attached");
                    assert_eq!(image.parent(), point.parent());
                    assert_eq!(std::fs::read(&image).unwrap(), b"owned image bytes");
                    assert_eq!(point.parent(), Some(root.as_path()));
                    calls.push("attach");
                    // The helper's actual input survives a caller cleaning up
                    // its own image while attach is still in progress.
                    std::fs::remove_file(&original).unwrap();
                    assert_eq!(std::fs::read(&image).unwrap(), b"owned image bytes");
                    if scenario == "attach-panic" {
                        panic!("injected helper unwind");
                    }
                    if scenario == "attach-unknown" {
                        return Err(aterm_uds::deadline::RunError::TimedOut);
                    }
                    if matches!(
                        scenario,
                        "refused" | "backstop-unknown" | "backstop-refused"
                    ) {
                        return Ok(answer(1));
                    }
                    std::fs::create_dir_all(point.join("Demo.app/Contents")).unwrap();
                    std::fs::write(point.join("Demo.app/Contents/tool"), b"payload").unwrap();
                    return Ok(answer(0));
                }
                assert_eq!(args[0], "detach");
                let forced = args.iter().any(|arg| arg == "-force");
                calls.push(if forced { "force" } else { "detach" });
                assert!(root.join("image.dmg").is_file());
                if (forced && scenario == "backstop-unknown")
                    || (!forced && scenario == "detach-unknown")
                {
                    return Err(aterm_uds::deadline::RunError::TimedOut);
                }
                Ok(answer(i32::from(
                    (forced && scenario == "backstop-refused")
                        || (!forced && scenario == "detach-refused"),
                )))
            };
            let result = catch_unwind(AssertUnwindSafe(|| {
                extract_dmg_with(&original, &out, scratch, &mut run)
            }));
            if scenario == "attach-panic" {
                assert!(result.is_err());
                assert_eq!(calls, ["attach"], "no late detach after an unknown helper");
            } else {
                let result = result.expect("only the injected panic unwinds");
                match scenario {
                    "copied" => {
                        result.unwrap();
                        assert_eq!(calls, ["attach", "detach"]);
                    }
                    "refused" => {
                        assert!(matches!(result, Err(DmgError::Attach(_))));
                        assert_eq!(calls, ["attach", "force"]);
                        assert!(!out.exists(), "an attach refusal copies nothing");
                    }
                    "detach-refused" => {
                        assert!(matches!(result, Err(DmgError::Detach)));
                        assert_eq!(calls, ["attach", "detach", "force"]);
                    }
                    _ => {
                        assert!(matches!(result, Err(DmgError::Uncertain(_))));
                        assert_eq!(
                            calls,
                            if scenario == "attach-unknown" {
                                vec!["attach"]
                            } else if scenario == "detach-unknown" {
                                vec!["attach", "detach"]
                            } else {
                                vec!["attach", "force"]
                            },
                            "unknown cleanup is never retried in Drop"
                        );
                    }
                }
            }
            let retained = matches!(
                scenario,
                "attach-unknown"
                    | "detach-unknown"
                    | "backstop-unknown"
                    | "backstop-refused"
                    | "attach-panic"
            );
            assert_eq!(root.exists(), retained, "{scenario}");
            if retained {
                assert_eq!(
                    std::fs::read(root.join("image.dmg")).unwrap(),
                    b"owned image bytes"
                );
                assert!(root.join("mount").is_dir());
                // This boundary ran no process and mounted nothing. Only its
                // own exclusive ordinary-file fixture is removed after proof.
                std::fs::remove_dir_all(&root).unwrap();
            }
            if matches!(scenario, "copied" | "detach-refused" | "detach-unknown") {
                assert_eq!(
                    std::fs::read(out.join("Demo.app/Contents/tool")).unwrap(),
                    b"payload"
                );
            }
        }
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn dmg_mount_copy_detaches_and_stages_app() {
        let tmp = aterm_tempfile::tempdir().unwrap();
        let src = tmp.path().join("image");
        std::fs::create_dir_all(src.join("Demo.app/Contents/MacOS")).unwrap();
        std::fs::write(src.join("Demo.app/Contents/MacOS/demo"), b"bin").unwrap();
        // An HFS image `makehybrid` writes directly: `extract_dmg` attaches,
        // copies and detaches it as it does a cask's UDIF image, and it is made
        // in 0.02 s and attached in 0.12 s, where `create -srcfolder` took 4.0 s
        // and its attach 2.4 s (14.4 s for this test beside the rest of `--lib`).
        let mut make = aterm_uds::spawnfd::command("/usr/bin/hdiutil");
        make.args(["makehybrid", "-quiet", "-hfs", "-o"])
            .arg(tmp.path().join("demo.dmg"))
            .arg(&src)
            .stdin(aterm_uds::spawnfd::Stdio::null());
        let mut retained = RetainedImage(Some(tmp));
        let made = run_dmg_helper(&mut make, std::time::Duration::from_secs(60)).unwrap();
        let tmp = retained.0.take().unwrap();
        assert!(
            made.status.success(),
            "hdiutil makehybrid fixture: {made:?}"
        );
        let out = tmp.path().join("payload");
        extract_dmg(&tmp.path().join("demo.dmg"), &out).unwrap();
        assert_eq!(
            std::fs::read(out.join("Demo.app/Contents/MacOS/demo")).unwrap(),
            b"bin"
        );
        // A garbage image never copies. Delegation also needs confirmed
        // cleanup: a refusal whose cleanup is unknown remains a hard error.
        std::fs::write(tmp.path().join("junk.dmg"), b"not an image").unwrap();
        let err = extract_dmg(&tmp.path().join("junk.dmg"), &tmp.path().join("nope")).unwrap_err();
        assert!(
            matches!(err, DmgError::Attach(_) | DmgError::Uncertain(_)),
            "{err}"
        );
        assert!(!tmp.path().join("nope").exists());
    }

    #[test]
    fn archives_refuse_traversal_and_escaping_links_before_writing() {
        use crate::extract::fixtures::{ZipMember, gzip_bytes, tar_bytes, zip_bytes, zstd_bytes};
        type Member<'a> = (&'a str, u8, &'a str, &'a [u8], u32);
        fn archives(entries: &[Member<'_>]) -> Vec<(&'static str, Vec<u8>)> {
            let tar = tar_bytes(entries);
            let members: Vec<_> = entries
                .iter()
                .map(|(name, kind, target, bytes, mode)| ZipMember {
                    name,
                    mode: match kind {
                        b'2' => 0o120_777,
                        b'5' => 0o040_755,
                        _ => 0o100_000 | mode,
                    },
                    data: if *kind == b'2' {
                        target.as_bytes()
                    } else {
                        bytes
                    },
                    deflate: false,
                })
                .collect();
            vec![
                ("tar", tar.clone()),
                ("gz", gzip_bytes(&tar)),
                ("zst", zstd_bytes(&tar)),
                ("zip", zip_bytes(&members, false)),
            ]
        }
        let tmp = aterm_tempfile::tempdir().unwrap();
        let outside = tmp.path().join("outside");
        std::fs::write(&outside, b"owner data").unwrap();
        let absolute = outside.to_string_lossy();
        for (index, (name, kind, link)) in [
            ("../outside", b'0', ""),
            (absolute.as_ref(), b'0', ""),
            ("escape", b'2', "../outside"),
            ("escape", b'2', absolute.as_ref()),
        ]
        .into_iter()
        .enumerate()
        {
            let content: &[u8] = if kind == b'0' { b"replaced" } else { b"" };
            for (format, bytes) in archives(&[(name, kind, link, content, 0o755)]) {
                let archive = tmp.path().join(format!("bad-{index}.{format}"));
                std::fs::write(&archive, bytes).unwrap();
                assert!(
                    untar(
                        &archive,
                        &tmp.path().join(format!("stage-{index}-{format}"))
                    )
                    .is_err(),
                    "accepted {format}: {name} -> {link}"
                );
                assert_eq!(std::fs::read(&outside).unwrap(), b"owner data");
            }
        }
        for (format, bytes) in archives(&[("inside", b'0', "", b"safe", 0o644)]) {
            let archive = tmp.path().join(format!("good.{format}"));
            let stage = tmp.path().join(format!("good-{format}"));
            std::fs::write(&archive, bytes).unwrap();
            untar(&archive, &stage).unwrap();
            assert_eq!(std::fs::read(stage.join("inside")).unwrap(), b"safe");
            assert!(untar(&archive, &stage).is_err());
            assert_eq!(std::fs::read(stage.join("inside")).unwrap(), b"safe");
            #[cfg(unix)]
            {
                let external = tmp.path().join(format!("external-{format}"));
                std::fs::create_dir(&external).unwrap();
                let alias = tmp.path().join(format!("alias-{format}"));
                std::os::unix::fs::symlink(&external, &alias).unwrap();
                assert!(untar(&archive, &alias).is_err());
                assert!(std::fs::read_dir(&external).unwrap().next().is_none());
            }
        }
        for (format, bytes) in [
            ("xz", b"\xfd7zXZ\0".as_slice()),
            ("bz2", b"BZh91AY".as_slice()),
            ("empty", b"".as_slice()),
        ] {
            let archive = tmp.path().join(format);
            let stage = tmp.path().join(format!("unsupported-{format}"));
            std::fs::write(&archive, bytes).unwrap();
            assert!(untar(&archive, &stage).is_err());
            assert!(!stage.exists());
        }
    }
}
