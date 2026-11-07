// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Clone tuning against real git: a scratch home holding the vault's inventory and clones whose
//! one origin names the inventory's GitHub repository, cloned from local sources.

use super::*;
use crate::items::defaults::tests::{ctx, user};
use crate::port::Live;
use aterm_converge::Outcome;
use std::cell::Cell;
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};

/// trust's declaration as committed.
const TRUST: &str = "[tuning]\n\tsuperproject = core.untrackedCache\n\tsuperproject = feature.manyFiles\n\tsubmodule = core.untrackedCache\n\tindexVersion = 4\n\tnever = core.fsmonitor\n";

struct World {
    sys: Live,
    home: aterm_tempfile::TempDir,
    state: aterm_tempfile::TempDir,
    sources: aterm_tempfile::TempDir,
    who: User,
}

impl World {
    fn new() -> Self {
        Self::with_program(None)
    }

    fn with_named_hooks() -> Self {
        Self::with_program(Some(feature_git()))
    }

    fn with_program(git: Option<PathBuf>) -> Self {
        if git.is_none() {
            crate::items::github::need_clt_git();
        }
        let sys = Live::new();
        let home = aterm_tempfile::tempdir().unwrap();
        let state = aterm_tempfile::tempdir().unwrap();
        let sources = aterm_tempfile::tempdir().unwrap();
        let mut who = user("fixture", sys.euid());
        who.home = home.path().to_string_lossy().into_owned();
        let world = Self {
            sys,
            home,
            state,
            sources,
            who,
        };
        if let Some(git) = git {
            // Select the admitted managed tool for both the subject and controls,
            // while all Git configuration and repositories stay in the fixture home.
            let prefix = git
                .parent()
                .and_then(Path::parent)
                .expect("package bin/git");
            let config = world.home.path().join(".config/aterm/aterm.toml");
            std::fs::create_dir_all(config.parent().unwrap()).unwrap();
            let prefix = aterm_toml::Value::String(prefix.to_string_lossy().into_owned());
            std::fs::write(config, format!("[packages]\nprefix = {prefix}\n")).unwrap();
            assert_eq!(checkout::git(&world.sys, &world.who, None).program, git);
        }
        world.global("");
        world
    }

    /// The fixture's `~/.gitconfig`: what every fixture git needs, then `extra`.
    fn global(&self, extra: &str) {
        std::fs::write(
            self.home.path().join(".gitconfig"),
            format!(
                "[user]\n\tname = Fixture\n\temail = fixture@example.invalid\n[init]\n\tdefaultBranch = main\n[commit]\n\tgpgsign = false\n[protocol \"file\"]\n\tallow = always\n{extra}"
            ),
        )
        .unwrap();
    }

    fn ctx(&self) -> Ctx {
        ctx(std::slice::from_ref(&self.who))
    }

    fn state(&self) -> &Path {
        self.state.path()
    }

    /// A fixture git as the fixture user, in `at`.
    fn git(&self, at: &Path, args: &[&str]) -> Ran {
        self.sys
            .run_complete_stdout(
                &checkout::git(&self.sys, &self.who, Some(at))
                    .args(NO_SIDE_RUNS)
                    .args(args.iter().copied()),
            )
            .unwrap()
    }

    /// A plain git, nothing turned off, in `at`: the control that a stand-in hook or filter is
    /// live.
    fn plain(&self, at: &Path, args: &[&str]) -> String {
        let ran = self
            .sys
            .run_complete_stdout(
                &checkout::git(&self.sys, &self.who, Some(at)).args(args.iter().copied()),
            )
            .unwrap();
        assert!(ran.success(), "git {args:?}: {}", ran.describe());
        stdout(&ran)
    }

    fn ok(&self, at: &Path, args: &[&str]) -> String {
        let ran = self.git(at, args);
        assert!(
            ran.success(),
            "git {args:?} in {}: {}",
            at.display(),
            ran.describe()
        );
        stdout(&ran)
    }

    /// A repository under the sources folder holding `files`, committed.
    fn source(&self, name: &str, files: &[(&str, &str)]) -> PathBuf {
        let at = self.sources.path().join(name);
        std::fs::create_dir_all(&at).unwrap();
        self.ok(&at, &["init", "--quiet", "--template="]);
        for (path, text) in files {
            let file = at.join(path);
            std::fs::create_dir_all(file.parent().unwrap()).unwrap();
            std::fs::write(file, text).unwrap();
        }
        self.ok(&at, &["add", "-A"]);
        self.ok(&at, &["commit", "--quiet", "-m", name]);
        at
    }

    /// A source carrying `declaration` (none for `None`) and a submodule `first-party/sub` that
    /// carries one of its own, `nested`.
    fn with_submodules(&self, name: &str, declaration: Option<&str>) -> PathBuf {
        let nested = self.source(&format!("{name}-nested"), &[("n.txt", "nested\n")]);
        let sub = self.source(&format!("{name}-sub"), &[("s.txt", "sub\n")]);
        self.ok(
            &sub,
            &[
                "submodule",
                "add",
                "--quiet",
                nested.to_str().unwrap(),
                "nested",
            ],
        );
        self.ok(&sub, &["commit", "--quiet", "-m", "nested"]);
        let mut files = vec![("README.md", "fixture\n")];
        if let Some(text) = declaration {
            files.push((DECLARATION, text));
        }
        let top = self.source(name, &files);
        self.ok(
            &top,
            &[
                "submodule",
                "add",
                "--quiet",
                sub.to_str().unwrap(),
                "first-party/sub",
            ],
        );
        self.ok(&top, &["commit", "--quiet", "-m", "sub"]);
        top
    }

    /// `~/<name>`, cloned from `source` with its submodules, its one origin then naming the
    /// inventory's GitHub repository.
    fn clone(&self, name: &str, source: &Path) -> PathBuf {
        let at = self.home.path().join(name);
        self.ok(
            self.home.path(),
            &[
                "clone",
                "--quiet",
                "--recurse-submodules",
                source.to_str().unwrap(),
                at.to_str().unwrap(),
            ],
        );
        self.ok(
            &at,
            &[
                "remote",
                "set-url",
                "origin",
                &format!("https://github.com/fixture/{name}.git"),
            ],
        );
        at
    }

    /// The vault's inventory, naming `names` as the fixture account's repositories.
    fn inventory(&self, names: &[&str]) {
        let repositories: Vec<String> = names
            .iter()
            .zip(1..)
            .map(|(name, id)| {
                format!(
                    r#"{{"id":{id},"name":"{name}","owner":{{"login":"fixture"}},"full_name":"fixture/{name}","private":true,"clone_url":"https://github.com/fixture/{name}.git","default_branch":"main"}}"#
                )
            })
            .collect();
        let dir = self.home.path().join(".secrets");
        std::fs::create_dir_all(&dir).unwrap();
        set_mode(&dir, 0o700);
        let file = dir.join("github-repositories.json");
        std::fs::write(
            &file,
            format!(
                r#"{{"schema":2,"account":"fixture","repositories":[{}]}}"#,
                repositories.join(",")
            ),
        )
        .unwrap();
        set_mode(&file, 0o600);
    }

