// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! A wake path's admission, one fixture per refusal word, with a probe that records every path it
//! is asked.

use std::cell::RefCell;
use std::collections::BTreeMap;

use super::*;
use crate::supervise::occupancy::fault::{self, Fault};

const HOME: &str = "/Users//u";

/// A probe that answers from a table (missing elsewhere) and records every path asked.
#[derive(Default)]
struct Recorder {
    at: BTreeMap<PathBuf, Probed>,
    asked: RefCell<Vec<PathBuf>>,
}

impl PathProbe for Recorder {
    fn probe(&self, path: &Path) -> Probed {
        self.asked.borrow_mut().push(path.to_path_buf());
        self.at.get(path).copied().unwrap_or(Probed::Missing)
    }
}

/// The privacy-guarded roots, as the window's host would hand them: the home's guarded folders,
/// the volumes, and the folder of every user's home.
fn protected() -> Vec<PathBuf> {
    [
        "/Users//u/Documents",
        "/Users//u/Desktop",
        "/Users//u/Library/Mail",
        "/Volumes",
        "/Users",
    ]
    .map(PathBuf::from)
    .to_vec()
}

/// The session's roots: its tab sits in the home itself (no root), a job runs in a project.
fn session() -> Roots {
    roots(
        Path::new(HOME),
        &[PathBuf::from(HOME), PathBuf::from("/Users//u/proj")],
        Path::new("/Users//u/scratch.noindex"),
        &protected(),
    )
}

fn admit_with(
    path: &str,
    home_relative: bool,
    mode: ContainmentWord,
    probe: &Recorder,
) -> Result<AdmittedPath, PathRefusal> {
    admit(path, home_relative, &session(), &protected(), mode, probe)
}

/// Whether `path` lies under a guarded root, as the owner's screen would see a `stat` there (the
/// home itself excepted from the folder of every home).
fn under_guard(path: &Path) -> bool {
    let text = path.to_string_lossy().to_ascii_lowercase();
    protected().iter().any(|root| {
        let root = root.to_string_lossy().to_ascii_lowercase();
        let inside = text == root || text.starts_with(&format!("{root}/"));
        inside && !(root == "/users" && (text == "/users/u" || text.starts_with("/users/u/")))
    })
}

/// EACH REFUSAL WORD, ONE FIXTURE EACH, and the probe is asked nothing for any of them: a dot
/// folder, a credential's name (either case), a guarded folder (either case), a volume, another
/// user's home, the home's guarded folders and its scratch under the data volume's spelling, a
/// guarded folder spelled with a long s or a Kelvin sign, a path under the home but no root (the
/// tab's cwd is the home, never a root), the home's guarded folder under another case of the
/// home, a relative path, a path that climbs, the safety and sandbox containments, a path one
/// byte too long and a control character.
#[test]
fn each_lexical_refusal_is_decided_before_any_probe() {
    use ContainmentWord::{Containment, Safety, User};
    let long = format!("scratch.noindex/{}", "a".repeat(PATH_MAX - 15));
    assert_eq!(long.len(), PATH_MAX + 1);
    for (path, home_relative, mode, want) in [
        ("scratch.noindex/x/.git/HEAD", true, User, PathRefusal::Dot),
        (
            "scratch.noindex/x/id_ed25519",
            true,
            User,
            PathRefusal::Credential,
        ),
        (
            "scratch.noindex/x/api_token.txt",
            true,
            User,
            PathRefusal::Credential,
        ),
        (
            "scratch.noindex/x/Release.KEY",
            true,
            User,
            PathRefusal::Credential,
        ),
        ("proj/secrets/x.log", true, User, PathRefusal::Credential),
        ("Documents/x", true, User, PathRefusal::Guarded),
        ("documents/x", true, User, PathRefusal::Guarded),
        ("/Volumes/x", false, User, PathRefusal::Guarded),
        ("/Users//other/x", false, User, PathRefusal::Guarded),
        (
            "/System/Volumes/Data/Users//u/Documents/x",
            false,
            User,
            PathRefusal::Guarded,
        ),
        (
            "/system/volumes/data/Users//u/scratch.noindex/x",
            false,
            User,
            PathRefusal::Guarded,
        ),
        ("De\u{17f}ktop/x", true, User, PathRefusal::Unicode),
        ("Des\u{212a}top/x", true, User, PathRefusal::Unicode),
        (
            "scratch.noindex/caf\u{e9}/x",
            true,
            User,
            PathRefusal::Unicode,
        ),
        ("notes.txt", true, User, PathRefusal::Outside),
        (
            "/users/U/Library/Messages/chat.db",
            false,
            User,
            PathRefusal::Outside,
        ),
        ("/opt/x/y", false, User, PathRefusal::Outside),
        ("scratch.noindex/x", false, User, PathRefusal::Relative),
        (
            "scratch.noindex/../Documents/x",
            true,
            User,
            PathRefusal::Dot,
        ),
        (
            "scratch.noindex/run/a.log",
            true,
            Containment,
            PathRefusal::Containment,
        ),
        (
            "scratch.noindex/run/a.log",
            true,
            Safety,
            PathRefusal::Containment,
        ),
        (long.as_str(), true, User, PathRefusal::Long),
        ("scratch.noindex/a\u{7}b", true, User, PathRefusal::Control),
    ] {
        let probe = Recorder::default();
        assert_eq!(
            admit_with(path, home_relative, mode, &probe),
            Err(want),
            "{path:?}"
        );
        assert!(probe.asked.borrow().is_empty(), "{path:?} was probed");
    }
}

