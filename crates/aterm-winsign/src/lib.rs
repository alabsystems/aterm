// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! `targo --unverified winsign`.
#![allow(
    clippy::disallowed_types,
    reason = "never linked into the shipped binary: Authenticode signing behind targo --unverified winsign"
)]

use std::fmt;
use std::path::{Path, PathBuf};
use std::process::Command;

pub use aterm_update_core::pins;

/// Tier WINDOWS, read off the committed anchor.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Tier {
    /// No anchor: signing is optional and the tier itself refuses nothing.
    Inactive,
    /// The anchor names the publisher every shipped exe must be signed by.
    Active {
        /// The leaf certificate's subject CN, as `signtool verify /v` prints it.
        publisher: String,
    },
}

impl Tier {
    /// Return `true` when Tier WINDOWS is active (`Tier::Active { .. }`).
    #[must_use]
    pub const fn is_active(&self) -> bool {
        matches!(self, Self::Active { .. })
    }
}

/// The tier the committed anchor selects.
#[must_use]
pub fn tier() -> Tier {
    tier_from(pins::WINDOWS_SIGNING_PUBLISHER)
}

fn tier_from(anchor: &str) -> Tier {
    if pins::anchor_active(anchor) {
        Tier::Active {
            publisher: anchor.to_string(),
        }
    } else {
        Tier::Inactive
    }
}

impl fmt::Display for Tier {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Tier::Inactive => f.write_str("Tier WINDOWS inactive (pins::WINDOWS_SIGNING_PUBLISHER empty): signing optional; unsigned or untrusted still exits 1"),
            Tier::Active { publisher } => write!(f, "Tier WINDOWS active: exe must be signed by `{publisher}`, timestamped, trusted chain"),
        }
    }
}

/// The result of running a tool.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Ran {
    pub code: Option<i32>,
    pub stdout: String,
    pub stderr: String,
}

impl Ran {
    /// Return `true` when the command exited with code `0`.
    #[must_use]
    pub const fn succeeded(&self) -> bool {
        matches!(self.code, Some(0))
    }
}

/// Return `true` when `len` is the expected 40-hex-character SHA-1 certificate thumbprint length.
#[must_use]
pub const fn is_valid_sha1_thumbprint_len(len: usize) -> bool {
    len == 40
}

/// Encode an Authenticode verification summary (`publisher_hash`, `exit_code`, `flags`) into 16 LE bytes.
#[must_use]
pub const fn encode_winsign_summary_le(
    publisher_hash: u64,
    exit_code: i32,
    flags: u32,
) -> [u8; 16] {
    let code = exit_code as u32;
    [
        publisher_hash as u8,
        (publisher_hash >> 8) as u8,
        (publisher_hash >> 16) as u8,
        (publisher_hash >> 24) as u8,
        (publisher_hash >> 32) as u8,
        (publisher_hash >> 40) as u8,
        (publisher_hash >> 48) as u8,
        (publisher_hash >> 56) as u8,
        code as u8,
        (code >> 8) as u8,
        (code >> 16) as u8,
        (code >> 24) as u8,
        flags as u8,
        (flags >> 8) as u8,
        (flags >> 16) as u8,
        (flags >> 24) as u8,
    ]
}

/// Decode 16 LE bytes into an Authenticode verification summary (`publisher_hash`, `exit_code`, `flags`).
#[must_use]
pub const fn decode_winsign_summary_le(bytes: [u8; 16]) -> (u64, i32, u32) {
    let publisher_hash = (bytes[0] as u64)
        | ((bytes[1] as u64) << 8)
        | ((bytes[2] as u64) << 16)
        | ((bytes[3] as u64) << 24)
        | ((bytes[4] as u64) << 32)
        | ((bytes[5] as u64) << 40)
        | ((bytes[6] as u64) << 48)
        | ((bytes[7] as u64) << 56);
    let code = (bytes[8] as u32)
        | ((bytes[9] as u32) << 8)
        | ((bytes[10] as u32) << 16)
        | ((bytes[11] as u32) << 24);
    let flags = (bytes[12] as u32)
        | ((bytes[13] as u32) << 8)
        | ((bytes[14] as u32) << 16)
        | ((bytes[15] as u32) << 24);
    (publisher_hash, code as i32, flags)
}

/// Everything this tool needs from the machine, behind one trait so every decision is testable against a fake.
pub trait Host {
    fn env(&self, key: &str) -> Option<String>;
    fn is_file(&self, path: &Path) -> bool;
    /// Directory entry names, or empty when the directory cannot be read.
    fn list_dir(&self, path: &Path) -> Vec<String>;
    fn write(&self, path: &Path, text: &str) -> Result<(), String>;
    fn remove_file(&self, path: &Path);
    fn temp_dir(&self) -> PathBuf;
    fn pid(&self) -> u32;
    fn home(&self) -> Option<PathBuf>;
    /// The SDK's `bin\<version>\<arch>` subdirectory for this machine.
    fn sdk_arch(&self) -> &'static str;
    /// Run a tool with extra environment (`DOTNET_ROOT` for the Trusted Signing
    /// plug-in when the runtime is a user-local install).
    fn run(&self, program: &Path, args: &[String], env: &[(String, String)])
    -> Result<Ran, String>;
    fn is_windows(&self) -> bool;
}

