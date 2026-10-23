// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;
use crate::checkout::tests::{Repos, rewrites, subcommands};
use crate::items::defaults::tests::{ctx, user};
use crate::model::{ItemReport, Mode, Outcome};
use crate::port::spawns::Spawns;
use crate::port::{Fake, Live, Ran, Stat};
use aterm_converge::{NoJournal, Scope};

/// A profile a person wrote: comments, spacing and a `[github]` table init must not touch.
const WRITTEN: &str = "# my init profile\n[github]\naccount = \"alice\"   # me\nowner = \
                       \"alice\"\ntoken_file = \"~/.config/gh-token\"\n\n# trailing words\n";

fn spelled(list: &[&str]) -> Vec<String> {
    list.iter().map(|&s| s.to_owned()).collect()
}

/// THE RECORD'S EDIT: `--develop` appends the `[develop]` table after a blank line and
/// every byte before it is the file's; `--no-develop` gives the file back byte for byte; a
/// second checkout rewrites only the array's own bytes; a table in the middle of the file,
/// with comments of its own, is edited and removed with every line outside it as it was.
/// Negative control: a whole-file rewrite through the decoder, run on the same file, is not
/// byte-identical (the comparison sees the comments go).
#[test]
fn the_profile_s_other_lines_are_byte_identical_after_develop_and_no_develop() {
    let one = spelled(&["~/aterm"]);
    let with = edited(WRITTEN, Some(&one)).unwrap();
    assert!(with.starts_with(WRITTEN), "{with}");
    assert_eq!(
        &with[WRITTEN.len()..],
        "\n[develop]\ncheckouts = [\"~/aterm\"]\n"
    );
    assert_eq!(edited(&with, None).unwrap(), WRITTEN);
    assert_eq!(edited(&with, Some(&one)).unwrap(), with);
    let two = spelled(&["~/aterm", "/Volumes/work/at \"x\""]);
    assert_eq!(
        edited(&with, Some(&two)).unwrap(),
        format!(
            "{WRITTEN}\n[develop]\ncheckouts = [\"~/aterm\", \"/Volumes/work/at \\\"x\\\"\"]\n"
        )
    );
    assert_eq!(edited(WRITTEN, None).unwrap(), WRITTEN);

    let unterminated = "[github]\naccount = \"alice\"\nowner = \"alice\"\ntoken_file = \"/t\"";
    let with_table = edited(unterminated, Some(&one)).unwrap();
    assert_eq!(
        with_table,
        format!("{unterminated}\n\n[develop]\ncheckouts = [\"~/aterm\"]")
    );
    assert_eq!(edited(&with_table, None).unwrap(), unterminated);
    assert_eq!(edited(&with_table, Some(&one)).unwrap(), with_table);
    assert_eq!(
        edited("", Some(&one)).unwrap(),
        "[develop]\ncheckouts = [\"~/aterm\"]\n"
    );
    assert_eq!(edited(&edited("", Some(&one)).unwrap(), None).unwrap(), "");

    let middle = "# top\n[develop]   # mine\n# which ones\ncheckouts = [\n  \"~/aterm\",\n]  # \
                  end\n# about github\n[github]\naccount = \"alice\"\nowner = \
                  \"alice\"\ntoken_file = \"/t\"\n";
    assert_eq!(
        edited(middle, Some(&two)).unwrap(),
        middle.replace(
            "[\n  \"~/aterm\",\n]",
            "[\"~/aterm\", \"/Volumes/work/at \\\"x\\\"\"]"
        )
    );
    assert_eq!(
        edited(middle, None).unwrap(),
        "# top\n# about github\n[github]\naccount = \"alice\"\nowner = \"alice\"\ntoken_file \
         = \"/t\"\n"
    );
    assert_eq!(
        edited("[develop]\n", Some(&one)).unwrap(),
        "[develop]\ncheckouts = [\"~/aterm\"]\n"
    );

    assert!(edited("develop.checkouts = []\n", Some(&one)).is_err());
    assert!(edited("develop = { checkouts = [] }\n", None).is_err());

    let value: aterm_toml::Value = aterm_toml::from_str(WRITTEN).unwrap();
    let rewritten = aterm_toml::to_string(&value).unwrap();
    assert!(!rewritten.starts_with(WRITTEN), "{rewritten}");
}

/// alice at uid 501 with an aterm checkout at `~/aterm` (its manifest is hers) and, when
/// given, a profile she wrote.
fn alice_with(profile: Option<&str>) -> (Fake, User) {
    let mut sys = Fake::root();
    let alice = user("alice", 501);
    sys.euid = alice.uid;
    let dir = || Stat {
        uid: 501,
        gid: 20,
        mode: 0o755,
        dir: true,
        symlink: false,
        len: 0,
    };
    sys.stat_of(&alice.home, dir());
    sys.stat_of("/Users//alice/aterm", dir());
    sys.write_user_file(
        &alice,
        Path::new("/Users//alice/aterm/Cargo.toml"),
        crate::checkout::tests::manifest(Path::new("/srv/origin.git")).as_bytes(),
        0o644,
    )
    .unwrap();
    if let Some(profile) = profile {
        sys.write_user_file(&alice, &profile_path(&alice), profile.as_bytes(), 0o600)
            .unwrap();
    }
    (sys, alice)
}

fn record_of(sys: &Fake, alice: &User, want: Option<bool>) -> Vec<ItemReport> {
    let ctx = ctx(std::slice::from_ref(alice));
    let items = compose(
        sys,
        &ctx,
        &Inputs {
            want,
            asked: false,
            cwd: Some(Path::new("/Users//alice/aterm/crates/aterm/src")),
            targo: None,
        },
    )
    .unwrap();
    let (record, place) = &items[0];
    assert_eq!(record.id(), RECORD);
    let bound = crate::engine::bind([(record.as_ref(), place.clone())], sys, &ctx);
    aterm_converge::run_scoped(
        &bound,
        Mode::Apply,
        Scope::EVERY,
        &mut NoJournal,
        &mut |_| {},
    )
    .unwrap()
}

