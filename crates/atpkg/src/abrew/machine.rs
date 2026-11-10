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
mod tests {
    use super::*;

    #[test]
    fn bottle_tags_never_name_a_newer_os_or_another_architecture() {
        let tags = Machine::mac("arm64", "14.6", None, false).bottle_tags();
        assert_eq!(
            tags,
            [
                "arm64_sonoma",
                "arm64_ventura",
                "arm64_monterey",
                "arm64_big_sur"
            ]
        );
        assert_eq!(
            Machine::mac("x86_64", "14.6", None, false).bottle_tags()[0],
            "sonoma"
        );
        assert!(
            Machine::mac("arm64", "10.15", None, false)
                .bottle_tags()
                .is_empty()
        );
        assert!(
            Machine::mac("mips", "26.0", None, false)
                .bottle_tags()
                .is_empty()
        );
        assert!(
            Machine::mac("arm64", "next", None, false)
                .bottle_tags()
                .is_empty()
        );
    }

    #[test]
    fn bottle_tag_symbols_follow_brew_tag() {
        assert_eq!(bottle_tag_symbol("sequoia", "intel"), "sequoia");
        assert_eq!(bottle_tag_symbol("sequoia", "arm"), "arm64_sequoia");
        assert_eq!(bottle_tag_symbol("linux", "intel"), "x86_64_linux");
        assert_eq!(bottle_tag_symbol("macos", "arm"), "arm64_macos");
        assert_eq!(bottle_tag_symbol("all", "all"), "all");
        assert_eq!(
            bottle_tag_from_symbol("bogus").as_deref(),
            Some("x86_64_bogus")
        );
        assert_eq!(
            bottle_tag_from_symbol("arm64_tahoe").as_deref(),
            Some("arm64_tahoe")
        );
        assert_eq!(bottle_tag_from_symbol("tahoe").as_deref(), Some("tahoe"));
        assert_eq!(bottle_tag_from_symbol("all").as_deref(), Some("all"));
        assert_eq!(bottle_tag_from_symbol("foo-bar"), None);
        assert_eq!(
            simulated_bottle_tag(Some("linux"), Some("intel")),
            "x86_64_linux"
        );
        assert_eq!(bottle_tag_combinations(Some("all"), Some("intel")).len(), 8);
        assert_eq!(bottle_tag_combinations(None, Some("all")).len(), 2);
    }

    #[test]
    fn simulated_machines_carry_brew_variation_keys() {
        assert_eq!(
            Machine::simulated(Some("linux"), Some("arm")).variation_keys(),
            Some(vec!["arm64_linux".to_string()])
        );
        assert_eq!(
            Machine::simulated(Some("linux"), Some("intel")).variation_keys(),
            Some(vec!["x86_64_linux".to_string()])
        );
        assert_eq!(
            Machine::simulated(Some("sequoia"), Some("intel")).variation_keys(),
            Some(vec!["x86_64_sequoia".to_string(), "sequoia".to_string()])
        );
        // Generic macos has no release, hence no variation keys
        // and the base API record applies.
        assert_eq!(
            Machine::simulated(Some("macos"), Some("arm")).variation_keys(),
            None
        );
    }

    #[test]
    fn variation_keys_name_exactly_this_system() {
        assert_eq!(
            Machine::mac("arm64", "27.0.1", None, true).variation_keys(),
            Some(vec!["arm64_golden_gate".to_string()])
        );
        assert_eq!(
            Machine::mac("x86_64", "15.2", None, true).variation_keys(),
            Some(vec!["x86_64_sequoia".to_string(), "sequoia".to_string()])
        );
        assert_eq!(
            Machine::mac("arm64", "40.0", None, true).variation_keys(),
            None
        );
        assert_eq!(
            Machine::mac("x86_64", "10.15.7", None, true).variation_keys(),
            Some(vec!["x86_64_catalina".to_string(), "catalina".to_string()])
        );
        assert_eq!(
            Machine::mac("x86_64", "10.14", None, true).variation_keys(),
            None,
            "Mojave is not Catalina"
        );
    }

