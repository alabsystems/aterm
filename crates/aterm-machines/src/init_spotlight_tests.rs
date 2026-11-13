// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;
use crate::engine::{self, NoJournal};
use crate::items::defaults::tests::{ctx, user};
use crate::model::{Mode, Outcome};
use crate::port::{Fake, Ran};

const ENV: &str = "alice$ CARGO_HOME=/Users//alice/.cargo RUSTUP_HOME=/Users//alice/.rustup /Users//alice/.cargo/bin/rustup";
const STABLE: &str =
    "/Users//alice/.rustup/toolchains/stable-aarch64-apple-darwin/lib/rustlib/components";
const PINNED: &str =
    "/Users//alice/.rustup/toolchains/1.97.1-aarch64-apple-darwin/lib/rustlib/components";
const WITH_DOCS: &[u8] =
    b"cargo-aarch64-apple-darwin\nrust-docs-aarch64-apple-darwin\nrustc-aarch64-apple-darwin\n";
const WITHOUT: &[u8] = b"cargo-aarch64-apple-darwin\nrustc-aarch64-apple-darwin\n";
const STABLE_HTML: &str =
    "/Users//alice/.rustup/toolchains/stable-aarch64-apple-darwin/share/doc/rust/html";
const STABLE_ASIDE: &str =
    "/Users//alice/.rustup/toolchains/stable-aarch64-apple-darwin/share/doc/rust/html.noindex";

fn context() -> Ctx {
    ctx(&[user("alice", 501)])
}

#[test]
fn indexing_status_requires_a_successful_unambiguous_reading() {
    let status = "/usr/bin/mdutil -s /System/Volumes/Data";
    for (output, expected) in [
        ("/System/Volumes/Data:\n\tIndexing enabled.\n", Some(true)),
        ("/System/Volumes/Data:\n\tIndexing disabled.\n", Some(false)),
        ("Spotlight server is disabled.", None),
        ("Indexing enabled.\nIndexing disabled.", None),
        ("", None),
    ] {
        let sys = Fake::root();
        sys.on(status, Ran::ok(output));
        assert_eq!(indexing(&sys).ok(), expected, "{output}");
    }
    let sys = Fake::root();
    sys.on(
        status,
        Ran {
            status: Some(1),
            stdout: b"Indexing disabled.".to_vec(),
            stderr: Vec::new(),
        },
    );
    assert!(indexing(&sys).is_err());
}

#[test]
fn setup_disables_indexing_and_verifies_the_result() {
    let sys = Fake::root();
    sys.on(
        "/usr/bin/mdutil -s /System/Volumes/Data",
        Ran::ok("Indexing enabled."),
    )
    .on(
        "/usr/bin/mdutil -s /System/Volumes/Data",
        Ran::ok("Indexing disabled."),
    )
    .on(
        "/usr/bin/mdutil -i off /System/Volumes/Data",
        Ran::ok("Indexing disabled."),
    );
    let items: Vec<Box<dyn Item>> = vec![Box::new(Indexing)];
    let rows = engine::run(&items, Mode::Apply, &sys, &context(), &mut NoJournal);
    assert_eq!(rows[0].outcome, Outcome::Changed, "{rows:?}");
    let rows = engine::run(&items, Mode::Apply, &sys, &context(), &mut NoJournal);
    assert_eq!(rows[0].outcome, Outcome::Ok, "{rows:?}");

    let still_on = Fake::root();
    still_on
        .on(
            "/usr/bin/mdutil -s /System/Volumes/Data",
            Ran::ok("Indexing enabled."),
        )
        .on("/usr/bin/mdutil -i off /System/Volumes/Data", Ran::ok(""));
    let rows = engine::run(&items, Mode::Apply, &still_on, &context(), &mut NoJournal);
    assert_eq!(rows[0].outcome, Outcome::Failed, "{rows:?}");

    let mut linux = Fake::root();
    linux.linux = true;
    assert!(matches!(
        Indexing.probe(&linux, &context()),
        Probe::NotApplicable { .. }
    ));
    assert!(Indexing.apply(&linux, &context()).is_err());
    assert!(linux.calls().is_empty());
}

fn run(sys: &Fake, mode: Mode) -> Vec<crate::model::ItemReport> {
    let alice = user("alice", 501);
    let rustup = Rustup {
        user: alice,
        cargo_home: "/Users//alice/.cargo".into(),
        rustup_home: "/Users//alice/.rustup".into(),
    };
    let items: Vec<Box<dyn Item>> = vec![
        Box::new(Profile(rustup.clone())),
        Box::new(NoDocs(rustup, LastReading::default())),
    ];
    engine::run(&items, mode, sys, &context(), &mut NoJournal)
}

