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

/// `--develop` and `--no-develop` preserve every line outside the `[develop]` table byte for byte.
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

/// `--develop` records the checkout it runs in, and `--no-develop` restores the file.
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

/// The develop layer composes items only when `[develop]` records a checkout, and refuses `--develop` outside one.
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
        reader: Reader::Init,
    }
}

fn found_targo() -> Targo {
    Arc::new(|| Ok(PathBuf::from("/Users//alice/.atpkg/bin/targo")))
}

/// [`WorkspaceTools`] runs the checkout's own `targo --unverified --locked verify provision` with the toolset's `bin` first on `PATH`.
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

/// [`WorkspaceTools`] runs no code from a dirty, detached, or foreign-origin tree and refuses a stale `Cargo.lock`.
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

/// `develop.workspace-tools` waits for `develop.submodules` when the submodule has not landed.
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

/// End-to-end `--develop` records and fast-forwards a real git checkout without rewriting history.
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
            reader: Reader::Init,
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

/// [`Submodules`] checks out an uninitialized or clean lagging `vendor/astream` with `--checkout` and leaves a dirty one untouched.
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

/// A submodule whose `HEAD` git cannot read fails closed before any update.
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
            reader: Reader::Init,
        };
        assert_eq!(item.moved_safely(&sys), Err(want.to_owned()), "exit {code}");
    }
}

/// [`Submodules`] leaves a person's `astream` branch and uncommitted gitlink bump untouched.
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

/// [`Submodules`] passes `--checkout` regardless of `submodule.vendor/astream.update`, never creating a merge commit.
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

/// A conflicted `vendor/astream` submodule is never updated by `git submodule update`.
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

/// `--develop` refuses a linked worktree (`git worktree add`) and names the checkout it belongs to.
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

/// `--no-develop`'s record row names `aterm init --no-develop` as its remedy.
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

/// `targo --unverified --locked --offline verify provision --check`: doctor's read of the
/// tools, run as alice in `~/aterm` with the toolset's `bin` first on `PATH`.
const DOCTOR_CHECK: &str = "cd /Users//alice/aterm && alice$ \
                            PATH=/Users//alice/.atpkg/bin:/usr/bin:/bin:/usr/sbin:/sbin \
                            /Users//alice/.atpkg/bin/targo --unverified --locked --offline verify \
                            provision --check";

/// The runs among `calls` that reach past this Mac, or run a checkout's code other than
/// doctor's own offline `--check`: a git `fetch`, `pull`, `clone` or `push`, an `ls-remote`
/// that asks origin (`--get-url` reads only the configuration), a `submodule update`, and any
/// `targo` run but `--offline … --check`.
fn reaches(calls: &[String]) -> Vec<String> {
    calls
        .iter()
        .filter(|call| {
            let words: Vec<&str> = call.split_whitespace().collect();
            if words.iter().any(|word| word.ends_with("/targo")) {
                return !(words.contains(&"--offline") && words.contains(&"--check"));
            }
            let Some(at) = words.iter().position(|word| word.ends_with("/git")) else {
                return false;
            };
            let mut rest = words[at + 1..].iter();
            let sub = loop {
                match rest.next() {
                    Some(&("-C" | "-c")) => {
                        rest.next();
                    }
                    other => break other.copied(),
                }
            };
            match sub {
                Some("fetch" | "pull" | "clone" | "push") => true,
                Some("ls-remote") => !words.contains(&"--get-url"),
                Some("submodule") => words.contains(&"update"),
                _ => false,
            }
        })
        .cloned()
        .collect()
}

/// Scripts what doctor reads of `~/aterm` past [`script_local`]: origin's default branch as
/// last fetched (`main`, at `tip`), and a submodule at the commit the tree records with
/// `submodule.recurse` true.
fn script_fetched(sys: &Fake, alice: &User, tip: &str) {
    let path = Path::new("/Users//alice/aterm");
    for (args, ran) in [
        (
            vec!["symbolic-ref", "--quiet", "refs/remotes/origin/HEAD"],
            Ran::ok("refs/remotes/origin/main\n"),
        ),
        (
            vec![
                "rev-parse",
                "--verify",
                "--quiet",
                "refs/remotes/origin/main^{commit}",
            ],
            Ran::ok(&format!("{tip}\n")),
        ),
        (
            vec!["config", "--get", "submodule.recurse"],
            Ran::ok("true\n"),
        ),
        (
            vec!["submodule", "status", "--", SUBMODULE],
            Ran::ok(" 2222222222222222222222222222222222222222 vendor/astream (heads/main)\n"),
        ),
    ] {
        sys.on(&checkout::git(alice, Some(path)).args(args).display(), ran);
    }
}

