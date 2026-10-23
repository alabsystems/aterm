// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The disk layers over a scratch directory, with an injected free-space reader
//! (`docs/DESIGN-vz-guest-2026-10-23.md` §13).

use std::cell::{Cell, RefCell};
use std::os::unix::fs::PermissionsExt as _;

use super::*;
use crate::seed::User;
use crate::space::BRING_UP_FLOOR;

/// The tests' disks: 1 MiB, sparse.
const DISK: u64 = 1 << 20;

/// A host whose volume has `free` bytes, plus what each file in `holds` frees once it is
/// gone; whose clones are copies it records; and whose ISO is the seed's files concatenated.
struct Fake {
    free: Cell<u64>,
    holds: RefCell<Vec<(PathBuf, u64)>>,
    clones: RefCell<Vec<(PathBuf, PathBuf)>>,
    isos: RefCell<Vec<PathBuf>>,
}

impl Fake {
    fn new() -> Self {
        Self {
            free: Cell::new(100 << 30),
            holds: RefCell::default(),
            clones: RefCell::default(),
            isos: RefCell::default(),
        }
    }

    /// `path` stands for a file of `bytes` on the volume: its removal frees them.
    fn holding(&self, path: &Path, bytes: u64) {
        std::fs::write(path, b"a big file").unwrap();
        self.holds.borrow_mut().push((path.to_path_buf(), bytes));
    }

    fn cloned_from(&self) -> Vec<PathBuf> {
        self.clones
            .borrow()
            .iter()
            .map(|(src, _)| src.clone())
            .collect()
    }
}

impl Host for Fake {
    fn free(&self, _path: &Path) -> Result<Free, String> {
        let freed: u64 = self
            .holds
            .borrow()
            .iter()
            .filter(|(path, _)| std::fs::symlink_metadata(path).is_err())
            .map(|(_, bytes)| bytes)
            .sum();
        Ok(Free {
            bytes: self.free.get() + freed,
            volume: "/scratch (container:disk9)".into(),
        })
    }

    fn clone_file(&self, src: &Path, dst: &Path) -> io::Result<()> {
        if std::fs::symlink_metadata(dst).is_ok() {
            return Err(io::Error::from(io::ErrorKind::AlreadyExists));
        }
        self.clones
            .borrow_mut()
            .push((src.to_path_buf(), dst.to_path_buf()));
        std::fs::copy(src, dst).map(|_| ())
    }

    fn make_iso(&self, files: &Path, iso: &Path) -> Result<(), String> {
        self.isos.borrow_mut().push(iso.to_path_buf());
        let mut all = Vec::new();
        for name in ["user-data", "meta-data", "network-config"] {
            all.extend(std::fs::read(files.join(name)).map_err(|e| e.to_string())?);
        }
        std::fs::write(iso, all).map_err(|e| e.to_string())
    }
}

/// A scratch state directory and a read-only base beside it.
struct Scratch {
    _dir: aterm_tempfile::TempDir,
    layers: Layers,
    base: Base,
}

const SEED: &str = "0123456789abcdef";

fn scratch() -> Scratch {
    let dir = aterm_tempfile::tempdir().unwrap();
    let base = dir
        .path()
        .join("tooling/ubuntu-noble-root-20260926/base.img");
    std::fs::create_dir_all(base.parent().unwrap()).unwrap();
    std::fs::write(&base, b"the root's ext4").unwrap();
    std::fs::set_permissions(&base, std::fs::Permissions::from_mode(0o444)).unwrap();
    Scratch {
        layers: Layers::at(dir.path().join("vz.noindex")).with_disk_bytes(DISK),
        base: Base {
            path: base,
            sha256: "a".repeat(64),
        },
        _dir: dir,
    }
}

fn mode(path: &Path) -> u32 {
    std::fs::metadata(path).unwrap().permissions().mode() & 0o777
}

fn names(dir: &Path) -> Vec<String> {
    let mut v: Vec<String> = std::fs::read_dir(dir)
        .unwrap()
        .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
        .collect();
    v.sort();
    v
}

/// Bring up, bake if asked (the guest writes `provisioned` at the disk's start), seal, and
/// bring up again: the live disk.
fn bake_and_bring_up(s: &Scratch, host: &Fake, base: &Base) -> PathBuf {
    match s.layers.prepare(host, base, SEED, &mut |_| {}).unwrap() {
        Prepared::Bake(bake) => {
            use std::io::{Seek as _, Write as _};
            let mut disk = std::fs::File::options()
                .write(true)
                .open(bake.path())
                .unwrap();
            disk.seek(std::io::SeekFrom::Start(0)).unwrap();
            disk.write_all(b"provisioned").unwrap();
            drop(disk);
            bake.seal().unwrap();
        }
        other => panic!("a first bring-up bakes: {other:?}"),
    }
    match s.layers.prepare(host, base, SEED, &mut |_| {}).unwrap() {
        Prepared::Live {
            disk,
            cloned: Some(_),
        } => disk,
        other => panic!("a bring-up after the seal clones live: {other:?}"),
    }
}

