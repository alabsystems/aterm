// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Worktree registrations whose folders are gone, in a scratch home: a main checkout with a
//! submodule, `origin/main` at its commit, and linked worktrees removed by hand. Git is the real
//! one; every repository is in the scratch folder.

use std::path::{Path, PathBuf};

use super::super::checkout::Placing;
use super::super::retired::{Git as _, GitCli};
use super::{Custody, Pass, Pin, Registration, drop_gone};

/// `git <args>` in `dir`, as a person runs it, which must succeed.
fn git(dir: &Path, args: &[&str]) {
    let a = attempt(dir, args);
    assert_eq!(
        a.code,
        Some(0),
        "git {args:?} in {}: {}",
        dir.display(),
        a.err
    );
}

/// `git <args>` in `dir`, as a person runs it, whatever it answers.
fn attempt(dir: &Path, args: &[&str]) -> super::super::retired::Answer {
    let mut all = vec![
        "-c",
        "user.name=t",
        "-c",
        "user.email=t@example.invalid",
        "-c",
        "commit.gpgsign=false",
        "-c",
        "protocol.file.allow=always",
        "-c",
        "init.defaultBranch=main",
    ];
    all.extend_from_slice(args);
    GitCli.run(dir, &all)
}

/// What `git <args>` in `dir` printed, trimmed; it must succeed.
fn read(dir: &Path, args: &[&str]) -> String {
    let a = GitCli.run(dir, args);
    assert_eq!(
        a.code,
        Some(0),
        "git {args:?} in {}: {}",
        dir.display(),
        a.err
    );
    a.out.trim().to_owned()
}

/// Whether `object` is in the repository of the checkout `dir`.
fn has(dir: &Path, object: &str) -> bool {
    GitCli.run(dir, &["cat-file", "-e", object]).code == Some(0)
}

/// Whether `object` is in the module store `store` of `checkout`: read with `checkout` as its
/// work tree, since the store's own may be gone.
fn stored(checkout: &Path, store: &Path, object: &str) -> bool {
    let git_dir = format!("--git-dir={}", store.display());
    let work_tree = format!("--work-tree={}", checkout.display());
    GitCli
        .run(checkout, &[&git_dir, &work_tree, "cat-file", "-e", object])
        .code
        == Some(0)
}

struct Home {
    _dir: aterm_tempfile::TempDir,
    home: PathBuf,
}

impl Home {
    fn checkout(&self) -> PathBuf {
        self.home.join("aterm")
    }

    fn witness(&self) -> PathBuf {
        self.checkout().join(".git/modules/vendor/astream")
    }

    /// A linked worktree of the checkout at `rel` under the home, detached at `HEAD`, with git's
    /// entry for it.
    fn worktree(&self, rel: &str) -> (PathBuf, PathBuf) {
        let wt = self.home.join(rel);
        git(
            &self.checkout(),
            &[
                "worktree",
                "add",
                "-q",
                "--detach",
                wt.to_str().expect("utf-8"),
                "HEAD",
            ],
        );
        let entry = PathBuf::from(read(&wt, &["rev-parse", "--absolute-git-dir"]));
        assert!(entry.starts_with(self.checkout().join(".git/worktrees")));
        (wt, entry)
    }

    /// The folders git lists as worktrees of the checkout.
    fn listed(&self) -> Vec<PathBuf> {
        read(&self.checkout(), &["worktree", "list", "--porcelain"])
            .lines()
            .filter_map(|l| l.strip_prefix("worktree "))
            .map(PathBuf::from)
            .collect()
    }

    fn pass(&self, placing: Option<&Placing>, custody: &mut Seen) -> Pass {
        drop_gone(
            &GitCli,
            &self.checkout(),
            Some(&self.home),
            placing,
            custody,
        )
        .expect("the checkout's registrations read")
    }
}

