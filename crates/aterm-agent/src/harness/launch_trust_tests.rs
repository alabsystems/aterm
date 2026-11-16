// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

//! Folder trust settled before a launch: the key Claude Code files a folder under, the skips
//! that start nothing, the one start that records, and what a start that records nothing leaves.

use std::path::{Path, PathBuf};
use std::sync::Mutex;
use std::time::Duration;

use super::*;
#[cfg(unix)]
use crate::supervise::config::Approve;

/// A test's wait for something that must happen: a hang detector, never a budget.
#[cfg(unix)]
const HANG: Duration = Duration::from_secs(60);

/// The line Claude Code 2.1.323 answered a recorded attestation with, in a lab's scratch home
/// (`targo --unverified lab launch-trust --answer <file>`), byte for byte but for its
/// `commands` array, emptied: 14 of its 18 kB, the vendor's slash-command catalog, one of whose
/// descriptions holds a word the export's content guard forbids in any shipped file.
const REAL: &str = include_str!(
    "fixtures/launch_trust/claude-2.1.323-initialize-workspace-trust-no-commands.stdout"
);

/// The answer Claude Code 2.1.323 gave a recorded attestation, cut to the fields this reads
/// (the measured line carries the session's commands, agents and models beside them).
const RECORDED: &str = r#"{"type":"control_response","response":{"subtype":"success","request_id":"aterm-trust","response":{"commands":[],"output_style":"default","pid":60684,"claude_code_version":"2.1.323","workspace_trust_recorded":true}}}"#;
/// Its answer for the home directory, a folder that resolves to another key, and a path that
/// is no directory.
const NOT_RECORDED: &str = r#"{"type":"control_response","response":{"subtype":"success","request_id":"aterm-trust","response":{"commands":[],"pid":61567,"claude_code_version":"2.1.323","workspace_trust_recorded":false}}}"#;
/// Its answer where it took no attestation (a relative directory, `accepted: false`, none sent).
const IGNORED: &str = r#"{"type":"control_response","response":{"subtype":"success","request_id":"aterm-trust","response":{"commands":[],"pid":58582,"claude_code_version":"2.1.323"}}}"#;

fn words(list: &[&str]) -> Vec<String> {
    list.iter().map(|w| (*w).to_string()).collect()
}

/// A scratch world: a user home, a Claude home, a folder to launch in and a build's path, all
/// under one resolved root.
struct World {
    _tmp: aterm_tempfile::TempDir,
    root: PathBuf,
    home: PathBuf,
    claude_home: PathBuf,
    folder: PathBuf,
    build: PathBuf,
}

impl World {
    fn new(tag: &str) -> Self {
        let tmp = aterm_tempfile::Builder::new()
            .prefix(&format!("aterm-launch-trust-{tag}-"))
            .tempdir()
            .expect("a scratch root");
        let root = std::fs::canonicalize(tmp.path()).expect("resolved");
        let (home, claude_home, folder) = (
            root.join("home"),
            root.join("claude-home"),
            root.join("work"),
        );
        for dir in [&home, &claude_home, &folder] {
            std::fs::create_dir_all(dir).expect("a folder");
        }
        Self {
            build: root.join("claude"),
            _tmp: tmp,
            root,
            home,
            claude_home,
            folder,
        }
    }

    fn env(&self) -> Vec<(String, String)> {
        vec![
            (
                "CLAUDE_CONFIG_DIR".to_string(),
                self.claude_home.to_string_lossy().into_owned(),
            ),
            ("PATH".to_string(), "/usr/bin:/bin".to_string()),
            (
                "STUB_OUT".to_string(),
                self.root.to_string_lossy().into_owned(),
            ),
        ]
    }

    fn profile(&self) -> PathBuf {
        self.claude_home.join(".claude.json")
    }

    fn launch<'a>(&'a self, argv: &'a [String], env: &'a [(String, String)]) -> Launch<'a> {
        Launch {
            argv,
            cwd: &self.folder,
            env,
            account_home: Some(&self.home),
            user_home: &self.home,
            no_harness: false,
            no_terminal: false,
            build: Some(&self.build),
        }
    }
}

/// A profile that records trust for each of `keys`, as Claude Code writes an entry.
fn profile_trusting(keys: &[&Path]) -> String {
    let entries: Vec<String> = keys
        .iter()
        .map(|key| {
            format!(
                "{}:{{\"allowedTools\":[],\"hasTrustDialogAccepted\":true,\
                 \"hasClaudeMdExternalIncludesApproved\":false}}",
                crate::supervise::journal::json_str(&key.to_string_lossy())
            )
        })
        .collect();
    format!(
        "{{\"numStartups\":3,\"projects\":{{{}}}}}\n",
        entries.join(",")
    )
}

/// One headless start a [`Fake`] was asked for.
#[derive(Clone, Debug, PartialEq, Eq)]
struct Call {
    build: PathBuf,
    cwd: PathBuf,
    env: Vec<(String, String)>,
    directory: String,
    deadline: Duration,
}

/// A vendor that starts nothing: it notes each ask, writes the profile Claude Code would (where
/// `writes` names one) and answers `says`.
struct Fake {
    calls: Mutex<Vec<Call>>,
    writes: Option<PathBuf>,
    says: Attested,
}

impl Fake {
    fn recording(profile: &Path) -> Self {
        Self {
            calls: Mutex::new(Vec::new()),
            writes: Some(profile.to_path_buf()),
            says: Attested::Recorded {
                version: "2.1.323".to_string(),
            },
        }
    }

    fn saying(says: Attested) -> Self {
        Self {
            calls: Mutex::new(Vec::new()),
            writes: None,
            says,
        }
    }

    fn calls(&self) -> Vec<Call> {
        self.calls.lock().expect("calls").clone()
    }
}

impl Vendor for Fake {
    fn attest(
        &self,
        build: &Path,
        cwd: &Path,
        env: &[(String, String)],
        directory: &str,
        deadline: Duration,
    ) -> Attested {
        self.calls.lock().expect("calls").push(Call {
            build: build.to_path_buf(),
            cwd: cwd.to_path_buf(),
            env: env.to_vec(),
            directory: directory.to_string(),
            deadline,
        });
        if let Some(profile) = &self.writes {
            std::fs::write(profile, profile_trusting(&[&trust_key(cwd)])).expect("the profile");
        }
        self.says.clone()
    }
}

/// What a [`Book`] was told.
#[derive(Default)]
struct Notes {
    recorded: Vec<(String, String, String)>,
    refused: Vec<(String, String)>,
}

impl Book for Notes {
    fn recorded(&mut self, rule: &str, folder: &str, by: &str) {
        self.recorded
            .push((rule.to_string(), folder.to_string(), by.to_string()));
    }
    fn refused(&mut self, folder: &str, why: &str) {
        self.refused.push((folder.to_string(), why.to_string()));
    }
}

fn trusts(_: &Path) -> Gate {
    Gate::Trusts(approval::RULE_TRUST_ANY)
}

/// A repository root, by hand: a folder holding a `.git` directory.
fn repo(at: &Path) {
    std::fs::create_dir_all(at.join(".git")).expect("a repository");
}

/// A linked worktree of `main` at `at`, by hand, as `git worktree add` lays one: the worktree's
/// `.git` file names `main/.git/worktrees/<name>`, whose `commondir` names `main/.git` and
/// whose `gitdir` points back at the worktree's `.git`.
fn worktree(main: &Path, at: &Path, name: &str) {
    let admin = main.join(".git/worktrees").join(name);
    std::fs::create_dir_all(&admin).expect("the worktree's git directory");
    std::fs::create_dir_all(at).expect("the worktree");
    std::fs::write(admin.join("commondir"), "../..\n").expect("commondir");
    std::fs::write(
        admin.join("gitdir"),
        format!("{}\n", at.join(".git").display()),
    )
    .expect("gitdir");
    std::fs::write(at.join(".git"), format!("gitdir: {}\n", admin.display())).expect(".git");
}