/// A Mac whose rustup reports `profile` first, with stable's `rust-docs` HTML in place when
/// its components record lists them.
fn mac(profile: &str, stable: &[u8]) -> Fake {
    let sys = Fake::root();
    if stable == WITH_DOCS {
        sys.create_dir_all(Path::new(&format!("{STABLE_HTML}/std")), 0o755)
            .unwrap();
        sys.file(&format!("{STABLE_HTML}/std/index.html"), b"<html>");
    }
    sys.file("/Users//alice/.cargo/bin/rustup", b"")
        .on(&format!("{ENV} show profile"), Ran::ok(profile))
        .on(
            &format!("{ENV} toolchain list"),
            Ran::ok("stable-aarch64-apple-darwin\n1.97.1-aarch64-apple-darwin (active, default)\ntrust\n"),
        )
        .file(STABLE, stable)
        .file(PINNED, WITHOUT)
        .link(
            "/Users//alice/.rustup/toolchains/trust",
            "/Users//alice/Library/Application Support/aterm/pkg/rustup/trust",
        );
    sys
}

#[test]
fn a_default_profile_and_stables_docs_are_planned_and_nothing_runs() {
    let sys = mac("default\n", WITH_DOCS);
    let rows = run(&sys, Mode::Plan);
    assert_eq!(rows[0].outcome, Outcome::Planned, "{rows:?}");
    assert_eq!(rows[1].outcome, Outcome::Planned, "{rows:?}");
    assert_eq!(
        rows[1].from.as_deref(),
        Some("rust-docs installed in stable-aarch64-apple-darwin")
    );
    assert_eq!(Reading::of(&rows[1]), Some(Reading::DocsInstalled));
    assert!(
        !sys.calls().iter().any(|c| c.contains(" set ")
            || c.contains(" remove ")
            || c.starts_with("rename ")
            || c.starts_with("symlink ")
            || c.starts_with("detach ")),
        "{:?}",
        sys.calls()
    );
}

/// Apply sets the profile, and keeps stable's docs out of Spotlight at once: the HTML is
/// renamed `html.noindex` with a link to it in its place, and rustup's removal of the
/// component, then of the HTML, starts in the background; no removal runs in the pass. Both
/// rows read back done at once.
#[test]
fn apply_sets_the_profile_and_the_docs_aside_at_once_and_removes_them_in_the_background() {
    let sys = mac("default\n", WITH_DOCS);
    let set = format!("{ENV} set profile minimal");
    sys.on(&format!("{ENV} show profile"), Ran::ok("minimal\n"))
        .on(&set, Ran::ok(""));
    let rows = run(&sys, Mode::Apply);
    assert_eq!(rows[0].outcome, Outcome::Changed, "{rows:?}");
    assert_eq!(rows[1].outcome, Outcome::Changed, "{rows:?}");
    let calls = sys.calls();
    assert_eq!(calls.iter().filter(|c| **c == set).count(), 1, "{calls:?}");
    let rename = format!("rename {STABLE_HTML} {STABLE_ASIDE}");
    let link = format!("symlink {STABLE_HTML} -> html.noindex");
    let removal = format!(
        "detach alice$ CARGO_HOME=/Users//alice/.cargo RUSTUP_HOME=/Users//alice/.rustup \
         /bin/sh -c {} sh /Users//alice/.cargo/bin/rustup stable-aarch64-apple-darwin \
         {STABLE_ASIDE} >> /dev/null",
        crate::port::shell_word(REMOVE_IN_BACKGROUND)
    );
    let at = |want: &str| calls.iter().position(|c| c == want);
    let (rename, link, removal) = (at(&rename), at(&link), at(&removal));
    assert!(
        rename.is_some() && rename < link && link < removal,
        "set aside, linked, then the removal in the background: {calls:?}"
    );
    assert!(
        !calls
            .iter()
            .any(|c| c.starts_with(ENV) && c.contains(" component remove ")),
        "no removal runs in the pass: {calls:?}"
    );
    assert!(
        !calls
            .iter()
            .any(|c| c.contains("--toolchain trust") || c.contains("1.97.1")),
        "{calls:?}"
    );
    assert_eq!(
        sys.read_file(Path::new(&format!("{STABLE_ASIDE}/std/index.html")))
            .unwrap(),
        b"<html>"
    );
    // Read again, the docs set aside are kept out and nothing runs again.
    let again = run(&sys, Mode::Apply);
    assert_eq!(again[1].outcome, Outcome::Ok, "{again:?}");
    assert_eq!(Reading::of(&again[1]), None);
    assert_eq!(
        sys.calls()
            .iter()
            .filter(|c| c.starts_with("detach "))
            .count(),
        1,
        "{:?}",
        sys.calls()
    );
}

/// An aside an earlier run left is not this run's to remove: the row fails, says where it
/// is, and moves nothing.
#[test]
fn an_aside_already_there_fails_the_row_and_moves_nothing() {
    let sys = mac("minimal\n", WITH_DOCS);
    sys.create_dir_all(Path::new(STABLE_ASIDE), 0o755).unwrap();
    let rows = run(&sys, Mode::Apply);
    assert_eq!(rows[1].outcome, Outcome::Failed, "{rows:?}");
    assert!(
        rows[1]
            .why
            .as_deref()
            .is_some_and(|why| why.contains(STABLE_ASIDE)),
        "{rows:?}"
    );
    assert!(
        !sys.calls().iter().any(|c| c.starts_with("rename ")
            || c.starts_with("symlink ")
            || c.starts_with("detach ")
            || c.contains(" component remove ")),
        "{:?}",
        sys.calls()
    );
}

