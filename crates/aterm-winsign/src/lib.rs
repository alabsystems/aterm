// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! `targo --unverified winsign`.
#![allow(
    clippy::disallowed_types,
    reason = "never linked into the shipped binary: Authenticode signing behind targo --unverified winsign"
)]

use std::collections::BTreeMap;
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
    fn read_to_string(&self, path: &Path) -> Result<String, String>;
    fn write(&self, path: &Path, text: &str) -> Result<(), String>;
    fn temp_dir(&self) -> PathBuf;
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
    fn read_to_string(&self, path: &Path) -> Result<String, String> {
        std::fs::read_to_string(path).map_err(|e| format!("{}: {e}", path.display()))
    }
    fn write(&self, path: &Path, text: &str) -> Result<(), String> {
        std::fs::write(path, text).map_err(|e| format!("{}: {e}", path.display()))
    }
    fn temp_dir(&self) -> PathBuf {
        std::env::temp_dir()
    }
    fn home(&self) -> Option<PathBuf> {
        std::env::var_os("USERPROFILE")
            .or_else(|| std::env::var_os("HOME"))
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
    /// Azure Trusted Signing.
    TrustedSigning {
        endpoint: String,
        account: String,
        profile: String,
        /// `Azure.CodeSigning.Dlib.dll`; `None` = find it under the NuGet cache.
        dlib: Option<PathBuf>,
    },
    /// A certificate already in the user's store, named by SHA-1 thumbprint.
    Thumbprint(String),
}

/// The keys every source spells the same way: `--<key>`, `ATERM_WINSIGN_<KEY>`,
/// `winsign_<key> = "…"`.
const KEYS: &[&str] = &[
    "lane",
    "endpoint",
    "account",
    "profile",
    "dlib",
    "thumbprint",
    "timestamp",
    "signtool",
];

/// Values gathered from one source, keyed by the bare key name.
pub type Values = BTreeMap<String, String>;

/// Environment values.
pub fn values_from_env(host: &dyn Host) -> Values {
    KEYS.iter()
        .filter_map(|k| {
            host.env(&format!("ATERM_WINSIGN_{}", k.to_ascii_uppercase()))
                .map(|v| (k.to_string(), v))
        })
        .collect()
}

/// Credentials-profile values.
pub fn values_from_profile(text: &str) -> Result<Values, String> {
    let mut out = Values::new();
    for line in text.lines() {
        let line = line.trim();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        let Some((key, value)) = line.split_once('=') else {
            return Err(format!("not a `key = value` line: {line}"));
        };
        let key = key.trim();
        let Some(bare) = key.strip_prefix("winsign_") else {
            continue;
        };
        if !KEYS.contains(&bare) {
            return Err(format!(
                "unknown credentials key `{key}` (known: {})",
                KEYS.join(", ")
            ));
        }
        let value = value.trim();
        let value = value
            .strip_prefix('"')
            .and_then(|v| v.strip_suffix('"'))
            .ok_or_else(|| format!("{key} must be a quoted string"))?;
        if value.is_empty() {
            return Err(format!("{key} is empty"));
        }
        out.insert(bare.to_string(), value.to_string());
    }
    Ok(out)
}

/// The resolved configuration.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Config {
    pub lane: Option<Lane>,
    /// RFC 3161 timestamp server.
    pub timestamp: Option<String>,
    /// An explicit `signtool.exe`.
    pub signtool: Option<PathBuf>,
}

/// Merge sources in precedence order (first wins) and validate the shape.
pub fn resolve_config(sources: &[&Values]) -> Result<Config, String> {
    let get = |key: &str| -> Option<String> { sources.iter().find_map(|s| s.get(key).cloned()) };
    let endpoint = get("endpoint");
    let account = get("account");
    let profile = get("profile");
    let thumbprint = get("thumbprint");
    let dlib = get("dlib").map(PathBuf::from);
    let trusted_any = endpoint.is_some() || account.is_some() || profile.is_some();

    let lane_name = get("lane");
    let lane = match lane_name.as_deref() {
        Some("trusted-signing") | None if trusted_any => {
            if thumbprint.is_some() && lane_name.is_none() {
                return Err("both a Trusted Signing account and a thumbprint are configured; say which with `--lane trusted-signing` or `--lane thumbprint`".into());
            }
            let (Some(endpoint), Some(account), Some(profile)) = (endpoint, account, profile)
            else {
                return Err("the Trusted Signing lane needs all three of endpoint, account and profile (`--endpoint https://<region>.codesigning.azure.net --account <name> --profile <name>`, or ATERM_WINSIGN_ENDPOINT / _ACCOUNT / _PROFILE, or winsign_endpoint / _account / _profile in the credentials profile)".into());
            };
            Some(Lane::TrustedSigning {
                endpoint,
                account,
                profile,
                dlib,
            })
        }
        Some("trusted-signing") => {
            return Err(
                "`--lane trusted-signing` was named but no endpoint/account/profile is configured"
                    .into(),
            );
        }
        Some("thumbprint") | None if thumbprint.is_some() => {
            let t = thumbprint.unwrap_or_default();
            let clean: String = t.chars().filter(|c| !c.is_whitespace()).collect();
            if clean.len() != 40 || !clean.chars().all(|c| c.is_ascii_hexdigit()) {
                return Err(format!(
                    "a certificate thumbprint is 40 hex digits (SHA-1); got `{t}`"
                ));
            }
            Some(Lane::Thumbprint(clean.to_ascii_uppercase()))
        }
        Some("thumbprint") => {
            return Err("`--lane thumbprint` was named but no thumbprint is configured".into());
        }
        Some(other) => {
            return Err(format!(
                "unknown lane `{other}`: use `trusted-signing` or `thumbprint`"
            ));
        }
        None => None,
    };
    Ok(Config {
        lane,
        timestamp: get("timestamp"),
        signtool: get("signtool").map(PathBuf::from),
    })
}

