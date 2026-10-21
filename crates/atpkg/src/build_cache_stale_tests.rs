// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

use super::*;
use std::collections::BTreeMap;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

const DAY: Duration = Duration::from_secs(24 * 60 * 60);

/// The clock every case reads: a fixed instant, so ages are exact.
fn now() -> SystemTime {
    UNIX_EPOCH + Duration::from_secs(1_800_000_000)
}

/// An in-memory tree: each path's kind, modification time, bytes and link target.
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

    /// Writes `path` again at `at`: its own time only, as a write into a folder moves the
    /// folder's.
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

/// `.rustc_info.json` as cargo keeps it: `-vV`'s output and the `--print` query's, whose one
/// absolute line is the sysroot.
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

/// A build directory the store's current compiler (build 15698, current since `moved`)
/// last built: in `debug`, one unit the old compiler built (`OLD`, unwritten `old_age`) with
/// its deps, build and examples entries, one the current compiler built since (`NEW`), one
/// the current compiler built fresh (`FRESH`); a stale incremental crate folder and a
/// fresh one.
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
    // Not a unit's: no hash, or a hash no fingerprint carries.
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

/// THE RULE ON A SURVEYED BUILD DIRECTORY: the incremental crate folder nothing wrote for
/// four days goes whole; the unit the old compiler built goes with every entry carrying its
/// hash (deps, its codegen objects, build, examples). NEGATIVE CONTROLS, each kept: the
/// fresh crate folder, the unit the current compiler built (whatever its age), the fresh
/// one, and the entries no fingerprint names.
#[test]
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

/// UNWRITTEN FOR A FEW DAYS, AND ALL OF IT: the old compiler's unit two days unwritten stays
/// (and so does the crate folder of its age); one whose codegen object something wrote
/// today stays whole; one whose fingerprint's compiler cannot be read (two JSONs that
/// disagree, or one that names none) stays.
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

/// WHICH COMPILER IS CURRENT IS READ, NEVER GUESSED: no unit is judged by compiler where
/// cargo last built with a compiler no store holds (a stock toolchain, or a folder named
/// like a build outside a `store`), where `.rustc_info.json` names two sysroots or none,
/// where `current` is a folder and not a link, or, cargo having last built with the
/// current build, in a profile nothing was built in since its link was made (a link laid
/// again over the same build reads that way). The stale crate folder goes in every case:
/// its rule asks no compiler.
#[test]
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
    // Foreign names cannot authorize a read outside the cache, even with the
    // store/program/numeric-build shape. These are captured paths, never OS probes.
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

    // The store moved after the profile's last build: nothing written since names the
    // current compiler, so neither compiler's unit goes.
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

/// Sets the unit `hash`'s fingerprint folder, its files and its entries to `at`.
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

/// THE STORE MOVED AND THIS WORKSPACE DID NOT FOLLOW: cargo last built here with the store's
/// previous build, so every unit no build since the link names goes once stale — the old
/// compiler's and the one before it alike, as the next build rebuilds each. NEGATIVE
/// CONTROL: the very same tree with `.rustc_info.json` naming the current build (its link
/// laid again, say) keeps every unit.
#[test]
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

/// UNDER THE LOCKS, AS SURVEYED OR NOT AT ALL: a crate folder a session was written into
/// since the survey, and a unit rebuilt since (its fingerprint written), are no longer
/// doomed; untouched ones still are. A part that cannot be read is never stale.
#[test]
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

    // An artifact-only write also keeps the unit on the fresh survey, without
    // requiring its fingerprint to have changed.
    let mut artifact = world(now() - 2 * DAY, 4 * DAY);
    let before = decide(&surveyed(&artifact), now());
    assert!(before.iter().any(|d| d.part == Part::Unit));
    artifact.touch(&format!("{PROFILE}/deps/libaterm_grid-{OLD}.rlib"), now());
    assert!(
        decide(&surveyed(&artifact), now())
            .iter()
            .all(|d| d.part == Part::Incremental)
    );

    // A re-publication of Current after the unlocked survey leaves no unit
    // naming the compiler since that link; a fresh survey keeps every unit.
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

/// A unit's hash in each name cargo gives its outputs; nothing else reads as one.
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
