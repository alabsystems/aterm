// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

use super::*;

fn facts(root: &Path) -> (Process, ProcArgs) {
    let image = Image::at(&std::env::current_exe().unwrap()).unwrap();
    let process = Process {
        pid: 123,
        birth: 456,
        parent: 10,
        group: 123,
        session: 10,
        image,
    };
    let args = ProcArgs {
        exec_path: String::from_utf8(process.image.path.clone()).unwrap(),
        argv: vec![
            "original-codex-name".into(),
            "exec".into(),
            "--json".into(),
            "-".into(),
            "line one\nline two".into(),
        ],
        env: vec![
            format!("HOME={}", root.display()),
            format!("CODEX_AUTH_PATH={}/source/auth.json", root.display()),
            "UNRELATED_SECRET=must-not-be-retained".into(),
        ],
    };
    (process, args)
}

/// A session on this machine's own login carries no pointer, as a TUI's does: a pointer to it
/// would name no row, so its row would never hand off. A managed login's carries its own.
#[test]
fn a_session_on_the_default_login_carries_no_pointer() {
    let root = aterm_tempfile::tempdir().unwrap();
    let (process, args) = facts(root.path());
    let mut source = ExecSource::from_observations(process, args, root.path().into()).unwrap();
    let default_home = root.path().join(".codex");
    source.auth_path = default_home.join("auth.json");
    assert_eq!(source.account_env(&default_home), Some(Vec::new()));
    source.auth_path = root.path().join("source/auth.json");
    let carried = source.account_env(&default_home).unwrap();
    assert_eq!(carried.len(), 1);
    assert_eq!(carried[0].0, "CODEX_AUTH_PATH");
}

#[test]
fn native_exec_source_retains_the_whole_invocation_and_only_auth_paths() {
    let root = aterm_tempfile::tempdir().unwrap();
    let (process, args) = facts(root.path());
    let expected = args.argv.clone();
    let source = ExecSource::from_observations(process, args, root.path().into()).unwrap();
    let target = root.path().join("other/auth.json");
    let command = source.command_for(&target).unwrap();
    assert_eq!(command.argv0, expected[0]);
    assert_eq!(command.argv, expected[1..]);
    assert_eq!(command.auth_path, target.to_str().unwrap());
    assert_eq!(source.auth_path, root.path().join("source/auth.json"));
    assert_eq!(source.config_home, root.path().join(".codex"));
    let debug = format!("{source:?}");
    assert!(!debug.contains("line one"));
    assert!(!debug.contains("UNRELATED_SECRET"));
}

#[test]
fn native_exec_source_keeps_managed_login_attribution_with_a_shared_config_home() {
    let root = aterm_tempfile::tempdir().unwrap();
    let (process, mut args) = facts(root.path());
    let config = root.path().join("shared-config");
    args.env.push(format!("CODEX_HOME={}", config.display()));
    let source = ExecSource::from_observations(process, args, root.path().into()).unwrap();
    let login = crate::harness::accounts::ProcessLogin::of_env_for(
        crate::harness::accounts::Cli::Codex,
        source.account_env(&root.path().join(".codex")).unwrap(),
    );
    assert!(login.default_home && login.route == crate::harness::accounts::AuthRoute::Stored);
    assert_eq!(login.login.as_deref(), root.path().join("source").to_str());
    assert_eq!(source.config_home, config);
    assert_eq!(source.auth_path, root.path().join("source/auth.json"));
}

/// A live `cat` whose argv is `argv` (its argv[0] first), run in `root` with only `HOME` and
/// `CODEX_AUTH_PATH` set, as [`facts`] sets them: a process the kernel reads as a test asks,
/// which has echoed a line, so it runs. It is `/bin/cat`'s stand-in
/// ([`aterm_uds::deadline::stand_in`]), since macOS shows no other process a platform binary's
/// environment. Killed when dropped.
struct Cat {
    child: aterm_uds::spawnfd::Child,
    _input: std::process::ChildStdin,
}

