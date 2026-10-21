// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

// THE POLICY'S PROBE: each derived answer reads back off a tape as it was
// written; a recorded probe answers exactly as the live one and tapes one
// round trip per look; and what a probe reads to reach its answer — the
// worker's environment, a repository's configuration — never reaches the
// tape, while the same detector finds a secret logged through the door
// (the negative control).

use std::path::{Path, PathBuf};

use astream_effects::EffectRecord;

use super::*;
use crate::supervise::fx::Fx;
use crate::supervise::policy::claude_settings::{AskKind, OwnerAsk};
use crate::supervise::tape::recover::Recovered;
use crate::supervise::tape::testing::MemoryTape;
use crate::supervise::tape::text;

/// `value` written as a tape holds it and read back.
fn reread<T: TapeValue>(value: &T) -> T {
    T::from_tape(&value.to_tape()).expect("an answer this build wrote reads back")
}

/// Every probe's answer, in each of its forms, reads back as it was written.
#[test]
fn each_answer_reads_back_as_it_was_written() {
    for twin in [
        Ok(None),
        Ok(Some("/w/x\u{200b}y".to_string())),
        Err("a Read whose folder cannot be listed".to_string()),
    ] {
        assert_eq!(reread(&twin), twin);
    }
    for real in [None, Some(vec!["private".to_string(), "tmp".to_string()])] {
        assert_eq!(reread(&real), real);
    }
    for disk in [
        Ok(OnDisk {
            comps: vec!["private".to_string(), "tmp".to_string(), "x".to_string()],
            foreign_link: true,
        }),
        Err("/x leads through more than 32 symbolic links".to_string()),
    ] {
        assert_eq!(reread(&disk), disk);
    }
    let view = GitView {
        config: vec![
            ("local".to_string(), "core.fsmonitor".to_string(), None),
            (
                "global".to_string(),
                "remote.o.promisor".to_string(),
                Some("false".to_string()),
            ),
        ],
        index_hook: Some(PathBuf::from("/w/.git/hooks/post-index-change")),
        filters_selected: Some(vec!["lfs".to_string()]),
        submodules: vec![(
            PathBuf::from("/w/sub"),
            GitView {
                external_diff: Some(VALUE_SET.to_string()),
                ..GitView::default()
            },
        )],
        external_diff: None,
    };
    for v in [
        Ok(view),
        Err("cannot read the git config in /w".to_string()),
    ] {
        assert_eq!(reread(&v), v);
    }
    let startup = ShellStartup::of_parts(
        vec![(
            "ll".to_string(),
            "an alias in /c/shell-snapshots/s.sh".to_string(),
            true,
        )],
        true,
    );
    for s in [
        Ok(startup),
        Err("the Bash tool's zsh starts with `setopt x`".to_string()),
    ] {
        assert_eq!(reread(&s), s);
    }
    let asks = vec![
        OwnerAsk {
            kind: AskKind::Rule {
                tool: "Bash".to_string(),
                bare: false,
            },
            text: "Bash(sed:*)".to_string(),
            source: "/c/settings.json".to_string(),
        },
        OwnerAsk {
            kind: AskKind::Hook { matcher: None },
            text: "(none)".to_string(),
            source: "/w/.claude/settings.json".to_string(),
        },
    ];
    for a in [Ok(asks), Err("its directory is unknown".to_string())] {
        assert_eq!(reread(&a), a);
    }
    let cwds = vec![PathBuf::from("/w"), PathBuf::from("/w/sub")];
    assert_eq!(reread(&cwds), cwds);
    // The worker reads back as its digest and pid, never a variable.
    let worker = WorkerEnv::hermetic(None)
        .with("ANTHROPIC_API_KEY=planted")
        .with_pid(77)
        .digested(9);
    let back = reread(&Ok::<_, String>(worker.clone())).expect("a worker");
    assert_eq!(back.digest(0), worker.digest(9));
    assert_eq!(back.pid(), Some(77));
    assert_eq!(back.var("ANTHROPIC_API_KEY"), None);
    let refused: Result<WorkerEnv, String> =
        Err("s-1's foreground process 7 names no session".into());
    assert_eq!(reread(&refused), refused);
}

