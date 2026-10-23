// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Provisioned tool rows for `verify provision`.

use aterm_uds::host::{Tool, When};

/// Origin verification anchor for an unpacked archive tree.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Anchor {
    /// Archive SHA-256 only (upstream signs nothing).
    Sha256,
    /// Archive SHA-256 plus Developer ID codesign verification of every Mach-O for `team`.
    DeveloperId {
        /// Apple team ID every Mach-O must carry.
        team: &'static str,
    },
}

/// One upstream archive a row fetches for one host.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Archive {
    /// `<os>-<arch>` (`macos-aarch64`), or `source` for any host.
    pub platform: &'static str,
    /// Upstream URL template (`{v}` is replaced by the row's version).
    pub url: &'static str,
    /// Lowercase-hex SHA-256.
    pub sha256: &'static str,
    /// Size in bytes.
    pub bytes: u64,
}

impl Archive {
    /// [`Archive::url`] at `version`.
    #[must_use]
    pub fn url_at(&self, version: &str) -> String {
        self.url.replace("{v}", version)
    }
}

/// Browser engine pinned by `crate::browser` and installed into the browser cache.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Engine {
    /// chrome-headless-shell.
    Chromium,
    /// Playwright's Firefox build.
    Firefox,
    /// Playwright's WebKit build.
    Webkit,
}

/// How a row is installed.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Install {
    /// Upstream release archive verified against `anchor` and unpacked with `strip` leading components dropped.
    Unpack {
        /// Archives per host.
        archives: &'static [Archive],
        /// Leading path components dropped on extraction.
        strip: u32,
        /// Origin verification anchor.
        anchor: Anchor,
        /// Required files in the unpacked tree (e.g. license notices).
        keep: &'static [&'static str],
    },
    /// Upstream source tarball built with Trust (`targo --unverified build --release --locked --no-default-features`).
    Build {
        /// Source tarball.
        source: Archive,
        /// Cargo features enabled past `--no-default-features`.
        features: &'static [&'static str],
    },
    /// Crate CLI built by `targo --unverified install --locked` at its lockfile pin.
    Cargo,
    /// Browser engine pinned by the browser lane.
    Browser(Engine),
    /// Pinned `rustup` stable toolchain with cross/wasm32 targets (`STOCK EXCEPTION`).
    Rustup {
        /// Pinned `rustup` release version.
        version: &'static str,
        /// `rustup-init` binaries per host.
        init: &'static [Archive],
    },
}

/// One row of `verify provision`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Row {
    /// CLI row name.
    pub name: &'static str,
    /// Corresponding [`aterm_uds::host::TOOLS`] row name, if any.
    pub tool: Option<&'static str>,
    /// Installation strategy.
    pub install: Install,
}

impl Row {
    /// The [`aterm_uds::host::TOOLS`] row it provides.
    #[must_use]
    pub fn tool_row(&self) -> Option<&'static Tool> {
        self.tool.map(aterm_uds::host::tool)
    }

    /// The run that provides it: its tool row's, else every run.
    #[must_use]
    pub fn when(&self) -> When {
        match self.tool_row().map(|t| t.provider) {
            Some(aterm_uds::host::Provider::Workspace { when }) => when,
            _ => When::Always,
        }
    }

    /// Version pinned by its tool row (`None` for browsers and `rustup`).
    #[must_use]
    pub fn version(&self) -> Option<&'static str> {
        self.tool_row().and_then(|t| t.pin).map(|p| p.version)
    }

    /// Archive fetched for `platform` (`<os>-<arch>`).
    #[must_use]
    pub fn archive_for(&self, platform: &str) -> Option<Archive> {
        match self.install {
            Install::Unpack { archives, .. } | Install::Rustup { init: archives, .. } => {
                archives.iter().find(|a| a.platform == platform).copied()
            }
            Install::Build { source, .. } => self
                .tool_row()
                .and_then(|t| t.pin)
                .is_some_and(|p| p.pinned_for(platform))
                .then_some(source),
            _ => None,
        }
    }
}

/// Microsoft's Apple Developer ID team signing the PowerShell archives.
pub const MICROSOFT_TEAM: &str = "UBF8T346G9";

