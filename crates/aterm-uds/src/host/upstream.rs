// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

//! Immutable upstream tool inputs shared by workspace provisioning and package producers.

/// One upstream archive, held to its exact host, length and SHA-256.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Archive {
    /// `<os>-<arch>`, or `source` for a source archive.
    pub platform: &'static str,
    /// URL template whose `{v}` is the recipe's version.
    pub url: &'static str,
    /// Lowercase SHA-256.
    pub sha256: &'static str,
    /// Exact archive length.
    pub bytes: u64,
}

impl Archive {
    /// The upstream URL at the supplied version.
    #[must_use]
    pub fn url_at(&self, version: &str) -> String {
        self.url.replace("{v}", version)
    }
}

/// An upstream tool's pinned recipe, independent of its install destination.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct PinnedTool {
    /// Package and executable identity.
    pub name: &'static str,
    /// The version all archives carry.
    pub version: &'static str,
    /// Program path after extraction and any declared source build.
    pub executable: &'static str,
    /// Whether the executable is self-contained or needs the complete archive tree.
    pub layout: Layout,
    /// Whether the archive is ready to use or needs a native source build.
    pub build: Build,
    /// Leading archive components to remove.
    pub strip: u32,
    /// Maximum decoded archive bytes, including its tar structure.
    pub max_extract_bytes: u64,
    /// License files preserved by both consumers.
    pub licenses: &'static [&'static str],
    /// Files unused by Darwin consumers, omitted only from Darwin packages. Workspace
    /// provisioning retains the original archive tree.
    pub darwin_omit: &'static [&'static str],
    /// Supported hosts, derived from the archives.
    pub hosts: &'static [&'static str],
    /// Pinned archive for each host.
    pub archives: &'static [Archive],
}

/// The runtime carried by a pinned archive.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Layout {
    /// Install one executable in `bin`, plus the declared licenses.
    Executable,
    /// Preserve the archive's runtime tree, including libraries and standard modules.
    RuntimeTree,
}

/// Preparation required before laying out the pinned runtime.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Build {
    /// The archive already holds the runtime.
    Prebuilt,
    /// Build the signing executable with statically linked OpenSSL libraries.
    OpenSsl,
}

/// One locally built reference artifact qualified for a particular cohort and host.
/// A later rebuild gets its own qualification and digest, even at the same source revision.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ReferenceArtifact {
    /// The qualified `<os>-<arch>` host.
    pub platform: &'static str,
    /// SHA-256 of the executable before package relocation and signing.
    pub sha256: &'static str,
    /// Exact executable length.
    pub bytes: u64,
    /// Trust compiler revision used by this qualified unverified release build.
    pub compiler: &'static str,
    /// The retained Cargo compiler-query output associated with this build.
    pub compiler_query_sha256: &'static str,
    /// Exact retained compiler-query text length.
    pub compiler_query_bytes: u64,
    /// Trust build selecting this artifact's compiler.
    pub trust_build: u64,
}

/// An original license text retained alongside the complete corresponding source.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ReferenceLicense {
    /// Package-relative file name.
    pub path: &'static str,
    /// Original text's SHA-256.
    pub sha256: &'static str,
    /// Exact text length.
    pub bytes: u64,
}

/// A source-derived reference tool prepared through the ordinary bottle install-tree path.
/// This is not an upstream prebuilt archive or a claim of checker soundness.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ReferenceTool {
    /// Package and executable name.
    pub name: &'static str,
    /// Upstream version.
    pub version: &'static str,
    /// Public source repository.
    pub repository: &'static str,
    /// Exact Git commit used by the qualified build.
    pub revision: &'static str,
    /// Exact Git tree used by the qualified build.
    pub source_tree: &'static str,
    /// SHA-256 of the unchanged source lockfile.
    pub lock_sha256: &'static str,
    /// The complete corresponding-source archive, including dependencies and rebuild materials.
    pub source_kit_sha256: &'static str,
    /// Exact corresponding-source archive length.
    pub source_kit_bytes: u64,
    /// Readily accessible original license texts; other notices remain in the complete kit.
    pub licenses: &'static [ReferenceLicense],
    /// Qualified artifacts; this does not admit other hosts or future rebuilds.
    pub artifacts: &'static [ReferenceArtifact],
}

