// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The pinned kernel as the VM boots it (`docs/DESIGN-vz-guest-2026-10-23.md` §3, §4).
//!
//! `VZLinuxBootLoader` refuses Ubuntu's gzip `vmlinuz` and boots the same kernel
//! gunzipped (fact 34). The gunzip is aterm-codec's: the result is held to no more than
//! the recorded size, the arm64 `Image` magic, and the recorded sha256. The kernel and the
//! initrd are laid together under `kernel/<key>/`, whole or not at all, and never written
//! again once whole.

use std::io::{Read, Write};
use std::path::{Path, PathBuf};

use crate::{Error, io, pins};

/// Where the arm64 `Image` header carries its magic (Linux's
/// `Documentation/arch/arm64/booting.rst`: `magic`, at byte 56).
pub const MAGIC_AT: usize = 56;

/// The arm64 `Image` magic, `0x644d5241` little-endian.
pub const MAGIC: [u8; 4] = *b"ARMd";

/// The gunzipped kernel's name in the kernel's directory.
pub const IMAGE: &str = "Image";
/// The initrd's name in the kernel's directory.
pub const INITRD: &str = "initrd";

/// A file's size and sha256, which a laid file is held to.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Expect<'a> {
    /// The lowercase-hex sha256.
    pub sha256: &'a str,
    /// The size in bytes.
    pub bytes: u64,
}

/// The pins a kernel directory is laid from: the gunzipped `Image` and the initrd.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Want<'a> {
    /// What the gunzipped `vmlinuz` must be.
    pub image: Expect<'a>,
    /// What the initrd must be.
    pub initrd: Expect<'a>,
}

impl Want<'static> {
    /// The tree's pins ([`pins::IMAGE_SHA256`], [`pins::INITRD`]).
    pub const PINNED: Self = Self {
        image: Expect {
            sha256: pins::IMAGE_SHA256,
            bytes: pins::IMAGE_BYTES,
        },
        initrd: Expect {
            sha256: pins::INITRD.sha256,
            bytes: pins::INITRD.bytes,
        },
    };
}

impl Want<'_> {
    /// The kernel directory's name: both files' content addresses, so a pin bump lays a
    /// new directory and never rewrites one a booted VM read.
    #[must_use]
    pub fn key(&self) -> String {
        let short = |s: &str| s.get(..16).unwrap_or(s).to_string();
        format!("{}-{}", short(self.image.sha256), short(self.initrd.sha256))
    }
}

/// The kernel the VM boots: `VZLinuxBootLoader`'s `kernelURL` and `initialRamdiskURL`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Kernel {
    /// The gunzipped arm64 `Image`.
    pub image: PathBuf,
    /// The initrd.
    pub initrd: PathBuf,
}

/// Gunzip `gz` into `out` in house, holding what comes out to `expect`: never more than
/// its bytes, the arm64 magic at [`MAGIC_AT`], and its sha256. The bytes written, flushed.
///
/// # Errors
/// [`Error::Input`]: the stream is not whole gzip (aterm-codec checks every member's CRC
/// and length), it inflates past `expect.bytes`, it is no arm64 `Image`, or its size or
/// sha256 is not `expect`'s. [`Error::Io`]: `out` refused a write or the flush (a full
/// volume). `out` then holds a partial write its caller discards.
pub fn gunzip_image(gz: impl Read, mut out: impl Write, expect: &Expect<'_>) -> Result<u64, Error> {
    let written_out =
        |e: std::io::Error| Error::Io(format!("could not write the gunzipped kernel: {e}"));
    let mut reader =
        aterm_codec::inflate::stream::GzipReader::new(gz).take(expect.bytes.saturating_add(1));
    let mut hash = aterm_digest::Sha256::new();
    let mut head = [0u8; MAGIC_AT + MAGIC.len()];
    let mut written: u64 = 0;
    let mut buf = vec![0u8; 1 << 16];
    loop {
        let n = match reader.read(&mut buf) {
            Ok(0) => break,
            Ok(n) => n,
            Err(e) if e.kind() == std::io::ErrorKind::Interrupted => continue,
            Err(e) => return Err(Error::Input(format!("the kernel does not gunzip: {e}"))),
        };
        let chunk = &buf[..n];
        let at = usize::try_from(written).unwrap_or(usize::MAX);
        if at < head.len() {
            let take = (head.len() - at).min(n);
            head[at..at + take].copy_from_slice(&chunk[..take]);
        }
        written += n as u64;
        if written > expect.bytes {
            return Err(Error::Input(format!(
                "the kernel inflates past the pinned Image's {} bytes: refused",
                expect.bytes
            )));
        }
        hash.update(chunk);
        out.write_all(chunk).map_err(written_out)?;
    }
    if written < head.len() as u64 || head[MAGIC_AT..] != MAGIC {
        return Err(Error::Input(format!(
            "the gunzipped kernel is no arm64 Image: no `ARMd` magic at byte {MAGIC_AT}"
        )));
    }
    if written != expect.bytes {
        return Err(Error::Input(format!(
            "the gunzipped kernel is {written} bytes, the pinned Image {}",
            expect.bytes
        )));
    }
    let got = crate::hex(&hash.finalize());
    if got != expect.sha256 {
        return Err(Error::Input(format!(
            "the gunzipped kernel hashes to {got}, the pinned Image to {}",
            expect.sha256
        )));
    }
    out.flush().map_err(written_out)?;
    Ok(written)
}