/// Every row, in the order a run provides them.
pub const ROWS: [Row; 9] = [
    Row {
        name: "rustup",
        tool: Some("rustup"),
        install: Install::Rustup {
            version: "1.29.2",
            init: &[
                Archive {
                    platform: "macos-aarch64",
                    url: "https://static.rust-lang.org/rustup/archive/{v}/aarch64-apple-darwin/rustup-init",
                    sha256: "f7c7e11eb2e07f006e532a8aaac77b32f4527d305ac386c0c633ca6133d35ca9",
                    bytes: 11_321_792,
                },
                Archive {
                    platform: "macos-x86_64",
                    url: "https://static.rust-lang.org/rustup/archive/{v}/x86_64-apple-darwin/rustup-init",
                    sha256: "b9610730ddeef52765723847866fd1e3540b2bf7d5ab0c324aa143a5edf25ba0",
                    bytes: 12_911_072,
                },
                Archive {
                    platform: "linux-x86_64",
                    url: "https://static.rust-lang.org/rustup/archive/{v}/x86_64-unknown-linux-gnu/rustup-init",
                    sha256: "5be191e8c70a5007074254cb74d78dbfdd25cd5a61c27de7b8e5410122ec24f4",
                    bytes: 21_139_856,
                },
                Archive {
                    platform: "linux-aarch64",
                    url: "https://static.rust-lang.org/rustup/archive/{v}/aarch64-unknown-linux-gnu/rustup-init",
                    sha256: "a9c168dbea81a84671c6b7a5c1795cb0eb699020110d107ddfca427bd797e697",
                    bytes: 19_930_504,
                },
            ],
        },
    },
    Row {
        name: "fish",
        tool: Some("fish"),
        install: Install::Build {
            source: Archive {
                platform: "source",
                url: "https://github.com/fish-shell/fish-shell/releases/download/{v}/fish-{v}.tar.xz",
                sha256: "20998a25f73217ddcc19f499055fd587e9912d1ad6e7109120fbcf2871f0b98c",
                bytes: 2_628_460,
            },
            features: &[],
        },
    },
    Row {
        name: "pwsh",
        tool: Some("pwsh"),
        install: Install::Unpack {
            archives: &[
                Archive {
                    platform: "macos-aarch64",
                    url: "https://github.com/PowerShell/PowerShell/releases/download/v{v}/powershell-{v}-osx-arm64.tar.gz",
                    sha256: "50dde99bb066adacbb9a748182d09eb9beff25119dc3b12e85c84b6b6b1c5da9",
                    bytes: 70_628_335,
                },
                Archive {
                    platform: "macos-x86_64",
                    url: "https://github.com/PowerShell/PowerShell/releases/download/v{v}/powershell-{v}-osx-x64.tar.gz",
                    sha256: "c354ca845cb81f9239bc0c6fbfd14256056400e4b269bd7a6373d113ad94c1e3",
                    bytes: 74_691_245,
                },
            ],
            strip: 0,
            anchor: Anchor::DeveloperId {
                team: MICROSOFT_TEAM,
            },
            keep: &["LICENSE.txt", "ThirdPartyNotices.txt"],
        },
    },
    Row {
        name: "binaryen",
        tool: Some("wasm-opt"),
        install: Install::Unpack {
            archives: &[
                Archive {
                    platform: "macos-aarch64",
                    url: "https://github.com/WebAssembly/binaryen/releases/download/version_{v}/binaryen-version_{v}-arm64-macos.tar.gz",
                    sha256: "ad66da82ac13f163e424b1643f16c6dfcccc98b5966296b43e52d3cab04f84a8",
                    bytes: 7_810_532,
                },
                Archive {
                    platform: "macos-x86_64",
                    url: "https://github.com/WebAssembly/binaryen/releases/download/version_{v}/binaryen-version_{v}-x86_64-macos.tar.gz",
                    sha256: "13a9b90be775c6389ce3d1f879cb8627bea56708ba8c122983941d53a8199b95",
                    bytes: 9_276_271,
                },
            ],
            strip: 1,
            anchor: Anchor::Sha256,
            keep: &[],
        },
    },
    Row {
        name: "chrome-headless-shell",
        tool: Some("chrome-headless-shell"),
        install: Install::Browser(Engine::Chromium),
    },
    Row {
        name: "firefox",
        tool: Some("firefox"),
        install: Install::Browser(Engine::Firefox),
    },
    Row {
        name: "webkit",
        tool: None,
        install: Install::Browser(Engine::Webkit),
    },
    Row {
        name: "lima",
        tool: Some("limactl"),
        install: Install::Unpack {
            archives: &[
                Archive {
                    platform: "macos-aarch64",
                    url: "https://github.com/lima-vm/lima/releases/download/v{v}/lima-{v}-Darwin-arm64.tar.gz",
                    sha256: "9e9eacce88f37e185c346bad73aa6136f738d8cdf8c3bb23cd42b071824bc66e",
                    bytes: 38_328_082,
                },
                Archive {
                    platform: "macos-x86_64",
                    url: "https://github.com/lima-vm/lima/releases/download/v{v}/lima-{v}-Darwin-x86_64.tar.gz",
                    sha256: "6b776e4bc41f358e6ee01fefcc78e2eb26f11635a0fcff632234af77a566c6c4",
                    bytes: 24_958_659,
                },
            ],
            strip: 0,
            anchor: Anchor::Sha256,
            keep: &[],
        },
    },
    Row {
        name: "wasm-bindgen",
        tool: Some("wasm-bindgen"),
        install: Install::Cargo,
    },
];

/// The row named `name`.
#[must_use]
pub fn row(name: &str) -> Option<&'static Row> {
    ROWS.iter().find(|r| r.name == name)
}
