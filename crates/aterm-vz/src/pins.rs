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
        sums.update(format!("{} *{}\n", file.sha256, file.name()).as_bytes());
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
mod tests {
    use super::*;

    #[test]
    fn the_pins_are_one_release_of_ubuntus_files() {
        assert_eq!(RELEASE, "20260926");
        assert_eq!(aterm_uds::host::pinned_version("ubuntu-kernel"), RELEASE);
        for file in [ROOT_ARCHIVE, VMLINUZ, INITRD] {
            assert!(crate::is_hex(file.sha256, 64), "{file:?}");
            assert!(
                file.url_at(RELEASE).starts_with(&format!(
                    "https://{PRIMARY_HOST}/releases/noble/release-{RELEASE}/"
                )),
                "{file:?}"
            );
        }
        assert!(crate::is_hex(IMAGE_SHA256, 64));
        assert_eq!(root_tool().pin.map(|p| p.exe), Some(ROOT_MEMBER));
        assert_eq!(kernel_tool().pin.map(|p| p.exe), Some(VMLINUZ.name()));
        for tool in [root_tool(), kernel_tool()] {
            assert_eq!(tool.pin.map(|p| p.hosts), Some(&[PLATFORM][..]));
        }
    }

    /// The stamp `verify provision` writes for a row of this release, host and `sha256`.
    fn stamp_text(row: &str, sha256: &str) -> String {
        format!(
            "{{\"row\":\"{row}\",\"version\":\"{RELEASE}\",\"sha256\":\"{sha256}\",\
             \"anchor\":\"sha256\",\"platform\":\"{PLATFORM}\",\"placed\":1790000000}}\n"
        )
    }

    /// Both rows placed and stamped in a scratch tooling cache, their files sparse.
    fn place_both(tooling: &Path) -> (PathBuf, PathBuf) {
        let root = root_tool().pinned_root(tooling).unwrap();
        std::fs::create_dir_all(&root).unwrap();
        std::fs::File::create(root.join(ROOT_MEMBER))
            .unwrap()
            .set_len(ROOT_MEMBER_BYTES)
            .unwrap();
        std::fs::write(
            root.join(PIN_STAMP),
            stamp_text("ubuntu-root", ROOT_ARCHIVE.sha256),
        )
        .unwrap();
        let kernel = kernel_tool().pinned_root(tooling).unwrap();
        std::fs::create_dir_all(&kernel).unwrap();
        for file in [VMLINUZ, INITRD] {
            std::fs::File::create(kernel.join(file.name()))
                .unwrap()
                .set_len(file.bytes)
                .unwrap();
        }
        std::fs::write(
            kernel.join(PIN_STAMP),
            stamp_text("ubuntu-kernel", &kernel_stamp_sha256()),
        )
        .unwrap();
        (root, kernel)
    }

    fn scratch_lookup(tooling: &Path) -> Lookup {
        Lookup {
            named: None,
            tooling: Some(tooling.to_path_buf()),
            path: None,
        }
    }

    #[test]
    fn a_placed_base_and_kernel_are_found_where_the_rows_put_them() {
        let home = aterm_tempfile::tempdir().unwrap();
        let tooling = home.path().join("tooling.noindex");
        let lookup = scratch_lookup(&tooling);
        match base(&lookup) {
            Err(Error::Input(why)) => assert!(
                why.contains("`targo --unverified verify provision --linux` fetches the pinned"),
                "{why}"
            ),
            other => panic!("{other:?}"),
        }
        let (root, kernel) = place_both(&tooling);
        let member = root.join(ROOT_MEMBER);
        assert_eq!(base(&lookup), Ok(member.clone()));
        assert_eq!(
            kernel_files(&lookup),
            Ok((kernel.join(VMLINUZ.name()), kernel.join(INITRD.name())))
        );

        std::fs::remove_file(root.join(PIN_STAMP)).unwrap();
        assert!(
            base(&lookup).is_err(),
            "a tree without the row's stamp is not placed"
        );
        std::fs::write(
            root.join(PIN_STAMP),
            stamp_text("ubuntu-root", &"e".repeat(64)),
        )
        .unwrap();
        assert!(
            matches!(base(&lookup), Err(Error::Input(why)) if why.contains("is not the pinned")),
            "a stamp of another archive is not this pin's"
        );
        std::fs::write(
            root.join(PIN_STAMP),
            stamp_text("ubuntu-root", ROOT_ARCHIVE.sha256),
        )
        .unwrap();
        std::fs::File::options()
            .write(true)
            .open(&member)
            .unwrap()
            .set_len(ROOT_MEMBER_BYTES - 1)
            .unwrap();
        assert!(
            matches!(base(&lookup), Err(Error::Input(why)) if why.contains("not the pin's")),
            "the negative control: a member of another size"
        );

        std::fs::remove_file(kernel.join(INITRD.name())).unwrap();
        assert!(matches!(kernel_files(&lookup), Err(Error::Input(_))));
    }