impl Cat {
    fn run(argv: &[&str], root: &Path) -> Self {
        use aterm_uds::spawnfd::Stdio;
        let (first, rest) = argv.split_first().unwrap();
        let program = aterm_uds::deadline::stand_in(Path::new("/bin/cat"));
        let mut child = aterm_uds::spawnfd::command(program)
            .arg0(first)
            .args(rest)
            .env_clear()
            .env("HOME", root)
            .env("CODEX_AUTH_PATH", root.join("source/auth.json"))
            .current_dir(root)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .spawn()
            .unwrap();
        let input = aterm_uds::deadline::cat_echoes(&mut child);
        Self {
            child,
            _input: input,
        }
    }

    fn pid(&self) -> u32 {
        self.child.id()
    }
}

impl Drop for Cat {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

fn miss(process: &Process, args: &ProcArgs, cwd: &Path) -> Option<CaptureMiss> {
    ExecSource::from_observations(process.clone(), args.clone(), cwd.into()).err()
}

/// Every argv that is not a Codex `exec` run's is no exec source, whatever else it holds: a run
/// that is not exec is not refused as an exec run this host cannot move.
#[test]
fn classify_reads_a_non_exec_argv_as_not_exec() {
    let root = aterm_tempfile::tempdir().unwrap();
    let (process, args) = facts(root.path());
    assert_eq!(miss(&process, &args, root.path()), None);
    let mut moved = process.clone();
    moved.image.inode += 1;
    for argv in [
        &["codex"][..],
        &["codex", "resume"],
        &["codex", "review"],
        &["codex", "login"],
        &["cat", "-"],
    ] {
        let mut changed = args.clone();
        changed.argv = argv.iter().map(|word| (*word).to_owned()).collect();
        assert_eq!(
            miss(&process, &changed, root.path()),
            Some(CaptureMiss::NotExec),
            "{argv:?}"
        );
        assert_eq!(
            miss(&moved, &changed, Path::new("relative")),
            Some(CaptureMiss::NotExec),
            "{argv:?}"
        );
    }
    let cat = Cat::run(&["cat", "-"], root.path());
    assert_eq!(
        ExecSource::classify(cat.pid()).err(),
        Some(CaptureMiss::NotExec)
    );
    assert_eq!(ExecSource::capture(cat.pid()), None);
}

/// Each refusal of an exec run's facts that no later read changes is `Unsupported`, never
/// `Unread`, so it ends the asking.
#[test]
fn classify_reads_each_permanent_miss_as_unsupported() {
    let root = aterm_tempfile::tempdir().unwrap();
    let (process, args) = facts(root.path());
    let cwd = root.path();
    assert_eq!(miss(&process, &args, cwd), None);
    let with_process = |change: fn(&mut Process)| {
        let mut changed = process.clone();
        change(&mut changed);
        miss(&changed, &args, cwd)
    };
    let with_env = |change: &dyn Fn(&mut Vec<String>)| {
        let mut changed = args.clone();
        change(&mut changed.env);
        miss(&process, &changed, cwd)
    };
    let with_argv = |change: fn(&mut Vec<String>)| {
        let mut changed = args.clone();
        change(&mut changed.argv);
        miss(&process, &changed, cwd)
    };
    let cases = [
        (
            "a relative working directory",
            miss(&process, &args, Path::new("relative")),
        ),
        ("pid 0", with_process(|p| p.pid = 0)),
        ("birth 0", with_process(|p| p.birth = 0)),
        (
            "an image path that is not UTF-8",
            with_process(|p| p.image.path = b"/codex-\xff".to_vec()),
        ),
        (
            "an image path that names another image",
            with_process(|p| p.image.inode += 1),
        ),
        (
            "HOME twice",
            with_env(&|env| env.push("HOME=/second".into())),
        ),
        (
            "CODEX_HOME twice",
            with_env(&|env| {
                env.extend(["CODEX_HOME=/first".into(), "CODEX_HOME=/second".into()]);
            }),
        ),
        (
            "CODEX_AUTH_PATH twice",
            with_env(&|env| env.push("CODEX_AUTH_PATH=/second/auth.json".into())),
        ),
        (
            "no Codex home",
            with_env(&|env| env.retain(|v| !v.starts_with("HOME="))),
        ),
        (
            "a relative HOME",
            with_env(&|env| {
                env.retain(|v| !v.starts_with("HOME="));
                env.push("HOME=relative".into());
            }),
        ),
        (
            "a relative auth path",
            with_env(&|env| {
                env.retain(|v| !v.starts_with("CODEX_AUTH_PATH="));
                env.push("CODEX_AUTH_PATH=relative/auth.json".into());
            }),
        ),
        (
            "a NUL in an argument",
            with_argv(|argv| argv.push("before\0after".into())),
        ),
        (
            "an invocation past the codec's bound",
            with_argv(|argv| argv.push("x".repeat(crate::harness::native_act::MAX_REQUEST))),
        ),
    ];
    for (why, got) in cases {
        assert_eq!(got, Some(CaptureMiss::Unsupported), "{why}");
    }
}

/// Two reads must agree on the process and on its judgment; any difference, or an unread
/// process, is `Unread`, which a later read may answer.
#[test]
fn classify_reads_two_different_reads_as_unread() {
    use CaptureMiss::{NotExec, Unread, Unsupported};
    let root = aterm_tempfile::tempdir().unwrap();
    let (process, args) = facts(root.path());
    let source = ExecSource::from_observations(process.clone(), args, root.path().into()).unwrap();
    let mut regenerated = source.clone();
    regenerated.exec_generation = Some(7);
    let mut reborn = process.clone();
    reborn.birth += 1;
    let over = |reads: Vec<Read>| {
        let mut reads = reads.into_iter();
        ExecSource::judged(move || reads.next().flatten())
    };
    let read = |process: &Process, judgment: Result<&ExecSource, CaptureMiss>| {
        Some((process.clone(), judgment.cloned()))
    };
    assert_eq!(
        over(vec![
            read(&process, Ok(&source)),
            read(&process, Ok(&source))
        ]),
        Ok(source.clone())
    );
    for kind in [NotExec, Unsupported] {
        assert_eq!(
            over(vec![read(&process, Err(kind)), read(&process, Err(kind))]),
            Err(kind)
        );
    }
    let differing = [
        ("no first read", vec![None, read(&process, Ok(&source))]),
        ("no second read", vec![read(&process, Ok(&source)), None]),
        (
            "another source",
            vec![
                read(&process, Ok(&source)),
                read(&process, Ok(&regenerated)),
            ],
        ),
        (
            "a miss, then a source",
            vec![read(&process, Err(NotExec)), read(&process, Ok(&source))],
        ),
        (
            "a source, then a miss",
            vec![
                read(&process, Ok(&source)),
                read(&process, Err(Unsupported)),
            ],
        ),
        (
            "two misses",
            vec![
                read(&process, Err(Unsupported)),
                read(&process, Err(NotExec)),
            ],
        ),
        (
            "one miss of two processes",
            vec![read(&process, Err(NotExec)), read(&reborn, Err(NotExec))],
        ),
    ];
    for (why, reads) in differing {
        assert_eq!(over(reads), Err(Unread), "{why}");
    }
    // The kernel reads no process at pid 1 or past i32.
    assert_eq!(ExecSource::classify(1), Err(Unread));
    assert_eq!(ExecSource::classify(u32::MAX), Err(Unread));
}

/// A kernel that gives each kind of answer from its script, in the order a read asks for that
/// kind, and then `None`.
#[derive(Default)]
struct Scripted {
    processes: std::collections::VecDeque<Option<Process>>,
    lineages: std::collections::VecDeque<Option<Lineage>>,
    args: std::collections::VecDeque<Option<ProcArgs>>,
    cwds: std::collections::VecDeque<Option<PathBuf>>,
}

/// What one bracket of a read answers: the process, then its lineage.
type Bracket = (Option<Process>, Option<Lineage>);

impl Scripted {
    /// One more read: its opening bracket, its arguments and directory, its closing bracket.
    fn then(
        mut self,
        open: Bracket,
        args: Option<ProcArgs>,
        cwd: Option<PathBuf>,
        close: Bracket,
    ) -> Self {
        for (process, lineage) in [open, close] {
            self.processes.push_back(process);
            self.lineages.push_back(lineage);
        }
        self.args.push_back(args);
        self.cwds.push_back(cwd);
        self
    }
}

impl Kernel for Scripted {
    fn process(&mut self, _: u32) -> Option<Process> {
        self.processes.pop_front().flatten()
    }