#[test]
fn a_minimal_mac_is_ok_and_one_without_rustup_has_nothing_to_do() {
    let sys = mac("minimal\n", WITHOUT);
    let rows = run(&sys, Mode::Apply);
    assert!(rows.iter().all(|r| r.outcome == Outcome::Ok), "{rows:?}");
    let bare = Fake::root();
    let rows = run(&bare, Mode::Apply);
    assert!(
        rows.iter().all(|r| r.outcome == Outcome::NotApplicable),
        "{rows:?}"
    );
    assert!(bare.calls().is_empty(), "{:?}", bare.calls());
}

const MDFIND: &str =
    "alice$ /usr/bin/mdfind -onlyin /Users//alice -0 'kMDItemFSName == \"CACHEDIR.TAG\"'";
const TARGET: &str = "/Users//alice/work/demo/target";
/// The cache tag cargo writes at the top of a build folder.
const CARGO_TAG: &[u8] = b"Signature: 8a477f597d28d172789f06886806bc55\n# This file is a cache directory tag created by cargo.\n# For information about cache directory tags see https://bford.info/cachedir/\n";

/// A scratch atpkg prefix: the item reads its store and rustup views on disk, by the
/// layout contract (`aterm_toolchain::layout`), never through the fake.
struct Pkg(aterm_tempfile::TempDir);

impl Pkg {
    fn new() -> Self {
        Self(aterm_tempfile::tempdir().unwrap())
    }

    fn path(&self) -> &Path {
        self.0.path()
    }

    /// `rustup/<name>` or `store/trust/<n>`, with its Rust sysroot.
    fn plain(&self, at: &str) -> String {
        let dir = self.path().join(at);
        std::fs::create_dir_all(dir.join("lib/rustlib")).unwrap();
        dir.display().to_string()
    }

    /// `<at>.noindex` with `<at>` a link to it, as atpkg lays one aside.
    fn aside(&self, at: &str) {
        let dir = self.path().join(at);
        let name = dir.file_name().unwrap().to_str().unwrap().to_owned();
        std::fs::create_dir_all(
            dir.with_file_name(format!("{name}.noindex"))
                .join("lib/rustlib"),
        )
        .unwrap();
        std::os::unix::fs::symlink(format!("{name}.noindex"), &dir).unwrap();
    }

    /// `store/trust/current`, a link to build `n`.
    fn current(&self, n: &str) {
        std::os::unix::fs::symlink(n, self.path().join("store/trust/current")).unwrap();
    }

    fn items(&self) -> Vec<Box<dyn Item>> {
        vec![Box::new(BuildNoindex::new(
            user("alice", 501),
            "/Users//alice".into(),
            self.path().to_path_buf(),
            "/Users//alice/.cargo".into(),
            Projects::Noted,
        ))]
    }
}

/// A Mac with aterm's caches made and `found`, each a folder holding cargo's cache tag,
/// with what Spotlight's search for them answers.
fn with_build_folders(found: &[&str]) -> (Fake, Ran) {
    let sys = Fake::root();
    for name in [".ubd/cargo-build.noindex", "tooling.noindex"] {
        sys.create_dir_all(
            &Path::new("/Users//alice/Library/Caches/aterm").join(name),
            0o755,
        )
        .unwrap();
    }
    let mut reply = String::new();
    for folder in found {
        sys.create_dir_all(Path::new(&format!("{folder}/debug")), 0o755)
            .unwrap();
        sys.file(&format!("{folder}/CACHEDIR.TAG"), CARGO_TAG);
        reply.push_str(&format!("{folder}/CACHEDIR.TAG\0"));
    }
    sys.file("/usr/bin/mdfind", b"");
    (sys, Ran::ok(&reply))
}

/// [`with_build_folders`], whose search finds them.
fn spotlit(found: &[&str]) -> Fake {
    let (sys, found) = with_build_folders(found);
    sys.on(MDFIND, found);
    sys
}

/// A Mac whose caches the item makes are `made`, with one project `target` of the
/// person's that Spotlight indexes.
fn with_a_spotlit_project(made: bool) -> Fake {
    let sys = Fake::root();
    sys.create_dir_all(Path::new("/Users//alice/Library/Caches"), 0o755)
        .unwrap();
    if made {
        for name in [".ubd/cargo-build.noindex", "tooling.noindex"] {
            sys.create_dir_all(
                &Path::new("/Users//alice/Library/Caches/aterm").join(name),
                0o755,
            )
            .unwrap();
        }
    }
    sys.create_dir_all(Path::new(&format!("{TARGET}/debug")), 0o755)
        .unwrap();
    sys.file(&format!("{TARGET}/CACHEDIR.TAG"), CARGO_TAG);
    sys.file("/usr/bin/mdfind", b"")
        .on(MDFIND, Ran::ok(&format!("{TARGET}/CACHEDIR.TAG\0")));
    sys
}

/// The item on a Mac with no atpkg prefix.
fn alice_builds() -> Vec<Box<dyn Item>> {
    cargo_home_builds("/Users//alice/.cargo")
}

