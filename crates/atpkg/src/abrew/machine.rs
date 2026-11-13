// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates
//! The machine abrew's admission judges a recipe against (design §3).

use std::sync::OnceLock;

/// Each macOS release abrew names, newest first: its major version (`10` is
/// 10.15 alone), Homebrew's name for it, and the first Xcode released for it,
/// which is Homebrew's `MacOS::Xcode.minimum_version` (`os/mac/xcode.rb`).
const RELEASES: [(u64, &str, &str); 8] = [
    (27, "golden_gate", "27.0"),
    (26, "tahoe", "26.0"),
    (15, "sequoia", "16.0"),
    (14, "sonoma", "15.0"),
    (13, "ventura", "14.1"),
    (12, "monterey", "13.1"),
    (11, "big_sur", "12.2"),
    (10, "catalina", "11.0"),
];

const OLDEST_BOTTLE_MAJOR: u64 = 11;

/// brew `MacOSVersion::SYMBOLS` keys (newest first).
pub const MACOS_SYMBOLS: [&str; 7] = [
    "golden_gate",
    "tahoe",
    "sequoia",
    "sonoma",
    "ventura",
    "monterey",
    "big_sur",
];

/// brew `Tag#standardized_arch`.
fn standardized_arch(arch: &str) -> &str {
    match arch {
        "x86_64" | "intel" => "x86_64",
        "arm64" | "arm" | "aarch64" => "arm64",
        other => other,
    }
}

/// brew `Utils::Bottles::Tag#to_sym` for a system/arch pair.
#[must_use]
pub fn bottle_tag_symbol(system: &str, arch: &str) -> String {
    let arch = standardized_arch(arch);
    if system == "all" && arch == "all" {
        return "all".to_string();
    }
    if MACOS_SYMBOLS.contains(&system) && arch == "x86_64" {
        return system.to_string();
    }
    format!("{arch}_{system}")
}

/// brew `Tag.from_symbol(value).to_sym`; `None` is brew's
/// `Invalid bottle tag symbol` ArgumentError.
#[must_use]
pub fn bottle_tag_from_symbol(value: &str) -> Option<String> {
    if value == "all" {
        return Some("all".to_string());
    }
    // Hardware::CPU::ALL_ARCHS, in order.
    const ALL_ARCHS: [&str; 12] = [
        "i386", "x86_64", "ppc", "ppc32", "ppc7400", "ppc7450", "ppc970", "ppc64", "ppc64le",
        "ppc970", "arm64", "aarch64",
    ];
    let wordy = |s: &str| {
        !s.is_empty()
            && s.chars()
                .all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '.')
    };
    for arch in ALL_ARCHS {
        if let Some(system) = value
            .strip_prefix(arch)
            .and_then(|rest| rest.strip_prefix('_'))
            && wordy(system)
        {
            return Some(bottle_tag_symbol(system, arch));
        }
    }
    wordy(value).then(|| bottle_tag_symbol(value, "x86_64"))
}

/// The tag symbol for a `--os`/`--arch` simulation pair
/// (brew `Tag.from_arg(nil, os:, arch:)` after
/// `os_arch_combinations` defaulting).
#[must_use]
pub fn simulated_bottle_tag(os: Option<&str>, arch: Option<&str>) -> String {
    let host = Machine::this();
    let system = match os {
        Some(os) => os.to_string(),
        None => match host.os {
            Os::Linux => "linux".to_string(),
            Os::Macos => host
                .macos_version()
                .and_then(release)
                .map(|(_, name, _)| name.to_string())
                .unwrap_or_else(|| "macos".to_string()),
            Os::Other => "macos".to_string(),
        },
    };
    let arch = arch.unwrap_or(&host.arch);
    bottle_tag_symbol(&system, arch)
}

/// brew `os_arch_combinations` as (os, arch) value pairs:
/// `all` expands to ALL_OS_OPTIONS / ARCH_OPTIONS; unset
/// stays `None` (the host's own, resolved downstream).
#[must_use]
pub fn os_arch_combinations(
    os: Option<&str>,
    arch: Option<&str>,
) -> Vec<(Option<String>, Option<String>)> {
    let mut oses: Vec<Option<String>> = Vec::new();
    match os {
        Some("all") => {
            oses.extend(MACOS_SYMBOLS.iter().map(|s| Some((*s).to_string())));
            oses.push(Some("linux".to_string()));
        }
        Some(os) => oses.push(Some(os.to_string())),
        None => oses.push(None),
    }
    let arches: Vec<Option<String>> = match arch {
        Some("all") => vec![Some("intel".to_string()), Some("arm".to_string())],
        Some(arch) => vec![Some(arch.to_string())],
        None => vec![None],
    };
    let mut out = Vec::new();
    for os in &oses {
        for arch in &arches {
            out.push((os.clone(), arch.clone()));
        }
    }
    out
}