/// The real machine.
pub struct RealHost;

#[cfg(all(
    any(target_os = "linux", target_os = "android"),
    any(target_arch = "aarch64", target_arch = "arm")
))]
const OPEN_FLAGS: i32 = 0o100_000 | 0o4000 | 0o2_000_000;
#[cfg(all(
    any(target_os = "linux", target_os = "android"),
    not(any(target_arch = "aarch64", target_arch = "arm"))
))]
const OPEN_FLAGS: i32 = 0o400_000 | 0o4000 | 0o2_000_000;
#[cfg(target_os = "openbsd")]
const OPEN_FLAGS: i32 = 0x0200 | 0x0004 | 0x0001_0000;
#[cfg(any(target_os = "macos", target_os = "ios"))]
const OPEN_FLAGS: i32 = 0x0100 | 0x0004 | 0x0100_0000;
#[cfg(all(
    unix,
    not(any(
        target_os = "linux",
        target_os = "android",
        target_os = "openbsd",
        target_os = "macos",
        target_os = "ios"
    ))
))]
const OPEN_FLAGS: i32 = 0x0100 | 0x0004;

impl Host for RealHost {
    fn env(&self, key: &str) -> Option<String> {
        std::env::var(key).ok().filter(|v| !v.is_empty())
    }
    fn is_file(&self, path: &Path) -> bool {
        path.is_file()
    }
    fn list_dir(&self, path: &Path) -> Vec<String> {
        std::fs::read_dir(path)
            .map(|rd| {
                rd.filter_map(Result::ok)
                    .filter_map(|e| e.file_name().into_string().ok())
                    .collect()
            })
            .unwrap_or_default()
    }
    fn write(&self, path: &Path, text: &str) -> Result<(), String> {
        use std::io::Write;
        use std::sync::atomic::{AtomicU64, Ordering};
        static NEXT_TMP: AtomicU64 = AtomicU64::new(0);

        let check_regular_or_absent = |target: &Path| -> Result<(), String> {
            match std::fs::symlink_metadata(target) {
                Ok(meta) if meta.file_type().is_file() => Ok(()),
                Ok(_) => Err(format!(
                    "{}: refusing to overwrite non-regular file",
                    target.display()
                )),
                Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(()),
                Err(e) => Err(format!("{}: {e}", target.display())),
            }
        };
        check_regular_or_absent(path)?;
        let file_name = path
            .file_name()
            .ok_or_else(|| format!("{}: missing file name", path.display()))?;
        let parent = path
            .parent()
            .filter(|p| !p.as_os_str().is_empty())
            .unwrap_or_else(|| Path::new("."));
        let (tmp, mut file) = loop {
            let seq = NEXT_TMP.fetch_add(1, Ordering::Relaxed);
            let candidate = parent.join(format!(
                ".{}.winsign-tmp-{}-{seq}",
                file_name.to_string_lossy(),
                std::process::id()
            ));
            let mut opts = std::fs::OpenOptions::new();
            opts.write(true).create_new(true);
            #[cfg(unix)]
            {
                use std::os::unix::fs::OpenOptionsExt;
                opts.mode(0o600).custom_flags(OPEN_FLAGS);
            }
            match opts.open(&candidate) {
                Ok(file) => break (candidate, file),
                Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => {}
                Err(e) => return Err(format!("{}: {e}", path.display())),
            }
        };
        let written = file
            .write_all(text.as_bytes())
            .map_err(|e| format!("{}: {e}", path.display()));
        drop(file);
        if let Err(err) = written {
            let _ = std::fs::remove_file(&tmp);
            return Err(err);
        }
        if let Err(err) = check_regular_or_absent(path) {
            let _ = std::fs::remove_file(&tmp);
            return Err(err);
        }
        if let Err(e) = std::fs::rename(&tmp, path) {
            let _ = std::fs::remove_file(&tmp);
            return Err(format!("{}: {e}", path.display()));
        }
        Ok(())
    }
    fn remove_file(&self, path: &Path) {
        let _ = std::fs::remove_file(path);
    }
    fn temp_dir(&self) -> PathBuf {
        std::env::temp_dir()
    }
    fn pid(&self) -> u32 {
        std::process::id()
    }
    fn home(&self) -> Option<PathBuf> {
        std::env::var_os("USERPROFILE")
            .filter(|v| !v.is_empty())
            .or_else(|| std::env::var_os("HOME").filter(|v| !v.is_empty()))
            .map(PathBuf::from)
    }
    fn sdk_arch(&self) -> &'static str {
        match std::env::consts::ARCH {
            "aarch64" => "arm64",
            "x86" => "x86",
            _ => "x64",
        }
    }
    fn run(
        &self,
        program: &Path,
        args: &[String],
        env: &[(String, String)],
    ) -> Result<Ran, String> {
        let out = Command::new(program)
            .args(args)
            .envs(env.iter().map(|(k, v)| (k.as_str(), v.as_str())))
            .output()
            .map_err(|e| format!("could not run {}: {e}", program.display()))?;
        Ok(Ran {
            code: out.status.code(),
            stdout: String::from_utf8_lossy(&out.stdout).into_owned(),
            stderr: String::from_utf8_lossy(&out.stderr).into_owned(),
        })
    }
    fn is_windows(&self) -> bool {
        cfg!(windows)
    }
}

