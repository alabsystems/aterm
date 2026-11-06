// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

use super::*;
use std::collections::{BTreeMap, BTreeSet};
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
    /// Folders whose metadata reads but which cannot be listed.
    blind: Vec<PathBuf>,
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
        if self.meta(dir)?.kind != Kind::Dir || self.blind.iter().any(|p| p == dir) {
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
        paths(&decide(&s, now(), RECLAIM_STALE)),
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

/// A SURVEY SAYS IT IS STILL GOING: it stats each unit of a profile and reads each fingerprint
/// folder's JSON, and a profile holds thousands of each.
#[test]
fn a_survey_ticks_for_each_stamp_and_fingerprint_it_reads() {
    let fs = world(now() - DAY, 4 * DAY);
    let ticks = aterm_uds::progress::ticks_here;
    let before = ticks();
    stamp(&fs, Path::new(LEAF));
    assert_eq!(ticks() - before, 1, "a stamp");
    let folder = format!("{PROFILE}/.fingerprint/aterm-grid-{OLD}");
    let before = ticks();
    assert!(compiler(&fs, Path::new(&folder)).is_some());
    assert_eq!(ticks() - before, 1, "a fingerprint folder");
}

/// THE WINDOW IS THE PASS'S: parts written four hours ago stay for a pass with room and go for
/// a pass below the floor, the unit by the same window as the folder. Parts written 59 minutes
/// ago stay for both, as does the folder written just now.
#[test]
fn below_the_floor_the_same_parts_go_after_an_hour() {
    const HOUR: Duration = Duration::from_secs(60 * 60);
    let hours = surveyed(&world(now() - 3 * HOUR, 4 * HOUR));
    assert_eq!(decide(&hours, now(), RECLAIM_STALE), Vec::new());
    let parts: Vec<Part> = decide(&hours, now(), RECLAIM_STALE_UNDER_PRESSURE)
        .iter()
        .map(|doomed| doomed.part)
        .collect();
    assert_eq!(parts, vec![Part::Incremental, Part::Unit]);

    let minutes = surveyed(&world(now() - 3 * HOUR, Duration::from_secs(59 * 60)));
    assert_eq!(
        decide(&minutes, now(), RECLAIM_STALE_UNDER_PRESSURE),
        Vec::new()
    );
}

#[test]
fn nothing_goes_while_any_of_it_was_written_within_the_window() {
    let young = world(now() - DAY, 2 * DAY);
    assert_eq!(decide(&surveyed(&young), now(), RECLAIM_STALE), Vec::new());

    let mut touched = world(now() - 2 * DAY, 4 * DAY);
    touched.touch(
        &format!("{PROFILE}/deps/aterm_grid-{OLD}.aterm_grid.1a2b3c-cgu.0.rcgu.o"),
        now(),
    );
    assert_eq!(
        paths(&decide(&surveyed(&touched), now(), RECLAIM_STALE))
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
            decide(&s, now(), RECLAIM_STALE)
                .iter()
                .all(|d| d.part == Part::Incremental)
        );
    }
}

/// A compiler laid aside reports its sysroot canonical, `<build>.noindex`, while `current`
/// names `<build>`, the link to it ([`aterm_toolchain::layout`]): the same build either way.
#[test]
#[cfg(unix)]
fn a_sysroot_laid_aside_is_the_build_its_link_names() {
    let info = format!("{LEAF}/.rustc_info.json");
    let mut fs = world(now() - 2 * DAY, 4 * DAY);
    fs.dir("/store/trust/15698.noindex", now() - 2 * DAY)
        .link("/store/trust/15698", now() - 2 * DAY, "15698.noindex")
        .file(&info, now(), &rustc_info(&["/store/trust/15698.noindex"]));
    assert_eq!(
        surveyed(&fs).last_built,
        Some(LastBuilt {
            current: true,
            since: now() - 2 * DAY
        })
    );
    fs.link("/store/trust/current", now() - DAY, "9192");
    assert_eq!(
        surveyed(&fs).last_built,
        Some(LastBuilt {
            current: false,
            since: now() - DAY
        }),
        "the previous compiler, laid aside, is still named as the previous one"
    );
}