#[test]
fn a_first_bring_up_bakes_a_partial_clone_then_clones_the_sealed_disk_to_live() {
    let s = scratch();
    let host = Fake::new();
    let bake = match s.layers.prepare(&host, &s.base, SEED, &mut |_| {}).unwrap() {
        Prepared::Bake(bake) => bake,
        other => panic!("{other:?}"),
    };
    let want = baked_name(&s.base.sha256, SEED);
    assert_eq!(bake.sealed_path(), s.layers.baked_dir().join(&want));
    assert_eq!(
        bake.path(),
        s.layers
            .baked_dir()
            .join(format!("{}-{SEED}.partial.img", s.base.sha256))
    );
    let meta = std::fs::metadata(bake.path()).unwrap();
    assert_eq!(meta.len(), DISK, "grown to the disk's size");
    assert_eq!(mode(bake.path()), 0o644, "a bake writes its partial");
    assert_eq!(
        std::fs::read(bake.path()).unwrap()[..15],
        *b"the root's ext4"
    );
    assert_eq!(host.cloned_from(), std::slice::from_ref(&s.base.path));
    assert_eq!(mode(&s.base.path), 0o444, "base stays read-only");
    assert_eq!(
        std::fs::metadata(&s.base.path).unwrap().len(),
        15,
        "and is never grown"
    );

    let sealed = bake.seal().unwrap();
    assert_eq!(mode(&sealed), 0o444, "a sealed disk is never written again");
    assert_eq!(names(&s.layers.baked_dir()), std::slice::from_ref(&want));

    let mut said = Vec::new();
    let next = s
        .layers
        .prepare(&host, &s.base, SEED, &mut |l| said.push(l.to_string()))
        .unwrap();
    assert_eq!(
        next,
        Prepared::Live {
            disk: s.layers.live_disk(),
            cloned: Some(Reclone::NoLiveDisk)
        }
    );
    assert_eq!(host.cloned_from(), [s.base.path.clone(), sealed.clone()]);
    assert_eq!(
        mode(&s.layers.live_disk()),
        0o644,
        "the live disk is the lane's"
    );
    assert_eq!(mode(&sealed), 0o444);
    let record = s.layers.record().unwrap();
    assert_eq!(
        (
            record.baked.as_str(),
            record.base_sha256.as_str(),
            record.seed.as_str()
        ),
        (want.as_str(), s.base.sha256.as_str(), SEED)
    );
    assert_eq!(
        names(&s.layers.live_dir()),
        ["aterm-linux.img", "aterm-linux.toml"]
    );
    assert!(
        said.iter().any(|l| l.contains("there was no live disk")),
        "{said:?}"
    );
}

#[test]
fn a_kept_live_disk_is_provisioned_in_place_and_a_base_change_reclones_it() {
    let s = scratch();
    let host = Fake::new();
    let live = bake_and_bring_up(&s, &host, &s.base);
    std::fs::write(&live, b"the guest's caches").unwrap();
    let clones = host.clones.borrow().len();

    for _ in 0..2 {
        assert_eq!(
            s.layers.prepare(&host, &s.base, SEED, &mut |_| {}).unwrap(),
            Prepared::Live {
                disk: live.clone(),
                cloned: None
            },
            "a reused bring-up, whatever the plan, bakes and clones nothing"
        );
    }
    assert_eq!(host.clones.borrow().len(), clones);
    assert_eq!(std::fs::read(&live).unwrap(), b"the guest's caches");

    let bumped = Base {
        sha256: "b".repeat(64),
        ..s.base.clone()
    };
    let old = s.layers.baked_dir().join(baked_name(&s.base.sha256, SEED));
    assert!(matches!(
        s.layers.prepare(&host, &bumped, SEED, &mut |_| {}).unwrap(),
        Prepared::Bake(_)
    ));
    assert!(old.is_file(), "the record still names the old baked disk");
    let live = bake_and_bring_up(&s, &host, &bumped);
    assert_eq!(
        &std::fs::read(&live).unwrap()[..11],
        b"provisioned",
        "the negative control: a base change clones a new live disk"
    );
    assert_eq!(s.layers.record().unwrap().base_sha256, bumped.sha256);
    assert!(
        !old.exists(),
        "the old baked disk goes once no live record names it"
    );
    assert_eq!(
        names(&s.layers.baked_dir()),
        [baked_name(&bumped.sha256, SEED)]
    );
}