/// [`alice_builds`] with `CARGO_HOME` at `cargo_home`.
fn cargo_home_builds(cargo_home: &str) -> Vec<Box<dyn Item>> {
    searching(cargo_home, Projects::Noted)
}

/// The item on a Mac with no atpkg prefix, `CARGO_HOME` at `cargo_home`, searching for the
/// person's project build folders as `projects` says.
fn searching(cargo_home: &str, projects: Projects) -> Vec<Box<dyn Item>> {
    vec![Box::new(BuildNoindex::new(
        user("alice", 501),
        "/Users//alice".into(),
        "/Users//alice/Library/Application Support/aterm/pkg".into(),
        cargo_home.into(),
        projects,
    ))]
}

/// No rename and no link: what the item never does to a folder.
fn moved_nothing(sys: &Fake) {
    assert!(
        !sys.calls()
            .iter()
            .any(|c| c.starts_with("rename ") || c.contains("/bin/ln")),
        "{:#?}",
        sys.calls()
    );
}

/// The Rust in Spotlight row these rows read as.
fn row_of(rows: &[crate::model::ItemReport]) -> aterm_types::macos_setup::Observation {
    let refs: Vec<&crate::model::ItemReport> = rows.iter().collect();
    crate::init_observe::observation(aterm_types::macos_setup::Setting::Spotlight, &refs)
}

/// The item makes aterm's two caches and nothing else: the Trust toolchain build in use,
/// left plain in the store, is atpkg's move to make at its next update, so the plan
/// names it in its line and promises only the caches, and the build stays as it is.
#[test]
fn build_noindex_makes_its_caches_and_only_notes_the_toolchain_build_in_use() {
    let sys = Fake::root();
    sys.file("/usr/bin/mdfind", b"").on(MDFIND, Ran::ok(""));
    sys.create_dir_all(Path::new("/Users//alice/Library/Caches"), 0o755)
        .unwrap();
    let pkg = Pkg::new();
    let build = pkg.plain("store/trust/15698");
    pkg.current("15698");
    let items = pkg.items();
    let planned = engine::run(&items, Mode::Plan, &sys, &context(), &mut NoJournal);
    assert_eq!(planned[0].outcome, Outcome::Planned, "{planned:?}");
    assert_eq!(
        Reading::of(&planned[0]),
        Some(Reading::BuildsIndexed {
            projects: 0,
            toolchain: true,
        })
    );
    assert!(
        planned[0]
            .from
            .as_deref()
            .is_some_and(|f| f.contains("cargo-build.noindex absent") && f.contains(&build)),
        "{planned:?}"
    );
    assert_eq!(
        planned[0].to.as_deref(),
        Some("cargo-build.noindex in .ubd, and tooling.noindex"),
        "the plan promises only what apply makes"
    );
    let rows = engine::run(&items, Mode::Apply, &sys, &context(), &mut NoJournal);
    assert_eq!(rows[0].outcome, Outcome::Changed, "{rows:?}");
    assert_eq!(rows[0].stood_aside, None);
    assert!(
        std::fs::symlink_metadata(&build).is_ok_and(|m| m.is_dir()),
        "the build stays where atpkg left it"
    );
    moved_nothing(&sys);
    assert!(
        sys.stat(Path::new(
            "/Users//alice/Library/Caches/aterm/.ubd/cargo-build.noindex"
        ))
        .is_ok_and(|s| s.dir)
    );
}

/// A Mac with the Trust toolchain atpkg installed: the build in use, left plain in the
/// store, is aterm's own move still to come, a quiet note on an in-place row and never
/// Needs you, on every pass ("no user required"), and the stage finishes. A `<n>` that
/// is a link to `<n>.noindex`, as atpkg now installs one, is kept out already and noted
/// by no one.
#[test]
fn an_atpkg_toolchain_build_is_a_note_and_never_needs_you() {
    let sys = with_a_spotlit_project(true);
    let pkg = Pkg::new();
    let build = pkg.plain("store/trust/15698");
    pkg.current("15698");
    let items = pkg.items();
    for mode in [Mode::Plan, Mode::Apply, Mode::Apply, Mode::Plan] {
        let rows = engine::run(&items, mode, &sys, &context(), &mut NoJournal);
        assert_eq!(rows[0].outcome, Outcome::Ok, "{mode:?}: {rows:?}");
        assert_eq!(rows[0].stood_aside, None);
        assert_eq!(
            Reading::of(&rows[0]),
            Some(Reading::KeptOut {
                projects: 1,
                toolchain: true,
            })
        );
        let read = row_of(&rows);
        assert_eq!(
            read.standing,
            aterm_types::macos_setup::Standing::InPlace,
            "{mode:?}: {read:?}"
        );
        assert!(
            read.note.as_deref().is_some_and(|note| note.starts_with(
                "aterm moves the Rust toolchain it is using out of Spotlight at its next \
                 update; Spotlight indexes 1 of your project build folders"
            )),
            "{read:?}"
        );
        assert!(
            read.detail.as_deref().is_some_and(|d| d.contains(&build)),
            "the build stands behind the note: {read:?}"
        );
    }
    moved_nothing(&sys);

    // atpkg's layout: `<n>.noindex`, with `<n>` a link to it.
    let linked = spotlit(&[]);
    let pkg = Pkg::new();
    pkg.aside("store/trust/15698");
    pkg.current("15698");
    let rows = engine::run(
        &pkg.items(),
        Mode::Apply,
        &linked,
        &context(),
        &mut NoJournal,
    );
    assert_eq!(rows[0].outcome, Outcome::Ok, "{rows:?}");
    assert_eq!(Reading::of(&rows[0]), None, "nothing to note");
    assert_eq!(row_of(&rows).note, None);
}