    /// `~/.config/aterm/init.toml`.
    fn init_toml(&self, text: &str) {
        let dir = self.home.path().join(".config/aterm");
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("init.toml"), text).unwrap();
    }

    /// The record's self-test result for the state folder's volume, as a kept one.
    fn kept_self_test(&self, passed: bool) {
        let dir = converge_dir(self.state());
        aterm_uds::private_file::ensure_private_dir(&dir).unwrap();
        let mut record = Record::read(&dir).unwrap().unwrap();
        let dev = test_volume(&self.sys, &dir).unwrap();
        record
            .volumes
            .insert(dev.to_string(), Tested { passed, at_s: 1 });
        record.write(&dir).unwrap();
    }

    fn read(&self) -> Reading {
        read(&self.sys, &self.ctx(), self.state(), &Holds::NONE).unwrap()
    }

    fn read_holding(&self, holds: &Holds) -> Reading {
        read(&self.sys, &self.ctx(), self.state(), holds).unwrap()
    }

    /// [`apply`] with a self-test that must not run: the volume's result is kept.
    fn apply(&self) -> Result<Tuned, String> {
        apply_with(
            &self.sys,
            &self.ctx(),
            self.state(),
            &Holds::NONE,
            &Seams {
                self_test: &|_, _, _| panic!("the volume's self-test result is kept"),
                planned: &|| {},
                lock_wait: LOCK_WAIT,
            },
        )
    }

    /// What [`World::apply`] said it tuned, where it stood nowhere aside.
    fn tuned(&self) -> String {
        let tuned = self.apply().unwrap();
        assert_eq!(tuned.aside, None, "{tuned:?}");
        tuned.said
    }

    /// `key` as `path`'s own config holds it (`--local`), every value.
    fn local(&self, path: &Path, key: &str) -> String {
        stdout(&self.git(path, &["config", "--local", "--get-all", key]))
    }

    /// The record's entry for the repository at `path`.
    fn owned(&self, path: &Path) -> Option<Owned> {
        Record::read(&converge_dir(self.state()))
            .unwrap()
            .unwrap()
            .repositories
            .into_values()
            .find(|owned| Path::new(&owned.path) == path)
    }

    /// The declaration on the source's default branch becomes `text`, and the clone at `top`
    /// fetches it.
    fn declare(&self, source: &Path, top: &Path, text: &str) {
        std::fs::write(source.join(DECLARATION), text).unwrap();
        self.ok(source, &["commit", "--quiet", "-am", "declare"]);
        self.ok(
            top,
            &[
                "fetch",
                "--quiet",
                source.to_str().unwrap(),
                "main:refs/remotes/origin/main",
            ],
        );
    }
}

/// System State's hold of GitHub repositories.
fn repositories_held() -> Holds {
    let mut off = Selection::none();
    off.set(Setting::Repositories, true);
    Holds { off, ..Holds::NONE }
}

/// A stand-in program at `path` that appends `tag` to `marker` and passes its input through.
fn stand_in(path: &Path, marker: &Path, tag: &str) {
    std::fs::write(
        path,
        format!("#!/bin/sh\necho {tag} >> '{}'\ncat\n", marker.display()),
    )
    .unwrap();
    set_mode(path, 0o755);
}

fn set_mode(path: &Path, mode: u32) {
    use std::os::unix::fs::PermissionsExt as _;
    std::fs::set_permissions(path, std::fs::Permissions::from_mode(mode)).unwrap();
}

/// The format in `repo`'s index header.
fn index_format(world: &World, repo: &Path) -> u32 {
    let path = world.ok(
        repo,
        &["rev-parse", "--path-format=absolute", "--git-path", "index"],
    );
    let head = std::fs::read(path).unwrap();
    assert_eq!(&head[..4], b"DIRC");
    u32::from_be_bytes([head[4], head[5], head[6], head[7]])
}

fn due(reading: &Reading) -> &str {
    match reading {
        Reading::Due { from, .. } => from,
        other => panic!("expected the clones to differ, read {other:?}"),
    }
}

fn current(reading: &Reading) -> &str {
    match reading {
        Reading::Current(now) => now,
        other => panic!("expected every clone as it declares, read {other:?}"),
    }
}

/// trust's shape: a clone found in place, its two submodules populated, untuned. The pass runs
/// git's own self-test once, sets the declared keys, moves the superproject's index to format 4
/// and records what it set; a second pass changes nothing and runs no self-test.
#[test]
fn a_clone_found_in_place_converges_to_its_declaration_and_a_second_pass_changes_nothing() {
    let world = World::new();
    let top = world.clone("trust", &world.with_submodules("trust", Some(TRUST)));
    world.inventory(&["trust"]);
    let sub = top.join("first-party/sub");
    let nested = sub.join("nested");
    let start = index_format(&world, &top);
    assert_ne!(
        start, INDEX_VERSION,
        "a fresh clone's index is not yet format 4"
    );
    let sub_format = index_format(&world, &sub);

    let before = world.read();
    assert_eq!(
        due(&before),
        format!(
            "$HOME/trust: core.untrackedCache and feature.manyFiles unset, index format {start}, \
             core.untrackedCache unset in 2 submodules (once git's untracked-cache self-test \
             passes on its volume)"
        )
    );
    let tests = Cell::new(0);
    let said = apply_with(
        &world.sys,
        &world.ctx(),
        world.state(),
        &Holds::NONE,
        &Seams {
            self_test: &|sys, user, dir| {
                tests.set(tests.get() + 1);
                self_test(sys, user, dir)
            },
            planned: &|| {},
            lock_wait: LOCK_WAIT,
        },
    )
    .unwrap();
    assert_eq!(
        said,
        Tuned {
            said: format!("tuned $HOME/trust (index format {start} -> 4)"),
            aside: None
        }
    );
    assert_eq!(
        tests.get(),
        1,
        "git's own self-test ran once, for the one volume"
    );
    assert_eq!(world.local(&top, "core.untrackedCache"), "true");
    assert_eq!(world.local(&top, "feature.manyFiles"), "true");
    assert_eq!(index_format(&world, &top), INDEX_VERSION);
    for repo in [&sub, &nested] {
        assert_eq!(world.local(repo, "core.untrackedCache"), "true");
        assert_eq!(
            world.local(repo, "feature.manyFiles"),
            "",
            "trust-cg's host boundary refuses it in a submodule"
        );
    }
    assert_eq!(
        index_format(&world, &sub),
        sub_format,
        "a submodule's index is left"
    );
    assert_eq!(world.local(&top, "core.fsmonitor"), "", "never set");
    assert_eq!(
        world.ok(&top, &["status", "--porcelain"]),
        "",
        "the checkout stays clean"
    );
    // Nothing of aterm's is in the clone: the record is aterm's own.
    let dir = converge_dir(world.state());
    let record = Record::read(&dir).unwrap().unwrap();
    assert_eq!(record.volumes.len(), 1);
    assert!(record.volumes.values().all(|tested| tested.passed));
    assert_eq!(
        world.owned(&top).unwrap().set,
        ["core.untrackedCache", "feature.manyFiles"]
    );

    let after = world.read();
    assert_eq!(
        current(&after),
        "1 vault clone on this Mac, each as it declares"
    );
    assert_eq!(world.tuned(), "nothing tuned");
    assert_eq!(
        world.local(&top, "core.untrackedCache"),
        "true",
        "one value, unchanged"
    );
}