/// THE KEY IS THE ONE CLAUDE CODE FILES THE FOLDER UNDER (each kind measured on 2.1.323): a
/// repository's root and any folder inside it are the root's; a linked worktree, here under a
/// `.ubd/*.noindex` parent as aterm lays them, is its main checkout's; a submodule is its own;
/// a folder outside any repository is its own, even below a trusted one.
#[test]
fn the_trust_key_is_the_folder_claude_code_files_the_trust_under() {
    let w = World::new("key");
    let main = w.root.join("main");
    repo(&main);
    let deep = main.join("crates/deep");
    std::fs::create_dir_all(&deep).expect("a subfolder");
    assert_eq!(trust_key(&main), main);
    assert_eq!(trust_key(&deep), main, "a subfolder is its repository's");

    let linked = w.root.join(".ubd/main-fix.noindex");
    worktree(&main, &linked, "main-fix");
    assert_eq!(trust_key(&linked), main, "a linked worktree is its main's");
    let inside = linked.join("src");
    std::fs::create_dir_all(&inside).expect("a subfolder");
    assert_eq!(trust_key(&inside), main);

    let sub = main.join("vendor/sub");
    std::fs::create_dir_all(main.join(".git/modules/sub")).expect("the module's git directory");
    std::fs::create_dir_all(&sub).expect("the submodule");
    std::fs::write(sub.join(".git"), "gitdir: ../../.git/modules/sub\n").expect(".git");
    assert_eq!(trust_key(&sub), sub, "a submodule is its own");

    let plain = w.root.join("plain/child");
    std::fs::create_dir_all(&plain).expect("a plain folder");
    assert_eq!(trust_key(&plain), plain, "no repository: the folder itself");

    // A symlinked folder is read by what it resolves to, as `settle` resolves a launch's.
    #[cfg(unix)]
    {
        let link = w.root.join("link");
        std::os::unix::fs::symlink(&deep, &link).expect("a link");
        assert_eq!(
            trust_key(&std::fs::canonicalize(&link).expect("resolved")),
            main
        );
    }
}

/// A `.git` FILE THAT ONLY CLAIMS TO BE A WORKTREE IS ITS OWN ROOT: one that names another
/// checkout's worktree directory, whose `gitdir` does not point back at it, would otherwise
/// borrow that checkout's trust. So is one whose git directory is not in a `worktrees/`, whose
/// `commondir` is missing, or that names nothing. CONTROL: the worktree the directory does
/// point back at is its main checkout's.
#[test]
fn a_forged_git_file_borrows_no_other_folders_trust() {
    let w = World::new("forged");
    let main = w.root.join("main");
    repo(&main);
    let real = w.root.join("real");
    worktree(&main, &real, "real");
    assert_eq!(trust_key(&real), main, "the control");

    let admin = main.join(".git/worktrees/real");
    let forged = w.root.join("forged");
    std::fs::create_dir_all(&forged).expect("a folder");
    std::fs::write(
        forged.join(".git"),
        format!("gitdir: {}\n", admin.display()),
    )
    .expect(".git");
    assert_eq!(trust_key(&forged), forged, "no pointer back");

    let elsewhere = w.root.join("elsewhere");
    let loose = main.join(".git/loose");
    std::fs::create_dir_all(&loose).expect("a git directory outside worktrees/");
    std::fs::create_dir_all(&elsewhere).expect("a folder");
    std::fs::write(loose.join("commondir"), "..\n").expect("commondir");
    std::fs::write(
        loose.join("gitdir"),
        format!("{}\n", elsewhere.join(".git").display()),
    )
    .expect("gitdir");
    std::fs::write(
        elsewhere.join(".git"),
        format!("gitdir: {}\n", loose.display()),
    )
    .expect(".git");
    assert_eq!(trust_key(&elsewhere), elsewhere, "not in a worktrees/");

    for (name, text) in [("empty", ""), ("words", "not a git file\n")] {
        let odd = w.root.join(name);
        std::fs::create_dir_all(&odd).expect("a folder");
        std::fs::write(odd.join(".git"), text).expect(".git");
        assert_eq!(trust_key(&odd), odd, "{name}");
    }
    let gone = w.root.join("gone");
    std::fs::create_dir_all(&gone).expect("a folder");
    std::fs::write(
        gone.join(".git"),
        format!("gitdir: {}\n", main.join(".git/worktrees/none").display()),
    )
    .expect(".git");
    assert_eq!(trust_key(&gone), gone, "no commondir");
}

/// ONLY THE EXACT KEY IS A RECORD: an entry for the key with `hasTrustDialogAccepted: true`.
/// A trusted folder above it, an entry that says `false` or holds no flag, a profile that is
/// missing or is not JSON, and a `projects` that is no object are none.
#[test]
fn only_the_exact_key_with_the_flag_set_is_a_record() {
    let w = World::new("recorded");
    let profile = w.profile();
    assert!(!recorded(&profile, &w.folder), "no profile");
    std::fs::write(&profile, profile_trusting(&[&w.folder])).expect("the profile");
    assert!(recorded(&profile, &w.folder));
    assert!(
        !recorded(&profile, &w.folder.join("child")),
        "a folder below"
    );
    std::fs::write(&profile, profile_trusting(&[&w.root])).expect("the profile");
    assert!(!recorded(&profile, &w.folder), "a trusted folder above");
    for text in [
        format!(
            "{{\"projects\":{{{}:{{\"hasTrustDialogAccepted\":false}}}}}}",
            crate::supervise::journal::json_str(&w.folder.to_string_lossy())
        ),
        format!(
            "{{\"projects\":{{{}:{{\"allowedTools\":[]}}}}}}",
            crate::supervise::journal::json_str(&w.folder.to_string_lossy())
        ),
        format!(
            "{{\"projects\":{{{}:\"yes\"}}}}",
            crate::supervise::journal::json_str(&w.folder.to_string_lossy())
        ),
        "{\"projects\":[]}".to_string(),
        "{\"projects\":".to_string(),
        String::new(),
    ] {
        std::fs::write(&profile, &text).expect("the profile");
        assert!(!recorded(&profile, &w.folder), "{text}");
    }
}

/// A KEY THAT IS NOT ASCII IS READ BY THE FOLDER IT NAMES: Claude Code spells its key in
/// Unicode's composed form, and a disk may hand back the decomposed one, so the two spellings of
/// one folder are one record where the disk reads them as one folder. CONTROL: a key of another
/// folder, and a spelling the disk reads as no folder, are none.
#[test]
fn a_key_that_is_not_ascii_is_read_by_the_folder_it_names() {
    let w = World::new("unicode");
    let (composed, decomposed) = (w.root.join("caf\u{e9}"), w.root.join("cafe\u{301}"));
    std::fs::create_dir_all(&decomposed).expect("a folder");
    let other = w.root.join("th\u{e9}");
    std::fs::create_dir_all(&other).expect("another folder");
    let profile = w.profile();
    std::fs::write(&profile, profile_trusting(&[&other])).expect("the profile");
    assert!(!recorded(&profile, &decomposed), "another folder's key");
    std::fs::write(&profile, profile_trusting(&[&composed])).expect("the profile");
    // A disk that keeps the two spellings apart (Linux) holds no folder under the composed one.
    assert_eq!(recorded(&profile, &decomposed), composed.is_dir());
    std::fs::write(&profile, profile_trusting(&[&decomposed])).expect("the profile");
    assert!(recorded(&profile, &decomposed), "the exact spelling");
}