/// The pinned independent Carcara reference used by AY's guarded comparison lane.
pub const CARCARA: ReferenceTool = ReferenceTool {
    name: "carcara",
    version: "1.1.0",
    repository: "ufmg-smite/carcara",
    revision: "9a352eea6c935ad35cb8ec22e521a7620ec5d474",
    source_tree: "80e8ca1f905ccd843ab784f6f9669d995010a505",
    lock_sha256: "55f48e70a8c943437b0d56cc4848ce4e86dfe6194a3c9f7a5f052c0c0788b6a4",
    source_kit_sha256: "eb69f57363031e8ba97e1936807434a0be31f9679685b53948f61afd315621a6",
    source_kit_bytes: 55_315_355,
    licenses: &[
        ReferenceLicense {
            path: "share/licenses/carcara/LICENSE-APACHE.txt",
            sha256: "62c7a1e35f56406896d7aa7ca52d0cc0d272ac022b5d2796e7d6905db8a3636a",
            bytes: 9_723,
        },
        ReferenceLicense {
            path: "share/licenses/carcara/LICENSE-LGPL.txt",
            sha256: "1e950a32358912876fcd3ea0bdd975c9ea8a7822eddc3cda662817c3abe99b1c",
            bytes: 7_642,
        },
        ReferenceLicense {
            path: "share/licenses/carcara/LICENSE-GPL.txt",
            sha256: "33bc26d4498c183b703d1760ee45b8774d21d6e0178ca393dc715271840c7a21",
            bytes: 34_916,
        },
    ],
    artifacts: &[
        ReferenceArtifact {
            platform: "macos-aarch64",
            sha256: "e27cb05e4119ac8ece1b3ba70cab2b7aae710fae70ad2252d0086be8d3b363a9",
            bytes: 4_658_712,
            compiler: "29ebb403de7a102850d6cb10a62d5cc3baf723ed",
            compiler_query_sha256: "c8c019cab02c90d11961e41e1cace4ef5dcf9181babfb0866c6517fd038c6897",
            compiler_query_bytes: 255,
            trust_build: 16_387,
        },
        ReferenceArtifact {
            platform: "macos-x86_64",
            sha256: "db27f7fe19c26ee14e53981e333b159dd216a757972ff4fc24d6c8c01703a532",
            bytes: 6_081_424,
            compiler: "3d7768caf6dcba0e4f5083a854bb77805ea1aafd",
            compiler_query_sha256: "cbb2ecb561be3c70df9c4448a4b1827bcfb8456b6ef06f9147b335a27be39bce",
            compiler_query_bytes: 228,
            trust_build: 6_808,
        },
    ],
};

impl PinnedTool {
    /// The archive this recipe admits on `platform`.
    #[must_use]
    pub fn archive_for(&self, platform: &str) -> Option<&Archive> {
        if !self.hosts.contains(&platform) {
            return None;
        }
        self.archives
            .iter()
            .find(|archive| archive.platform == platform)
            .or_else(|| {
                self.archives
                    .iter()
                    .find(|archive| archive.platform == "source")
            })
    }
}

const fn platforms<const N: usize>(archives: &[Archive; N]) -> [&'static str; N] {
    let mut hosts = [""; N];
    let mut index = 0;
    while index < N {
        hosts[index] = archives[index].platform;
        index += 1;
    }
    hosts
}

