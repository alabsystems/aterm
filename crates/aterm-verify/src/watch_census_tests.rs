// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;
#[test]
fn bootstrap_stop_census_keeps_stage_and_environment_kind() {
    let cause = |pid| {
        format!(
            "sample exit=75\nwatch bootstrap stopped: code=Some(75) kind=environment condition=refusal for {pid}"
        )
    };
    assert_eq!(summary(&cause(41)), "watch bootstrap environment");
    assert_eq!(summary(&cause(41)), summary(&cause(42)));
    assert_eq!(
        summary(
            "sample exit=75\nwatch stage=tree stopped: code=Some(75) kind=run-stop condition=stopping"
        ),
        "watch stage=tree run-stop"
    );
}

#[test]
fn preemption_census_ranks_conditions_across_distinct_senders() {
    let ledger = |pid| {
        format!(
            "tree: metrics\ncapacity background preemption: signal=15 sender_pid={pid}; ledger observation failed: planted refusal\nverify: COULD NOT RUN — siblings unrun"
        )
    };
    let release = "tree: metrics\ncapacity background preemption: signal=15 sender_pid=43; Release claim pid=44 what=ledger observation failed: test text\nverify: COULD NOT RUN — siblings unrun";
    assert_eq!(summary(&ledger(41)), "capacity ledger observation failed");
    assert_eq!(summary(&ledger(42)), summary(&ledger(41)));
    assert_eq!(summary(release), "Release claim preemption");
    assert_ne!(summary(release), summary(&ledger(41)));
    assert_eq!(
        summary(
            "capacity background stopped before admission: ledger observation failed: planted; detail"
        ),
        "capacity ledger observation failed"
    );
    assert_eq!(
        summary("tree: metrics\naterm-verify: the child was killed by signal 15"),
        "child signal=15"
    );
}

#[test]
fn named_cause_wins_and_historical_absence_is_not_attributed() {
    let mut record: Record = aterm_toml::from_str("commit='c'\ntree='t'\nhost='h'\nverifier=''\ninputs=''\ntoken='k'\nstate='cancelled'\nstarted=0\nupdated=1\nlease_until=0\nrows=[]").unwrap();
    assert!(cause(&record).starts_with("unknown"));
    record.cause = "sample signal=15; sender unavailable".into();
    assert_eq!(cause(&record), record.cause);
}

/// A repository whose `main` holds a workspace and whose `origin` holds the lane's journal. The
/// case's crate `item` depends on `dep` through the workspace table, and by version on `shim`
/// and on the registry's `reg`, which depends on `linked`; the root manifest patches `shim` and
/// `linked` to paths. `dep` depends on a package inside its own directory and on one inside the
/// git pin `vendor/bus`. Nothing depends on `other`. [`LOCK`] is the lock cargo would write.
struct Workspace {
    _dir: aterm_tempfile::TempDir,
    root: std::path::PathBuf,
    /// The time the next verdict is recorded at; each verdict moves it on by one.
    clock: std::cell::Cell<u64>,
}

const CASE: &str = "-p item --lib -- tests::case";

const MANIFEST: &str = "[workspace]\nmembers = [\"crates/*\"]\n\
    [workspace.package]\nversion = \"0.1.0\"\n\
    [workspace.dependencies]\ndep = { path = \"crates/dep\" }\n\
    [patch.crates-io]\nshim = { path = \"crates/shim\" }\nlinked = { path = \"patched/linked\" }\n";