    #[test]
    fn a_modelled_machine_is_one_the_probes_could_report() {
        let intel = Machine::modelled("x86_64", "15.5", None, false).unwrap();
        assert_eq!(
            intel.describe(),
            "x86_64, macOS 15.5, no Xcode, no Command Line Tools"
        );
        assert!(Machine::modelled("arm64", "40.0", Some("40.1"), true).is_ok());
        for (arch, macos, xcode) in [
            ("aarch64", "27.0", None),
            ("mips", "27.0", None),
            ("arm64", "garbage", None),
            ("arm64", "", None),
            ("arm64", "27.0", Some("26.x")),
        ] {
            assert!(
                Machine::modelled(arch, macos, xcode, true).is_err(),
                "{arch} {macos} {xcode:?}"
            );
        }
    }

    /// A version plist the way an Xcode bundle carries it.
    #[cfg(unix)]
    fn version_plist(version: &str) -> String {
        format!(
            "<?xml version=\"1.0\" encoding=\"UTF-8\"?>\n<plist version=\"1.0\">\n<dict>\n\
             \t<key>BuildVersion</key>\n\t<string>7</string>\n\
             \t<key>CFBundleShortVersionString</key>\n\t<string>{version}</string>\n\
             \t<key>CFBundleVersion</key>\n\t<string>24000</string>\n</dict>\n</plist>\n"
        )
    }

    /// aterm's shim directory in `DEVELOPER_DIR` reads as no choice at all.
    /// NEGATIVE CONTROL: any other directory there wins over the selection.
    #[test]
    fn the_shims_developer_dir_is_no_choice_of_tools() {
        let tmp = aterm_tempfile::tempdir().unwrap();
        let selected = tmp.path().join("selected");
        std::fs::create_dir_all(&selected).unwrap();
        let active = |env: &str| {
            DeveloperDir {
                developer_dir_env: Some(env.into()),
                select_link: tmp.path().join("no-link"),
                defaults: vec![selected.clone()],
            }
            .active()
        };
        assert_eq!(active("/s/clt-shim/developer"), Some(selected.clone()));
        assert_eq!(active("/s/own"), Some("/s/own".into()));
    }

    /// The Xcode version is read off the selected developer directory's
    /// bundle, no process run: the link `xcode-select` writes, overridden by
    /// `DEVELOPER_DIR` (a bundle or its `Contents/Developer`), else the
    /// default bundle. NEGATIVE CONTROL: the Command Line Tools selected, a
    /// missing or non-numeric plist and an unset selection with no default
    /// Xcode all read as no Xcode, and so does a developer directory that is
    /// not a bundle's `Contents/Developer` even with a version plist beside
    /// it (a folder named otherwise, or a `Developer` outside `Contents`).
    #[cfg(unix)]
    #[test]
    fn the_xcode_version_is_read_from_the_selected_bundle() {
        let tmp = aterm_tempfile::tempdir().unwrap();
        let root = tmp.path();
        let xcode = root.join("Xcode-26.1.app");
        std::fs::create_dir_all(xcode.join("Contents/Developer")).unwrap();
        std::fs::write(xcode.join("Contents/version.plist"), version_plist("26.1")).unwrap();
        let beta = root.join("Xcode-beta.app");
        std::fs::create_dir_all(beta.join("Contents/Developer")).unwrap();
        std::fs::write(beta.join("Contents/version.plist"), version_plist("27.0")).unwrap();
        let clt = root.join("CommandLineTools");
        std::fs::create_dir_all(&clt).unwrap();
        let link = root.join("xcode_select_link");
        let absent = root.join("absent/Contents/Developer");
        let probe = |env: Option<&std::path::Path>, default: &std::path::Path| {
            DeveloperDir {
                developer_dir_env: env.map(|p| p.as_os_str().to_owned()),
                select_link: link.clone(),
                defaults: vec![default.to_path_buf()],
            }
            .bundle_version()
        };
        std::os::unix::fs::symlink(xcode.join("Contents/Developer"), &link).unwrap();
        assert_eq!(probe(None, &absent).as_deref(), Some("26.1"));
        assert_eq!(
            probe(Some(&beta.join("Contents/Developer")), &absent).as_deref(),
            Some("27.0")
        );
        assert_eq!(probe(Some(&beta), &absent).as_deref(), Some("27.0"));
        assert_eq!(probe(Some(&clt), &absent), None);
        let tools = root.join("Library/Developer/CommandLineTools");
        std::fs::create_dir_all(&tools).unwrap();
        std::fs::write(
            root.join("Library/Developer/version.plist"),
            version_plist("16.4"),
        )
        .unwrap();
        let loose = root.join("Odd.app/Resources/Developer");
        std::fs::create_dir_all(&loose).unwrap();
        std::fs::write(
            root.join("Odd.app/Resources/version.plist"),
            version_plist("16.4"),
        )
        .unwrap();
        for not_a_bundle in [&tools, &loose] {
            assert_eq!(
                probe(Some(not_a_bundle), &absent),
                None,
                "{}",
                not_a_bundle.display()
            );
        }
        std::fs::remove_file(&link).unwrap();
        std::os::unix::fs::symlink(&clt, &link).unwrap();
        assert_eq!(probe(None, &xcode.join("Contents/Developer")), None);
        std::fs::remove_file(&link).unwrap();
        assert_eq!(
            probe(None, &xcode.join("Contents/Developer")).as_deref(),
            Some("26.1"),
            "nothing selected: the default bundle"
        );
        assert_eq!(probe(None, &absent), None);
        std::fs::write(xcode.join("Contents/version.plist"), version_plist("26.x")).unwrap();
        assert_eq!(probe(None, &xcode.join("Contents/Developer")), None);
        std::fs::remove_file(xcode.join("Contents/version.plist")).unwrap();
        assert_eq!(probe(None, &xcode.join("Contents/Developer")), None);
    }