/// THE RECORD ITEM: run inside a checkout (from a directory below its root), `--develop`
/// records `~/aterm`, writing only the appended table with the file's own mode; a second
/// `--develop` changes nothing; `--no-develop` leaves the file exactly as she wrote it.
#[test]
fn develop_records_the_workspace_it_runs_in_and_no_develop_gives_the_file_back() {
    let (sys, alice) = alice_with(Some(WRITTEN));
    let path = profile_path(&alice);
    let rows = record_of(&sys, &alice, Some(true));
    assert_eq!(rows[0].outcome, Outcome::Changed, "{rows:?}");
    assert_eq!(rows[0].layer, Some(Layer::Develop));
    assert_eq!(rows[0].class, Some(Class::Relay));
    assert_eq!(
        String::from_utf8(sys.written(path.to_str().unwrap()).unwrap()).unwrap(),
        format!("{WRITTEN}\n[develop]\ncheckouts = [\"~/aterm\"]\n")
    );
    assert_eq!(sys.mode_of(path.to_str().unwrap()), Some(0o600));
    let writes = || {
        sys.calls()
            .iter()
            .filter(|call| call.contains("$ write /Users//alice/.config"))
            .count()
    };
    let before = writes();
    let rows = record_of(&sys, &alice, Some(true));
    assert_eq!(rows[0].outcome, Outcome::Ok, "{rows:?}");
    assert_eq!(writes(), before);
    let rows = record_of(&sys, &alice, Some(false));
    assert_eq!(rows[0].outcome, Outcome::Changed, "{rows:?}");
    assert_eq!(
        String::from_utf8(sys.written(path.to_str().unwrap()).unwrap()).unwrap(),
        WRITTEN
    );
}

/// A Mac with no `[develop]` table, asked for nothing of the develop layer, composes no
/// develop item (the user profile); one whose table records a checkout composes its four.
/// `--develop` with nowhere to find a checkout is refused before anything is changed.
#[test]
fn the_develop_layer_follows_the_record_and_never_guesses() {
    let compose_for = |sys: &Fake, alice: &User, cwd: Option<&Path>, want| {
        compose(
            sys,
            &ctx(std::slice::from_ref(alice)),
            &Inputs {
                want,
                asked: false,
                cwd,
                targo: None,
            },
        )
    };
    let (sys, alice) = alice_with(Some(WRITTEN));
    assert!(compose_for(&sys, &alice, None, None).unwrap().is_empty());
    let (sys, alice) = alice_with(Some(&format!(
        "{WRITTEN}\n[develop]\ncheckouts = [\"~/aterm\"]\n"
    )));
    let ids: Vec<String> = compose_for(&sys, &alice, None, None)
        .unwrap()
        .iter()
        .map(|(item, _)| item.id())
        .collect();
    assert_eq!(
        ids,
        [
            "develop.record",
            "develop.checkout@~/aterm",
            "develop.submodules@~/aterm",
            "develop.workspace-tools@~/aterm"
        ]
    );
    let (sys, alice) = alice_with(None);
    let refused = compose_for(&sys, &alice, Some(Path::new("/Users//alice")), Some(true))
        .err()
        .unwrap();
    assert!(refused.contains("no aterm checkout to record"), "{refused}");
    assert!(
        sys.calls()
            .iter()
            .all(|call| !call.contains("$ write /Users//alice/.config"))
    );
}

/// Scripts the local git reads [`checkout::kept_here`] performs on `~/aterm`.
pub(crate) fn script_local(sys: &Fake, alice: &User, url: &str, branch: bool, status: &str) {
    let path = Path::new("/Users//alice/aterm");
    for (args, ran) in [
        (
            vec!["rev-parse", "--is-inside-work-tree"],
            Ran::ok("true\n"),
        ),
        (vec!["rev-parse", "--show-prefix"], Ran::ok("")),
        (
            vec!["config", "--get-all", "remote.origin.url"],
            Ran::ok(&format!("{url}\n")),
        ),
        (
            vec!["ls-remote", "--get-url", "origin"],
            Ran::ok(&format!("{url}\n")),
        ),
        (
            vec!["symbolic-ref", "--quiet", "HEAD"],
            if branch {
                Ran::ok("refs/heads/main\n")
            } else {
                Ran::exit(1, "")
            },
        ),
        (
            vec!["rev-parse", "--verify", "--quiet", "HEAD^{commit}"],
            Ran::ok("1111111111111111111111111111111111111111\n"),
        ),
        (
            vec![
                "status",
                "--porcelain=v1",
                "-z",
                "--untracked-files=normal",
                "--ignore-submodules=all",
            ],
            Ran::ok(status),
        ),
    ] {
        sys.on(&checkout::git(alice, Some(path)).args(args).display(), ran);
    }
}

/// The origin alice's fixture manifest admits (`[workspace.package] repository`).
pub(crate) const ORIGIN: &str = "/srv/origin.git";

/// `targo --unverified --locked verify provision`, run as alice in `~/aterm` with the
/// toolset's `bin` first on `PATH`.
const CHECK: &str = "cd /Users//alice/aterm && alice$ \
                     PATH=/Users//alice/.atpkg/bin:/usr/bin:/bin:/usr/sbin:/sbin \
                     /Users//alice/.atpkg/bin/targo --unverified --locked verify provision --check";