    fn lineage(&mut self, _: i32) -> Option<Lineage> {
        self.lineages.pop_front().flatten()
    }

    fn args(&mut self, _: u32) -> Option<ProcArgs> {
        self.args.pop_front().flatten()
    }

    fn cwd(&mut self, _: u32) -> Option<PathBuf> {
        self.cwds.pop_front().flatten()
    }
}

/// The lineage a kernel gives `process`: exec generation 1, unique id 77.
fn lineage_of(process: &Process) -> Lineage {
    Lineage {
        identity: aterm_uds::process::Identity {
            pid: i32::try_from(process.pid).unwrap(),
            birth: aterm_uds::process::Birth(process.birth),
            idversion: Some(1),
        },
        uniqueid: 77,
        parent: process.parent,
        pgid: process.group,
        launcher_version: None,
        tty: None,
    }
}

/// A read judges only the process both its brackets read. It is `None` where its opening lacks an
/// answer or reads a lineage of another birth, and where its closing bracket lacks one or differs
/// from the opening in the process, its birth, its exec generation or its unique id: for a miss
/// as for a source.
#[test]
fn a_read_holds_only_between_brackets_that_read_one_process() {
    use CaptureMiss::{NotExec, Unsupported};
    let root = aterm_tempfile::tempdir().unwrap();
    let (process, args) = facts(root.path());
    let lineage = lineage_of(&process);
    let cwd = root.path().to_path_buf();
    let mut not_exec = args.clone();
    not_exec.argv = vec!["codex".into(), "resume".into()];
    let mut source =
        ExecSource::from_observations(process.clone(), args.clone(), cwd.clone()).unwrap();
    source.exec_generation = Some(1);
    source.uniqueid = 77;
    let both = (Some(process.clone()), Some(lineage));
    let read = |open: Bracket, args: Option<&ProcArgs>, cwd: Option<&Path>, close: Bracket| {
        let mut kernel =
            Scripted::default().then(open, args.cloned(), cwd.map(Path::to_path_buf), close);
        ExecSource::read(&mut kernel, process.pid)
    };
    let mut reborn = process.clone();
    reborn.birth += 1;
    let mut moved = process.clone();
    moved.image.inode += 1;
    let mut regenerated = lineage;
    regenerated.identity.idversion = Some(2);
    let mut reused = lineage;
    reused.uniqueid += 1;
    let mut later = lineage;
    later.identity.birth.0 += 1;
    let closings = [
        ("no closing process", (None, Some(lineage))),
        (
            "a closing process of another birth",
            (Some(reborn.clone()), Some(lineage)),
        ),
        (
            "a closing process of another image",
            (Some(moved), Some(lineage)),
        ),
        ("no closing lineage", (Some(process.clone()), None)),
        (
            "an exec between the brackets",
            (Some(process.clone()), Some(regenerated)),
        ),
        (
            "another unique id at the close",
            (Some(process.clone()), Some(reused)),
        ),
        (
            "another birth at the close",
            (Some(process.clone()), Some(later)),
        ),
    ];
    for (args, cwd, judgment) in [
        (&args, cwd.as_path(), Ok(source)),
        (&not_exec, cwd.as_path(), Err(NotExec)),
        (&args, Path::new("relative"), Err(Unsupported)),
    ] {
        assert_eq!(
            read(both.clone(), Some(args), Some(cwd), both.clone()),
            Some((process.clone(), judgment.clone()))
        );
        for (why, close) in closings.clone() {
            assert_eq!(
                read(both.clone(), Some(args), Some(cwd), close),
                None,
                "{why}: {judgment:?}"
            );
        }
        let openings = [
            ("no process", (None, Some(lineage)), Some(args), Some(cwd)),
            (
                "no lineage",
                (Some(process.clone()), None),
                Some(args),
                Some(cwd),
            ),
            (
                "a lineage of another birth",
                (Some(reborn.clone()), Some(lineage)),
                Some(args),
                Some(cwd),
            ),
            ("no arguments", both.clone(), None, Some(cwd)),
            ("no directory", both.clone(), Some(args), None),
        ];
        // The closing bracket reads what the opening read, so only the opening refuses.
        for (why, open, args, cwd) in openings {
            assert_eq!(
                read(open.clone(), args, cwd, open),
                None,
                "{why}: {judgment:?}"
            );
        }
    }
    // No kernel reads a pid past i32.
    let mut kernel = Scripted::default().then(both.clone(), Some(args), Some(cwd), both);
    assert_eq!(ExecSource::read(&mut kernel, u32::MAX), None);
}

/// A miss read of one process and then of a process that took its pid with the same facts is
/// `Unread`: no judgment of the second read's process counts the first read's facts, though the
/// two reads agree on the process and on the miss.
#[test]
fn a_miss_read_across_a_pid_reuse_is_unread() {
    use CaptureMiss::{NotExec, Unread, Unsupported};
    let root = aterm_tempfile::tempdir().unwrap();
    let (process, args) = facts(root.path());
    let first = lineage_of(&process);
    let mut reused = first;
    reused.uniqueid += 1;
    let mut not_exec = args.clone();
    not_exec.argv = vec!["codex".into(), "resume".into()];
    for (args, cwd, miss) in [
        (&not_exec, root.path(), NotExec),
        (&args, Path::new("relative"), Unsupported),
    ] {
        let at = |lineage: Lineage| (Some(process.clone()), Some(lineage));
        let classify = |opening: Lineage| {
            let mut kernel = Scripted::default()
                .then(
                    at(opening),
                    Some(args.clone()),
                    Some(cwd.into()),
                    at(reused),
                )
                .then(at(reused), Some(args.clone()), Some(cwd.into()), at(reused));
            ExecSource::judged(|| ExecSource::read(&mut kernel, process.pid))
        };
        assert_eq!(classify(reused), Err(miss));
        assert_eq!(classify(first), Err(Unread), "{miss:?}");
    }
}

/// A captured source carries the unique id the kernel gave it at the same read, and a source
/// whose unique id differs is not current.
#[test]
fn the_source_carries_its_lineage_uniqueid() {
    let root = aterm_tempfile::tempdir().unwrap();
    let cat = Cat::run(&["original-codex-name", "exec", "-"], root.path());
    let source = ExecSource::classify(cat.pid()).expect("a live exec run is a source");
    let lineage = aterm_uds::process::read_lineage(i32::try_from(cat.pid()).unwrap()).unwrap();
    assert_eq!(source.uniqueid, lineage.uniqueid);
    assert_ne!(source.uniqueid, 0);
    assert_eq!(source.exec_generation, lineage.identity.idversion);
    assert_eq!(ExecSource::capture(cat.pid()).as_ref(), Some(&source));
    assert!(source.current());
    let mut other = source.clone();
    other.uniqueid += 1;
    assert!(!other.current());
}
