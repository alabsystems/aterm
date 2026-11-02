// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Preparing a Claude child without contacting or changing its preparation host.

use super::*;

struct Fixture {
    root: aterm_tempfile::TempDir,
    run: Run,
}

#[derive(Debug, PartialEq, Eq)]
struct Host {
    record: Run,
    rendered: String,
    saved: Vec<u8>,
    settings: Vec<u8>,
    launcher: Vec<u8>,
    socket: Vec<u8>,
}

impl Fixture {
    fn new() -> Self {
        let root = aterm_tempfile::Builder::new()
            .prefix("aterm-claude-prepared-")
            .tempdir()
            .unwrap();
        let base = fs::canonicalize(root.path()).unwrap();
        let run_dir = base.join("run with spaces");
        let w = run_dir.join("scratch");
        let runtime = base.join("runtime");
        let project = base.join("project ; literal");
        for dir in [
            w.join("home/.claude"),
            w.join("cfg"),
            w.join("tmp"),
            runtime.clone(),
            project.clone(),
            run_dir.join("shim"),
        ] {
            fs::create_dir_all(dir).unwrap();
        }
        let claude = run_dir.join("fake claude");
        fs::write(&claude, "#!/bin/sh\nprintf 'fake Claude\\n'\n").unwrap();
        fs::set_permissions(&claude, fs::Permissions::from_mode(0o755)).unwrap();
        let run = Run {
            tag: "prepared-test".into(),
            run: run_dir,
            w,
            sockdir: runtime.to_string_lossy().into_owned(),
            sock: base.join("not-a-socket").to_string_lossy().into_owned(),
            proj: project.to_string_lossy().into_owned(),
            port: "48991".into(),
            proxy: "http://127.0.0.1:48992".into(),
            instance_pid: 42,
            agent: "codex".into(),
            claude_bin: claude.to_string_lossy().into_owned(),
            claude_ver: "2.1.999".into(),
            ..Default::default()
        };
        fs::write(&run.sock, "a regular file, never a control socket").unwrap();
        fs::write(
            run.w.join("home/.claude/settings.json"),
            "{\"apiKeyHelper\":\"lab-only\"}\n",
        )
        .unwrap();
        fs::write(run.run.join("claude.sh"), "normal launcher, untouched\n").unwrap();
        run.save();
        Self { root, run }
    }

    fn host(&self) -> Host {
        Host {
            record: self.run.clone(),
            rendered: self.run.render(),
            saved: fs::read(self.run.run.join("env.sh")).unwrap(),
            settings: fs::read(self.run.w.join("home/.claude/settings.json")).unwrap(),
            launcher: fs::read(self.run.run.join("claude.sh")).unwrap(),
            socket: fs::read(&self.run.sock).unwrap(),
        }
    }

    fn no_preparation_written(&self, before: &Host) {
        assert_eq!(&self.host(), before, "the preparation host is unchanged");
        assert!(!self.run.run.join("claude-prepared.sh").exists());
    }
}

#[test]
fn preparing_a_claude_child_needs_no_socket_and_does_not_change_its_host() {
    let fixture = Fixture::new();
    let before = fixture.host();
    let launch = Launch {
        prepare_only: true,
        ..Default::default()
    };
    let called = std::cell::Cell::new(false);
    let plan = prepare_claude_with(&fixture.run, &launch, "lab-user", |run| {
        assert_eq!(run, &fixture.run);
        called.set(true);
        Ok(())
    })
    .expect("a live preparation host can prepare without a control socket");
    assert!(called.get());
    assert_eq!(fixture.host(), before);
    assert_eq!(plan.launcher, fixture.run.run.join("claude-prepared.sh"));
    assert_eq!(plan.cwd, Path::new(&fixture.run.proj));
    assert_eq!(
        fs::read_to_string(&plan.launcher).unwrap(),
        launcher(&fixture.run, &launch)
    );
    let json = plan.json(&fixture.run.tag);
    assert_eq!(json.get("schema").and_then(J::as_i64), Some(1));
    assert_eq!(
        json.get("kind").and_then(J::as_str),
        Some("claude-prepared")
    );
    assert_eq!(
        json.get("tag").and_then(J::as_str),
        Some(fixture.run.tag.as_str())
    );
    assert_eq!(
        json.get("cwd").and_then(J::as_str),
        Some(fixture.run.proj.as_str())
    );
    assert_eq!(json.get("requires_preparation_host"), Some(&J::Bool(true)));
    assert_eq!(
        json.get("argv")
            .and_then(J::as_arr)
            .unwrap()
            .iter()
            .map(|arg| arg.as_str().unwrap())
            .collect::<Vec<_>>(),
        plan.argv.iter().map(String::as_str).collect::<Vec<_>>()
    );
}