fn tools(targo: Option<Targo>) -> WorkspaceTools {
    WorkspaceTools {
        user: user("alice", 501),
        path: PathBuf::from("/Users//alice/aterm"),
        spelled: "~/aterm".into(),
        targo,
    }
}

fn found_targo() -> Targo {
    Arc::new(|| Ok(PathBuf::from("/Users//alice/.atpkg/bin/targo")))
}

/// THE TOOLS ITEM runs the checkout's own `targo --unverified --locked verify provision`
/// from inside the checkout (so its `.cargo/config.toml` alias applies, and cargo refuses to
/// rewrite its lock), as alice, with the toolset's `bin` first on `PATH` and nothing else
/// added to the environment: `--check` to read, the verb to apply, `--check` again to read
/// back. A tree that pins nothing for this host is not applicable; with no targo it is
/// blocked on what installs one.
#[test]
fn workspace_tools_run_the_checkouts_own_verify_provision_from_inside_it() {
    let provide = CHECK.strip_suffix(" --check").unwrap();
    let (sys, alice) = alice_with(None);
    script_local(&sys, &alice, ORIGIN, true, "");
    sys.on(
        CHECK,
        Ran::exit(
            1,
            "verify provision: fish — in place: /c/fish\nverify provision: MISSING — pwsh \
             7.5.1 is not placed\nverify provision: FAIL — 1 of 2 rows not in place\n",
        ),
    )
    .on(
        CHECK,
        Ran::exit(0, "verify provision: PASS — 2 rows in place\n"),
    );
    sys.on(
        provide,
        Ran::exit(0, "verify provision: PASS — 2 rows in place\n"),
    );
    let item: Box<dyn Item> = Box::new(tools(Some(found_targo())));
    let rows = crate::engine::run(
        &[item],
        Mode::Apply,
        &sys,
        &ctx(std::slice::from_ref(&alice)),
        &mut NoJournal,
    );
    assert_eq!(rows[0].outcome, Outcome::Changed, "{rows:?}");
    assert_eq!(
        rows[0].from.as_deref(),
        Some("FAIL — 1 of 2 rows not in place (MISSING — pwsh 7.5.1 is not placed)")
    );
    let ran: Vec<String> = sys
        .calls()
        .into_iter()
        .filter(|call| call.contains("targo"))
        .collect();
    assert_eq!(ran, [CHECK, provide, CHECK]);
    let cmd = tools(Some(found_targo())).command(Path::new("/Users//alice/.atpkg/bin/targo"), true);
    assert_eq!(cmd.cwd.as_deref(), Some(Path::new("/Users//alice/aterm")));
    assert_eq!(
        cmd.env,
        [(
            "PATH".to_owned(),
            "/Users//alice/.atpkg/bin:/usr/bin:/bin:/usr/sbin:/sbin".to_owned()
        )]
    );

    let (sys, alice) = alice_with(None);
    script_local(&sys, &alice, ORIGIN, true, "");
    sys.on(
        CHECK,
        Ran::exit(
            3,
            "verify provision: COULD NOT RUN — none of the rows asked for is pinned for \
             linux-riscv64\n",
        ),
    );
    let probe = tools(Some(found_targo())).probe(&sys, &ctx(&[user("alice", 501)]));
    assert!(matches!(probe, Probe::NotApplicable { .. }), "{probe:?}");

    let (sys, alice) = alice_with(None);
    script_local(&sys, &alice, ORIGIN, true, "");
    let missing: Targo = Arc::new(|| {
        Err(
            "the toolset's targo is not installed: `aterm pkg install --default-set` installs it"
                .into(),
        )
    });
    let probe = tools(Some(missing)).probe(&sys, &ctx(&[user("alice", 501)]));
    assert!(
        matches!(&probe, Probe::Blocked { why } if why.contains("aterm pkg install --default-set")),
        "{probe:?}"
    );
    assert!(sys.calls().iter().all(|call| !call.contains("targo")));
}

/// INIT RUNS NO CODE FROM A TREE IT LEAVES AS IT IS: a checkout with uncommitted changes or
/// untracked files, a detached HEAD, or an origin that is not admitted reads not applicable,
/// saying why, in a read and in an apply, and its own `targo` is never run (no build, no
/// lock rewrite, nothing of the tree executed). A tree whose `Cargo.lock` cargo would have
/// to rewrite reads unusable, naming the lock: `--locked` refused it. Negative control: the
/// same checkout clean, on its branch, with its admitted origin, runs `--check`.
#[test]
fn workspace_tools_run_no_code_from_a_tree_init_leaves_as_it_is_and_never_rewrite_its_lock() {
    for (case, url, branch, status, why) in [
        (
            "dirty",
            ORIGIN,
            true,
            " M Cargo.toml\0",
            "uncommitted changes",
        ),
        ("untracked", ORIGIN, true, "?? new.rs\0", "untracked files"),
        ("detached", ORIGIN, false, "", "HEAD is detached"),
        (
            "foreign origin",
            "/elsewhere/fork.git",
            true,
            "",
            "its origin is not",
        ),
    ] {
        for mode in [Mode::Plan, Mode::Apply] {
            let (sys, alice) = alice_with(None);
            script_local(&sys, &alice, url, branch, status);
            sys.on(CHECK, Ran::exit(1, "verify provision: FAIL — 1 row\n"));
            let item: Box<dyn Item> = Box::new(tools(Some(found_targo())));
            let rows = crate::engine::run(
                &[item],
                mode,
                &sys,
                &ctx(std::slice::from_ref(&alice)),
                &mut NoJournal,
            );
            assert_eq!(rows[0].outcome, Outcome::NotApplicable, "{case}: {rows:?}");
            let said = rows[0].why.as_deref().unwrap_or_default();
            assert!(
                said.contains("~/aterm is left as it is") && said.contains(why),
                "{case}: {said}"
            );
            assert!(
                sys.calls().iter().all(|call| !call.contains("targo")),
                "{case}: {:?}",
                sys.calls()
            );
        }
    }

    let (sys, alice) = alice_with(None);
    script_local(&sys, &alice, ORIGIN, true, "");
    sys.on(
        CHECK,
        Ran::exit(
            101,
            "error: cannot update the lock file /Users//alice/aterm/Cargo.lock because \
             --locked was passed to prevent this\n",
        ),
    );
    let probe = tools(Some(found_targo())).probe(&sys, &ctx(std::slice::from_ref(&alice)));
    assert_eq!(
        probe,
        Probe::Unusable {
            why: STALE_LOCK.into()
        }
    );

    let (sys, alice) = alice_with(None);
    script_local(&sys, &alice, ORIGIN, true, "");
    sys.on(CHECK, Ran::exit(0, "verify provision: PASS — 2 rows\n"));
    let probe = tools(Some(found_targo())).probe(&sys, &ctx(std::slice::from_ref(&alice)));
    assert!(matches!(probe, Probe::Ok { .. }), "{probe:?}");
    assert_eq!(
        sys.calls()
            .iter()
            .filter(|call| call.as_str() == CHECK)
            .count(),
        1
    );
}