/// What [`nothing_the_clone_names_runs_while_aterm_writes_it`] stands in for: a hooks folder's
/// hook, a config's, an fsmonitor hook, a clean filter named in the worktree's `.gitattributes`,
/// in `.git/info/attributes` and in `core.attributesFile`, a long-running (`process`) one, and a
/// hook and a driver each named with the empty name.
const NAMED: [&str; 9] = [
    "folder-hook",
    "config-hook",
    "fsmonitor",
    "filter",
    "info-filter",
    "global-filter",
    "process-filter",
    "empty-hook",
    "empty-filter",
];

/// Nothing the clone names runs while aterm writes it: not a hooks folder's
/// `post-index-change`, not one its config names (`hook.<name>.command`), not its
/// `core.fsmonitor` hook, and not a filter driver any attributes file names (the worktree's,
/// `.git/info/attributes`, `core.attributesFile`, the last with its driver in the global config),
/// a `clean` one or a `process` one, for a racily clean entry the index rewrite looks at again.
#[test]
fn nothing_the_clone_names_runs_while_aterm_writes_it() {
    let world = World::with_named_hooks();
    let top = world.clone("trust", &world.source("trust", &[(DECLARATION, TRUST)]));
    world.inventory(&["trust"]);
    world.kept_self_test(true);
    let marker = world.sources.path().join("ran");
    let programs = world.sources.path().join("programs");
    std::fs::create_dir_all(&programs).unwrap();
    for tag in NAMED {
        stand_in(&programs.join(tag), &marker, tag);
    }
    let hooks = top.join(".git/hooks");
    std::fs::create_dir_all(&hooks).unwrap();
    std::fs::copy(
        programs.join("folder-hook"),
        hooks.join("post-index-change"),
    )
    .unwrap();
    set_mode(&hooks.join("post-index-change"), 0o755);
    let program = |tag: &str| programs.join(tag).to_string_lossy().into_owned();
    world.ok(
        &top,
        &["config", "hook.odd.name.event", "post-index-change"],
    );
    world.ok(
        &top,
        &["config", "hook.odd.name.command", &program("config-hook")],
    );
    world.ok(&top, &["config", "core.fsmonitor", &program("fsmonitor")]);
    world.ok(&top, &["config", "filter.probe.clean", &program("filter")]);
    world.ok(
        &top,
        &["config", "filter.info.clean", &program("info-filter")],
    );
    // A required driver turned off must not fail the write.
    world.ok(&top, &["config", "filter.info.required", "true"]);
    // The stand-in never answers the long-running protocol: git notes that and goes on, once the
    // program has run.
    world.ok(
        &top,
        &["config", "filter.proc.process", &program("process-filter")],
    );
    std::fs::write(
        top.join(".gitattributes"),
        "racy.txt filter=probe\nproc.txt filter=proc\n",
    )
    .unwrap();
    {
        use std::io::Write as _;
        let mut config = std::fs::File::options()
            .append(true)
            .open(top.join(".git/config"))
            .unwrap();
        write!(
            config,
            "[hook \"\"]\n\tevent = post-index-change\n\tcommand = {}\n[filter \"\"]\n\tclean = {}\n",
            program("empty-hook"),
            program("empty-filter")
        )
        .unwrap();
    }
    std::fs::create_dir_all(top.join(".git/info")).unwrap();
    std::fs::write(
        top.join(".git/info/attributes"),
        "info.txt filter=info\nempty.txt filter=\n",
    )
    .unwrap();
    let attributes = world.sources.path().join("attributes");
    std::fs::write(&attributes, "global.txt filter=global\n").unwrap();
    world.global(&format!(
        "[core]\n\tattributesFile = {}\n[filter \"global\"]\n\tclean = {}\n",
        attributes.display(),
        program("global-filter")
    ));
    // An entry stamped later than any index written now stays racily clean, so each index
    // rewrite hashes its file again, through its filter.
    for name in [
        "racy.txt",
        "info.txt",
        "global.txt",
        "proc.txt",
        "empty.txt",
    ] {
        let racy = top.join(name);
        std::fs::write(&racy, "racy\n").unwrap();
        std::fs::File::options()
            .write(true)
            .open(&racy)
            .unwrap()
            .set_modified(std::time::SystemTime::now() + Duration::from_secs(86_400))
            .unwrap();
    }
    world.ok(
        &top,
        &[
            "add",
            ".gitattributes",
            "racy.txt",
            "info.txt",
            "global.txt",
            "proc.txt",
            "empty.txt",
        ],
    );
    let _ = std::fs::remove_file(&marker);
    let start = index_format(&world, &top);
    assert_eq!(
        world.tuned(),
        format!("tuned $HOME/trust (index format {start} -> 4)")
    );
    assert_eq!(index_format(&world, &top), INDEX_VERSION);
    assert!(
        !marker.exists(),
        "ran: {}",
        std::fs::read_to_string(&marker).unwrap_or_default()
    );
    // The control: a plain git that rewrites the index runs every one of them.
    world.plain(&top, &["update-index", "--index-version", "2"]);
    let ran = std::fs::read_to_string(&marker).unwrap_or_default();
    for tag in NAMED {
        assert!(
            ran.lines().any(|line| line == tag),
            "{tag} is live: {ran:?}"
        );
    }
}

/// A key some scope already sets is somebody's choice: kept, and named in no change. An
/// explicit `index.version` keeps its index at its own format.
#[test]
fn a_key_set_in_any_scope_and_an_explicit_index_version_are_kept() {
    let world = World::new();
    world.global("[core]\n\tuntrackedCache = false\n");
    let top = world.clone("trust", &world.source("trust", &[(DECLARATION, TRUST)]));
    world.ok(&top, &["config", "index.version", "2"]);
    world.inventory(&["trust"]);
    world.kept_self_test(true);
    assert_eq!(due(&world.read()), "$HOME/trust: feature.manyFiles unset");
    world.apply().unwrap();
    assert_eq!(
        world.local(&top, "core.untrackedCache"),
        "",
        "the global false is kept"
    );
    assert_eq!(world.local(&top, "feature.manyFiles"), "true");
    assert_eq!(
        index_format(&world, &top),
        2,
        "index.version=2 keeps its index"
    );
    current(&world.read());
}

/// A clone another git is writing in is left for the next pass, as a whole: its lock is
/// neither waited on nor removed.
#[test]
fn a_held_index_lock_or_config_lock_leaves_the_clone_for_the_next_pass() {
    let world = World::new();
    let top = world.clone("trust", &world.source("trust", &[(DECLARATION, TRUST)]));
    world.inventory(&["trust"]);
    world.kept_self_test(true);
    let start = index_format(&world, &top);
    for lock in ["index.lock", "config.lock"] {
        let held = top.join(".git").join(lock);
        std::fs::write(&held, "").unwrap();
        assert_eq!(
            world.read(),
            Reading::Deferred(format!(
                "$HOME/trust: its {lock} is there (another git holds it, or one that stopped left \
                 it); the next pass tunes it"
            ))
        );
        assert_eq!(
            world.apply().unwrap(),
            Tuned {
                said: "nothing tuned".into(),
                aside: Some(format!(
                    "$HOME/trust: its {lock} is there (another git holds it, or one that stopped left \
                 it); the next pass tunes it"
                )),
            }
        );
        assert!(held.exists(), "{lock} is never removed");
        assert_eq!(world.local(&top, "feature.manyFiles"), "");
        assert_eq!(index_format(&world, &top), start);
        std::fs::remove_file(&held).unwrap();
    }
    world.apply().unwrap();
    assert_eq!(index_format(&world, &top), INDEX_VERSION);
}