/// The signing lane.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Lane {
    /// Azure Artifact Signing (formerly Trusted Signing).
    TrustedSigning {
        endpoint: String,
        account: String,
        profile: String,
    },
    /// A certificate already in the user's store, named by SHA-1 thumbprint.
    Thumbprint(String),
}

/// Tier WINDOWS's Lane A identity, committed beside its anchor
/// (`pins::WINDOWS_SIGNING_PUBLISHER`): the Azure Artifact Signing endpoint, account and
/// certificate profile, empty until the identity exists. None of the three is a secret; the
/// plug-in authenticates the Azure principal itself.
pub const TRUSTED_SIGNING: [&str; 3] = ["", "", ""];

/// Tier WINDOWS's Lane B identity: the SHA-1 thumbprint of a certificate in
/// `Cert:\CurrentUser\My`, empty until one exists. A thumbprint is public: every signature
/// carries it.
pub const THUMBPRINT: &str = "";

/// The lane the committed identity names.
///
/// # Errors
/// A partial Lane A identity, both lanes pinned, or a thumbprint that is not 40 hex digits.
pub fn pinned_lane() -> Result<Option<Lane>, String> {
    lane_from(TRUSTED_SIGNING, THUMBPRINT)
}

fn lane_from(trusted: [&str; 3], thumbprint: &str) -> Result<Option<Lane>, String> {
    let [endpoint, account, profile] = trusted;
    let trusted_any = trusted.iter().any(|v| !v.is_empty());
    if trusted_any && trusted.iter().any(|v| v.is_empty()) {
        return Err(
            "TRUSTED_SIGNING pins an endpoint, an account and a profile, all three or none".into(),
        );
    }
    if trusted_any && !thumbprint.is_empty() {
        return Err("both TRUSTED_SIGNING and THUMBPRINT are pinned; one lane signs".into());
    }
    if trusted_any {
        return Ok(Some(Lane::TrustedSigning {
            endpoint: endpoint.to_owned(),
            account: account.to_owned(),
            profile: profile.to_owned(),
        }));
    }
    if thumbprint.is_empty() {
        return Ok(None);
    }
    if thumbprint.len() != 40 || !thumbprint.chars().all(|c| c.is_ascii_hexdigit()) {
        return Err(format!(
            "THUMBPRINT is a certificate's 40 hex digits (SHA-1); got `{thumbprint}`"
        ));
    }
    Ok(Some(Lane::Thumbprint(thumbprint.to_ascii_uppercase())))
}

/// The RFC 3161 timestamp server a lane signs against.
#[must_use]
pub const fn default_timestamp(lane: &Lane) -> &'static str {
    match lane {
        Lane::TrustedSigning { .. } => "http://timestamp.acs.microsoft.com",
        Lane::Thumbprint(_) => "http://timestamp.digicert.com",
    }
}

/// Join components onto a Windows path with `\`, whatever the build host.
fn win_join<'a>(base: impl Into<PathBuf>, parts: impl IntoIterator<Item = &'a str>) -> PathBuf {
    let mut out = base.into().into_os_string();
    for part in parts {
        let tail = out.as_encoded_bytes().last().copied();
        if !matches!(tail, None | Some(b'\\') | Some(b'/') | Some(b':')) {
            out.push("\\");
        }
        out.push(part);
    }
    PathBuf::from(out)
}

/// `10.a.b.c` directory names, newest first, compared numerically.
pub fn versions_newest_first<'a>(names: impl IntoIterator<Item = &'a str>) -> Vec<&'a str> {
    let mut v: Vec<(Vec<u64>, &str)> = names
        .into_iter()
        .filter_map(|n| {
            let parts: Vec<u64> = n
                .split('.')
                .map(|p| {
                    p.bytes()
                        .all(|b| b.is_ascii_digit())
                        .then(|| p.parse().ok())
                        .flatten()
                })
                .collect::<Option<_>>()?;
            Some((parts, n))
        })
        .collect();
    v.sort_by(|a, b| b.0.cmp(&a.0));
    v.into_iter().map(|(_, n)| n).collect()
}

/// Resolve a bare program name against a `;`-separated Windows `PATH`
/// (executable extensions tried when the name has none).
fn find_on_path(host: &dyn Host, program: &str) -> Option<PathBuf> {
    let paths = host.env("PATH")?;
    for raw in paths.split(';') {
        let dir = raw.trim().trim_matches('"');
        if dir.is_empty() {
            continue;
        }
        for candidate in [
            win_join(dir, [program]),
            win_join(dir, [format!("{program}.exe").as_str()]),
            win_join(dir, [format!("{program}.cmd").as_str()]),
        ] {
            if host.is_file(&candidate) {
                return Some(candidate);
            }
        }
    }
    None
}