#[test]
fn refused_preparations_write_nothing_before_validation_and_witness_checks() {
    let mut fixture = Fixture::new();
    let env = |value: &str| Launch {
        envs: vec![value.into()],
        ..Default::default()
    };
    for launch in [
        Launch {
            relay: true,
            ..Default::default()
        },
        Launch {
            subscriber: true,
            ..Default::default()
        },
        Launch {
            by_name: true,
            ..Default::default()
        },
        env("MISSING_EQUALS"),
        env("BAD-NAME=value"),
        env("HOME=/a-different-home"),
        env("ANTHROPIC_BASE_URL=http://127.0.0.1:9"),
        env("TERM=other"),
        env("COLORTERM=other"),
        env("TERM_PROGRAM=other"),
        env("EXTRA=contains\0nul"),
        Launch {
            args: vec!["argument\0nul".into()],
            ..Default::default()
        },
    ] {
        let before = fixture.host();
        let error = prepare_claude_with(&fixture.run, &launch, "lab-user", |_| {
            panic!("invalid launch reached its witness")
        })
        .err()
        .expect("invalid launch refused");
        assert_eq!(error.code, 2);
        fixture.no_preparation_written(&before);
    }
    for recorded in ["HOME=/a-different-home", "TMPDIR=/outside-the-lab"] {
        fixture.run.instance_env = recorded.into();
        fixture.run.save();
        let before = fixture.host();
        assert!(
            prepare_claude_with(&fixture.run, &Launch::default(), "lab-user", |_| {
                panic!("invalid recorded environment reached its witness")
            })
            .is_err()
        );
        fixture.no_preparation_written(&before);
    }
    let before = fixture.host();
    let mut traversal = fixture.run.clone();
    traversal.w = "/tmp/aterm-prepared-synthetic-scratch".into();
    traversal.instance_env = format!("TMPDIR={}/tmp/../../outside", traversal.w.display());
    let error = prepare_claude_with(&traversal, &Launch::default(), "lab-user", |_| {
        panic!("an escaping TMPDIR reached its witness")
    })
    .err()
    .expect("TMPDIR cannot walk out of its scratch root");
    assert_eq!(error.code, 2);
    assert!(error.msg.unwrap().contains("within the lab scratch"));
    fixture.no_preparation_written(&before);
    fixture.run.instance_env.clear();
    for pid in [0, i32::try_from(std::process::id()).unwrap()] {
        fixture.run.sampler_pid = pid;
        fixture.run.save();
        let before = fixture.host();
        let error = prepare_claude_with(
            &fixture.run,
            &Launch::default(),
            "lab-user",
            procs::need_witness,
        )
        .err()
        .expect("a missing or unrelated process is no witness");
        assert_eq!(error.code, 1);
        fixture.no_preparation_written(&before);
    }
}