/// A volume whose self-test fails gets neither cache key, and its result is kept: the next pass
/// runs no self-test and changes nothing.
#[test]
fn a_failed_self_test_withholds_the_cache_keys_and_is_kept() {
    let world = World::new();
    let top = world.clone("trust", &world.with_submodules("trust", Some(TRUST)));
    world.inventory(&["trust"]);
    let start = index_format(&world, &top);
    let said = apply_with(
        &world.sys,
        &world.ctx(),
        world.state(),
        &Holds::NONE,
        &Seams {
            self_test: &|_, _, _| Ok(false),
            planned: &|| {},
            lock_wait: LOCK_WAIT,
        },
    )
    .unwrap();
    assert_eq!(said.said, "nothing tuned");
    assert_eq!(world.local(&top, "core.untrackedCache"), "");
    assert_eq!(world.local(&top, "feature.manyFiles"), "");
    assert_eq!(
        world.local(&top.join("first-party/sub"), "core.untrackedCache"),
        ""
    );
    assert_eq!(index_format(&world, &top), start);
    assert_eq!(
        current(&world.read()),
        "1 vault clone on this Mac, each as it declares; $HOME/trust: no untracked cache: git's \
         untracked-cache self-test failed on its volume"
    );
    assert_eq!(
        world.tuned(),
        "nothing tuned",
        "the kept result, no self-test"
    );
}

/// A key the person removed after aterm set it is their opt-out: aterm does not set it again,
/// and says what still turns the cache on. A clone made again at the same path is a new one.
#[test]
fn a_key_removed_after_aterm_set_it_is_not_set_again() {
    let world = World::new();
    let top = world.clone("trust", &world.source("trust", &[(DECLARATION, TRUST)]));
    world.inventory(&["trust"]);
    world.kept_self_test(true);
    world.tuned();
    world.ok(&top, &["config", "--unset", "core.untrackedCache"]);
    assert_eq!(
        current(&world.read()),
        "1 vault clone on this Mac, each as it declares; $HOME/trust: core.untrackedCache no longer \
         holds the true aterm set in its own config; aterm does not set it again (while \
         feature.manyFiles is on, the untracked cache stays on unless core.untrackedCache is \
         false)"
    );
    assert_eq!(world.tuned(), "nothing tuned");
    assert_eq!(world.local(&top, "core.untrackedCache"), "");
    // A clone made again at the same path is a new one: aterm's record of the old one is not
    // its opt-out.
    std::fs::remove_dir_all(&top).unwrap();
    world.clone("trust", &world.sources.path().join("trust"));
    world.tuned();
    assert_eq!(world.local(&top, "core.untrackedCache"), "true");
}

/// A key the declaration drops is unset while it holds aterm's value, and left once it holds
/// another: the person's.
#[test]
fn a_key_the_declaration_drops_is_unset_only_while_it_holds_aterms_value() {
    let world = World::new();
    let source = world.with_submodules("trust", Some(TRUST));
    let top = world.clone("trust", &source);
    let sub = top.join("first-party/sub");
    world.inventory(&["trust"]);
    world.kept_self_test(true);
    world.tuned();
    // The person turns feature.manyFiles off themselves; then the declaration drops both
    // superproject keys and every submodule key.
    world.ok(&top, &["config", "feature.manyFiles", "false"]);
    std::fs::write(source.join(DECLARATION), "[tuning]\n\tindexVersion = 4\n").unwrap();
    world.ok(&source, &["commit", "--quiet", "-am", "drop"]);
    // The worktree's copy is never read: only the default branch as last fetched, which still
    // declares feature.manyFiles, so the person's false is their opt-out.
    assert_eq!(
        current(&world.read()),
        "1 vault clone on this Mac, each as it declares; $HOME/trust: feature.manyFiles no longer \
         holds the true aterm set in its own config; aterm does not set it again"
    );
    world.ok(
        &top,
        &[
            "fetch",
            "--quiet",
            source.to_str().unwrap(),
            "main:refs/remotes/origin/main",
        ],
    );
    assert_eq!(
        due(&world.read()),
        "$HOME/trust: core.untrackedCache dropped from its declaration but still set, \
         core.untrackedCache dropped but still set in 2 submodules"
    );
    world.tuned();
    assert_eq!(world.local(&top, "core.untrackedCache"), "");
    assert_eq!(
        world.local(&top, "feature.manyFiles"),
        "false",
        "the person's value stays"
    );
    assert_eq!(world.local(&sub, "core.untrackedCache"), "");
    current(&world.read());
}

/// What a declaration names outside aterm's allowlist is named and never applied; the rest of
/// it still is. An oversized declaration is refused whole.
#[test]
fn a_refused_key_is_named_and_never_applied() {
    let world = World::new();
    let declaration = "[tuning]\n\tsuperproject = core.hooksPath\n\tsuperproject = core.fsmonitor\n\tsuperproject = core.preloadIndex\n\tsuperproject = feature.manyFiles\n\tsuperproject = core.untrackedCache\n\tsubmodule = feature.manyFiles\n\tindexVersion = 3\n\tnever = core.untrackedCache\n[other]\n\tthing = 1\n";
    let top = world.clone("trust", &world.with_submodules("trust", Some(declaration)));
    world.inventory(&["trust"]);
    world.kept_self_test(true);
    let start = index_format(&world, &top);
    world.apply().unwrap();
    for key in [
        "core.hooksPath",
        "core.fsmonitor",
        "core.preloadIndex",
        "core.untrackedCache",
    ] {
        assert_eq!(world.local(&top, key), "", "{key} is not applied");
    }
    assert_eq!(
        world.local(&top, "feature.manyFiles"),
        "true",
        "the allowed key still is"
    );
    assert_eq!(
        world.local(&top.join("first-party/sub"), "feature.manyFiles"),
        ""
    );
    assert_eq!(
        index_format(&world, &top),
        start,
        "aterm converts only to 4"
    );
    let now = world.read();
    let now = current(&now);
    for named in [
        "declares tuning.superproject=core.hooksPath, outside aterm's allowlist: not applied",
        "declares tuning.superproject=core.fsmonitor, outside aterm's allowlist: not applied",
        "declares tuning.superproject=core.preloadIndex, outside aterm's allowlist: not applied",
        "declares tuning.submodule=feature.manyFiles, outside aterm's allowlist: not applied",
        "declares index format 3; aterm converts an index only to 4",
        "declares core.untrackedCache both to set and never set: not set",
        "declares other.thing, no tuning entry aterm reads: ignored",
    ] {
        assert!(
            now.contains(&format!("$HOME/trust {named}")),
            "{named:?} in {now:?}"
        );
    }

    let big = world.clone(
        "big",
        &world.source(
            "big",
            &[(DECLARATION, &format!("{TRUST}# {}\n", "x".repeat(70_000)))],
        ),
    );
    world.inventory(&["trust", "big"]);
    let now = world.read();
    assert!(
        current(&now).contains("~/big: its scripts/git-clone.gitconfig on main is "),
        "{now:?}"
    );
    world.tuned();
    assert_eq!(world.local(&big, "feature.manyFiles"), "", "refused whole");
}