/// `signtool.exe`.
pub fn find_signtool(host: &dyn Host, configured: Option<&Path>) -> Result<PathBuf, String> {
    if let Some(p) = configured {
        if host.is_file(p) {
            return Ok(p.to_path_buf());
        }
        return Err(format!(
            "configured signtool `{}` is not a file",
            p.display()
        ));
    }
    if let Some(p) = find_on_path(host, "signtool") {
        return Ok(p);
    }
    let mut looked = vec!["signtool on PATH".to_string()];
    for root_var in ["ProgramFiles(x86)", "ProgramFiles"] {
        let Some(root) = host.env(root_var) else {
            continue;
        };
        let bin = win_join(root, ["Windows Kits", "10", "bin"]);
        let names = host.list_dir(&bin);
        let versions = versions_newest_first(names.iter().map(String::as_str));
        if versions.is_empty() {
            looked.push(win_join(&bin, ["<10.x.y.z>"]).display().to_string());
        }
        for ver in versions {
            let p = win_join(&bin, [ver, host.sdk_arch(), "signtool.exe"]);
            if host.is_file(&p) {
                return Ok(p);
            }
            looked.push(p.display().to_string());
        }
    }
    Err(format!(
        "signtool.exe not found (looked for {}). Install the Windows SDK's \"Windows SDK Signing Tools\" \
         feature, open a Developer prompt, or pass --signtool <path>.",
        looked.join(", ")
    ))
}

/// The NuGet package ids that carry the signtool plug-in, newest lineage first.
pub const DLIB_PACKAGES: &[&str] = &[
    "microsoft.artifactsigning.client",
    "microsoft.trusted.signing.client",
];

/// `Azure.CodeSigning.Dlib.dll`.
pub fn find_dlib(host: &dyn Host, configured: Option<&Path>) -> Result<PathBuf, String> {
    if let Some(p) = configured {
        if host.is_file(p) {
            return Ok(p.to_path_buf());
        }
        return Err(format!("configured dlib `{}` is not a file", p.display()));
    }
    let cache = host
        .env("NUGET_PACKAGES")
        .map(PathBuf::from)
        .or_else(|| host.home().map(|h| win_join(h, [".nuget", "packages"])));
    let Some(cache) = cache else {
        return Err("no NuGet cache: neither NUGET_PACKAGES nor a home directory is set".into());
    };
    let mut looked = Vec::new();
    for package in DLIB_PACKAGES {
        let pkg = win_join(&cache, [*package]);
        let names = host.list_dir(&pkg);
        let versions = versions_newest_first(names.iter().map(String::as_str));
        if versions.is_empty() {
            looked.push(win_join(&pkg, ["<version>"]).display().to_string());
        }
        for ver in versions {
            let p = win_join(
                &pkg,
                [ver, "bin", host.sdk_arch(), "Azure.CodeSigning.Dlib.dll"],
            );
            if host.is_file(&p) {
                return Ok(p);
            }
            looked.push(p.display().to_string());
        }
    }
    Err(format!(
        "Azure.CodeSigning.Dlib.dll not found (looked for {}). Install the Microsoft.ArtifactSigning.Client \
         NuGet package (apps/aterm-win/SIGNING.md, Lane A step 7) or pass --dlib <path>.",
        looked.join(", ")
    ))
}

/// The .NET runtime the Trusted Signing plug-in loads into signtool.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DotnetRuntime {
    /// The dotnet root (`…\dotnet`), holding `shared\Microsoft.NETCore.App\<version>`.
    pub root: PathBuf,
    /// The newest `Microsoft.NETCore.App` version found there.
    pub version: String,
    /// A user-local install the host loader would not find on its own.
    pub needs_root_env: bool,
}

/// The major version the dlib's `runtimeconfig.json` names (`net8.0`,
/// `rollForward: Major`, so 8 or newer).
const DOTNET_MIN_MAJOR: u64 = 8;

/// The newest `Microsoft.NETCore.App` at or above [`DOTNET_MIN_MAJOR`] under a
/// dotnet root, if any.
fn dotnet_version_under(host: &dyn Host, root: &Path) -> Option<String> {
    let shared = win_join(root, ["shared", "Microsoft.NETCore.App"]);
    let names = host.list_dir(&shared);
    versions_newest_first(names.iter().map(String::as_str))
        .into_iter()
        .find(|v| {
            v.split('.')
                .next()
                .and_then(|m| m.parse::<u64>().ok())
                .is_some_and(|m| m >= DOTNET_MIN_MAJOR)
        })
        .map(str::to_string)
}

/// Find a .NET runtime the dlib can load, in the order the host loader itself resolves one.
pub fn find_dotnet_runtime(host: &dyn Host) -> Result<DotnetRuntime, String> {
    let mut looked = Vec::new();
    let mut candidates: Vec<(PathBuf, bool)> = Vec::new();
    if let Some(r) = host.env("DOTNET_ROOT") {
        candidates.push((PathBuf::from(r), false));
    }
    if let Some(pf) = host.env("ProgramFiles") {
        candidates.push((win_join(pf, ["dotnet"]), false));
    }
    if let Some(la) = host.env("LOCALAPPDATA") {
        candidates.push((win_join(la, ["Microsoft", "dotnet"]), true));
    }
    for (root, needs_root_env) in candidates {
        if let Some(version) = dotnet_version_under(host, &root) {
            return Ok(DotnetRuntime {
                root,
                version,
                needs_root_env,
            });
        }
        let floor = format!("<{DOTNET_MIN_MAJOR}+>");
        looked.push(
            win_join(&root, ["shared", "Microsoft.NETCore.App", &floor])
                .display()
                .to_string(),
        );
    }
    Err(format!(
        ".NET {DOTNET_MIN_MAJOR}+ runtime not found (looked for {}). The Trusted Signing plug-in is a .NET \
         component; install the runtime without admin with Microsoft's dotnet-install.ps1 \
         (apps/aterm-win/SIGNING.md, Lane A step 7), or set DOTNET_ROOT.",
        looked.join(", ")
    ))
}