/// THE ANSWER IS READ FROM THE ONE LINE THAT ANSWERS THE REQUEST, and only
/// `workspace_trust_recorded: true` in a success is a recorded trust (the three shapes measured
/// on 2.1.323). Lines that are no answer to this request are passed over. The real build's
/// answer for a folder outside any repository (`REAL`, kept by `lab launch-trust --answer`)
/// reads as its trimmed shape does.
#[test]
fn only_a_true_workspace_trust_recorded_is_a_recorded_trust() {
    for answer in [RECORDED, REAL] {
        assert_eq!(
            answer_of(answer),
            Some(Attested::Recorded {
                version: "2.1.323".to_string()
            })
        );
    }
    // Claude Code's own answer that it records nothing is a decline; a line that names the
    // request and cannot be read is no answer of its.
    let declined = |line: &str| match answer_of(line) {
        Some(Attested::Declined(why)) => why,
        other => panic!("{line}: {other:?}"),
    };
    assert!(declined(NOT_RECORDED).contains("Claude Code 2.1.323 recorded nothing"));
    assert!(declined(IGNORED).contains("does not take the attestation"));
    assert!(
        declined(
            r#"{"type":"control_response","response":{"subtype":"error","request_id":"aterm-trust","error":"no"}}"#
        )
        .contains("refused the request")
    );
    assert!(matches!(
        answer_of(r#"{"type":"control_response","response":{"request_id":"aterm-trust","#),
        Some(Attested::Failed(why)) if why.contains("could not be read")
    ));
    // A string where the flag belongs is no `true`, and no answer this reads.
    assert!(matches!(
        answer_of(
            r#"{"type":"control_response","response":{"subtype":"success","request_id":"aterm-trust","response":{"workspace_trust_recorded":"true"}}}"#
        ),
        Some(Attested::Failed(_))
    ));
    for other in [
        "",
        "starting up",
        r#"{"type":"system","subtype":"init","session_id":"x"}"#,
        r#"{"type":"control_response","response":{"subtype":"success","request_id":"another","response":{"workspace_trust_recorded":true}}}"#,
        r#"{"type":"control_request","request_id":"aterm-trust","request":{"subtype":"can_use_tool"}}"#,
    ] {
        assert_eq!(answer_of(other), None, "{other}");
    }
}

/// THE REQUEST IS ONE LINE: an `initialize` whose `workspaceTrust` attests the folder, its path
/// escaped as JSON.
#[test]
fn the_request_is_one_initialize_line_attesting_the_folder() {
    assert_eq!(
        request_line("/w/a \"b\""),
        "{\"type\":\"control_request\",\"request_id\":\"aterm-trust\",\"request\":{\"subtype\":\
         \"initialize\",\"workspaceTrust\":{\"accepted\":true,\"directory\":\"/w/a \\\"b\\\"\"}}}\n"
    );
}

/// AN UNTRUSTED FOLDER THE POLICY TRUSTS IS SETTLED BY ONE START: the build is started once, in
/// the folder, under the launch's own environment, attesting the resolved folder; the trust it
/// recorded is booked under the gate's rule, as recorded by that Claude Code. The launch after
/// it finds the record and starts nothing.
#[test]
fn an_untrusted_folder_is_settled_by_one_start_and_booked() {
    let w = World::new("settle");
    let (argv, env) = (words(&["claude", "--model", "haiku"]), w.env());
    let vendor = Fake::recording(&w.profile());
    let mut notes = Notes::default();
    let settled = settle(
        &w.launch(&argv, &env),
        &trusts,
        &vendor,
        &mut notes,
        DEADLINE,
    );
    let by = "Claude Code 2.1.323".to_string();
    assert_eq!(
        settled,
        Settled::Recorded {
            rule: approval::RULE_TRUST_ANY,
            by: by.clone()
        }
    );
    let folder = w.folder.to_string_lossy().into_owned();
    assert_eq!(
        vendor.calls(),
        [Call {
            build: w.build.clone(),
            cwd: w.folder.clone(),
            env: env.clone(),
            directory: folder.clone(),
            deadline: DEADLINE,
        }]
    );
    assert_eq!(
        notes.recorded,
        [(approval::RULE_TRUST_ANY.to_string(), folder, by)]
    );
    assert!(notes.refused.is_empty());

    let again = settle(
        &w.launch(&argv, &env),
        &|_| panic!("a recorded folder asks no gate"),
        &vendor,
        &mut notes,
        DEADLINE,
    );
    assert_eq!(again, Settled::Already);
    assert_eq!(vendor.calls().len(), 1, "no second start");
    assert_eq!(notes.recorded.len(), 1, "nothing booked twice");
}

/// [`settle`] in `w` under `gate`, asserting it started nothing, booked nothing and wrote no
/// profile.
fn settled_starting_nothing(
    w: &World,
    launch: &Launch<'_>,
    gate: &dyn Fn(&Path) -> Gate,
) -> Settled {
    let vendor = Fake::recording(&w.profile());
    let mut notes = Notes::default();
    let settled = settle(launch, gate, &vendor, &mut notes, DEADLINE);
    assert!(vendor.calls().is_empty(), "{settled:?} started the build");
    assert!(notes.recorded.is_empty() && notes.refused.is_empty());
    assert!(!w.profile().exists(), "{settled:?} wrote a profile");
    settled
}

/// NOTHING IS STARTED where there is nothing to settle: a launch that prints and exits (`-p`,
/// `--print`), a subcommand, a line that does not parse, a launch with no terminal on its output,
/// the harness off for the launch, and the home directory, the account's or the one the launch's
/// own home variable names (`HOME`; `USERPROFILE` on Windows, which Claude Code's `os.homedir()`
/// reads there). None of them asks the gate or is journaled: each is the launch's own. CONTROL:
/// the same world with none of those starts the build
/// ([`an_untrusted_folder_is_settled_by_one_start_and_booked`]).
#[test]
fn nothing_is_started_where_there_is_nothing_to_settle() {
    let w = World::new("skips");
    let env = w.env();
    let run = |launch: &Launch<'_>, gate: &dyn Fn(&Path) -> Gate| {
        settled_starting_nothing(&w, launch, gate)
    };
    for line in [
        &["claude", "-p", "hello there"][..],
        &["claude", "--print", "hello there"][..],
        &["claude", "mcp", "list"][..],
        &["claude", "doctor"][..],
        &["claude", "--no-such-flag-of-any-build"][..],
        &["claude", "--bare"][..],
    ] {
        let argv = words(line);
        assert_eq!(
            run(&w.launch(&argv, &env), &|_| panic!("asked the gate")),
            Settled::NotInteractive,
            "{line:?}"
        );
    }
    // A `--settings` of the line's own bars the usage tap's value and nothing of the folder's
    // trust: the line is still an interactive launch.
    let own = words(&["claude", "--settings", "{}"]);
    assert!(super::super::tap::launch_interactive(&own));
    assert!(!super::super::tap::launch_admits(&own));
    let argv = words(&["claude"]);
    assert_eq!(
        run(
            &Launch {
                no_harness: true,
                ..w.launch(&argv, &env)
            },
            &|_| panic!("asked the gate")
        ),
        Settled::NotInteractive
    );
    // No terminal on the launch's output: Claude Code prints and exits there, whatever its
    // arguments say.
    assert_eq!(
        run(
            &Launch {
                no_terminal: true,
                ..w.launch(&argv, &env)
            },
            &|_| panic!("asked the gate")
        ),
        Settled::NotInteractive
    );
    // The home directory: the account's, and the one the launch's own home variable names.
    assert_eq!(
        run(
            &Launch {
                cwd: &w.home,
                ..w.launch(&argv, &env)
            },
            &|_| panic!("asked the gate")
        ),
        Settled::Home
    );
    let mut at_home = env.clone();
    at_home.push((
        super::super::accounts::HOMEDIR_ENV.to_string(),
        w.folder.to_string_lossy().into_owned(),
    ));
    assert_eq!(
        run(&w.launch(&argv, &at_home), &|_| panic!("asked the gate")),
        Settled::Home
    );
}