const GITLEAKS_ARCHIVES: [Archive; 4] = [
    Archive {
        platform: "macos-aarch64",
        url: "https://github.com/gitleaks/gitleaks/releases/download/v{v}/gitleaks_{v}_darwin_arm64.tar.gz",
        sha256: "b40ab0ae55c505963e365f271a8d3846efbc170aa17f2607f13df610a9aeb6a5",
        bytes: 7_897_593,
    },
    Archive {
        platform: "macos-x86_64",
        url: "https://github.com/gitleaks/gitleaks/releases/download/v{v}/gitleaks_{v}_darwin_x64.tar.gz",
        sha256: "dfe101a4db2255fc85120ac7f3d25e4342c3c20cf749f2c20a18081af1952709",
        bytes: 8_359_235,
    },
    Archive {
        platform: "linux-aarch64",
        url: "https://github.com/gitleaks/gitleaks/releases/download/v{v}/gitleaks_{v}_linux_arm64.tar.gz",
        sha256: "e4a487ee7ccd7d3a7f7ec08657610aa3606637dab924210b3aee62570fb4b080",
        bytes: 7_601_421,
    },
    Archive {
        platform: "linux-x86_64",
        url: "https://github.com/gitleaks/gitleaks/releases/download/v{v}/gitleaks_{v}_linux_x64.tar.gz",
        sha256: "551f6fc83ea457d62a0d98237cbad105af8d557003051f41f3e7ca7b3f2470eb",
        bytes: 8_230_402,
    },
];

/// The same source and layout used by `verify provision gitleaks` and atpkg's producer.
pub const GITLEAKS: PinnedTool = PinnedTool {
    name: "gitleaks",
    version: "8.30.1",
    executable: "gitleaks",
    layout: Layout::Executable,
    build: Build::Prebuilt,
    strip: 0,
    max_extract_bytes: 1024 * 1024 * 1024,
    licenses: &["LICENSE"],
    darwin_omit: &[],
    hosts: &platforms(&GITLEAKS_ARCHIVES),
    archives: &GITLEAKS_ARCHIVES,
};

const CPYTHON_ARCHIVES: [Archive; 2] = [
    Archive {
        platform: "macos-aarch64",
        url: "https://github.com/astral-sh/python-build-standalone/releases/download/20261013/cpython-{v}-aarch64-apple-darwin-install_only.tar.gz",
        sha256: "709f419d175eb015f810b68aa54bdae1d776c9824cb362aa701c93b0a9f2a9e7",
        bytes: 26_814_582,
    },
    Archive {
        platform: "macos-x86_64",
        url: "https://github.com/astral-sh/python-build-standalone/releases/download/20261013/cpython-{v}-x86_64-apple-darwin-install_only.tar.gz",
        sha256: "e09bce050cf075e777d1a802c4a11b6d43d54291b5f76911887173b28ad9c9fd",
        bytes: 27_005_136,
    },
];

/// The standalone release-host interpreter used by workspace provisioning and atpkg.
pub const CPYTHON: PinnedTool = PinnedTool {
    name: "cpython",
    version: "3.14.8+20261013",
    executable: "bin/python3.14",
    layout: Layout::RuntimeTree,
    build: Build::Prebuilt,
    strip: 1,
    max_extract_bytes: 1024 * 1024 * 1024,
    licenses: &["lib/python3.14/LICENSE.txt"],
    // pip's distlib loads these launchers only under os.name == 'nt'.
    darwin_omit: &[
        "lib/python3.14/site-packages/pip/_vendor/distlib/t32.exe",
        "lib/python3.14/site-packages/pip/_vendor/distlib/t64.exe",
        "lib/python3.14/site-packages/pip/_vendor/distlib/t64-arm.exe",
        "lib/python3.14/site-packages/pip/_vendor/distlib/w32.exe",
        "lib/python3.14/site-packages/pip/_vendor/distlib/w64.exe",
        "lib/python3.14/site-packages/pip/_vendor/distlib/w64-arm.exe",
    ],
    hosts: &platforms(&CPYTHON_ARCHIVES),
    archives: &CPYTHON_ARCHIVES,
};

/// OpenSSL's native signing build has no OpenSSL shared libraries or loadable providers.
pub const OPENSSL_CONFIGURE_OPTIONS: &[&str] = &["no-shared", "no-dso", "no-module"];

/// The same immutable source used by workspace provisioning and package producers.
pub const OPENSSL_SIGNING_SOURCE: Archive = Archive {
    platform: "source",
    url: "https://github.com/openssl/openssl/releases/download/openssl-{v}/openssl-{v}.tar.gz",
    sha256: "603f5602e2eef00d77fbd429d34dcd5822bb301757a1bc9cdb24c670f1eb859a",
    bytes: 53_279_637,
};

