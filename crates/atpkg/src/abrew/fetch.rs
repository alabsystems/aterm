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
    pub(super) fn other(message: String) -> Self {
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
