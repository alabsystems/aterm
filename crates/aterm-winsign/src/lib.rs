// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! `cargo winsign` — Authenticode signing and verification of THE shipped
//! Windows executable, and the Windows half of the identity tier the mac lane
//! already has (`crates/aterm-release/src/sign.rs`, Tier APPLE).
//!
//! # Why this exists: Code Integrity, not SmartScreen
//!
//! Windows 11 with Smart App Control ON runs unsigned executables only while
//! Microsoft's reputation service happens to say yes, and says no whenever it
//! likes — measured on this box on 2026-09-15: the installed `aterm-gui.exe`
//! ran for a day, then Code Integrity refused it from Explorer with event 3077
//! ("did not meet the Enterprise signing level requirements"), and no rerun
//! fixed it. That is not the SmartScreen "more info → run anyway" prompt an
//! unsigned download gets; it is the kernel's code-integrity policy, and the
//! only durable answer is a signature whose chain that policy trusts. Two
//! identities qualify: **Azure Artifact Signing** (formerly Trusted Signing; Microsoft's own service —
//! Smart App Control trusts it by design, and it is the lane Microsoft
//! recommends for exactly this) and a **publicly trusted code-signing
//! certificate** from a CA (EV gets reputation at once; OV earns it). A
//! self-signed certificate — the MSIX development lane's — satisfies neither,
//! however widely it is trusted locally, so it is deliberately not a lane here.
//!
//! # The two states, and nothing in between
//!
//! Tier WINDOWS is driven by ONE committed constant,
//! `aterm_update_core::pins::WINDOWS_SIGNING_PUBLISHER`:
//!
//! * **anchor empty → [`Tier::Inactive`].** Signing is optional: with a lane
//!   configured the exe is signed and the result reported; without one the
//!   verbs say what is missing and exit non-zero, so a script that expected a
//!   signature does not read silence as success. No PUBLISHER is required here:
//!   the tier itself refuses nothing ([`judge`] is `Ok` for every report). The
//!   verdict is still the one Code Integrity will reach, though, so an exe that
//!   is unsigned — or whose chain the Authenticode policy does not trust — is
//!   reported and exits 1 even in this state.
//! * **anchor set → [`Tier::Active`].** Signing is REQUIRED, and after signing
//!   the tool reads the signature back off the file: it must verify under the
//!   default Authenticode policy, its leaf must be issued to exactly the
//!   anchored publisher, and it must carry a timestamp — or the verb fails.
//!   The anchor is the leaf certificate's subject Common Name as `signtool
//!   verify /v` prints it after `Issued to:`; it is committed, never read from
//!   the environment, so a build signed by the wrong identity cannot pass.
//!
//! # Where each value comes from
//!
//! The identity is never on the command line by accident and never in the
//! tree. Precedence is flag > `ATERM_WINSIGN_*` environment > the credentials
//! profile (`--credentials <file>`, the same `key = "value"` grammar as the
//! release cutter's `~/.aterm/release-credentials.toml`, keys `winsign_*`). No
//! lane takes a password: the thumbprint lane names a certificate already in
//! the user's store (`signtool /sha1`, never `/f <pfx> /p <password>`), and
//! the Trusted Signing lane authenticates through Azure identity — measured
//! with client 1.0.95: its metadata defaults to `"ExcludeCredentials": []`, so
//! with nothing else configured it opens the system browser on a Microsoft
//! sign-in; an `az login` session or the `AZURE_TENANT_ID` /
//! `AZURE_CLIENT_ID` / `AZURE_CLIENT_SECRET` triple is used first when present.
//! The plug-in is a .NET 8 component: a machine-wide runtime is found by the
//! host loader, a user-local one (`dotnet-install.ps1`, no admin) is handed to
//! signtool as `DOTNET_ROOT` by this tool.
//!
//! # What it drives
//!
//! `signtool.exe` from the Windows SDK, found on `PATH` or under the Windows
//! Kits root, newest SDK first. The Artifact Signing lane also needs the
//! `Microsoft.ArtifactSigning.Client` (or the unlisted `Microsoft.Trusted.Signing.Client`) NuGet package's
//! `Azure.CodeSigning.Dlib.dll`, found under the NuGet cache or named
//! explicitly. Everything the tool decides — argument shapes, precedence, the
//! verdict read off `signtool verify` — is pure and unit-tested against a fake
//! host; [`RealHost`] is what reaches the machine, with one exception a reader
//! of the fake-host tests should know: `sign` unlinks its own temporary Trusted
//! Signing metadata file through `std::fs` rather than the seam.

