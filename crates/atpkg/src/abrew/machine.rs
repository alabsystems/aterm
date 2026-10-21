// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates
//! The machine abrew's admission judges a recipe against (design §3): its
//! operating system and architecture, its macOS and Xcode versions and
//! whether the Command Line Tools are installed. [`Machine::this`] is the
//! running machine, each fact probed once per process when a recipe first
//! asks for it; [`Machine::mac`] is a modelled one, which the admission
//! census ([`super::census`]) and tests judge against.

use std::sync::OnceLock;

/// macOS releases abrew can name, newest first: (major version, Homebrew's
/// name for it in bottle tags and `variations` keys).
const RELEASES: [(u64, &str); 8] = [
    (27, "golden_gate"),
    (26, "tahoe"),
    (15, "sequoia"),
    (14, "sonoma"),
    (13, "ventura"),
    (12, "monterey"),
    (11, "big_sur"),
    (10, "catalina"),
];

/// The oldest release a bottle tag is selected for: no catalina bottle pours.
const OLDEST_BOTTLE_MAJOR: u64 = 11;

/// The architectures Homebrew builds bottles for, in its spelling.
const ARCHES: [&str; 2] = ["arm64", "x86_64"];

/// Homebrew's name for a macOS major version.
#[must_use]
pub fn release_name(major: u64) -> Option<&'static str> {
    RELEASES
        .iter()
        .find(|(known, _)| *known == major)
        .map(|(_, name)| *name)
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
    /// The running machine. Its versions are probed (`sw_vers`,
    /// `xcodebuild -version`, the CommandLineTools folder) at most once per
    /// process, the first time admission needs each.
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

    /// A modelled Mac: `arch` in Homebrew's spelling, the macOS and Xcode
    /// versions it runs (`None`: no Xcode) and whether the Command Line
    /// Tools are installed. Nothing is probed.
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

    /// [`Machine::mac`] from facts a person typed, refused unless this
    /// machine's own probes could report them: an architecture Homebrew
    /// builds bottles for, and numeric macOS and Xcode versions.
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

    /// The newest Mac abrew can name: arm64 on the newest release in its
    /// table, with that release's Xcode and the Command Line Tools — the
    /// API's default system, whose top-level fields need no variation.
    #[must_use]
    pub fn newest_mac() -> Machine {
        let version = format!("{}.0", RELEASES[0].0);
        Machine::mac("arm64", &version, Some(&version), true)
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
        self.macos_version()?.split('.').next()?.parse().ok()
    }

    /// The installed Xcode's version, for `depends_on xcode:` floors. `None`
    /// off macOS, with only the Command Line Tools selected, or on any probe
    /// failure: an unmet requirement then delegates like the macOS floors do.
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

    /// Whether the Command Line Tools are installed (`pour_bottle_only_if:
    /// :clt_installed`).
    #[must_use]
    pub fn has_clt(&self) -> bool {
        *self.clt.get_or_init(|| {
            self.os == Os::Macos
                && std::path::Path::new("/Library/Developer/CommandLineTools").is_dir()
        })
    }

    /// Compatible bottle tags, best first. Linux names its architecture; a
    /// Mac every release from its own back to big_sur (Intel tags carry no
    /// architecture prefix). An unknown macOS version admits only a
    /// formula's `all` fallback: it never guesses a newer OS requirement.
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
                    .filter(|(release, _)| (OLDEST_BOTTLE_MAJOR..=major).contains(release))
                    .map(|(_, name)| format!("{prefix}{name}"))
                    .collect()
            }
            Os::Other => Vec::new(),
        }
    }

    /// The `variations` keys that describe exactly this system, in
    /// Homebrew's order: `<arch>_linux`; on a Mac `arm64_<release>`, or for
    /// Intel `x86_64_<release>` then the bare release. `None` when the
    /// system cannot be named (an unknown macOS release, another OS).
    #[must_use]
    pub fn variation_keys(&self) -> Option<Vec<String>> {
        match self.os {
            Os::Linux => Some(vec![format!("{}_linux", self.arch)]),
            Os::Macos => {
                let release = release_name(self.macos_major()?)?;
                Some(if self.arch == "arm64" {
                    vec![format!("arm64_{release}")]
                } else {
                    vec![format!("{}_{release}", self.arch), release.to_string()]
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

/// `sw_vers -productVersion`, when it is a plain numeric version.
fn probe_macos() -> Option<String> {
    let out = super::fetch::run_capture("/usr/bin/sw_vers", &["-productVersion"]).ok()?;
    if !out.status.success() {
        return None;
    }
    let version = String::from_utf8(out.stdout).ok()?.trim().to_string();
    super::formula::numeric_macos_version(&version).then_some(version)
}

/// `xcodebuild -version`'s first line (`Xcode 26.1`).
fn probe_xcode() -> Option<String> {
    let out = super::fetch::run_capture("/usr/bin/xcodebuild", &["-version"]).ok()?;
    if !out.status.success() {
        return None;
    }
    let text = String::from_utf8(out.stdout).ok()?;
    let version = text
        .lines()
        .next()?
        .strip_prefix("Xcode ")?
        .split_whitespace()
        .next()?
        .to_string();
    super::formula::numeric_macos_version(&version).then_some(version)
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
        // An unknown version is no version: only `all` can pour.
        assert!(
            Machine::mac("arm64", "next", None, false)
                .bottle_tags()
                .is_empty()
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
}