/// A stand-in for the checkout and submodule items, with a fixed id and the outcome its
/// probe and apply give.
struct Standing {
    id: &'static str,
    probe: Probe,
    apply: Result<(), String>,
}

impl Item for Standing {
    fn id(&self) -> String {
        self.id.to_owned()
    }
    fn family(&self) -> &'static str {
        "checkout"
    }
    fn title(&self) -> String {
        self.id.to_owned()
    }
    fn probe(&self, _sys: &dyn System, _ctx: &Ctx) -> Probe {
        self.probe.clone()
    }
    fn apply(&self, _sys: &dyn System, _ctx: &Ctx) -> Result<(), String> {
        self.apply.clone()
    }
}

/// THE TOOLS WAIT FOR THE SUBMODULE: the workspace does not load without `vendor/astream`,
/// so when `develop.submodules@~/aterm` did not land, `develop.workspace-tools@~/aterm` waits
/// for it and runs nothing. Negative control: with the submodule in place, the tools are
/// read with `--check`.
#[test]
fn workspace_tools_wait_for_the_submodule() {
    for landed in [false, true] {
        let (sys, alice) = alice_with(None);
        script_local(&sys, &alice, ORIGIN, true, "");
        sys.on(CHECK, Ran::exit(0, "verify provision: PASS — 2 rows\n"));
        let items: Vec<Box<dyn Item>> = vec![
            Box::new(Standing {
                id: "develop.checkout@~/aterm",
                probe: Probe::Ok {
                    now: "up to date".into(),
                },
                apply: Ok(()),
            }),
            Box::new(Standing {
                id: "develop.submodules@~/aterm",
                probe: if landed {
                    Probe::Ok {
                        now: "checked out".into(),
                    }
                } else {
                    Probe::Change {
                        from: "vendor/astream is not checked out".into(),
                        to: "checked out".into(),
                    }
                },
                apply: Err("git submodule update failed".into()),
            }),
            Box::new(tools(Some(found_targo()))),
        ];
        let rows = crate::engine::run(
            &items,
            Mode::Apply,
            &sys,
            &ctx(std::slice::from_ref(&alice)),
            &mut NoJournal,
        );
        let ran = sys
            .calls()
            .iter()
            .filter(|call| call.contains("targo"))
            .count();
        if landed {
            assert_eq!(rows[2].outcome, Outcome::Ok, "{rows:?}");
            assert_eq!(ran, 1);
        } else {
            assert_eq!(rows[1].outcome, Outcome::Failed, "{rows:?}");
            assert_eq!(rows[2].outcome, Outcome::Blocked, "{rows:?}");
            assert_eq!(
                rows[2].why.as_deref(),
                Some("waits for develop.submodules@~/aterm")
            );
            assert_eq!(ran, 0, "{:?}", sys.calls());
        }
    }
}

/// THE DEVELOP ITEMS, END TO END on real git: `--develop` run from below a checkout's root
/// records it as `~/work`, fast-forwards it (it was clean and behind), finds no submodule
/// to check out, and blocks the tools on a missing targo; no run rewrites anything. A second
/// pass changes nothing.
#[test]
fn develop_records_and_fast_forwards_a_real_checkout_without_a_rewrite() {
    let live = Live::new();
    let repos = Repos::new(&live, "develop-items");
    let tip = repos.advance("two.txt", "two\n");
    let sys = Spawns::new(&live);
    let ctx = ctx(std::slice::from_ref(&repos.who));
    let pass = |want| {
        let items = compose(
            &sys,
            &ctx,
            &Inputs {
                want,
                asked: false,
                cwd: Some(&repos.work.join("crates/deep")),
                targo: None,
            },
        )
        .unwrap();
        let bound = crate::engine::bind(
            items
                .iter()
                .map(|(item, place)| (item.as_ref(), place.clone())),
            &sys,
            &ctx,
        );
        aterm_converge::run_scoped(
            &bound,
            Mode::Apply,
            Scope::EVERY,
            &mut NoJournal,
            &mut |_| {},
        )
        .unwrap()
    };
    let rows = pass(Some(true));
    let outcomes: Vec<(String, Outcome)> = rows
        .iter()
        .map(|row| (row.id.clone(), row.outcome))
        .collect();
    assert_eq!(
        outcomes,
        [
            ("develop.record".to_owned(), Outcome::Changed),
            ("develop.checkout@~/work".to_owned(), Outcome::Changed),
            ("develop.submodules@~/work".to_owned(), Outcome::Ok),
            (
                "develop.workspace-tools@~/work".to_owned(),
                Outcome::Blocked
            ),
        ],
        "{rows:?}"
    );
    assert!(rows.iter().all(|row| row.layer == Some(Layer::Develop)));
    assert_eq!(repos.head(&repos.work), tip);
    assert_eq!(
        std::fs::read_to_string(Path::new(&repos.who.home).join(PROFILE)).unwrap(),
        "[develop]\ncheckouts = [\"~/work\"]\n"
    );
    let rows = pass(None);
    assert_eq!(rows[1].outcome, Outcome::Ok, "{rows:?}");
    assert_eq!(
        rows[1].to.as_deref(),
        Some(format!("up to date with origin/main at {}", &tip[..12]).as_str())
    );
    let record = sys.record();
    assert_eq!(rewrites(&record), Vec::<String>::new());
    assert_eq!(
        subcommands(&record)
            .iter()
            .filter(|sub| *sub == "fetch")
            .count(),
        1
    );
}