#[test]
fn a_seed_change_alone_keeps_the_live_disk() {
    let s = scratch();
    let host = Fake::new();
    let live = bake_and_bring_up(&s, &host, &s.base);
    let other = "fedcba9876543210";
    assert_eq!(
        s.layers
            .prepare(&host, &s.base, other, &mut |_| {})
            .unwrap(),
        Prepared::Live {
            disk: live,
            cloned: None
        },
        "a disk is cloned live only with none there or another base"
    );
    assert!(
        s.layers
            .baked_dir()
            .join(baked_name(&s.base.sha256, SEED))
            .is_file(),
        "the record's baked disk stays"
    );
}

#[test]
fn a_partial_file_is_removed_unread_and_never_cloned() {
    let s = scratch();
    let host = Fake::new();
    let first = match s.layers.prepare(&host, &s.base, SEED, &mut |_| {}).unwrap() {
        Prepared::Bake(bake) => bake.path().to_path_buf(),
        other => panic!("{other:?}"),
    };
    std::fs::write(&first, b"half provisioned").unwrap();
    let stale_live = s.layers.live_dir().join("aterm-linux.partial.img");
    std::fs::write(&stale_live, b"a clone cut short").unwrap();

    let mut said = Vec::new();
    let again = match s
        .layers
        .prepare(&host, &s.base, SEED, &mut |l| said.push(l.to_string()))
        .unwrap()
    {
        Prepared::Bake(bake) => bake,
        other => panic!("an unsealed bake is never a baked disk: {other:?}"),
    };
    assert_eq!(
        again.path(),
        first,
        "the bake begins again under the same name"
    );
    assert_eq!(
        std::fs::read(again.path()).unwrap()[..15],
        *b"the root's ext4",
        "from base, not from what the cut bake left"
    );
    assert!(!stale_live.exists());
    assert_eq!(said.len(), 2, "{said:?}");
    assert!(
        host.cloned_from().iter().all(|src| *src == s.base.path),
        "only base was cloned: {:?}",
        host.cloned_from()
    );
    assert!(s.layers.record().is_none());
}

#[test]
fn a_bring_up_below_the_floor_is_refused_before_it_clones() {
    let s = scratch();
    let host = Fake::new();
    let live = bake_and_bring_up(&s, &host, &s.base);
    std::fs::write(s.layers.record_path(), "baked = \"elsewhere.img\"\n").unwrap();
    let record = std::fs::read(s.layers.record_path()).unwrap();
    let clones = host.clones.borrow().len();
    host.free.set(BRING_UP_FLOOR - 1);
    match s.layers.prepare(&host, &s.base, SEED, &mut |_| {}) {
        Err(Error::Floor(why)) => assert!(
            why.starts_with("NOT RUN — the volume holding aterm-linux.img, /scratch (container:disk9), has 20.0 GiB free")
                && why.ends_with("nothing was cloned or baked"),
            "{why}"
        ),
        other => panic!("a bring-up that would clone, below the floor: {other:?}"),
    }
    assert_eq!(host.clones.borrow().len(), clones, "nothing was cloned");
    assert_eq!(&std::fs::read(&live).unwrap()[..11], b"provisioned");
    assert_eq!(std::fs::read(s.layers.record_path()).unwrap(), record);
    host.free.set(BRING_UP_FLOOR);
    assert!(matches!(
        s.layers.prepare(&host, &s.base, SEED, &mut |_| {}),
        Ok(Prepared::Live {
            cloned: Some(Reclone::NoRecord),
            ..
        })
    ));
}

