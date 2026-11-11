// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Ubuntu's pinned release files (`docs/DESIGN-vz-guest-2026-10-23.md` §3).
//!
//! Three files of one dated release, each held to the sha256 Ubuntu's `SHA256SUMS` lists,
//! and the gunzipped kernel held to the sha256 recorded here. There is no unpinned
//! fallback. `verify provision --linux` fetches them (its `ubuntu-root` and
//! `ubuntu-kernel` rows read these constants); the lab finds what it placed through
//! [`aterm_uds::host::TOOLS`]' rows of the same names.
//!
//! A bump is one commit, made while Ubuntu's primary host still serves the pinned release
//! and once a newer dated release has passed the lane: the three files' sha256 and sizes,
//! [`ROOT_MEMBER_BYTES`], [`IMAGE_SHA256`] and [`IMAGE_BYTES`], [`KERNEL_RELEASE`] (the
//! root's module directory), and the release in the two `TOOLS` rows ([`RELEASE`] reads it).

use std::path::{Path, PathBuf};

use aterm_uds::host::{Lookup, PIN_STAMP, Tool};

use crate::Error;

/// The dated release the image pins (`20260926`), as the `ubuntu-root` row of
/// [`aterm_uds::host::TOOLS`] carries it.
pub const RELEASE: &str = aterm_uds::host::pinned_version("ubuntu-root");

/// The one host the pins are fetched for: the lane is Apple silicon only.
pub const PLATFORM: &str = "macos-aarch64";

/// Ubuntu's primary host for its cloud images, which serves about three months of dated
/// releases over https.
pub const PRIMARY_HOST: &str = "cloud-images.ubuntu.com";

/// Where Ubuntu moves an older dated release: it serves plain http only.
pub const ARCHIVE_HOST: &str = "cloud-images-archive.ubuntu.com";

/// One file of the release, as Ubuntu publishes it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct File {
    /// Its URL, `{v}` standing for the release (`verify provision`'s convention).
    pub url: &'static str,
    /// Its size in bytes.
    pub bytes: u64,
    /// The lowercase-hex sha256 Ubuntu's `SHA256SUMS` lists for it.
    pub sha256: &'static str,
}

impl File {
    /// The name it is kept under: its URL's last segment.
    #[must_use]
    pub fn name(&self) -> &'static str {
        self.url.rsplit('/').next().unwrap_or(self.url)
    }

    /// Its URL at `release`.
    #[must_use]
    pub fn url_at(&self, release: &str) -> String {
        self.url.replace("{v}", release)
    }
}

/// The root archive: one plain ustar member, the raw ext4 root ([`ROOT_MEMBER`]).
pub const ROOT_ARCHIVE: File = File {
    url: "https://cloud-images.ubuntu.com/releases/noble/release-{v}/ubuntu-24.04-server-cloudimg-arm64.tar.gz",
    bytes: 536_366_268,
    sha256: "1800eb56b6b839a08c020551e16fbd8c95b2772452ecce3737a9ad25fe594126",
};

/// The root archive's one member: "a raw ext4 partition" (its README), label
/// `cloudimg-rootfs`, no partition table. The base disk every layer clones.
pub const ROOT_MEMBER: &str = "noble-server-cloudimg-arm64.img";

/// [`ROOT_MEMBER`]'s size in bytes.
pub const ROOT_MEMBER_BYTES: u64 = 2_084_569_088;

/// The most the root row may unpack: twice its member, the rule `verify provision`'s shared
/// cap states, so a bump whose root grows is not refused as too large.
pub const ROOT_CAP: u64 = 4 << 30;

// A pin bump that outgrows the cap does not build: the cap stays at least twice the member.
const _: () = assert!(
    ROOT_CAP >= 2 * ROOT_MEMBER_BYTES,
    "the root row's cap is at least twice its member"
);

/// The kernel as Ubuntu ships it: gzip, which `VZLinuxBootLoader` refuses (fact 34).
pub const VMLINUZ: File = File {
    url: "https://cloud-images.ubuntu.com/releases/noble/release-{v}/unpacked/ubuntu-24.04-server-cloudimg-arm64-vmlinuz-generic",
    bytes: 18_585_914,
    sha256: "71fe6776ef591ea452661a5933e11d2044c5df6aaf76cf39b8d9655fa4e47904",
};