/// Only the inventory's present clones are in scope: another repository in the home, however it
/// declares, is never touched; and a clone that declares nothing gets nothing.
#[test]
fn a_folder_the_inventory_does_not_name_is_never_touched() {
    let world = World::new();
    let other = world.clone("other", &world.source("other", &[(DECLARATION, TRUST)]));
    let plain = world.clone("plain", &world.source("plain", &[("README.md", "plain\n")]));
    world.inventory(&["plain"]);
    world.kept_self_test(true);
    assert_eq!(
        current(&world.read()),
        "1 vault clone on this Mac, each as it declares (0 declare tuning)"
    );
    world.apply().unwrap();
    for repo in [&other, &plain] {
        assert_eq!(world.local(repo, "core.untrackedCache"), "");
        assert_eq!(world.local(repo, "feature.manyFiles"), "");
    }
    // The control: listed, the same clone is tuned.
    world.inventory(&["plain", "other"]);
    world.apply().unwrap();
    assert_eq!(world.local(&other, "feature.manyFiles"), "true");
}

/// A System State hold of GitHub repositories, `[github] enabled = false` and an explicit
/// profile each put every clone out of scope: read not applicable, applied nothing.
#[test]
fn a_hold_github_off_or_an_explicit_profile_tunes_nothing() {
    let world = World::new();
    let top = world.clone("trust", &world.source("trust", &[(DECLARATION, TRUST)]));
    world.inventory(&["trust"]);
    world.kept_self_test(true);
    let holds = repositories_held();
    assert_eq!(
        world.read_holding(&holds),
        Reading::NotApplicable("left as you set it in System State".into())
    );
    assert_eq!(
        apply_with(
            &world.sys,
            &world.ctx(),
            world.state(),
            &holds,
            &Seams {
                self_test: &|_, _, _| unreachable!(),
                planned: &|| {},
                lock_wait: LOCK_WAIT,
            },
        )
        .unwrap(),
        Tuned {
            said: "nothing tuned".into(),
            aside: Some("left as you set it in System State".into())
        }
    );
    for (toml, why) in [
        (
            "[github]\nenabled = false\n",
            "off: init.toml's [github] enabled = false",
        ),
        (
            "[github]\naccount = \"fixture\"\n",
            "init.toml's [github] profile names its own repositories, and aterm tunes the vault \
             inventory's clones alone",
        ),
    ] {
        world.init_toml(toml);
        assert_eq!(world.read(), Reading::NotApplicable(why.into()));
        world.apply().unwrap();
    }
    assert_eq!(world.local(&top, "feature.manyFiles"), "");
    // The control: with neither, the clone is tuned.
    std::fs::remove_file(world.home.path().join(".config/aterm/init.toml")).unwrap();
    world.apply().unwrap();
    assert_eq!(world.local(&top, "feature.manyFiles"), "true");
}

/// A partial clone's declaration blob it never fetched is never fetched: neither the read nor
/// the apply reaches origin for it, and the clone is left as it is until a fetch brings it.
#[test]
fn a_partial_clone_s_unfetched_declaration_is_never_fetched() {
    let world = World::new();
    let source = world.source("trust", &[("README.md", "fixture\n")]);
    world.ok(&source, &["config", "uploadpack.allowFilter", "true"]);
    world.ok(
        &source,
        &["config", "uploadpack.allowAnySHA1InWant", "true"],
    );
    let top = world.home.path().join("trust");
    world.ok(
        world.home.path(),
        &[
            "clone",
            "--quiet",
            "--filter=blob:none",
            &format!("file://{}", source.display()),
            top.to_str().unwrap(),
        ],
    );
    world.ok(
        &top,
        &[
            "remote",
            "set-url",
            "origin",
            "https://github.com/fixture/trust.git",
        ],
    );
    // A fetch of origin, lazy or not, reaches the local source.
    world.ok(
        &top,
        &[
            "config",
            &format!("url.file://{}.insteadOf", source.display()),
            "https://github.com/fixture/trust.git",
        ],
    );
    world.inventory(&["trust"]);
    world.kept_self_test(true);
    std::fs::create_dir_all(source.join("scripts")).unwrap();
    std::fs::write(source.join(DECLARATION), TRUST).unwrap();
    world.ok(&source, &["add", DECLARATION]);
    world.ok(&source, &["commit", "--quiet", "-m", "declare"]);
    world.ok(&top, &["fetch", "--quiet", "origin"]);
    let blob = world.ok(&top, &["rev-parse", &format!("origin/main:{DECLARATION}")]);
    let here = |world: &World| {
        world
            .sys
            .run_complete_stdout(
                &local(&world.sys, &world.who, Some(&top)).args(["cat-file", "-e", &blob]),
            )
            .unwrap()
            .success()
    };
    assert!(!here(&world), "the filtered fetch left the blob on origin");
    let now = world.read();
    assert!(
        current(&now).contains(
            "$HOME/trust: its scripts/git-clone.gitconfig on main has not been fetched to this Mac \
             (a partial clone's), so nothing there changed"
        ),
        "{now:?}"
    );
    world.tuned();
    assert!(!here(&world), "nothing aterm ran fetched it");
    assert_eq!(world.local(&top, "feature.manyFiles"), "");
    // The control: a git that may fetch a missing object does, from that origin.
    world.ok(&top, &["cat-file", "-e", &blob]);
    assert!(here(&world), "a lazy fetch reaches origin");
}

/// A `.gitmodules` path reached through a link inside the clone names no submodule of it: the
/// repository it lands in is never touched.
#[test]
fn a_submodule_path_through_a_link_is_never_touched() {
    let world = World::new();
    let top = world.clone("trust", &world.source("trust", &[(DECLARATION, TRUST)]));
    let elsewhere = world.clone(
        "elsewhere",
        &world.source("elsewhere", &[("README.md", "elsewhere\n")]),
    );
    world.inventory(&["trust"]);
    world.kept_self_test(true);
    std::os::unix::fs::symlink(world.home.path(), top.join("link")).unwrap();
    std::fs::write(
        top.join(".gitmodules"),
        "[submodule \"x\"]\n\tpath = link/elsewhere\n\turl = https://example.invalid/x\n",
    )
    .unwrap();
    world.tuned();
    assert_eq!(world.local(&top, "feature.manyFiles"), "true");
    assert_eq!(
        world.local(&elsewhere, "core.untrackedCache"),
        "",
        "outside the clone"
    );
    // The control: the same repository at that path inside the clone is its submodule.
    std::fs::remove_file(top.join("link")).unwrap();
    std::fs::rename(&elsewhere, top.join("elsewhere")).unwrap();
    std::fs::write(
        top.join(".gitmodules"),
        "[submodule \"x\"]\n\tpath = elsewhere\n\turl = https://example.invalid/x\n",
    )
    .unwrap();
    world.tuned();
    assert_eq!(
        world.local(&top.join("elsewhere"), "core.untrackedCache"),
        "true"
    );
}