/// A cut bake's partial and an old pin's baked disk are the lane's own, and go before the
/// floor is read: they cannot hold the volume under it at every bring-up. NEGATIVE CONTROL:
/// a file the lane does not own frees the same and is never removed, so the bring-up is
/// refused.
#[test]
fn the_lanes_own_leftovers_go_before_the_floor_is_read() {
    let s = scratch();
    let host = Fake::new();
    let live = bake_and_bring_up(&s, &host, &s.base);
    let partial = s
        .layers
        .baked_dir()
        .join(format!("{}-{SEED}{PARTIAL}", "b".repeat(64)));
    let old_pin = s.layers.baked_dir().join(baked_name(&"c".repeat(64), SEED));
    let not_ours = s.layers.root().join("someone-elses.img");
    host.holding(&partial, 5 << 30);
    host.holding(&old_pin, 5 << 30);
    host.holding(&not_ours, 15 << 30);
    host.free.set(BRING_UP_FLOOR - (9 << 30));
    let mut said = Vec::new();
    assert_eq!(
        s.layers
            .prepare(&host, &s.base, SEED, &mut |l| said.push(l.to_string())),
        Ok(Prepared::Live {
            disk: live,
            cloned: None
        }),
        "10 GiB of the lane's own leftovers lift the volume over its floor"
    );
    assert!(!partial.exists() && !old_pin.exists() && not_ours.exists());
    assert_eq!(said.len(), 2, "{said:?}");

    host.holds.borrow_mut().clear();
    host.holding(&partial, 5 << 30);
    host.holding(&not_ours, 15 << 30);
    host.free.set(BRING_UP_FLOOR - (6 << 30));
    assert!(
        matches!(
            s.layers.prepare(&host, &s.base, SEED, &mut |_| {}),
            Err(Error::Floor(_))
        ),
        "the partial's 5 GiB are not enough, and the file the lane does not own stays"
    );
    assert!(!partial.exists() && not_ours.exists());
}

#[test]
fn a_live_disk_without_a_whole_record_is_cloned_again() {
    let s = scratch();
    let host = Fake::new();
    let live = bake_and_bring_up(&s, &host, &s.base);
    std::fs::write(&live, b"unknown origin").unwrap();
    std::fs::write(s.layers.record_path(), "baked = \"elsewhere.img\"\n").unwrap();
    assert_eq!(
        s.layers.prepare(&host, &s.base, SEED, &mut |_| {}).unwrap(),
        Prepared::Live {
            disk: live.clone(),
            cloned: Some(Reclone::NoRecord)
        }
    );
    assert_eq!(&std::fs::read(&live).unwrap()[..11], b"provisioned");
}

#[test]
fn a_record_reads_back_only_whole() {
    let record = Record {
        baked: baked_name(&"c".repeat(64), SEED),
        base_sha256: "c".repeat(64),
        seed: SEED.into(),
        cloned: 1_790_000_000,
    };
    assert_eq!(Record::parse(&record.text()), Some(record.clone()));
    let other = Record {
        baked: baked_name(&"d".repeat(64), SEED),
        ..record.clone()
    };
    assert_eq!(
        Record::parse(&other.text()),
        None,
        "a name that is not its base's and seed's"
    );
    assert_eq!(Record::parse("baked = 1"), None);
    assert_eq!(Record::parse(""), None);
}

#[test]
fn a_base_larger_than_the_disk_is_refused() {
    let s = scratch();
    let layers = s.layers.clone().with_disk_bytes(4);
    assert!(matches!(
        layers.prepare(&Fake::new(), &s.base, SEED, &mut |_| {}),
        Err(Error::Input(why)) if why.contains("more than the 4-byte disk")
    ));
}

#[test]
fn the_seed_iso_is_made_once_per_fingerprint() {
    let s = scratch();
    let host = Fake::new();
    let lane = User {
        name: "lane".into(),
        uid: 501,
    };
    let seed = Seed::new(&lane).unwrap();
    let iso = s.layers.seed_iso(&host, &seed).unwrap();
    assert_eq!(
        iso,
        s.layers
            .seed_dir()
            .join(format!("{}.iso", seed.fingerprint()))
    );
    let text = std::fs::read_to_string(&iso).unwrap();
    assert!(text.starts_with("#cloud-config\n") && text.contains("dhcp4: true"));
    assert_eq!(s.layers.seed_iso(&host, &seed).unwrap(), iso);
    assert_eq!(
        host.isos.borrow().len(),
        1,
        "a made ISO is read where it lies"
    );

    let other = Seed::new(&User { uid: 502, ..lane }).unwrap();
    let next = s.layers.seed_iso(&host, &other).unwrap();
    assert_eq!(
        names(&s.layers.seed_dir()),
        [next.file_name().unwrap().to_string_lossy().into_owned()],
        "the old seed and every scratch go"
    );
}

#[test]
fn the_layers_live_in_aterms_cache_beside_the_tooling_that_holds_base() {
    let home = Path::new("/Users//someone");
    let cache = |name| aterm_uds::host::user_cache("macos", Some(home), None, name).unwrap();
    let layers = Layers::at(cache("vz"));
    assert_eq!(
        layers.root(),
        Path::new("/Users//someone/Library/Caches/aterm/vz.noindex")
    );
    assert_eq!(
        layers.root().parent(),
        cache(aterm_uds::host::TOOLING).parent(),
        "one volume, so base clones"
    );
    assert_eq!(
        layers.live_disk(),
        Path::new("/Users//someone/Library/Caches/aterm/vz.noindex/live/aterm-linux.img")
    );
}