/// The release host's Ed25519 signing executable, built from pinned OpenSSL source.
pub const OPENSSL_SIGNING: PinnedTool = PinnedTool {
    name: "openssl-signing",
    version: "3.5.9",
    executable: "apps/openssl",
    layout: Layout::Executable,
    build: Build::OpenSsl,
    strip: 1,
    max_extract_bytes: 1024 * 1024 * 1024,
    licenses: &["LICENSE.txt"],
    darwin_omit: &[],
    hosts: &["macos-aarch64", "macos-x86_64"],
    archives: &[OPENSSL_SIGNING_SOURCE],
};

/// Official self-contained Node runtime for JavaScript/WebAssembly reference checks.
/// This package exposes Node alone; npm is not part of its runtime contract.
pub const NODE: PinnedTool = PinnedTool {
    name: "node",
    version: "22.23.4",
    executable: "bin/node",
    layout: Layout::Executable,
    build: Build::Prebuilt,
    strip: 1,
    max_extract_bytes: 256 * 1024 * 1024,
    licenses: &["LICENSE"],
    darwin_omit: &[],
    hosts: &["macos-aarch64", "macos-x86_64"],
    archives: &[
        Archive {
            platform: "macos-aarch64",
            url: "https://nodejs.org/dist/v{v}/node-v{v}-darwin-arm64.tar.gz",
            sha256: "c9e1dbc748b33ed2d14f42445191ecaf14c56259c7a9a26d0042c49ec2b66428",
            bytes: 49_996_369,
        },
        Archive {
            platform: "macos-x86_64",
            url: "https://nodejs.org/dist/v{v}/node-v{v}-darwin-x64.tar.gz",
            sha256: "efcc2b0db6f94bf53eddb038fe2b370277a042bd29e4f19b8f8e23598795b819",
            bytes: 51_190_214,
        },
    ],
};

const LEAN4_ARCHIVES: [Archive; 2] = [
    Archive {
        platform: "macos-aarch64",
        url: "https://github.com/leanprover/lean4/releases/download/v{v}/lean-{v}-darwin_aarch64.tar.zst",
        sha256: "264105500c8abdf37b68ffe03390a783ed259807807222698da8dd92d6ce0a27",
        bytes: 543_754_552,
    },
    Archive {
        platform: "macos-x86_64",
        url: "https://github.com/leanprover/lean4/releases/download/v{v}/lean-{v}-darwin.tar.zst",
        sha256: "6dac7a8f9d6d0bc339b4ea9376c06a88f3fd1a7f462beb3c7ded9fbc934f3fb5",
        bytes: 546_928_252,
    },
];

/// Lean and Lake for the proof projects that need upstream Lean rather than Clean.
pub const LEAN4: PinnedTool = PinnedTool {
    name: "lean4",
    version: "4.31.0",
    executable: "bin/lake",
    layout: Layout::RuntimeTree,
    build: Build::Prebuilt,
    strip: 1,
    // The pinned ARM archive contains 2,761,381,330 file bytes and 15,194 entries.
    // Include tar headers/padding without changing the ordinary bottle allowance.
    max_extract_bytes: 3 * 1024 * 1024 * 1024,
    licenses: &["LICENSE", "LICENSES"],
    darwin_omit: &[],
    hosts: &platforms(&LEAN4_ARCHIVES),
    archives: &LEAN4_ARCHIVES,
};

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_shared_source_archive_does_not_admit_unsupported_hosts() {
        let recipe = PinnedTool {
            hosts: &["macos-aarch64"],
            ..OPENSSL_SIGNING
        };
        assert_eq!(
            recipe.archive_for("macos-aarch64"),
            Some(&OPENSSL_SIGNING_SOURCE)
        );
        for host in ["macos-x86_64", "linux-aarch64", "windows-aarch64", "source"] {
            assert_eq!(recipe.archive_for(host), None, "{host}");
        }
    }
}