/// Copy `from` into `out`, holding it to `expect`'s size and sha256, and flush `out`.
///
/// # Errors
/// [`Error::Input`]: `from` cannot be read, or is not `expect`'s. [`Error::Io`]: `out`
/// refused a write or the flush.
fn copy_checked(from: &Path, mut out: impl Write, expect: &Expect<'_>) -> Result<(), Error> {
    let file = std::fs::File::open(from)
        .map_err(|e| Error::Input(format!("could not open {}: {e}", from.display())))?;
    let mut reader = file.take(expect.bytes.saturating_add(1));
    let mut hash = aterm_digest::Sha256::new();
    let mut buf = vec![0u8; 1 << 16];
    let mut copied: u64 = 0;
    let written_out =
        |e: std::io::Error| Error::Io(format!("could not copy {}: {e}", from.display()));
    loop {
        let n = match reader.read(&mut buf) {
            Ok(0) => break,
            Ok(n) => n,
            Err(e) if e.kind() == std::io::ErrorKind::Interrupted => continue,
            Err(e) => {
                return Err(Error::Input(format!(
                    "could not read {}: {e}",
                    from.display()
                )));
            }
        };
        copied += n as u64;
        hash.update(&buf[..n]);
        out.write_all(&buf[..n]).map_err(written_out)?;
    }
    let got = crate::hex(&hash.finalize());
    if copied != expect.bytes || got != expect.sha256 {
        return Err(Error::Input(format!(
            "{} is {copied} bytes hashing to {got}; the pin is {} bytes, {}",
            from.display(),
            expect.bytes,
            expect.sha256
        )));
    }
    out.flush().map_err(written_out)
}

/// Whether `kernel` lies whole for `want`: both regular files (no link followed) of their
/// pins' sizes. A lay makes both durable before its one rename, so a file under the key
/// is never a crash's truncation; their sha256 was held when they were laid, and they are
/// 0444.
fn whole(kernel: &Kernel, want: &Want<'_>) -> bool {
    let sized = |path: &Path, bytes: u64| {
        std::fs::symlink_metadata(path).is_ok_and(|m| m.is_file() && m.len() == bytes)
    };
    sized(&kernel.image, want.image.bytes) && sized(&kernel.initrd, want.initrd.bytes)
}

/// Write `path` through `write`, then flush it to stable media.
fn write_durably(
    path: &Path,
    write: impl FnOnce(&mut std::io::BufWriter<std::fs::File>) -> Result<(), Error>,
) -> Result<(), Error> {
    let file = std::fs::File::create(path).map_err(|e| io("create", path, &e))?;
    let mut out = std::io::BufWriter::new(file);
    write(&mut out).map_err(|e| match e {
        Error::Io(why) => Error::Io(format!("{}: {why}", path.display())),
        other => other,
    })?;
    let file = out.into_inner().map_err(|e| io("write", path, e.error()))?;
    aterm_durable::durable(&file).map_err(|e| io("flush", path, &e))
}