const LOCK: &str = "version = 4\n\n\
    [[package]]\nname = \"bus\"\nversion = \"3.0.0\"\n\n\
    [[package]]\nname = \"dep\"\nversion = \"0.1.0\"\ndependencies = [\n \"bus\",\n \"nested\",\n]\n\n\
    [[package]]\nname = \"item\"\nversion = \"0.1.0\"\ndependencies = [\n \"dep\",\n \"reg\",\n \"shim\",\n]\n\n\
    [[package]]\nname = \"linked\"\nversion = \"1.0.0\"\n\n\
    [[package]]\nname = \"nested\"\nversion = \"0.1.0\"\n\n\
    [[package]]\nname = \"other\"\nversion = \"0.1.0\"\n\n\
    [[package]]\nname = \"reg\"\nversion = \"1.2.0\"\n\
    source = \"registry+https://example.invalid/index\"\n\
    checksum = \"checksum-of-reg-1.2.0\"\ndependencies = [\n \"linked\",\n]\n\n\
    [[package]]\nname = \"shim\"\nversion = \"1.0.0\"\n";

impl Workspace {
    fn new() -> Self {
        let dir = aterm_tempfile::tempdir().unwrap();
        judged::git(dir.path(), &["init", "--bare", "remote"]).unwrap();
        judged::git(dir.path(), &["init", "-b", "main", "source"]).unwrap();
        let root = dir.path().join("source");
        let git = |args: &[&str]| judged::git(&root, args).unwrap();
        git(&["config", "user.name", "Andrew Yates"]);
        git(&["config", "user.email", "test@example.invalid"]);
        git(&[
            "remote",
            "add",
            "origin",
            dir.path().join("remote").to_str().unwrap(),
        ]);
        let repo = Self {
            _dir: dir,
            root,
            clock: std::cell::Cell::new(0),
        };
        repo.commit_files(&[
            ("Cargo.toml", MANIFEST),
            ("Cargo.lock", LOCK),
            (
                "crates/item/Cargo.toml",
                "[package]\nname = \"item\"\nversion.workspace = true\n[dependencies]\ndep = { workspace = true }\n\
                 shim = \"1\"\nreg = \"1\"\n",
            ),
            (
                "crates/item/src/lib.rs",
                "pub fn item() -> u8 { dep::dep() }\n",
            ),
            (
                "crates/dep/Cargo.toml",
                "[package]\nname = \"dep\"\nversion.workspace = true\n[dependencies]\nnested = { path = \"nested\" }\n\
                 bus = { path = \"../../vendor/bus/crates/client\" }\n",
            ),
            ("crates/dep/src/lib.rs", "pub fn dep() -> u8 { 1 }\n"),
            (
                "crates/dep/nested/Cargo.toml",
                "[package]\nname = \"nested\"\nversion.workspace = true\n",
            ),
            ("crates/dep/nested/src/lib.rs", "pub fn nested() {}\n"),
            ("crates/other/Cargo.toml", "[package]\nname = \"other\"\nversion.workspace = true\n"),
            ("crates/other/src/lib.rs", "pub fn other() -> u8 { 1 }\n"),
            (
                "crates/shim/Cargo.toml",
                "[package]\nname = \"shim\"\nversion = \"1.0.0\"\n",
            ),
            ("crates/shim/src/lib.rs", "pub fn shim() -> u8 { 1 }\n"),
            (
                "patched/linked/Cargo.toml",
                "[package]\nname = \"linked\"\nversion = \"1.0.0\"\n",
            ),
            ("patched/linked/src/lib.rs", "pub fn linked() -> u8 { 1 }\n"),
        ]);
        repo.pin(&repo.head());
        repo
    }

    /// Commit `vendor/bus` pinned at `commit`: the commit that pins it.
    fn pin(&self, commit: &str) -> String {
        let entry = format!("160000,{commit},vendor/bus");
        judged::git(
            &self.root,
            &["update-index", "--add", "--cacheinfo", &entry],
        )
        .unwrap();
        judged::git(&self.root, &["commit", "-m", "pin"]).unwrap();
        self.head()
    }

    fn head(&self) -> String {
        judged::git(&self.root, &["rev-parse", "HEAD"]).unwrap()
    }

    /// Commit `text` at `path`: the commit.
    fn commit(&self, path: &str, text: &str) -> String {
        self.commit_files(&[(path, text)])
    }