/// A checkout whose tree records `vendor/astream`: a scratch astream remote and a seed clone
/// of it, the superproject seed with the submodule added, and `~/fresh`, a clone with the
/// submodule not yet initialized.
struct Bus<'a> {
    repos: Repos<'a>,
    astream: PathBuf,
    astream_seed: PathBuf,
    work: PathBuf,
}

impl<'a> Bus<'a> {
    fn new(live: &'a Live, label: &str) -> Self {
        let repos = Repos::new(live, label);
        let home = PathBuf::from(&repos.who.home);
        let astream = home.join("astream.git");
        let astream_seed = home.join("astream-seed");
        repos.init(&astream, true);
        repos.init(&astream_seed, false);
        repos.g(
            &astream_seed,
            &["remote", "add", "origin", &astream.to_string_lossy()],
        );
        let bus = Self {
            work: home.join("fresh"),
            repos,
            astream,
            astream_seed,
        };
        bus.astream_commit("lib.rs", "one\n");
        bus.repos.g(
            &bus.repos.seed,
            &[
                "submodule",
                "add",
                "--quiet",
                &bus.astream.to_string_lossy(),
                SUBMODULE,
            ],
        );
        bus.repos
            .g(&bus.repos.seed, &["commit", "--quiet", "-m", "astream"]);
        bus.repos
            .g(&bus.repos.seed, &["push", "--quiet", "origin", "main"]);
        bus.repos.clone_into(&bus.repos.origin, &bus.work);
        bus
    }

    /// A new commit on astream's `main`, pushed: its id.
    fn astream_commit(&self, file: &str, text: &str) -> String {
        std::fs::write(self.astream_seed.join(file), text).unwrap();
        self.repos.g(&self.astream_seed, &["add", "--all"]);
        self.repos
            .g(&self.astream_seed, &["commit", "--quiet", "-m", file]);
        self.repos
            .g(&self.astream_seed, &["push", "--quiet", "origin", "HEAD"]);
        self.repos.head(&self.astream_seed)
    }

    /// The superproject's `main` records astream at `commit` (pushed to astream), pushed.
    fn bump(&self, commit: &str) {
        let inner = self.repos.seed.join(SUBMODULE);
        self.repos.g(&inner, &["fetch", "--quiet", "origin"]);
        self.repos
            .g(&inner, &["checkout", "--quiet", "--detach", commit]);
        self.repos.g(&self.repos.seed, &["add", SUBMODULE]);
        self.repos
            .g(&self.repos.seed, &["commit", "--quiet", "-m", "bump"]);
        self.repos
            .g(&self.repos.seed, &["push", "--quiet", "origin", "main"]);
    }

    /// `~/fresh` fast-forwarded to origin's `main` the way `develop.checkout@` does it: the
    /// submodule's worktree is not touched, so it lags the new gitlink.
    fn fast_forward(&self) {
        self.repos.g(&self.work, &["fetch", "--quiet", "origin"]);
        self.repos.g(
            &self.work,
            &[
                "-c",
                "submodule.recurse=false",
                "merge",
                "--ff-only",
                "--quiet",
                "origin/main",
            ],
        );
    }

    fn inner(&self) -> PathBuf {
        self.work.join(SUBMODULE)
    }

    fn item(&self) -> Submodules {
        Submodules {
            user: self.repos.who.clone(),
            path: self.work.clone(),
            spelled: "~/fresh".into(),
            stood: Aside::default(),
        }
    }

    fn ctx(&self) -> Ctx {
        ctx(std::slice::from_ref(&self.repos.who))
    }

    /// The submodule's HEAD, its branch (`None` when detached), and the checkout's status.
    fn state(&self) -> (String, Option<String>, String) {
        let inner = self.inner();
        let branch = Live::new()
            .run(&crate::checkout::git(&self.repos.who, Some(&inner)).args([
                "symbolic-ref",
                "--quiet",
                "--short",
                "HEAD",
            ]))
            .unwrap();
        (
            self.repos.head(&inner),
            branch.success().then(|| branch.out().trim().to_owned()),
            self.repos.status(&self.work),
        )
    }
}

/// Whether `record` ran `git submodule update`.
fn updated(record: &[(Cmd, Option<Vec<u8>>)]) -> bool {
    record
        .iter()
        .any(|(cmd, _)| cmd.args.iter().any(|arg| arg == "update"))
}