/// The Trusted Signing metadata file the dlib reads (`/dmdf`). It excludes the plug-in's
/// interactive browser credential, so a `sign` never waits on a person's sign-in.
#[must_use]
pub fn trusted_signing_metadata(endpoint: &str, account: &str, profile: &str) -> String {
    fn esc(s: &str) -> String {
        let mut out = String::with_capacity(s.len());
        for c in s.chars() {
            match c {
                '"' => out.push_str("\\\""),
                '\\' => out.push_str("\\\\"),
                '\n' => out.push_str("\\n"),
                '\r' => out.push_str("\\r"),
                '\t' => out.push_str("\\t"),
                c if (c as u32) < 0x20 => out.push_str(&format!("\\u{:04x}", c as u32)),
                c => out.push(c),
            }
        }
        out
    }
    format!(
        "{{\n  \"Endpoint\": \"{}\",\n  \"CodeSigningAccountName\": \"{}\",\n  \"CertificateProfileName\": \"{}\",\n  \"ExcludeCredentials\": [\"InteractiveBrowserCredential\"]\n}}\n",
        esc(endpoint),
        esc(account),
        esc(profile)
    )
}

/// `signtool sign …` for one lane over the given files.
#[must_use]
pub fn sign_args(
    lane: &Lane,
    timestamp: &str,
    dlib: Option<&Path>,
    metadata: Option<&Path>,
    files: &[PathBuf],
) -> Vec<String> {
    let mut a: Vec<String> = vec![
        "sign".into(),
        "/v".into(),
        "/fd".into(),
        "SHA256".into(),
        "/tr".into(),
        timestamp.into(),
        "/td".into(),
        "SHA256".into(),
    ];
    match lane {
        Lane::TrustedSigning { .. } => {
            a.push("/dlib".into());
            a.push(dlib.map(|p| p.display().to_string()).unwrap_or_default());
            a.push("/dmdf".into());
            a.push(
                metadata
                    .map(|p| p.display().to_string())
                    .unwrap_or_default(),
            );
        }
        Lane::Thumbprint(t) => {
            a.push("/sha1".into());
            a.push(t.clone());
        }
    }
    a.extend(files.iter().map(|f| f.display().to_string()));
    a
}

/// `signtool verify /pa /v <file>`.
#[must_use]
pub fn verify_args(file: &Path) -> Vec<String> {
    vec![
        "verify".into(),
        "/pa".into(),
        "/v".into(),
        file.display().to_string(),
    ]
}

/// What `signtool verify /pa /v` said about one file.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct VerifyReport {
    /// Exit 0 and "Successfully verified".
    pub verified: bool,
    /// `true` unless signtool printed `"No signature found"` (positive trust still requires [`Self::verified`]).
    pub signed: bool,
    /// The chain terminates in a root the trust provider does not trust.
    pub untrusted_root: bool,
    /// The leaf's `Issued to:` (the subject CN), when a chain was printed.
    pub issued_to: Option<String>,
    /// The leaf's `Issued by:`.
    pub issued_by: Option<String>,
    /// "The signature is timestamped".
    pub timestamped: bool,
}

/// Parse signtool's verbose verify output: positive markers set [`VerifyReport::verified`],
/// [`VerifyReport::untrusted_root`], and [`VerifyReport::timestamped`], while `"No signature found"`
/// clears [`VerifyReport::signed`].
#[must_use]
pub fn parse_verify_output(ran: &Ran) -> VerifyReport {
    /// One certificate of the printed chain: its depth (leading whitespace) and
    /// its `Issued to:` / `Issued by:` values.
    struct Cert {
        depth: usize,
        to: String,
        by: Option<String>,
    }
    let text = format!("{}\n{}", ran.stdout, ran.stderr);
    let mut report = VerifyReport {
        verified: ran.code == Some(0) && text.contains("Successfully verified"),
        signed: !text.contains("No signature found"),
        untrusted_root: text.contains("not trusted by the trust provider")
            || text.contains("certificate chain could not be built"),
        timestamped: text.contains("The signature is timestamped"),
        ..VerifyReport::default()
    };
    let mut chain: Vec<Cert> = Vec::new();
    let mut in_chain = false;
    for line in text.lines() {
        let l = line.trim();
        if l.starts_with("Signing Certificate Chain:") {
            in_chain = chain.is_empty();
            continue;
        }
        if !in_chain {
            continue;
        }
        let depth = line
            .find(|c: char| !c.is_whitespace())
            .unwrap_or(line.len());
        if let Some(v) = l.strip_prefix("Issued to:") {
            chain.push(Cert {
                depth,
                to: v.trim().to_string(),
                by: None,
            });
        } else if let Some(v) = l.strip_prefix("Issued by:") {
            if let Some(cert) = chain
                .last_mut()
                .filter(|c| c.depth == depth && c.by.is_none())
            {
                cert.by = Some(v.trim().to_string());
            }
        } else if l.starts_with("The signature is timestamped")
            || l.starts_with("Timestamp Verified by:")
            || l.starts_with("File has page hashes")
            || l.starts_with("Successfully verified")
        {
            in_chain = false;
        }
    }
    let leaf = chain
        .split_last()
        .filter(|(leaf, issuers)| issuers.iter().all(|c| c.depth < leaf.depth));
    if let Some((leaf, _)) = leaf {
        report.issued_to = Some(leaf.to.clone());
        report.issued_by.clone_from(&leaf.by);
    }
    report
}