/// The commit [`script_local`]'s `~/aterm` is at.
const HEAD: &str = "1111111111111111111111111111111111111111";

/// DOCTOR READS THE FOUR DEVELOP ROWS FROM THIS MAC ALONE (design §5, S9): on a Mac whose
/// `init.toml` records `~/aterm`, doctor's layer is the record, the checkout, the submodule and
/// the tools, each read: the checkout against origin as last fetched, the tools by the
/// checkout's own `--check` under `--locked --offline`. No run reaches origin (no fetch, no
/// `ls-remote` to it), none updates the submodule, nothing is written, and no item is applied.
/// Negative control: init's reader of the same Mac (what `aterm init --check` runs) asks origin
/// with `ls-remote` and runs the tools without `--offline`, and the same predicate catches both.
#[test]
fn doctor_reads_the_four_develop_rows_from_this_mac_alone() {
    let recorded = "[develop]\ncheckouts = [\"~/aterm\"]\n";
    let scripted = || {
        let (sys, alice) = alice_with(Some(recorded));
        script_local(&sys, &alice, ORIGIN, true, "");
        script_fetched(&sys, &alice, HEAD);
        sys.on(
            DOCTOR_CHECK,
            Ran::exit(0, "verify provision: PASS — 7 rows in place\n"),
        );
        (sys, alice)
    };
    let (sys, alice) = scripted();
    let ctx = ctx(std::slice::from_ref(&alice));
    let setup = sys.calls().len();
    let composed = doctor(&sys, &ctx, false, Some(found_targo()));
    assert!(!composed.unreadable);
    let rows = read(&sys, &ctx, &composed, &[]).unwrap();
    let read_as: Vec<(&str, Outcome, Option<&str>)> = rows
        .iter()
        .map(|row| (row.id.as_str(), row.outcome, row.to.as_deref()))
        .collect();
    assert_eq!(
        read_as,
        [
            (
                "develop.record",
                Outcome::Ok,
                Some("the develop profile, for ~/aterm")
            ),
            (
                "develop.checkout@~/aterm",
                Outcome::Ok,
                Some("up to date with origin/main at 111111111111, as last fetched")
            ),
            (
                "develop.submodules@~/aterm",
                Outcome::Ok,
                Some("vendor/astream at the commit the tree records; submodule.recurse true")
            ),
            (
                "develop.workspace-tools@~/aterm",
                Outcome::Ok,
                Some("PASS — 7 rows in place")
            ),
        ],
        "{rows:?}"
    );
    assert!(rows.iter().all(|row| row.layer == Some(Layer::Develop)));
    let calls = sys.calls().split_off(setup);
    assert_eq!(reaches(&calls), Vec::<String>::new(), "{calls:#?}");
    assert_eq!(
        calls
            .iter()
            .filter(|call| call.as_str() == DOCTOR_CHECK)
            .count(),
        1,
        "{calls:#?}"
    );
    assert!(
        calls.iter().all(|call| !call.contains("$ write")),
        "{calls:#?}"
    );

    let (sys, alice) = scripted();
    let ctx = crate::items::defaults::tests::ctx(std::slice::from_ref(&alice));
    let init = compose(
        &sys,
        &ctx,
        &Inputs {
            want: None,
            asked: false,
            cwd: None,
            targo: Some(found_targo()),
        },
    )
    .unwrap();
    let bound = crate::engine::bind(
        init.iter()
            .map(|(item, place)| (item.as_ref(), place.clone())),
        &sys,
        &ctx,
    );
    aterm_converge::run_scoped(
        &bound,
        Mode::Plan,
        Scope::EVERY,
        &mut NoJournal,
        &mut |_| {},
    )
    .unwrap();
    let caught = reaches(&sys.calls());
    assert!(
        caught
            .iter()
            .any(|call| call.ends_with("ls-remote --symref origin HEAD")),
        "{caught:#?}"
    );
}

