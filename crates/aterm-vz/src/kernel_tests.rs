// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

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

/// NEGATIVE CONTROL: `lay` refuses symlinked kernel roots and symlinked input files without
/// following them, and replaces a symlinked key directory instead of trusting files behind it.
#[cfg(unix)]
#[test]
fn lay_refuses_symlinked_roots_and_inputs_and_replaces_symlinked_key_dir() {
    let scratch = aterm_tempfile::tempdir().unwrap();
    let p = Pinned::new(scratch.path());
    let want = p.want();

    let outside = scratch.path().join("outside");
    std::fs::create_dir_all(&outside).unwrap();
    std::fs::write(outside.join("keep"), b"keep").unwrap();
    let linked_root = scratch.path().join("linked-kernel");
    std::os::unix::fs::symlink(&outside, &linked_root).unwrap();
    assert!(matches!(
        lay(&linked_root, &p.vmlinuz, &p.initrd, &want),
        Err(Error::Io(_))
    ));
    assert_eq!(names(&outside), ["keep"]);

    let linked_vmlinuz = scratch.path().join("vmlinuz-link");
    std::os::unix::fs::symlink(&p.vmlinuz, &linked_vmlinuz).unwrap();
    let real_dir = scratch.path().join("real-kernel");
    assert!(matches!(
        lay(&real_dir, &linked_vmlinuz, &p.initrd, &want),
        Err(Error::Input(_))
    ));

    // Plant a symlink at `<real_dir>/<key>` pointing at `outside` populated with pin-sized files:
    // `whole` must reject the symlinked parent and `lay` must replace the symlink cleanly.
    std::fs::create_dir_all(&real_dir).unwrap();
    std::fs::write(outside.join(IMAGE), &p.kernel).unwrap();
    std::fs::write(outside.join(INITRD), &p.initrd_bytes).unwrap();
    std::os::unix::fs::symlink(&outside, real_dir.join(want.key())).unwrap();
    let laid = lay(&real_dir, &p.vmlinuz, &p.initrd, &want).unwrap();
    assert!(
        std::fs::symlink_metadata(real_dir.join(want.key()))
            .unwrap()
            .file_type()
            .is_dir()
    );
    assert_eq!(std::fs::read(&laid.image).unwrap(), p.kernel);
    assert_eq!(std::fs::read(outside.join("keep")).unwrap(), b"keep");
}