/// A value the repository's config only includes is not aterm's: a key the declaration drops is
/// unset only from the repository's own file, so one the person moved to an included file
/// stays, and the apply fails nothing.
#[test]
fn an_included_value_is_never_taken_for_aterms() {
    let world = World::new();
    let source = world.source("trust", &[(DECLARATION, TRUST)]);
    let top = world.clone("trust", &source);
    world.inventory(&["trust"]);
    world.kept_self_test(true);
    world.tuned();
    let included = top.join(".git/person.inc");
    std::fs::write(&included, "[core]\n\tuntrackedCache = true\n").unwrap();
    world.ok(&top, &["config", "--unset", "core.untrackedCache"]);
    world.ok(&top, &["config", "include.path", "person.inc"]);
    world.declare(
        &source,
        &top,
        "[tuning]\n\tsuperproject = feature.manyFiles\n\tindexVersion = 4\n",
    );
    current(&world.read());
    assert_eq!(world.tuned(), "nothing tuned");
    assert_eq!(
        world.ok(&top, &["config", "--get", "core.untrackedCache"]),
        "true",
        "the person's included value stays"
    );
    // The control: aterm's own value, in the repository's own file, is unset once dropped.
    world.ok(&top, &["config", "--unset", "include.path"]);
    world.ok(&top, &["config", "core.untrackedCache", "true"]);
    assert_eq!(
        due(&world.read()),
        "$HOME/trust: core.untrackedCache dropped from its declaration but still set"
    );
}

/// A record a newer aterm wrote leaves the clones to it: read not applicable, nothing applied,
/// and the record as it was.
#[test]
fn a_newer_aterm_s_record_leaves_the_clones_to_it() {
    let world = World::new();
    let top = world.clone("trust", &world.source("trust", &[(DECLARATION, TRUST)]));
    world.inventory(&["trust"]);
    let dir = converge_dir(world.state());
    aterm_uds::private_file::ensure_private_dir(&dir).unwrap();
    let newer = format!(
        r#"{{"schema":{},"repositories":{{"1:2":{{"path":"/x","set":[],"why":"newer"}}}}}}"#,
        SCHEMA + 1
    );
    aterm_uds::private_file::write_private(&dir, RECORD, newer.as_bytes()).unwrap();
    let why = format!(
        "{} is a newer aterm's, which tunes these clones; this one changes nothing",
        dir.join(RECORD).display()
    );
    assert_eq!(world.read(), Reading::NotApplicable(why.clone()));
    assert_eq!(
        world.apply().unwrap(),
        Tuned {
            said: "nothing tuned".into(),
            aside: Some(why)
        }
    );
    assert_eq!(world.local(&top, "feature.manyFiles"), "");
    assert_eq!(std::fs::read(dir.join(RECORD)).unwrap(), newer.as_bytes());
}

/// An oversized `.gitmodules` is not read: its submodules are left, named, and the clone's own
/// keys are still set.
#[test]
fn an_oversized_gitmodules_is_never_read() {
    let world = World::new();
    let top = world.clone("trust", &world.with_submodules("trust", Some(TRUST)));
    world.inventory(&["trust"]);
    world.kept_self_test(true);
    let listing = top.join(".gitmodules");
    let mut text = std::fs::read_to_string(&listing).unwrap();
    text.push_str(&format!("# {}\n", "x".repeat(300_000)));
    std::fs::write(&listing, text).unwrap();
    world.tuned();
    assert_eq!(world.local(&top, "feature.manyFiles"), "true");
    assert_eq!(
        world.local(&top.join("first-party/sub"), "core.untrackedCache"),
        ""
    );
    let now = world.read();
    let now = current(&now);
    assert!(
        now.contains("$HOME/trust: its .gitmodules is ")
            && now.contains(
                " bytes, over the 262144 aterm reads, so its submodules are left as they are"
            ),
        "{now:?}"
    );
}

/// A passing self-test vouches only for the volume of aterm's own state: a repository on another
/// volume gets no cache key, whatever a record says of a volume with its number.
#[test]
fn a_repository_off_the_self_test_s_volume_gets_no_cache_key() {
    let world = World::new();
    let top = world.clone("trust", &world.source("trust", &[(DECLARATION, TRUST)]));
    world.inventory(&["trust"]);
    world.kept_self_test(true);
    let dir = converge_dir(world.state());
    let record = Record::read(&dir).unwrap().unwrap();
    let dev = test_volume(&world.sys, &dir).unwrap();
    let elsewhere = plan_all(
        &world.sys,
        &world.who,
        std::slice::from_ref(&top),
        &record,
        Some(dev + 1),
    );
    assert_eq!(
        current(&summary(&elsewhere)),
        "1 vault clone on this Mac, each as it declares; $HOME/trust: no untracked cache: it is not \
         on the volume of aterm's own state, where git's untracked-cache self-test runs"
    );
    // The control: on the self-test's volume, the same record tunes it.
    let here = plan_all(&world.sys, &world.who, &[top], &record, Some(dev));
    due(&summary(&here));
}

/// The doctor's row reads System State's holds at its apply as well as its probe: a hold set
/// between them stands the row aside, and so does a lock another git takes between them; the
/// row is never failed for either, and nothing is written.
#[test]
fn a_hold_or_a_lock_taken_after_the_row_read_stands_its_apply_aside() {
    let world = World::new();
    let top = world.clone("trust", &world.source("trust", &[(DECLARATION, TRUST)]));
    world.inventory(&["trust"]);
    world.kept_self_test(true);
    let ctx = world.ctx();
    let run = |holds: HoldsNow| {
        let item = item(&ctx, world.state(), holds).unwrap();
        aterm_converge::run_scoped(
            &crate::engine::bind([(item.as_ref(), place())], &world.sys, &ctx),
            aterm_converge::Mode::Apply,
            aterm_converge::Scope::REPAIR,
            &mut aterm_converge::NoJournal,
            &mut |_| {},
        )
        .unwrap()
    };
    let reads = Arc::new(AtomicUsize::new(0));
    let counted = Arc::clone(&reads);
    let rows = run(Box::new(move || {
        if counted.fetch_add(1, Ordering::SeqCst) == 0 {
            Holds::NONE
        } else {
            repositories_held()
        }
    }));
    assert_eq!(
        reads.load(Ordering::SeqCst),
        2,
        "read at the probe and the apply"
    );
    assert_eq!(
        (rows[0].outcome, rows[0].why.as_deref()),
        (Outcome::Blocked, Some("left as you set it in System State")),
        "{rows:?}"
    );
    assert_eq!(world.local(&top, "feature.manyFiles"), "");

    let lock = top.join(".git/config.lock");
    let taken = Arc::new(AtomicUsize::new(0));
    let counted = Arc::clone(&taken);
    let at = lock.clone();
    let rows = run(Box::new(move || {
        if counted.fetch_add(1, Ordering::SeqCst) == 1 {
            std::fs::write(&at, "").unwrap();
        }
        Holds::NONE
    }));
    assert_eq!(
        (rows[0].outcome, rows[0].why.as_deref()),
        (
            Outcome::Blocked,
            Some(
                "$HOME/trust: its config.lock is there (another git holds it, or one that stopped \
                 left it); the next pass tunes it"
            )
        ),
        "{rows:?}"
    );
    assert!(lock.exists());
    assert_eq!(world.local(&top, "feature.manyFiles"), "");
    // The control: with neither, the row tunes the clone.
    std::fs::remove_file(&lock).unwrap();
    let rows = run(Box::new(|| Holds::NONE));
    assert_eq!(rows[0].outcome, Outcome::Changed, "{rows:?}");
    assert_eq!(world.local(&top, "feature.manyFiles"), "true");
}

