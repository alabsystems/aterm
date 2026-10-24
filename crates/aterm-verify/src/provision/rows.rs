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

/// What an unpacked row's pinned file is.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Holds {
    /// A program: held to its execute bit.
    Program,
    /// A disk image the Linux lane's VM clones (`aterm_vz::pins`): held to being a regular
    /// file of its size, there and whenever the row is read in place, and made read-only
    /// (0444), since nothing writes it.
    Image {
        /// Its size in bytes.
        bytes: u64,
    },
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
        /// The most the tree may hold, in bytes: `None` is the shared cap, sized for the
        /// tool rows; a row whose tree outgrows it names its own, by the same rule.
        cap: Option<u64>,
        /// What the pin's file (its tool row's `exe`) is.
        holds: Holds,
    },
    /// Upstream files, no archive: each fetched for the host, held to its size and sha256,
    /// kept read-only (0444) under its URL's last segment, and placed together; the row is
    /// in place only while every file is there at its size. The tool row's `exe` names the
    /// first.
    Files {
        /// The files, each for its host.
        files: &'static [Archive],
    },
    /// Upstream source tarball built with Trust (`targo --unverified build --release --locked --no-default-features`).
    Build {
        /// Source tarball.
        source: Archive,
        /// Cargo features enabled past `--no-default-features`.
        features: &'static [&'static str],
    },
    /// Upstream C source tarball and its official patches, each held to its sha256, the
    /// patches applied in order (`patch -p0`), built by its own `./configure` and `make` with
    /// the OS's compiler.
    Configure {
        /// Source tarball.
        source: Archive,
        /// Patches, in the order they apply.
        patches: &'static [Archive],
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
            Install::Build { source, .. } | Install::Configure { source, .. } => self
                .tool_row()
                .and_then(|t| t.pin)
                .is_some_and(|p| p.pinned_for(platform))
                .then_some(source),
            _ => None,
        }
    }

    /// The files an [`Install::Files`] row fetches for `platform`, in order (none for any
    /// other row).
    #[must_use]
    pub fn files_for(&self, platform: &str) -> Vec<Archive> {
        match self.install {
            Install::Files { files } => files
                .iter()
                .filter(|f| f.platform == platform)
                .copied()
                .collect(),
            _ => Vec::new(),
        }
    }
}

/// An [`aterm_vz::pins::File`] as the row's archive for the Linux lane's one host.
const fn ubuntu(file: aterm_vz::pins::File) -> Archive {
    Archive {
        platform: aterm_vz::pins::PLATFORM,
        url: file.url,
        sha256: file.sha256,
        bytes: file.bytes,
    }
}

/// Microsoft's Apple Developer ID team signing the PowerShell archives.
pub const MICROSOFT_TEAM: &str = "UBF8T346G9";