/// DOCTOR APPLIES NO DEVELOP ITEM, even under an engine that would: each of doctor's items
/// refuses its apply, so a real checkout behind its origin is never fetched or merged (the
/// submodule's and the tools' refusals are pinned by
/// `every_doctor_develop_item_refuses_its_apply_and_runs_nothing`).
/// Negative control: a plan of the same items (what doctor runs) reads them as planned and runs
/// nothing more.
#[test]
fn doctor_s_develop_items_apply_nothing_whatever_the_engine_asks() {
    let live = Live::new();
    let repos = Repos::new(&live, "doctor-no-apply");
    repos.advance("two.txt", "two\n");
    repos.g(&repos.work, &["fetch", "--quiet", "origin"]);
    let before = repos.head(&repos.work);
    let profile = Path::new(&repos.who.home).join(PROFILE);
    std::fs::create_dir_all(profile.parent().unwrap()).unwrap();
    std::fs::write(&profile, "[develop]\ncheckouts = [\"~/work\"]\n").unwrap();
    let ctx = ctx(std::slice::from_ref(&repos.who));
    for mode in [Mode::Apply, Mode::Plan] {
        let sys = Spawns::new(&live);
        let composed = doctor(&sys, &ctx, false, None);
        let bound = crate::engine::bind(
            composed
                .items
                .iter()
                .map(|(item, place)| (item.as_ref(), place.clone())),
            &sys,
            &ctx,
        );
        let rows =
            aterm_converge::run_scoped(&bound, mode, Scope::EVERY, &mut NoJournal, &mut |_| {})
                .unwrap();
        let checkout = &rows[1];
        assert_eq!(checkout.id, "develop.checkout@~/work");
        if mode == Mode::Apply {
            assert_eq!(checkout.outcome, Outcome::Failed, "{rows:?}");
            assert_eq!(checkout.why.as_deref(), Some(DOCTOR_CHANGES_NOTHING));
        } else {
            assert_eq!(checkout.outcome, Outcome::Planned, "{rows:?}");
        }
        let record = sys.record();
        let calls: Vec<String> = record.iter().map(|(cmd, _)| cmd.display()).collect();
        assert_eq!(
            reaches(&calls),
            Vec::<String>::new(),
            "{mode:?}: {calls:#?}"
        );
        assert_eq!(rewrites(&record), Vec::<String>::new());
        assert_eq!(repos.head(&repos.work), before, "{mode:?}");
    }
}

