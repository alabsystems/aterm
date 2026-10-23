// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! A wake path's admission.

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

fn under_guard(path: &Path) -> bool {
    let text = path.to_string_lossy().to_ascii_lowercase();
    protected().iter().any(|root| {
        let root = root.to_string_lossy().to_ascii_lowercase();
        let inside = text == root || text.starts_with(&format!("{root}/"));
        inside && !(root == "/users" && (text == "/users/u" || text.starts_with("/users/u/")))
    })
}

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