/// Unix only: the fixture's sysroot (`/store/trust/…`) is read only when it is
/// absolute, which a path with no drive is not on Windows.
#[test]
#[cfg(unix)]
fn units_stay_where_the_current_compiler_cannot_be_named() {
    let incremental_only = |fs: &Fake| {
        let s = surveyed(fs);
        let parts: Vec<Part> = decide(&s, now(), RECLAIM_STALE)
            .iter()
            .map(|d| d.part)
            .collect();
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
        decide(&s, now(), RECLAIM_STALE)
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
    let units: Vec<PathBuf> = decide(&s, now(), RECLAIM_STALE)
        .into_iter()
        .filter(|d| d.part == Part::Unit)
        .map(|d| d.witness.path)
        .collect();
    let fingerprint_of =
        |hash: &str| PathBuf::from(format!("{PROFILE}/.fingerprint/aterm-grid-{hash}"));
    assert_eq!(units, [OLD, NEW, FRESH].map(fingerprint_of));

    fs.file(&info, now() - 5 * DAY, &rustc_info(&["/store/trust/15698"]));
    let parts: Vec<Part> = decide(&surveyed(&fs), now(), RECLAIM_STALE)
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
    let doomed = decide(&surveyed(&fs), now(), RECLAIM_STALE);
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
    let before = decide(&surveyed(&artifact), now(), RECLAIM_STALE);
    assert!(before.iter().any(|d| d.part == Part::Unit));
    artifact.touch(&format!("{PROFILE}/deps/libaterm_grid-{OLD}.rlib"), now());
    assert!(
        decide(&surveyed(&artifact), now(), RECLAIM_STALE)
            .iter()
            .all(|d| d.part == Part::Incremental)
    );

    let mut moved = world(now() - 2 * DAY, 4 * DAY);
    assert!(
        decide(&surveyed(&moved), now(), RECLAIM_STALE)
            .iter()
            .any(|d| d.part == Part::Unit)
    );
    moved.link("/store/trust/current", now() + DAY, "15698");
    assert!(
        decide(&surveyed(&moved), now(), RECLAIM_STALE)
            .iter()
            .all(|d| d.part == Part::Incremental)
    );

    let mut hidden = world(now() - 2 * DAY, 4 * DAY);
    hidden.unreadable.push(PathBuf::from(format!(
        "{PROFILE}/incremental/aterm_grid-1k116trr0a0xf/s-hneouhc0uf-1tn8o9j.lock"
    )));
    let s = surveyed(&hidden);
    assert_eq!(s.profiles[0].crates[0].newest, None);
    assert!(
        decide(&s, now(), RECLAIM_STALE)
            .iter()
            .all(|d| d.part == Part::Unit)
    );
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

/// One unit of the layout that keeps each in a folder, `build/<pkg>/<hash>/`: its fingerprint
/// naming `kind` (`lib-aterm_grid`, `test-lib-aterm_grid`) and compiler `rustc`, and its outputs
/// `outputs` (named with the hash where they hold `{h}`), all written `at`.
fn folder_unit(
    fs: &mut Fake,
    package: &str,
    hash: &str,
    kind: &str,
    rustc: u64,
    outputs: &[&str],
    at: SystemTime,
) {
    let unit = format!("{PROFILE}/build/{package}/{hash}");
    fs.dir(&unit, at)
        .file(&format!("{unit}/fingerprint/{kind}"), at, b"h")
        .file(
            &format!("{unit}/fingerprint/{kind}.json"),
            at,
            &fingerprint(rustc),
        )
        .file(&format!("{unit}/fingerprint/dep-{kind}"), at, b"d")
        .file(&format!("{unit}/fingerprint/invoked.timestamp"), at, b"");
    for out in outputs {
        fs.file(
            &format!("{unit}/out/{}", out.replace("{h}", hash)),
            at,
            b"o",
        );
    }
}

/// THE LAYOUT THAT KEEPS EACH UNIT IN A FOLDER IS READ TOO: a unit an old compiler built goes
/// whole, as its one folder (`build/<pkg>/<hash>`), once stale; the current compiler's units
/// stay. Before A4 the survey read `.fingerprint/` alone, which this layout has none of, so it
/// took nothing of it. NEGATIVE CONTROL: the same old unit written within the window stays.
#[test]
#[cfg(unix)]
fn a_folder_layout_unit_an_old_compiler_built_goes_whole() {
    let moved = now() - 2 * DAY;
    let mut fs = Fake::default();
    fs.dir("/store/trust/15698", moved)
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
    let lib = [
        "libaterm_grid-{h}.rlib",
        "libaterm_grid-{h}.rmeta",
        "aterm_grid-{h}.d",
    ];
    folder_unit(
        &mut fs,
        "aterm-grid",
        OLD,
        "lib-aterm_grid",
        1,
        &lib,
        now() - 4 * DAY,
    );
    folder_unit(
        &mut fs,
        "aterm-grid",
        NEW,
        "lib-aterm_grid",
        2,
        &lib,
        moved + DAY,
    );
    folder_unit(
        &mut fs,
        "aterm-grid",
        FRESH,
        "lib-aterm_grid",
        2,
        &lib,
        now(),
    );
    let s = surveyed(&fs);
    assert_eq!(
        s.profiles[0]
            .units
            .iter()
            .map(|u| (u.package.as_str(), u.hash.as_str()))
            .collect::<Vec<_>>(),
        vec![
            ("aterm-grid", OLD),
            ("aterm-grid", NEW),
            ("aterm-grid", FRESH)
        ]
    );
    assert_eq!(
        paths(&decide(&s, now(), RECLAIM_STALE)),
        vec![(Part::Unit, vec![format!("build/aterm-grid/{OLD}")])]
    );
    folder_unit(
        &mut fs,
        "aterm-grid",
        OLD,
        "lib-aterm_grid",
        1,
        &lib,
        now() - DAY,
    );
    assert_eq!(decide(&surveyed(&fs), now(), RECLAIM_STALE), Vec::new());
}

/// The library build the release run's tests used.
const USED_LIB: &str = "6666666666666666";
/// Lint's check of that library, which writes only `.rmeta`.
const CHECK: &str = "4444444444444444";
/// A build of that library another compile of the run made under its own features (spec-check's).
const SPEC: &str = "8888888888888888";
/// The build of `libc` the release run's tests used.
const LIBC: &str = "5555555555555555";
/// Another build of `libc`, under another compile's features (spec-check's).
const LIBC_SPEC: &str = "7777777777777777";

/// `at` in milliseconds since the epoch.
fn millis(at: SystemTime) -> u64 {
    u64::try_from(at.duration_since(UNIX_EPOCH).unwrap().as_millis()).unwrap()
}

/// When the last passing release run of [`release_clone`] began: an hour before its record was
/// written, four days ago.
fn began() -> SystemTime {
    now() - 4 * DAY - Duration::from_secs(60 * 60)
}

/// The release clone's profile. Of `aterm-grid`, a package of the workspace's own: the test unit of
/// the candidate before (`OLD`), its test unit now (`NEW`), the library build its tests used
/// ([`USED_LIB`], `.rlib` and `.rmeta`) and lint's check of it ([`CHECK`]), each written five days
/// ago; a test unit built a day ago (`FRESH`); and the build of its library another compile of
/// the run made ([`SPEC`]), written after the run began. Of `libc`, a dependency every candidate
/// reuses, unwritten a month: the build the tests used ([`LIBC`]) and another compile's
/// ([`LIBC_SPEC`]). The record of the last passing release run ([`began`]), written four days
/// ago, names `NEW`, [`USED_LIB`] and [`LIBC`], and `aterm-grid` as the workspace's own.
fn release_clone() -> Fake {
    release_clone_with(
        &["libaterm_grid-{h}.rlib", "libaterm_grid-{h}.rmeta"],
        &["aterm-grid"],
    )
}

/// [`release_clone`], with [`USED_LIB`]'s outputs `lib` and the record's own packages `members`.
fn release_clone_with(lib: &[&str], members: &[&str]) -> Fake {
    let mut fs = Fake::default();
    fs.file(
        &format!("{PROFILE}/.cargo-build-lock"),
        now() - 90 * DAY,
        b"",
    );
    let then = now() - 5 * DAY;
    let test = ["aterm_grid-{h}", "aterm_grid-{h}.d"];
    let grid = |fs: &mut Fake, hash: &str, kind: &str, outputs: &[&str], at: SystemTime| {
        folder_unit(fs, "aterm-grid", hash, kind, 2, outputs, at);
    };
    grid(&mut fs, OLD, "test-lib-aterm_grid", &test, then);
    grid(&mut fs, NEW, "test-lib-aterm_grid", &test, then);
    grid(&mut fs, FRESH, "test-lib-aterm_grid", &test, now() - DAY);
    grid(&mut fs, USED_LIB, "lib-aterm_grid", lib, then);
    grid(
        &mut fs,
        CHECK,
        "lib-aterm_grid",
        &["libaterm_grid-{h}.rmeta"],
        then,
    );
    grid(
        &mut fs,
        SPEC,
        "lib-aterm_grid",
        &["libaterm_grid-{h}.rlib", "libaterm_grid-{h}.rmeta"],
        began() + Duration::from_secs(10 * 60),
    );
    let libc = ["liblibc-{h}.rlib", "liblibc-{h}.rmeta"];
    for hash in [LIBC, LIBC_SPEC] {
        folder_unit(
            &mut fs,
            "libc",
            hash,
            "lib-libc",
            2,
            &libc,
            now() - 30 * DAY,
        );
    }
    let mut used = Used {
        began_ms: millis(began()),
        members: members.iter().map(|m| (*m).to_owned()).collect(),
        ..Used::default()
    };
    for file in [
        format!("{PROFILE}/build/aterm-grid/{NEW}/out/aterm_grid-{NEW}"),
        format!("{PROFILE}/build/aterm-grid/{USED_LIB}/out/libaterm_grid-{USED_LIB}.rmeta"),
        format!("{PROFILE}/build/libc/{LIBC}/out/liblibc-{LIBC}.rlib"),
    ] {
        used.add(Path::new(LEAF), Path::new(&file));
    }
    fs.file(
        &format!("{LEAF}/{USED}"),
        now() - 4 * DAY,
        used.encode().as_bytes(),
    );
    fs
}

/// The release clone's profile ([`release_clone`]) as [`survey`] reads it.
fn clone_survey(fs: &Fake) -> Survey {
    survey(fs, Path::new(LEAF), &[PathBuf::from(PROFILE)], None)
}

/// What [`decide`] takes as superseded: each `(package, hash)` unit's folder.
fn superseded(units: &[(&str, &str)]) -> Vec<(Part, Vec<String>)> {
    units
        .iter()
        .map(|(package, hash)| (Part::Superseded, vec![format!("build/{package}/{hash}")]))
        .collect()
}

/// A UNIT THE LAST PASSING RELEASE RUN SUPERSEDED GOES ([`Part::Superseded`]): of the release
/// clone's units ([`release_clone`]), the test unit of the candidate before, which that run did not
/// use beside the test unit of the same package it did, goes whole once stale, and below the floor
/// alone too. Kept: the units the run used, however long unwritten (cargo found them fresh);
/// lint's check of the library, which wrote `.rmeta` alone, so another kind than the library build
/// the run used; the test unit built a day ago; the library build another compile of the run made,
/// written after the run began; and `libc`'s other build, a dependency's, which no version bump
/// hashes anew and another compile may read fresh. NEGATIVE CONTROLS, each the one thing that kept
/// a unit taken away: the old test unit written after the run began stays; the check goes where the
/// library build the run used wrote `.rmeta` alone too; `libc`'s other build goes where the record
/// names `libc` among the workspace's own; the other compile's library build goes where it was
/// written before the run began; and without the record, or with a torn one, nothing goes.
#[test]
fn a_unit_the_last_passing_release_run_superseded_goes() {
    let s = clone_survey(&release_clone());
    assert_eq!(s.began, Some(began()));
    assert_eq!(s.members, BTreeSet::from(["aterm-grid".to_owned()]));
    let gone = |s: &Survey, after| paths(&decide(s, now(), after));
    assert_eq!(gone(&s, RECLAIM_STALE), superseded(&[("aterm-grid", OLD)]));
    assert_eq!(
        gone(&s, RECLAIM_STALE_UNDER_PRESSURE),
        superseded(&[("aterm-grid", OLD)]),
        "below the floor the day-old test unit and the other compile's build are stale too, but \
         written after the run began"
    );

    let mut after = release_clone();
    date_folder(
        &mut after,
        &format!("build/aterm-grid/{OLD}"),
        began() + Duration::from_secs(60),
    );
    assert_eq!(
        gone(&clone_survey(&after), RECLAIM_STALE),
        Vec::new(),
        "written after the run began"
    );

    let checked = release_clone_with(&["libaterm_grid-{h}.rmeta"], &["aterm-grid"]);
    assert_eq!(
        gone(&clone_survey(&checked), RECLAIM_STALE),
        superseded(&[("aterm-grid", OLD), ("aterm-grid", CHECK)]),
        "the library build the run used, of the check's output kind"
    );

    let libc_ours = release_clone_with(
        &["libaterm_grid-{h}.rlib", "libaterm_grid-{h}.rmeta"],
        &["aterm-grid", "libc"],
    );
    assert_eq!(
        gone(&clone_survey(&libc_ours), RECLAIM_STALE),
        superseded(&[("aterm-grid", OLD), ("libc", LIBC_SPEC)]),
        "a dependency the record calls the workspace's own"
    );

    let mut earlier = release_clone();
    date_folder(
        &mut earlier,
        &format!("build/aterm-grid/{SPEC}"),
        now() - 5 * DAY,
    );
    assert_eq!(
        gone(&clone_survey(&earlier), RECLAIM_STALE),
        superseded(&[("aterm-grid", OLD), ("aterm-grid", SPEC)]),
        "the other compile's build, written before the run began"
    );

    let mut none = release_clone();
    none.nodes.remove(Path::new(&format!("{LEAF}/{USED}")));
    let s = clone_survey(&none);
    assert_eq!((s.began, s.profiles[0].used.clone()), (None, None));
    assert_eq!(decide(&s, now(), RECLAIM_STALE), Vec::new());

    let mut torn = release_clone();
    torn.file(
        &format!("{LEAF}/{USED}"),
        now() - 4 * DAY,
        b"{\"schema\":1,\"began_ms\":1,\"members\":[\"aterm-grid\"],\"profiles\":{\"debug\":[\"x\"]}}",
    );
    let s = clone_survey(&torn);
    assert_eq!(
        (s.began, decide(&s, now(), RECLAIM_STALE)),
        (None, Vec::new()),
        "a record with a word that is no hash is no record"
    );
}

/// THE RUN'S BEGINNING IS READ NO LATER THAN ITS RECORD WAS WRITTEN: a record that says its run
/// began after it was written (a clock moved) is read as beginning when it was written, so the old
/// test unit, written after the record, is never taken, while the other compile's library build,
/// written before it, is. NEGATIVE CONTROL: a record whose run began before it was written is read
/// as it says.
#[test]
fn a_run_is_read_as_beginning_no_later_than_its_record() {
    let mut fs = release_clone();
    let mut used = Used::decode(&fs.nodes[Path::new(&format!("{LEAF}/{USED}"))].2).unwrap();
    used.began_ms = millis(now());
    fs.file(
        &format!("{LEAF}/{USED}"),
        now() - 4 * DAY,
        used.encode().as_bytes(),
    );
    date_folder(
        &mut fs,
        &format!("build/aterm-grid/{OLD}"),
        now() - 4 * DAY + Duration::from_secs(60),
    );
    let s = clone_survey(&fs);
    assert_eq!(s.began, Some(now() - 4 * DAY));
    assert_eq!(
        paths(&decide(&s, now(), RECLAIM_STALE)),
        superseded(&[("aterm-grid", SPEC)])
    );
    assert_eq!(clone_survey(&release_clone()).began, Some(began()));
}

/// A UNIT WHOSE FOLDER CANNOT BE LISTED IS NOT READ, so nothing is decided of it: an old
/// compiler's unit whose folder lists nothing stays, as does one whose `out/` lists nothing; in the
/// layout that names entries with the hash, a profile whose `deps/` is there but lists nothing
/// loses none of its units (their entries would go unread), only its stale incremental folder.
/// NEGATIVE CONTROL: the same units, readable, go.
#[test]
#[cfg(unix)]
fn a_unit_that_cannot_be_listed_is_not_read() {
    let moved = now() - 2 * DAY;
    let mut fs = Fake::default();
    fs.dir("/store/trust/15698", moved)
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
    let lib = ["libaterm_grid-{h}.rlib", "libaterm_grid-{h}.rmeta"];
    folder_unit(
        &mut fs,
        "aterm-grid",
        OLD,
        "lib-aterm_grid",
        1,
        &lib,
        now() - 4 * DAY,
    );
    folder_unit(
        &mut fs,
        "aterm-grid",
        NEW,
        "lib-aterm_grid",
        2,
        &lib,
        moved + DAY,
    );
    let old = format!("build/aterm-grid/{OLD}");
    assert_eq!(
        paths(&decide(&surveyed(&fs), now(), RECLAIM_STALE)),
        vec![(Part::Unit, vec![old.clone()])]
    );
    for blind in [old.clone(), format!("{old}/out")] {
        fs.blind = vec![PathBuf::from(format!("{PROFILE}/{blind}"))];
        assert_eq!(
            decide(&surveyed(&fs), now(), RECLAIM_STALE),
            Vec::new(),
            "{blind} lists nothing"
        );
    }

    let mut named = world(now() - 2 * DAY, 4 * DAY);
    let both = paths(&decide(&surveyed(&named), now(), RECLAIM_STALE));
    assert!(both.iter().any(|(part, _)| *part == Part::Unit), "{both:?}");
    named.blind = vec![PathBuf::from(format!("{PROFILE}/deps"))];
    assert_eq!(
        paths(&decide(&surveyed(&named), now(), RECLAIM_STALE))
            .into_iter()
            .map(|(part, _)| part)
            .collect::<Vec<_>>(),
        vec![Part::Incremental]
    );
}

/// Sets every node at or below `rel` (under the profile) to `at`.
fn date_folder(fs: &mut Fake, rel: &str, at: SystemTime) {
    let root = PathBuf::from(format!("{PROFILE}/{rel}"));
    let named: Vec<PathBuf> = fs
        .nodes
        .keys()
        .filter(|p| p.starts_with(&root))
        .cloned()
        .collect();
    for p in named {
        fs.nodes.get_mut(&p).unwrap().1 = at;
    }
}

/// THE RECORD NAMES THE UNIT EACH OUTPUT IS OF, in either layout and any profile, and reads back
/// as written, with when its run began and its own packages: `build/<pkg>/<hash>/…`, a `deps/`
/// file and a `build/<pkg>-<hash>` folder named with the hash, a build script's run (its
/// `out/`), a cross target's profile. NEGATIVE CONTROL: an output cargo copied into the target
/// directory, one named with no hash, and a path with no profile name no unit; a record without
/// when its run began, or without its packages, or of another schema, is none.
#[test]
fn the_used_record_names_each_outputs_unit() {
    let leaf = Path::new(LEAF);
    let mut used = Used {
        began_ms: 1_799_990_000_000,
        members: BTreeSet::from(["aterm-grid".to_owned()]),
        ..Used::default()
    };
    for file in [
        format!("{PROFILE}/build/aterm-grid/{NEW}/out/libaterm_grid-{NEW}.rlib"),
        format!("{PROFILE}/build/aterm-grid/{NEW}/out/aterm_grid-{NEW}.d"),
        format!("{PROFILE}/deps/libaterm_grid-{OLD}.rmeta"),
        format!("{PROFILE}/build/aterm-grid-{FRESH}/out/generated.rs"),
        format!("{PROFILE}/build/libc/{LIBC}/out"),
        format!("{LEAF}/aarch64-apple-darwin/debug/build/libc/{NEW}/out/liblibc-{NEW}.rlib"),
        "/c/Users//me/ws/target/debug/aterm".to_owned(),
        format!("{PROFILE}/deps/libaterm_gpu_web.rlib"),
        format!("{LEAF}/deps/libx-{OLD}.rlib"),
    ] {
        used.add(leaf, Path::new(&file));
    }
    let set = |hashes: &[&str]| {
        hashes
            .iter()
            .map(|h| (*h).to_owned())
            .collect::<BTreeSet<_>>()
    };
    assert_eq!(
        used.profiles,
        BTreeMap::from([
            ("aarch64-apple-darwin/debug".to_owned(), set(&[NEW])),
            ("debug".to_owned(), set(&[OLD, NEW, FRESH, LIBC])),
        ])
    );
    assert_eq!(Used::decode(used.encode().as_bytes()), Some(used));
    for torn in [
        &b"{\"schema\":1,\"members\":[],\"profiles\":{}}"[..],
        b"{\"schema\":1,\"began_ms\":1,\"profiles\":{}}",
        b"{\"schema\":1,\"began_ms\":1,\"members\":[\"\"],\"profiles\":{}}",
        b"{\"schema\":2,\"began_ms\":1,\"members\":[],\"profiles\":{}}",
    ] {
        assert_eq!(
            Used::decode(torn),
            None,
            "{}",
            String::from_utf8_lossy(torn)
        );
    }
}

/// `file`'s modification time set to `at`.
#[cfg(unix)]
fn written_at(file: &Path, at: SystemTime) {
    std::fs::File::options()
        .write(true)
        .open(file)
        .unwrap()
        .set_modified(at)
        .unwrap();
}

/// A BIN A TEST NEEDS IS RECORDED BY THE UNIT ITS COPY CAME FROM ([`Used::add_uplifted`]): cargo
/// names it by its copy in the target directory alone, a copy keeping the unit's length and its
/// modification time to the nanosecond; the unit of its package whose `out/` holds that twin under
/// its crate name is recorded, and a hard link to a unit's output is that unit's. NEGATIVE
/// CONTROLS: another build of the same bin, as long but written at another instant, is not
/// recorded; nor is anything for a twin looked for under another package or another profile, or
/// for a package or profile that is no plain name.
#[cfg(unix)]
#[test]
fn a_bin_a_test_needs_is_recorded_by_the_unit_its_copy_came_from() {
    let dir = aterm_tempfile::Builder::new()
        .prefix("stale-uplift-")
        .tempdir()
        .unwrap();
    let leaf = dir.path().join("build");
    let unit = |hash: &str| {
        let out = leaf.join("debug/build/atpkg").join(hash).join("out");
        std::fs::create_dir_all(&out).unwrap();
        let bin = out.join("atpkg_abrew");
        std::fs::write(&bin, b"the bin the tests ran").unwrap();
        bin
    };
    let at = UNIX_EPOCH + Duration::new(1_799_000_000, 123_456_789);
    let ran = unit(NEW);
    written_at(&ran, at);
    let other = unit(OLD);
    written_at(&other, at + Duration::from_nanos(1));
    let target = dir.path().join("target/debug");
    std::fs::create_dir_all(&target).unwrap();
    let copy = target.join("atpkg-abrew");
    std::fs::write(&copy, b"the bin the tests ran").unwrap();
    written_at(&copy, at);

    let mut used = Used::default();
    used.add_uplifted(&leaf, "debug", "atpkg", &copy);
    let debug = |used: &Used| used.profiles.get("debug").cloned().unwrap_or_default();
    assert_eq!(debug(&used), BTreeSet::from([NEW.to_owned()]));

    let linked_unit = unit(FRESH);
    written_at(&linked_unit, at + Duration::from_secs(5));
    let linked = dir.path().join("linked/debug");
    std::fs::create_dir_all(&linked).unwrap();
    std::fs::hard_link(&linked_unit, linked.join("atpkg-abrew")).unwrap();
    used.add_uplifted(&leaf, "debug", "atpkg", &linked.join("atpkg-abrew"));
    assert_eq!(
        debug(&used),
        BTreeSet::from([NEW.to_owned(), FRESH.to_owned()])
    );

    let mut none = Used::default();
    for (profile, package) in [
        ("debug", "aterm"),
        ("aarch64-apple-darwin/debug", "atpkg"),
        ("../debug", "atpkg"),
        ("debug", "../atpkg"),
        ("debug", ""),
    ] {
        none.add_uplifted(&leaf, profile, package, &copy);
    }
    assert_eq!(none, Used::default());
}
