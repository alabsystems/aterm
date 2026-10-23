// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

use super::*;
use std::collections::BTreeMap;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

const DAY: Duration = Duration::from_secs(24 * 60 * 60);

/// The clock every case reads.
fn now() -> SystemTime {
    UNIX_EPOCH + Duration::from_secs(1_800_000_000)
}

/// An in-memory tree.
#[derive(Default)]
struct Fake {
    nodes: BTreeMap<PathBuf, (Kind, SystemTime, Vec<u8>, Option<PathBuf>)>,
    /// Paths whose metadata cannot be read (a listing shows them with none).
    unreadable: Vec<PathBuf>,
    looked: std::cell::RefCell<Vec<PathBuf>>,
    devices: BTreeMap<PathBuf, Option<u64>>,
}

impl Fake {
    /// `path` (and every missing folder above it, written `at`) as `kind`.
    fn put(&mut self, path: &str, kind: Kind, at: SystemTime, bytes: &[u8]) -> &mut Self {
        let path = PathBuf::from(path);
        for parent in path.ancestors().skip(1) {
            self.nodes
                .entry(parent.to_path_buf())
                .or_insert((Kind::Dir, at, Vec::new(), None));
        }
        self.nodes.insert(path, (kind, at, bytes.to_vec(), None));
        self
    }

    fn dir(&mut self, path: &str, at: SystemTime) -> &mut Self {
        self.put(path, Kind::Dir, at, b"")
    }

    fn file(&mut self, path: &str, at: SystemTime, bytes: &[u8]) -> &mut Self {
        self.put(path, Kind::File, at, bytes)
    }

    fn link(&mut self, path: &str, at: SystemTime, to: &str) -> &mut Self {
        self.put(path, Kind::Link, at, b"");
        self.nodes.get_mut(Path::new(path)).unwrap().3 = Some(PathBuf::from(to));
        self
    }

    /// Writes `path` again at `at`: its own time only, as a write into a folder moves the folder's.
    fn touch(&mut self, path: &str, at: SystemTime) {
        self.nodes.get_mut(Path::new(path)).unwrap().1 = at;
    }
}

impl Fs for Fake {
    fn meta(&self, path: &Path) -> Option<Meta> {
        self.looked.borrow_mut().push(path.to_path_buf());
        if self.unreadable.iter().any(|p| p == path) {
            return None;
        }
        let (kind, modified, _, _) = self.nodes.get(path)?;
        Some(Meta {
            kind: *kind,
            modified: *modified,
            device: self.devices.get(path).copied().unwrap_or(Some(1)),
        })
    }

    fn list(&self, dir: &Path) -> Option<Vec<(OsString, Option<Meta>)>> {
        self.looked.borrow_mut().push(dir.to_path_buf());
        if self.meta(dir)?.kind != Kind::Dir {
            return None;
        }
        Some(
            self.nodes
                .keys()
                .filter(|p| p.parent() == Some(dir))
                .map(|p| (p.file_name().unwrap().to_owned(), self.meta(p)))
                .collect(),
        )
    }

    fn head(&self, file: &Path, max: usize) -> Option<Vec<u8>> {
        self.looked.borrow_mut().push(file.to_path_buf());
        let (kind, _, bytes, _) = self.nodes.get(file)?;
        (*kind == Kind::File).then(|| bytes.iter().take(max).copied().collect())
    }

    fn read_link(&self, link: &Path) -> Option<PathBuf> {
        self.looked.borrow_mut().push(link.to_path_buf());
        self.nodes.get(link)?.3.clone()
    }
}

const LEAF: &str = "/c/Users//me/ws/.cargo-build";
const PROFILE: &str = "/c/Users//me/ws/.cargo-build/debug";
const OLD: &str = "1111111111111111";
const NEW: &str = "2222222222222222";
const FRESH: &str = "3333333333333333";