/// A view projected by a rule this build does not have is refused, never
/// judged as though the keys that rule kept were never set.
#[test]
fn a_view_projected_by_another_rule_is_refused() {
    let written =
        String::from_utf8(Ok::<_, String>(GitView::default()).to_tape()).expect("json text");
    let other = written.replace(
        &format!("\"projection\":{PROJECTION}"),
        &format!("\"projection\":{}", PROJECTION + 1),
    );
    assert_ne!(other, written, "the version is on the record: {written}");
    assert!(<Result<GitView, String>>::from_tape(other.as_bytes()).is_err());
}

/// The round trips of a finished tape at `door`: request and reply.
pub(crate) fn exchanges<'a>(read: &'a Recovered, door: &str) -> Vec<(&'a [u8], &'a [u8])> {
    read.items
        .iter()
        .filter_map(|i| match i.record() {
            Some(EffectRecord::Exchange {
                door: d,
                request,
                reply,
            }) if d == door => Some((request.as_slice(), reply.as_slice())),
            _ => None,
        })
        .collect()
}

/// A RECORDED PROBE IS THE LIVE ONE: over a scratch tree with a symbolic
/// link, the hidden-twin walk, the Read rule's resolver and the rm rule's
/// walk answer through the recording probe exactly as through the live one,
/// and each look is one round trip at its own door, its request the path and
/// its reply the answer — which reads back as what the policy was handed.
#[test]
fn a_recorded_probe_answers_as_the_live_one_and_tapes_each_look() {
    let dir = crate::supervise::ScratchDir::new("probe", "disk");
    let root = std::fs::canonicalize(&*dir).expect("canonical");
    std::fs::create_dir_all(root.join("real")).expect("real");
    #[cfg(unix)]
    std::os::unix::fs::symlink(root.join("real"), root.join("link")).expect("link");
    // The walks take a path's names below its root (on Windows, below its drive too).
    let comps: Vec<String> = root
        .join("link")
        .join("x")
        .components()
        .filter_map(|c| match c {
            std::path::Component::Normal(name) => Some(name.to_string_lossy().into_owned()),
            _ => None,
        })
        .collect();
    let tape = MemoryTape::over(&Fx::os());
    let taped = tape.fx.policy_probe();
    let live = LiveProbe;
    let twin = taped.hidden_twin(&comps);
    assert_eq!(twin, live.hidden_twin(&comps));
    let real = taped.real_components(&comps);
    assert_eq!(real, live.real_components(&comps));
    let disk = taped.rm_physical(&comps);
    assert_eq!(disk, live.rm_physical(&comps));
    #[cfg(unix)]
    assert!(
        real.as_ref()
            .is_some_and(|r| r.iter().any(|c| c == "real") && !r.iter().any(|c| c == "link")),
        "the link is resolved: {real:?}"
    );
    let read = tape.finish("done");
    let path = format!("/{}", comps.join("/"));
    for (door, reply) in [
        (doors::PROBE_HIDDEN_TWIN, twin.to_tape()),
        (doors::PROBE_REAL_COMPONENTS, real.to_tape()),
        (doors::PROBE_RM_PHYSICAL, disk.to_tape()),
    ] {
        let taped = exchanges(&read, door);
        assert_eq!(taped.len(), 1, "one round trip at {door}");
        assert_eq!(taped[0].0, path.as_bytes(), "{door}'s request is the path");
        assert_eq!(taped[0].1, reply.as_slice(), "{door}'s reply is the answer");
    }
    assert_eq!(
        <Option<Vec<String>>>::from_tape(exchanges(&read, doors::PROBE_REAL_COMPONENTS)[0].1)
            .expect("reads"),
        real
    );
}

/// Whether anything a finished tape holds — its text form, and every
/// request, reply, output and note as raw bytes — contains `secret`.
pub(crate) fn tape_holds(read: &Recovered, secret: &str) -> bool {
    if text::render(None, &read.items).contains(secret) {
        return true;
    }
    let needle = secret.as_bytes();
    let has = |hay: &[u8]| hay.windows(needle.len()).any(|w| w == needle);
    read.items.iter().any(|i| match i.record() {
        Some(EffectRecord::Exchange { request, reply, .. }) => has(request) || has(reply),
        Some(EffectRecord::Emit { bytes, .. }) => has(bytes),
        Some(EffectRecord::File { bytes, .. }) => has(bytes),
        _ => format!("{i:?}").contains(secret),
    })
}