/// AN ADMITTED PATH IS PROBED FROM THE HOME DOWN, never under a guarded root, and only as far as
/// it exists: `~/scratch.noindex/run/logs/a.log`, its `run` folder not made yet, is admitted under
/// the scratch root after three probes; a path under the job's project is admitted under that
/// root; one under a root beside the home is probed from the filesystem's root; a path of
/// [`PATH_MAX`] bytes is not too long. NEGATIVE CONTROL: with `CanonicalizeAtRead` (the probe asked before the judgement), a
/// refused path under `~/Documents` is touched.
#[test]
fn an_admitted_path_is_probed_from_the_home_down_and_a_refused_one_never() {
    let mut probe = Recorder::default();
    for dir in [HOME, "/Users//u/scratch.noindex"] {
        probe.at.insert(PathBuf::from(dir), Probed::Dir);
    }
    let admitted = admit_with(
        "scratch.noindex/run/logs/a.log",
        true,
        ContainmentWord::User,
        &probe,
    );
    assert_eq!(
        admitted,
        Ok(AdmittedPath {
            path: PathBuf::from("/Users//u/scratch.noindex/run/logs/a.log"),
            root: PathBuf::from("/Users//u/scratch.noindex"),
        })
    );
    assert_eq!(
        *probe.asked.borrow(),
        [
            "/Users//u",
            "/Users//u/scratch.noindex",
            "/Users//u/scratch.noindex/run"
        ]
        .map(PathBuf::from)
    );
    let project = Recorder::default();
    let admitted = admit_with(
        "/Users//u/proj/out/done",
        false,
        ContainmentWord::User,
        &project,
    );
    assert_eq!(admitted.map(|a| a.root), Ok(PathBuf::from("/Users//u/proj")));
    for asked in project
        .asked
        .borrow()
        .iter()
        .chain(probe.asked.borrow().iter())
    {
        assert!(!under_guard(asked), "{asked:?}");
    }

    let beside = Recorder::default();
    let opt = roots(
        Path::new(HOME),
        &[PathBuf::from("/opt/build")],
        Path::new("/Users//u/scratch.noindex"),
        &protected(),
    );
    assert_eq!(
        admit(
            "/opt/build/out/done",
            false,
            &opt,
            &protected(),
            ContainmentWord::User,
            &beside
        )
        .map(|a| a.root),
        Ok(PathBuf::from("/opt/build"))
    );
    assert_eq!(
        beside.asked.borrow().first(),
        Some(&PathBuf::from("/opt")),
        "a root beside the home is probed from the filesystem's root"
    );
    let longest = format!("scratch.noindex/{}", "a".repeat(PATH_MAX - 16));
    assert_eq!(longest.len(), PATH_MAX);
    assert!(
        admit_with(&longest, true, ContainmentWord::User, &Recorder::default()).is_ok(),
        "{PATH_MAX} bytes is not too long"
    );

    let touched = Recorder::default();
    let refused = fault::with(Fault::CanonicalizeAtRead, || {
        admit_with("Documents/x", true, ContainmentWord::User, &touched)
    });
    assert_eq!(refused, Err(PathRefusal::Guarded));
    assert!(
        touched.asked.borrow().iter().any(|p| under_guard(p)),
        "{:?}",
        touched.asked.borrow()
    );
}