/// THE SUBMODULE ITEM on real git: a fresh clone's uninitialized `vendor/astream` is checked
/// out and `submodule.recurse` set; after a fast-forward that bumped the gitlink, the
/// submodule, detached and clean at the commit the tree recorded before, is checked out at
/// the new one (`--checkout`); the same one with an untracked file in it stays where it
/// is, saying why. No run rewrites anything (`submodule update` is the one writer).
#[test]
fn the_submodule_is_checked_out_only_where_that_loses_nothing() {
    let live = Live::new();
    let bus = Bus::new(&live, "develop-submodule");
    let sys = Spawns::new(&live);
    let (item, ctx) = (bus.item(), bus.ctx());
    assert!(
        matches!(item.probe(&sys, &ctx), Probe::Change { .. }),
        "{:?}",
        item.probe(&sys, &ctx)
    );
    item.apply(&sys, &ctx).unwrap();
    assert!(matches!(item.probe(&sys, &ctx), Probe::Ok { .. }));
    assert!(
        bus.repos
            .g(&bus.work, &["submodule", "status", "--", SUBMODULE])
            .starts_with(|c: char| c.is_ascii_hexdigit())
    );
    assert_eq!(
        bus.repos
            .g(&bus.work, &["config", "--get", "submodule.recurse"]),
        "true"
    );
    let first = bus.repos.head(&bus.inner());

    let two = bus.astream_commit("lib.rs", "two\n");
    bus.bump(&two);
    bus.fast_forward();
    assert_eq!(bus.state().0, first);
    std::fs::write(bus.inner().join("scratch.txt"), "mine\n").unwrap();
    match item.probe(&sys, &ctx) {
        Probe::Ok { now } => assert!(
            now.contains("vendor/astream has uncommitted changes"),
            "{now}"
        ),
        other => panic!("{other:?}"),
    }
    assert_eq!(bus.state().0, first);
    std::fs::remove_file(bus.inner().join("scratch.txt")).unwrap();
    assert!(matches!(item.probe(&sys, &ctx), Probe::Change { .. }));
    item.apply(&sys, &ctx).unwrap();
    let (head, branch, status) = bus.state();
    assert_eq!(
        (head.as_str(), branch, status.as_str()),
        (two.as_str(), None, "")
    );
    assert!(matches!(item.probe(&sys, &ctx), Probe::Ok { .. }));
    let record = sys.record();
    assert_eq!(rewrites(&record), Vec::<String>::new());
    assert!(
        record
            .iter()
            .any(|(cmd, _)| cmd.args.iter().any(|arg| arg == "--checkout"))
    );
}

/// A SUBMODULE HEAD GIT CANNOT READ IS NEVER UPDATED: when `git symbolic-ref` fails with
/// anything but "not on a branch" (exit 1), whether the submodule is on a branch of the
/// person's is unknown, so the read fails, naming it, and nothing reaches the update.
/// Negative control: the same script with exit 1 (detached) goes on to read the commit.
#[test]
fn a_submodule_head_git_cannot_read_is_never_updated() {
    for (code, want) in [
        (128, "cannot read vendor/astream's HEAD"),
        (1, "cannot run git rev-parse in vendor/astream"),
    ] {
        let (sys, alice) = alice_with(None);
        let path = Path::new("/Users//alice/aterm");
        let git = checkout::git(&alice, Some(&path.join(SUBMODULE)));
        let status = ["status", "--porcelain=v1", "-z", "--untracked-files=normal"];
        sys.on(&git.clone().args(status).display(), Ran::ok(""));
        let branch = ["symbolic-ref", "--quiet", "--short", "HEAD"];
        sys.on(&git.clone().args(branch).display(), Ran::exit(code, ""));
        let item = Submodules {
            user: alice.clone(),
            path: path.to_path_buf(),
            spelled: "~/aterm".into(),
            stood: Aside::default(),
        };
        assert_eq!(item.moved_safely(&sys), Err(want.to_owned()), "exit {code}");
    }
}