/// Every (os, arch) combination as bottle tag symbols.
#[must_use]
pub fn bottle_tag_combinations(os: Option<&str>, arch: Option<&str>) -> Vec<String> {
    os_arch_combinations(os, arch)
        .iter()
        .map(|(os, arch)| simulated_bottle_tag(os.as_deref(), arch.as_deref()))
        .collect()
}

/// This host's OS as a brew simulation symbol (`linux`, a
/// macOS release name, or generic `macos`).
#[must_use]
pub fn current_os_symbol() -> String {
    let host = Machine::this();
    match host.os {
        Os::Linux => "linux".to_string(),
        Os::Macos => host
            .macos_version()
            .and_then(release)
            .map(|(_, name, _)| name.to_string())
            .unwrap_or_else(|| "macos".to_string()),
        Os::Other => "macos".to_string(),
    }
}

/// Whether this process runs translated under Rosetta 2
/// (`sysctl.proc_translated`), brew's `in_rosetta2?`.
#[must_use]
pub fn in_rosetta2() -> bool {
    std::env::consts::ARCH == "x86_64"
        && super::fetch::run_capture("/usr/sbin/sysctl", &["-n", "sysctl.proc_translated"])
            .ok()
            .filter(|o| o.status.success())
            .is_some_and(|o| String::from_utf8_lossy(&o.stdout).trim() == "1")
}

/// This host's arch as a brew simulation symbol.
#[must_use]
pub fn current_arch_symbol() -> &'static str {
    if Machine::this().arch() == "x86_64" {
        "intel"
    } else {
        "arm"
    }
}

const ARCHES: [&str; 2] = ["arm64", "x86_64"];

/// The release macOS `version` is: by its major from 11 on, and of the 10.x
/// line, whose releases are its minors, only 10.15.
fn release(version: &str) -> Option<&'static (u64, &'static str, &'static str)> {
    let mut parts = version.split('.');
    let major: u64 = parts.next().filter(|s| !s.starts_with('+'))?.parse().ok()?;
    if major == 10 && parts.next() != Some("15") {
        return None;
    }
    RELEASES.iter().find(|(known, ..)| *known == major)
}