/// This is a minimal synthetic jail, not a replica of the production lab profile. Its denied
/// file is newly created by this test; no owner path, credential or provider is used.
#[cfg(target_os = "macos")]
#[test]
fn the_prepared_argv_runs_a_literal_claude_child_in_its_own_environment_and_jail() {
    let fixture = Fixture::new();
    let clear = fixture.run.run.join("shim/clear");
    fs::write(&clear, "#!/bin/sh\nexit 0\n").unwrap();
    fs::set_permissions(&clear, fs::Permissions::from_mode(0o755)).unwrap();
    let denied = fixture.root.path().join("synthetic-external-file");
    fs::write(&denied, "synthetic external data\n").unwrap();
    let denied = fs::canonicalize(denied).unwrap();
    assert_eq!(
        fs::read_to_string(&denied).unwrap(),
        "synthetic external data\n"
    );
    fs::write(
        fixture.run.run.join("jail.sb"),
        format!(
            "(version 1)(allow default)(deny file-read-data (literal {:?}))\n",
            denied.to_string_lossy()
        ),
    )
    .unwrap();
    fs::write(&fixture.run.claude_bin, r#"#!/bin/sh
for name in HOME XDG_CONFIG_HOME XDG_RUNTIME_DIR XDG_CACHE_HOME XDG_DATA_HOME XDG_STATE_HOME ATERM_STATE_HOME TMPDIR TMP TEMP PWD ANTHROPIC_BASE_URL HTTP_PROXY HTTPS_PROXY ALL_PROXY NO_PROXY USER LOGNAME TERM COLORTERM TERM_PROGRAM PREPARED_EXTRA; do
    eval 'printf "%s=%s\n" "$name" "${'"$name"'}"'
done
printf 'PARENT=%s\n' "${ATERM_PREPARED_PARENT_ONLY-unset}"
printf 'ARG=%s\n' "$@"
if /bin/cat "$PREPARED_DENIED" >/dev/null 2>&1; then
    printf 'EXTERNAL=read\n'
else
    printf 'EXTERNAL=denied\n'
fi
"#).unwrap();
    let literal = r#"spaces ; $(printf expanded) `printf expanded` ' " \ literal"#;
    let argument = "an argument ; $HOME $(printf expanded)";
    let launch = Launch {
        prepare_only: true,
        envs: vec![
            format!("PREPARED_EXTRA={literal}"),
            format!("PREPARED_DENIED={}", denied.display()),
        ],
        args: vec![argument.into()],
        ..Default::default()
    };
    let plan = prepare_claude_with(&fixture.run, &launch, "lab-user", |_| Ok(())).unwrap();
    let mut cmd = aterm_uds::spawnfd::command(&plan.argv[0]);
    cmd.args(&plan.argv[1..])
        .current_dir(fixture.root.path())
        .env("ATERM_PREPARED_PARENT_ONLY", "must not reach the child");
    let out = aterm_uds::deadline::output_until(&mut cmd, Instant::now() + Duration::from_secs(60))
        .expect("the harmless prepared child answers within a minute");
    assert!(out.status.success(), "{out:?}");
    let stdout = String::from_utf8(out.stdout).unwrap();
    let w = &fixture.run.w;
    let expected = [
        format!("HOME={}/home", w.display()),
        format!("XDG_CONFIG_HOME={}/cfg", w.display()),
        format!("XDG_RUNTIME_DIR={}", fixture.run.sockdir),
        format!("XDG_CACHE_HOME={}/home/.cache", w.display()),
        format!("XDG_DATA_HOME={}/home/.local/share", w.display()),
        format!("XDG_STATE_HOME={}/home/.local/state", w.display()),
        format!("ATERM_STATE_HOME={}/state", w.display()),
        format!("TMPDIR={}/tmp", w.display()),
        format!("TMP={}/tmp", w.display()),
        format!("TEMP={}/tmp", w.display()),
        format!("PWD={}", fixture.run.proj),
        format!("ANTHROPIC_BASE_URL=http://127.0.0.1:{}", fixture.run.port),
        format!("HTTP_PROXY={}", fixture.run.proxy),
        format!("HTTPS_PROXY={}", fixture.run.proxy),
        format!("ALL_PROXY={}", fixture.run.proxy),
        "NO_PROXY=127.0.0.1,localhost".into(),
        "USER=lab-user".into(),
        "LOGNAME=lab-user".into(),
        "TERM=xterm-256color".into(),
        "COLORTERM=truecolor".into(),
        "TERM_PROGRAM=aterm".into(),
        format!("PREPARED_EXTRA={literal}"),
        "PARENT=unset".into(),
        format!("ARG={argument}"),
        "EXTERNAL=denied".into(),
    ];
    assert_eq!(
        stdout.lines().collect::<Vec<_>>(),
        expected.iter().map(String::as_str).collect::<Vec<_>>()
    );
    assert_eq!(
        fs::read_to_string(&denied).unwrap(),
        "synthetic external data\n"
    );
}