/// A clone whose config names a hook in bytes that are not UTF-8 is left as it is: aterm cannot
/// pass that name to git to turn the hook off, so it writes nothing there and says why. Under a
/// UTF-8 locale (the test's, and an aterm tab's) git's `.` matches no such byte, so this also
/// holds [`checkout::Programs::listing`] to the C locale, where its expression finds the key;
/// a plain git runs the hook.
#[test]
fn a_hook_named_in_bytes_that_are_not_utf8_leaves_the_clone_as_it_is() {
    use std::io::Write as _;
    use std::os::unix::ffi::OsStrExt as _;
    let world = World::with_named_hooks();
    let top = world.clone("trust", &world.source("trust", &[(DECLARATION, TRUST)]));
    world.inventory(&["trust"]);
    world.kept_self_test(true);
    let marker = world.sources.path().join("ran");
    let program = world.sources.path().join("hook");
    stand_in(&program, &marker, "hook");
    let mut config = std::fs::File::options()
        .append(true)
        .open(top.join(".git/config"))
        .unwrap();
    config
        .write_all(b"[hook \"\xff\"]\n\tevent = post-index-change\n\tcommand = ")
        .unwrap();
    config.write_all(program.as_os_str().as_bytes()).unwrap();
    config.write_all(b"\n").unwrap();
    drop(config);
    let start = index_format(&world, &top);
    let why = "$HOME/trust: its config names a hook or filter whose name is not UTF-8, which aterm \
               cannot turn off; the clone is left as it is";
    let reading = world.read();
    assert!(current(&reading).ends_with(why), "{reading:?}");
    assert_eq!(world.tuned(), NOTHING);
    assert_eq!(world.local(&top, "feature.manyFiles"), "");
    assert_eq!(index_format(&world, &top), start);
    assert!(!marker.exists(), "nothing ran");
    // The control: the hook is live.
    world.plain(&top, &["update-index", "--index-version", "3"]);
    assert_eq!(
        std::fs::read_to_string(&marker).unwrap_or_default().trim(),
        "hook"
    );
}

/// A lock another git takes after the plan looked, before aterm writes, leaves the clone for the
/// next pass and fails nothing: the superproject's `config.lock` before any key is written, a
/// submodule's after the superproject's keys, an `index.lock` after every key and before the
/// index; the keys written are said and recorded. No lock is removed.
#[test]
fn a_lock_taken_after_the_plan_leaves_the_clone_for_the_next_pass() {
    let world = World::new();
    let top = world.clone("trust", &world.with_submodules("trust", Some(TRUST)));
    let sub = top.join("first-party/sub");
    world.inventory(&["trust"]);
    world.kept_self_test(true);
    let start = index_format(&world, &top);
    let taken_after_the_plan = |lock: &Path| {
        apply_with(
            &world.sys,
            &world.ctx(),
            world.state(),
            &Holds::NONE,
            &Seams {
                self_test: &|_, _, _| panic!("the volume's self-test result is kept"),
                planned: &|| std::fs::write(lock, "").unwrap(),
                lock_wait: LOCK_WAIT,
            },
        )
    };
    let held = |label: &str, lock: &str| {
        Some(format!(
            "{label}: its {lock} is there (another git holds it, or one that stopped left it); \
             the next pass tunes it"
        ))
    };
    let config = top.join(".git/config.lock");
    assert_eq!(
        taken_after_the_plan(&config),
        Ok(Tuned {
            said: NOTHING.into(),
            aside: held("$HOME/trust", "config.lock"),
        })
    );
    assert!(config.exists(), "never removed");
    assert_eq!(world.local(&top, "feature.manyFiles"), "");
    assert_eq!(world.owned(&top), None);
    std::fs::remove_file(&config).unwrap();

    let config = top.join(".git/modules/first-party/sub/config.lock");
    assert_eq!(
        taken_after_the_plan(&config),
        Ok(Tuned {
            said: "tuned $HOME/trust".into(),
            aside: held("$HOME/trust/first-party/sub", "config.lock"),
        })
    );
    assert!(config.exists(), "never removed");
    assert_eq!(world.local(&top, "feature.manyFiles"), "true");
    assert_eq!(world.local(&sub, "core.untrackedCache"), "");
    std::fs::remove_file(&config).unwrap();

    let index = top.join(".git/index.lock");
    assert_eq!(
        taken_after_the_plan(&index),
        Ok(Tuned {
            said: "tuned $HOME/trust".into(),
            aside: held("$HOME/trust", "index.lock"),
        })
    );
    assert!(index.exists(), "never removed");
    assert_eq!(world.local(&top, "core.untrackedCache"), "true");
    assert_eq!(world.local(&sub, "core.untrackedCache"), "true");
    let owned = world.owned(&top).unwrap().set;
    assert!(
        owned.contains(&"feature.manyFiles".to_owned())
            && owned.contains(&"core.untrackedCache".to_owned()),
        "{owned:?}"
    );
    assert_eq!(
        world.owned(&sub).unwrap().set,
        vec!["core.untrackedCache".to_owned()]
    );
    assert_eq!(index_format(&world, &top), start);
    std::fs::remove_file(&index).unwrap();
    assert_eq!(
        world.tuned(),
        format!("tuned $HOME/trust (index format {start} -> 4)")
    );
}

/// A git directory's record key is its inode and birth time where the filesystem keeps one: an
/// inode number reused for a clone made again at the same path names another repository, and a
/// volume numbered otherwise after a restart (`st_dev`) names the same one.
#[test]
fn a_git_directory_s_key_is_its_inode_and_birth_time() {
    let id = |dev, born_ns| FileId {
        dev,
        ino: 2,
        born_ns,
    };
    assert_eq!(git_dir_key(id(1, Some(3))), "2@3");
    assert_ne!(git_dir_key(id(1, Some(3))), git_dir_key(id(1, Some(4))));
    assert_eq!(git_dir_key(id(1, Some(3))), git_dir_key(id(9, Some(3))));
    assert_eq!(git_dir_key(id(1, None)), "1:2");
}

/// Git's self-test verdict is its exit: 0 passes and 1 fails, each kept; any other end (a
/// `die`, as on a full disk; a signal) is no verdict, and nothing is kept.
#[test]
fn only_git_s_own_self_test_verdict_is_kept() {
    assert_eq!(verdict(&Ran::ok("")), Ok(true));
    assert_eq!(verdict(&Ran::exit(1, "")), Ok(false));
    assert!(
        verdict(&Ran::exit(128, "fatal: could not make temporary directory"))
            .unwrap_err()
            .ends_with("the next pass tests again")
    );
    assert!(
        verdict(&Ran {
            status: None,
            ..Ran::ok("")
        })
        .is_err()
    );
}