/// A PERSON'S ASTREAM WORK IS LEFT AS IT IS: astream committed and pushed on its branch
/// before the checkout's bump is committed (AGENTS.md: bus changes land in astream first),
/// the same commit on a detached HEAD, and a topic branch that diverges from the recorded
/// commit under a configured `update = rebase`: each reads `Ok` saying why, and an apply
/// runs no `git submodule update`, so the submodule's HEAD and branch and the checkout's
/// uncommitted bump stay exactly as they were. Negative controls: the update 1299c27cbd ran
/// (`git submodule update --init`, no `--checkout`) on the same states puts the old commit
/// back on a detached HEAD, dropping the bump, and rebases the topic branch into a commit
/// no remote holds.
#[test]
fn a_person_s_astream_branch_and_uncommitted_bump_are_left_as_they_are() {
    let live = Live::new();
    for case in ["branch", "detached", "rebase"] {
        let bus = Bus::new(&live, "develop-bus");
        let (item, ctx) = (bus.item(), bus.ctx());
        let sys = Spawns::new(&live);
        item.apply(&sys, &ctx).unwrap();
        let recorded = bus.repos.head(&bus.inner());
        let inner = bus.inner();
        let why = match case {
            "branch" | "detached" => {
                bus.repos.g(&inner, &["switch", "--quiet", "main"]);
                std::fs::write(inner.join("bus.rs"), "new\n").unwrap();
                bus.repos.g(&inner, &["add", "--all"]);
                bus.repos.g(&inner, &["commit", "--quiet", "-m", "bus"]);
                bus.repos.g(&inner, &["push", "--quiet", "origin", "main"]);
                if case == "detached" {
                    bus.repos.g(&inner, &["checkout", "--quiet", "--detach"]);
                    "which no commit of ~/fresh records"
                } else {
                    "on its branch main"
                }
            }
            _ => {
                let base = bus.repos.head(&bus.astream_seed);
                let two = bus.astream_commit("lib.rs", "two\n");
                bus.bump(&two);
                bus.fast_forward();
                bus.repos.g(&inner, &["fetch", "--quiet", "origin"]);
                bus.repos
                    .g(&inner, &["switch", "--quiet", "-c", "topic", &base]);
                std::fs::write(inner.join("topic.rs"), "topic\n").unwrap();
                bus.repos.g(&inner, &["add", "--all"]);
                bus.repos.g(&inner, &["commit", "--quiet", "-m", "topic"]);
                bus.repos.g(&inner, &["push", "--quiet", "origin", "topic"]);
                bus.repos.g(
                    &bus.work,
                    &["config", "submodule.vendor/astream.update", "rebase"],
                );
                "on its branch topic"
            }
        };
        let before = bus.state();
        assert_ne!(before.0, recorded, "{case}");
        if case != "rebase" {
            assert_eq!(before.2, "M vendor/astream", "{case}");
        }
        let sys = Spawns::new(&live);
        let rows = crate::engine::run(
            &[Box::new(bus.item()) as Box<dyn Item>],
            Mode::Apply,
            &sys,
            &ctx,
            &mut NoJournal,
        );
        assert_eq!(rows[0].outcome, Outcome::Ok, "{case}: {rows:?}");
        assert!(
            rows[0].to.as_deref().is_some_and(|now| now.contains(why)),
            "{case}: {rows:?}"
        );
        assert!(!updated(&sys.record()), "{case}: {:?}", sys.record());
        assert_eq!(bus.state(), before, "{case}");

        bus.repos.g(
            &bus.work,
            &["submodule", "update", "--init", "--", SUBMODULE],
        );
        let after = bus.state();
        if case == "rebase" {
            assert_eq!(after.1.as_deref(), Some("topic"));
            assert_ne!(after.0, before.0, "the topic branch was rebased");
            assert_eq!(
                bus.repos.g(
                    &inner,
                    &["rev-list", "--count", "topic", "--not", "--remotes"]
                ),
                "1"
            );
        } else {
            assert_eq!(after, (recorded.clone(), None, String::new()), "{case}");
        }
    }
}

/// `--checkout` WHATEVER THE CONFIGURED MODE: a lagging submodule (detached, clean, at a
/// commit the tree recorded before) whose recorded commit does not contain it is checked
/// out at that commit even under `submodule.vendor/astream.update = merge`, making no
/// commit. Negative control: the same update without `--checkout` merges, leaving a commit
/// no remote holds.
#[test]
fn a_configured_update_mode_never_makes_a_commit_in_the_submodule() {
    let live = Live::new();
    let mut heads = Vec::new();
    for checkout in [true, false] {
        let bus = Bus::new(&live, "develop-update-mode");
        let (item, ctx) = (bus.item(), bus.ctx());
        let sys = Spawns::new(&live);
        item.apply(&sys, &ctx).unwrap();
        let one = bus.repos.head(&bus.astream_seed);
        let a = bus.astream_commit("a.rs", "a\n");
        bus.bump(&a);
        bus.fast_forward();
        item.apply(&sys, &ctx).unwrap();
        assert_eq!(bus.state().0, a);
        bus.repos.g(
            &bus.astream_seed,
            &["switch", "--quiet", "-c", "other", &one],
        );
        let b = bus.astream_commit("b.rs", "b\n");
        bus.bump(&b);
        bus.fast_forward();
        bus.repos.g(
            &bus.work,
            &["config", "submodule.vendor/astream.update", "merge"],
        );
        if checkout {
            assert!(matches!(item.probe(&sys, &ctx), Probe::Change { .. }));
            item.apply(&sys, &ctx).unwrap();
            assert_eq!(bus.state(), (b.clone(), None, String::new()));
            heads.push(b);
        } else {
            bus.repos.g(
                &bus.work,
                &["submodule", "update", "--init", "--", SUBMODULE],
            );
            let head = bus.state().0;
            assert_ne!(head, b, "the configured merge made a commit");
            assert_eq!(
                bus.repos.g(
                    &bus.inner(),
                    &["rev-list", "--count", "HEAD", "--not", "--remotes"]
                ),
                "1"
            );
            heads.push(head);
        }
        assert_eq!(rewrites(&sys.record()), Vec::<String>::new());
    }
    assert_ne!(heads[0], heads[1]);
}