/// NOR WHERE THE POLICY OR THE SESSION REFUSES BEFORE ANY FOLDER IS JUDGED: Autopilot off (the
/// table's, and the session's own word over a table that is on), the harness off, off for Claude
/// Code, and a session a hold stands on (under the window's own host too) or another supervisor
/// claims. None of them is journaled: each is the policy's own, and each names its reason.
/// CONTROL: the default policy, over a session with no word of its own, no hold and no
/// supervisor but a window's own host, answers as the folder judgment does ([`gate`] over no
/// session's facts), which names none of those reasons: it trusts the folder on unix
/// ([`nothing_is_started_where_the_policy_gives_no_leave`]) and none on Windows
/// (ATERM-KI-1115-31415).
#[test]
fn nothing_is_started_where_the_policy_or_the_session_refuses_before_any_folder() {
    let w = World::new("refuses");
    let env = w.env();
    let run = |launch: &Launch<'_>, gate: &dyn Fn(&Path) -> Gate| {
        settled_starting_nothing(&w, launch, gate)
    };
    let argv = words(&["claude"]);
    let policy = |config: &SupervisorConfig, word: Option<Autopilot>| {
        gate(
            &Policy {
                config,
                autopilot: word,
                trusted_cwd: None,
                home: Some(&w.home),
                uid: 502,
            },
            &w.folder,
        )
    };
    let on = SupervisorConfig::default();
    let judged = policy(&on, None);
    let off = "the harness is off for Claude Code";
    for (what, config, word, why) in [
        (
            "autopilot off",
            SupervisorConfig {
                turn_end: alab_policy::config::PolicyConfig {
                    autopilot: Autopilot::Off,
                    ..on.turn_end.clone()
                },
                ..on.clone()
            },
            None,
            "Autopilot is off",
        ),
        (
            "the session's word off",
            on.clone(),
            Some(Autopilot::Off),
            "Autopilot is off",
        ),
        (
            "harness off",
            SupervisorConfig {
                enabled: false,
                ..on.clone()
            },
            None,
            off,
        ),
        (
            "off for Claude Code",
            SupervisorConfig {
                off: vec![aterm_phase::Program::Claude],
                ..on.clone()
            },
            None,
            off,
        ),
    ] {
        assert_eq!(policy(&config, word), Gate::Refuses(why), "{what}");
        assert_ne!(
            judged,
            Gate::Refuses(why),
            "the control names no reason of {what}"
        );
        assert_eq!(
            run(&w.launch(&argv, &env), &|_| policy(&config, word)),
            Settled::Gated(why),
            "{what}"
        );
    }
    // The session, as its window says it stands: a hold on it (a person took the tab over, a
    // local or fleet halt), and a supervisor that is no window's own host.
    let seen = |hold: &str, supervisor: &str| Seen {
        meta: "OK cwd=%2Fw trusted_cwd=- autopilot=-\n".to_string(),
        status: format!("OK schema=1 hold={hold} supervisor={supervisor} autopilot=cruise\n"),
    };
    let session = |seen: &Seen| session_gate(&on, Some(seen), Some(&w.home), 502, &w.folder);
    for (hold, supervisor) in [("0", "-"), ("0", "aterm-harness@4242")] {
        assert_eq!(
            session(&seen(hold, supervisor)),
            judged,
            "the control: hold={hold} supervisor={supervisor}"
        );
    }
    for (what, at, why) in [
        ("a hold", seen("1", "-"), "a hold stands on the session"),
        (
            "a hold under the window's own host",
            seen("1", "aterm-harness@4242"),
            "a hold stands on the session",
        ),
        (
            "another supervisor",
            seen("0", "codex-alab@77"),
            "another supervisor claims the session",
        ),
    ] {
        assert_eq!(session(&at), Gate::Refuses(why), "{what}");
        assert_ne!(
            judged,
            Gate::Refuses(why),
            "the control names no reason of {what}"
        );
        assert_eq!(
            run(&w.launch(&argv, &env), &|_| session(&at)),
            Settled::Gated(why),
            "{what}"
        );
    }
}

/// NOR WHERE THE POLICY TRUSTS NO FOLDER HERE: `approve = "none"`, and `approve = "safe"` with
/// no trust root over the folder; nor where the folder is already recorded. None of them is
/// journaled: each is the policy's own. CONTROLS: the default policy trusts the folder, and so
/// does a session whose own word is on over a table that is off. Unix's: the folder judgment
/// ([`approval::trust_folder_rule`]) reads `/`-rooted folders alone, as the folder-trust
/// dialog's own reading does, and the key a record is read under is spelled as Unix spells it,
/// so on Windows no folder is trusted (ATERM-KI-1115-31415); the refusals made before any folder
/// is judged run on every platform
/// ([`nothing_is_started_where_the_policy_or_the_session_refuses_before_any_folder`]).
#[cfg(unix)]
#[test]
fn nothing_is_started_where_the_policy_gives_no_leave() {
    let w = World::new("leave");
    let env = w.env();
    let run = |launch: &Launch<'_>, gate: &dyn Fn(&Path) -> Gate| {
        settled_starting_nothing(&w, launch, gate)
    };
    let argv = words(&["claude"]);
    // The policy: `approve = "none"`, and `approve = "safe"` with no root over the folder.
    let policy = |config: &SupervisorConfig, word: Option<Autopilot>| {
        gate(
            &Policy {
                config,
                autopilot: word,
                trusted_cwd: None,
                home: Some(&w.home),
                uid: 502,
            },
            &w.folder,
        )
    };
    let on = SupervisorConfig::default();
    assert_eq!(
        policy(&on, None),
        Gate::Trusts(approval::RULE_TRUST_ANY),
        "the control: the default policy trusts the folder"
    );
    for (what, config, word) in [
        (
            "approve none",
            SupervisorConfig {
                approve: Approve::None,
                ..on.clone()
            },
            None,
        ),
        (
            "approve safe, no root",
            SupervisorConfig {
                approve: Approve::Safe,
                ..on.clone()
            },
            None,
        ),
    ] {
        let said = policy(&config, word);
        let Gate::Refuses(why) = said else {
            panic!("{what}: {said:?}");
        };
        assert_eq!(
            run(&w.launch(&argv, &env), &|_| policy(&config, word)),
            Settled::Gated(why),
            "{what}"
        );
    }
    // A session whose own word is on, over a table that is off, is settled as its dialog is
    // answered.
    assert_eq!(
        policy(
            &SupervisorConfig {
                turn_end: alab_policy::config::PolicyConfig {
                    autopilot: Autopilot::Off,
                    ..on.turn_end.clone()
                },
                ..on.clone()
            },
            Some(Autopilot::Land)
        ),
        Gate::Trusts(approval::RULE_TRUST_ANY)
    );
    // A folder already recorded: no gate asked, nothing started.
    std::fs::write(w.profile(), profile_trusting(&[&w.folder])).expect("the profile");
    let vendor = Fake::recording(&w.profile());
    let mut notes = Notes::default();
    assert_eq!(
        settle(
            &w.launch(&argv, &env),
            &|_| panic!("asked the gate"),
            &vendor,
            &mut notes,
            DEADLINE
        ),
        Settled::Already
    );
    assert!(vendor.calls().is_empty());
}

/// THE GATE'S RULE IS THE DIALOG'S: under `approve = "safe"` a folder under a trust root, or the
/// one the session's starter trusted, is settled under the dialog's safe rule, and under
/// `approve = "all"` any other folder under the full-power one, so the row booked names the rule
/// that would have answered the dialog (the table itself is pinned against the dialog's
/// decision in the policy's own tests). Unix's: on Windows the folder judgment trusts no folder
/// (ATERM-KI-1115-31415).
#[cfg(unix)]
#[test]
fn the_gate_names_the_rule_the_dialog_would_be_answered_under() {
    let w = World::new("gate");
    let rooted = SupervisorConfig {
        approve: Approve::Safe,
        trust_roots: vec![format!("{}/wor*", w.root.display())],
        ..SupervisorConfig::default()
    };
    let ask = |config: &SupervisorConfig, trusted: Option<&Path>| {
        gate(
            &Policy {
                config,
                autopilot: None,
                trusted_cwd: trusted,
                home: Some(&w.home),
                uid: 502,
            },
            &w.folder,
        )
    };
    assert_eq!(
        ask(&rooted, None),
        Gate::Trusts(approval::RULE_TRUST_DIALOG)
    );
    let safe = SupervisorConfig {
        approve: Approve::Safe,
        ..SupervisorConfig::default()
    };
    assert_eq!(
        ask(&safe, Some(&w.folder)),
        Gate::Trusts(approval::RULE_TRUST_DIALOG)
    );
    assert!(matches!(ask(&safe, Some(&w.root)), Gate::Refuses(_)));
    assert_eq!(
        ask(&SupervisorConfig::default(), None),
        Gate::Trusts(approval::RULE_TRUST_ANY)
    );
}