/// DOCTOR READS A REAL CHECKOUT WITHOUT ITS ORIGIN: a clean checkout behind origin's tip that
/// it has not fetched reads up to date as last fetched; once fetched, one commit behind as last
/// fetched (planned, `aterm init --develop` the remedy); and with origin's repository gone
/// altogether doctor reads it exactly so, while the tools' `--check` runs (a stand-in `targo`)
/// under `--offline`. On another branch, clean, the checkout is left as it is and the tools'
/// `--check` still runs, as init's does there (`checkout::kept_here` admits any branch). In a
/// checkout init leaves as it is (an untracked file, a detached HEAD) the tools are not
/// applicable and nothing of the checkout runs. No run fetches, asks origin or rewrites, and the
/// checkout's HEAD never moves. Negative controls: with origin gone, init's own read of the same
/// checkout cannot answer (its `ls-remote` reaches for origin), and init's apply, a mutant
/// doctor that fetched, is caught by the same predicate over the spawn record.
#[test]
fn doctor_reads_a_real_checkout_as_last_fetched_without_its_origin() {
    let live = Live::new();
    let repos = Repos::new(&live, "doctor-develop");
    let tip = repos.advance("two.txt", "two\n");
    let start = repos.head(&repos.work);
    let profile = Path::new(&repos.who.home).join(PROFILE);
    std::fs::create_dir_all(profile.parent().unwrap()).unwrap();
    std::fs::write(&profile, "[develop]\ncheckouts = [\"~/work\"]\n").unwrap();
    let ctx = ctx(std::slice::from_ref(&repos.who));
    let true_targo: Targo = Arc::new(|| Ok(PathBuf::from("/usr/bin/true")));
    let doctor_rows = |sys: &Spawns<'_>| {
        let composed = doctor(sys, &ctx, false, Some(true_targo.clone()));
        read(sys, &ctx, &composed, &[]).unwrap()
    };
    let outcome = |rows: &[ItemReport], id: &str| {
        rows.iter()
            .find(|row| row.id == id)
            .map(|row| (row.outcome, row.from.clone(), row.to.clone()))
            .unwrap_or_else(|| panic!("no {id} in {rows:?}"))
    };
    let sys = Spawns::new(&live);
    let rows = doctor_rows(&sys);
    assert_eq!(
        outcome(&rows, "develop.checkout@~/work"),
        (
            Outcome::Ok,
            None,
            Some(format!(
                "up to date with origin/main at {}, as last fetched",
                &start[..12]
            ))
        ),
        "{rows:?}"
    );

    repos.g(&repos.work, &["fetch", "--quiet", "origin"]);
    let behind = (
        Outcome::Planned,
        Some("1 commit behind origin/main, as last fetched".to_owned()),
        Some(format!("fast-forwarded to origin/main at {}", &tip[..12])),
    );
    let rows = doctor_rows(&sys);
    assert_eq!(
        outcome(&rows, "develop.checkout@~/work"),
        behind,
        "{rows:?}"
    );
    let checkout = rows.iter().find(|row| row.id == "develop.checkout@~/work");
    assert_eq!(
        checkout
            .and_then(|row| row.explain.as_ref())
            .map(|e| &e.remedy),
        Some(&Remedy::Develop)
    );

    std::fs::remove_dir_all(&repos.origin).unwrap();
    let rows = doctor_rows(&sys);
    assert_eq!(
        outcome(&rows, "develop.checkout@~/work"),
        behind,
        "{rows:?}"
    );
    assert_eq!(
        outcome(&rows, "develop.workspace-tools@~/work").0,
        Outcome::Ok,
        "{rows:?}"
    );
    let ran_tools = |record: &[(Cmd, Option<Vec<u8>>)]| -> Vec<String> {
        record
            .iter()
            .map(|(cmd, _)| cmd.display())
            .filter(|call| call.contains("/usr/bin/true"))
            .collect()
    };
    let tools = ran_tools(&sys.record());
    assert!(
        !tools.is_empty()
            && tools
                .iter()
                .all(|call| call
                    .ends_with("--unverified --locked --offline verify provision --check")),
        "{tools:#?}"
    );

    std::fs::write(repos.work.join("mine.txt"), "mine\n").unwrap();
    let kept = Spawns::new(&live);
    let rows = doctor_rows(&kept);
    let (tools_read, ..) = outcome(&rows, "develop.workspace-tools@~/work");
    assert_eq!(tools_read, Outcome::NotApplicable, "{rows:?}");
    assert_eq!(ran_tools(&kept.record()), Vec::<String>::new());
    std::fs::remove_file(repos.work.join("mine.txt")).unwrap();

    repos.g(&repos.work, &["checkout", "--quiet", "-b", "topic"]);
    let topic = Spawns::new(&live);
    let rows = doctor_rows(&topic);
    assert_eq!(
        outcome(&rows, "develop.checkout@~/work"),
        (
            Outcome::Ok,
            None,
            Some("left as it is: it is on topic, not origin's default branch main".to_owned())
        ),
        "{rows:?}"
    );
    assert_eq!(
        outcome(&rows, "develop.workspace-tools@~/work").0,
        Outcome::Ok,
        "{rows:?}"
    );
    assert_eq!(ran_tools(&topic.record()).len(), 1, "{:#?}", topic.record());
    repos.g(&repos.work, &["checkout", "--quiet", "--detach"]);
    let detached = Spawns::new(&live);
    let rows = doctor_rows(&detached);
    assert_eq!(
        outcome(&rows, "develop.workspace-tools@~/work").0,
        Outcome::NotApplicable,
        "{rows:?}"
    );
    assert_eq!(ran_tools(&detached.record()), Vec::<String>::new());
    repos.g(&repos.work, &["checkout", "--quiet", "main"]);
    for record in [topic.record(), detached.record()] {
        let calls: Vec<String> = record.iter().map(|(cmd, _)| cmd.display()).collect();
        assert_eq!(reaches(&calls), Vec::<String>::new(), "{calls:#?}");
        assert_eq!(rewrites(&record), Vec::<String>::new());
    }

    let calls: Vec<String> = sys.record().iter().map(|(cmd, _)| cmd.display()).collect();
    assert_eq!(reaches(&calls), Vec::<String>::new(), "{calls:#?}");
    assert_eq!(rewrites(&sys.record()), Vec::<String>::new());
    assert_eq!(repos.head(&repos.work), start);

    let init_sys = Spawns::new(&live);
    let init_inputs = Inputs {
        want: None,
        asked: false,
        cwd: None,
        targo: None,
    };
    let init = compose(&init_sys, &ctx, &init_inputs).unwrap();
    let bound = crate::engine::bind(
        init.iter()
            .map(|(item, place)| (item.as_ref(), place.clone())),
        &init_sys,
        &ctx,
    );
    let rows = aterm_converge::run_scoped(
        &bound,
        Mode::Plan,
        Scope::EVERY,
        &mut NoJournal,
        &mut |_| {},
    )
    .unwrap();
    assert_eq!(
        outcome(&rows, "develop.checkout@~/work").0,
        Outcome::Unknown,
        "{rows:?}"
    );

    repos.init(&repos.origin, true);
    repos.g(&repos.seed, &["push", "--quiet", "origin", "main"]);
    let mutant = Spawns::new(&live);
    let init = compose(&mutant, &ctx, &init_inputs).unwrap();
    let bound = crate::engine::bind(
        init.iter()
            .map(|(item, place)| (item.as_ref(), place.clone())),
        &mutant,
        &ctx,
    );
    aterm_converge::run_scoped(
        &bound,
        Mode::Apply,
        Scope::EVERY,
        &mut NoJournal,
        &mut |_| {},
    )
    .unwrap();
    let calls: Vec<String> = mutant
        .record()
        .iter()
        .map(|(cmd, _)| cmd.display())
        .collect();
    let caught = reaches(&calls);
    assert!(
        caught.iter().any(|call| call.contains(" fetch ")),
        "{caught:#?}"
    );
    assert_eq!(repos.head(&repos.work), tip);
}