/// A LINK ANYWHERE FROM THE HOME DOWN IS REFUSED, and the probe goes no further than the link.
/// CONTROL: the same folder as a folder is admitted.
#[test]
fn a_link_on_the_way_is_refused() {
    let mut probe = Recorder::default();
    for (dir, kind) in [
        (HOME, Probed::Dir),
        ("/Users//u/scratch.noindex", Probed::Dir),
        ("/Users//u/scratch.noindex/run", Probed::Link),
    ] {
        probe.at.insert(PathBuf::from(dir), kind);
    }
    assert_eq!(
        admit_with(
            "scratch.noindex/run/a.log",
            true,
            ContainmentWord::User,
            &probe
        ),
        Err(PathRefusal::Link)
    );
    assert_eq!(
        probe.asked.borrow().last(),
        Some(&PathBuf::from("/Users//u/scratch.noindex/run"))
    );
    probe
        .at
        .insert(PathBuf::from("/Users//u/scratch.noindex/run"), Probed::Dir);
    probe.at.insert(
        PathBuf::from("/Users//u/scratch.noindex/run/a.log"),
        Probed::Link,
    );
    assert_eq!(
        admit_with(
            "scratch.noindex/run/a.log",
            true,
            ContainmentWord::User,
            &probe
        ),
        Err(PathRefusal::Link),
        "the leaf a link"
    );
    probe.at.insert(
        PathBuf::from("/Users//u/scratch.noindex/run/a.log"),
        Probed::File,
    );
    assert!(
        admit_with(
            "scratch.noindex/run/a.log",
            true,
            ContainmentWord::User,
            &probe
        )
        .is_ok()
    );
}

/// THE ROOTS: the scratch folder and each working directory, less the home itself, any folder
/// above it, any under a guarded root (either case), a volume, a relative one and a dot folder;
/// each once. The home and its guarded folders spelled another way are no roots either: the home
/// in another case, the data volume's spelling of the home, of the folder of every home and of a
/// project in it, and a guarded folder spelled with a long s or a Kelvin sign; so is any folder
/// named outside ASCII. CONTROL: a project folder and one beside the home are roots.
#[test]
fn a_cwd_at_or_above_home_or_guarded_is_no_root() {
    let cwds = [
        HOME,
        "/Users",
        "/",
        "/Users//u/Documents/proj",
        "/Users//u/documents/proj",
        "/Volumes/ext/proj",
        "proj",
        "/Users//u/.config/x",
        "/users/U",
        "/USERS",
        "/System/Volumes/Data/Users//u",
        "/System/Volumes/Data/Users",
        "/system/volumes/data/Users//u/proj",
        "/Users//u/De\u{17f}ktop/proj",
        "/Users//u/Des\u{212a}top/proj",
        "/Users//u/caf\u{e9}",
        "/Users//u/proj",
        "/Users//u/proj",
        "/opt/build",
    ]
    .map(PathBuf::from);
    let roots = roots(
        Path::new(HOME),
        &cwds,
        Path::new("/Users//u/scratch.noindex"),
        &protected(),
    );
    assert_eq!(
        roots.dirs,
        ["/Users//u/scratch.noindex", "/Users//u/proj", "/opt/build"].map(PathBuf::from)
    );
}