/// The initrd, booted as is.
pub const INITRD: File = File {
    url: "https://cloud-images.ubuntu.com/releases/noble/release-{v}/unpacked/ubuntu-24.04-server-cloudimg-arm64-initrd-generic",
    bytes: 29_286_178,
    sha256: "f77f65eccd619be537e44a7bb2664048e27f20fc8c8e7d4eeb5cf63d0c8cd213",
};

/// The sha256 of [`VMLINUZ`] gunzipped: the arm64 `Image` the VM boots.
pub const IMAGE_SHA256: &str = "e989ab36e015f982493402632ec2b01031e70fad892e182797a82b442a301914";

/// The gunzipped `Image`'s size in bytes.
pub const IMAGE_BYTES: u64 = 59_079_048;

// A gunzipped kernel is larger than its gzip: a bump that swapped the two does not build.
const _: () = assert!(
    IMAGE_BYTES > VMLINUZ.bytes,
    "the Image is the gunzipped vmlinuz"
);

/// The pinned kernel's release: its modules live in the root's `/lib/modules/<this>`, so
/// the bake holds the packages that carry them.
pub const KERNEL_RELEASE: &str = "6.8.0-142-generic";

/// The root's provider row.
#[must_use]
pub fn root_tool() -> &'static Tool {
    aterm_uds::host::tool("ubuntu-root")
}

/// The kernel's provider row (the initrd lies beside the kernel).
#[must_use]
pub fn kernel_tool() -> &'static Tool {
    aterm_uds::host::tool("ubuntu-kernel")
}

/// The base: the root member `verify provision --linux` placed and stamped for this pin,
/// of the pinned member's size ([`placed`]). Every bring-up reads it, which marks the row
/// used.
///
/// # Errors
/// [`Error::Input`]: none is placed (naming the command that places it), the placement is
/// not this pin's, `$ATERM_TEST_UBUNTU_ROOT` names a file, or the member is not the pinned
/// member's size.
pub fn base(lookup: &Lookup) -> Result<PathBuf, Error> {
    let path = placed(root_tool(), lookup, ROOT_ARCHIVE.sha256)?;
    sized(&path, ROOT_MEMBER_BYTES)?;
    Ok(path)
}

/// The placed `vmlinuz` and the initrd beside it, as `verify provision --linux` placed and
/// stamped them for this pin ([`placed`]). Every bring-up reads them, which marks the row
/// used.
///
/// # Errors
/// [`Error::Input`]: none is placed, the placement is not this pin's, the test variable
/// names a file, or either file is not its pin's size.
pub fn kernel_files(lookup: &Lookup) -> Result<(PathBuf, PathBuf), Error> {
    let vmlinuz = placed(kernel_tool(), lookup, &kernel_stamp_sha256())?;
    let initrd = vmlinuz.with_file_name(INITRD.name());
    sized(&vmlinuz, VMLINUZ.bytes)?;
    sized(&initrd, INITRD.bytes)?;
    Ok((vmlinuz, initrd))
}

/// The sha256 the kernel row's stamp records, as `verify provision` writes it for a files
/// row: the sha256 of its `SHA256SUMS` lines, `<sha256> *<name>` for [`VMLINUZ`] then
/// [`INITRD`] (`aterm-verify`'s test holds the two equal).
#[must_use]
pub fn kernel_stamp_sha256() -> String {
    let mut sums = aterm_digest::Sha256::new();
    for file in [VMLINUZ, INITRD] {
        sums.update(file.sha256.as_bytes());
        sums.update(b" *");
        sums.update(file.name().as_bytes());
        sums.update(b"\n");
    }
    crate::hex(&sums.finalize())
}