/// `git <args>` in `dir`, hermetic, asserted ok.
fn git(dir: &Path, args: &[&str]) {
    let out = aterm_uds::deadline::output_until(
        aterm_uds::spawnfd::command("git")
            .args(args)
            .current_dir(dir)
            .env("GIT_CONFIG_NOSYSTEM", "1")
            .env("GIT_CONFIG_GLOBAL", "/dev/null"),
        std::time::Instant::now() + aterm_spec::wait::MUST_HAPPEN,
    )
    .unwrap_or_else(|e| panic!("git {args:?} within the hang detector: {e}"));
    assert!(out.status.success(), "git {args:?}");
}

/// The secrets the privacy tests plant: an API key in the worker's
/// environment, and in the repository's configuration a token in an extra
/// header, a user and password in a URL rewrite, and a credential helper.
pub(crate) const PLANTED_KEY: &str = "sk-ant-api03-planted-0005-environment-secret"; // gitleaks:allow
pub(crate) const PLANTED_HEADER: &str = "planted-0005-bearer-token";
pub(crate) const PLANTED_USER: &str = "planted0005user";
pub(crate) const PLANTED_PASS: &str = "planted0005pass";
pub(crate) const PLANTED_HELPER: &str = "planted-0005-credential-helper";

/// A repository whose configuration holds [`PLANTED_HEADER`],
/// [`PLANTED_USER`], [`PLANTED_PASS`] and [`PLANTED_HELPER`].
pub(crate) fn planted_repo(at: &Path) {
    git(at, &["init", "-q", "."]);
    git(
        at,
        &[
            "config",
            "http.https://example.com/.extraheader",
            &format!("Authorization: Bearer {PLANTED_HEADER}"),
        ],
    );
    git(
        at,
        &[
            "config",
            &format!("url.https://{PLANTED_USER}:{PLANTED_PASS}@example.com/.insteadOf"),
            "https://example.com/",
        ],
    );
    git(at, &["config", "credential.helper", PLANTED_HELPER]);
}

/// Every planted secret.
pub(crate) fn planted() -> [&'static str; 5] {
    [
        PLANTED_KEY,
        PLANTED_HEADER,
        PLANTED_USER,
        PLANTED_PASS,
        PLANTED_HELPER,
    ]
}

/// WHAT A PROBE READS NEVER REACHES THE TAPE: a worker whose environment
/// plants an API key, judging a git read in a repository whose configuration
/// plants a token, a URL's user and password and a credential helper —
/// through the git view, the settings' `env` check and the Bash tool's
/// directories — leaves none of them on the tape, and the view the policy was
/// handed holds none of them either. Negative control: the same detector
/// finds the key when it IS logged through the door.
#[test]
fn what_a_probe_reads_never_reaches_the_tape() {
    let dir = crate::supervise::ScratchDir::new("probe", "privacy");
    let repo = std::fs::canonicalize(&*dir).expect("canonical");
    planted_repo(&repo);
    let worker = WorkerEnv::hermetic(None).with(&format!("ANTHROPIC_API_KEY={PLANTED_KEY}"));
    let tape = MemoryTape::over(&Fx::os());
    let probe = tape.fx.policy_probe();
    let writes = WorkerWrites {
        cwd: &repo,
        shell_cwds: &[],
        scratch_roots: &[],
    };
    let view = probe
        .git_view(&worker, &repo, &writes)
        .expect("the repository's view");
    assert_eq!(probe.settings_env(&worker, &repo), Ok(()));
    assert!(probe.shell_cwds(&worker, &repo).is_empty());
    let handed = format!("{view:?}");
    let read = tape.finish("done");
    assert_eq!(exchanges(&read, doors::PROBE_GIT_VIEW).len(), 1);
    for secret in planted() {
        assert!(!tape_holds(&read, secret), "{secret} reached the tape");
        assert!(
            !handed.contains(secret),
            "{secret} reached the policy: {handed}"
        );
    }
    // The tape names the worker by its digest under this tape's seed.
    let digest = format!("{:016x}", worker.digest(tape.core().seed()));
    assert!(
        tape_holds(&read, &digest),
        "the worker is named by its digest"
    );

    // Negative control: a door that logs the key is caught by the same detector.
    let leaky = MemoryTape::over(&Fx::os());
    leaky.fx.note("leak", || {
        worker
            .var("ANTHROPIC_API_KEY")
            .unwrap_or_default()
            .to_string()
    });
    let leaked = leaky.finish("done");
    assert!(
        tape_holds(&leaked, PLANTED_KEY),
        "the detector sees a logged key"
    );
}