/// A CONFLICTED SUBMODULE IS NEVER UPDATED: a merge in the checkout that left
/// `vendor/astream` conflicted (`U`) reads saying so; an apply sets only
/// `submodule.recurse`, runs no `git submodule update`, and the conflict is as it was.
/// Negative control: before the conflict, the same item on the same checkout would update
/// (its uninitialized submodule reads as a change to check out).
#[test]
fn a_conflicted_submodule_is_never_updated() {
    let live = Live::new();
    let bus = Bus::new(&live, "develop-conflict");
    let (item, ctx) = (bus.item(), bus.ctx());
    let sys = Spawns::new(&live);
    match item.probe(&sys, &ctx) {
        Probe::Change { from, .. } => assert!(from.contains("not at the commit"), "{from}"),
        other => panic!("{other:?}"),
    }
    let one = bus.repos.head(&bus.astream_seed);
    let p = bus.astream_commit("p.rs", "p\n");
    bus.repos.g(
        &bus.astream_seed,
        &["switch", "--quiet", "-c", "other", &one],
    );
    let q = bus.astream_commit("q.rs", "q\n");
    bus.repos
        .g(&bus.work, &["submodule", "update", "--init", "--quiet"]);
    let inner = bus.inner();
    bus.repos.g(&inner, &["fetch", "--quiet", "origin"]);
    bus.repos.g(&bus.work, &["switch", "--quiet", "-c", "side"]);
    bus.repos
        .g(&inner, &["checkout", "--quiet", "--detach", &p]);
    bus.repos
        .g(&bus.work, &["commit", "--quiet", "--all", "-m", "p"]);
    bus.repos.g(&bus.work, &["switch", "--quiet", "main"]);
    bus.repos
        .g(&inner, &["checkout", "--quiet", "--detach", &q]);
    bus.repos
        .g(&bus.work, &["commit", "--quiet", "--all", "-m", "q"]);
    let merged = live
        .run(&crate::checkout::git(&bus.repos.who, Some(&bus.work)).args(["merge", "side"]))
        .unwrap();
    assert!(!merged.success(), "{}", merged.describe());
    let status = bus
        .repos
        .g(&bus.work, &["submodule", "status", "--", SUBMODULE]);
    assert!(status.starts_with('U'), "{status}");
    let before = bus.state();
    let sys = Spawns::new(&live);
    let rows = crate::engine::run(
        &[Box::new(bus.item()) as Box<dyn Item>],
        Mode::Apply,
        &sys,
        &ctx,
        &mut NoJournal,
    );
    assert_eq!(rows[0].outcome, Outcome::Changed, "{rows:?}");
    assert_eq!(
        rows[0].to.as_deref(),
        Some("`git config submodule.recurse true`"),
        "{rows:?}"
    );
    assert!(
        rows[0]
            .from
            .as_deref()
            .is_some_and(|from| from.contains("merge conflict")),
        "{rows:?}"
    );
    match item.probe(&sys, &ctx) {
        Probe::Ok { now } => assert!(now.contains("merge conflict"), "{now}"),
        other => panic!("{other:?}"),
    }
    assert!(!updated(&sys.record()), "{:?}", sys.record());
    assert_eq!(bus.state(), before);
    assert_eq!(
        bus.repos
            .g(&bus.work, &["config", "--get", "submodule.recurse"]),
        "true"
    );
}

/// A LINKED WORKTREE IS NEVER RECORDED: `--develop` run inside a `git worktree add` of a
/// checkout (removed once its work lands) is refused, naming the checkout it belongs to,
/// and nothing is written; the layer is then one `develop.record` row saying so. Negative
/// control: run in that checkout itself, it records it.
#[test]
fn develop_never_records_a_linked_worktree() {
    let live = Live::new();
    let repos = Repos::new(&live, "develop-linked");
    let linked = PathBuf::from(&repos.who.home).join("linked");
    repos.g(
        &repos.work,
        &[
            "worktree",
            "add",
            "--quiet",
            "-b",
            "topic",
            &linked.to_string_lossy(),
        ],
    );
    let sys = Spawns::new(&live);
    let ctx = ctx(std::slice::from_ref(&repos.who));
    let inputs = |cwd| Inputs {
        want: Some(true),
        asked: false,
        cwd: Some(cwd),
        targo: None,
    };
    let refused = compose(&sys, &ctx, &inputs(&linked)).err().unwrap();
    assert!(
        refused.contains("~/linked is a linked worktree of ~/work")
            && refused.contains("run it in ~/work"),
        "{refused}"
    );
    let layer = layer(&sys, &ctx, &inputs(&linked));
    assert!(layer.unreadable);
    assert_eq!(layer.items.len(), 1);
    assert_eq!(layer.items[0].0.id(), RECORD);
    assert!(!Path::new(&repos.who.home).join(PROFILE).exists());

    let items = compose(&sys, &ctx, &inputs(&repos.work)).unwrap();
    let ids: Vec<String> = items.iter().map(|(item, _)| item.id()).collect();
    assert_eq!(ids[1], "develop.checkout@~/work");
}

/// `--no-develop`'s RECORD ROW NAMES `--no-develop`: removing the table is remedied by the
/// flag that removes it, never by `--develop`, which would record the checkout again.
/// Negative control: `--develop`'s record row names `--develop`.
#[test]
fn no_develop_names_its_own_remedy() {
    for (want, profile, remedy) in [
        (
            Some(false),
            Some("[develop]\ncheckouts = [\"~/aterm\"]\n"),
            Remedy::Command("aterm init --no-develop".into()),
        ),
        (Some(true), None, Remedy::Develop),
    ] {
        let (sys, alice) = alice_with(profile);
        let ctx = ctx(std::slice::from_ref(&alice));
        let items = compose(
            &sys,
            &ctx,
            &Inputs {
                want,
                asked: false,
                cwd: Some(Path::new("/Users//alice/aterm")),
                targo: None,
            },
        )
        .unwrap();
        let (record, place) = &items[0];
        let bound = crate::engine::bind([(record.as_ref(), place.clone())], &sys, &ctx);
        let rows = aterm_converge::run_scoped(
            &bound,
            Mode::Plan,
            Scope::EVERY,
            &mut NoJournal,
            &mut |_| {},
        )
        .unwrap();
        assert_eq!(rows[0].outcome, Outcome::Planned, "{rows:?}");
        assert_eq!(
            rows[0].explain.as_ref().map(|explain| &explain.remedy),
            Some(&remedy),
            "{want:?}"
        );
    }
}

/// `--only`'s develop words: ids with or without their `@<path>`, the families, the layer.
#[test]
fn only_names_develop_items_by_id_family_and_layer() {
    for word in [
        "develop",
        "develop.record",
        "develop.checkout",
        "develop.checkout@~/aterm",
        "develop.submodules@/Volumes/x",
        "develop.workspace-tools",
        "profile",
        "checkout",
        "tooling",
    ] {
        assert!(names(word), "{word}");
    }
    for word in [
        "base",
        "develop.check",
        "workstation",
        "keyboard",
        "develop.records",
    ] {
        assert!(!names(word), "{word}");
    }
}