/// Structured reason why Tier WINDOWS refused a [`VerifyReport`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RefusalReason {
    /// The executable carries no Authenticode signature (`!report.signed`).
    Unsigned,
    /// Verification failed and signtool reported an untrusted root (`!report.verified && report.untrusted_root`).
    UntrustedRoot,
    /// Verification failed under the default Authenticode policy (`!report.verified && !report.untrusted_root`).
    UnverifiedPolicy,
    /// The leaf certificate's `Issued to:` does not match the pinned publisher.
    PublisherMismatch { got: String, publisher: String },
    /// Signtool printed no signing certificate chain with a unique deepest leaf (`report.issued_to.is_none()`).
    MissingChain,
    /// The signature carries no RFC 3161 timestamp (`!report.timestamped`).
    NotTimestamped,
}

impl fmt::Display for RefusalReason {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Unsigned => {
                f.write_str("the exe carries no signature, and Tier WINDOWS is active")
            }
            Self::UntrustedRoot => f.write_str(
                "the signature chains to a root the Authenticode policy does not trust (a self-signed or private-CA certificate); Code Integrity will refuse it",
            ),
            Self::UnverifiedPolicy => {
                f.write_str("the signature does not verify under the default Authenticode policy")
            }
            Self::PublisherMismatch { got, publisher } => write!(
                f,
                "signed by `{got}`, but pins::WINDOWS_SIGNING_PUBLISHER is `{publisher}`"
            ),
            Self::MissingChain => f.write_str("signtool printed no signing certificate chain"),
            Self::NotTimestamped => {
                f.write_str("the signature is not timestamped; it would expire with the certificate")
            }
        }
    }
}

/// Structured tier judgement of one verified file, naming the first failing check.
pub fn judge_reason(tier: &Tier, report: &VerifyReport) -> Result<(), RefusalReason> {
    match tier {
        Tier::Inactive => Ok(()),
        Tier::Active { publisher } => {
            if !report.signed {
                return Err(RefusalReason::Unsigned);
            }
            if !report.verified {
                return Err(if report.untrusted_root {
                    RefusalReason::UntrustedRoot
                } else {
                    RefusalReason::UnverifiedPolicy
                });
            }
            match report.issued_to.as_deref() {
                Some(got) if got == publisher => {}
                Some(got) => {
                    return Err(RefusalReason::PublisherMismatch {
                        got: got.to_string(),
                        publisher: publisher.clone(),
                    });
                }
                None => return Err(RefusalReason::MissingChain),
            }
            if !report.timestamped {
                return Err(RefusalReason::NotTimestamped);
            }
            Ok(())
        }
    }
}

/// The tier's judgement of one verified file.
pub fn judge(tier: &Tier, report: &VerifyReport) -> Result<(), String> {
    judge_reason(tier, report).map_err(|r| r.to_string())
}

pub const USAGE: &str =
    "aterm-winsign — Authenticode signing of the shipped Windows exe (Tier WINDOWS)

USAGE
  targo --unverified winsign sign   [options] <exe>...   sign, then read the signature back and judge it
  targo --unverified winsign verify [options] <exe>...   read the signature back and judge it (no signing)
  targo --unverified winsign doctor [options]            what this machine can sign with, and what is missing
  targo --unverified winsign help

The identity it signs with is committed: TRUSTED_SIGNING (Lane A) or THUMBPRINT
(Lane B) in crates/aterm-winsign/src/lib.rs, beside pins::WINDOWS_SIGNING_PUBLISHER.

OPTIONS
  --signtool <path>      signtool.exe (default: PATH, then the newest Windows SDK)
  --dlib <path>          Azure.CodeSigning.Dlib.dll (default: newest under the NuGet cache)

EXIT
  0 verdict accepted, `doctor` READY, or `help`
  1 bad signing or verdict: unsigned, untrusted chain, refused by the active
    tier, or signtool ran and exited non-zero (its output printed)
  2 nothing signed and no verdict read: bad arguments, no pinned identity,
    or a missing tool

Identity: Azure Artifact Signing or a publicly trusted code-signing
certificate; apps/aterm-win/SIGNING.md is the runbook.";

/// Where the tools come from when discovery is not enough.
#[derive(Debug, Default)]
struct Flags {
    verb: Option<String>,
    signtool: Option<PathBuf>,
    dlib: Option<PathBuf>,
    files: Vec<PathBuf>,
}

