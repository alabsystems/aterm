// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The rows `verify provision` provides (docs/DESIGN-init-system-2026-10-20.md §4.4, §6.2–§6.4):
//! what each is, where upstream serves it for which host, what it must hash to, the anchor its tree
//! is held to, and how it is installed.

use aterm_uds::host::{Tool, When};

/// How a row's tree is held to its origin before anything is placed.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Anchor {
    /// The archive's sha256 is the only anchor: upstream signs nothing.
    Sha256,
    /// The sha256, then every Mach-O in the unpacked tree verified by codesign as a Developer ID of
    /// `team` (`aterm_update_core::codesign::verify_developer_id_until`).
    DeveloperId {
        /// The Apple team id every Mach-O must carry.
        team: &'static str,
    },
}

/// One upstream archive a row fetches for one host.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Archive {
    /// `<os>-<arch>` as Rust names them (`macos-aarch64`), or `source` for any host.
    pub platform: &'static str,
    /// Where upstream serves it, `{v}` standing for the row's version.
    pub url: &'static str,
    /// Its sha256, lowercase hex.
    pub sha256: &'static str,
    /// Its size in bytes.
    pub bytes: u64,
}

impl Archive {
    /// [`Archive::url`] at `version`.
    #[must_use]
    pub fn url_at(&self, version: &str) -> String {
        self.url.replace("{v}", version)
    }
}

/// The browser lane's three engines, whose pins live beside the code that drives them
/// (`crate::browser`): each is placed in the browser cache the lane reads.
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
    /// An upstream release archive, checked against `anchor` and unpacked through atpkg's vetted
    /// extractor, `strip` leading components dropped; every name in `keep` (a licence, its notices)
    /// must be in the tree.
    Unpack {
        /// One per host upstream builds for.
        archives: &'static [Archive],
        /// Leading path components each member loses.
        strip: u32,
        /// What the tree is held to.
        anchor: Anchor,
        /// Files the tree must keep.
        keep: &'static [&'static str],
    },
    /// Upstream's source tarball, built here with Trust (fish, §6.2): `targo --unverified build
    /// --release --locked --no-default-features --features <features>`, on the hosts its tool row's
    /// pin names ([`aterm_uds::host::Pin::hosts`]).
    Build {
        /// The tarball.
        source: Archive,
        /// The features the build names (empty: none past `--no-default-features`).
        features: &'static [&'static str],
    },
    /// A crate's CLI built by `targo --unverified install --locked` at the pin (wasm-bindgen):
    /// crates.io's checksums and the crate's lockfile are its anchor.
    Cargo,
    /// A browser the browser lane pins.
    Browser(Engine),
    /// rustup's stable toolchain with the slices the cells, the web modules and the shipped
    /// binary's x86_64 slice need, and the wasm32 pin (STOCK EXCEPTION).
    Rustup {
        /// The rustup release its installer is pinned at.
        version: &'static str,
        /// rustup-init, one per host.
        init: &'static [Archive],
    },
}

/// One row of `verify provision`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Row {
    /// The row's name on the command line.
    pub name: &'static str,
    /// The [`aterm_uds::host::TOOLS`] row it provides, where it provides one.
    pub tool: Option<&'static str>,
    /// How it is installed.
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

    /// The version its tool row pins (`None` for a row whose version lives elsewhere: the
    /// browsers', in `crate::browser`, and rustup's toolchains).
    #[must_use]
    pub fn version(&self) -> Option<&'static str> {
        self.tool_row().and_then(|t| t.pin).map(|p| p.version)
    }

    /// The archive it fetches for `platform` (`<os>-<arch>`): an unpacked row's, a built row's
    /// source where its pin names the host, rustup's installer.
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

/// Microsoft's Apple team, which signs every Mach-O of the PowerShell archives (all 17 of 7.6.7's
/// in each, measured with `codesign -dv`).
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