    /// Commit each text at its path, in one commit: the commit.
    fn commit_files(&self, files: &[(&str, &str)]) -> String {
        for (path, text) in files {
            let file = self.root.join(path);
            std::fs::create_dir_all(file.parent().unwrap()).unwrap();
            std::fs::write(file, text).unwrap();
            judged::git(&self.root, &["add", path]).unwrap();
        }
        judged::git(&self.root, &["commit", "-m", files[0].0]).unwrap();
        self.head()
    }

    /// Append the lane's verdict on `commit`, recorded by `host` at the clock's time: one row
    /// for each id of `rows`, run at `load1`, red where its items name anything.
    fn judge(&self, commit: &str, tree: &str, host: &str, load1: &str, rows: &[(&str, &[&str])]) {
        let outcome = |red_items: &[&str]| if red_items.is_empty() { "pass" } else { "red" };
        let state = if rows.iter().all(|(_, red_items)| red_items.is_empty()) {
            "pass"
        } else {
            "red"
        };
        let updated = self.clock.get();
        self.clock.set(updated + 1);
        let record = Record {
            commit: commit.into(),
            tree: tree.into(),
            host: host.into(),
            verifier: commit.into(),
            controller: String::new(),
            inputs: "test".into(),
            token: commit.into(),
            state: state.into(),
            cause: String::new(),
            started: updated,
            updated,
            lease_until: 0,
            rows: rows
                .iter()
                .map(|(id, red_items)| judged::Row {
                    id: (*id).into(),
                    outcome: outcome(red_items).into(),
                    log: format!(
                        "granted_cores=4 admission_waiting_s=0.5 load1={load1} elapsed_s=60.25\n\
                         test result: {}",
                        outcome(red_items)
                    ),
                    red_items: red_items.iter().map(|item| (*item).to_owned()).collect(),
                    suspects: vec![],
                    fixed_by: None,
                    decisions: vec![],
                })
                .collect(),
        };
        assert!(
            judged::append(
                &self.root,
                "origin",
                self.journal().as_deref(),
                &record,
                judged::deadline()
            )
            .unwrap()
        );
    }

    /// [`Workspace::judge`] of the rows of a commit this repository holds.
    fn judged_rows(&self, commit: &str, host: &str, load1: &str, rows: &[(&str, &[&str])]) {
        let tree = judged::git(&self.root, &["rev-parse", &format!("{commit}^{{tree}}")]).unwrap();
        self.judge(commit, &tree, host, load1, rows);
    }

    /// [`Workspace::judged_rows`] of the one row `tree`.
    fn judged(&self, commit: &str, host: &str, load1: &str, red_items: &[&str]) {
        self.judged_rows(commit, host, load1, &[("tree", red_items)]);
    }

    fn journal(&self) -> Option<String> {
        judged::head(&self.root, "origin", judged::deadline()).unwrap()
    }

    /// What `verify watch --census <journal>` prints with `flags`: its lines and the journal's
    /// head.
    fn census(&self, flags: &[&str]) -> (Vec<String>, String) {
        let journal = self.journal().unwrap();
        let mut args = vec![journal.clone()];
        args.extend(flags.iter().map(|flag| (*flag).to_owned()));
        (report(&self.root, &args).unwrap(), journal)
    }

    /// [`Workspace::census`] with `--flaky`.
    fn flaky(&self) -> (Vec<String>, String) {
        self.census(&["--flaky"])
    }

    /// Fail unless `--flaky` lists nothing of `records` records that name `red_cases` cases.
    #[track_caller]
    fn lists_nothing(&self, records: usize, red_cases: usize, why: &str) {
        let (lines, journal) = self.flaky();
        assert_eq!(
            lines,
            [format!(
                "journal={journal} records={records} red_cases={red_cases} flaky=0 since=None"
            )],
            "{why}"
        );
    }
}