/// A MAC WITH NO `[develop]` RECORD HAS NO DEVELOP ROW in doctor's read, and asked for one
/// (`--only develop`) its record row says what makes one. A profile that cannot be read is
/// one `develop.record` row saying why, kept whatever `--only` narrows the read to, and no
/// other develop item is composed.
#[test]
fn doctor_reads_no_develop_row_unrecorded_and_one_row_for_a_profile_it_cannot_read() {
    let (sys, alice) = alice_with(Some(WRITTEN));
    let ctx = ctx(std::slice::from_ref(&alice));
    let composed = doctor(&sys, &ctx, false, Some(found_targo()));
    assert!(composed.items.is_empty() && !composed.unreadable);
    assert_eq!(read(&sys, &ctx, &composed, &[]).unwrap(), Vec::new());
    let (sys, _) = alice_with(None);
    let composed = doctor(&sys, &ctx, false, None);
    assert!(composed.items.is_empty() && !composed.unreadable);
    let asked = doctor(&sys, &ctx, true, None);
    let rows = read(&sys, &ctx, &asked, &["develop".to_owned()]).unwrap();
    assert_eq!(rows.len(), 1, "{rows:?}");
    assert_eq!(rows[0].outcome, Outcome::Ok);
    assert_eq!(
        rows[0].to.as_deref(),
        Some("no [develop] table: the user profile (`aterm init --develop` records one)")
    );
    assert!(
        sys.calls().iter().all(|call| !call.contains("git")),
        "{:?}",
        sys.calls()
    );

    for (profile, mode, why) in [
        ("[develop\n", 0o600, "invalid ~/.config/aterm/init.toml"),
        (
            "[develop]\ncheckouts = [\"~/aterm\"]\n",
            0o664,
            "cannot safely read ~/.config/aterm/init.toml",
        ),
        (
            "[develop]\ncheckouts = [\"aterm\"]\n",
            0o600,
            "must be ~/<path> or absolute",
        ),
    ] {
        let (sys, alice) = alice_with(None);
        sys.write_user_file(&alice, &profile_path(&alice), profile.as_bytes(), mode)
            .unwrap();
        let ctx = crate::items::defaults::tests::ctx(std::slice::from_ref(&alice));
        let composed = doctor(&sys, &ctx, false, Some(found_targo()));
        assert!(composed.unreadable, "{profile}");
        for only in [Vec::new(), vec!["shell".to_owned()]] {
            let rows = read(&sys, &ctx, &composed, &only).unwrap();
            assert_eq!(rows.len(), 1, "{profile}: {rows:?}");
            assert_eq!(rows[0].id, RECORD);
            assert_eq!(rows[0].outcome, Outcome::Unknown);
            assert!(
                rows[0]
                    .why
                    .as_deref()
                    .is_some_and(|said| said.contains(why)),
                "{profile}: {rows:?}"
            );
        }
        assert!(sys.calls().iter().all(|call| !call.contains("git")));
    }
}