/// The secrets the startup privacy test plants: a key in a `.zshenv` alias, a
/// value on a snapshot's line of a shape the check does not read, a settings
/// `env` value, a hook's command, and another session's id in the worker's
/// `ATERM_PARENT_SESSION_ID`.
const PLANTED_RC: &str = "planted-0005-zshenv-alias-key";
const PLANTED_SNAPSHOT: &str = "planted-0005-snapshot-line";
const PLANTED_SETTINGS: &str = "planted-0005-settings-env-value";
const PLANTED_HOOK: &str = "planted-0005-hook-command-token";
const PLANTED_SESSION: &str = "planted-0005-other-session";

/// A zsh shell snapshot as Claude Code writes one, its person's part
/// holding `extra` among the shell's options.
fn zsh_snapshot(extra: &str) -> String {
    format!(
        "# Snapshot file\n# Unset all aliases to avoid conflicts with functions\n\
         unalias -a 2>/dev/null || true\n# Functions\n# Shell Options\nsetopt nohashdirs\n\
         setopt login\n{extra}# Aliases\nalias -- run-help=man\n\
         # Check for rg availability\nexport PATH='/usr/bin:/bin'\n"
    )
}

/// WHAT THE SHELL'S STARTUP AND THE SETTINGS HOLD NEVER REACHES THE TAPE
/// (the replay plan's §7: rc files, Claude Code's settings — derived answers
/// only). A `.zshenv` whose alias carries a key, a snapshot line of a shape
/// the check does not read, a settings `env` value that fails a git read and
/// a PreToolUse hook's command each reach the probe that judges them — and
/// the answers it tapes (refusals included, which a tape holds as written)
/// name the file, the line's number, the word or the variable, never what
/// the file held. A worker bound to another session is refused without that
/// session's id. Negative control: the same detector finds the `.zshenv`'s
/// key when its text IS taped.
#[test]
fn what_the_shells_startup_and_the_settings_hold_never_reaches_the_tape() {
    let dir = crate::supervise::ScratchDir::new("probe", "startup");
    let root = std::fs::canonicalize(&*dir).expect("canonical");
    let (home, project, claude) = (root.join("home"), root.join("project"), root.join("claude"));
    for d in [
        &home,
        &project.join(".claude"),
        &claude.join("shell-snapshots"),
    ] {
        std::fs::create_dir_all(d).expect("scratch tree");
    }
    let snapshot = claude.join("shell-snapshots/snapshot-zsh-1-a1b2c3.sh");
    std::fs::write(&snapshot, zsh_snapshot("")).expect("snapshot");
    let zshenv =
        format!("export A=1\nalias deploy='AWS_SECRET_ACCESS_KEY={PLANTED_RC} ./deploy'\n");
    std::fs::write(home.join(".zshenv"), &zshenv).expect("zshenv");
    std::fs::write(
        project.join(".claude/settings.json"),
        format!(
            "{{\"env\": {{\"GIT_ASKPASS\": \"{PLANTED_SETTINGS}\"}}, \"hooks\": {{\"PreToolUse\": \
             [{{\"matcher\": \"Bash\", \"hooks\": [{{\"type\": \"command\", \"command\": \
             \"guard --token {PLANTED_HOOK}\"}}]}}]}}}}"
        ),
    )
    .expect("settings");
    let worker = WorkerEnv::from_env(vec![
        format!("HOME={}", home.display()),
        "SHELL=/bin/zsh".to_string(),
        format!("CLAUDE_CONFIG_DIR={}", claude.display()),
    ]);
    let tape = MemoryTape::over(&Fx::os());
    let probe = tape.fx.policy_probe();

    let rc = probe
        .shell_startup(&worker, &project)
        .expect_err("the .zshenv's alias fails every line");
    assert!(
        rc.contains(".zshenv") && rc.contains("line 2") && rc.contains("`alias`"),
        "the file, the line's number and the word are named: {rc}"
    );
    std::fs::write(home.join(".zshenv"), "export A=1\n").expect("a quiet zshenv");
    std::fs::write(
        &snapshot,
        zsh_snapshot(&format!("set -A keys {PLANTED_SNAPSHOT}\n")),
    )
    .expect("snapshot");
    let line = probe
        .shell_startup(&worker, &project)
        .expect_err("a `set` line fails every line");
    assert!(
        line.contains("a `set` line") && line.contains("line 8"),
        "the line's first word and number are named: {line}"
    );
    let env = probe
        .settings_env(&worker, &project)
        .expect_err("a git-relevant variable from the settings");
    assert!(
        env.contains("`GIT_ASKPASS`"),
        "the variable is named: {env}"
    );
    let asks = probe
        .owner_asks(&worker, &project)
        .expect("the agent's asks");
    assert_eq!(
        asks.iter().map(|a| a.text.as_str()).collect::<Vec<_>>(),
        ["Bash"],
        "the hook, by its matcher"
    );
    let read = tape.finish("done");
    assert_eq!(exchanges(&read, doors::PROBE_SHELL_STARTUP).len(), 2);
    assert_eq!(exchanges(&read, doors::PROBE_SETTINGS_ENV).len(), 1);
    assert_eq!(exchanges(&read, doors::PROBE_OWNER_ASKS).len(), 1);
    for secret in [PLANTED_RC, PLANTED_SNAPSHOT, PLANTED_SETTINGS, PLANTED_HOOK] {
        assert!(!tape_holds(&read, secret), "{secret} reached the tape");
    }

    // The worker's binding to its session: a process whose variable names
    // another session is refused, and what is refused names no value.
    let other = WorkerEnv::from_env(vec![format!("ATERM_PARENT_SESSION_ID={PLANTED_SESSION}")]);
    let refused = super::super::probe_live::bound_to(other, 7, "s-mine");
    let why = refused.clone().expect_err("another session's process");
    assert!(why.contains("another session"), "{why}");
    let taped = String::from_utf8(refused.to_tape()).expect("json text");
    assert!(!taped.contains(PLANTED_SESSION), "{taped}");

    // Negative control: the detector finds the key when the text is taped.
    let leaky = MemoryTape::over(&Fx::os());
    leaky.fx.note("leak", || zshenv.clone());
    assert!(
        tape_holds(&leaky.finish("done"), PLANTED_RC),
        "the detector sees a taped .zshenv"
    );
}