#[test]
fn a_case_red_on_one_tree_and_green_on_another_with_an_unchanged_closure_is_listed() {
    let repo = Workspace::new();
    let red = repo.head();
    repo.judged(
        &red,
        "m45",
        "Some(61.5)",
        &[
            CASE,
            "NOT RUN targo test --workspace --tests (trustdoc) — 2 of 3 test binaries not run",
            "test    1m00s  1 test failed",
        ],
    );
    let green = repo.commit("crates/other/src/lib.rs", "pub fn other() -> u8 { 2 }\n");
    repo.judged(&green, "m112", "Some(3.25)", &[]);
    let (lines, journal) = repo.flaky();
    assert_eq!(
        lines,
        [
            format!(
                "flaky item={CASE:?} row=tree red={red} red_host=m45 red_load1=61.50 \
                 green={green} green_host=m112 green_load1=3.25 config_changed=none"
            ),
            format!("journal={journal} records=2 red_cases=1 flaky=1 since=None"),
        ],
        "only the row's test case is a case, and a change to a crate it does not read keeps it one"
    );

    // The same two verdicts with a commit between them that changes the case's own crate.
    let repo = Workspace::new();
    let red = repo.head();
    repo.judged(&red, "m45", "Some(61.5)", &[CASE]);
    repo.commit("crates/other/src/lib.rs", "pub fn other() -> u8 { 2 }\n");
    let green = repo.commit(
        "crates/item/src/lib.rs",
        "pub fn item() -> u8 { dep::dep() + 1 }\n",
    );
    repo.judged(&green, "m112", "Some(3.25)", &[]);
    repo.lists_nothing(2, 1, "a green on a changed crate is a change, not a flake");
}

#[test]
fn a_change_to_a_crate_the_case_depends_on_lists_nothing() {
    let repo = Workspace::new();
    let red = repo.head();
    repo.judged(&red, "m45", "Some(61.5)", &[CASE]);
    let green = repo.commit("crates/dep/src/lib.rs", "pub fn dep() -> u8 { 2 }\n");
    repo.judged(&green, "m112", "Some(3.25)", &[]);
    repo.lists_nothing(
        2,
        1,
        "the closure is the case's crate and every crate that one depends on by path, and a \
         directory is compared although the closure also holds a crate inside it",
    );

    repo.commit("crates/dep/src/lib.rs", "pub fn dep() -> u8 { 1 }\n");
    let repinned = repo.pin(&green);
    repo.judged(&repinned, "m112", "Some(3.25)", &[]);
    repo.lists_nothing(
        3,
        1,
        "a crate inside a git pin is compared by the commit pinned",
    );
}

#[test]
fn a_change_to_a_crate_the_root_manifest_patches_in_lists_nothing() {
    // No manifest names either crate by path: `item` names `shim` by version and the registry's
    // `reg` names `linked`. The root manifest's patch table puts both at a path, and only the
    // lock says that `item` links them.
    for (path, text, why) in [
        (
            "crates/shim/src/lib.rs",
            "pub fn shim() -> u8 { 2 }\n",
            "a crate the case names by version, patched to a path, is in its closure",
        ),
        (
            "patched/linked/src/lib.rs",
            "pub fn linked() -> u8 { 2 }\n",
            "a patched crate a registry package of the case links is in its closure",
        ),
    ] {
        let repo = Workspace::new();
        let red = repo.head();
        repo.judged(&red, "m45", "Some(61.5)", &[CASE]);
        let green = repo.commit(path, text);
        repo.judged(&green, "m112", "Some(3.25)", &[]);
        repo.lists_nothing(2, 1, why);
    }
}