/// The first Xcode released for macOS `macos`; past the table, the yearly
/// Xcode that shares its number.
#[must_use]
pub fn release_xcode(macos: &str) -> Option<String> {
    if let Some((.., xcode)) = release(macos) {
        return Some((*xcode).to_string());
    }
    let major: u64 = macos
        .split('.')
        .next()
        .filter(|s| !s.starts_with('+'))?
        .parse()
        .ok()?;
    (major > RELEASES[0].0).then(|| format!("{major}.0"))
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Os {
    Macos,
    Linux,
    Other,
}

/// What admission reads about the machine an install would land on.
#[derive(Debug)]
pub struct Machine {
    os: Os,
    /// Homebrew's spelling: `arm64`, `x86_64`.
    arch: String,
    macos: OnceLock<Option<String>>,
    xcode: OnceLock<Option<String>>,
    clt: OnceLock<bool>,
}

impl Machine {
    /// The running machine.
    #[must_use]
    pub fn this() -> &'static Machine {
        static THIS: OnceLock<Machine> = OnceLock::new();
        THIS.get_or_init(|| Machine {
            os: if cfg!(target_os = "macos") {
                Os::Macos
            } else if cfg!(target_os = "linux") {
                Os::Linux
            } else {
                Os::Other
            },
            arch: match std::env::consts::ARCH {
                "aarch64" => "arm64",
                other => other,
            }
            .to_string(),
            macos: OnceLock::new(),
            xcode: OnceLock::new(),
            clt: OnceLock::new(),
        })
    }

    /// A modelled Mac with `arch`, macOS and Xcode versions, and CLT presence.
    #[must_use]
    pub fn mac(arch: &str, macos: &str, xcode: Option<&str>, clt: bool) -> Machine {
        Machine {
            os: Os::Macos,
            arch: arch.to_string(),
            macos: OnceLock::from(Some(macos.to_string())),
            xcode: OnceLock::from(xcode.map(str::to_owned)),
            clt: OnceLock::from(clt),
        }
    }

    /// [`Machine::mac`] from user-supplied architecture, macOS, and Xcode strings.
    pub fn modelled(
        arch: &str,
        macos: &str,
        xcode: Option<&str>,
        clt: bool,
    ) -> Result<Machine, String> {
        if !ARCHES.contains(&arch) {
            return Err(format!(
                "architecture {arch:?} is not one of {}",
                ARCHES.join(", ")
            ));
        }
        for (what, version) in [("macOS", Some(macos)), ("Xcode", xcode)] {
            if let Some(version) = version
                && !super::formula::numeric_macos_version(version)
            {
                return Err(format!("{what} version {version:?} is not numeric"));
            }
        }
        Ok(Machine::mac(arch, macos, xcode, clt))
    }

    /// A machine simulated from brew `--os`/`--arch` values
    /// (`SimulateSystem`): `os` is a macOS release symbol,
    /// `macos` (generic, no release), or `linux`; `arch` is
    /// `arm`/`intel`. Each defaults to this host's own.
    #[must_use]
    pub fn simulated(os: Option<&str>, arch: Option<&str>) -> Machine {
        let host = Machine::this();
        let arch = match arch {
            Some("arm") => "arm64".to_string(),
            Some("intel") => "x86_64".to_string(),
            _ => host.arch.clone(),
        };
        let blank = |os: Os| Machine {
            os,
            arch: arch.clone(),
            macos: OnceLock::from(None),
            xcode: OnceLock::from(None),
            clt: OnceLock::from(false),
        };
        match os {
            Some("linux") => blank(Os::Linux),
            Some("macos") => blank(Os::Macos),
            Some(symbol) => match RELEASES.iter().find(|(_, name, _)| *name == symbol) {
                Some((major, ..)) => Machine::mac(&arch, &major.to_string(), None, true),
                None => blank(Os::Macos),
            },
            None => match host.os {
                Os::Linux => blank(Os::Linux),
                Os::Macos => match host.macos_version() {
                    Some(version) => Machine::mac(&arch, version, None, true),
                    None => blank(Os::Macos),
                },
                Os::Other => blank(Os::Other),
            },
        }
    }

    /// The newest Mac abrew can name (arm64 on the newest release with Xcode and CLT).
    #[must_use]
    pub fn newest_mac() -> Machine {
        let version = format!("{}.0", RELEASES[0].0);
        Machine::mac("arm64", &version, release_xcode(&version).as_deref(), true)
    }

    /// Homebrew's architecture name (`arm64`, `x86_64`).
    #[must_use]
    pub fn arch(&self) -> &str {
        &self.arch
    }

    /// The macOS version (`27.0.1`); `None` off macOS or when the probe fails.
    #[must_use]
    pub fn macos_version(&self) -> Option<&str> {
        self.macos
            .get_or_init(|| {
                if self.os == Os::Macos {
                    probe_macos()
                } else {
                    None
                }
            })
            .as_deref()
    }

    /// The macOS major version (`27`).
    #[must_use]
    pub fn macos_major(&self) -> Option<u64> {
        self.macos_version()?
            .split('.')
            .next()
            .filter(|s| !s.starts_with('+'))?
            .parse()
            .ok()
    }

    /// The installed Xcode's version, for `depends_on xcode:` floors.
    #[must_use]
    pub fn xcode_version(&self) -> Option<&str> {
        self.xcode
            .get_or_init(|| {
                if self.os == Os::Macos {
                    probe_xcode()
                } else {
                    None
                }
            })
            .as_deref()
    }

    /// Whether the Command Line Tools are installed (`pour_bottle_only_if: :clt_installed`).
    #[must_use]
    pub fn has_clt(&self) -> bool {
        *self.clt.get_or_init(|| {
            self.os == Os::Macos
                && std::path::Path::new("/Library/Developer/CommandLineTools").is_dir()
        })
    }

    /// Compatible bottle tags, best first.
    #[must_use]
    pub fn bottle_tags(&self) -> Vec<String> {
        if !ARCHES.contains(&self.arch()) {
            return Vec::new();
        }
        match self.os {
            Os::Linux => vec![format!("{}_linux", self.arch)],
            Os::Macos => {
                let Some(major) = self.macos_major() else {
                    return Vec::new();
                };
                let prefix = if self.arch == "arm64" { "arm64_" } else { "" };
                RELEASES
                    .iter()
                    .filter(|(release, ..)| (OLDEST_BOTTLE_MAJOR..=major).contains(release))
                    .map(|(_, name, _)| format!("{prefix}{name}"))
                    .collect()
            }
            Os::Other => Vec::new(),
        }
    }

    /// The `variations` keys that describe exactly this system, in Homebrew's order.
    #[must_use]
    pub fn variation_keys(&self) -> Option<Vec<String>> {
        match self.os {
            Os::Linux => Some(vec![format!("{}_linux", self.arch)]),
            Os::Macos => {
                let (_, name, _) = release(self.macos_version()?)?;
                Some(if self.arch == "arm64" {
                    vec![format!("arm64_{name}")]
                } else {
                    vec![format!("{}_{name}", self.arch), (*name).to_string()]
                })
            }
            Os::Other => None,
        }
    }

    /// One line naming what admission was judged against.
    #[must_use]
    pub fn describe(&self) -> String {
        let os = match self.os {
            Os::Macos => format!("macOS {}", self.macos_version().unwrap_or("unknown")),
            Os::Linux => "Linux".to_string(),
            Os::Other => std::env::consts::OS.to_string(),
        };
        let xcode = self
            .xcode_version()
            .map_or_else(|| "no Xcode".to_string(), |v| format!("Xcode {v}"));
        let clt = if self.has_clt() {
            "Command Line Tools"
        } else {
            "no Command Line Tools"
        };
        format!("{}, {os}, {xcode}, {clt}", self.arch)
    }
}