/// An older aterm's record is read as a fresh one: what it claims is forgotten, so each key it
/// set is kept as the person's, and its self-tests run again.
#[test]
fn an_older_aterm_s_record_is_read_afresh() {
    let world = World::new();
    let dir = converge_dir(world.state());
    aterm_uds::private_file::ensure_private_dir(&dir).unwrap();
    aterm_uds::private_file::write_private(
        &dir,
        RECORD,
        br#"{"schema":1,"volumes":{"1":{"passed":true,"at_s":1}},"repositories":{"1:2:3":{"path":"/x","set":["feature.manyFiles"]}}}"#,
    )
    .unwrap();
    let record = Record::read(&dir).unwrap().unwrap();
    assert_eq!(
        (
            record.schema,
            record.volumes.len(),
            record.repositories.len()
        ),
        (SCHEMA, 0, 0)
    );
}

/// Another aterm tuning the clones now (its lock held past the wait) stands this apply aside
/// for the next pass: nothing written, nothing failed.
#[test]
fn another_aterm_tuning_now_stands_the_apply_aside() {
    let world = World::new();
    let top = world.clone("trust", &world.source("trust", &[(DECLARATION, TRUST)]));
    world.inventory(&["trust"]);
    world.kept_self_test(true);
    let dir = converge_dir(world.state());
    let _other = aterm_uds::private_file::PrivateLock::take(&dir, LOCK, LOCK_WAIT, "held").unwrap();
    assert_eq!(
        apply_with(
            &world.sys,
            &world.ctx(),
            world.state(),
            &Holds::NONE,
            &Seams {
                self_test: &|_, _, _| panic!("nothing is planned"),
                planned: &|| panic!("nothing is planned"),
                lock_wait: Duration::ZERO,
            },
        ),
        Ok(Tuned {
            said: NOTHING.into(),
            aside: Some(
                "another aterm tunes the vault's clones now; the next pass tunes them".into()
            ),
        })
    );
    assert_eq!(world.local(&top, "feature.manyFiles"), "");
}

/// A write that fails leaves the row failed, naming it, and says what the rest of the apply
/// tuned: a submodule whose git directory refuses the config's lock, beside a superproject that
/// tunes.
#[test]
fn a_failed_write_says_what_the_rest_tuned() {
    let world = World::new();
    let top = world.clone("trust", &world.with_submodules("trust", Some(TRUST)));
    world.inventory(&["trust"]);
    world.kept_self_test(true);
    let start = index_format(&world, &top);
    let modules = top.join(".git/modules/first-party/sub");
    set_mode(&modules, 0o500);
    let applied = world.apply();
    set_mode(&modules, 0o700);
    let why = applied.unwrap_err();
    assert!(
        why.starts_with("$HOME/trust/first-party/sub: core.untrackedCache: ")
            && why.ends_with(&format!("; tuned $HOME/trust (index format {start} -> 4)")),
        "{why}"
    );
    assert_eq!(world.local(&top, "feature.manyFiles"), "true");
}

/// A clone whose config is a link is left as it is: git would write through it, into a file
/// outside the clone that others may share.
#[test]
fn a_clone_whose_config_is_a_link_is_left_as_it_is() {
    let world = World::new();
    let top = world.clone("trust", &world.source("trust", &[(DECLARATION, TRUST)]));
    world.inventory(&["trust"]);
    world.kept_self_test(true);
    let shared = world.sources.path().join("shared.gitconfig");
    std::fs::rename(top.join(".git/config"), &shared).unwrap();
    std::os::unix::fs::symlink(&shared, top.join(".git/config")).unwrap();
    let start = index_format(&world, &top);
    let reading = world.read();
    assert!(
        current(&reading).ends_with(
            "$HOME/trust: its config is a link, which aterm does not write through; it is left as \
             it is"
        ),
        "{reading:?}"
    );
    assert_eq!(world.tuned(), NOTHING);
    assert!(
        !std::fs::read_to_string(&shared)
            .unwrap()
            .contains("manyFiles"),
        "nothing written through the link"
    );
    assert_eq!(index_format(&world, &top), start);
}

/// A git that refuses `--no-lazy-fetch` as unknown (129) puts the clones out of scope, said; a
/// git that did not run as git is an error; one with the option tunes.
#[test]
fn a_git_without_no_lazy_fetch_tunes_nothing() {
    assert_eq!(lazy_fetch_guard(&Ran::ok("git version 2.56.0\n")), Ok(None));
    assert!(
        lazy_fetch_guard(&Ran::exit(129, "unknown option: --no-lazy-fetch"))
            .unwrap()
            .is_some_and(|why| why.contains("no --no-lazy-fetch"))
    );
    assert!(lazy_fetch_guard(&Ran::exit(127, "not found")).is_err());
}

/// The installed Git 2.56 control for named config hooks and config-lock waiting.
/// Native Git remains the other fixtures' baseline; an older one need not have these features.
fn feature_git() -> PathBuf {
    let home = aterm_uds::host::tool_home().unwrap_or_else(|| {
        aterm_uds::host::could_not_run("no tool home for the managed Git controls")
    });
    let git = aterm_toolchain::atpkg_prefix(&home, None).join("bin/git");
    let version = Live::new()
        .run_complete_stdout(&Cmd::new(&git).arg("--version"))
        .ok()
        .map(|ran| stdout(&ran))
        .unwrap_or_default();
    let minor = version
        .strip_prefix("git version 2.")
        .and_then(|rest| rest.split('.').next())
        .and_then(|minor| minor.parse::<u32>().ok());
    if !minor.is_some_and(|minor| minor >= 56) {
        aterm_uds::host::could_not_run(format!(
            "no git 2.56 or later at {} ({version:?}), needed for named-hook and config-lock controls",
            git.display()
        ));
    }
    git
}

/// A write never waits on a `config.lock` another git took after the plan looked, whatever wait
/// the person's own config asks git for: it fails at once, for the next pass. Run by a git that
/// waits ([`feature_git`]), on the command [`quiet`] builds.
#[test]
fn a_write_never_waits_on_a_config_lock() {
    let world = World::new();
    let git = feature_git();
    world.global("[core]\n\tconfigLockTimeout = 600000\n");
    let top = world.clone("trust", &world.source("trust", &[(DECLARATION, TRUST)]));
    std::fs::write(top.join(".git/config.lock"), "").unwrap();
    let (tx, rx) = std::sync::mpsc::channel();
    let who = world.who.clone();
    let at = top.clone();
    std::thread::spawn(move || {
        let sys = Live::new();
        let mut quiet = quiet(&sys, &who, &at).unwrap();
        quiet.program = git;
        tx.send(write(
            &sys,
            &quiet,
            &["config", "--local", "feature.manyFiles", "true"],
        ))
        .unwrap();
    });
    // A hang detector: git would wait ten minutes for the lock.
    let wrote = rx
        .recv_timeout(Duration::from_secs(60))
        .expect("the write waited on the lock");
    assert!(matches!(wrote, Err(Wrote::Locked)), "{wrote:?}");
    assert!(top.join(".git/config.lock").exists(), "never removed");
}

use aterm_types::macos_setup::Selection;