#[test]
fn a_green_that_builds_the_case_from_other_inputs_is_no_witness() {
    // Each green holds every directory of the red commit's closure as the red commit does.
    let retargeted = MANIFEST.replace("crates/dep", "vendored/dep");
    let rebuilt = LOCK.replace("1.2.0", "1.2.1");
    let greens: [(&[(&str, &str)], &str); 5] = [
        (
            &[
                ("Cargo.toml", &retargeted),
                (
                    "vendored/dep/Cargo.toml",
                    "[package]\nname = \"dep\"\nversion.workspace = true\n",
                ),
                ("vendored/dep/src/lib.rs", "pub fn dep() -> u8 { 2 }\n"),
            ],
            "the workspace table puts a crate the case depends on in another directory",
        ),
        (
            &[("Cargo.lock", &rebuilt)],
            "the lock resolves another build of a registry package the case links",
        ),
        (
            &[("trust.lock", "[trust]\nbuild = 2\n")],
            "another compiler built the case",
        ),
        (
            &[("rust-toolchain.toml", "[toolchain]\nchannel = \"other\"\n")],
            "another toolchain built the case",
        ),
        (
            &[(".cargo/config.toml", "[env]\nFIXTURE = \"1\"\n")],
            "another cargo configuration built the case",
        ),
    ];
    for (files, why) in greens {
        let repo = Workspace::new();
        let red = repo.head();
        repo.judged(&red, "m45", "Some(61.5)", &[CASE]);
        let green = repo.commit_files(files);
        repo.judged(&green, "m112", "Some(3.25)", &[]);
        repo.lists_nothing(2, 1, why);
    }
}

#[test]
fn the_witness_is_the_green_whose_workspace_files_differ_least_and_an_unread_commit_is_said() {
    let repo = Workspace::new();
    let red = repo.head();
    repo.judged(&red, "m45", "None", &[CASE]);
    // The workspace's version moves: the root manifest and every member's lock entry change,
    // the case's own among them, and no file of a crate does.
    let version = |text: &str| text.replace("version = \"0.1.0\"", "version = \"0.2.0\"");
    let bumped = repo.commit_files(&[
        ("Cargo.toml", &version(MANIFEST)),
        ("Cargo.lock", &version(LOCK)),
    ]);
    repo.judged(&bumped, "m112", "Some(3.25)", &[]);
    let (lines, journal) = repo.flaky();
    assert_eq!(
        lines,
        [
            format!(
                "flaky item={CASE:?} row=tree red={red} red_host=m45 red_load1=unknown \
                 green={bumped} green_host=m112 green_load1=3.25 \
                 config_changed=Cargo.toml,Cargo.lock"
            ),
            format!("journal={journal} records=2 red_cases=1 flaky=1 since=None"),
        ],
        "a workspace-wide file that differs where the case's closure does not is named"
    );

    repo.commit_files(&[("Cargo.toml", MANIFEST), ("Cargo.lock", LOCK)]);
    let same = repo.commit("crates/other/src/lib.rs", "pub fn other() -> u8 { 2 }\n");
    repo.judged(&same, "m72", "Some(9)", &[]);
    // A verdict on a commit this repository was never given: its closure cannot be compared.
    let absent = "1".repeat(40);
    repo.judge(
        &absent,
        &"2".repeat(40),
        "m45",
        "Some(1)",
        &[("tree", &[CASE])],
    );
    let (lines, journal) = repo.flaky();
    assert_eq!(
        lines[0],
        format!(
            "flaky item={CASE:?} row=tree red={red} red_host=m45 red_load1=unknown \
             green={same} green_host=m72 green_load1=9.00 config_changed=none"
        ),
        "a later green with every workspace file unchanged is the better witness"
    );
    assert!(
        lines[1].starts_with(&format!("unread commit={absent} why=")),
        "{lines:?}"
    );
    assert_eq!(
        lines[2..],
        [format!(
            "journal={journal} records=4 red_cases=2 flaky=1 since=None"
        )]
    );
}