/// EACH OF DOCTOR'S DEVELOP ITEMS REFUSES ITS APPLY AND RUNS NOTHING, called straight as an
/// engine that applied install items would call it: the record, the checkout (no fetch, no
/// merge), the submodule (no `submodule update`) and the tools (no `verify provision`) each
/// answer [`DOCTOR_CHANGES_NOTHING`], and no command runs and no file is written. Negative
/// control: init's items of the same Mac, applied the same way, each run something (the
/// record writes the `init.toml` it wants, the others read the checkout with git), so a refusal
/// removed from any of doctor's four would show.
#[test]
fn every_doctor_develop_item_refuses_its_apply_and_runs_nothing() {
    for reader in [Reader::Doctor, Reader::Init] {
        let (sys, alice) = alice_with(None);
        script_local(&sys, &alice, ORIGIN, true, "");
        script_fetched(&sys, &alice, HEAD);
        let ctx = ctx(std::slice::from_ref(&alice));
        let placed = items(
            &alice,
            Some(spelled(&["~/aterm"])),
            reader,
            Some(found_targo()),
        )
        .unwrap();
        let ids: Vec<String> = placed.iter().map(|(item, _)| item.id()).collect();
        assert_eq!(
            ids,
            [
                RECORD,
                "develop.checkout@~/aterm",
                "develop.submodules@~/aterm",
                "develop.workspace-tools@~/aterm"
            ]
        );
        for (item, _) in &placed {
            let before = sys.calls().len();
            let applied = item.apply(&sys, &ctx);
            let ran = sys.calls().split_off(before);
            if reader == Reader::Doctor {
                assert_eq!(
                    applied,
                    Err(DOCTOR_CHANGES_NOTHING.to_owned()),
                    "{}",
                    item.id()
                );
                assert_eq!(ran, Vec::<String>::new(), "{}", item.id());
            } else {
                assert_ne!(
                    applied,
                    Err(DOCTOR_CHANGES_NOTHING.to_owned()),
                    "{}",
                    item.id()
                );
                assert!(!ran.is_empty(), "{}: init's apply ran nothing", item.id());
            }
        }
    }
}

/// A relay item that wants a change, recording whether it was applied.
struct WantsChange(Arc<std::sync::atomic::AtomicBool>);

impl Item for WantsChange {
    fn id(&self) -> String {
        RECORD.to_owned()
    }
    fn family(&self) -> &'static str {
        "profile"
    }
    fn title(&self) -> String {
        "a develop item whose probe wants a change".to_owned()
    }
    fn probe(&self, _sys: &dyn System, _ctx: &Ctx) -> Probe {
        Probe::Change {
            from: "as it is".into(),
            to: "as wanted".into(),
        }
    }
    fn apply(&self, _sys: &dyn System, _ctx: &Ctx) -> Result<(), String> {
        self.0.store(true, std::sync::atomic::Ordering::SeqCst);
        Ok(())
    }
}

/// DOCTOR'S READ IS A PLAN, whatever its items would do: a relay item, which doctor's repair
/// scope applies, that wants a change reads planned through [`read`] and is never applied.
/// Negative control: the same item run in that scope as an apply is applied, so a read that
/// applied would show even though doctor's own items never plan a relay change today.
#[test]
fn doctor_s_develop_read_plans_and_applies_nothing_even_in_its_repair_scope() {
    let (sys, alice) = alice_with(None);
    let ctx = ctx(std::slice::from_ref(&alice));
    let applied = Arc::new(std::sync::atomic::AtomicBool::new(false));
    let composed = Composed {
        items: vec![(Box::new(WantsChange(applied.clone())), place(Class::Relay))],
        unreadable: false,
    };
    let rows = read(&sys, &ctx, &composed, &[]).unwrap();
    assert_eq!(rows.len(), 1, "{rows:?}");
    assert_eq!(rows[0].outcome, Outcome::Planned, "{rows:?}");
    assert!(!applied.load(std::sync::atomic::Ordering::SeqCst));

    let bound = crate::engine::bind(
        composed
            .items
            .iter()
            .map(|(item, place)| (item.as_ref(), place.clone())),
        &sys,
        &ctx,
    );
    aterm_converge::run_scoped(
        &bound,
        Mode::Apply,
        Scope::REPAIR,
        &mut NoJournal,
        &mut |_| {},
    )
    .unwrap();
    assert!(applied.load(std::sync::atomic::Ordering::SeqCst));
}

/// The tools item as doctor reads it ([`tools`], [`Reader::Doctor`]).
fn doctor_tools(targo: Option<Targo>) -> WorkspaceTools {
    WorkspaceTools {
        reader: Reader::Doctor,
        ..tools(targo)
    }
}