/// A TRUSTED FOLDER ABOVE IS NO RECORD: Claude Code files a folder outside any repository under
/// itself even below a trusted one, so the build is still started for it.
#[test]
fn a_trusted_folder_above_still_starts_the_build() {
    let w = World::new("ancestor");
    std::fs::write(w.profile(), profile_trusting(&[&w.root])).expect("the profile");
    let (argv, env) = (words(&["claude"]), w.env());
    let vendor = Fake::recording(&w.profile());
    let mut notes = Notes::default();
    let settled = settle(
        &w.launch(&argv, &env),
        &trusts,
        &vendor,
        &mut notes,
        DEADLINE,
    );
    assert!(matches!(settled, Settled::Recorded { .. }), "{settled:?}");
    assert_eq!(vendor.calls().len(), 1);
}

/// A START THAT RECORDS NOTHING BOOKS NO APPROVAL: a refusal, and an answer of "recorded" the
/// profile does not bear out, are journaled with their reason and leave no ledger row, so the
/// dialog's own rule answers and books it.
#[test]
fn a_start_that_records_nothing_books_no_approval() {
    let w = World::new("refused");
    let (argv, env) = (words(&["claude"]), w.env());
    let folder = w.folder.to_string_lossy().into_owned();
    for (says, why) in [
        (
            answer_of(NOT_RECORDED).expect("an answer"),
            "recorded nothing for this folder",
        ),
        (
            answer_of(IGNORED).expect("an answer"),
            "does not take the attestation",
        ),
        (
            Attested::Recorded {
                version: "2.1.323".to_string(),
            },
            "holds none for",
        ),
    ] {
        let vendor = Fake::saying(says);
        let mut notes = Notes::default();
        let settled = settle(
            &w.launch(&argv, &env),
            &trusts,
            &vendor,
            &mut notes,
            DEADLINE,
        );
        let Settled::Refused(said) = &settled else {
            panic!("{settled:?}");
        };
        assert!(said.contains(why), "{said}");
        assert_eq!(vendor.calls().len(), 1);
        assert!(notes.recorded.is_empty(), "{why}: booked an approval");
        assert_eq!(notes.refused, [(folder.clone(), said.clone())]);
    }
    // An earlier answer that still stands started nothing now: its reason was journaled when
    // it was given, and is not journaled again at every plan of a relaunch pass.
    let vendor = Fake::saying(Attested::Stands("not asked again".to_string()));
    let mut notes = Notes::default();
    assert_eq!(
        settle(
            &w.launch(&argv, &env),
            &trusts,
            &vendor,
            &mut notes,
            DEADLINE
        ),
        Settled::Refused("not asked again".to_string())
    );
    assert!(notes.recorded.is_empty() && notes.refused.is_empty());
}

/// A STEP SKIPPED BECAUSE SOMETHING COULD NOT BE READ IS JOURNALED WITH ITS REASON, so a launch
/// whose dialog drew all the same leaves a line that says why nothing was settled: a session
/// whose window did not answer, and a launch whose build could not be resolved. Nothing is
/// started and no approval booked. CONTROL: a policy's refusal journals nothing
/// ([`nothing_is_started_where_there_is_nothing_to_settle`]).
#[test]
fn a_step_skipped_unread_is_journaled_with_its_reason() {
    let w = World::new("unread");
    let (argv, env) = (words(&["claude"]), w.env());
    let folder = w.folder.to_string_lossy().into_owned();
    let on = SupervisorConfig::default();
    let unanswered = |cwd: &Path| session_gate(&on, None, Some(&w.home), 502, cwd);
    let no_build = Launch {
        build: None,
        ..w.launch(&argv, &env)
    };
    for (launch, gate, why) in [
        (
            w.launch(&argv, &env),
            &unanswered as &dyn Fn(&Path) -> Gate,
            "the session's hold and Autopilot word could not be read",
        ),
        (
            no_build,
            &trusts as &dyn Fn(&Path) -> Gate,
            "the launch's build could not be resolved",
        ),
    ] {
        let vendor = Fake::recording(&w.profile());
        let mut notes = Notes::default();
        assert_eq!(
            settle(&launch, gate, &vendor, &mut notes, DEADLINE),
            Settled::Unread(why)
        );
        assert!(vendor.calls().is_empty(), "{why}: started the build");
        assert!(notes.recorded.is_empty(), "{why}: booked an approval");
        assert_eq!(notes.refused, [(folder.clone(), why.to_string())]);
    }
}

/// THE SESSION'S BOOKS: a recorded trust is one approved row of the session's ledger, under the
/// rule, the folder its subject, who recorded it in its reason, and a journal line; a refusal is
/// a journal line and no row.
#[test]
fn a_recorded_trust_is_one_approved_row_of_the_sessions_ledger() {
    let w = World::new("book");
    let state = w.root.join("state");
    let mut book = DriveBook {
        state: Some(state.clone()),
        sid: "s-0123".to_string(),
    };
    let ledger = state.join("drive/s-0123.jsonl");
    let journal = state.join("drive/s-0123.journal.jsonl");
    book.refused(
        "/w/elsewhere",
        "Claude Code 2.1.323 recorded nothing for this folder",
    );
    assert!(!ledger.exists(), "a refusal books no row");
    book.recorded(approval::RULE_TRUST_ANY, "/w/work", "Claude Code 2.1.323");
    let rows = std::fs::read_to_string(&ledger).expect("the ledger");
    let rows: Vec<&str> = rows.lines().collect();
    assert_eq!(rows.len(), 1, "{rows:?}");
    for part in [
        "\"sid\":\"s-0123\"",
        "\"rule_id\":\"trust-any@v1\"",
        "\"decision\":\"approved\"",
        "\"command\":\"/w/work\"",
        "\"reason\":\"recorded before launch by Claude Code 2.1.323\"",
        "\"box_seq\":0",
    ] {
        assert!(rows[0].contains(part), "{part}: {}", rows[0]);
    }
    let (noted, _) = crate::supervise::read_journal(&journal).expect("the journal");
    let lines: Vec<&str> = noted.iter().map(|record| record.line.as_str()).collect();
    assert_eq!(
        lines,
        [
            "HOST launch trust /w/elsewhere: not recorded before launch (Claude Code 2.1.323 \
             recorded nothing for this folder): its dialog is the rule's to answer",
            "HOST launch trust rule=trust-any@v1 /w/work: recorded before launch by Claude Code \
             2.1.323",
        ]
    );
}

/// MANY LAUNCHES IN ONE FOLDER AT ONCE each start the build at most once, and aterm writes no
/// byte of the profile: with a vendor that writes nothing, the profile the launches found is
/// the profile they leave.
#[test]
fn launches_at_once_start_at_most_one_build_each_and_write_no_profile() {
    const LAUNCHES: usize = 8;
    let w = World::new("parallel");
    let before = profile_trusting(&[&w.root]);
    std::fs::write(w.profile(), &before).expect("the profile");
    let (argv, env) = (words(&["claude"]), w.env());
    let vendor = Fake::saying(Attested::Recorded {
        version: "2.1.323".to_string(),
    });
    std::thread::scope(|scope| {
        for _ in 0..LAUNCHES {
            scope.spawn(|| {
                let mut notes = Notes::default();
                let settled = settle(
                    &w.launch(&argv, &env),
                    &trusts,
                    &vendor,
                    &mut notes,
                    DEADLINE,
                );
                assert!(matches!(settled, Settled::Refused(_)), "{settled:?}");
                assert!(notes.recorded.is_empty());
            });
        }
    });
    assert_eq!(vendor.calls().len(), LAUNCHES);
    assert_eq!(
        std::fs::read_to_string(w.profile()).expect("the profile"),
        before
    );
    let left: Vec<String> = std::fs::read_dir(&w.claude_home)
        .expect("the Claude home")
        .map(|entry| {
            entry
                .expect("an entry")
                .file_name()
                .to_string_lossy()
                .into_owned()
        })
        .collect();
    assert_eq!(left, [".claude.json"], "nothing beside the profile");
}