/// The kernel laid under `dir` (`vz.noindex/kernel`) for `want`, laid first where it is not
/// there whole ([`whole`]): `vmlinuz` gunzipped to [`IMAGE`] and `initrd` copied to
/// [`INITRD`], each held to `want`, made durable and 0444 in a scratch directory renamed
/// into place. A key directory that is not whole is replaced. Every other kernel directory
/// (`<16 hex>-<16 hex>`) and unfinished scratch (`.partial-*`) under `dir` is removed, as the
/// framework reads the kernel only when a VM starts; nothing else under `dir` is touched.
///
/// # Errors
/// [`Error::Input`] when a file is missing or is not its pin's (nothing is laid),
/// [`Error::Io`] when the directory cannot be written (a full volume among them).
pub fn lay(dir: &Path, vmlinuz: &Path, initrd: &Path, want: &Want<'_>) -> Result<Kernel, Error> {
    let dest = dir.join(want.key());
    let kernel = Kernel {
        image: dest.join(IMAGE),
        initrd: dest.join(INITRD),
    };
    if !whole(&kernel, want) {
        std::fs::create_dir_all(dir).map_err(|e| io("create", dir, &e))?;
        let scratch = aterm_tempfile::Builder::new()
            .prefix(SCRATCH)
            .tempdir_in(dir)
            .map_err(|e| io("create a scratch in", dir, &e))?;
        let gz = std::fs::File::open(vmlinuz)
            .map_err(|e| Error::Input(format!("{}: {e}", vmlinuz.display())))?;
        let image = scratch.path().join(IMAGE);
        write_durably(&image, |out| {
            gunzip_image(std::io::BufReader::new(gz), out, &want.image)
                .map(|_| ())
                .map_err(|e| match e {
                    Error::Input(why) => Error::Input(format!("{}: {why}", vmlinuz.display())),
                    other => other,
                })
        })?;
        let copy = scratch.path().join(INITRD);
        write_durably(&copy, |out| copy_checked(initrd, out, &want.initrd))?;
        crate::read_only(&image)?;
        crate::read_only(&copy)?;
        aterm_durable::durable_dir(scratch.path()).map_err(|e| io("flush", scratch.path(), &e))?;
        if std::fs::symlink_metadata(&dest).is_ok() {
            remove(&dest).map_err(|e| io("replace the kernel that is not whole at", &dest, &e))?;
        }
        if let Err(e) = std::fs::rename(scratch.path(), &dest)
            && !whole(&kernel, want)
        {
            return Err(io("place", &dest, &e));
        }
        aterm_durable::durable_dir(dir).map_err(|e| io("flush", dir, &e))?;
    }
    reclaim(dir, &want.key());
    Ok(kernel)
}

/// The prefix of a lay's scratch directory under the kernel directory.
const SCRATCH: &str = ".partial-";

/// Whether `name` is a kernel directory's: `<16 hex>-<16 hex>` ([`Want::key`]).
fn is_key(name: &str) -> bool {
    name.split_once('-')
        .is_some_and(|(image, initrd)| crate::is_hex(image, 16) && crate::is_hex(initrd, 16))
}

/// Remove `path`, a directory with what it holds or any other entry, never following a link.
fn remove(path: &Path) -> std::io::Result<()> {
    if std::fs::symlink_metadata(path)?.is_dir() {
        std::fs::remove_dir_all(path)
    } else {
        std::fs::remove_file(path)
    }
}