/// A HOME NAMED OUTSIDE ASCII KEEPS ITS ROOTS, spelled as the home is: its scratch folder and a
/// project in it are roots, the same home spelled decomposed is no root, and a path under the
/// home's own spelling is admitted. CONTROL: the decomposed spelling of a path is refused.
#[test]
fn a_home_named_outside_ascii_keeps_its_own_spelling() {
    let home = "/Users//jos\u{e9}";
    let protected: Vec<PathBuf> = ["/Users//jos\u{e9}/Documents", "/Users"]
        .map(PathBuf::from)
        .to_vec();
    let roots = roots(
        Path::new(home),
        &[
            PathBuf::from("/Users//jos\u{e9}/proj"),
            PathBuf::from("/Users//jose\u{301}/proj"),
        ],
        Path::new("/Users//jos\u{e9}/scratch.noindex"),
        &protected,
    );
    assert_eq!(
        roots.dirs,
        ["/Users//jos\u{e9}/scratch.noindex", "/Users//jos\u{e9}/proj"].map(PathBuf::from)
    );
    let probe = Recorder::default();
    let judge = |path: &str, home_relative: bool| {
        admit(
            path,
            home_relative,
            &roots,
            &protected,
            ContainmentWord::User,
            &probe,
        )
    };
    assert!(judge("scratch.noindex/run/a.log", true).is_ok());
    assert!(judge("/Users//jos\u{e9}/proj/out", false).is_ok());
    assert_eq!(
        judge("/Users//jose\u{301}/proj/out", false),
        Err(PathRefusal::Unicode)
    );
    assert_eq!(judge("Documents/x", true), Err(PathRefusal::Guarded));
}

/// THE HOME'S ALIASES OPEN NO GUARDED FOLDER (the 2.1 safety review's finding): a tab that says
/// its cwd is the home in another case, the home under the data volume's spelling, the folder of
/// every home there, or a guarded folder spelled with a long s, makes no root of it, and a path
/// into the home's guarded folders through any of those spellings is refused with nothing probed.
/// CONTROL: the same session admits `~/scratch.noindex/run/logs/a.log`, probed from the home down.
#[test]
fn an_alias_of_the_home_opens_no_guarded_folder() {
    let aliases = [
        "/users/U",
        "/System/Volumes/Data/Users//u",
        "/System/Volumes/Data/Users",
        "/Users//u/De\u{17f}ktop",
    ]
    .map(PathBuf::from);
    let session = roots(
        Path::new(HOME),
        &aliases,
        Path::new("/Users//u/scratch.noindex"),
        &protected(),
    );
    assert_eq!(session.dirs, [PathBuf::from("/Users//u/scratch.noindex")]);
    for path in [
        "/users/U/Documents/x",
        "/users/u/Library/Messages/chat.db",
        "/System/Volumes/Data/Users//u/Documents/x",
        "/System/Volumes/Data/Users//other/x",
        "/Users//u/De\u{17f}ktop/x",
    ] {
        let probe = Recorder::default();
        let admitted = admit(
            path,
            false,
            &session,
            &protected(),
            ContainmentWord::User,
            &probe,
        );
        assert!(admitted.is_err(), "{path:?} admitted: {admitted:?}");
        assert!(probe.asked.borrow().is_empty(), "{path:?} was probed");
    }
    let probe = Recorder::default();
    assert_eq!(
        admit(
            "scratch.noindex/run/logs/a.log",
            true,
            &session,
            &protected(),
            ContainmentWord::User,
            &probe,
        )
        .map(|a| a.root),
        Ok(PathBuf::from("/Users//u/scratch.noindex"))
    );
    assert_eq!(probe.asked.borrow().first(), Some(&PathBuf::from(HOME)));
}

/// THE LONGEST ROOT A PATH IS UNDER IS ITS ROOT: a job's folder inside the tab's.
#[test]
fn a_path_is_under_its_longest_root() {
    let nested = roots(
        Path::new(HOME),
        &[
            PathBuf::from("/Users//u/proj"),
            PathBuf::from("/Users//u/proj/sub"),
        ],
        Path::new("/Users//u/scratch.noindex"),
        &protected(),
    );
    let admitted = admit(
        "/Users//u/proj/sub/out/done",
        false,
        &nested,
        &protected(),
        ContainmentWord::User,
        &Recorder::default(),
    );
    assert_eq!(
        admitted.map(|a| a.root),
        Ok(PathBuf::from("/Users//u/proj/sub"))
    );
}