    /// On this Mac the probe agrees with `xcodebuild -version` for the
    /// selected developer directory. An Xcode's own `xcodebuild`, run from
    /// inside its bundle, names the version the probe reads. With the Command
    /// Line Tools selected, the `/usr/bin` shim refuses ("requires Xcode")
    /// and the probe reads no Xcode: the shim runs only once that directory
    /// holds the tools (its `clang`), so it cannot raise Apple's install
    /// dialog. With no developer tools at all the comparison ends COULD NOT
    /// RUN, never a pass.
    #[cfg(target_os = "macos")]
    #[test]
    fn the_probe_agrees_with_the_selected_xcode() {
        let probe = DeveloperDir {
            developer_dir_env: None,
            ..DeveloperDir::this()
        };
        let read = probe.bundle_version();
        let Some(developer) = probe.active() else {
            aterm_uds::host::could_not_run_without(&["xcodebuild"]);
            return;
        };
        let own = developer.join("usr/bin/xcodebuild");
        let (xcodebuild, is_xcode) = if own.is_file() {
            (own, true)
        } else if probe.tool("clang").is_some() {
            (std::path::PathBuf::from("/usr/bin/xcodebuild"), false)
        } else {
            aterm_uds::host::could_not_run_without(&["xcodebuild"]);
            return;
        };
        let out = aterm_uds::spawnfd::command(&xcodebuild)
            .arg("-version")
            .env_remove("DEVELOPER_DIR")
            .output()
            .expect("xcodebuild runs");
        let said = format!(
            "{}{}",
            String::from_utf8_lossy(&out.stdout),
            String::from_utf8_lossy(&out.stderr)
        );
        if !is_xcode {
            assert!(
                !out.status.success(),
                "{} names an Xcode the probe did not: {said}",
                developer.display()
            );
            assert_eq!(read, None, "{} is no Xcode", developer.display());
            return;
        }
        assert!(out.status.success(), "{said}");
        let first = said.lines().next().unwrap_or_default().to_owned();
        assert_eq!(first.strip_prefix("Xcode "), read.as_deref(), "{first}");
    }