/// A run nobody asked for ([`Projects::Unread`]) puts no question of macOS's on the screen:
/// it neither asks Spotlight for the person's build folders nor reads one, so a project
/// `target` in Documents, whose first read can raise macOS's question about that folder, is
/// never reached, and the row still reads aterm's caches as in place. CONTROL: the row that
/// notes them searches, reads that folder's cache tag and notes it.
#[test]
fn a_run_nobody_asked_for_reads_no_folder_of_the_persons() {
    let documents = "/Users//alice/Documents/demo/target";
    for (projects, reads) in [(Projects::Unread, false), (Projects::Noted, true)] {
        let sys = spotlit(&[documents]);
        let items = searching("/Users//alice/.cargo", projects);
        let rows = engine::run(&items, Mode::Apply, &sys, &context(), &mut NoJournal);
        assert_eq!(rows[0].outcome, Outcome::Ok, "{projects:?}: {rows:?}");
        let searched = sys.calls().iter().any(|call| call == MDFIND);
        assert_eq!(searched, reads, "{projects:?}: {:?}", sys.calls());
        let noted = Reading::of(&rows[0]).is_some_and(|reading| {
            reading
                == Reading::KeptOut {
                    projects: 1,
                    toolchain: false,
                }
        });
        assert_eq!(noted, reads, "{projects:?}: {rows:?}");
        assert_eq!(
            rows[0]
                .to
                .as_deref()
                .is_some_and(|to| to.contains(documents)),
            reads,
            "{projects:?}: {rows:?}"
        );
    }
}

/// The person's indexed project `target` is a quiet note with the folder named in its detail,
/// never a change, and is never renamed or linked whatever stands beside it.
#[test]
fn a_spotlit_project_folder_of_the_persons_is_a_note_and_never_touched() {
    let sys = with_a_spotlit_project(true);
    let items = alice_builds();
    for mode in [Mode::Plan, Mode::Apply] {
        let rows = engine::run(&items, mode, &sys, &context(), &mut NoJournal);
        assert_eq!(rows[0].outcome, Outcome::Ok, "{mode:?}: {rows:?}");
        assert_eq!(
            Reading::of(&rows[0]),
            Some(Reading::KeptOut {
                projects: 1,
                toolchain: false,
            })
        );
        let line = format!(
            "Spotlight indexes these project build folders of yours, which aterm leaves as they are: {TARGET}"
        );
        assert!(
            rows[0].to.as_deref().is_some_and(|to| to.ends_with(&line)),
            "{rows:?}"
        );
        let read = row_of(&rows);
        assert_eq!(
            read.standing,
            aterm_types::macos_setup::Standing::InPlace,
            "{read:?}"
        );
        assert_eq!(
            read.note.as_deref(),
            Some(crate::init_observe::projects_note(1).as_str())
        );
        assert!(
            read.detail.as_deref().is_some_and(|d| d.ends_with(&line)),
            "the folder stands behind the note: {read:?}"
        );
    }
    // A `.noindex` beside it changes nothing: it is still theirs, still indexed.
    let kept = format!("{TARGET}/retained");
    sys.file(&kept, b"existing build bytes");
    sys.create_dir_all(Path::new(&format!("{TARGET}.noindex")), 0o755)
        .unwrap();
    let rows = engine::run(&items, Mode::Apply, &sys, &context(), &mut NoJournal);
    assert_eq!(rows[0].outcome, Outcome::Ok, "{rows:?}");
    assert_eq!(
        sys.read_file(Path::new(&kept)).unwrap(),
        b"existing build bytes"
    );
    assert!(sys.stat(Path::new(TARGET)).unwrap().dir);
    moved_nothing(&sys);

    // Spotlight not answering costs the note, never aterm's own caches.
    let census = |reply| {
        let sys = Fake::root();
        sys.create_dir_all(Path::new("/Users//alice/Library/Caches"), 0o755)
            .unwrap();
        sys.file("/usr/bin/mdfind", b"").on(MDFIND, reply);
        sys
    };
    let incomplete = census(Ran::ok(""));
    incomplete.refuse_complete_stdout(MDFIND, "complete capture unavailable");
    let missing = Fake::root();
    missing
        .create_dir_all(Path::new("/Users//alice/Library/Caches"), 0o755)
        .unwrap();
    for unread in [
        census(Ran::exit(1, "census failed")),
        census(Ran::ok("/Users//alice/work/demo/target/CACHEDIR.TAG")),
        incomplete,
        missing,
    ] {
        let items = alice_builds();
        let rows = engine::run(&items, Mode::Apply, &unread, &context(), &mut NoJournal);
        assert_eq!(rows[0].outcome, Outcome::Changed, "{rows:?}");
        assert_eq!(
            Reading::of(&rows[0]),
            None,
            "made and read back in place, with no project counted"
        );
        assert!(
            unread
                .stat(Path::new(
                    "/Users//alice/Library/Caches/aterm/.ubd/cargo-build.noindex"
                ))
                .is_ok_and(|s| s.dir)
        );
    }
}