    #[test]
    fn a_base_the_test_variable_names_is_refused_however_it_is_sized() {
        let home = aterm_tempfile::tempdir().unwrap();
        let tooling = home.path().join("tooling.noindex");
        let (root, _) = place_both(&tooling);
        let stand_in = home.path().join("not-ubuntu.img");
        std::fs::File::create(&stand_in)
            .unwrap()
            .set_len(ROOT_MEMBER_BYTES)
            .unwrap();
        let named = Lookup {
            named: Some(stand_in.clone().into_os_string()),
            ..scratch_lookup(&tooling)
        };
        match base(&named) {
            Err(Error::Input(why)) => assert!(
                why.starts_with("ATERM_TEST_UBUNTU_ROOT names ")
                    && why.contains(
                        "takes only what `targo --unverified verify provision --linux` placed"
                    ),
                "{why}"
            ),
            other => panic!("a stand-in of the member's size is no base: {other:?}"),
        }
        let on_path = Lookup {
            path: Some(home.path().as_os_str().to_owned()),
            ..scratch_lookup(&home.path().join("empty"))
        };
        std::fs::rename(&stand_in, home.path().join("ubuntu-root")).unwrap();
        assert!(
            matches!(base(&on_path), Err(Error::Input(why)) if why.starts_with("no Ubuntu")),
            "nor is a file on $PATH"
        );
        assert_eq!(
            base(&scratch_lookup(&tooling)),
            Ok(root.join(ROOT_MEMBER)),
            "the negative control: the placed row is the base"
        );
    }

    /// Every file and folder of `dir` and `dir` itself last touched at `at`.
    #[cfg(unix)]
    fn age(dir: &Path, at: std::time::SystemTime) {
        let times = std::fs::FileTimes::new().set_accessed(at).set_modified(at);
        for entry in std::fs::read_dir(dir).unwrap().flatten() {
            std::fs::File::open(entry.path())
                .unwrap()
                .set_times(times)
                .unwrap();
        }
        std::fs::File::open(dir).unwrap().set_times(times).unwrap();
    }

    /// The tooling reclaim keeps a row a bring-up read, though reading it changes no file's
    /// time. The negative control: the row not read yet is still listed.
    #[cfg(unix)]
    #[test]
    fn a_bring_up_marks_its_rows_used_so_the_tooling_reclaim_keeps_them() {
        let home = aterm_tempfile::tempdir().unwrap();
        let tooling = home.path().join("tooling.noindex");
        let (root, kernel) = place_both(&tooling);
        let now = std::time::SystemTime::now();
        let month_ago =
            now - atpkg::build_cache::RECLAIM_IDLE - std::time::Duration::from_secs(86_400);
        age(&root, month_ago);
        age(&kernel, month_ago);
        let listed = || -> Vec<PathBuf> {
            atpkg::build_cache::tooling::plan(&tooling, now)
                .into_iter()
                .map(|p| p.path)
                .collect()
        };
        assert_eq!(
            listed(),
            [kernel.clone(), root.clone()],
            "both idle a month"
        );

        let lookup = scratch_lookup(&tooling);
        base(&lookup).unwrap();
        assert_eq!(
            listed(),
            std::slice::from_ref(&kernel),
            "the base was read; the kernel row was not, and is still listed"
        );
        kernel_files(&lookup).unwrap();
        assert_eq!(listed(), Vec::<PathBuf>::new(), "both were read");
    }

    #[test]
    fn an_archived_release_names_the_archive_host_and_the_bump() {
        let url = ROOT_ARCHIVE.url_at(RELEASE);
        let curl =
            "/usr/bin/curl exited Some(1): curl: (1) Protocol \"http\" disabled (in redirect)";
        let words = archived(&url, curl).expect("the archive's redirect");
        for needle in [
            "release-20260926",
            ARCHIVE_HOST,
            "plain http only",
            "Bump the image pin",
            "crates/aterm-vz/src/pins.rs",
            curl,
        ] {
            assert!(words.contains(needle), "{needle}: {words}");
        }
        assert_eq!(
            archived(
                &url,
                "/usr/bin/curl exited Some(6): curl: (6) Could not resolve host"
            ),
            None,
            "the negative control: another failure is curl's own"
        );
        assert_eq!(
            archived("https://github.com/x/y.tar.gz", curl),
            None,
            "another host's redirect is not Ubuntu's archive"
        );
    }
}