/// Every row, in the order a run provides them.
pub const ROWS: [Row; 12] = [
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
        name: "bash",
        tool: Some("bash-5.3"),
        install: Install::Configure {
            source: Archive {
                platform: "source",
                url: "https://ftp.gnu.org/gnu/bash/bash-5.3.tar.gz",
                sha256: "0d5cd86965f869a26cf64f4b71be7b96f90a3ba8b3d74e27e8e9d9d5550f31ba",
                bytes: 11_355_854,
            },
            patches: &[
                Archive {
                    platform: "source",
                    url: "https://ftp.gnu.org/gnu/bash/bash-5.3-patches/bash53-001",
                    sha256: "1f608434364af86b9b45c8b0ea3fb3b165fb830d27697e6cdfc7ac17dee3287f",
                    bytes: 1531,
                },
                Archive {
                    platform: "source",
                    url: "https://ftp.gnu.org/gnu/bash/bash-5.3-patches/bash53-002",
                    sha256: "e385548a00130765ec7938a56fbdca52447ab41fabc95a25f19ade527e282001",
                    bytes: 2635,
                },
                Archive {
                    platform: "source",
                    url: "https://ftp.gnu.org/gnu/bash/bash-5.3-patches/bash53-003",
                    sha256: "f245d9c7dc3f5a20d84b53d249334747940936f09dc97e1dcb89fc3ab37d60ed",
                    bytes: 2683,
                },
                Archive {
                    platform: "source",
                    url: "https://ftp.gnu.org/gnu/bash/bash-5.3-patches/bash53-004",
                    sha256: "9591d245045529f32f0812f94180b9d9ce9023f5a765c039b852e5dfc99747d0",
                    bytes: 1266,
                },
                Archive {
                    platform: "source",
                    url: "https://ftp.gnu.org/gnu/bash/bash-5.3-patches/bash53-005",
                    sha256: "cca1ef52dbbf433bc98e33269b64b2c814028efe2538be1e2c9a377da90bc99d",
                    bytes: 1072,
                },
                Archive {
                    platform: "source",
                    url: "https://ftp.gnu.org/gnu/bash/bash-5.3-patches/bash53-006",
                    sha256: "29119addefed8eff91ae37fd51822c31780ee30d4a28376e96002706c995ff10",
                    bytes: 1346,
                },
                Archive {
                    platform: "source",
                    url: "https://ftp.gnu.org/gnu/bash/bash-5.3-patches/bash53-007",
                    sha256: "c0976bbfffa1453c7cfdd62058f206a318568ff2d690f5d4fa048793fa3eb299",
                    bytes: 1437,
                },
                Archive {
                    platform: "source",
                    url: "https://ftp.gnu.org/gnu/bash/bash-5.3-patches/bash53-008",
                    sha256: "097cd723cbfb8907674ac32214063a3fd85282657ec5b4e544d2c0f719653fb4",
                    bytes: 5717,
                },
                Archive {
                    platform: "source",
                    url: "https://ftp.gnu.org/gnu/bash/bash-5.3-patches/bash53-009",
                    sha256: "eee30fe78a4b0cb2fe20e010e00308899cfc613e0774ebb3c8557a1552f24f8c",
                    bytes: 2468,
                },
                Archive {
                    platform: "source",
                    url: "https://ftp.gnu.org/gnu/bash/bash-5.3-patches/bash53-010",
                    sha256: "cf76f1cce2ea300c18bff9f002d21f280cc931acd17c28518110b93fe6e72569",
                    bytes: 1457,
                },
                Archive {
                    platform: "source",
                    url: "https://ftp.gnu.org/gnu/bash/bash-5.3-patches/bash53-011",
                    sha256: "0298df8f5ea2a31d3be43ed7d269c5b3c7c342dd5b570bea7f64d66dcbbe7531",
                    bytes: 2251,
                },
                Archive {
                    platform: "source",
                    url: "https://ftp.gnu.org/gnu/bash/bash-5.3-patches/bash53-012",
                    sha256: "d71379b39bebaedaf123414414e77fb458a0a43b9ad3116594c6df7ca6754573",
                    bytes: 2378,
                },
                Archive {
                    platform: "source",
                    url: "https://ftp.gnu.org/gnu/bash/bash-5.3-patches/bash53-013",
                    sha256: "042f9cda967e24bf4211944697441e93d06ff42b4b998629a98a1b249279f200",
                    bytes: 1467,
                },
                Archive {
                    platform: "source",
                    url: "https://ftp.gnu.org/gnu/bash/bash-5.3-patches/bash53-014",
                    sha256: "bd4360b401d38507e358783dcad8536a99c6789f0d3a5bd0cfb8c4a34144696c",
                    bytes: 2229,
                },
                Archive {
                    platform: "source",
                    url: "https://ftp.gnu.org/gnu/bash/bash-5.3-patches/bash53-015",
                    sha256: "55b79ceee2fc27f6767eed697e939a7eb2fe2a28c01556bd75f18d581014f46e",
                    bytes: 2151,
                },
                Archive {
                    platform: "source",
                    url: "https://ftp.gnu.org/gnu/bash/bash-5.3-patches/bash53-016",
                    sha256: "9ea29b266b7d24cb34d0ff3f1c4631e4d527bfe2d1ef15d17cdb924bf31ef767",
                    bytes: 4185,
                },
                Archive {
                    platform: "source",
                    url: "https://ftp.gnu.org/gnu/bash/bash-5.3-patches/bash53-017",
                    sha256: "443b927b45c1558ca72052410f8b8f6e5152b617ed707061a2781d4375b0d1c3",
                    bytes: 2229,
                },
                Archive {
                    platform: "source",
                    url: "https://ftp.gnu.org/gnu/bash/bash-5.3-patches/bash53-018",
                    sha256: "ae715d76c50341d7d7095e9a8d2eeed1ca9546152c2ac7289206f90cf30ac697",
                    bytes: 2660,
                },
                Archive {
                    platform: "source",
                    url: "https://ftp.gnu.org/gnu/bash/bash-5.3-patches/bash53-019",
                    sha256: "a25c581e4d0057dea3833918438a930e2e86ee4c6dc17fe15267b7f04cbc4e3d",
                    bytes: 5061,
                },
                Archive {
                    platform: "source",
                    url: "https://ftp.gnu.org/gnu/bash/bash-5.3-patches/bash53-020",
                    sha256: "df217ed3a9122aa2286d9b67bbe348661b6a9db262b580c29150dae55d532896",
                    bytes: 1591,
                },
            ],
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
            cap: None,
            holds: Holds::Program,
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
            cap: None,
            holds: Holds::Program,
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
            cap: None,
            holds: Holds::Program,
        },
    },
    Row {
        name: "ubuntu-root",
        tool: Some("ubuntu-root"),
        install: Install::Unpack {
            archives: &[ubuntu(aterm_vz::pins::ROOT_ARCHIVE)],
            strip: 0,
            anchor: Anchor::Sha256,
            keep: &[aterm_vz::pins::ROOT_MEMBER],
            cap: Some(aterm_vz::pins::ROOT_CAP),
            holds: Holds::Image {
                bytes: aterm_vz::pins::ROOT_MEMBER_BYTES,
            },
        },
    },
    Row {
        name: "ubuntu-kernel",
        tool: Some("ubuntu-kernel"),
        install: Install::Files {
            files: &[
                ubuntu(aterm_vz::pins::VMLINUZ),
                ubuntu(aterm_vz::pins::INITRD),
            ],
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