/// Spotlight is asked for build folders by `BUILD_MARKER`, and a search that fails (here, over
/// the complete capture) stands its age like one that found.
#[test]
fn spotlight_is_asked_for_build_folders_and_a_failed_search_stands_its_age() {
    let sys = with_a_spotlit_project(true);
    let items = alice_builds();
    let searches = |sys: &Fake| sys.calls().iter().filter(|c| c.contains("mdfind")).count();
    let rows = engine::run(&items, Mode::Plan, &sys, &context(), &mut NoJournal);
    assert_eq!(
        sys.calls().iter().filter(|c| c.as_str() == MDFIND).count(),
        1,
        "{:#?}",
        sys.calls()
    );
    assert_eq!(
        Reading::of(&rows[0]),
        Some(Reading::KeptOut {
            projects: 1,
            toolchain: false,
        })
    );

    let mut big = String::new();
    let mut n = 0;
    while big.len() <= aterm_uds::deadline::OUTPUT_KEEP {
        big.push_str(&format!("/Users//alice/work/p{n:06}/target/CACHEDIR.TAG\0"));
        n += 1;
    }
    let mut over = Fake::root();
    for name in [".ubd/cargo-build.noindex", "tooling.noindex"] {
        over.create_dir_all(
            &Path::new("/Users//alice/Library/Caches/aterm").join(name),
            0o755,
        )
        .unwrap();
    }
    over.file("/usr/bin/mdfind", b"").on(MDFIND, Ran::ok(&big));
    let items = alice_builds();
    for _ in 0..3 {
        let rows = engine::run(&items, Mode::Plan, &over, &context(), &mut NoJournal);
        assert_eq!(rows[0].outcome, Outcome::Ok, "{rows:?}");
        assert_eq!(Reading::of(&rows[0]), None, "the note goes, nothing else");
    }
    assert_eq!(searches(&over), 1, "a failed search stands its age too");
    over.now += INVENTORY_AGE;
    engine::run(&items, Mode::Plan, &over, &context(), &mut NoJournal);
    assert_eq!(searches(&over), 2);
}

/// One search of the home folder stands for every probe for [`INVENTORY_AGE`]: a run's
/// plan, apply and read-back, and the page's reading, ask Spotlight once.
#[test]
fn spotlight_is_searched_once_per_inventory_age() {
    let mut sys = with_a_spotlit_project(true);
    let items = alice_builds();
    let searches = |sys: &Fake| sys.calls().iter().filter(|c| c.contains("mdfind")).count();
    engine::run(&items, Mode::Apply, &sys, &context(), &mut NoJournal);
    engine::run(&items, Mode::Plan, &sys, &context(), &mut NoJournal);
    assert_eq!(searches(&sys), 1, "{:#?}", sys.calls());
    sys.now += INVENTORY_AGE - 1;
    engine::run(&items, Mode::Plan, &sys, &context(), &mut NoJournal);
    assert_eq!(searches(&sys), 1);
    sys.now += 1;
    let rows = engine::run(&items, Mode::Plan, &sys, &context(), &mut NoJournal);
    assert_eq!(
        searches(&sys),
        2,
        "an inventory past its age is taken again"
    );
    assert_eq!(
        Reading::of(&rows[0]),
        Some(Reading::KeptOut {
            projects: 1,
            toolchain: false,
        })
    );
    // A clock that went back reads again.
    sys.now -= 10;
    engine::run(&items, Mode::Plan, &sys, &context(), &mut NoJournal);
    assert_eq!(searches(&sys), 3);
}

/// Spotlight's build folders are the folders cargo tagged, whatever their names (a
/// `CARGO_TARGET_DIR` or `--target-dir` names its own), each counted once: a build
/// folder inside one found (a build script's own under its `OUT_DIR`) is part of it.
/// NEGATIVE CONTROL: a folder another tool tagged is no build folder of cargo's, and
/// a cargo build folder inside it still counts.
#[test]
fn a_cargo_build_folder_counts_once_whatever_its_name() {
    let read = |sys: &Fake| {
        let rows = engine::run(&alice_builds(), Mode::Plan, sys, &context(), &mut NoJournal);
        Reading::of(&rows[0])
    };
    let projects = |projects| {
        Some(Reading::KeptOut {
            projects,
            toolchain: false,
        })
    };
    assert_eq!(read(&spotlit(&["/Users//alice/brtgt"])), projects(1));
    let nested = format!("{TARGET}/debug/build/pmt-1/out/target");
    assert_eq!(read(&spotlit(&[TARGET, &nested])), projects(1));
    let beside = "/Users//alice/work/demo/target-main";
    assert_eq!(read(&spotlit(&[TARGET, &nested, beside])), projects(2));
    let other_tool = "/Users//alice/.cache/pip";
    let pip = |sys: &Fake| {
        sys.file(
            &format!("{other_tool}/CACHEDIR.TAG"),
            b"Signature: 8a477f597d28d172789f06886806bc55\n# This file is a cache directory tag created by pip.\n",
        );
    };
    let sys = spotlit(&[other_tool]);
    pip(&sys);
    assert_eq!(read(&sys), None, "nothing of cargo's to note");
    let inside = format!("{other_tool}/target");
    let sys = spotlit(&[other_tool, &inside]);
    pip(&sys);
    assert_eq!(read(&sys), projects(1));
}