/// Remove every kernel directory and lay scratch under `dir` but `keep`.
fn reclaim(dir: &Path, keep: &str) {
    for entry in std::fs::read_dir(dir).into_iter().flatten().flatten() {
        let name = entry.file_name();
        let Some(name) = name.to_str() else { continue };
        if name != keep
            && (is_key(name) || name.starts_with(SCRATCH))
            && entry.file_type().is_ok_and(|t| t.is_dir())
        {
            let _ = std::fs::remove_dir_all(entry.path());
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A gzip member of `data` in stored blocks, as a fixture needs no deflater.
    pub(crate) fn gzip(data: &[u8]) -> Vec<u8> {
        let mut out = vec![0x1f, 0x8b, 8, 0, 0, 0, 0, 0, 0, 255];
        let mut rest = data;
        loop {
            let (block, tail) = rest.split_at(rest.len().min(0xFFFF));
            out.push(u8::from(tail.is_empty()));
            let len = u16::try_from(block.len()).unwrap();
            out.extend_from_slice(&len.to_le_bytes());
            out.extend_from_slice(&(!len).to_le_bytes());
            out.extend_from_slice(block);
            if tail.is_empty() {
                break;
            }
            rest = tail;
        }
        out.extend_from_slice(&aterm_codec::crc32::Crc32::of(data).to_le_bytes());
        out.extend_from_slice(&u32::try_from(data.len()).unwrap().to_le_bytes());
        out
    }

    /// A stand-in arm64 Image: a 64-byte header with the magic at 56, then a body.
    fn image(magic: [u8; 4], body: usize) -> Vec<u8> {
        let mut out = vec![0u8; 64];
        out[MAGIC_AT..MAGIC_AT + 4].copy_from_slice(&magic);
        out.extend((0..body).map(|i| (i % 251) as u8));
        out
    }

    fn sha256(bytes: &[u8]) -> String {
        let mut h = aterm_digest::Sha256::new();
        h.update(bytes);
        crate::hex(&h.finalize())
    }

    /// The words of an [`Error::Input`], or a panic naming what came instead.
    fn input<T: std::fmt::Debug>(got: Result<T, Error>) -> String {
        match got {
            Err(Error::Input(why)) => why,
            other => panic!("not a refused input: {other:?}"),
        }
    }

    /// A writer that takes `room` bytes and then refuses every write and every flush, as a
    /// full volume does.
    struct Full {
        room: usize,
    }

    impl Write for Full {
        fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
            if self.room == 0 {
                return Err(std::io::Error::other("No space left on device"));
            }
            let n = buf.len().min(self.room);
            self.room -= n;
            Ok(n)
        }

        fn flush(&mut self) -> std::io::Result<()> {
            if self.room == 0 {
                Err(std::io::Error::other("No space left on device"))
            } else {
                Ok(())
            }
        }
    }

    #[test]
    fn a_gunzipped_kernel_is_held_to_its_magic_size_and_hash() {
        let kernel = image(MAGIC, 200_000);
        let sum = sha256(&kernel);
        let want = Expect {
            sha256: &sum,
            bytes: kernel.len() as u64,
        };
        let mut out = Vec::new();
        assert_eq!(
            gunzip_image(&gzip(&kernel)[..], &mut out, &want),
            Ok(kernel.len() as u64)
        );
        assert_eq!(out, kernel);

        let not_arm = image(*b"MZ\0\0", 10);
        let sum = sha256(&not_arm);
        let why = input(gunzip_image(
            &gzip(&not_arm)[..],
            Vec::new(),
            &Expect {
                sha256: &sum,
                bytes: not_arm.len() as u64,
            },
        ));
        assert!(why.contains("no arm64 Image"), "{why}");

        let wrong = Expect {
            sha256: &"0".repeat(64),
            ..want
        };
        let why = input(gunzip_image(&gzip(&kernel)[..], Vec::new(), &wrong));
        assert!(why.contains("hashes to"), "{why}");

        let small = Expect {
            bytes: kernel.len() as u64 - 1,
            ..want
        };
        let why = input(gunzip_image(&gzip(&kernel)[..], Vec::new(), &small));
        assert!(
            why.contains("inflates past"),
            "a bomb stops at the cap: {why}"
        );

        let mut cut = gzip(&kernel);
        cut.truncate(cut.len() - 6);
        input(gunzip_image(&cut[..], Vec::new(), &want));
        input(gunzip_image(&kernel[..], Vec::new(), &want));
    }

    /// A volume that fills under the gunzip or the initrd's copy (its last buffered tail
    /// included) is a failed write, never a refused pin; with room the same writes succeed.
    #[test]
    fn a_full_volume_is_a_failed_write_and_never_a_bad_input() {
        let kernel = image(MAGIC, 200_000);
        let sum = sha256(&kernel);
        let want = Expect {
            sha256: &sum,
            bytes: kernel.len() as u64,
        };
        match gunzip_image(&gzip(&kernel)[..], Full { room: 4096 }, &want) {
            Err(Error::Io(why)) => assert!(why.contains("No space left on device"), "{why}"),
            other => panic!("a full volume under the gunzip: {other:?}"),
        }
        assert_eq!(
            gunzip_image(&gzip(&kernel)[..], Full { room: usize::MAX }, &want),
            Ok(kernel.len() as u64)
        );

        let scratch = aterm_tempfile::tempdir().unwrap();
        let initrd = scratch.path().join("initrd");
        std::fs::write(&initrd, b"an initrd").unwrap();
        let sum = sha256(b"an initrd");
        let want = Expect {
            sha256: &sum,
            bytes: 9,
        };
        match copy_checked(&initrd, std::io::BufWriter::new(Full { room: 0 }), &want) {
            Err(Error::Io(why)) => assert!(why.contains("No space left on device"), "{why}"),
            other => panic!("a tail the final flush could not write: {other:?}"),
        }
        assert_eq!(
            copy_checked(&initrd, std::io::BufWriter::new(Full { room: 64 }), &want),
            Ok(())
        );
    }

    /// The pinned files under `scratch/pinned`, and the [`Want`] they are.
    struct Pinned {
        vmlinuz: PathBuf,
        initrd: PathBuf,
        kernel: Vec<u8>,
        initrd_bytes: Vec<u8>,
        sums: (String, String),
    }

    impl Pinned {
        fn new(scratch: &Path) -> Self {
            let pinned = scratch.join("pinned");
            std::fs::create_dir_all(&pinned).unwrap();
            let kernel = image(MAGIC, 70_000);
            let initrd_bytes = b"an initrd".to_vec();
            std::fs::write(pinned.join("vmlinuz"), gzip(&kernel)).unwrap();
            std::fs::write(pinned.join("initrd"), &initrd_bytes).unwrap();
            Self {
                vmlinuz: pinned.join("vmlinuz"),
                initrd: pinned.join("initrd"),
                sums: (sha256(&kernel), sha256(&initrd_bytes)),
                kernel,
                initrd_bytes,
            }
        }

        fn want(&self) -> Want<'_> {
            Want {
                image: Expect {
                    sha256: &self.sums.0,
                    bytes: self.kernel.len() as u64,
                },
                initrd: Expect {
                    sha256: &self.sums.1,
                    bytes: self.initrd_bytes.len() as u64,
                },
            }
        }
    }

    fn names(dir: &Path) -> Vec<String> {
        let mut v: Vec<String> = std::fs::read_dir(dir)
            .unwrap()
            .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
            .collect();
        v.sort();
        v
    }

    #[test]
    fn a_kernel_directory_is_laid_whole_once_and_an_older_one_goes() {
        let scratch = aterm_tempfile::tempdir().unwrap();
        let p = Pinned::new(scratch.path());
        let want = p.want();
        let dir = scratch.path().join("kernel");
        std::fs::create_dir_all(dir.join("0123456789abcdef-0123456789abcdef")).unwrap();
        std::fs::create_dir_all(dir.join(".partial-cut")).unwrap();
        let laid = lay(&dir, &p.vmlinuz, &p.initrd, &want).unwrap();
        assert_eq!(laid.image, dir.join(want.key()).join(IMAGE));
        assert_eq!(std::fs::read(&laid.image).unwrap(), p.kernel);
        assert_eq!(std::fs::read(&laid.initrd).unwrap(), p.initrd_bytes);
        assert!(
            std::fs::metadata(&laid.image)
                .unwrap()
                .permissions()
                .readonly()
        );
        assert_eq!(
            names(&dir),
            [want.key()],
            "the older pin's directory and a cut lay's scratch are reclaimed"
        );

        std::fs::remove_file(&p.vmlinuz).unwrap();
        assert_eq!(
            lay(&dir, &p.vmlinuz, &p.initrd, &want),
            Ok(laid),
            "a laid kernel is read where it lies"
        );

        std::fs::write(&p.vmlinuz, gzip(&p.kernel)).unwrap();
        std::fs::write(&p.initrd, b"another initrd").unwrap();
        let other_sum = "1".repeat(64);
        let bumped = Want {
            initrd: Expect {
                sha256: &other_sum,
                bytes: p.initrd_bytes.len() as u64,
            },
            ..want
        };
        let why = input(lay(&dir, &p.vmlinuz, &p.initrd, &bumped));
        assert!(why.contains("the pin is"), "{why}");
        assert_eq!(names(&dir).len(), 1, "a refused lay leaves no scratch");
    }

    /// An initrd of the pin's size but other bytes is refused by its sha256 alone.
    #[test]
    fn an_initrd_of_the_pins_size_is_held_to_its_hash() {
        let scratch = aterm_tempfile::tempdir().unwrap();
        let p = Pinned::new(scratch.path());
        let want = p.want();
        std::fs::write(&p.initrd, b"an initrz").unwrap();
        let dir = scratch.path().join("kernel");
        let why = input(lay(&dir, &p.vmlinuz, &p.initrd, &want));
        assert!(
            why.contains("hashing to") && why.contains("the pin is 9 bytes"),
            "{why}"
        );
        assert_eq!(names(&dir), Vec::<String>::new(), "nothing was laid");
        std::fs::write(&p.initrd, b"an initrd").unwrap();
        assert!(
            lay(&dir, &p.vmlinuz, &p.initrd, &want).is_ok(),
            "the negative control"
        );
    }

    /// A key directory that is not whole (an emptied file, a lost one) is laid again from
    /// the pinned files; a whole one is kept, its inode unchanged.
    #[test]
    fn a_kernel_directory_that_is_not_whole_is_laid_again() {
        use std::os::unix::fs::MetadataExt as _;
        let scratch = aterm_tempfile::tempdir().unwrap();
        let p = Pinned::new(scratch.path());
        let want = p.want();
        let dir = scratch.path().join("kernel");
        let key = dir.join(want.key());
        std::fs::create_dir_all(&key).unwrap();
        std::fs::write(key.join(IMAGE), b"").unwrap();
        std::fs::write(key.join(INITRD), b"").unwrap();
        let laid = lay(&dir, &p.vmlinuz, &p.initrd, &want).unwrap();
        assert_eq!(std::fs::read(&laid.image).unwrap(), p.kernel, "empty files");
        assert_eq!(std::fs::read(&laid.initrd).unwrap(), p.initrd_bytes);

        let inode = std::fs::metadata(&laid.image).unwrap().ino();
        assert_eq!(lay(&dir, &p.vmlinuz, &p.initrd, &want), Ok(laid.clone()));
        assert_eq!(
            std::fs::metadata(&laid.image).unwrap().ino(),
            inode,
            "a whole kernel is not laid again"
        );

        std::fs::remove_file(&laid.initrd).unwrap();
        assert_eq!(
            lay(&dir, &p.vmlinuz, &p.initrd, &want),
            Ok(laid.clone()),
            "a key directory missing a file"
        );
        assert_eq!(std::fs::read(&laid.initrd).unwrap(), p.initrd_bytes);
    }

    /// The reclaim takes only kernel directories and lay scratches: a caller that passes
    /// the wrong directory loses nothing else.
    #[test]
    fn a_lay_removes_only_kernel_directories_and_its_scratches() {
        let scratch = aterm_tempfile::tempdir().unwrap();
        let p = Pinned::new(scratch.path());
        let want = p.want();
        let dir = scratch.path().join("vz.noindex");
        for name in [
            "baked",
            "live",
            "seed",
            "0123456789abcdef",
            "0123456789abcdeg-0123456789abcdef",
        ] {
            std::fs::create_dir_all(dir.join(name)).unwrap();
        }
        std::fs::create_dir_all(dir.join("fedcba9876543210-fedcba9876543210")).unwrap();
        lay(&dir, &p.vmlinuz, &p.initrd, &want).unwrap();
        let mut kept = vec![
            "0123456789abcdef".to_string(),
            "0123456789abcdeg-0123456789abcdef".into(),
            "baked".into(),
            "live".into(),
            "seed".into(),
            want.key(),
        ];
        kept.sort();
        assert_eq!(names(&dir), kept, "only the older key directory went");
        assert!(is_key(&want.key()) && !is_key("baked"));
    }

    #[test]
    fn the_pinned_kernels_directory_is_named_by_both_hashes() {
        assert_eq!(
            Want::PINNED.key(),
            format!(
                "{}-{}",
                &pins::IMAGE_SHA256[..16],
                &pins::INITRD.sha256[..16]
            )
        );
    }
}