#[test]
fn of_two_greens_the_nearer_in_time_is_the_witness() {
    // The journal lists records by tree, so neither its first nor its last is the nearer by rule.
    for (earlier, later, nearer_is_later) in [(4, 13, true), (8, 16, false)] {
        let repo = Workspace::new();
        repo.clock.set(earlier);
        let before = repo.head();
        repo.judged(&before, "m72", "Some(9)", &[]);
        repo.clock.set(10);
        let red = repo.commit("crates/other/src/lib.rs", "pub fn other() -> u8 { 2 }\n");
        repo.judged(&red, "m45", "Some(61.5)", &[CASE]);
        repo.clock.set(later);
        let after = repo.commit("crates/other/src/lib.rs", "pub fn other() -> u8 { 3 }\n");
        repo.judged(&after, "m112", "Some(3.25)", &[]);
        let (green, host, load1) = if nearer_is_later {
            (&after, "m112", "3.25")
        } else {
            (&before, "m72", "9.00")
        };
        let (lines, journal) = repo.flaky();
        assert_eq!(
            lines,
            [
                format!(
                    "flaky item={CASE:?} row=tree red={red} red_host=m45 red_load1=61.50 \
                     green={green} green_host={host} green_load1={load1} config_changed=none"
                ),
                format!("journal={journal} records=3 red_cases=1 flaky=1 since=None"),
            ],
            "greens recorded at {earlier} and {later} of a red recorded at 10"
        );
    }
}

#[test]
fn a_pass_of_another_row_is_no_witness() {
    let repo = Workspace::new();
    let red = repo.head();
    repo.judged(&red, "m45", "Some(61.5)", &[CASE]);
    // Only another row ran and passed here.
    let other_row = repo.commit("crates/other/src/lib.rs", "pub fn other() -> u8 { 2 }\n");
    repo.judged_rows(&other_row, "m112", "Some(3.25)", &[("stable-x86_64", &[])]);
    // Another row passed here beside a `tree` row that is red and names no case.
    let still_red = repo.commit("crates/other/src/lib.rs", "pub fn other() -> u8 { 3 }\n");
    repo.judged_rows(
        &still_red,
        "m112",
        "Some(3.25)",
        &[
            ("stable-x86_64", &[]),
            ("tree", &["test    1m00s  1 test failed"]),
        ],
    );
    repo.lists_nothing(
        3,
        1,
        "the case ran in the `tree` row, which passed on neither tree",
    );

    // The `tree` row passed here beside another row that is red.
    let green = repo.commit("crates/other/src/lib.rs", "pub fn other() -> u8 { 4 }\n");
    let unrelated = "-p other --lib -- tests::unrelated";
    repo.judged_rows(
        &green,
        "m72",
        "Some(9)",
        &[("stable-x86_64", &[unrelated]), ("tree", &[])],
    );
    let (lines, journal) = repo.flaky();
    assert_eq!(
        lines,
        [
            format!(
                "flaky item={CASE:?} row=tree red={red} red_host=m45 red_load1=61.50 \
                 green={green} green_host=m72 green_load1=9.00 config_changed=none"
            ),
            format!("journal={journal} records=4 red_cases=2 flaky=1 since=None"),
        ],
        "a row's pass is a witness for that row's cases only: the other row passed twice, on \
         trees whose `other` crate is not the one its red case ran"
    );
}

#[test]
fn since_bounds_the_records_the_list_compares() {
    let repo = Workspace::new();
    let red = repo.head();
    repo.judged(&red, "m45", "Some(61.5)", &[CASE]);
    let green = repo.commit("crates/other/src/lib.rs", "pub fn other() -> u8 { 2 }\n");
    repo.judged(&green, "m112", "Some(3.25)", &[]);
    // The red verdict was recorded at 0 and the green one at 1.
    let (lines, journal) = repo.census(&["--since", "0", "--flaky"]);
    assert_eq!(
        lines,
        [
            format!(
                "flaky item={CASE:?} row=tree red={red} red_host=m45 red_load1=61.50 \
                 green={green} green_host=m112 green_load1=3.25 config_changed=none"
            ),
            format!("journal={journal} records=2 red_cases=1 flaky=1 since=Some(0)"),
        ]
    );
    let (lines, journal) = repo.census(&["--flaky", "--since", "1"]);
    assert_eq!(
        lines,
        [format!(
            "journal={journal} records=1 red_cases=0 flaky=0 since=Some(1)"
        )],
        "a red recorded before the window is not compared"
    );
}
