// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! THE PINNED GNU BASH 5.3, ONE SOURCE: GNU's release archive and each of its official patches,
//! held to its sha256 and size, that every build of the `bash-5.3` row's pin is made from.
//! `verify provision` builds it on a Mac (`aterm-verify`'s `bash` row) and the Linux lane's
//! guest builds it the same way (`aterm-lab`'s `linux_guest::BASH_5_3`). A bump here moves
//! both, with the row's version: the release and its patch level ([`PATCHES`]' count).

/// One file GNU publishes for the release, the same on every host.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct File {
    /// Its URL.
    pub url: &'static str,
    /// Its lowercase-hex sha256.
    pub sha256: &'static str,
    /// Its size in bytes.
    pub bytes: u64,
}

/// GNU's bash 5.3 release archive.
pub const SOURCE: File = File {
    url: "https://ftp.gnu.org/gnu/bash/bash-5.3.tar.gz",
    sha256: "0d5cd86965f869a26cf64f4b71be7b96f90a3ba8b3d74e27e8e9d9d5550f31ba",
    bytes: 11_355_854,
};

/// Its official patches, in the order they apply (`patch -p0` in the unpacked tree).
pub const PATCHES: &[File] = &[
    File {
        url: "https://ftp.gnu.org/gnu/bash/bash-5.3-patches/bash53-001",
        sha256: "1f608434364af86b9b45c8b0ea3fb3b165fb830d27697e6cdfc7ac17dee3287f",
        bytes: 1531,
    },
    File {
        url: "https://ftp.gnu.org/gnu/bash/bash-5.3-patches/bash53-002",
        sha256: "e385548a00130765ec7938a56fbdca52447ab41fabc95a25f19ade527e282001",
        bytes: 2635,
    },
    File {
        url: "https://ftp.gnu.org/gnu/bash/bash-5.3-patches/bash53-003",
        sha256: "f245d9c7dc3f5a20d84b53d249334747940936f09dc97e1dcb89fc3ab37d60ed",
        bytes: 2683,
    },
    File {
        url: "https://ftp.gnu.org/gnu/bash/bash-5.3-patches/bash53-004",
        sha256: "9591d245045529f32f0812f94180b9d9ce9023f5a765c039b852e5dfc99747d0",
        bytes: 1266,
    },
    File {
        url: "https://ftp.gnu.org/gnu/bash/bash-5.3-patches/bash53-005",
        sha256: "cca1ef52dbbf433bc98e33269b64b2c814028efe2538be1e2c9a377da90bc99d",
        bytes: 1072,
    },
    File {
        url: "https://ftp.gnu.org/gnu/bash/bash-5.3-patches/bash53-006",
        sha256: "29119addefed8eff91ae37fd51822c31780ee30d4a28376e96002706c995ff10",
        bytes: 1346,
    },
    File {
        url: "https://ftp.gnu.org/gnu/bash/bash-5.3-patches/bash53-007",
        sha256: "c0976bbfffa1453c7cfdd62058f206a318568ff2d690f5d4fa048793fa3eb299",
        bytes: 1437,
    },
    File {
        url: "https://ftp.gnu.org/gnu/bash/bash-5.3-patches/bash53-008",
        sha256: "097cd723cbfb8907674ac32214063a3fd85282657ec5b4e544d2c0f719653fb4",
        bytes: 5717,
    },
    File {
        url: "https://ftp.gnu.org/gnu/bash/bash-5.3-patches/bash53-009",
        sha256: "eee30fe78a4b0cb2fe20e010e00308899cfc613e0774ebb3c8557a1552f24f8c",
        bytes: 2468,
    },
    File {
        url: "https://ftp.gnu.org/gnu/bash/bash-5.3-patches/bash53-010",
        sha256: "cf76f1cce2ea300c18bff9f002d21f280cc931acd17c28518110b93fe6e72569",
        bytes: 1457,
    },
    File {
        url: "https://ftp.gnu.org/gnu/bash/bash-5.3-patches/bash53-011",
        sha256: "0298df8f5ea2a31d3be43ed7d269c5b3c7c342dd5b570bea7f64d66dcbbe7531",
        bytes: 2251,
    },
    File {
        url: "https://ftp.gnu.org/gnu/bash/bash-5.3-patches/bash53-012",
        sha256: "d71379b39bebaedaf123414414e77fb458a0a43b9ad3116594c6df7ca6754573",
        bytes: 2378,
    },
    File {
        url: "https://ftp.gnu.org/gnu/bash/bash-5.3-patches/bash53-013",
        sha256: "042f9cda967e24bf4211944697441e93d06ff42b4b998629a98a1b249279f200",
        bytes: 1467,
    },
    File {
        url: "https://ftp.gnu.org/gnu/bash/bash-5.3-patches/bash53-014",
        sha256: "bd4360b401d38507e358783dcad8536a99c6789f0d3a5bd0cfb8c4a34144696c",
        bytes: 2229,
    },
    File {
        url: "https://ftp.gnu.org/gnu/bash/bash-5.3-patches/bash53-015",
        sha256: "55b79ceee2fc27f6767eed697e939a7eb2fe2a28c01556bd75f18d581014f46e",
        bytes: 2151,
    },
    File {
        url: "https://ftp.gnu.org/gnu/bash/bash-5.3-patches/bash53-016",
        sha256: "9ea29b266b7d24cb34d0ff3f1c4631e4d527bfe2d1ef15d17cdb924bf31ef767",
        bytes: 4185,
    },
    File {
        url: "https://ftp.gnu.org/gnu/bash/bash-5.3-patches/bash53-017",
        sha256: "443b927b45c1558ca72052410f8b8f6e5152b617ed707061a2781d4375b0d1c3",
        bytes: 2229,
    },
    File {
        url: "https://ftp.gnu.org/gnu/bash/bash-5.3-patches/bash53-018",
        sha256: "ae715d76c50341d7d7095e9a8d2eeed1ca9546152c2ac7289206f90cf30ac697",
        bytes: 2660,
    },
    File {
        url: "https://ftp.gnu.org/gnu/bash/bash-5.3-patches/bash53-019",
        sha256: "a25c581e4d0057dea3833918438a930e2e86ee4c6dc17fe15267b7f04cbc4e3d",
        bytes: 5061,
    },
    File {
        url: "https://ftp.gnu.org/gnu/bash/bash-5.3-patches/bash53-020",
        sha256: "df217ed3a9122aa2286d9b67bbe348661b6a9db262b580c29150dae55d532896",
        bytes: 1591,
    },
];