/// DOCTOR'S TOOLS READ OF A LOCK NAMING CRATES THIS MAC HAS NOT DOWNLOADED IS BLOCKED, naming
/// what fetches them: cargo under `--offline` refuses the download, in either of the two ways
/// it says so, and doctor reads [`NOT_DOWNLOADED`]. Negative control: init's read of the same
/// answer (it runs without `--offline`, so such a refusal is not its own) is the plain "did not
/// finish".
#[test]
fn doctor_reads_an_offline_refusal_of_the_checkout_s_lock_as_blocked() {
    for said in [
        "error: failed to download `aterm-fixture v1.0.0`\n\nCaused by:\n  attempting to make \
         an HTTP request, but --offline was specified\n",
        "error: no matching package named `aterm-fixture` found\nnote: offline mode \
         (--offline) can sometimes cause surprising resolution failures\n",
    ] {
        let (sys, alice) = alice_with(None);
        script_local(&sys, &alice, ORIGIN, true, "");
        sys.on(DOCTOR_CHECK, Ran::exit(101, said));
        sys.on(CHECK, Ran::exit(101, said));
        let ctx = ctx(std::slice::from_ref(&alice));
        assert_eq!(
            doctor_tools(Some(found_targo())).probe(&sys, &ctx),
            Probe::Blocked {
                why: NOT_DOWNLOADED.into()
            },
            "{said}"
        );
        let init = tools(Some(found_targo())).probe(&sys, &ctx);
        assert!(
            matches!(&init, Probe::Unusable { why } if why.contains("did not finish")),
            "{init:?}"
        );
    }
}

/// DOCTOR WAITS AT MOST TWO MINUTES FOR A CHECKOUT'S TOOLS: its `--check` runs under
/// [`DOCTOR_BUDGET`], so a cargo waiting on another build's lock on the checkout's build
/// directory holds doctor's report no longer, and a run that gives no answer in time reads
/// blocked, saying so and what reads it with no such limit. Negative control: init's own run
/// keeps [`LONG_BUDGET`], and its read of a run that gave no answer is unusable, as before.
#[test]
fn doctor_stops_waiting_for_a_checkout_s_tools_after_its_budget() {
    let targo = Path::new("/Users//alice/.atpkg/bin/targo");
    assert_eq!(
        doctor_tools(Some(found_targo()))
            .command(targo, true)
            .budget,
        DOCTOR_BUDGET
    );
    assert_eq!(
        tools(Some(found_targo())).command(targo, true).budget,
        LONG_BUDGET
    );
    assert!(DOCTOR_BUDGET < LONG_BUDGET);

    let (sys, alice) = alice_with(None);
    script_local(&sys, &alice, ORIGIN, true, "");
    sys.time_out(DOCTOR_CHECK).time_out(CHECK);
    let ctx = ctx(std::slice::from_ref(&alice));
    let probe = doctor_tools(Some(found_targo())).probe(&sys, &ctx);
    assert!(
        matches!(&probe, Probe::Blocked { why }
            if why.contains("no answer within 120s")
                && why.contains("`aterm init --check --develop` waits for it")),
        "{probe:?}"
    );
    let init = tools(Some(found_targo())).probe(&sys, &ctx);
    assert!(
        matches!(&init, Probe::Unusable { why } if why.contains("no answer within 1800s")),
        "{init:?}"
    );
}