    /// A developer tool is the selected directory's own, found without
    /// running the `/usr/bin` shim: the Command Line Tools' `usr/bin`, an
    /// Xcode's default toolchain (`DEVELOPER_DIR` naming the bundle), and
    /// with nothing selected the first default there. NEGATIVE CONTROL: a
    /// selection whose directory holds no such tool, one whose tool is not
    /// executable, a `DEVELOPER_DIR` over a link that does hold one, and no
    /// selection and no default each find none, where the shim would raise
    /// Apple's install dialog.
    #[cfg(unix)]
    #[test]
    fn a_developer_tool_is_found_only_in_the_selected_directory() {
        use std::os::unix::fs::PermissionsExt as _;
        let tmp = aterm_tempfile::tempdir().unwrap();
        let root = tmp.path();
        let put = |path: &std::path::Path, mode: u32| {
            std::fs::create_dir_all(path.parent().unwrap()).unwrap();
            std::fs::write(path, b"#!/bin/sh\n").unwrap();
            std::fs::set_permissions(path, std::fs::Permissions::from_mode(mode)).unwrap();
        };
        let clt = root.join("CommandLineTools");
        put(&clt.join("usr/bin/install_name_tool"), 0o755);
        let xcode = root.join("Xcode.app");
        let in_xcode = xcode.join(
            "Contents/Developer/Toolchains/XcodeDefault.xctoolchain/usr/bin/install_name_tool",
        );
        put(&in_xcode, 0o755);
        let bare = root.join("Bare");
        std::fs::create_dir_all(bare.join("usr/bin")).unwrap();
        let unusable = root.join("Unusable");
        put(&unusable.join("usr/bin/install_name_tool"), 0o644);
        let link = root.join("xcode_select_link");
        let dir = |env: Option<&std::path::Path>, defaults: &[&std::path::Path]| DeveloperDir {
            developer_dir_env: env.map(|p| p.as_os_str().to_owned()),
            select_link: link.clone(),
            defaults: defaults.iter().map(|d| d.to_path_buf()).collect(),
        };
        let tool = |d: DeveloperDir| d.tool("install_name_tool");
        std::os::unix::fs::symlink(&clt, &link).unwrap();
        assert_eq!(
            tool(dir(None, &[])),
            Some(clt.join("usr/bin/install_name_tool"))
        );
        assert_eq!(tool(dir(Some(&xcode), &[])), Some(in_xcode.clone()));
        assert_eq!(tool(dir(Some(&bare), &[])), None, "DEVELOPER_DIR wins");
        std::fs::remove_file(&link).unwrap();
        std::os::unix::fs::symlink(&bare, &link).unwrap();
        assert_eq!(tool(dir(None, &[&clt])), None, "the selection wins");
        std::fs::remove_file(&link).unwrap();
        std::os::unix::fs::symlink(&unusable, &link).unwrap();
        assert_eq!(tool(dir(None, &[])), None, "not executable");
        std::fs::remove_file(&link).unwrap();
        let absent = root.join("absent");
        assert_eq!(
            tool(dir(None, &[&absent, &clt])),
            Some(clt.join("usr/bin/install_name_tool")),
            "nothing selected: the first default there"
        );
        assert_eq!(tool(dir(None, &[&absent])), None, "no developer tools");
    }

    #[test]
    fn the_newest_mac_is_the_api_default_system() {
        let newest = Machine::newest_mac();
        assert_eq!(newest.arch(), "arm64");
        assert_eq!(newest.macos_major(), Some(RELEASES[0].0));
        assert_eq!(newest.xcode_version(), newest.macos_version());
        assert!(newest.has_clt());
        assert_eq!(
            newest.describe(),
            "arm64, macOS 27.0, Xcode 27.0, Command Line Tools"
        );
    }

    #[test]
    fn a_release_runs_the_xcode_released_with_it() {
        for (macos, xcode) in [
            ("27.0", Some("27.0")),
            ("26.1", Some("26.0")),
            ("15.5", Some("16.0")),
            ("14.6", Some("15.0")),
            ("13.5", Some("14.1")),
            ("12.7", Some("13.1")),
            ("11.7", Some("12.2")),
            ("10.15.7", Some("11.0")),
            ("10.14", None),
            ("10", None),
            ("30.0", Some("30.0")),
            ("20.0", None),
            ("9.3", None),
            ("+27.0", None),
            ("+30.0", None),
            ("garbage", None),
        ] {
            assert_eq!(release_xcode(macos).as_deref(), xcode, "{macos}");
        }
    }
}