/// A window's `meta` reply naming the session's own Autopilot word and the folder its starter
/// trusted, each as the reply encodes it.
fn meta_reply(autopilot: &str, trusted: &str) -> String {
    format!(
        "OK title=x cwd=%2Fw state=idle agent_cwd=%2Fw trusted_cwd={trusted} worker=- \
         autopilot={autopilot} land_at=-\n"
    )
}

/// A window's `status` reply naming the hold on the session and its supervisor.
fn status_reply(hold: &str, supervisor: &str) -> String {
    format!(
        "OK schema=1 sid=s-0123 enabled=true hold={hold} hold_advisory=- fabric=absent \
         hold_by=- supervisor={supervisor} supervisor_act=- autopilot=cruise human_ms=12\n"
    )
}

/// THE SESSION'S FACTS ARE WHAT ITS WINDOW SAYS OF IT: from `meta`, its own Autopilot word (`-`:
/// none) and the folder its starter trusted, an absolute one as this platform spells it; from
/// `status`, the hold on it and whether a supervisor that is no window's own host claims it. A
/// reply that is no `OK`, a word this build does not know, or a `status` that names no `hold=`
/// is no reading.
#[test]
fn the_sessions_facts_are_read_from_its_meta_and_status_replies() {
    let seen = |meta: String, status: String| Seen { meta, status };
    let facts = |autopilot: &str, trusted: &str, hold: &str, supervisor: &str| {
        session_facts(&seen(
            meta_reply(autopilot, trusted),
            status_reply(hold, supervisor),
        ))
    };
    let clear = Facts {
        autopilot: None,
        trusted_cwd: None,
        held: false,
        claimed: false,
    };
    // The starter's folder, and the reply's spelling of it.
    let (trusted, encoded) = if cfg!(windows) {
        (r"C:\w\a b", r"C:\w\a%20b")
    } else {
        ("/w/a b", "%2Fw%2Fa%20b")
    };
    assert_eq!(facts("-", "-", "0", "-"), Some(clear.clone()));
    assert_eq!(
        facts("off", encoded, "0", "aterm-harness@19307"),
        Some(Facts {
            autopilot: Some(Autopilot::Off),
            trusted_cwd: Some(PathBuf::from(trusted)),
            ..clear.clone()
        }),
        "a window's own host is no other supervisor"
    );
    assert_eq!(
        facts("land", "relative", "1", "-"),
        Some(Facts {
            autopilot: Some(Autopilot::Land),
            held: true,
            ..clear.clone()
        })
    );
    assert_eq!(
        facts("-", "-", "0", "codex-alab%4077"),
        Some(Facts {
            claimed: true,
            ..clear.clone()
        }),
        "a supervisor that is no window's host"
    );
    assert_eq!(facts("someday", "-", "0", "-"), None, "an unknown word");
    for (what, unread) in [
        (
            "meta refused",
            seen("ERR no such session\n".to_string(), status_reply("0", "-")),
        ),
        ("no meta", seen(String::new(), status_reply("0", "-"))),
        (
            "status refused",
            seen(meta_reply("-", "-"), "ERR busy\n".to_string()),
        ),
        (
            "a status that names no hold",
            seen(
                meta_reply("-", "-"),
                "OK schema=1 supervisor=- autopilot=cruise\n".to_string(),
            ),
        ),
        (
            "a hold that says nothing",
            seen(meta_reply("-", "-"), status_reply("-", "-")),
        ),
    ] {
        assert_eq!(session_facts(&unread), None, "{what}");
    }
}

/// THE GATE OVER NO READING, OR OVER A SESSION WHOSE OWN WORD IS OFF, TRUSTS NOTHING
/// ([`session_gate`]), before any folder is judged: no reply, and a `meta` that is no `OK`, are
/// no reading ([`Gate::Unread`]), and a session whose own Autopilot word is off is refused.
/// CONTROL: a session with no word of its own, no hold and no other supervisor answers as the
/// folder judgment over its facts does ([`gate`]), which is neither: it trusts the folder on
/// unix ([`the_gate_over_the_sessions_facts_trusts_by_them`]) and none on Windows
/// (ATERM-KI-1115-31415).
#[test]
fn the_gate_over_no_reading_or_an_off_word_trusts_nothing() {
    let seen = |meta: String, status: String| Seen { meta, status };
    let w = World::new("off-word");
    let on = SupervisorConfig::default();
    let ask = |seen: Option<&Seen>| session_gate(&on, seen, Some(&w.home), 502, &w.folder);
    let judged = gate(
        &Policy {
            config: &on,
            autopilot: None,
            trusted_cwd: None,
            home: Some(&w.home),
            uid: 502,
        },
        &w.folder,
    );
    assert!(
        !matches!(judged, Gate::Unread(_) | Gate::Refuses("Autopilot is off")),
        "{judged:?}"
    );
    assert_eq!(
        ask(Some(&seen(meta_reply("-", "-"), status_reply("0", "-")))),
        judged,
        "the control"
    );
    assert_eq!(
        ask(Some(&seen(meta_reply("off", "-"), status_reply("0", "-")))),
        Gate::Refuses("Autopilot is off")
    );
    assert!(matches!(ask(None), Gate::Unread(_)), "no reply");
    assert!(
        matches!(
            ask(Some(&seen(
                "ERR busy\n".to_string(),
                status_reply("0", "-")
            ))),
            Gate::Unread(_)
        ),
        "a meta refused"
    );
}

/// THE GATE OVER THE SESSION'S FACTS TRUSTS BY THEM ([`session_gate`]): the default policy
/// trusts the folder of a session with no word of its own, and under `approve = "safe"` the
/// folder the session's starter trusted. Unix's: on Windows the folder judgment trusts no folder
/// (ATERM-KI-1115-31415); what the gate refuses before any folder is judged runs on every
/// platform ([`the_gate_over_no_reading_or_an_off_word_trusts_nothing`]).
#[cfg(unix)]
#[test]
fn the_gate_over_the_sessions_facts_trusts_by_them() {
    let seen = |meta: String, status: String| Seen { meta, status };
    let w = World::new("seen");
    let on = SupervisorConfig::default();
    let ask = |seen: Option<&Seen>| session_gate(&on, seen, Some(&w.home), 502, &w.folder);
    assert_eq!(
        ask(Some(&seen(meta_reply("-", "-"), status_reply("0", "-")))),
        Gate::Trusts(approval::RULE_TRUST_ANY)
    );
    let safe = SupervisorConfig {
        approve: Approve::Safe,
        ..on.clone()
    };
    let folder = aterm_codec::pct::encode(&w.folder.to_string_lossy());
    assert_eq!(
        session_gate(
            &safe,
            Some(&seen(meta_reply("-", &folder), status_reply("0", "-"))),
            Some(&w.home),
            502,
            &w.folder
        ),
        Gate::Trusts(approval::RULE_TRUST_DIALOG),
        "the folder the session's starter trusted"
    );
}