/// The timestamp server a lane uses when none is configured.
#[must_use]
pub fn default_timestamp(lane: &Lane) -> &'static str {
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
                .map(str::parse)
                .collect::<Result<_, _>>()
                .ok()?;
            (!parts.is_empty()).then_some((parts, n))
        })
        .collect();
    v.sort_by(|a, b| b.0.cmp(&a.0));
    v.into_iter().map(|(_, n)| n).collect()
}

/// Resolve a bare program name against `PATH` (Windows executable extensions
/// tried when the name has none).
fn find_on_path(host: &dyn Host, program: &str) -> Option<PathBuf> {
    let paths = host.env("PATH")?;
    for dir in std::env::split_paths(&paths) {
        for candidate in [
            dir.join(program),
            dir.join(format!("{program}.exe")),
            dir.join(format!("{program}.cmd")),
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
         feature, open a Developer prompt, or set ATERM_WINSIGN_SIGNTOOL.",
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
         NuGet package (apps/aterm-win/SIGNING.md, step 4) or set ATERM_WINSIGN_DLIB to the dll.",
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
         (apps/aterm-win/SIGNING.md, step 4), or set DOTNET_ROOT.",
        looked.join(", ")
    ))
}

/// The Trusted Signing metadata file the dlib reads (`/dmdf`).
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
        "{{\n  \"Endpoint\": \"{}\",\n  \"CodeSigningAccountName\": \"{}\",\n  \"CertificateProfileName\": \"{}\"\n}}\n",
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
    /// A signature is present at all.
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

/// Parse signtool's verbose verify output.
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

/// The tier's judgement of one verified file.
pub fn judge(tier: &Tier, report: &VerifyReport) -> Result<(), String> {
    match tier {
        Tier::Inactive => Ok(()),
        Tier::Active { publisher } => {
            if !report.signed {
                return Err("the exe carries no signature, and Tier WINDOWS is active".into());
            }
            if !report.verified {
                return Err(if report.untrusted_root {
                    "the signature chains to a root the Authenticode policy does not trust (a self-signed or private-CA certificate); Code Integrity will refuse it".into()
                } else {
                    "the signature does not verify under the default Authenticode policy".into()
                });
            }
            match report.issued_to.as_deref() {
                Some(got) if got == publisher => {}
                Some(got) => {
                    return Err(format!(
                        "signed by `{got}`, but pins::WINDOWS_SIGNING_PUBLISHER is `{publisher}`"
                    ));
                }
                None => return Err("signtool printed no signing certificate chain".into()),
            }
            if !report.timestamped {
                return Err(
                    "the signature is not timestamped; it would expire with the certificate".into(),
                );
            }
            Ok(())
        }
    }
}

pub const USAGE: &str =
    "aterm-winsign — Authenticode signing of the shipped Windows exe (Tier WINDOWS)

USAGE
  targo --unverified winsign sign   [options] <exe>...   sign, then read the signature back and judge it
  targo --unverified winsign verify [options] <exe>...   read the signature back and judge it (no signing)
  targo --unverified winsign doctor [options]            what this machine can sign with, and what is missing
  targo --unverified winsign help

OPTIONS (flag > ATERM_WINSIGN_<KEY> > credentials profile `winsign_<key> = \"…\"`)
  --credentials <file>   a release-credentials profile to read winsign_* keys from
  --lane <l>             trusted-signing | thumbprint (inferred when only one is configured)
  --endpoint <url>       Trusted Signing endpoint, e.g. https://eus.codesigning.azure.net
  --account <name>       Trusted Signing account name
  --profile <name>       Trusted Signing certificate profile name
  --dlib <path>          Azure.CodeSigning.Dlib.dll (default: newest under the NuGet cache)
  --thumbprint <sha1>    a certificate in the user's store (Cert:\\CurrentUser\\My), by thumbprint
  --timestamp <url>      RFC 3161 server (default: the lane's own)
  --signtool <path>      signtool.exe (default: PATH, then the newest Windows SDK)

EXIT
  0 verdict accepted, `doctor` READY, or `help`
  1 bad signing or verdict: unsigned, untrusted chain, refused by the active
    tier, or signtool ran and exited non-zero (its output printed)
  2 nothing signed and no verdict read: bad arguments or credentials profile,
    an unresolved lane, nothing to sign with, or a missing tool

Identity: Azure Artifact Signing or a publicly trusted code-signing
certificate; apps/aterm-win/SIGNING.md is the runbook.";

#[derive(Debug, Default)]
struct Flags {
    verb: Option<String>,
    credentials: Option<PathBuf>,
    values: Values,
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
        if let Some(key) = a.strip_prefix("--") {
            if key == "credentials" {
                let v = it.next().ok_or("--credentials needs a file")?;
                f.credentials = Some(PathBuf::from(v));
            } else if KEYS.contains(&key) {
                let v = it.next().ok_or_else(|| format!("--{key} needs a value"))?;
                f.values.insert(key.to_string(), v.clone());
            } else if key == "help" {
                f.verb = Some("help".into());
            } else {
                return Err(format!("unknown option --{key}"));
            }
        } else {
            f.files.push(PathBuf::from(a));
        }
    }
    Ok(f)
}

/// Run the tool.
pub fn run(args: Vec<String>, host: &dyn Host) -> i32 {
    match run_inner(&args, host) {
        Ok(code) => code,
        Err(e) => {
            eprintln!("aterm-winsign: {e}");
            2
        }
    }
}