use std::collections::BTreeMap;
use std::fmt;
use std::path::{Path, PathBuf};
use std::process::Command;

pub use aterm_update_core::pins;

// ---------------------------------------------------------------------------
// The tier
// ---------------------------------------------------------------------------

/// Tier WINDOWS, read off the committed anchor.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Tier {
    /// No anchor: signing is optional and the tier itself refuses nothing —
    /// but verification is not advisory, because the verdict is Code
    /// Integrity's: an unsigned exe, or one whose chain the Authenticode
    /// policy does not trust, is reported and exits 1 in this state too.
    Inactive,
    /// The anchor names the publisher every shipped exe must be signed by.
    Active {
        /// The leaf certificate's subject CN, as `signtool verify /v` prints it.
        publisher: String,
    },
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
            Tier::Inactive => f.write_str("Tier WINDOWS inactive (pins::WINDOWS_SIGNING_PUBLISHER is empty): signing optional, this tier refuses nothing — but an unsigned or untrusted chain is still reported and exits 1"),
            Tier::Active { publisher } => write!(f, "Tier WINDOWS active: every shipped exe must be signed by `{publisher}`, timestamped, and chain to a trusted root"),
        }
    }
}

// ---------------------------------------------------------------------------
// The host seam
// ---------------------------------------------------------------------------

/// The result of running a tool.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Ran {
    pub code: Option<i32>,
    pub stdout: String,
    pub stderr: String,
}

/// Everything this tool needs from the machine, behind one trait so every
/// decision is testable against a fake.
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

// ---------------------------------------------------------------------------
// Configuration: flags > environment > credentials profile
// ---------------------------------------------------------------------------

/// The signing lane — WHICH identity signs, and through what.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Lane {
    /// Azure Trusted Signing: the account and certificate profile on the
    /// service, reached through the client dlib and Azure identity.
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

/// Environment values: `ATERM_WINSIGN_<KEY>`.
pub fn values_from_env(host: &dyn Host) -> Values {
    KEYS.iter()
        .filter_map(|k| {
            host.env(&format!("ATERM_WINSIGN_{}", k.to_ascii_uppercase()))
                .map(|v| (k.to_string(), v))
        })
        .collect()
}

/// Credentials-profile values: `winsign_<key> = "…"` lines, the release
/// cutter's grammar (comments, blank lines, quoted strings, nothing else).
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
    /// RFC 3161 timestamp server. `None` = the lane's default.
    pub timestamp: Option<String>,
    /// An explicit `signtool.exe`; `None` = find it.
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

/// The timestamp server a lane uses when none is configured. Trusted Signing
/// publishes its own; the DigiCert one is a widely trusted public RFC 3161
/// responder for store certificates.
#[must_use]
pub fn default_timestamp(lane: &Lane) -> &'static str {
    match lane {
        Lane::TrustedSigning { .. } => "http://timestamp.acs.microsoft.com",
        Lane::Thumbprint(_) => "http://timestamp.digicert.com",
    }
}

// ---------------------------------------------------------------------------
// Finding the tools
// ---------------------------------------------------------------------------