/// A scratch home: `astream`, a repository with one commit, and `aterm`, a main checkout with
/// it as the submodule `vendor/astream` (its canonical module store in `.git/modules`), one
/// commit, and `origin/main` there.
fn home() -> Home {
    let dir = aterm_tempfile::Builder::new()
        .prefix("gone-entries-")
        .tempdir()
        .expect("scratch");
    let home = std::fs::canonicalize(dir.path())
        .expect("resolved")
        .join("home");
    let sub = home.join("astream");
    let checkout = home.join("aterm");
    for repo in [&sub, &checkout] {
        std::fs::create_dir_all(repo).expect("a repo");
        git(repo, &["init", "-q"]);
    }
    std::fs::write(sub.join("lib.rs"), "// sub\n").expect("written");
    git(&sub, &["add", "lib.rs"]);
    git(&sub, &["commit", "-qm", "sub"]);
    std::fs::write(checkout.join("Cargo.toml"), "[workspace]\n").expect("written");
    git(&checkout, &["add", "Cargo.toml"]);
    git(
        &checkout,
        &[
            "submodule",
            "add",
            "-q",
            sub.to_str().expect("utf-8"),
            "vendor/astream",
        ],
    );
    git(&checkout, &["commit", "-qm", "main"]);
    git(
        &checkout,
        &["update-ref", "refs/remotes/origin/main", "HEAD"],
    );
    // The folder the worktrees are in holds more than them, as a folder of worktrees does: one
    // that stands empty is where a volume not mounted now may be mounted.
    std::fs::create_dir_all(home.join("wts")).expect("a folder");
    std::fs::write(home.join("wts/README"), "worktrees\n").expect("written");
    Home { _dir: dir, home }
}