/// A RELAUNCH LINE IS SETTLED UNDER THE CLAUDE HOME IT NAMES: a line that moves a session to a
/// second Claude home is settled against that home's profile, with the build started under the
/// line's own variables and none of this process's AI variables. CONTROL: the same line under a
/// home that already records the folder starts nothing. Unix's: on Windows the folder judgment
/// trusts no folder, so nothing is started (ATERM-KI-1115-31415).
#[cfg(unix)]
#[test]
fn a_relaunch_line_is_settled_under_the_claude_home_it_names() {
    let w = World::new("relaunch");
    let second = w.root.join("second-home");
    std::fs::create_dir_all(&second).expect("the second Claude home");
    // The first home records the folder; the second holds no profile.
    std::fs::write(w.profile(), profile_trusting(&[&w.folder])).expect("the profile");
    let base = vec![
        (
            "CLAUDE_CONFIG_DIR".to_string(),
            w.claude_home.to_string_lossy().into_owned(),
        ),
        ("ANTHROPIC_API_KEY".to_string(), "not-a-key".to_string()),
        ("PATH".to_string(), "/usr/bin:/bin".to_string()),
    ];
    let config = SupervisorConfig::default();
    let at = Relaunch {
        tab: "s-0123",
        config: &config,
        base_env: &base,
        user_home: &w.home,
        account_home: Some(&w.home),
        uid: 502,
        state: None,
        build: Some(&w.build),
    };
    let argv = words(&["claude", "--resume", "0b5e7a11-f100-4a2b-9c3d-5e6f70819a2b"]);
    let folder = w.folder.to_string_lossy().into_owned();
    let second_home = second.to_string_lossy().into_owned();
    let seen = |tab: &str| {
        assert_eq!(tab, "s-0123");
        Some(Seen {
            meta: "OK cwd=%2Fw trusted_cwd=- autopilot=-\n".to_string(),
            status: "OK schema=1 hold=0 supervisor=aterm-harness@4242\n".to_string(),
        })
    };
    let vendor = Fake::recording(&second.join(".claude.json"));
    let settled = settle_relaunch(
        &at,
        &Line {
            argv: &argv,
            cwd: &folder,
            env: &[("CLAUDE_CONFIG_DIR", second_home.as_str())],
            exe: &w.build,
        },
        &vendor,
        &seen,
    );
    assert!(matches!(settled, Settled::Recorded { .. }), "{settled:?}");
    let calls = vendor.calls();
    assert_eq!(calls.len(), 1);
    assert_eq!(calls[0].build, w.build);
    assert_eq!(
        calls[0].env,
        [
            ("PATH".to_string(), "/usr/bin:/bin".to_string()),
            ("CLAUDE_CONFIG_DIR".to_string(), second_home),
        ]
    );
    assert!(recorded(&second.join(".claude.json"), &w.folder));

    let vendor = Fake::recording(&w.profile());
    let first_home = w.claude_home.to_string_lossy().into_owned();
    let settled = settle_relaunch(
        &at,
        &Line {
            argv: &argv,
            cwd: &folder,
            env: &[("CLAUDE_CONFIG_DIR", first_home.as_str())],
            exe: &w.build,
        },
        &vendor,
        &|_| panic!("a recorded folder reads no session"),
    );
    assert_eq!(settled, Settled::Already);
    assert!(vendor.calls().is_empty());
}

/// An executable `sh` stand-in for the build at `path`.
#[cfg(unix)]
fn stub(path: &Path, body: &str) {
    use std::os::unix::fs::PermissionsExt as _;
    std::fs::write(path, format!("#!/bin/sh\n{body}")).expect("the stand-in");
    std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o755)).expect("its mode");
}

/// The answer a stand-in prints: Claude Code 2.1.323's, with `recorded` as its flag.
#[cfg(unix)]
fn answer_line(recorded: bool) -> String {
    format!(
        "echo '{{\"type\":\"control_response\",\"response\":{{\"subtype\":\"success\",\
         \"request_id\":\"aterm-trust\",\"response\":{{\"pid\":1,\"claude_code_version\":\
         \"2.1.323\",\"workspace_trust_recorded\":{recorded}}}}}}}'\n"
    )
}

/// The process id a stand-in wrote to `name` under `root`.
#[cfg(unix)]
fn pid_in(root: &Path, name: &str) -> Option<u32> {
    std::fs::read_to_string(root.join(name))
        .ok()?
        .trim()
        .parse()
        .ok()
}

/// Wait, as a hang detector, until none of `pids` is alive.
#[cfg(unix)]
fn gone(pids: &[u32]) {
    let until = std::time::Instant::now() + HANG;
    while pids.iter().any(|pid| aterm_uds::process::pid_alive(*pid)) {
        assert!(
            std::time::Instant::now() < until,
            "the start's group outlived it: {pids:?}"
        );
        std::thread::sleep(Duration::from_millis(10));
    }
}

/// THE HEADLESS START IS ATPKG'S WARM START, HANDED ONE LINE, AND LEFT TO EXIT BY ITSELF: the
/// build runs once in the folder with exactly the arguments atpkg warms a build with (which keep
/// a folder's hooks and MCP servers from running), under the launch's own environment with
/// atpkg's variables over it, and reads exactly the one `initialize` line on its stdin. What it
/// prints before its answer, on either stream, is read past, and the trust its profile then
/// holds is the result. Its answer read, its stdin is closed and it finishes what it was doing:
/// the stand-in writes a file only after its stdin ended, as Claude Code finishes a profile
/// save or a login refresh it had under way, and the start returns only once it has. A start
/// ended at its answer never writes that file.
#[cfg(unix)]
#[test]
fn the_headless_start_is_the_warm_start_handed_one_request_line() {
    let w = World::new("start");
    stub(
        &w.build,
        &format!(
            r#"printf '%s\n' "$@" > "$STUB_OUT/argv"
pwd -P > "$STUB_OUT/cwd"
printf '%s|%s|%s\n' "${{ENABLE_CLAUDEAI_MCP_SERVERS-unset}}" "${{DISABLE_AUTOUPDATER-unset}}" "${{CLAUDE_CODE_DISABLE_FEEDBACK_SURVEY-unset}}" > "$STUB_OUT/vars"
IFS= read -r line
printf '%s\n' "$line" > "$STUB_OUT/stdin"
echo 'starting up'
echo 'a warning' >&2
echo '{{"type":"system","subtype":"init"}}'
printf '{{"projects":{{"%s":{{"hasTrustDialogAccepted":true}}}}}}\n' "$(pwd -P)" > "$CLAUDE_CONFIG_DIR/.claude.json"
{}cat > /dev/null
echo finished > "$STUB_OUT/finished"
"#,
            answer_line(true)
        ),
    );
    let (argv, env) = (words(&["claude"]), w.env());
    let state = w.root.join("state");
    let mut book = DriveBook {
        state: Some(state.clone()),
        sid: "s-0123".to_string(),
    };
    let settled = settle(
        &w.launch(&argv, &env),
        &trusts,
        &StoreVendor,
        &mut book,
        HANG,
    );
    assert_eq!(
        settled,
        Settled::Recorded {
            rule: approval::RULE_TRUST_ANY,
            by: "Claude Code 2.1.323".to_string()
        }
    );
    let read = |name: &str| std::fs::read_to_string(w.root.join(name)).expect(name);
    let warm = atpkg::vendor_direct::spec("claude")
        .and_then(|spec| spec.warm)
        .expect("Claude Code's warm start");
    assert_eq!(
        read("argv").lines().collect::<Vec<_>>(),
        warm.argv,
        "the warm arguments, and nothing of the launch's own"
    );
    assert!(
        warm.argv.contains(&"--strict-mcp-config") && warm.argv.contains(&"-p"),
        "{:?}",
        warm.argv
    );
    assert_eq!(read("cwd").trim(), w.folder.to_string_lossy());
    assert_eq!(read("vars").trim(), "false|1|1");
    assert_eq!(
        read("stdin"),
        request_line(&w.folder.to_string_lossy()),
        "exactly the one request line"
    );
    assert_eq!(
        read("finished").trim(),
        "finished",
        "the start was left to finish after its stdin closed"
    );
    let rows = std::fs::read_to_string(state.join("drive/s-0123.jsonl")).expect("the ledger");
    assert_eq!(rows.lines().count(), 1, "{rows}");
    assert!(rows.contains("recorded before launch by Claude Code 2.1.323"));
}

/// The session a process leads, where it leads one: `getsid(pid) == pid`.
#[cfg(unix)]
fn leads_a_session(pid: u32) -> Option<bool> {
    let pid = i32::try_from(pid).ok()?;
    // SAFETY: `getsid` takes a process id and returns one or -1; it reads no memory of ours.
    let session = unsafe { libc::getsid(pid) };
    (session > 0).then_some(session == pid)
}