/// Join components onto a WINDOWS path, with Windows' separator, on whatever
/// host this binary was built for.
///
/// `Path::join` is the BUILD host's join, and every path this module assembles
/// is a path on the machine that SIGNS: the Windows SDK layout, the NuGet
/// cache, the dotnet root — handed to `signtool.exe` in its argv or its
/// environment, or printed for a person who will type it into a Windows
/// prompt. Built on Windows the two agree byte for byte (everything joined
/// here is a plain relative name, never a rooted one, so `Path::join`'s
/// absolute-path replacement never fires); built anywhere else `Path::join`
/// writes `/` and reads `C:\Users\u` as ONE opaque component, which is how
/// `C:\Users\u/.nuget/packages/...\<version>` — half platform join, half
/// hand-written separator — reached an error message. Spelling it here means
/// the discovery this crate's tests exercise is the discovery Windows gets,
/// from the macOS box that cuts the release as much as from Windows itself.
fn win_join<'a>(base: impl Into<PathBuf>, parts: impl IntoIterator<Item = &'a str>) -> PathBuf {
    let mut out = base.into().into_os_string();
    for part in parts {
        // Neither double a separator the caller already wrote nor put one
        // after a bare drive (`C:` + `x` is `C:x`, drive-relative, exactly as
        // Windows' own join has it).
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
///
/// The one place host-native path handling is the RIGHT handling: the `PATH`
/// being split, and the directories in it, come from the same machine this is
/// running on, so its separators are this platform's — unlike the Windows tool
/// layouts below, which are [`win_join`]ed.
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

/// `signtool.exe`: configured, on `PATH` (a Developer prompt), else the newest
/// Windows SDK under either program-files root. Every miss is named.
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
/// The service was renamed from Trusted Signing to **Artifact Signing** at its
/// GA (January 2026); `Microsoft.Trusted.Signing.Client` is unlisted at 1.0.95
/// and `Microsoft.ArtifactSigning.Client` (1.0.128 at the time of writing) is
/// its continuation — same dll name inside, same metadata file, same
/// `net8.0` runtime config. Both are searched so an older cache still signs.
pub const DLIB_PACKAGES: &[&str] = &[
    "microsoft.artifactsigning.client",
    "microsoft.trusted.signing.client",
];

/// `Azure.CodeSigning.Dlib.dll`: configured, else the newest version of the
/// newest package lineage in [`DLIB_PACKAGES`] under the NuGet cache
/// (`NUGET_PACKAGES`, else `~\.nuget\packages`).
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
    /// A user-local install the host loader would not find on its own: the
    /// child gets `DOTNET_ROOT` set to `root`.
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

/// Find a .NET runtime the dlib can load, in the order the host loader itself
/// resolves one: `DOTNET_ROOT`, the machine-wide install under Program Files,
/// then the user-local install `dotnet-install.ps1` makes without admin — which
/// the loader does NOT find by itself, so that one is passed as `DOTNET_ROOT`.
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

// ---------------------------------------------------------------------------
// Argument shapes (pure)
// ---------------------------------------------------------------------------

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

/// `signtool sign …` for one lane over the given files. `/fd SHA256` and an
/// RFC 3161 `/tr … /td SHA256` timestamp always: a signature without a
/// timestamp dies with its certificate, and an SHA-1 file digest is refused by
/// modern policy.
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

/// `signtool verify /pa /v <file>` — the default Authenticode policy (`/pa`),
/// which is the chain Code Integrity and SmartScreen both evaluate; the
/// verbose form prints the chain this tool reads back.
#[must_use]
pub fn verify_args(file: &Path) -> Vec<String> {
    vec![
        "verify".into(),
        "/pa".into(),
        "/v".into(),
        file.display().to_string(),
    ]
}

// ---------------------------------------------------------------------------
// Reading the verdict back (pure)
// ---------------------------------------------------------------------------

/// What `signtool verify /pa /v` said about one file.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct VerifyReport {
    /// Exit 0 and "Successfully verified": the chain is trusted under `/pa`.
    pub verified: bool,
    /// A signature is present at all.
    pub signed: bool,
    /// The chain terminates in a root the trust provider does not trust —
    /// the self-signed case, and the one Code Integrity refuses.
    pub untrusted_root: bool,
    /// The leaf's `Issued to:` (the subject CN), when a chain was printed.
    pub issued_to: Option<String>,
    /// The leaf's `Issued by:`.
    pub issued_by: Option<String>,
    /// "The signature is timestamped".
    pub timestamped: bool,
}

/// Parse signtool's verbose verify output. Tolerant of wording drift on
/// purpose: absence of a marker reads as absence of the property, never as a
/// panic, and the raw text is what the verb prints when something is off.
#[must_use]
pub fn parse_verify_output(ran: &Ran) -> VerifyReport {
    let text = format!("{}\n{}", ran.stdout, ran.stderr);
    let mut report = VerifyReport {
        verified: ran.code == Some(0) && text.contains("Successfully verified"),
        signed: !text.contains("No signature found"),
        untrusted_root: text.contains("not trusted by the trust provider")
            || text.contains("certificate chain could not be built"),
        timestamped: text.contains("The signature is timestamped"),
        ..VerifyReport::default()
    };
    let mut in_chain = false;
    for line in text.lines() {
        let l = line.trim();
        if l.starts_with("Signing Certificate Chain:") {
            in_chain = true;
            continue;
        }
        if !in_chain {
            continue;
        }
        if let Some(v) = l.strip_prefix("Issued to:") {
            if report.issued_to.is_none() {
                report.issued_to = Some(v.trim().to_string());
            }
        } else if let Some(v) = l.strip_prefix("Issued by:") {
            if report.issued_by.is_none() {
                report.issued_by = Some(v.trim().to_string());
            }
        } else if l.starts_with("The signature is timestamped")
            || l.starts_with("Timestamp Verified by:")
            || l.starts_with("File has page hashes")
            || l.starts_with("Successfully verified")
        {
            in_chain = false;
        }
    }
    report
}

/// The tier's judgement of one verified file. `Ok(())` is "ship it";
/// `Err` names the one thing that is wrong.
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

// ---------------------------------------------------------------------------
// The verbs
// ---------------------------------------------------------------------------

pub const USAGE: &str =
    "aterm-winsign — Authenticode signing of the shipped Windows exe (Tier WINDOWS)

USAGE
  cargo winsign sign   [options] <exe>...   sign, then read the signature back and judge it
  cargo winsign verify [options] <exe>...   read the signature back and judge it (no signing)
  cargo winsign doctor [options]            what this machine can sign with, and what is missing
  cargo winsign help

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
  0 nothing to refuse: `sign`/`verify` read a verdict and the tier accepts it,
    `doctor` is READY, `help` printed
  1 the signing or the verdict is bad: a verdict was read and it is unsigned, an
    untrusted chain, or a signature the active tier refuses (the first two count
    in every tier) — or `sign` launched signtool and signtool exited non-zero (a
    refused credential, a rejected profile), its output printed
  2 nothing was signed and no verdict was read — EVERY refusal before either,
    including the arguments: an unknown verb or option, a flag with no value, no
    exe named, a path that is not a file, a credentials profile that will not
    read, a lane that does not resolve (ambiguous, incomplete, or a thumbprint
    that is not 40 hex digits), nothing to sign with, a missing tool, or one that
    could not be launched at all (says what and where)

The identity itself comes from Azure Artifact Signing (formerly Trusted Signing)
or a publicly trusted code-signing certificate: apps/aterm-win/SIGNING.md is the
runbook.";

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

/// Run the tool. Returns the exit code.
pub fn run(args: Vec<String>, host: &dyn Host) -> i32 {
    match run_inner(&args, host) {
        Ok(code) => code,
        Err(e) => {
            eprintln!("cargo winsign: {e}");
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
        "verify" | "status" => {
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
                    // Host-native on purpose, not `win_join`: this file is
                    // one WE create, with this host's filesystem, under this
                    // host's temp dir — the only path here that is not a
                    // Windows layout read off the signing machine.
                    let meta = host
                        .temp_dir()
                        .join(format!("aterm-winsign-{}.json", std::process::id()));
                    host.write(&meta, &trusted_signing_metadata(endpoint, account, profile))?;
                    eprintln!(
                        "lane: Azure Artifact Signing (formerly Trusted Signing) — {account} / {profile} at {endpoint}"
                    );
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
                // **A SIGNTOOL THAT RAN AND REFUSED IS A SIGNING FAILURE, NOT A
                // MISSING TOOL.** `run` maps every `Err` to exit 2, which
                // `USAGE` reserves for "nothing was signed and no verdict was
                // read" — so returning `Err` here made a refused credential
                // or a rejected profile indistinguishable, to the script that
                // calls this, from an absent SDK. `USAGE`'s own exit 1 covers
                // "`sign` launched signtool and signtool exited non-zero", which
                // is exactly what this is. Found 2026-09-15 by reading `USAGE`
                // against this handler for the help-surface roster; the
                // contradiction had shipped with the lane.
                eprintln!("cargo winsign: signtool sign failed (exit {:?})", ran.code);
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
                println!(
                    "{name}: UNSIGNED — Smart App Control (Code Integrity) may refuse it at any time; {tier}"
                );
                Ok(false)
            } else {
                println!(
                    "{name}: signed by `{}` but the chain is NOT trusted ({}) — Code Integrity will refuse it exactly as it refuses an unsigned exe; a self-signed certificate cannot satisfy it (apps/aterm-win/SIGNING.md)",
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
                "lane: Azure Artifact Signing (formerly Trusted Signing) — account {account}, profile {profile}, endpoint {endpoint}"
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
            // MEASURED 2026-09-15 (client 1.0.95): the plug-in's default metadata
            // is `"ExcludeCredentials": []` — every Azure.Identity credential is
            // tried, and with nothing else on the box it opens the system
            // browser on a Microsoft sign-in. So a desktop with a person at it
            // is never "without a credential"; the two unattended routes are
            // reported when present because they change what happens on `sign`.
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
            "READY: `cargo winsign sign <exe>` will run."
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

    /// One path, spelled the way the WINDOWS machine this fake models would
    /// spell it: `/` and `\` are both separators, runs of them collapse, and a
    /// trailing one is nothing.
    ///
    /// The fake needs this because `Path`'s own comparison is host-dependent
    /// and cannot be borrowed for the job. `impl PartialEq for Path` compares
    /// `components()`, and the component parser is the host's: on Windows `\`
    /// and `/` both split and `C:` is a `Prefix`, so `C:\PF\dotnet`,
    /// `C:\PF/dotnet` and `C:\\PF\dotnet` are all one path; on Unix `\` is an
    /// ordinary filename character, so `C:\PF\dotnet` is a SINGLE component and
    /// none of those three are equal (measured on this box, 2026-09-15).
    ///
    /// This fake was written when production built these paths with
    /// `PathBuf::join`, which inserts the BUILD host's separator — `\` on the
    /// Windows machine that actually signs, `/` under a test run on macOS. The
    /// Windows tool layouts go through [`win_join`] now, which writes `\` on
    /// every host; `Path::join` survives only where host-native handling is the
    /// right handling ([`find_on_path`]'s `PATH` entries, and the temporary
    /// metadata file under this host's temp dir), so both spellings still reach
    /// this fake. Either way it is the FAKE that must not key its filesystem on
    /// raw host bytes, because off-Windows every lookup then missed and the
    /// whole find-the-tools half of this crate went untested on the box that
    /// cuts releases — a Windows-target crate whose tests only run on Windows
    /// gives this repo no signal.
    ///
    /// Case is deliberately NOT folded, though Windows would: the fake should
    /// never answer to a name no test wrote.
    fn win_path(path: &Path) -> String {
        let s = path.to_string_lossy().replace('/', "\\");
        let body = s.trim_start_matches('\\');
        // A leading run is the one run that carries meaning: `\\server\share`
        // is a UNC root and `\dir` is root-relative, so keep what Windows keeps.
        let lead = "\\".repeat((s.len() - body.len()).min(2));
        let segments: Vec<&str> = body.split('\\').filter(|s| !s.is_empty()).collect();
        format!("{lead}{}", segments.join("\\"))
    }

    /// `win_path` on both sides of an `assert_eq!`, so a mismatch prints the two
    /// Windows spellings rather than two host-mangled ones.
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

    const VERIFIED: &str = "Verifying: aterm.exe
Signature Index: 0 (Primary Signature)
Hash of file (sha256): AB
Signing Certificate Chain:
    Issued to: Andrew Yates
    Issued by: Microsoft ID Verified CS EOC CA 01
    Expires:   Wed Sep 16 00:00:00 2026
    SHA1 hash: 11

    Issued to: Microsoft ID Verified CS EOC CA 01
    Issued by: Microsoft ID Verified Code Signing PCA 2021
    Expires:   Mon Apr 13 00:00:00 2036
    SHA1 hash: 22

The signature is timestamped: Mon Sep 15 10:00:00 2026
Timestamp Verified by:
    Issued to: Microsoft Public RSA Timestamping CA 2020
    Issued by: Microsoft Identity Verification Root Certificate Authority 2020

Successfully verified: aterm.exe

Number of files successfully Verified: 1
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

    #[test]
    fn a_verified_chain_is_read_back_leaf_first() {
        let r = parse_verify_output(&ran(0, VERIFIED));
        assert!(r.verified && r.signed && r.timestamped && !r.untrusted_root);
        assert_eq!(r.issued_to.as_deref(), Some("Andrew Yates"));
        assert_eq!(
            r.issued_by.as_deref(),
            Some("Microsoft ID Verified CS EOC CA 01")
        );
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
        // Inactive judges nothing: advisory only.
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
        // ONE backslash in the file, because the grammar is verbatim between the
        // quotes — see `the_profile_grammar_refuses_what_the_release_cutter_refuses`.
        // This fixture used to write two and expect one; on Windows that passed
        // anyway, because `Path` collapses the doubled separator.
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

        // Both configured and no --lane: refuse rather than guess.
        let e = resolve_config(&[&ts, &env]).unwrap_err();
        assert!(e.contains("--lane"), "{e}");
        let mut pick = Values::new();
        pick.insert("lane".into(), "thumbprint".into());
        let c = resolve_config(&[&pick, &ts, &env]).unwrap();
        assert!(matches!(c.lane, Some(Lane::Thumbprint(_))));

        // A partial Trusted Signing triple is an error that names the missing keys.
        let mut partial = Values::new();
        partial.insert("account".into(), "a".into());
        assert!(
            resolve_config(&[&partial])
                .unwrap_err()
                .contains("endpoint")
        );
        // A malformed thumbprint is refused.
        let mut bad = Values::new();
        bad.insert("thumbprint".into(), "nope".into());
        assert!(resolve_config(&[&bad]).unwrap_err().contains("40 hex"));
        // Nothing configured is not an error here; `sign` refuses later, by name.
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
        // What is between the quotes is taken VERBATIM: no escape processing,
        // exactly like the release cutter's `credentials_value`, which returns a
        // borrowed slice of the file and so could not unescape if it wanted to.
        // A Windows path is therefore written with the single backslashes it
        // really has — doubling them (a TOML habit, and the file is named
        // `.toml`) yields a doubled separator, which Windows tolerates but which
        // is not what the writer asked for.
        let v = values_from_profile("winsign_signtool = \"C:\\st.exe\"\n").unwrap();
        assert_eq!(v.get("signtool").map(String::as_str), Some(r"C:\st.exe"));
        let v = values_from_profile("winsign_signtool = \"C:\\\\st.exe\"\n").unwrap();
        assert_eq!(v.get("signtool").map(String::as_str), Some(r"C:\\st.exe"));
    }

    #[test]
    fn the_fake_spells_paths_the_way_the_windows_machine_it_models_would() {
        // Both separators, runs collapse, a trailing one is nothing — the three
        // ways a Windows path can be respelled without naming a different file,
        // and the three that `Path` gets right on Windows and wrong on Unix.
        for spelling in [
            r"C:\PF\dotnet",
            "C:/PF/dotnet",
            r"C:\PF/dotnet",
            r"C:\\PF\\\dotnet",
            r"C:\PF\dotnet\",
        ] {
            assert_eq!(win_path(Path::new(spelling)), r"C:\PF\dotnet", "{spelling}");
        }
        // A leading run is the one run that means something.
        assert_eq!(
            win_path(Path::new(r"\\server\share\x")),
            r"\\server\share\x"
        );
        assert_eq!(win_path(Path::new(r"\dir\x")), r"\dir\x");
        // And it does not fuse two different paths into one.
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
        // The SDK's own bin root, spelled the way Windows spells it — the
        // fixture is a Windows machine's filesystem whatever this test is
        // built for.
        let bin = PathBuf::from(r"C:\PFx86\Windows Kits\10\bin");
        h.dirs.insert(
            bin.clone(),
            vec!["10.0.9600.0".into(), "10.0.26100.0".into(), "x64".into()],
        );
        // BOTH SDKs have a signtool.exe, so "newest first" is what this measures.
        // With only the newest installed, any order would find it and the name of
        // this test would be a claim nothing checked. Joined the Windows way, like
        // the discovery it feeds, so the fixture matches on every build host.
        let want = win_join(&bin, ["10.0.26100.0", "x64", "signtool.exe"]);
        h.files
            .push(win_join(&bin, ["10.0.9600.0", "x64", "signtool.exe"]));
        h.files.push(want.clone());
        // The literal, not `want`: this pins the Windows spelling the SDK layout
        // actually has, on whichever host runs the test.
        assert_eq!(
            win_path(&find_signtool(&h, None).unwrap()),
            r"C:\PFx86\Windows Kits\10\bin\10.0.26100.0\x64\signtool.exe"
        );
        // A configured path that does not exist is an error, not a fallback.
        assert!(find_signtool(&h, Some(Path::new(r"C:\nope.exe"))).is_err());
        // Nothing anywhere: the error names the roots it looked under.
        let e = find_signtool(&Fake::new(), None).unwrap_err();
        assert!(e.contains("signtool on PATH"), "{e}");
        // An SDK root with no versioned bin under it is named as one path, in
        // one spelling, placeholder included.
        let mut empty = Fake::new();
        empty.env.insert("ProgramFiles".into(), r"C:\PF".into());
        let e = find_signtool(&empty, None).unwrap_err();
        assert!(e.contains(r"C:\PF\Windows Kits\10\bin\<10.x.y.z>"), "{e}");
    }

    #[test]
    fn the_dlib_is_found_under_the_nuget_cache_newest_package_first() {
        let mut h = Fake::new();
        let pkg = PathBuf::from(r"C:\Users\u\.nuget\packages\microsoft.trusted.signing.client");
        h.dirs
            .insert(pkg.clone(), vec!["1.0.52".into(), "1.0.60".into()]);
        let want = win_join(&pkg, ["1.0.60", "bin", "x64", "Azure.CodeSigning.Dlib.dll"]);
        // Both packages carry the dll, so this measures "newest package first"
        // rather than "the only one present".
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
        // The invariant the discovery above rests on, pinned where it can be
        // read: these are paths on the machine that SIGNS, so they are spelled
        // the way that machine spells them no matter what this is built for.
        // `Path::join` would write `/` here on the macOS box that cuts the
        // release, and `C:\PFx86` is one opaque component to it.
        assert_eq!(
            win_join(r"C:\PFx86", ["Windows Kits", "10", "bin"]),
            PathBuf::from(r"C:\PFx86\Windows Kits\10\bin")
        );
        // A base that already ends in a separator does not grow a second one,
        // and a bare drive stays drive-relative — Windows' own join, exactly.
        assert_eq!(win_join(r"C:\", ["x"]), PathBuf::from(r"C:\x"));
        assert_eq!(win_join("C:", ["x"]), PathBuf::from(r"C:x"));
        // A separator the operator typed is left as they typed it.
        assert_eq!(win_join("D:/cache", ["p"]), PathBuf::from(r"D:/cache\p"));
        // Nothing to join is the base itself.
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
        // Only a user-local install: found, and flagged for DOTNET_ROOT.
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
        // A machine-wide install wins and needs nothing passed.
        let global = PathBuf::from(r"C:\PF\dotnet");
        h.dirs.insert(
            PathBuf::from(r"C:\PF\dotnet\shared\Microsoft.NETCore.App"),
            vec!["9.0.4".into()],
        );
        let r = find_dotnet_runtime(&h).unwrap();
        assert_same_path!(&r.root, &global);
        assert_eq!((r.version.as_str(), r.needs_root_env), ("9.0.4", false));
        // A runtime older than the dlib's floor does not count.
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

    /// **A SIGNTOOL THAT RAN AND REFUSED EXITS 1, NOT 2.** `USAGE` publishes
    /// three exit codes and `apps/aterm-win/build.ps1 -Sign` throws on any
    /// non-zero, so the only reader that can act on the difference is a human
    /// or a script reading the code: 1 is "the signing or the verdict is bad",
    /// 2 is "nothing was signed and no verdict was read".
    /// `run_inner` returned `Err` for a signtool that ran and failed, and `run`
    /// maps every `Err` to 2 — so a refused credential or a rejected profile
    /// was indistinguishable from an absent SDK. Found 2026-09-15 by reading
    /// `USAGE` against its handler for the help-surface roster.
    ///
    /// THE TWIN: return `Err` there again and this goes red with 2 against 1,
    /// while `a_missing_signtool_is_exit_two` below keeps passing — which is
    /// the point: the two causes must not share a code.
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
        // signtool itself ran and said no — a bad credential, a refused profile.
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

        // …and the missing-tool cause still has exit 2 to itself.
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
        // Without any runtime the sign refuses before running signtool.
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