fn run_inner(args: &[String], host: &dyn Host) -> Result<i32, String> {
    let flags = parse_flags(args)?;
    let verb = flags.verb.clone().unwrap_or_else(|| "help".into());
    if verb == "help" {
        println!("{USAGE}");
        return Ok(0);
    }
    let tier = tier();
    let env = values_from_env(host);
    let profile = match &flags.credentials {
        Some(p) => values_from_profile(&host.read_to_string(p)?)
            .map_err(|e| format!("{}: {e}", p.display()))?,
        None => Values::new(),
    };
    let config = resolve_config(&[&flags.values, &env, &profile])?;

    match verb.as_str() {
        "doctor" => Ok(doctor(host, &tier, &config)),
        "verify" => {
            if flags.files.is_empty() {
                return Err("verify needs at least one exe".into());
            }
            let signtool = find_signtool(host, config.signtool.as_deref())?;
            let mut code = 0;
            for f in &flags.files {
                if !report_file(host, &signtool, &tier, f)? {
                    code = 1;
                }
            }
            Ok(code)
        }
        "sign" => {
            if flags.files.is_empty() {
                return Err("sign needs at least one exe".into());
            }
            for f in &flags.files {
                if !host.is_file(f) {
                    return Err(format!("not a file: {}", f.display()));
                }
            }
            let Some(lane) = &config.lane else {
                let why = "no signing lane is configured: name a Trusted Signing account (endpoint/account/profile) or a certificate thumbprint — apps/aterm-win/SIGNING.md";
                return match tier {
                    Tier::Active { .. } => Err(format!("{tier}; {why}")),
                    Tier::Inactive => Err(why.into()),
                };
            };
            let signtool = find_signtool(host, config.signtool.as_deref())?;
            eprintln!("{tier}");
            eprintln!("signtool: {}", signtool.display());
            let timestamp = config
                .timestamp
                .clone()
                .unwrap_or_else(|| default_timestamp(lane).to_string());
            let mut child_env: Vec<(String, String)> = Vec::new();
            let (dlib, metadata) = match lane {
                Lane::TrustedSigning {
                    endpoint,
                    account,
                    profile,
                    dlib,
                } => {
                    let dlib = find_dlib(host, dlib.as_deref())?;
                    let runtime = find_dotnet_runtime(host)?;
                    if runtime.needs_root_env {
                        child_env.push((
                            "DOTNET_ROOT".to_string(),
                            runtime.root.display().to_string(),
                        ));
                    }
                    let meta = host
                        .temp_dir()
                        .join(format!("aterm-winsign-{}.json", std::process::id()));
                    host.write(&meta, &trusted_signing_metadata(endpoint, account, profile))?;
                    eprintln!("lane: Azure Artifact Signing — {account} / {profile} at {endpoint}");
                    eprintln!("dlib: {}", dlib.display());
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
            let args = sign_args(
                lane,
                &timestamp,
                dlib.as_deref(),
                metadata.as_deref(),
                &flags.files,
            );
            let ran = host.run(&signtool, &args, &child_env);
            if let Some(m) = &metadata {
                let _ = std::fs::remove_file(m);
            }
            let ran = ran?;
            if ran.code != Some(0) {
                eprint!("{}{}", ran.stdout, ran.stderr);
                eprintln!("aterm-winsign: signtool sign failed (exit {:?})", ran.code);
                return Ok(1);
            }
            let mut code = 0;
            for f in &flags.files {
                if !report_file(host, &signtool, &tier, f)? {
                    code = 1;
                }
            }
            Ok(code)
        }
        other => Err(format!("unknown verb `{other}`\n{USAGE}")),
    }
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
fn doctor(host: &dyn Host, tier: &Tier, config: &Config) -> i32 {
    let mut ok = true;
    println!("{tier}");
    if !host.is_windows() {
        println!("host: not Windows — signtool is a Windows SDK tool; sign on a Windows machine");
        ok = false;
    }
    match find_signtool(host, config.signtool.as_deref()) {
        Ok(p) => println!("signtool: {}", p.display()),
        Err(e) => {
            println!("signtool: MISSING — {e}");
            ok = false;
        }
    }
    match &config.lane {
        None => {
            println!(
                "lane: none configured — set ATERM_WINSIGN_THUMBPRINT, or ATERM_WINSIGN_ENDPOINT / _ACCOUNT / _PROFILE (or the winsign_* keys of a credentials profile)"
            );
            ok = false;
        }
        Some(Lane::Thumbprint(t)) => println!(
            "lane: certificate thumbprint {t} (must be in the user's certificate store with its private key)"
        ),
        Some(Lane::TrustedSigning {
            endpoint,
            account,
            profile,
            dlib,
        }) => {
            println!(
                "lane: Azure Artifact Signing — account {account}, profile {profile}, endpoint {endpoint}"
            );
            match find_dlib(host, dlib.as_deref()) {
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
            let sp = ["AZURE_TENANT_ID", "AZURE_CLIENT_ID", "AZURE_CLIENT_SECRET"]
                .iter()
                .all(|k| host.env(k).is_some());
            let az = find_on_path(host, "az").is_some();
            println!(
                "azure identity: {}",
                if sp {
                    "service-principal environment set (unattended)"
                } else if az {
                    "az CLI on PATH — its `az login` session is used when valid; otherwise the browser sign-in"
                } else {
                    "interactive browser sign-in (the plug-in opens it on `sign`; sign in as a principal holding Artifact Signing Certificate Profile Signer)"
                }
            );
        }
    }
    println!(
        "timestamp: {}",
        config
            .timestamp
            .as_deref()
            .unwrap_or("(the lane's default)")
    );
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
mod tests {
    use super::*;
    use std::cell::RefCell;

    /// One recorded tool launch: program, argv, extra env.
    type Run = (PathBuf, Vec<String>, Vec<(String, String)>);

    /// One path, spelled the way the Windows machine this fake models would
    /// spell it: `/` and `\` are both separators, runs of them collapse, and a
    /// trailing one is nothing.
    fn win_path(path: &Path) -> String {
        let s = path.to_string_lossy().replace('/', "\\");
        let body = s.trim_start_matches('\\');
        let lead = "\\".repeat((s.len() - body.len()).min(2));
        let segments: Vec<&str> = body.split('\\').filter(|s| !s.is_empty()).collect();
        format!("{lead}{}", segments.join("\\"))
    }

    /// `win_path` on both sides of an `assert_eq!`, so a mismatch prints the two Windows spellings rather than two host-mangled ones.
    macro_rules! assert_same_path {
        ($got:expr, $want:expr $(,)?) => {
            assert_eq!(win_path($got.as_ref()), win_path($want.as_ref()))
        };
    }

    struct Fake {
        env: BTreeMap<String, String>,
        files: Vec<PathBuf>,
        dirs: BTreeMap<PathBuf, Vec<String>>,
        runs: RefCell<Vec<Run>>,
        reply: Ran,
    }

    impl Fake {
        fn new() -> Self {
            Fake {
                env: BTreeMap::new(),
                files: Vec::new(),
                dirs: BTreeMap::new(),
                runs: RefCell::new(Vec::new()),
                reply: Ran::default(),
            }
        }
    }

    impl Host for Fake {
        fn env(&self, key: &str) -> Option<String> {
            self.env.get(key).cloned()
        }
        fn is_file(&self, path: &Path) -> bool {
            let want = win_path(path);
            self.files.iter().any(|f| win_path(f) == want)
        }
        fn list_dir(&self, path: &Path) -> Vec<String> {
            let want = win_path(path);
            self.dirs
                .iter()
                .find(|(dir, _)| win_path(dir) == want)
                .map(|(_, names)| names.clone())
                .unwrap_or_default()
        }
        fn read_to_string(&self, path: &Path) -> Result<String, String> {
            Err(format!("no such file {}", path.display()))
        }
        fn write(&self, _: &Path, _: &str) -> Result<(), String> {
            Ok(())
        }
        fn temp_dir(&self) -> PathBuf {
            PathBuf::from(r"C:\tmp")
        }
        fn home(&self) -> Option<PathBuf> {
            Some(PathBuf::from(r"C:\Users\u"))
        }
        fn sdk_arch(&self) -> &'static str {
            "x64"
        }
        fn run(
            &self,
            program: &Path,
            args: &[String],
            env: &[(String, String)],
        ) -> Result<Ran, String> {
            self.runs
                .borrow_mut()
                .push((program.to_path_buf(), args.to_vec(), env.to_vec()));
            Ok(self.reply.clone())
        }
        fn is_windows(&self) -> bool {
            true
        }
    }

    /// `signtool verify /pa /v` over an Artifact Signing signature: each chain
    /// from the root down, every certificate one level deeper than its issuer,
    /// the signer's leaf last.
    const VERIFIED: &str = "Verifying: aterm.exe

Signature Index: 0 (Primary Signature)
Hash of file (sha256): AB

Signing Certificate Chain:
    Issued to: Microsoft Identity Verification Root Certificate Authority 2020
    Issued by: Microsoft Identity Verification Root Certificate Authority 2020
    Expires:   Mon Apr 16 18:44:40 2045
    SHA1 hash: 11

        Issued to: Microsoft ID Verified Code Signing PCA 2021
        Issued by: Microsoft Identity Verification Root Certificate Authority 2020
        Expires:   Mon Apr 01 20:05:20 2036
        SHA1 hash: 22

            Issued to: Microsoft ID Verified CS EOC CA 01
            Issued by: Microsoft ID Verified Code Signing PCA 2021
            Expires:   Mon Apr 13 17:31:53 2026
            SHA1 hash: 33

                Issued to: Andrew Yates
                Issued by: Microsoft ID Verified CS EOC CA 01
                Expires:   Wed Sep 16 00:00:00 2026
                SHA1 hash: 44

The signature is timestamped: Mon Sep 15 10:00:00 2026
Timestamp Verified by:
    Issued to: Microsoft Identity Verification Root Certificate Authority 2020
    Issued by: Microsoft Identity Verification Root Certificate Authority 2020
    Expires:   Mon Apr 16 18:44:40 2045
    SHA1 hash: 11

        Issued to: Microsoft Public RSA Timestamping CA 2020
        Issued by: Microsoft Identity Verification Root Certificate Authority 2020
        Expires:   Mon Nov 19 20:42:31 2035
        SHA1 hash: 55

            Issued to: Microsoft Public RSA Time Stamping Authority
            Issued by: Microsoft Public RSA Timestamping CA 2020
            Expires:   Thu Feb 15 20:36:12 2027
            SHA1 hash: 66

Successfully verified: aterm.exe

Number of files successfully Verified: 1
Number of warnings: 0
Number of errors: 0
";

    const UNTRUSTED: &str = "Verifying: aterm.exe
Signature Index: 0 (Primary Signature)
Signing Certificate Chain:
    Issued to: aterm dev
    Issued by: aterm dev
    Expires:   Sat Sep 15 00:00:00 2029
    SHA1 hash: 33

The signature is timestamped: Mon Sep 15 10:00:00 2026

SignTool Error: A certificate chain processed, but terminated in a root certificate which is not trusted by the trust provider.

Number of errors: 1
";

    fn ran(code: i32, text: &str) -> Ran {
        Ran {
            code: Some(code),
            stdout: text.to_string(),
            stderr: String::new(),
        }
    }

    #[test]
    fn the_tier_is_the_anchor_and_nothing_else() {
        assert_eq!(tier_from(""), Tier::Inactive);
        assert_eq!(
            tier_from("Andrew Yates"),
            Tier::Active {
                publisher: "Andrew Yates".into()
            }
        );
    }

    /// The signer is the leaf certificate (printed last in the chain), with LF
    /// or CRLF line endings.
    #[test]
    fn a_verified_chain_is_read_back_as_its_leaf_not_its_root() {
        for text in [VERIFIED.to_string(), VERIFIED.replace('\n', "\r\n")] {
            let r = parse_verify_output(&ran(0, &text));
            assert!(r.verified && r.signed && r.timestamped && !r.untrusted_root);
            assert_eq!(r.issued_to.as_deref(), Some("Andrew Yates"));
            assert_eq!(
                r.issued_by.as_deref(),
                Some("Microsoft ID Verified CS EOC CA 01")
            );
        }
    }

    /// Another customer's leaf under the same root is refused by the pin, as is
    /// a pin on the root's name or a flat block with two certificates at the
    /// same depth.
    #[test]
    fn the_pin_names_one_leaf_and_never_the_root_it_chains_to() {
        let root = "Microsoft Identity Verification Root Certificate Authority 2020";
        let other = VERIFIED.replace(
            "Issued to: Andrew Yates",
            "Issued to: Mallory Unrelated Customer LLC",
        );
        let e = judge(
            &tier_from("Andrew Yates"),
            &parse_verify_output(&ran(0, &other)),
        )
        .unwrap_err();
        assert!(
            e.contains("signed by `Mallory Unrelated Customer LLC`"),
            "{e}"
        );
        for text in [VERIFIED, other.as_str()] {
            let e = judge(&tier_from(root), &parse_verify_output(&ran(0, text))).unwrap_err();
            assert!(
                e.contains("is `Microsoft Identity Verification Root"),
                "{e}"
            );
        }

        let flat = "Verifying: aterm.exe
Signing Certificate Chain:
    Issued to: Andrew Yates
    Issued by: Microsoft ID Verified CS EOC CA 01

    Issued to: Microsoft ID Verified CS EOC CA 01
    Issued by: Microsoft ID Verified Code Signing PCA 2021

The signature is timestamped: Mon Sep 15 10:00:00 2026
Successfully verified: aterm.exe
";
        let r = parse_verify_output(&ran(0, flat));
        assert_eq!((r.issued_to, r.issued_by), (None, None));
        let e = judge(
            &tier_from("Andrew Yates"),
            &parse_verify_output(&ran(0, flat)),
        )
        .unwrap_err();
        assert!(e.contains("no signing certificate chain"), "{e}");
    }

    #[test]
    fn a_self_signed_chain_is_signed_but_not_verified() {
        let r = parse_verify_output(&ran(1, UNTRUSTED));
        assert!(!r.verified && r.signed && r.untrusted_root && r.timestamped);
        assert_eq!(r.issued_to.as_deref(), Some("aterm dev"));
    }

    #[test]
    fn an_unsigned_file_reads_as_unsigned() {
        let r = parse_verify_output(&ran(1, "SignTool Error: No signature found.\n"));
        assert!(!r.signed && !r.verified && r.issued_to.is_none());
    }

    #[test]
    fn the_active_tier_refuses_everything_but_the_anchored_trusted_timestamped_leaf() {
        let active = tier_from("Andrew Yates");
        assert!(judge(&active, &parse_verify_output(&ran(0, VERIFIED))).is_ok());
        let wrong = tier_from("Someone Else");
        let e = judge(&wrong, &parse_verify_output(&ran(0, VERIFIED))).unwrap_err();
        assert!(e.contains("signed by `Andrew Yates`"), "{e}");
        let e = judge(&active, &parse_verify_output(&ran(1, UNTRUSTED))).unwrap_err();
        assert!(e.contains("root"), "{e}");
        let e = judge(
            &active,
            &parse_verify_output(&ran(1, "SignTool Error: No signature found.\n")),
        )
        .unwrap_err();
        assert!(e.contains("no signature"), "{e}");
        let no_ts = VERIFIED.replace("The signature is timestamped", "The signature is not");
        let e = judge(&active, &parse_verify_output(&ran(0, &no_ts))).unwrap_err();
        assert!(e.contains("timestamped"), "{e}");
        assert!(judge(&Tier::Inactive, &parse_verify_output(&ran(1, UNTRUSTED))).is_ok());
    }

    #[test]
    fn sign_args_per_lane_carry_a_sha256_digest_and_a_timestamp() {
        let files = vec![PathBuf::from(r"C:\d\aterm.exe")];
        let ts = Lane::TrustedSigning {
            endpoint: "https://eus.codesigning.azure.net".into(),
            account: "acct".into(),
            profile: "prof".into(),
            dlib: None,
        };
        let a = sign_args(
            &ts,
            default_timestamp(&ts),
            Some(Path::new(r"C:\n\Azure.CodeSigning.Dlib.dll")),
            Some(Path::new(r"C:\tmp\m.json")),
            &files,
        );
        assert_eq!(
            a,
            [
                "sign",
                "/v",
                "/fd",
                "SHA256",
                "/tr",
                "http://timestamp.acs.microsoft.com",
                "/td",
                "SHA256",
                "/dlib",
                r"C:\n\Azure.CodeSigning.Dlib.dll",
                "/dmdf",
                r"C:\tmp\m.json",
                r"C:\d\aterm.exe"
            ]
        );
        let th = Lane::Thumbprint("AB".repeat(20));
        let a = sign_args(&th, default_timestamp(&th), None, None, &files);
        assert_eq!(&a[8..10], &["/sha1", &"AB".repeat(20)]);
        assert!(
            !a.iter().any(|x| x == "/f" || x == "/p"),
            "no PFX/password lane, ever"
        );
        assert_eq!(
            verify_args(Path::new("a.exe")),
            ["verify", "/pa", "/v", "a.exe"]
        );
    }

    #[test]
    fn metadata_json_escapes_what_a_name_could_carry() {
        let m = trusted_signing_metadata("https://x", r#"a"b\c"#, "p");
        assert!(m.contains(r#""CodeSigningAccountName": "a\"b\\c""#), "{m}");
        assert!(m.contains(r#""Endpoint": "https://x""#));
        assert!(m.contains(r#""CertificateProfileName": "p""#));
    }

    #[test]
    fn precedence_is_flag_then_env_then_profile_and_the_lane_is_inferred() {
        let mut flag = Values::new();
        flag.insert("timestamp".into(), "http://flag".into());
        let mut env = Values::new();
        env.insert("timestamp".into(), "http://env".into());
        env.insert("thumbprint".into(), "ab".repeat(20));
        let prof = values_from_profile(
            "# comment\nsigning_key = \"ignored-by-us\"\nwinsign_timestamp = \"http://profile\"\nwinsign_signtool = \"C:\\st.exe\"\n",
        )
        .unwrap();
        let c = resolve_config(&[&flag, &env, &prof]).unwrap();
        assert_eq!(c.timestamp.as_deref(), Some("http://flag"));
        assert_eq!(c.lane, Some(Lane::Thumbprint("AB".repeat(20))));
        assert_eq!(c.signtool.as_deref(), Some(Path::new(r"C:\st.exe")));

        let mut ts = Values::new();
        ts.insert("endpoint".into(), "https://e".into());
        ts.insert("account".into(), "a".into());
        ts.insert("profile".into(), "p".into());
        let c = resolve_config(&[&ts]).unwrap();
        assert!(matches!(c.lane, Some(Lane::TrustedSigning { .. })));

        let e = resolve_config(&[&ts, &env]).unwrap_err();
        assert!(e.contains("--lane"), "{e}");
        let mut pick = Values::new();
        pick.insert("lane".into(), "thumbprint".into());
        let c = resolve_config(&[&pick, &ts, &env]).unwrap();
        assert!(matches!(c.lane, Some(Lane::Thumbprint(_))));

        let mut partial = Values::new();
        partial.insert("account".into(), "a".into());
        assert!(
            resolve_config(&[&partial])
                .unwrap_err()
                .contains("endpoint")
        );
        let mut bad = Values::new();
        bad.insert("thumbprint".into(), "nope".into());
        assert!(resolve_config(&[&bad]).unwrap_err().contains("40 hex"));
        assert_eq!(resolve_config(&[]).unwrap().lane, None);
    }

    #[test]
    fn the_profile_grammar_refuses_what_the_release_cutter_refuses() {
        assert!(values_from_profile("winsign_thumbprint = unquoted\n").is_err());
        assert!(values_from_profile("winsign_thumbprint = \"\"\n").is_err());
        assert!(values_from_profile("winsign_bogus = \"x\"\n").is_err());
        assert!(values_from_profile("garbage line\n").is_err());
        assert!(
            values_from_profile("notary_profile = \"n\"\n")
                .unwrap()
                .is_empty()
        );
        let v = values_from_profile("winsign_signtool = \"C:\\st.exe\"\n").unwrap();
        assert_eq!(v.get("signtool").map(String::as_str), Some(r"C:\st.exe"));
        let v = values_from_profile("winsign_signtool = \"C:\\\\st.exe\"\n").unwrap();
        assert_eq!(v.get("signtool").map(String::as_str), Some(r"C:\\st.exe"));
    }

    #[test]
    fn the_fake_spells_paths_the_way_the_windows_machine_it_models_would() {
        for spelling in [
            r"C:\PF\dotnet",
            "C:/PF/dotnet",
            r"C:\PF/dotnet",
            r"C:\\PF\\\dotnet",
            r"C:\PF\dotnet\",
        ] {
            assert_eq!(win_path(Path::new(spelling)), r"C:\PF\dotnet", "{spelling}");
        }
        assert_eq!(
            win_path(Path::new(r"\\server\share\x")),
            r"\\server\share\x"
        );
        assert_eq!(win_path(Path::new(r"\dir\x")), r"\dir\x");
        assert_ne!(win_path(Path::new(r"C:\PF")), win_path(Path::new(r"C:\pf")));
        assert_ne!(
            win_path(Path::new(r"C:\PF\dotnet")),
            win_path(Path::new(r"C:\PF\dotnetx"))
        );
    }

    #[test]
    fn signtool_is_found_newest_sdk_first_and_every_miss_is_named() {
        let mut h = Fake::new();
        h.env.insert("ProgramFiles(x86)".into(), r"C:\PFx86".into());
        h.env.insert("ProgramFiles".into(), r"C:\PF".into());
        let bin = PathBuf::from(r"C:\PFx86\Windows Kits\10\bin");
        h.dirs.insert(
            bin.clone(),
            vec!["10.0.9600.0".into(), "10.0.26100.0".into(), "x64".into()],
        );
        let want = win_join(&bin, ["10.0.26100.0", "x64", "signtool.exe"]);
        h.files
            .push(win_join(&bin, ["10.0.9600.0", "x64", "signtool.exe"]));
        h.files.push(want.clone());
        assert_eq!(
            win_path(&find_signtool(&h, None).unwrap()),
            r"C:\PFx86\Windows Kits\10\bin\10.0.26100.0\x64\signtool.exe"
        );
        assert!(find_signtool(&h, Some(Path::new(r"C:\nope.exe"))).is_err());
        let e = find_signtool(&Fake::new(), None).unwrap_err();
        assert!(e.contains("signtool on PATH"), "{e}");
        let mut empty = Fake::new();
        empty.env.insert("ProgramFiles".into(), r"C:\PF".into());
        let e = find_signtool(&empty, None).unwrap_err();
        assert!(e.contains(r"C:\PF\Windows Kits\10\bin\<10.x.y.z>"), "{e}");
    }

    #[test]
    fn the_dlib_is_found_under_the_nuget_cache_newest_version_first() {
        let mut h = Fake::new();
        let pkg = PathBuf::from(r"C:\Users\u\.nuget\packages\microsoft.trusted.signing.client");
        h.dirs
            .insert(pkg.clone(), vec!["1.0.52".into(), "1.0.60".into()]);
        let want = win_join(&pkg, ["1.0.60", "bin", "x64", "Azure.CodeSigning.Dlib.dll"]);
        h.files.push(win_join(
            &pkg,
            ["1.0.52", "bin", "x64", "Azure.CodeSigning.Dlib.dll"],
        ));
        h.files.push(want.clone());
        assert_eq!(
            win_path(&find_dlib(&h, None).unwrap()),
            r"C:\Users\u\.nuget\packages\microsoft.trusted.signing.client\1.0.60\bin\x64\Azure.CodeSigning.Dlib.dll"
        );
        h.env.insert("NUGET_PACKAGES".into(), r"D:\cache".into());
        let e = find_dlib(&h, None).unwrap_err();
        assert!(
            e.contains(r"D:\cache\microsoft.trusted.signing.client\<version>"),
            "{e}"
        );
    }

    #[test]
    fn windows_paths_are_joined_with_backslashes_on_every_host() {
        assert_eq!(
            win_join(r"C:\PFx86", ["Windows Kits", "10", "bin"]),
            PathBuf::from(r"C:\PFx86\Windows Kits\10\bin")
        );
        assert_eq!(win_join(r"C:\", ["x"]), PathBuf::from(r"C:\x"));
        assert_eq!(win_join("C:", ["x"]), PathBuf::from(r"C:x"));
        assert_eq!(win_join("D:/cache", ["p"]), PathBuf::from(r"D:/cache\p"));
        assert_eq!(
            win_join(r"C:\a", std::iter::empty::<&str>()),
            PathBuf::from(r"C:\a")
        );
    }

    #[test]
    fn the_renamed_package_lineage_wins_over_the_unlisted_one() {
        let mut h = Fake::new();
        let cache = PathBuf::from(r"C:\Users\u\.nuget\packages");
        let old = win_join(&cache, ["microsoft.trusted.signing.client"]);
        let new = win_join(&cache, ["microsoft.artifactsigning.client"]);
        h.dirs.insert(old.clone(), vec!["1.0.95".into()]);
        h.dirs.insert(new.clone(), vec!["1.0.128".into()]);
        let old_dll = win_join(&old, ["1.0.95", "bin", "x64", "Azure.CodeSigning.Dlib.dll"]);
        let new_dll = win_join(
            &new,
            ["1.0.128", "bin", "x64", "Azure.CodeSigning.Dlib.dll"],
        );
        h.files.push(old_dll.clone());
        assert_eq!(
            find_dlib(&h, None).unwrap(),
            old_dll,
            "the unlisted lineage still serves"
        );
        h.files.push(new_dll.clone());
        assert_eq!(
            find_dlib(&h, None).unwrap(),
            new_dll,
            "the current lineage is preferred"
        );
        let e = find_dlib(&Fake::new(), None).unwrap_err();
        assert!(
            e.contains("microsoft.artifactsigning.client")
                && e.contains("microsoft.trusted.signing.client"),
            "{e}"
        );
    }

    #[test]
    fn versions_sort_numerically() {
        assert_eq!(
            versions_newest_first(["1.0.9", "1.0.10", "x", "1.0.52"]),
            ["1.0.52", "1.0.10", "1.0.9"]
        );
    }

    #[test]
    fn sign_without_a_lane_refuses_by_name_and_help_exits_zero() {
        let mut h = Fake::new();
        h.files.push(PathBuf::from(r"C:\d\aterm.exe"));
        assert_eq!(run(vec!["help".into()], &h), 0);
        assert_eq!(run(vec!["sign".into(), r"C:\d\aterm.exe".into()], &h), 2);
        assert_eq!(run(vec!["bogus".into()], &h), 2);
        assert_eq!(run(vec!["sign".into(), "--nope".into()], &h), 2);
        assert!(h.runs.borrow().is_empty(), "nothing was executed");
    }

    #[test]
    fn sign_with_a_thumbprint_runs_signtool_then_reads_the_signature_back() {
        let mut h = Fake::new();
        let exe = PathBuf::from(r"C:\d\aterm.exe");
        let st = PathBuf::from(r"C:\sdk\signtool.exe");
        h.files.push(exe.clone());
        h.files.push(st.clone());
        h.env
            .insert("ATERM_WINSIGN_THUMBPRINT".into(), "cd".repeat(20));
        h.env
            .insert("ATERM_WINSIGN_SIGNTOOL".into(), st.display().to_string());
        h.reply = ran(0, VERIFIED);
        let code = run(vec!["sign".into(), exe.display().to_string()], &h);
        assert_eq!(code, 0);
        let runs = h.runs.borrow();
        assert_eq!(runs.len(), 2, "one sign, one verify");
        assert_eq!(runs[0].0, st);
        assert_eq!(runs[0].1[0], "sign");
        assert!(runs[0].1.contains(&"CD".repeat(20)));
        assert_eq!(runs[1].1, verify_args(&exe));
    }

    #[test]
    fn the_dotnet_runtime_is_found_global_first_and_user_local_needs_dotnet_root() {
        let mut h = Fake::new();
        h.env.insert("ProgramFiles".into(), r"C:\PF".into());
        h.env
            .insert("LOCALAPPDATA".into(), r"C:\Users\u\AppData\Local".into());
        let local = PathBuf::from(r"C:\Users\u\AppData\Local\Microsoft\dotnet");
        h.dirs.insert(
            PathBuf::from(
                r"C:\Users\u\AppData\Local\Microsoft\dotnet\shared\Microsoft.NETCore.App",
            ),
            vec!["8.0.31".into(), "6.0.36".into()],
        );
        let r = find_dotnet_runtime(&h).unwrap();
        assert_same_path!(&r.root, &local);
        assert_eq!(r.version, "8.0.31");
        assert!(r.needs_root_env);
        let global = PathBuf::from(r"C:\PF\dotnet");
        h.dirs.insert(
            PathBuf::from(r"C:\PF\dotnet\shared\Microsoft.NETCore.App"),
            vec!["9.0.4".into()],
        );
        let r = find_dotnet_runtime(&h).unwrap();
        assert_same_path!(&r.root, &global);
        assert_eq!((r.version.as_str(), r.needs_root_env), ("9.0.4", false));
        let mut old = Fake::new();
        old.env.insert("ProgramFiles".into(), r"C:\PF".into());
        old.dirs.insert(
            PathBuf::from(r"C:\PF\dotnet\shared\Microsoft.NETCore.App"),
            vec!["6.0.36".into()],
        );
        let e = find_dotnet_runtime(&old).unwrap_err();
        assert!(
            e.contains(r"C:\PF\dotnet\shared\Microsoft.NETCore.App\<8+>"),
            "{e}"
        );
    }

    /// `USAGE`'s exit codes: 1 is "the signing or the verdict is bad", 2 is
    /// "nothing was signed and no verdict was read".
    #[test]
    fn a_signtool_that_ran_and_refused_is_exit_one_not_two() {
        let exe = PathBuf::from(r"C:\d\aterm.exe");
        let st = PathBuf::from(r"C:\sdk\signtool.exe");
        let mut h = Fake::new();
        h.files.extend([exe.clone(), st.clone()]);
        h.env
            .insert("ATERM_WINSIGN_SIGNTOOL".into(), st.display().to_string());
        h.env.insert(
            "ATERM_WINSIGN_THUMBPRINT".into(),
            "A1B2C3D4E5F60718293A4B5C6D7E8F90A1B2C3D4".into(),
        );
        h.reply = ran(
            1,
            "SignTool Error: An unexpected internal error has occurred.",
        );
        assert_eq!(
            run(vec!["sign".into(), exe.display().to_string()], &h),
            1,
            "a refused signature must not wear the missing-tool code"
        );
        assert_eq!(h.runs.borrow().len(), 1, "it stopped at the failed sign");

        let mut gone = Fake::new();
        gone.files.push(exe.clone());
        gone.env.insert(
            "ATERM_WINSIGN_THUMBPRINT".into(),
            "A1B2C3D4E5F60718293A4B5C6D7E8F90A1B2C3D4".into(),
        );
        assert_eq!(
            run(vec!["sign".into(), exe.display().to_string()], &gone),
            2,
            "nothing to sign with is still exit 2"
        );
    }

    #[test]
    fn a_trusted_signing_sign_passes_dotnet_root_for_a_user_local_runtime() {
        let mut h = Fake::new();
        let exe = PathBuf::from(r"C:\d\aterm.exe");
        let st = PathBuf::from(r"C:\sdk\signtool.exe");
        let dlib = PathBuf::from(r"C:\n\Azure.CodeSigning.Dlib.dll");
        h.files.extend([exe.clone(), st.clone(), dlib.clone()]);
        h.env
            .insert("ATERM_WINSIGN_SIGNTOOL".into(), st.display().to_string());
        h.env
            .insert("ATERM_WINSIGN_DLIB".into(), dlib.display().to_string());
        h.env.insert(
            "ATERM_WINSIGN_ENDPOINT".into(),
            "https://eus.codesigning.azure.net".into(),
        );
        h.env.insert("ATERM_WINSIGN_ACCOUNT".into(), "acct".into());
        h.env.insert("ATERM_WINSIGN_PROFILE".into(), "prof".into());
        h.env
            .insert("LOCALAPPDATA".into(), r"C:\Users\u\AppData\Local".into());
        h.dirs.insert(
            PathBuf::from(
                r"C:\Users\u\AppData\Local\Microsoft\dotnet\shared\Microsoft.NETCore.App",
            ),
            vec!["8.0.31".into()],
        );
        h.reply = ran(0, VERIFIED);
        assert_eq!(run(vec!["sign".into(), exe.display().to_string()], &h), 0);
        let runs = h.runs.borrow();
        assert_eq!(runs.len(), 2);
        assert!(runs[0].1.contains(&"/dlib".to_string()));
        let child_env = &runs[0].2;
        assert_eq!(child_env.len(), 1, "one variable, and only that one");
        assert_eq!(child_env[0].0, "DOTNET_ROOT");
        assert_eq!(
            win_path(Path::new(&child_env[0].1)),
            r"C:\Users\u\AppData\Local\Microsoft\dotnet"
        );
        assert!(runs[1].2.is_empty(), "verify needs no runtime");
        let mut none = Fake::new();
        none.files.extend([exe.clone(), st.clone(), dlib.clone()]);
        none.env = h.env.clone();
        none.env.remove("LOCALAPPDATA");
        assert_eq!(
            run(vec!["sign".into(), exe.display().to_string()], &none),
            2
        );
        assert!(none.runs.borrow().is_empty());
    }

    #[test]
    fn verify_reports_an_untrusted_chain_with_exit_one_even_when_inactive() {
        let mut h = Fake::new();
        let exe = PathBuf::from(r"C:\d\aterm.exe");
        let st = PathBuf::from(r"C:\sdk\signtool.exe");
        h.files.push(exe.clone());
        h.files.push(st.clone());
        h.env
            .insert("ATERM_WINSIGN_SIGNTOOL".into(), st.display().to_string());
        h.reply = ran(1, UNTRUSTED);
        assert_eq!(run(vec!["verify".into(), exe.display().to_string()], &h), 1);
        h.reply = ran(0, VERIFIED);
        assert_eq!(run(vec!["verify".into(), exe.display().to_string()], &h), 0);
    }
}