/// What `verify provision --linux` placed for `tool` in `lookup`'s tooling cache: its
/// pinned file, beside a stamp of this release, this host and `sha256`. Nothing else is
/// the pin: a file `$ATERM_TEST_<NAME>` or `$PATH` names would be labelled with a sha256
/// nobody checked, so the lane has no unpinned fallback (§3).
///
/// Finding it marks the row used ([`mark_used`]): the tooling cache's reclaim removes a
/// row idle for a month, and the lane reads these files at every bring-up, by a clone of
/// the base and a reuse of the laid kernel that change no file time.
fn placed(tool: &Tool, lookup: &Lookup, sha256: &str) -> Result<PathBuf, Error> {
    let provides = "`targo --unverified verify provision --linux`";
    if let Some(named) = &lookup.named {
        return Err(Error::Input(format!(
            "{} names {}, but the lane takes only what {provides} placed for the pin: unset it",
            tool.var(),
            Path::new(named).display()
        )));
    }
    let missing = || {
        Error::Input(format!(
            "no {} here: {}",
            tool.missing.unwrap_or(tool.name),
            tool.remedy_on(PLATFORM)
        ))
    };
    let tooling = lookup.tooling.as_deref().ok_or_else(missing)?;
    let (Some(root), Some(file)) = (tool.pinned_root(tooling), tool.placed_in(tooling)) else {
        return Err(missing());
    };
    let stamp = root.join(PIN_STAMP);
    let pinned = crate::read_small_text(&stamp, 64 * 1024)
        .and_then(|text| aterm_json::from_str::<aterm_json::Value>(&text).ok())
        .is_some_and(|s| {
            let field = |key: &str| s.get(key).and_then(aterm_json::Value::as_str);
            field("version") == Some(RELEASE)
                && field("platform") == Some(PLATFORM)
                && field("sha256") == Some(sha256)
                && s.get("adopted").and_then(aterm_json::Value::as_bool) != Some(true)
        });
    if !pinned {
        return Err(Error::Input(format!(
            "{} is not the pinned {} {RELEASE} for {PLATFORM} (its stamp names another \
             release, host or sha256 than {sha256}): {provides} replaces it",
            root.display(),
            tool.name
        )));
    }
    mark_used(&stamp);
    Ok(file)
}

/// Mark the placed row whose stamp is `stamp` used now, as `verify provision` does when it
/// finds a row in place: the reclaim's idle rule reads the stamp's time. Best effort: a
/// cache the lane cannot write is still read.
fn mark_used(stamp: &Path) {
    #[cfg(unix)]
    let open = {
        use std::os::unix::fs::OpenOptionsExt as _;
        std::fs::OpenOptions::new()
            .append(true)
            .custom_flags(libc::O_NOFOLLOW | libc::O_CLOEXEC | libc::O_NONBLOCK)
            .open(stamp)
    };
    #[cfg(not(unix))]
    let open = std::fs::symlink_metadata(stamp).and_then(|m| {
        if m.is_file() {
            std::fs::File::options().append(true).open(stamp)
        } else {
            Err(std::io::Error::from(std::io::ErrorKind::InvalidInput))
        }
    });
    if let Ok(file) = open
        && file.metadata().is_ok_and(|m| m.is_file())
    {
        let _ = file.set_modified(std::time::SystemTime::now());
    }
}

/// Refuse `path` unless it is a regular file of `bytes`.
fn sized(path: &Path, bytes: u64) -> Result<(), Error> {
    let remedy = "`targo --unverified verify provision --linux` replaces it";
    match std::fs::symlink_metadata(path) {
        Ok(m) if m.is_file() && m.len() == bytes => Ok(()),
        Ok(m) => Err(Error::Input(format!(
            "{} is {} bytes{}, not the pin's {bytes}: {remedy}",
            path.display(),
            m.len(),
            if m.is_file() {
                ""
            } else {
                " and no regular file"
            }
        ))),
        Err(e) => Err(Error::Input(format!(
            "{} cannot be read ({e}): {remedy}",
            path.display()
        ))),
    }
}

/// The words for a fetch of `url` that failed with `error` because Ubuntu moved its release
/// to [`ARCHIVE_HOST`]: the primary host redirects there, to http only, and curl refuses it
/// (`--proto-redir =https`) with `Protocol "http" disabled (in redirect)`. `None` for any
/// other failure, or a URL of another host.
#[must_use]
pub fn archived(url: &str, error: &str) -> Option<String> {
    let path = url.strip_prefix(&format!("https://{PRIMARY_HOST}/"))?;
    if !(error.contains("Protocol \"http\" disabled") || error.contains("(in redirect)")) {
        return None;
    }
    let release = path
        .split('/')
        .find(|segment| segment.starts_with("release-"))
        .unwrap_or("the pinned release");
    Some(format!(
        "Ubuntu has moved {release} off {PRIMARY_HOST}: it redirects there to {ARCHIVE_HOST}, \
         which serves plain http only, and this fetch follows https alone. Bump the image pin \
         to a newer dated release that has passed the lane, in one commit: the three sha256 \
         rows, the gunzipped Image's sha256 and size and the kernel release in \
         crates/aterm-vz/src/pins.rs, and the release of aterm_uds::host::TOOLS' ubuntu-root \
         and ubuntu-kernel rows (docs/DESIGN-vz-guest-2026-10-23.md §3). curl said: {}",
        error.trim()
    ))
}

#[cfg(test)]
#[path = "pins_tests.rs"]
mod tests;