/// Cargo tags its download caches in its home (`registry`, `git`) exactly as it tags a
/// build folder. NEGATIVE CONTROL: with a visible `CARGO_HOME`, which Spotlight indexes,
/// they are cargo's own and never counted as the person's project build folders; a
/// project's beside them still is. A cargo home that holds the home folder leaves out
/// only those caches.
#[test]
fn cargos_download_caches_are_no_project_build_folders() {
    let read = |items: Vec<Box<dyn Item>>, sys: &Fake| {
        let rows = engine::run(&items, Mode::Plan, sys, &context(), &mut NoJournal);
        Reading::of(&rows[0])
    };
    let caches = ["/Users//alice/cargo/registry", "/Users//alice/cargo/git"];
    let sys = spotlit(&caches);
    assert_eq!(
        read(cargo_home_builds("/Users//alice/cargo"), &sys),
        None,
        "nothing of the person's to note"
    );
    let sys = spotlit(&[caches[0], caches[1], TARGET]);
    assert_eq!(
        read(cargo_home_builds("/Users//alice/cargo"), &sys),
        Some(Reading::KeptOut {
            projects: 1,
            toolchain: false,
        })
    );
    let caches = ["/Users//alice/registry", "/Users//alice/git"];
    let sys = spotlit(&[caches[0], caches[1], TARGET]);
    assert_eq!(
        read(cargo_home_builds("/Users//alice"), &sys),
        Some(Reading::KeptOut {
            projects: 1,
            toolchain: false,
        })
    );
}

/// System State reads inside the app, whose environment a launch from the Dock gives
/// it names no `CARGO_HOME`: a visible cargo home a shell's startup file names
/// (`~/cargo`) is still cargo's, by the files cargo keeps at its top, whichever it
/// wrote (`.package-cache`, `.global-cache`), and its download caches are no project
/// build folders. So are those of `~/.cargo` linked to a visible folder, which
/// Spotlight reports by the folder's own path. NEGATIVE CONTROLS: a `registry` and a
/// `git` cargo tagged beside neither (a target folder of that name) are build folders
/// like any other, and a project's beside the caches always counts.
#[test]
fn a_cargo_home_the_environment_does_not_name_is_still_cargos() {
    let read = |sys: &Fake| {
        let rows = engine::run(&alice_builds(), Mode::Plan, sys, &context(), &mut NoJournal);
        Reading::of(&rows[0])
    };
    let projects = |projects| {
        Some(Reading::KeptOut {
            projects,
            toolchain: false,
        })
    };
    let visible = ["/Users//alice/cargo/registry", "/Users//alice/cargo/git"];
    for mark in [".package-cache", ".global-cache"] {
        let sys = spotlit(&[visible[0], visible[1], TARGET]);
        sys.file(&format!("/Users//alice/cargo/{mark}"), b"");
        assert_eq!(read(&sys), projects(1), "{mark}");
    }
    let sys = spotlit(&[visible[0], visible[1], TARGET]);
    assert_eq!(read(&sys), projects(3), "nothing of cargo's beside them");
    let real = [
        "/Users//alice/dev/cargo/registry",
        "/Users//alice/dev/cargo/git",
    ];
    let sys = spotlit(&[real[0], real[1], TARGET]);
    sys.link("/Users//alice/.cargo", "/Users//alice/dev/cargo");
    assert_eq!(read(&sys), projects(1), "~/.cargo is a link to them");
    let sys = spotlit(&[real[0], real[1], TARGET]);
    assert_eq!(
        read(&sys),
        projects(3),
        "no link, nothing of cargo's beside them"
    );
}