/// A START THAT NEVER ANSWERS IS ENDED AT THE DEADLINE, ITS WHOLE GROUP, AND RAN IN A SESSION OF
/// ITS OWN: the launch is refused with the deadline as its reason, and neither the build nor a
/// child it started outlives it. While it ran it led its own session, so it had no terminal of
/// the tab's to open, draw or prompt on. The stand-in first writes its process ids, which shows
/// it ran.
#[cfg(unix)]
#[test]
fn a_start_that_never_answers_is_ended_at_the_deadline_with_its_group() {
    // The deadline doubles until one start lived long enough to write its process ids and be
    // read while it ran: a start ended before that shows nothing of its group or session.
    let mut deadline = Duration::from_millis(400);
    loop {
        assert!(deadline < HANG, "no stand-in was read while it ran");
        let w = World::new("hang");
        stub(
            &w.build,
            r#"sleep 600 &
echo "$!" > "$STUB_OUT/child"
echo "$$" > "$STUB_OUT/pid.new" && mv "$STUB_OUT/pid.new" "$STUB_OUT/pid"
wait
"#,
        );
        let env = w.env();
        let began = std::time::Instant::now();
        let folder = w.folder.to_string_lossy().into_owned();
        let ended = std::sync::atomic::AtomicBool::new(false);
        let (said, led) = std::thread::scope(|scope| {
            // Read the stand-in's session while it runs: it is gone once the start returns.
            let watch = scope.spawn(|| {
                loop {
                    if let Some(led) = pid_in(&w.root, "pid").and_then(leads_a_session) {
                        return Some(led);
                    }
                    if ended.load(std::sync::atomic::Ordering::SeqCst) {
                        return None;
                    }
                    std::thread::sleep(Duration::from_millis(5));
                }
            });
            let said = start(&w.build, &w.folder, &env, &folder, deadline);
            ended.store(true, std::sync::atomic::Ordering::SeqCst);
            (said, watch.join().expect("the watcher"))
        });
        assert_eq!(
            said,
            Attested::Failed(format!(
                "Claude Code did not answer within {} ms",
                deadline.as_millis()
            ))
        );
        assert!(began.elapsed() >= deadline, "ended before its deadline");
        let (Some(build), Some(child), Some(led)) =
            (pid_in(&w.root, "pid"), pid_in(&w.root, "child"), led)
        else {
            deadline *= 2;
            continue;
        };
        assert!(led, "the start led no session of its own: pid {build}");
        gone(&[build, child]);
        break;
    }
}

/// A START THAT ANSWERED AND THEN DOES NOT GO IS ENDED AT THE DEADLINE, AND ITS ANSWER STANDS:
/// the answer was read before the deadline, so the launch has it, and the build that ignored
/// its stdin's end does not outlive the deadline.
#[cfg(unix)]
#[test]
fn a_start_that_answers_and_lingers_is_ended_at_the_deadline_and_its_answer_stands() {
    // The deadline doubles until one start lived long enough to answer.
    let mut deadline = Duration::from_millis(400);
    loop {
        assert!(deadline < HANG, "no stand-in answered within its deadline");
        let w = World::new("linger");
        stub(
            &w.build,
            &format!(
                "echo \"$$\" > \"$STUB_OUT/pid\"\n{}exec sleep 600\n",
                answer_line(false)
            ),
        );
        let folder = w.folder.to_string_lossy().into_owned();
        let began = std::time::Instant::now();
        let said = start(&w.build, &w.folder, &w.env(), &folder, deadline);
        if matches!(&said, Attested::Failed(why) if why.contains("did not answer within")) {
            deadline *= 2;
            continue;
        }
        assert_eq!(
            said,
            Attested::Declined("Claude Code 2.1.323 recorded nothing for this folder".to_string())
        );
        assert!(
            began.elapsed() >= deadline,
            "a start that had not exited was ended before its deadline"
        );
        gone(&[pid_in(&w.root, "pid").expect("the stand-in's process id")]);
        break;
    }
}

/// A BUILD THAT REFUSES, ENDS UNANSWERED OR CANNOT START records nothing, with its reason, and
/// its output never becomes a recorded trust: Claude Code's own answer is a decline, and a
/// start that gave none a failure.
#[cfg(unix)]
#[test]
fn a_build_that_refuses_or_ends_unanswered_is_a_refusal() {
    let declined = format!("IFS= read -r line\n{}cat > /dev/null\n", answer_line(false));
    for (tag, body, said) in [
        (
            "false",
            declined.as_str(),
            Attested::Declined("Claude Code 2.1.323 recorded nothing for this folder".to_string()),
        ),
        (
            "old",
            "echo 'error: unknown option --input-format' >&2\nexit 1\n",
            Attested::Failed("Claude Code ended without answering".to_string()),
        ),
        (
            "partial",
            "printf '%s' '{\"type\":\"control_response\",\"response\":{\"subtype\":\"success\",\"request_id\":\"aterm-trust\",\"response\":{\"workspace_trust_recorded\":tr'\n",
            Attested::Failed("Claude Code's answer could not be read".to_string()),
        ),
    ] {
        let w = World::new(tag);
        stub(&w.build, body);
        let folder = w.folder.to_string_lossy().into_owned();
        assert_eq!(
            start(&w.build, &w.folder, &w.env(), &folder, HANG),
            said,
            "{tag}"
        );
    }
    let w = World::new("missing");
    let folder = w.folder.to_string_lossy().into_owned();
    let said = start(&w.build, &w.folder, &w.env(), &folder, HANG);
    assert!(
        matches!(&said, Attested::Failed(why) if why.starts_with("Claude Code could not be started")),
        "{said:?}"
    );
}

/// WHAT RECORDED NOTHING IS NOT ASKED AGAIN AT ONCE, AND IS ASKED AGAIN IN TIME: the same build,
/// folder and home is not started a second time while an earlier answer stands, so a relaunch
/// pass that plans again every few seconds does not start Claude Code at each plan. Claude
/// Code's own answer stands for [`DECLINE_STANDS`]; a start that gave no answer (here one that
/// ended unanswered, as a start missed on a loaded machine does) for the shorter
/// [`FAILURE_STANDS`], after which it is tried again, so one missed start is not a folder never
/// settled. Another folder is another ask.
#[cfg(unix)]
#[test]
fn a_start_that_recorded_nothing_stands_for_a_time_and_is_then_asked_again() {
    let declines = format!(
        "echo started >> \"$STUB_OUT/starts\"\nIFS= read -r line\n{}cat > /dev/null\n",
        answer_line(false)
    );
    let fails = "echo started >> \"$STUB_OUT/starts\"\nexit 1\n";
    for (tag, body, stands) in [
        ("declined", declines.as_str(), DECLINE_STANDS),
        ("failed", fails, FAILURE_STANDS),
    ] {
        let w = World::new(tag);
        stub(&w.build, body);
        let env = w.env();
        let folder = w.folder.to_string_lossy().into_owned();
        let starts = || {
            std::fs::read_to_string(w.root.join("starts")).map_or(0, |text| text.lines().count())
        };
        let now = std::time::Instant::now();
        let ask = |at: std::time::Instant| {
            StoreVendor::attest_at(&w.build, &w.folder, &env, &folder, HANG, at)
        };
        let first = ask(now);
        assert!(
            matches!(first, Attested::Declined(_) | Attested::Failed(_)),
            "{tag}: {first:?}"
        );
        assert_eq!(starts(), 1, "{tag}");
        for at in [now, now + stands - Duration::from_millis(1)] {
            let again = ask(at);
            assert!(
                matches!(&again, Attested::Stands(why) if why.contains("not asked again")),
                "{tag}: {again:?}"
            );
        }
        assert_eq!(starts(), 1, "{tag}: started while its answer stood");
        // Another folder under the same build and home is another ask.
        let other = w.root.join("other");
        std::fs::create_dir_all(&other).expect("another folder");
        let elsewhere =
            StoreVendor::attest_at(&w.build, &other, &env, &other.to_string_lossy(), HANG, now);
        assert!(!matches!(elsewhere, Attested::Stands(_)), "{tag}");
        assert_eq!(starts(), 2, "{tag}");
        let later = ask(now + stands);
        assert!(!matches!(later, Attested::Stands(_)), "{tag}: {later:?}");
        assert_eq!(starts(), 3, "{tag}: asked again once its time had come");
    }
}