/// The macOS major version (first component of `sw_vers`).
pub(super) fn macos_major() -> Option<String> {
    probe_macos()?.split('.').next().map(str::to_string)
}

fn probe_macos() -> Option<String> {
    let out = super::fetch::run_capture("/usr/bin/sw_vers", &["-productVersion"]).ok()?;
    if !out.status.success() {
        return None;
    }
    let version = String::from_utf8(out.stdout).ok()?.trim().to_string();
    super::formula::numeric_macos_version(&version).then_some(version)
}

fn probe_xcode() -> Option<String> {
    DeveloperDir::this().bundle_version()
}

/// The developer directory `xcode-select`'s shims would run a tool from, read without running one.
pub(super) struct DeveloperDir {
    /// `DEVELOPER_DIR`, which the shims honour before the selection.
    developer_dir_env: Option<std::ffi::OsString>,
    /// The developer-dir link `xcode-select --switch` writes.
    select_link: std::path::PathBuf,
    /// Where the shims look when nothing is selected, in their order: the
    /// first that is a directory.
    defaults: Vec<std::path::PathBuf>,
}

impl DeveloperDir {
    /// This Mac's developer directory (`DEVELOPER_DIR`, `xcode_select_link`, Xcode, or CLT).
    pub(super) fn this() -> DeveloperDir {
        DeveloperDir {
            developer_dir_env: std::env::var_os("DEVELOPER_DIR"),
            select_link: "/var/db/xcode_select_link".into(),
            defaults: vec![
                "/Applications/Xcode.app/Contents/Developer".into(),
                "/Library/Developer/CommandLineTools".into(),
            ],
        }
    }

    fn active(&self) -> Option<std::path::PathBuf> {
        let mut dir = match &self.developer_dir_env {
            // A shell opened before the tools arrived can still name aterm's shim directory,
            // which holds only `xcrun` and says nothing of this Mac's tools.
            Some(env)
                if !env.is_empty()
                    && !aterm_types::clt_shim::is_shim_developer_dir(std::path::Path::new(env)) =>
            {
                std::path::PathBuf::from(env)
            }
            _ => match std::fs::read_link(&self.select_link) {
                Ok(target) => target,
                Err(_) => self.defaults.iter().find(|d| d.is_dir())?.clone(),
            },
        };
        if dir.extension().is_some_and(|e| e == "app") {
            dir = dir.join("Contents/Developer");
        }
        Some(dir)
    }

    /// The active macOS SDK directory (`xcrun --show-sdk-path`
    /// without running it), as brew names it.
    pub(super) fn sdk_path(&self) -> Option<std::path::PathBuf> {
        let sdks = self.active()?.join("SDKs");
        // Brew names the versioned SDK (`MacOSX27.sdk`), not
        // the resolved `MacOSX27.0.sdk` directory.
        if let Some(major) = macos_major() {
            let versioned = sdks.join(format!("MacOSX{major}.sdk"));
            if versioned.is_dir() {
                return Some(versioned);
            }
        }
        let generic = sdks.join("MacOSX.sdk");
        if generic.is_dir() {
            return Some(generic);
        }
        let mut dirs: Vec<_> = std::fs::read_dir(&sdks)
            .ok()?
            .flatten()
            .map(|e| e.path())
            .filter(|p| p.is_dir())
            .collect();
        dirs.sort();
        dirs.pop()
    }

    /// The developer tool `name` a shim would run.
    #[cfg_attr(not(target_os = "macos"), allow(dead_code))]
    pub(super) fn tool(&self, name: &str) -> Option<std::path::PathBuf> {
        let dir = self.active()?;
        ["usr/bin", "Toolchains/XcodeDefault.xctoolchain/usr/bin"]
            .iter()
            .map(|bin| dir.join(bin).join(name))
            .find(|tool| crate::toolchain::is_executable_file(tool))
    }

    fn bundle_version(&self) -> Option<String> {
        let dir = self.active()?;
        let contents = dir.parent()?;
        if dir.file_name()? != "Developer" || contents.file_name()? != "Contents" {
            return None;
        }
        let plist = crate::metadata_io::read_bounded_regular_utf8(
            &contents.join("version.plist"),
            64 * 1024,
        )
        .ok()?;
        let version = crate::doctor::plist_short_version(&plist)?;
        super::formula::numeric_macos_version(&version).then_some(version)
    }
}

#[cfg(test)]
#[path = "machine_tests.rs"]
mod tests;