fn parse_flags(args: &[String]) -> Result<Flags, String> {
    let mut f = Flags::default();
    let mut it = args.iter();
    while let Some(a) = it.next() {
        if f.verb.is_none() && !a.starts_with("--") {
            f.verb = Some(a.clone());
            continue;
        }
        match a.as_str() {
            "--signtool" => {
                f.signtool = Some(PathBuf::from(it.next().ok_or("--signtool needs a path")?));
            }
            "--dlib" => f.dlib = Some(PathBuf::from(it.next().ok_or("--dlib needs a path")?)),
            "--help" => f.verb = Some("help".into()),
            _ if a.starts_with("--") => return Err(format!("unknown option {a}")),
            _ => f.files.push(PathBuf::from(a)),
        }
    }
    Ok(f)
}

/// Run the tool against the committed [`tier()`] and [`pinned_lane()`]. An invalid pin fails
/// only the verbs that sign with it (`sign`, `doctor`); `help` and `verify` need no lane.
pub fn run(args: Vec<String>, host: &dyn Host) -> i32 {
    let pinned = pinned_lane();
    exit_code(run_inner(
        &args,
        host,
        &tier(),
        pinned.as_ref().map(Option::as_ref).map_err(String::as_str),
    ))
}

/// Run the tool against an explicit [`Tier`] and lane.
pub fn run_with(args: Vec<String>, host: &dyn Host, tier: &Tier, lane: Option<&Lane>) -> i32 {
    exit_code(run_inner(&args, host, tier, Ok(lane)))
}

fn exit_code(r: Result<i32, String>) -> i32 {
    r.unwrap_or_else(|e| {
        eprintln!("aterm-winsign: {e}");
        2
    })
}

fn run_inner(
    args: &[String],
    host: &dyn Host,
    tier: &Tier,
    lane: Result<Option<&Lane>, &str>,
) -> Result<i32, String> {
    let flags = parse_flags(args)?;
    let verb = flags.verb.clone().unwrap_or_else(|| "help".into());
    match verb.as_str() {
        "help" => {
            println!("{USAGE}");
            Ok(0)
        }
        "doctor" => Ok(doctor(host, tier, lane, &flags)),
        "verify" => {
            if flags.files.is_empty() {
                return Err("verify needs at least one exe".into());
            }
            let signtool = find_signtool(host, flags.signtool.as_deref())?;
            let mut code = 0;
            for f in &flags.files {
                if !report_file(host, &signtool, tier, f)? {
                    code = 1;
                }
            }
            Ok(code)
        }
        "sign" => run_sign(host, tier, lane?, &flags),
        other => Err(format!("unknown verb `{other}`\n{USAGE}")),
    }
}

/// Why `sign` has nothing to sign with: a fact about the committed identity, not a request.
const NO_IDENTITY: &str = "no Windows signing identity is pinned (TRUSTED_SIGNING and THUMBPRINT in crates/aterm-winsign/src/lib.rs are empty)";

/// Why Lane A has no Azure principal: a fact about this host, not a request.
const NO_PRINCIPAL: &str = "no Azure signing principal on this host (no AZURE_TENANT_ID, AZURE_CLIENT_ID and AZURE_CLIENT_SECRET, no az CLI)";

/// The non-interactive source the plug-in authenticates the Azure principal from, if any.
fn azure_principal(host: &dyn Host) -> Option<&'static str> {
    if ["AZURE_TENANT_ID", "AZURE_CLIENT_ID", "AZURE_CLIENT_SECRET"]
        .iter()
        .all(|k| host.env(k).is_some())
    {
        Some("service-principal environment")
    } else if find_on_path(host, "az").is_some() {
        Some("az CLI session")
    } else {
        None
    }
}

fn run_sign(
    host: &dyn Host,
    tier: &Tier,
    lane: Option<&Lane>,
    flags: &Flags,
) -> Result<i32, String> {
    let files = &flags.files;
    if files.is_empty() {
        return Err("sign needs at least one exe".into());
    }
    for f in files {
        if !host.is_file(f) {
            return Err(format!("not a file: {}", f.display()));
        }
    }
    let Some(lane) = lane else {
        return match tier {
            Tier::Active { .. } => Err(format!("{tier}; {NO_IDENTITY}")),
            Tier::Inactive => Err(NO_IDENTITY.into()),
        };
    };
    let signtool = find_signtool(host, flags.signtool.as_deref())?;
    eprintln!("{tier}");
    eprintln!("signtool: {}", signtool.display());
    let timestamp = default_timestamp(lane);
    let mut child_env: Vec<(String, String)> = Vec::new();
    let (dlib, metadata) = match lane {
        Lane::TrustedSigning {
            endpoint,
            account,
            profile,
        } => {
            let principal = azure_principal(host).ok_or(NO_PRINCIPAL)?;
            let dlib = find_dlib(host, flags.dlib.as_deref())?;
            let runtime = find_dotnet_runtime(host)?;
            if runtime.needs_root_env {
                child_env.push((
                    "DOTNET_ROOT".to_string(),
                    runtime.root.display().to_string(),
                ));
            }
            let meta = win_join(
                host.temp_dir(),
                [format!("aterm-winsign-{}.json", host.pid()).as_str()],
            );
            host.write(&meta, &trusted_signing_metadata(endpoint, account, profile))?;
            eprintln!("lane: Azure Artifact Signing — {account} / {profile} at {endpoint}");
            eprintln!("dlib: {}", dlib.display());
            eprintln!("azure identity: {principal}");
            eprintln!(
                "dotnet: {} at {}{}",
                runtime.version,
                runtime.root.display(),
                if runtime.needs_root_env {
                    " (passed as DOTNET_ROOT)"
                } else {
                    ""
                }
            );
            (Some(dlib), Some(meta))
        }
        Lane::Thumbprint(t) => {
            eprintln!("lane: certificate {t} from the user's store");
            (None, None)
        }
    };
    let args = sign_args(lane, timestamp, dlib.as_deref(), metadata.as_deref(), files);
    let ran = host.run(&signtool, &args, &child_env);
    if let Some(m) = &metadata {
        host.remove_file(m);
    }
    let ran = ran?;
    if ran.code != Some(0) {
        eprint!("{}{}", ran.stdout, ran.stderr);
        eprintln!("aterm-winsign: signtool sign failed (exit {:?})", ran.code);
        return Ok(1);
    }
    let mut code = 0;
    for f in files {
        if !report_file(host, &signtool, tier, f)? {
            code = 1;
        }
    }
    Ok(code)
}