/// Check again ([`search_again`]) never waits on a search under way, which holds the
/// inventory's lock for as long as Spotlight takes (a minute, or more): the window that
/// presses it stays live. The search under way began before it, so the next reading
/// searches anew. The wait here is a hang detector: the stand-in search is held on a
/// channel until the forget has returned.
#[test]
fn check_again_never_waits_on_a_search_under_way() {
    use std::sync::mpsc;
    const HANG: std::time::Duration = std::time::Duration::from_secs(60);
    let home = Path::new("/Users//alice");
    let inventory = Arc::new(Inventory::default());
    let (started, searching) = mpsc::channel();
    let (release, held) = mpsc::channel::<()>();
    let search = {
        let inventory = Arc::clone(&inventory);
        std::thread::spawn(move || {
            inventory.read(home, 0, || {
                started.send(()).unwrap();
                held.recv().unwrap();
                Ok(BTreeSet::new())
            })
        })
    };
    searching.recv_timeout(HANG).expect("the search began");
    let (forgot, forgetting) = mpsc::channel();
    let forget = {
        let inventory = Arc::clone(&inventory);
        std::thread::spawn(move || {
            inventory.forget();
            forgot.send(()).unwrap();
        })
    };
    let returned = forgetting.recv_timeout(HANG);
    release.send(()).unwrap();
    search.join().unwrap().unwrap();
    forget.join().unwrap();
    returned.expect("Check again waited on the search under way");
    let searched = std::cell::Cell::new(false);
    inventory
        .read(home, 0, || {
            searched.set(true);
            Ok(BTreeSet::new())
        })
        .unwrap();
    assert!(searched.get(), "a search begun before Check again is stale");
}

/// The build `store/trust/current` names is the one in use, which atpkg never moves
/// until an update makes another current, and the rustup view laid from it moves with
/// the next update too: one note says so, never "older". A plain build no longer in
/// use, atpkg's to move at its next pass, goes unsaid. With nothing plain left, the row
/// says the toolchain is kept out, with no note.
#[test]
fn only_the_toolchain_in_use_is_noted() {
    let sys = spotlit(&[]);
    let read = |pkg: &Pkg| {
        let rows = engine::run(&pkg.items(), Mode::Plan, &sys, &context(), &mut NoJournal);
        assert_eq!(rows[0].outcome, Outcome::Ok, "{rows:?}");
        (row_of(&rows), rows[0].to.clone().unwrap_or_default())
    };
    let pkg = Pkg::new();
    let older = pkg.plain("store/trust/15600");
    let build = pkg.plain("store/trust/15698");
    pkg.current("15698");
    let view = pkg.plain("rustup/trust");
    let (row, line) = read(&pkg);
    assert_eq!(row.note.as_deref(), Some(TOOLCHAIN_NOTE), "{row:?}");
    assert!(
        line.contains(&build) && line.contains(&view) && !line.contains(&older),
        "{line}"
    );
    assert!(line.contains(&format!("{TOOLCHAIN_NOTE}: ")), "{line}");

    // The build in use laid aside, its view still plain: the view is the note.
    let pkg = Pkg::new();
    pkg.plain("store/trust/15600");
    pkg.aside("store/trust/15698");
    pkg.current("15698");
    let view = pkg.plain("rustup/trust");
    let (row, line) = read(&pkg);
    assert_eq!(row.note.as_deref(), Some(TOOLCHAIN_NOTE), "{row:?}");
    assert!(line.contains(&view), "{line}");

    // Both laid aside: the older plain build is no note, and no "kept out" is claimed.
    let pkg = Pkg::new();
    pkg.plain("store/trust/15600");
    pkg.aside("store/trust/15698");
    pkg.current("15698");
    pkg.aside("rustup/trust");
    let (row, line) = read(&pkg);
    assert_eq!(row.note, None, "{row:?}");
    assert_eq!(
        line,
        format!("cargo-build.noindex and tooling.noindex are .noindex; {QUIET_LINE}")
    );

    // Nothing plain: kept out, no note.
    std::fs::remove_dir_all(pkg.path().join("store/trust/15600")).unwrap();
    let (row, line) = read(&pkg);
    assert_eq!(row.note, None, "{row:?}");
    assert_eq!(
        line,
        format!(
            "cargo-build.noindex, tooling.noindex, and the Trust toolchain are .noindex; \
             {QUIET_LINE}"
        )
    );
}

/// Check again searches Spotlight anew ([`search_again`]): a search kept from minutes
/// ago, found or failed, would leave the row's note as it was.
#[test]
fn check_again_searches_spotlight_anew() {
    let (sys, found) = with_build_folders(&[TARGET]);
    sys.on(MDFIND, Ran::exit(1, "Spotlight is rebuilding its index"))
        .on(MDFIND, found);
    let item = BuildNoindex {
        inventory: SHARED.get_or_init(Arc::default).clone(),
        ..BuildNoindex::new(
            user("alice", 501),
            "/Users//alice".into(),
            "/Users//alice/Library/Application Support/aterm/pkg".into(),
            "/Users//alice/.cargo".into(),
            Projects::Noted,
        )
    };
    let items: Vec<Box<dyn Item>> = vec![Box::new(item)];
    let read = || {
        let rows = engine::run(&items, Mode::Plan, &sys, &context(), &mut NoJournal);
        Reading::of(&rows[0])
    };
    let searches = || sys.calls().iter().filter(|c| c.as_str() == MDFIND).count();
    assert_eq!(read(), None, "the search failed");
    assert_eq!((read(), searches()), (None, 1), "and stands its age");
    search_again();
    assert_eq!(
        (read(), searches()),
        (
            Some(Reading::KeptOut {
                projects: 1,
                toolchain: false,
            }),
            2
        )
    );
}