/// A CHECKOUT THAT RECORDS NO DEFAULT BRANCH OF ORIGIN'S IS BLOCKED IN DOCTOR'S READ, NAMING THE
/// GIT ACT THAT RECORDS IT, and that act clears it: with `refs/remotes/origin/HEAD` deleted the
/// row names `git -C ~/work remote set-head origin --auto` (no remedy of aterm's), and once it
/// has run the row reads up to date as last fetched; with origin's branch itself deleted the row
/// names `git -C ~/work fetch origin`, and once that has run it reads up to date again. Doctor
/// itself never fetches or asks origin. Negative control: `aterm init --develop`'s own apply of
/// the same checkout (current with origin) fetches nothing and leaves `refs/remotes/origin/HEAD`
/// missing, so naming it would change nothing.
#[test]
fn a_checkout_without_origin_s_head_names_the_git_act_that_records_it() {
    let live = Live::new();
    let repos = Repos::new(&live, "doctor-origin-head");
    let profile = Path::new(&repos.who.home).join(PROFILE);
    std::fs::create_dir_all(profile.parent().unwrap()).unwrap();
    std::fs::write(&profile, "[develop]\ncheckouts = [\"~/work\"]\n").unwrap();
    let ctx = ctx(std::slice::from_ref(&repos.who));
    let checkout_row = |sys: &Spawns<'_>| {
        let composed = doctor(sys, &ctx, false, None);
        let rows = read(sys, &ctx, &composed, &[]).unwrap();
        let row = rows
            .into_iter()
            .find(|row| row.id == "develop.checkout@~/work")
            .unwrap_or_else(|| panic!("no checkout row"));
        let calls: Vec<String> = sys.record().iter().map(|(cmd, _)| cmd.display()).collect();
        assert_eq!(reaches(&calls), Vec::<String>::new(), "{calls:#?}");
        row
    };
    let head = repos.head(&repos.work);
    let current = Some(format!(
        "up to date with origin/main at {}, as last fetched",
        &head[..12]
    ));

    repos.g(&repos.work, &["remote", "set-head", "origin", "--delete"]);
    let row = checkout_row(&Spawns::new(&live));
    assert_eq!(row.outcome, Outcome::Blocked, "{row:?}");
    assert!(
        row.why
            .as_deref()
            .is_some_and(|why| why.contains("records no default branch of origin's")
                && why.ends_with(
                    "`git -C ~/work remote set-head origin --auto` records it (it asks origin once)"
                )),
        "{row:?}"
    );
    assert_eq!(
        row.explain.as_ref().map(|explain| &explain.remedy),
        Some(&Remedy::None)
    );

    let init_sys = Spawns::new(&live);
    let init = compose(
        &init_sys,
        &ctx,
        &Inputs {
            want: None,
            asked: false,
            cwd: None,
            targo: None,
        },
    )
    .unwrap();
    let bound = crate::engine::bind(
        init.iter()
            .map(|(item, place)| (item.as_ref(), place.clone())),
        &init_sys,
        &ctx,
    );
    let rows = aterm_converge::run_scoped(
        &bound,
        Mode::Apply,
        Scope::EVERY,
        &mut NoJournal,
        &mut |_| {},
    )
    .unwrap();
    let init_row = rows
        .iter()
        .find(|row| row.id == "develop.checkout@~/work")
        .unwrap();
    assert_eq!(init_row.outcome, Outcome::Ok, "{rows:?}");
    assert!(
        !subcommands(&init_sys.record()).contains(&"fetch".to_owned()),
        "{:#?}",
        init_sys.record()
    );
    assert_eq!(checkout_row(&Spawns::new(&live)).outcome, Outcome::Blocked);

    repos.g(&repos.work, &["remote", "set-head", "origin", "--auto"]);
    let row = checkout_row(&Spawns::new(&live));
    assert_eq!(
        (row.outcome, row.to.clone()),
        (Outcome::Ok, current.clone()),
        "{row:?}"
    );

    repos.g(
        &repos.work,
        &["update-ref", "-d", "refs/remotes/origin/main"],
    );
    let row = checkout_row(&Spawns::new(&live));
    assert_eq!(row.outcome, Outcome::Blocked, "{row:?}");
    assert!(
        row.why.as_deref().is_some_and(|why| why
            .starts_with("refs/remotes/origin/HEAD names origin/main, which this checkout")
            && why.ends_with("`git -C ~/work fetch origin` records it (it asks origin once)")),
        "{row:?}"
    );
    repos.g(&repos.work, &["fetch", "--quiet", "origin"]);
    let row = checkout_row(&Spawns::new(&live));
    assert_eq!(
        (row.outcome, row.to.clone()),
        (Outcome::Ok, current),
        "{row:?}"
    );
    assert_eq!(repos.head(&repos.work), head);
}

/// A checkout's spelling as one shell word: plain as it is, else single-quoted after a `~/`
/// that still expands, a quote inside closed and reopened.
#[test]
fn a_spelled_checkout_is_one_shell_word() {
    assert_eq!(shell_word("~/aterm"), "~/aterm");
    assert_eq!(shell_word("/Volumes/w/aterm-2"), "/Volumes/w/aterm-2");
    assert_eq!(shell_word("~/my work"), "~/'my work'");
    assert_eq!(shell_word("/Volumes/at \"x\""), "'/Volumes/at \"x\"'");
    assert_eq!(shell_word("~/it's"), "~/'it'\\''s'");
}