/// `.rustc_info.json` as cargo keeps it.
fn rustc_info(sysroots: &[&str]) -> Vec<u8> {
    let mut outputs = String::from(
        r#""1":{"success":true,"status":"","code":0,"stdout":"rustc 1.99.0-dev\nbinary: trustc\nrelease: 1.99.0-dev\n","stderr":""}"#,
    );
    for (i, root) in sysroots.iter().enumerate() {
        outputs.push_str(&format!(
            r#","{}":{{"success":true,"status":"","code":0,"stdout":"___\nlib___.rlib\n{root}\noff\ndebug_assertions\ntarget_os=\"macos\"\n","stderr":""}}"#,
            i + 2
        ));
    }
    format!(r#"{{"rustc_fingerprint":7,"outputs":{{{outputs}}},"successes":{{}}}}"#).into_bytes()
}

fn fingerprint(rustc: u64) -> Vec<u8> {
    format!(r#"{{"rustc":{rustc},"features":"[]","target":1,"profile":2,"path":3,"deps":[]}}"#)
        .into_bytes()
}

/// A build directory the store's current compiler last built.
fn world(moved: SystemTime, old_age: Duration) -> Fake {
    let then = now() - old_age;
    let mut fs = Fake::default();
    fs.dir("/store/trust/9192", now() - 60 * DAY)
        .dir("/store/trust/15698", moved)
        .link("/store/trust/current", moved, "15698")
        .file(
            &format!("{LEAF}/.rustc_info.json"),
            moved,
            &rustc_info(&["/store/trust/15698"]),
        )
        .file(
            &format!("{PROFILE}/.cargo-build-lock"),
            now() - 90 * DAY,
            b"",
        );
    for (hash, rustc, at) in [(OLD, 1, then), (NEW, 2, moved + DAY), (FRESH, 2, now())] {
        let fp = format!("{PROFILE}/.fingerprint/aterm-grid-{hash}");
        fs.dir(&fp, at)
            .file(
                &format!("{fp}/lib-aterm_grid.json"),
                at,
                &fingerprint(rustc),
            )
            .file(&format!("{fp}/invoked.timestamp"), at, b"")
            .file(
                &format!("{PROFILE}/deps/libaterm_grid-{hash}.rlib"),
                at,
                b"r",
            )
            .file(&format!("{PROFILE}/deps/aterm_grid-{hash}.d"), at, b"d");
    }
    fs.file(
        &format!("{PROFILE}/deps/aterm_grid-{OLD}.aterm_grid.1a2b3c-cgu.0.rcgu.o"),
        then,
        b"o",
    )
    .dir(&format!("{PROFILE}/build/aterm-grid-{OLD}"), then)
    .file(
        &format!("{PROFILE}/build/aterm-grid-{OLD}/output"),
        then,
        b"",
    )
    .file(&format!("{PROFILE}/examples/demo-{OLD}"), then, b"x")
    .file(&format!("{PROFILE}/deps/libaterm_gpu_web.rlib"), then, b"w")
    .file(&format!("{PROFILE}/deps/0b6650033266f1a6.twit"), then, b"t");
    for (folder, at) in [
        ("aterm_grid-1k116trr0a0xf", then),
        ("aterm_grid-3g63pv341xugh", now()),
    ] {
        let crate_dir = format!("{PROFILE}/incremental/{folder}");
        fs.dir(&crate_dir, at)
            .dir(
                &format!("{crate_dir}/s-hneouhc0uf-1tn8o9j-2awpw07x6x4g1"),
                at,
            )
            .file(&format!("{crate_dir}/s-hneouhc0uf-1tn8o9j.lock"), at, b"");
    }
    fs
}

fn surveyed(fs: &Fake) -> Survey {
    survey(
        fs,
        Path::new(LEAF),
        &[PathBuf::from(PROFILE)],
        Some(Path::new("/store")),
    )
}

fn paths(doomed: &[Doomed]) -> Vec<(Part, Vec<String>)> {
    doomed
        .iter()
        .map(|d| {
            let mut p: Vec<String> = d
                .paths
                .iter()
                .map(|p| p.strip_prefix(PROFILE).unwrap().display().to_string())
                .collect();
            p.sort();
            (d.part, p)
        })
        .collect()
}

/// Unix only: the fixture's sysroot (`/store/trust/…`) is read only when it is
/// absolute, which a path with no drive is not on Windows.
#[test]
#[cfg(unix)]
fn a_stale_crate_folder_and_an_old_compilers_unit_go_with_their_entries() {
    let fs = world(now() - 2 * DAY, 4 * DAY);
    let s = surveyed(&fs);
    assert_eq!(
        s.last_built,
        Some(LastBuilt {
            current: true,
            since: now() - 2 * DAY
        })
    );
    assert_eq!(
        paths(&decide(&s, now())),
        vec![
            (
                Part::Incremental,
                vec!["incremental/aterm_grid-1k116trr0a0xf".to_owned()]
            ),
            (
                Part::Unit,
                vec![
                    format!(".fingerprint/aterm-grid-{OLD}"),
                    format!("build/aterm-grid-{OLD}"),
                    format!("deps/aterm_grid-{OLD}.aterm_grid.1a2b3c-cgu.0.rcgu.o"),
                    format!("deps/aterm_grid-{OLD}.d"),
                    format!("deps/libaterm_grid-{OLD}.rlib"),
                    format!("examples/demo-{OLD}"),
                ]
            ),
        ]
    );
}

#[test]
fn nothing_goes_while_any_of_it_was_written_within_the_window() {
    let young = world(now() - DAY, 2 * DAY);
    assert_eq!(decide(&surveyed(&young), now()), Vec::new());

    let mut touched = world(now() - 2 * DAY, 4 * DAY);
    touched.touch(
        &format!("{PROFILE}/deps/aterm_grid-{OLD}.aterm_grid.1a2b3c-cgu.0.rcgu.o"),
        now(),
    );
    assert_eq!(
        paths(&decide(&surveyed(&touched), now()))
            .into_iter()
            .map(|(part, _)| part)
            .collect::<Vec<_>>(),
        vec![Part::Incremental]
    );

    for second in [fingerprint(2), b"{\"features\":\"[]\"}".to_vec()] {
        let mut unread = world(now() - 2 * DAY, 4 * DAY);
        unread.file(
            &format!("{PROFILE}/.fingerprint/aterm-grid-{OLD}/test-lib-aterm_grid.json"),
            now() - 4 * DAY,
            &second,
        );
        let s = surveyed(&unread);
        assert_eq!(s.profiles[0].units[0].rustc, None);
        assert!(
            decide(&s, now())
                .iter()
                .all(|d| d.part == Part::Incremental)
        );
    }
}

/// Unix only: the fixture's sysroot (`/store/trust/…`) is read only when it is
/// absolute, which a path with no drive is not on Windows.
#[test]
#[cfg(unix)]
fn units_stay_where_the_current_compiler_cannot_be_named() {
    let incremental_only = |fs: &Fake| {
        let s = surveyed(fs);
        let parts: Vec<Part> = decide(&s, now()).iter().map(|d| d.part).collect();
        assert_eq!(parts, vec![Part::Incremental]);
        s.last_built
    };
    let info = format!("{LEAF}/.rustc_info.json");
    let stock = "/Users//me/.rustup/toolchains/stable-aarch64-apple-darwin";
    for sysroots in [
        vec![stock],
        vec!["/opt/trust/15698"],
        vec!["/store/trust/15698", "/store/trust/9192"],
        vec![],
    ] {
        let mut fs = world(now() - 2 * DAY, 4 * DAY);
        fs.file(&info, now(), &rustc_info(&sysroots));
        assert_eq!(incremental_only(&fs), None, "{sysroots:?}");
    }
    for foreign in [
        "/Users//me/Documents/store/trust/7",
        "/Volumes/away/store/trust/7",
        "/other/store/trust/7",
        "/store/trust/../../other/7",
    ] {
        let mut fs = world(now() - 2 * DAY, 4 * DAY);
        fs.file(&info, now(), &rustc_info(&[foreign]));
        fs.looked.borrow_mut().clear();
        assert_eq!(incremental_only(&fs), None);
        assert!(fs.looked.borrow().iter().all(|p| p.starts_with(LEAF)));
    }
    let no_store = world(now() - 2 * DAY, 4 * DAY);
    let s = survey(&no_store, Path::new(LEAF), &[PathBuf::from(PROFILE)], None);
    assert_eq!(s.last_built, None);
    assert!(no_store.looked.borrow().iter().all(|p| p.starts_with(LEAF)));
    assert!(
        decide(&s, now())
            .iter()
            .all(|d| d.part == Part::Incremental)
    );
    let mut aliased = world(now() - 2 * DAY, 4 * DAY);
    aliased.link("/store/trust", now(), "/Users//me/Documents/other");
    aliased.looked.borrow_mut().clear();
    assert_eq!(incremental_only(&aliased), None);
    assert!(
        !aliased
            .looked
            .borrow()
            .iter()
            .any(|p| p == Path::new("/store/trust/current"))
    );

    for device in [None, Some(2)] {
        let mut foreign_device = world(now() - 2 * DAY, 4 * DAY);
        foreign_device
            .devices
            .insert(PathBuf::from("/store/trust"), device);
        foreign_device.looked.borrow_mut().clear();
        assert_eq!(incremental_only(&foreign_device), None);
        assert!(
            !foreign_device
                .looked
                .borrow()
                .iter()
                .any(|p| p == Path::new("/store/trust/current"))
        );
    }

    let mut no_link = world(now() - 2 * DAY, 4 * DAY);
    no_link.dir("/store/trust/current", now() - 2 * DAY);
    assert_eq!(incremental_only(&no_link), None);

    let mut unbuilt = world(now(), 4 * DAY);
    for hash in [NEW, FRESH] {
        let fp = format!("{PROFILE}/.fingerprint/aterm-grid-{hash}");
        for p in [
            fp.clone(),
            format!("{fp}/lib-aterm_grid.json"),
            format!("{fp}/invoked.timestamp"),
        ] {
            unbuilt.touch(&p, now() - DAY);
        }
    }
    assert_eq!(
        incremental_only(&unbuilt),
        Some(LastBuilt {
            current: true,
            since: now()
        })
    );
}

/// Sets the unit `hash`'s fingerprint folder, its files and its entries to `at`
/// (for the Unix-only rows below).
#[cfg(unix)]
fn date_unit(fs: &mut Fake, hash: &str, at: SystemTime) {
    let named: Vec<String> = fs
        .nodes
        .keys()
        .filter(|p| p.to_str().is_some_and(|p| p.contains(&format!("-{hash}"))))
        .map(|p| p.display().to_string())
        .collect();
    for p in named {
        fs.touch(&p, at);
    }
}

/// Unix only: the fixture's sysroot (`/store/trust/…`) is read only when it is
/// absolute, which a path with no drive is not on Windows.
#[test]
#[cfg(unix)]
fn a_workspace_left_on_the_previous_compiler_loses_every_unit_once_stale() {
    let mut fs = world(now() - DAY, 4 * DAY);
    for hash in [NEW, FRESH] {
        date_unit(&mut fs, hash, now() - 5 * DAY);
    }
    let info = format!("{LEAF}/.rustc_info.json");
    fs.file(&info, now() - 5 * DAY, &rustc_info(&["/store/trust/9192"]));
    let s = surveyed(&fs);
    assert_eq!(
        s.last_built,
        Some(LastBuilt {
            current: false,
            since: now() - DAY
        })
    );
    let units: Vec<PathBuf> = decide(&s, now())
        .into_iter()
        .filter(|d| d.part == Part::Unit)
        .map(|d| d.witness.path)
        .collect();
    let fingerprint_of =
        |hash: &str| PathBuf::from(format!("{PROFILE}/.fingerprint/aterm-grid-{hash}"));
    assert_eq!(units, [OLD, NEW, FRESH].map(fingerprint_of));

    fs.file(&info, now() - 5 * DAY, &rustc_info(&["/store/trust/15698"]));
    let parts: Vec<Part> = decide(&surveyed(&fs), now())
        .iter()
        .map(|d| d.part)
        .collect();
    assert_eq!(parts, vec![Part::Incremental]);
}

/// Unix only: the fixture's sysroot (`/store/trust/…`) is read only when it is
/// absolute, which a path with no drive is not on Windows.
#[test]
#[cfg(unix)]
fn what_was_written_since_the_survey_is_kept() {
    let mut fs = world(now() - 2 * DAY, 4 * DAY);
    let doomed = decide(&surveyed(&fs), now());
    assert!(doomed.iter().all(|d| unchanged(&fs, d)));
    fs.touch(
        &format!("{PROFILE}/incremental/aterm_grid-1k116trr0a0xf/s-hneouhc0uf-1tn8o9j.lock"),
        now(),
    );
    fs.touch(
        &format!("{PROFILE}/.fingerprint/aterm-grid-{OLD}/invoked.timestamp"),
        now(),
    );
    assert!(doomed.iter().all(|d| !unchanged(&fs, d)));

    let mut artifact = world(now() - 2 * DAY, 4 * DAY);
    let before = decide(&surveyed(&artifact), now());
    assert!(before.iter().any(|d| d.part == Part::Unit));
    artifact.touch(&format!("{PROFILE}/deps/libaterm_grid-{OLD}.rlib"), now());
    assert!(
        decide(&surveyed(&artifact), now())
            .iter()
            .all(|d| d.part == Part::Incremental)
    );

    let mut moved = world(now() - 2 * DAY, 4 * DAY);
    assert!(
        decide(&surveyed(&moved), now())
            .iter()
            .any(|d| d.part == Part::Unit)
    );
    moved.link("/store/trust/current", now() + DAY, "15698");
    assert!(
        decide(&surveyed(&moved), now())
            .iter()
            .all(|d| d.part == Part::Incremental)
    );

    let mut hidden = world(now() - 2 * DAY, 4 * DAY);
    hidden.unreadable.push(PathBuf::from(format!(
        "{PROFILE}/incremental/aterm_grid-1k116trr0a0xf/s-hneouhc0uf-1tn8o9j.lock"
    )));
    let s = surveyed(&hidden);
    assert_eq!(s.profiles[0].crates[0].newest, None);
    assert!(decide(&s, now()).iter().all(|d| d.part == Part::Unit));
}

#[test]
fn the_unit_hash_is_read_from_every_output_name() {
    let h = "0123456789abcdef";
    for name in [
        format!("libaterm_grid-{h}.rlib"),
        format!("libaterm_grid-{h}.rmeta"),
        format!("aterm_grid-{h}"),
        format!("aterm_grid-{h}.d"),
        format!("aterm_grid-{h}.dSYM"),
        format!("astream_sys-{h}.0asrtn8wzkwablau3ejrbexnp.030s1tl.rcgu.o"),
        format!("my-pkg-{h}"),
    ] {
        assert_eq!(unit_hash(&name), Some(h), "{name}");
    }
    for name in [
        "libaterm_gpu_web.rlib",
        "0b6650033266f1a6.twit",
        "aterm_grid-0123456789ABCDEF",
        "aterm_grid-0123456789abcde",
        "aterm_grid-0123456789abcdef0",
        "aterm_grid-1k116trr0a0xf",
    ] {
        assert_eq!(unit_hash(name), None, "{name}");
    }
}