/// A KEYED PROBE ANSWERS FROM WHAT THE RECORDED LOOP HAD READ, bounded by
/// astream-effects' keyed view's own bound — a COUNT of records read, never
/// an index: a look recorded as the tape's second record is no answer to a
/// probe standing where the loop had read none or one record (it had not
/// learned it yet; the negative control of the bound), and is the answer
/// once it has read two; of two answers to one request the latest read is
/// taken, past the tape's end too; and a request the tape never holds — the
/// same look asked of another path — is answered as a refused look and
/// named among the misses, door and request as the tape writes them.
#[test]
fn a_keyed_probe_answers_from_what_the_loop_had_read() {
    let roots = crate::supervise::tape::header::Roots::of(vec![("w", PathBuf::from("/w"))]);
    let twin = |path: &str| {
        let answer: Result<Option<String>, String> = Ok(Some(path.to_string()));
        answer.to_tape()
    };
    let asked = |reply: Vec<u8>| EffectRecord::Exchange {
        door: doors::PROBE_HIDDEN_TWIN.to_string(),
        request: b"@w/a/b".to_vec(),
        reply,
    };
    let records: Arc<[EffectRecord]> = vec![
        EffectRecord::Mono(5),
        asked(twin("/w/a/first")),
        asked(twin("/w/a/second")),
    ]
    .into();
    let comps: Vec<String> = ["w", "a", "b"].map(String::from).to_vec();
    // One index, every probe: as a tape opened to explain its decisions.
    let index = Arc::new(TapeIndex::new(records));
    let at = |seen| KeyedProbe::at(Arc::clone(&index), seen, roots.clone(), 0);
    for seen in [0, 1] {
        let probe = at(seen);
        let got = probe.hidden_twin(&comps);
        assert!(got.is_err(), "seen={seen}: {got:?}");
        assert_eq!(probe.misses(), vec!["probe.hidden_twin @w/a/b".to_string()]);
    }
    assert_eq!(
        at(2).hidden_twin(&comps),
        Ok(Some("/w/a/first".to_string()))
    );
    for seen in [3, usize::MAX] {
        let probe = at(seen);
        assert_eq!(
            probe.hidden_twin(&comps),
            Ok(Some("/w/a/second".to_string())),
            "seen={seen}"
        );
        assert!(probe.misses().is_empty(), "seen={seen}");
    }
    let probe = at(3);
    let other: Vec<String> = ["w", "c"].map(String::from).to_vec();
    assert!(probe.hidden_twin(&other).is_err());
    assert_eq!(probe.misses(), vec!["probe.hidden_twin @w/c".to_string()]);
}