/// What the pass asked of its caller, each call checked by `check` as it is made.
struct Seen {
    asked: Vec<(&'static str, Registration, Vec<Pin>)>,
    check: Box<dyn FnMut(&'static str, &Registration, &[Pin])>,
    refuse_pins: bool,
    refuse_drop: bool,
}

impl Seen {
    fn new(check: impl FnMut(&'static str, &Registration, &[Pin]) + 'static) -> Self {
        Self {
            asked: Vec::new(),
            check: Box::new(check),
            refuse_pins: false,
            refuse_drop: false,
        }
    }

    fn quiet() -> Self {
        Self::new(|_, _, _| {})
    }

    fn calls(&self) -> Vec<&'static str> {
        self.asked.iter().map(|(what, _, _)| *what).collect()
    }
}

impl Custody for Seen {
    fn more(&mut self) -> bool {
        true
    }

    fn pinning(&mut self, gone: &Registration, pins: &[Pin]) -> bool {
        (self.check)("pinning", gone, pins);
        self.asked.push(("pinning", gone.clone(), pins.to_vec()));
        !self.refuse_pins
    }

    fn dropping(&mut self, gone: &Registration, pins: &[Pin]) -> Result<(), String> {
        (self.check)("dropping", gone, pins);
        self.asked.push(("dropping", gone.clone(), pins.to_vec()));
        if self.refuse_drop {
            Err("its pins' pushes could not be recorded".to_owned())
        } else {
            Ok(())
        }
    }
}

/// Whether `pin` names its commit in its repository.
fn stands(checkout: &Path, pin: &Pin) -> bool {
    let mut args = Vec::new();
    let git_dir = format!("--git-dir={}", pin.repo.display());
    if pin.repo != checkout {
        args.push(git_dir.as_str());
    }
    args.extend(["rev-parse", "--verify", "--quiet", pin.name.as_str()]);
    let a = GitCli.run(&pin.repo, &args);
    a.code == Some(0) && a.out.trim() == pin.oid
}

#[test]
fn a_registration_whose_folder_is_gone_is_pinned_then_dropped() {
    let h = home();
    let checkout = h.checkout();
    let (live, live_entry) = h.worktree("wts/live.noindex");
    let (wt, entry) = h.worktree("wts/one.noindex");
    // Its HEAD: a commit only it holds.
    std::fs::write(wt.join("one.txt"), "one\n").expect("written");
    git(&wt, &["add", "one.txt"]);
    git(&wt, &["commit", "-qm", "only its HEAD holds this"]);
    let head = read(&wt, &["rev-parse", "HEAD"]);
    // A commit only its reflog holds.
    std::fs::write(wt.join("two.txt"), "two\n").expect("written");
    git(&wt, &["add", "two.txt"]);
    git(&wt, &["commit", "-qm", "only its reflog holds this"]);
    let reflogged = read(&wt, &["rev-parse", "HEAD"]);
    git(&wt, &["reset", "-q", "--hard", &head]);
    // A commit only its own ref holds.
    let owned = read(
        &wt,
        &[
            "-c",
            "user.name=t",
            "-c",
            "user.email=t@example.invalid",
            "commit-tree",
            "HEAD^{tree}",
            "-p",
            "HEAD",
            "-m",
            "only its own ref holds this",
        ],
    );
    git(&wt, &["update-ref", "refs/worktree/kept", &owned]);
    assert!(entry.join("refs/worktree/kept").is_file());
    // What only its index staged.
    std::fs::write(wt.join("staged.txt"), "staged and never committed\n").expect("written");
    git(&wt, &["add", "staged.txt"]);
    let staged = read(&wt, &["rev-parse", ":staged.txt"]);
    // Negative control: nothing but the entry holds any of them.
    let lasting = read(
        &checkout,
        &["rev-list", "--objects", "--branches", "--tags", "--remotes"],
    );
    for object in [&head, &reflogged, &owned, &staged] {
        assert!(
            !lasting.contains(object.as_str()),
            "{object} is held already"
        );
    }
    std::fs::remove_dir_all(&wt).expect("its folder removed by hand");

    let witness_entry = entry.clone();
    let repo = checkout.clone();
    let mut seen = Seen::new(move |what, gone, pins| {
        // Pin before drop: git's entry stands while the pins are put on record and once they
        // stand; a pin is written only after its intent.
        assert!(
            witness_entry.is_dir(),
            "{what}: the entry went before its pins"
        );
        assert_eq!(gone.entry, witness_entry);
        for pin in pins {
            assert!(pin.name.starts_with("refs/orphans/one/"), "{}", pin.name);
            assert_eq!(pin.repo, repo);
            assert_eq!(
                stands(&repo, pin),
                what == "dropping",
                "{what}: {}",
                pin.name
            );
        }
    });
    let pass = h.pass(None, &mut seen);
    assert_eq!(seen.calls(), ["pinning", "dropping"], "{pass:#?}");
    assert!(pass.kept.is_empty(), "{pass:#?}");
    assert_eq!(pass.dropped.len(), 1, "{pass:#?}");
    let (gone, pins) = &pass.dropped[0];
    assert_eq!(gone.folder, wt);
    assert_eq!(gone.head.as_deref(), Some(head.as_str()));
    assert!(!pins.is_empty());
    assert!(!entry.exists(), "its entry went");
    assert_eq!(h.listed(), [checkout.clone(), live.clone()]);
    assert!(
        live_entry.is_dir(),
        "a live worktree's entry is not touched"
    );

    // The pins alone now hold what it held: git's collection keeps every object.
    git(&checkout, &["gc", "-q", "--prune=now"]);
    let pinned = read(
        &checkout,
        &["rev-list", "--objects", "--glob=refs/orphans/*"],
    );
    for object in [&head, &reflogged, &owned, &staged] {
        assert!(has(&checkout, object), "{object} was lost");
        assert!(pinned.contains(object.as_str()), "{object} is on no pin");
    }

    // A second pass finds nothing more to do.
    let mut again = Seen::quiet();
    let pass = h.pass(None, &mut again);
    assert!(pass.dropped.is_empty() && pass.kept.is_empty(), "{pass:#?}");
    assert!(again.asked.is_empty());
}

#[test]
fn a_gone_registrations_private_submodule_work_is_pinned_before_its_store_goes() {
    let h = home();
    let checkout = h.checkout();
    let witness = h.witness();
    let (wt, entry) = h.worktree("wts/two.noindex");
    git(&wt, &["submodule", "update", "-q", "--init"]);
    let store = entry.join("modules/vendor/astream");
    assert!(store.join("objects").is_dir(), "a private module store");
    let sub = wt.join("vendor/astream");
    std::fs::write(sub.join("lib.rs"), "// only the private store holds this\n").expect("written");
    git(&sub, &["commit", "-qam", "private submodule work"]);
    let private = read(&sub, &["rev-parse", "HEAD"]);
    let before = read(&sub, &["rev-parse", "HEAD~1"]);
    assert!(
        !stored(&checkout, &witness, &private),
        "negative control: the witness lacks it"
    );
    std::fs::remove_dir_all(&wt).expect("its folder removed by hand");

    // A store that cannot take its pin keeps the entry and the private store.
    let pin = format!("refs/orphans/two/{}", &private[..12]);
    git(&witness, &["update-ref", &pin, &before]);
    let mut seen = Seen::quiet();
    let pass = h.pass(None, &mut seen);
    assert!(pass.dropped.is_empty(), "{pass:#?}");
    assert_eq!(pass.kept.len(), 1, "{pass:#?}");
    assert!(pass.kept[0].1.contains(&pin), "{pass:#?}");
    assert_eq!(seen.calls(), ["pinning"]);
    assert!(stored(&checkout, &store, &private), "its store stays whole");
    git(&witness, &["update-ref", "-d", &pin, &before]);

    let (w, s, p) = (witness.clone(), store.clone(), private.clone());
    let mut seen = Seen::new(move |what, _, pins| {
        assert!(
            s.join("objects").is_dir(),
            "{what}: the store went before its pin"
        );
        assert_eq!(pins.len(), 1);
        assert_eq!(pins[0].repo, w);
        assert_eq!(pins[0].oid, p);
        let rooted = GitCli
            .run(
                &w,
                &[
                    &format!("--git-dir={}", w.display()),
                    "rev-parse",
                    "--verify",
                    "--quiet",
                    &pins[0].name,
                ],
            )
            .out;
        assert_eq!(rooted.trim() == p, what == "dropping", "{what}");
    });
    let pass = h.pass(None, &mut seen);
    assert_eq!(seen.calls(), ["pinning", "dropping"], "{pass:#?}");
    assert_eq!(pass.dropped.len(), 1, "{pass:#?}");
    assert!(
        !entry.exists() && !store.exists(),
        "the entry and its store went"
    );
    assert_eq!(h.listed(), [checkout.clone()]);
    assert_eq!(
        read(
            &checkout,
            &[
                &format!("--git-dir={}", witness.display()),
                "rev-parse",
                &pin
            ]
        ),
        private
    );
    git(
        &checkout,
        &[
            &format!("--git-dir={}", witness.display()),
            "gc",
            "-q",
            "--prune=now",
        ],
    );
    assert!(
        stored(&checkout, &witness, &private),
        "the canonical store keeps it"
    );
}

#[test]
fn a_locked_or_present_or_away_registration_stays() {
    let h = home();
    let checkout = h.checkout();
    let (present, present_entry) = h.worktree("wts/present.noindex");
    // A standing folder without a manifest is one the build cache's rule alone calls gone: only
    // its standing keeps it.
    std::fs::remove_file(present.join("Cargo.toml")).expect("removed");
    let (locked, locked_entry) = h.worktree("wts/locked.noindex");
    git(
        &checkout,
        &["worktree", "lock", locked.to_str().expect("utf-8")],
    );
    std::fs::remove_dir_all(&locked).expect("removed");
    // Away: its folder and the folder that held it are both gone, as a volume not mounted now.
    let (away, away_entry) = h.worktree("vol/away.noindex");
    std::fs::remove_dir_all(h.home.join("vol")).expect("removed");

    let mut seen = Seen::quiet();
    let pass = h.pass(None, &mut seen);
    assert!(pass.dropped.is_empty() && pass.kept.is_empty(), "{pass:#?}");
    assert!(seen.asked.is_empty(), "nothing was pinned or put on record");
    for entry in [&present_entry, &locked_entry, &away_entry] {
        assert!(entry.is_dir(), "{} stays", entry.display());
    }

    // Negative controls: each stays for that reason alone.
    // A volume mounted between keeps the away one away …
    let home_dev = {
        use std::os::unix::fs::MetadataExt as _;
        std::fs::metadata(&h.home).expect("home").dev()
    };
    let mounted = Placing {
        home_dev,
        mounts: vec![h.home.join("vol")],
    };
    let pass = h.pass(Some(&mounted), &mut Seen::quiet());
    assert!(pass.dropped.is_empty(), "{pass:#?}");
    assert!(away_entry.is_dir());
    // … and the home volume placing it with no mount between makes it gone.
    let placed = Placing {
        home_dev,
        mounts: Vec::new(),
    };
    let pass = h.pass(Some(&placed), &mut Seen::quiet());
    let went: Vec<&Path> = pass
        .dropped
        .iter()
        .map(|(r, _)| r.folder.as_path())
        .collect();
    assert_eq!(went, [away.as_path()], "{pass:#?}");
    assert!(!away_entry.exists());
    assert!(locked_entry.is_dir() && present_entry.is_dir());
    // Unlocked, the locked one goes.
    git(
        &checkout,
        &["worktree", "unlock", locked.to_str().expect("utf-8")],
    );
    let pass = h.pass(None, &mut Seen::quiet());
    let went: Vec<&Path> = pass
        .dropped
        .iter()
        .map(|(r, _)| r.folder.as_path())
        .collect();
    assert_eq!(went, [locked.as_path()], "{pass:#?}");
    assert!(!locked_entry.exists() && present_entry.is_dir());
    // Removed, the present one goes.
    std::fs::remove_dir_all(&present).expect("removed");
    let pass = h.pass(None, &mut Seen::quiet());
    let went: Vec<&Path> = pass
        .dropped
        .iter()
        .map(|(r, _)| r.folder.as_path())
        .collect();
    assert_eq!(went, [present.as_path()], "{pass:#?}");
    assert!(!present_entry.exists());
    assert_eq!(h.listed(), [checkout]);
}

#[test]
fn a_registration_whose_pins_are_not_on_record_stays_unpinned() {
    let h = home();
    let checkout = h.checkout();
    let (wt, entry) = h.worktree("wts/three.noindex");
    std::fs::write(wt.join("three.txt"), "three\n").expect("written");
    git(&wt, &["add", "three.txt"]);
    git(&wt, &["commit", "-qm", "only its HEAD holds this"]);
    let head = read(&wt, &["rev-parse", "HEAD"]);
    std::fs::remove_dir_all(&wt).expect("removed");
    let mut seen = Seen::quiet();
    seen.refuse_pins = true;
    let pass = h.pass(None, &mut seen);
    assert!(pass.dropped.is_empty(), "{pass:#?}");
    assert_eq!(pass.kept.len(), 1, "{pass:#?}");
    assert_eq!(seen.calls(), ["pinning"]);
    assert!(entry.is_dir());
    assert_eq!(
        read(&checkout, &["for-each-ref", "refs/orphans/"]),
        "",
        "no pin was written without its intent on record"
    );
    assert_eq!(
        read(
            &checkout,
            &[
                &format!("--git-dir={}", entry.display()),
                "rev-parse",
                "HEAD"
            ]
        ),
        head
    );
}

/// Stop a rebase of `dir`'s last commit under way, with what was changed and staged put in its
/// autostash: `rebase-merge/autostash` of its git directory, which no ref names. Returns that
/// stash.
fn rebase_stopped_with_autostash(dir: &Path, git_dir: &Path, file: &str) -> String {
    std::fs::write(dir.join(file), "changed and never committed\n").expect("written");
    std::fs::write(dir.join("staged.txt"), "staged and never committed\n").expect("written");
    git(dir, &["add", "staged.txt"]);
    let stopped = attempt(
        dir,
        &[
            "rebase",
            "--autostash",
            "--force-rebase",
            "--exec",
            "false",
            "HEAD~1",
        ],
    );
    assert_ne!(stopped.code, Some(0), "the rebase stops at its exec");
    let stash = std::fs::read_to_string(git_dir.join("rebase-merge/autostash"))
        .expect("its autostash")
        .trim()
        .to_owned();
    assert_eq!(stash.len(), 40, "{stash}");
    stash
}

#[test]
fn an_autostash_and_an_annotated_tag_only_a_gone_registration_names_are_pinned() {
    let h = home();
    let checkout = h.checkout();
    let (wt, entry) = h.worktree("wts/four.noindex");
    for n in ["a", "b"] {
        std::fs::write(wt.join("four.txt"), format!("{n}\n")).expect("written");
        git(&wt, &["add", "four.txt"]);
        git(&wt, &["commit", "-qm", n]);
    }
    // An annotated tag of a commit only it names, on a ref of its own.
    let tagged = read(
        &wt,
        &[
            "-c",
            "user.name=t",
            "-c",
            "user.email=t@example.invalid",
            "commit-tree",
            "HEAD^{tree}",
            "-m",
            "only its tag holds this",
        ],
    );
    git(&wt, &["tag", "-a", "-m", "a tag", "t", &tagged]);
    let tag = read(&wt, &["rev-parse", "refs/tags/t"]);
    assert_ne!(tag, tagged);
    git(&wt, &["update-ref", "refs/worktree/tagged", &tag]);
    git(&wt, &["tag", "-d", "t"]);
    let stash = rebase_stopped_with_autostash(&wt, &entry, "four.txt");
    // Negative control: nothing but the entry holds any of them, and no ref or reflog names the
    // stash at all.
    let lasting = read(
        &checkout,
        &["rev-list", "--objects", "--branches", "--tags", "--remotes"],
    );
    for object in [&tag, &tagged, &stash] {
        assert!(!lasting.contains(object.as_str()), "{object} is held");
    }
    let named = read(&checkout, &["rev-list", "--all", "--reflog"]);
    assert!(!named.contains(stash.as_str()), "a ref names the stash");
    std::fs::remove_dir_all(&wt).expect("its folder removed by hand");

    let mut seen = Seen::quiet();
    let pass = h.pass(None, &mut seen);
    assert_eq!(pass.dropped.len(), 1, "{pass:#?}");
    assert!(!entry.exists(), "its entry went");
    let pins = &pass.dropped[0].1;
    assert!(
        pins.iter().any(|p| p.oid == tag),
        "the tag itself: {pins:#?}"
    );
    assert!(pins.iter().any(|p| p.oid == stash), "the stash: {pins:#?}");
    git(&checkout, &["gc", "-q", "--prune=now"]);
    for object in [&tag, &tagged, &stash] {
        assert!(has(&checkout, object), "{object} was lost");
    }
    assert_eq!(
        read(&checkout, &["cat-file", "-p", &format!("{stash}:four.txt")]),
        "changed and never committed"
    );
    assert_eq!(
        read(
            &checkout,
            &["cat-file", "-p", &format!("{stash}^2:staged.txt")]
        ),
        "staged and never committed"
    );
}

#[test]
fn a_gone_registrations_staged_submodule_index_goes_to_its_module_store() {
    let h = home();
    let checkout = h.checkout();
    let witness = h.witness();
    let (wt, entry) = h.worktree("wts/five.noindex");
    git(&wt, &["submodule", "update", "-q", "--init"]);
    let store = entry.join("modules/vendor/astream");
    let sub = wt.join("vendor/astream");
    std::fs::write(sub.join("index.txt"), "only its index holds this\n").expect("written");
    git(&sub, &["add", "index.txt"]);
    let blob = read(&sub, &["rev-parse", ":index.txt"]);
    // One whose submodule has a rebase under way, its autostash in the private store.
    let (busy, busy_entry) = h.worktree("wts/busy.noindex");
    git(&busy, &["submodule", "update", "-q", "--init"]);
    let busy_store = busy_entry.join("modules/vendor/astream");
    let busy_sub = busy.join("vendor/astream");
    for n in ["p", "q"] {
        std::fs::write(busy_sub.join("lib.rs"), format!("// {n}\n")).expect("written");
        git(&busy_sub, &["commit", "-qam", n]);
    }
    let stash = rebase_stopped_with_autostash(&busy_sub, &busy_store, "lib.rs");
    for object in [&blob, &stash] {
        assert!(
            !stored(&checkout, &witness, object),
            "negative control: the witness lacks {object}"
        );
    }
    std::fs::remove_dir_all(&wt).expect("its folder removed by hand");
    std::fs::remove_dir_all(&busy).expect("its folder removed by hand");

    let pass = h.pass(None, &mut Seen::quiet());
    assert_eq!(pass.dropped.len(), 1, "{pass:#?}");
    assert_eq!(pass.dropped[0].0.entry, entry, "{pass:#?}");
    assert!(
        !entry.exists() && !store.exists(),
        "the entry and its store went"
    );
    assert!(
        pass.dropped[0].1.iter().all(|p| p.repo == witness),
        "{pass:#?}"
    );
    // The rebase under way keeps its entry, and its autostash, whole.
    assert_eq!(pass.kept.len(), 1, "{pass:#?}");
    assert!(pass.kept[0].1.contains("rebase-merge"), "{pass:#?}");
    assert!(busy_store.join("rebase-merge/autostash").is_file());
    assert!(stored(&checkout, &busy_store, &stash));
    git(
        &checkout,
        &[
            &format!("--git-dir={}", witness.display()),
            "gc",
            "-q",
            "--prune=now",
        ],
    );
    assert!(stored(&checkout, &witness, &blob), "{blob} was lost");
}

#[test]
fn a_module_pin_a_stopped_pass_left_is_owed_again() {
    let h = home();
    let witness = h.witness();
    let (wt, entry) = h.worktree("wts/six.noindex");
    git(&wt, &["submodule", "update", "-q", "--init"]);
    let sub = wt.join("vendor/astream");
    std::fs::write(sub.join("lib.rs"), "// only the private store holds this\n").expect("written");
    git(&sub, &["commit", "-qam", "private submodule work"]);
    let private = read(&sub, &["rev-parse", "HEAD"]);
    std::fs::remove_dir_all(&wt).expect("its folder removed by hand");

    // The first pass pins it, then cannot put its push on record.
    let mut seen = Seen::quiet();
    seen.refuse_drop = true;
    let pass = h.pass(None, &mut seen);
    assert!(pass.dropped.is_empty(), "{pass:#?}");
    assert!(entry.is_dir());
    let module_pin = |pins: &[Pin]| pins.iter().any(|p| p.repo == witness && p.oid == private);
    assert!(module_pin(&seen.asked[1].2), "{:#?}", seen.asked);
    assert!(stored(&h.checkout(), &witness, &private), "its pin stands");

    // The next owes it again: its own pin holds nothing for it.
    let mut seen = Seen::quiet();
    let pass = h.pass(None, &mut seen);
    assert_eq!(pass.dropped.len(), 1, "{pass:#?}");
    assert_eq!(seen.calls(), ["pinning", "dropping"]);
    assert!(module_pin(&seen.asked[1].2), "{:#?}", seen.asked);
    assert!(!entry.exists());
}

#[test]
fn a_shallow_private_store_keeps_its_registration() {
    let h = home();
    let checkout = h.checkout();
    let witness = h.witness();
    let upstream = h.home.join("astream");
    let (wt, entry) = h.worktree("wts/seven.noindex");
    git(&wt, &["submodule", "update", "-q", "--init"]);
    let store = entry.join("modules/vendor/astream");
    let sub = wt.join("vendor/astream");
    // Upstream moves on twice; the private store takes only its tip, shallow, and builds on it.
    for n in ["u2", "u3"] {
        std::fs::write(upstream.join("lib.rs"), format!("// {n}\n")).expect("written");
        git(&upstream, &["commit", "-qam", n]);
    }
    let url = format!("file://{}", upstream.display());
    git(&sub, &["fetch", "-q", "--depth=1", &url, "main"]);
    git(&sub, &["checkout", "-q", "FETCH_HEAD"]);
    std::fs::write(sub.join("lib.rs"), "// private, on a shallow root\n").expect("written");
    git(&sub, &["commit", "-qam", "private work on a shallow root"]);
    let private = read(&sub, &["rev-parse", "HEAD"]);
    assert!(
        store.join("shallow").is_file(),
        "the private store is shallow"
    );
    std::fs::remove_dir_all(&wt).expect("its folder removed by hand");

    let pass = h.pass(None, &mut Seen::quiet());
    assert!(pass.dropped.is_empty(), "{pass:#?}");
    assert_eq!(pass.kept.len(), 1, "{pass:#?}");
    assert!(pass.kept[0].1.contains("shallow"), "{pass:#?}");
    assert!(entry.is_dir());
    assert!(
        !stored(&checkout, &witness, &private),
        "nothing of it reached the module store"
    );

    // Negative control: made whole, it goes, its history whole in the module store.
    git(
        &checkout,
        &[
            &format!("--git-dir={}", store.display()),
            &format!("--work-tree={}", checkout.display()),
            "fetch",
            "-q",
            "--unshallow",
            &url,
            "main",
        ],
    );
    let pass = h.pass(None, &mut Seen::quiet());
    assert_eq!(pass.dropped.len(), 1, "{pass:#?}");
    let git_dir = format!("--git-dir={}", witness.display());
    let walked = read(
        &checkout,
        &[
            &git_dir,
            "rev-list",
            "--objects",
            "--missing=print",
            &private,
        ],
    );
    assert!(!walked.contains('?'), "{walked}");
}

#[test]
fn a_registration_whose_folder_was_alone_in_a_folder_left_empty_stays() {
    let h = home();
    let (wt, entry) = h.worktree("ext/wt.noindex");
    std::fs::remove_dir_all(&wt).expect("removed");
    // An empty folder where it was may be a volume's mount point while it is not mounted.
    let pass = h.pass(None, &mut Seen::quiet());
    assert!(pass.dropped.is_empty() && pass.kept.is_empty(), "{pass:#?}");
    std::fs::write(h.home.join("ext/.DS_Store"), "").expect("written");
    let pass = h.pass(None, &mut Seen::quiet());
    assert!(pass.dropped.is_empty() && pass.kept.is_empty(), "{pass:#?}");
    assert!(entry.is_dir());
    // Negative control: the folder holding something else, it is gone.
    std::fs::write(h.home.join("ext/other"), "").expect("written");
    let pass = h.pass(None, &mut Seen::quiet());
    assert_eq!(pass.dropped.len(), 1, "{pass:#?}");
    assert!(!entry.exists());
}

#[test]
fn a_registration_that_changes_while_it_is_pinned_or_names_another_repository_stays() {
    let h = home();
    let checkout = h.checkout();
    let mut entries = Vec::new();
    for name in ["locked", "back", "moved", "rewritten", "elsewhere"] {
        let (wt, entry) = h.worktree(&format!("wts/{name}.noindex"));
        std::fs::remove_dir_all(&wt).expect("removed");
        entries.push(entry);
    }
    std::fs::write(
        entries[4].join("commondir"),
        format!("{}\n", h.home.join("astream/.git").display()),
    )
    .expect("written");
    let repo = checkout.clone();
    let mut seen = Seen::new(move |what, gone, _| {
        if what != "dropping" {
            return;
        }
        let id = gone.entry.file_name().expect("an id").to_string_lossy();
        match id.trim_end_matches(".noindex") {
            "locked" => std::fs::write(gone.entry.join("locked"), "").expect("locked"),
            "back" => std::fs::create_dir_all(&gone.folder).expect("made again"),
            "moved" => {
                let late = read(
                    &repo,
                    &[
                        "-c",
                        "user.name=t",
                        "-c",
                        "user.email=t@example.invalid",
                        "commit-tree",
                        "HEAD^{tree}",
                        "-m",
                        "made while it was pinned",
                    ],
                );
                git(
                    &repo,
                    &[
                        &format!("--git-dir={}", gone.entry.display()),
                        "update-ref",
                        "refs/worktree/late",
                        &late,
                    ],
                );
            }
            "rewritten" => std::fs::write(
                gone.entry.join("gitdir"),
                format!(
                    "{}\n",
                    gone.folder.with_extension("moved").join(".git").display()
                ),
            )
            .expect("rewritten"),
            other => panic!("{other} is not one of its entries"),
        }
    });
    let pass = h.pass(None, &mut seen);
    assert!(pass.dropped.is_empty(), "{pass:#?}");
    // Each stays for its own check, every one judged before any assertion fails.
    let wrong: Vec<(&str, Option<&String>)> = [
        ("locked", "locked while"),
        ("back", "no longer gone"),
        ("moved", "changed while"),
        ("rewritten", "rewritten while"),
        ("elsewhere", "commondir"),
    ]
    .into_iter()
    .map(|(name, said)| {
        let why = pass
            .kept
            .iter()
            .find(|(r, _)| r.entry.ends_with(format!("{name}.noindex")))
            .map(|(_, why)| why);
        (name, said, why)
    })
    .filter(|(_, said, why)| !why.is_some_and(|why| why.contains(said)))
    .map(|(name, _, why)| (name, why))
    .collect();
    assert!(wrong.is_empty(), "{wrong:#?}");
    for entry in &entries {
        assert!(entry.is_dir(), "{} stays", entry.display());
    }
}