/// Verify one file, print the verdict, and say whether the tier accepts it.
fn report_file(host: &dyn Host, signtool: &Path, tier: &Tier, file: &Path) -> Result<bool, String> {
    let ran = host.run(signtool, &verify_args(file), &[])?;
    let report = parse_verify_output(&ran);
    let name = file.display();
    match judge(tier, &report) {
        Ok(()) => {
            if report.verified {
                println!(
                    "{name}: signed by `{}` (issued by `{}`), chain trusted, {}",
                    report.issued_to.as_deref().unwrap_or("?"),
                    report.issued_by.as_deref().unwrap_or("?"),
                    if report.timestamped {
                        "timestamped"
                    } else {
                        "NOT timestamped"
                    }
                );
                Ok(true)
            } else if !report.signed {
                println!("{name}: UNSIGNED; Smart App Control may refuse it at any time");
                Ok(false)
            } else {
                println!(
                    "{name}: signed by `{}`, chain NOT trusted ({}); Code Integrity refuses it like an unsigned exe (apps/aterm-win/SIGNING.md)",
                    report.issued_to.as_deref().unwrap_or("?"),
                    if report.untrusted_root {
                        "untrusted root"
                    } else {
                        "policy failure"
                    }
                );
                eprint!("{}{}", ran.stdout, ran.stderr);
                Ok(false)
            }
        }
        Err(why) => {
            println!("{name}: REFUSED — {why}");
            eprint!("{}{}", ran.stdout, ran.stderr);
            Ok(false)
        }
    }
}

/// What this machine can sign with. Exit 0 when a `sign` would run.
fn doctor(host: &dyn Host, tier: &Tier, lane: Result<Option<&Lane>, &str>, flags: &Flags) -> i32 {
    let mut ok = true;
    println!("{tier}");
    if !host.is_windows() {
        println!("host: not Windows — signtool is a Windows SDK tool; sign on a Windows machine");
        ok = false;
    }
    match find_signtool(host, flags.signtool.as_deref()) {
        Ok(p) => println!("signtool: {}", p.display()),
        Err(e) => {
            println!("signtool: MISSING — {e}");
            ok = false;
        }
    }
    match lane {
        Err(e) => {
            println!("lane: INVALID — {e}");
            ok = false;
        }
        Ok(None) => {
            println!("lane: none — {NO_IDENTITY}");
            ok = false;
        }
        Ok(Some(Lane::Thumbprint(t))) => println!(
            "lane: certificate thumbprint {t} (must be in the user's certificate store with its private key)"
        ),
        Ok(Some(Lane::TrustedSigning {
            endpoint,
            account,
            profile,
        })) => {
            println!(
                "lane: Azure Artifact Signing — account {account}, profile {profile}, endpoint {endpoint}"
            );
            match find_dlib(host, flags.dlib.as_deref()) {
                Ok(p) => println!("dlib: {}", p.display()),
                Err(e) => {
                    println!("dlib: MISSING — {e}");
                    ok = false;
                }
            }
            match find_dotnet_runtime(host) {
                Ok(r) => println!(
                    "dotnet: Microsoft.NETCore.App {} at {}{}",
                    r.version,
                    r.root.display(),
                    if r.needs_root_env {
                        " (user-local; passed to signtool as DOTNET_ROOT)"
                    } else {
                        ""
                    }
                ),
                Err(e) => {
                    println!("dotnet: MISSING — {e}");
                    ok = false;
                }
            }
            match azure_principal(host) {
                Some(p) => println!("azure identity: {p}"),
                None => {
                    println!("azure identity: MISSING — {NO_PRINCIPAL}");
                    ok = false;
                }
            }
        }
    }
    if let Ok(Some(lane)) = lane {
        println!("timestamp: {}", default_timestamp(lane));
    }
    println!(
        "{}",
        if ok {
            "READY: `targo --unverified winsign sign <exe>` will run."
        } else {
            "NOT READY — see the lines above; apps/aterm-win/SIGNING.md is the runbook."
        }
    );
    if ok { 0 } else { 2 }
}

#[cfg(test)]
#[path = "lib_tests.rs"]
mod tests;
