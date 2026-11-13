// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

use super::*;

struct Scratch {
    layout: Layout,
    _dir: aterm_tempfile::TempDir,
}

fn scratch(label: &str) -> Scratch {
    let dir = aterm_tempfile::Builder::new()
        .prefix(&format!("atpkg-vlane-warm-{label}-"))
        .tempdir()
        .unwrap();
    Scratch {
        layout: Layout {
            prefix: dir.path().to_path_buf(),
        },
        _dir: dir,
    }
}

fn install(layout: &Layout, program: &str, build: u64, script: &str) {
    let dir = layout.build_dir(program, build);
    std::fs::create_dir_all(dir.join("bin")).unwrap();
    let tool = ToolName::new(program).unwrap();
    let exe = dir.join("bin").join(tool.exe_file());
    std::fs::write(&exe, script).unwrap();
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt as _;
        std::fs::set_permissions(&exe, std::fs::Permissions::from_mode(0o755)).unwrap();
    }
    crate::activate::install_shims(
        layout,
        &dir,
        &[program.to_string()],
        crate::activate::Aliases::Off,
    )
    .unwrap();
    crate::store::mark_build_ready(&dir).unwrap();
    assert_eq!(crate::active_builds(layout).get(program), Some(&build));
}

/// The profile of a Claude Code that is signed in.
const SIGNED_IN: &str = r#"{"oauthAccount":{"emailAddress":"person@example.com"}}"#;

/// Make `home` one where Claude Code and Codex are signed in: at their default homes, and
/// with `<home>/.claude` named as Claude Code's own.
fn signed_in_home(home: &Path) {
    std::fs::create_dir_all(home.join(".claude")).unwrap();
    std::fs::create_dir_all(home.join(".codex")).unwrap();
    std::fs::write(home.join(".claude.json"), SIGNED_IN).unwrap();
    std::fs::write(home.join(".claude/.claude.json"), SIGNED_IN).unwrap();
    std::fs::write(home.join(".codex/auth.json"), "{}").unwrap();
}

fn landed(programs: &[&str]) -> BTreeSet<String> {
    programs.iter().map(|p| (*p).to_string()).collect()
}

fn planned(layout: &Layout, programs: &[&str]) -> Vec<(&'static str, u64)> {
    plan(layout, &landed(programs))
        .into_iter()
        .map(|(spec, build)| (spec.program, build))
        .collect()
}

fn os(args: &[&str]) -> Vec<OsString> {
    args.iter().map(OsString::from).collect()
}

fn abs_prefix() -> &'static str {
    if cfg!(windows) {
        "C:\\aterm\\pkg"
    } else {
        "/opt/pkg"
    }
}

/// Poll for `path` for up to a minute — a freshly written script's first exec can wait
/// on the system's code-policy check for seconds on a busy machine.
#[cfg(unix)]
fn appears(path: &Path) -> bool {
    (0..1200).any(|_| {
        let there = path.exists();
        if !there {
            std::thread::sleep(Duration::from_millis(50));
        }
        there
    })
}

#[test]
fn a_warm_starts_only_for_the_configured_store_on_the_network_lane() {
    let dir = std::ffi::OsStr::new("dir:/tmp/registry");
    let network = std::ffi::OsStr::new("https://example.invalid");
    assert!(admitted(true, None, true, false), "the negative control");
    assert!(admitted(true, Some(network), true, false));
    assert!(!admitted(false, None, true, false), "an unpinned manager");
    assert!(!admitted(true, Some(dir), true, false), "a dir: registry");
    assert!(
        !admitted(true, None, false, false),
        "not the configured store"
    );
    assert!(!admitted(true, None, true, true), "root");
}

#[test]
fn a_landed_build_is_launched_once_through_atpkg_only_when_admitted() {
    let scratch = scratch("launches");
    let layout = &scratch.layout;
    install(layout, "claude", 2_009_991, "#!/bin/sh\nexit 0\n");
    let programs = landed(&["claude", "ay"]);
    let atpkg = Path::new("/Applications/aterm.app/Contents/MacOS/atpkg");
    assert!(
        launches(layout, &programs, false, Some(atpkg)).is_empty(),
        "refused"
    );
    assert!(
        launches(layout, &programs, true, None).is_empty(),
        "no executable"
    );
    let mut expected = vec![atpkg.as_os_str().to_os_string()];
    expected.extend(os(&["__warm", "claude"]));
    expected.push(layout.prefix.clone().into_os_string());
    expected.push(OsString::from("2009991"));
    assert_eq!(
        launches(layout, &programs, true, Some(atpkg)),
        [("claude", 2_009_991, expected)]
    );
    assert!(
        launches(layout, &programs, true, Some(atpkg)).is_empty(),
        "started once per process"
    );
}

#[test]
fn the_warm_runs_only_an_executable_named_atpkg() {
    let bundle = PathBuf::from(abs_prefix()).join("bundle");
    let embedded = bundle.join("atpkg");
    let aterm = Some(bundle.join("aterm"));
    let dev = Some(PathBuf::from("/src/target/debug/atpkg"));
    assert_eq!(
        pick_executable(embedded.clone(), true, aterm.clone()),
        Some(embedded.clone())
    );
    assert_eq!(pick_executable(embedded.clone(), false, aterm), None);
    assert_eq!(pick_executable(embedded.clone(), false, dev.clone()), dev);
    assert_eq!(
        pick_executable(PathBuf::from("atpkg"), true, None),
        None,
        "a relative alias"
    );
    assert_eq!(
        pick_executable(bundle.join("aterm"), true, None),
        None,
        "an alias not named atpkg"
    );
    let picked = warm_executable();
    assert!(
        picked
            .as_deref()
            .is_none_or(|p| p.file_stem() == Some(std::ffi::OsStr::new("atpkg"))),
        "{picked:?}"
    );
}

#[test]
fn the_plan_skips_removed_held_linked_inactive_and_unwarmed_programs() {
    let scratch = scratch("plan");
    let layout = &scratch.layout;
    assert!(
        planned(layout, &["claude", "codex"]).is_empty(),
        "nothing active"
    );
    install(layout, "claude", 2_001_280, "#!/bin/sh\nexit 0\n");
    install(layout, "codex", 1_560_001, "#!/bin/sh\nexit 0\n");
    install(layout, "ay", 18, "#!/bin/sh\nexit 0\n");
    assert_eq!(
        planned(layout, &["claude", "codex", "ay", "trust"]),
        [("claude", 2_001_280), ("codex", 1_560_001)],
        "only the vendor rows, at their active builds"
    );
    assert_eq!(planned(layout, &["codex"]), [("codex", 1_560_001)]);
    std::fs::write(layout.removed(), "# removed\ncodex\n").unwrap();
    assert_eq!(
        planned(layout, &["claude", "codex"]),
        [("claude", 2_001_280)]
    );
    std::fs::remove_file(layout.removed()).unwrap();
    crate::pin::set_pinned(layout, "claude", true).unwrap();
    assert_eq!(
        planned(layout, &["claude", "codex"]),
        [("codex", 1_560_001)]
    );
    crate::pin::set_pinned(layout, "claude", false).unwrap();
    let marker = layout.link_marker("claude");
    std::fs::create_dir_all(marker.parent().unwrap()).unwrap();
    // An absolute checkout in this platform's spelling, in a literal TOML string.
    let checkout = if cfg!(windows) {
        r"C:\tmp\claude"
    } else {
        "/tmp/claude"
    };
    std::fs::write(
        &marker,
        format!("schema = 1\nprogram = \"claude\"\ncheckout = '{checkout}'\n"),
    )
    .unwrap();
    assert!(crate::linkmode::is_linked(layout, "claude"));
    assert_eq!(
        planned(layout, &["claude", "codex"]),
        [("codex", 1_560_001)]
    );
    std::fs::remove_file(&marker).unwrap();
    assert_eq!(
        planned(layout, &["claude", "codex"]),
        [("claude", 2_001_280), ("codex", 1_560_001)],
        "the negative control"
    );
}

#[test]
fn the_verb_takes_exactly_a_warmed_program_an_absolute_prefix_and_a_build() {
    let prefix = abs_prefix();
    let layout = Layout {
        prefix: PathBuf::from(prefix),
    };
    let argv = verb_args("claude", &layout, 2_001_280);
    assert_eq!(argv, os(&["__warm", "claude", prefix, "2001280"]));
    assert_eq!(
        Request::parse(&argv[1..]),
        Some(Request {
            spec: crate::vendor_direct::spec("claude").unwrap(),
            prefix: PathBuf::from(prefix),
            build: 2_001_280,
        })
    );
    for bad in [
        &["claude", prefix][..],
        &["claude", prefix, "7", "extra"],
        &["claude", "relative/pkg", "7"],
        &["claude", prefix, "seven"],
        &["claude", prefix, "+7"],
        &["claude", prefix, "07"],
        &["ay", prefix, "7"],
        &["", prefix, "7"],
    ] {
        assert_eq!(Request::parse(&os(bad)), None, "{bad:?}");
    }
}

/// A prefix that is not UTF-8 reaches the request byte for byte.
#[cfg(unix)]
#[test]
fn a_non_utf8_prefix_is_taken_byte_for_byte() {
    use std::os::unix::ffi::OsStrExt as _;
    let prefix = std::ffi::OsStr::from_bytes(b"/opt/p\xffkg").to_os_string();
    let request = Request::parse(&[
        OsString::from("claude"),
        prefix.clone(),
        OsString::from("7"),
    ])
    .unwrap();
    assert_eq!(request.prefix.as_os_str(), prefix);
}

#[test]
#[cfg(unix)]
fn setup_retargeting_cannot_change_the_build_a_warm_holds_and_launches() {
    let scratch = scratch("pinned ' build");
    let layout = &scratch.layout;
    let _bound =
        crate::packages_log::test_bind::bind(layout.prefix.join("logs"), layout.prefix.clone());
    let home = layout.prefix.join("home");
    signed_in_home(&home);
    let spec = crate::vendor_direct::spec("claude").unwrap();
    let tool = ToolName::new("claude").unwrap();
    let first = 2_001_298;
    let second = 2_001_299;
    install(layout, "claude", first, "inert first build\n");
    let first_build = layout.build_dir("claude", first);
    let first_exe = first_build.join("bin").join(tool.exe_file());
    let first_env = crate::shim_env::ShimEnv::admit(&[
        "DISABLE_AUTOUPDATER=1".into(),
        "WARM_BUILD=first '$ value".into(),
    ])
    .unwrap();
    crate::shim_env::write_sidecar(&first_build, &first_env).unwrap();
    let request = Request {
        spec,
        prefix: layout.prefix.clone(),
        build: first,
    };
    let inherited = vec![
        (OsString::from("HOME"), home.as_os_str().to_owned()),
        (OsString::from("ANTHROPIC_API_KEY"), OsString::from("inert")),
    ];
    let setup = || {
        Some(vec![
            (OsString::from("PATH"), OsString::from("owned login path")),
            (
                OsString::from("CLAUDE_CONFIG_DIR"),
                home.join(".claude").into_os_string(),
            ),
            (OsString::from("AI_AGENT"), OsString::from("not inherited")),
        ])
    };
    let captured = std::cell::Cell::new(false);
    let ended =
        warm_with_launch(&request, &inherited, setup, Some(&home), |command| {
            captured.set(true);
            assert!(crate::store::held_unproven(&first_build));
            assert_eq!(command.get_current_dir(), Some(home.as_path()));
            let args: Vec<_> = command.get_args().collect();
            let wanted: Vec<_> = spec
                .warm
                .unwrap()
                .argv
                .iter()
                .map(|arg| std::ffi::OsStr::new(*arg))
                .collect();
            assert!(args.ends_with(&wanted));
            assert!(command.get_envs().any(|(key, value)| key == "PATH"
                && value == Some(std::ffi::OsStr::new("owned login path"))));
            assert!(
                command
                    .get_envs()
                    .any(|(key, value)| key == "ANTHROPIC_API_KEY" && value.is_none())
            );
            assert!(!command.get_envs().any(|(key, _)| key == "AI_AGENT"));
            Ended::SpawnFailed
        });
    assert!(
        captured.get(),
        "ordinary setup reached the actual launch seam"
    );
    assert_eq!(ended, Some(Ended::SpawnFailed));
    assert!(!crate::store::held_unproven(&first_build));

    let login_ran = std::cell::Cell::new(false);
    let ended = warm_with_launch(
        &request,
        &inherited,
        || {
            login_ran.set(true);
            assert_eq!(crate::active_builds(layout).get("claude"), Some(&first));
            install(layout, "claude", second, "inert second build\n");
            let second_env =
                crate::shim_env::ShimEnv::admit(&["WARM_BUILD=second".into()]).unwrap();
            crate::shim_env::write_sidecar(&layout.build_dir("claude", second), &second_env)
                .unwrap();
            assert_eq!(crate::active_builds(layout).get("claude"), Some(&second));
            setup()
        },
        Some(&home),
        |command| {
            assert!(
                login_ran.get(),
                "the activation changed after the initial check"
            );
            assert!(crate::store::held_unproven(&first_build));
            assert!(!crate::store::held_unproven(
                &layout.build_dir("claude", second)
            ));
            assert_eq!(command.get_program(), std::ffi::OsStr::new("/bin/sh"));
            let expected_body =
                crate::platform::sh_shim_content_routed(&first_exe, &first_env, None);
            let mut expected = os(&["-c", &expected_body]);
            expected.push(layout.shim(&tool).into_os_string());
            expected.extend(
                spec.warm
                    .unwrap()
                    .argv
                    .iter()
                    .map(|arg| OsString::from(*arg)),
            );
            assert_eq!(
                command
                    .get_args()
                    .map(std::ffi::OsStr::to_os_string)
                    .collect::<Vec<_>>(),
                expected
            );
            assert!(!expected_body.contains("WARM_BUILD='second'"));
            Ended::SpawnFailed
        },
    );
    assert_eq!(ended, Some(Ended::SpawnFailed));
    assert!(!crate::store::held_unproven(&first_build));
}

#[test]
#[cfg(unix)]
fn the_command_pins_the_wrapper_with_the_table_argv_and_no_ai_variables() {
    let layout = Layout {
        prefix: PathBuf::from(abs_prefix()),
    };
    let claude = crate::vendor_direct::spec("claude").unwrap().warm.unwrap();
    let tool = ToolName::new("claude").unwrap();
    let inherited = [
        "CLAUDECODE",
        "CLAUDE_CODE_SIMPLE",
        "CLAUDE_CODE_SESSION_ID",
        "CLAUDE_CODE_MESSAGING_SOCKET",
        "AI_AGENT",
        "ANTHROPIC_API_KEY",
        "CLAUDE_CONFIG_DIR",
        "CODEX_HOME",
        "ANTHROPIC_BASE_URL",
        "CLAUDE_CODE_USE_BEDROCK",
        "CLAUDE_CODE_USE_VERTEX",
        "PATH",
        "HOME",
        "LANG",
    ]
    .map(OsString::from);
    let home = Path::new("/Users//someone");
    let envs = |command: &WarmCommand| -> Vec<(String, Option<String>)> {
        command
            .get_envs()
            .map(|(k, v)| {
                (
                    k.to_string_lossy().into_owned(),
                    v.map(|v| v.to_string_lossy().into_owned()),
                )
            })
            .collect()
    };
    let removed = |names: &[&str]| -> Vec<(String, Option<String>)> {
        names.iter().map(|k| ((*k).to_string(), None)).collect()
    };
    let session = [
        "AI_AGENT",
        "ANTHROPIC_API_KEY",
        "CLAUDECODE",
        "CLAUDE_CODE_MESSAGING_SOCKET",
        "CLAUDE_CODE_SESSION_ID",
        "CLAUDE_CODE_SIMPLE",
    ];
    let exe = layout
        .build_dir("claude", 7)
        .join("bin")
        .join(tool.exe_file());
    let command =
        warm_command(&layout, &tool, &exe, &claude, inherited.clone(), Some(home)).unwrap();
    assert_eq!(command.get_program(), "/bin/sh");
    let args: Vec<&std::ffi::OsStr> = command.get_args().collect();
    assert_eq!(args[0], "-c");
    assert_eq!(
        args[1],
        crate::platform::sh_shim_content_routed(&exe, &crate::shim_env::ShimEnv::NONE, None)
            .as_str()
    );
    assert_eq!(args[2], layout.shim(&tool).as_os_str());
    assert_eq!(args[3..], *claude.argv);
    assert_eq!(command.get_current_dir(), Some(home));
    let mut expected = removed(&session);
    expected.extend(removed(&["CODEX_HOME"]));
    expected.push((
        "ENABLE_CLAUDEAI_MCP_SERVERS".to_string(),
        Some("false".to_string()),
    ));
    expected.sort();
    assert_eq!(
        envs(&command),
        expected,
        "claude's config home, its endpoint choice, PATH, HOME and LANG are inherited"
    );
    let codex = crate::vendor_direct::spec("codex").unwrap().warm.unwrap();
    let tool = ToolName::new("codex").unwrap();
    let exe = layout
        .build_dir("codex", 8)
        .join("bin")
        .join(tool.exe_file());
    let command = warm_command(&layout, &tool, &exe, &codex, inherited, Some(home)).unwrap();
    let mut expected = removed(&session);
    expected.extend(removed(&[
        "ANTHROPIC_BASE_URL",
        "CLAUDE_CODE_USE_BEDROCK",
        "CLAUDE_CODE_USE_VERTEX",
        "CLAUDE_CONFIG_DIR",
    ]));
    expected.sort();
    assert_eq!(envs(&command), expected, "CODEX_HOME is inherited");
    let command = warm_command(&layout, &tool, &exe, &codex, [], None).unwrap();
    assert_eq!(command.get_current_dir(), Some(layout.prefix.as_path()));
    let odd = Warm {
        argv: &["", "a b", "'\"$word", "--", "line\nbreak"],
        ..codex
    };
    let command = warm_command(&layout, &tool, &exe, &odd, [], None).unwrap();
    assert_eq!(command.get_args().skip(3).collect::<Vec<_>>(), odd.argv);
}

#[test]
#[cfg(unix)]
fn an_unrepresentable_target_never_falls_back_to_the_active_shim() {
    use std::os::unix::ffi::OsStringExt as _;
    let layout = Layout {
        prefix: PathBuf::from("/opt/pkg"),
    };
    let tool = ToolName::new("claude").unwrap();
    let warm = crate::vendor_direct::spec("claude").unwrap().warm.unwrap();
    let exe = PathBuf::from(OsString::from_vec(
        b"/opt/p\xffkg/store/claude/7/bin/claude".to_vec(),
    ));
    assert!(warm_command(&layout, &tool, &exe, &warm, [], None).is_none());
}

#[test]
#[cfg(not(unix))]
fn an_unsupported_wrapper_never_falls_back_to_the_active_shim() {
    let layout = Layout {
        prefix: PathBuf::from(abs_prefix()),
    };
    let tool = ToolName::new("claude").unwrap();
    let warm = crate::vendor_direct::spec("claude").unwrap().warm.unwrap();
    let exe = layout
        .build_dir("claude", 7)
        .join("bin")
        .join(tool.exe_file());
    assert!(warm_command(&layout, &tool, &exe, &warm, [], None).is_none());
}

#[test]
fn an_opted_out_or_never_run_program_is_declined() {
    let scratch = scratch("declined");
    let home = scratch.layout.prefix.join("home");
    std::fs::create_dir_all(&home).unwrap();
    let env = |pairs: &[(&str, &str)]| -> Vec<(OsString, OsString)> {
        pairs
            .iter()
            .map(|(k, v)| (OsString::from(k), OsString::from(v)))
            .collect()
    };
    let claude = &crate::vendor_direct::spec("claude").unwrap().warm.unwrap();
    let codex = &crate::vendor_direct::spec("codex").unwrap().warm.unwrap();
    let none = env(&[]);
    let h = Some(home.as_path());
    assert_eq!(declined(claude, &none, h), Some(Ended::NeverRun));
    assert_eq!(declined(codex, &none, h), Some(Ended::NeverRun));
    assert_eq!(declined(claude, &none, None), Some(Ended::NeverRun));
    signed_in_home(&home);
    assert_eq!(declined(claude, &none, h), None, "the negative control");
    assert_eq!(declined(codex, &none, h), None);
    let elsewhere = scratch.layout.prefix.join("elsewhere");
    let moved = env(&[
        ("CLAUDE_CONFIG_DIR", elsewhere.to_str().unwrap()),
        ("CODEX_HOME", elsewhere.to_str().unwrap()),
    ]);
    assert_eq!(declined(claude, &moved, h), Some(Ended::NeverRun));
    assert_eq!(declined(codex, &moved, h), Some(Ended::NeverRun));
    std::fs::create_dir_all(elsewhere.join(".claude")).unwrap();
    std::fs::write(elsewhere.join(".claude/.claude.json"), SIGNED_IN).unwrap();
    std::fs::write(elsewhere.join("auth.json"), "{}").unwrap();
    assert_eq!(
        declined(claude, &moved, h),
        Some(Ended::SignedOut),
        "a named home's profile is its own, not one under it"
    );
    std::fs::write(elsewhere.join(".claude.json"), SIGNED_IN).unwrap();
    assert_eq!(declined(claude, &moved, h), None);
    assert_eq!(declined(codex, &moved, h), None);
    let off = env(&[("CLAUDE_CODE_DISABLE_NONESSENTIAL_TRAFFIC", "1")]);
    assert_eq!(declined(claude, &off, h), Some(Ended::OptedOut));
    assert_eq!(declined(codex, &off, h), None);
    let empty = env(&[("CLAUDE_CODE_DISABLE_NONESSENTIAL_TRAFFIC", "")]);
    assert_eq!(declined(claude, &empty, h), None);
    let stateless = Warm {
        state: None,
        ..*claude
    };
    assert_eq!(
        declined(&stateless, &none, None),
        Some(Ended::SignedOut),
        "no home to read a login from"
    );
    assert_eq!(declined(&stateless, &none, h), None);
}

/// A program is warmed only where its own files or kept variables show it signed in: a
/// home it has run in without a login declines, whatever else is there. Control: the same
/// home with the login declines nothing.
#[test]
fn a_program_that_is_not_signed_in_is_declined() {
    let scratch = scratch("signed-out");
    let home = scratch.layout.prefix.join("home");
    let env = |pairs: &[(&str, &str)]| -> Vec<(OsString, OsString)> {
        pairs
            .iter()
            .map(|(k, v)| (OsString::from(k), OsString::from(v)))
            .collect()
    };
    let claude = &crate::vendor_direct::spec("claude").unwrap().warm.unwrap();
    let codex = &crate::vendor_direct::spec("codex").unwrap().warm.unwrap();
    let (none, h) = (env(&[]), Some(home.as_path()));
    std::fs::create_dir_all(home.join(".claude")).unwrap();
    std::fs::create_dir_all(home.join(".codex")).unwrap();
    assert_eq!(declined(claude, &none, h), Some(Ended::SignedOut));
    assert_eq!(declined(codex, &none, h), Some(Ended::SignedOut));
    // What a signed-out program leaves behind is no login: a profile with no account, one
    // that is not JSON, an empty login file.
    for profile in [
        "{}",
        r#"{"oauthAccount":null}"#,
        r#"{"oauthAccount":{}}"#,
        "not json",
    ] {
        std::fs::write(home.join(".claude.json"), profile).unwrap();
        assert_eq!(
            declined(claude, &none, h),
            Some(Ended::SignedOut),
            "{profile}"
        );
    }
    std::fs::write(home.join(".codex/auth.json"), "").unwrap();
    assert_eq!(declined(codex, &none, h), Some(Ended::SignedOut));
    // An API key is stripped before the warm, so it signs nothing in; a kept cloud switch does.
    let key = env(&[("ANTHROPIC_API_KEY", "inert"), ("OPENAI_API_KEY", "inert")]);
    assert_eq!(declined(claude, &key, h), Some(Ended::SignedOut));
    assert_eq!(declined(codex, &key, h), Some(Ended::SignedOut));
    let cloud = env(&[("CLAUDE_CODE_USE_BEDROCK", "1")]);
    assert_eq!(declined(claude, &cloud, h), None);
    for name in claude.login.env {
        assert!(claude.keep.contains(name), "{name} reaches the program");
    }
    // Each login, alone.
    std::fs::write(home.join(".claude/.credentials.json"), "{}").unwrap();
    assert_eq!(declined(claude, &none, h), None, "its login file");
    std::fs::remove_file(home.join(".claude/.credentials.json")).unwrap();
    std::fs::write(home.join(".claude.json"), SIGNED_IN).unwrap();
    assert_eq!(declined(claude, &none, h), None, "its profile's account");
    std::fs::write(home.join(".codex/auth.json"), "{}").unwrap();
    assert_eq!(declined(codex, &none, h), None);
}

/// Nothing of the person's runs for an account that is not signed in: their login shell
/// is never asked and the program never launched, and the one log line says why. Control:
/// the same warm in a signed-in home asks the shell and reaches the launch.
#[cfg(unix)]
#[test]
fn a_signed_out_account_runs_neither_the_login_shell_nor_the_program() {
    let scratch = scratch("signed-out-runs");
    let layout = &scratch.layout;
    let logs = layout.prefix.join("logs");
    let _bound = crate::packages_log::test_bind::bind(logs.clone(), layout.prefix.clone());
    let home = layout.prefix.join("home");
    std::fs::create_dir_all(home.join(".claude")).unwrap();
    install(layout, "claude", 2_001_300, "#!/bin/sh\nexit 0\n");
    let request = Request {
        spec: crate::vendor_direct::spec("claude").unwrap(),
        prefix: layout.prefix.clone(),
        build: 2_001_300,
    };
    let (asked, launched) = (std::cell::Cell::new(0), std::cell::Cell::new(0));
    let run =
        || {
            warm_with_launch(
                &request,
                &[],
                || {
                    asked.set(asked.get() + 1);
                    Some(vec![(OsString::from("BROWSER"), OsString::from("open-me"))])
                },
                Some(&home),
                |command| {
                    launched.set(launched.get() + 1);
                    // Whatever the person's setup names, the program is given no browser, no
                    // askpass and no display.
                    for (name, value) in NO_DESK {
                        assert!(
                            command.get_envs().any(|(key, set)| key == *name
                                && set == Some(std::ffi::OsStr::new(value))),
                            "{name}"
                        );
                    }
                    for name in NO_DISPLAY {
                        assert!(
                            command
                                .get_envs()
                                .any(|(key, set)| key == *name && set.is_none())
                        );
                    }
                    Ended::Exit(0)
                },
            )
        };
    assert_eq!(run(), Some(Ended::SignedOut));
    assert_eq!((asked.get(), launched.get()), (0, 0));
    let last = crate::packages_log::read_tail(&logs, 1);
    assert_eq!(last[0].get("outcome"), Some("signed-out"));
    signed_in_home(&home);
    assert_eq!(run(), Some(Ended::Exit(0)));
    assert_eq!((asked.get(), launched.get()), (1, 1), "the control");
}

/// The warmed program and the login shell each run in a session of their own, so neither
/// has a controlling terminal to prompt on, and the login shell is given no desk either.
/// Control: a child started the ordinary way stays in this process's session.
#[cfg(unix)]
#[test]
fn the_program_and_the_login_shell_run_in_sessions_of_their_own() {
    let scratch = scratch("session");
    let layout = &scratch.layout;
    install(layout, "claude", 7, "#!/bin/sh\nexit 0\n");
    let tool = ToolName::new("claude").unwrap();
    let warm = crate::vendor_direct::spec("claude").unwrap().warm.unwrap();
    let exe = layout
        .build_dir("claude", 7)
        .join("bin")
        .join(tool.exe_file());
    let session_of = |command: &mut WarmCommand| {
        let mut child = command
            .stdin(aterm_uds::spawnfd::Stdio::piped())
            .stdout(aterm_uds::spawnfd::Stdio::null())
            .spawn()
            .unwrap();
        let pid = libc::pid_t::try_from(child.id()).unwrap();
        // SAFETY: `getsid` reads the session of a child this test holds unreaped.
        let session = unsafe { libc::getsid(pid) };
        kill_group(&mut child);
        let _ = child.wait();
        (session, pid)
    };
    let mut program = warm_command(layout, &tool, &exe, &warm, [], None).unwrap();
    let (session, pid) = session_of(&mut program);
    assert_eq!(session, pid, "the program leads its own session");
    let mut shell = login_shell_command(Path::new("/bin/cat"));
    for (name, value) in NO_DESK {
        assert!(
            shell
                .get_envs()
                .any(|(key, set)| key == *name && set == Some(std::ffi::OsStr::new(value))),
            "{name}"
        );
    }
    let (session, pid) = session_of(&mut shell);
    assert_eq!(session, pid, "the login shell leads its own session");
    let mut plain = aterm_uds::spawnfd::command("/bin/cat");
    let (session, pid) = session_of(&mut plain);
    // SAFETY: `getsid(0)` reads this process's own session.
    assert_eq!(session, unsafe { libc::getsid(0) }, "the control");
    assert_ne!(session, pid);
}

/// In-process with short durations: a program that exits on stdin EOF ends once the
/// hold closes it, with its own code; one that ignores EOF and SIGTERM is killed with
/// its whole group at the deadline.
#[cfg(unix)]
#[test]
fn the_warm_closes_stdin_at_the_hold_and_kills_the_group_at_the_deadline() {
    let scratch = scratch("drive");
    let layout = &scratch.layout;
    let marks = layout.prefix.join("marks");
    std::fs::create_dir_all(&marks).unwrap();
    let tool = ToolName::new("claude").unwrap();
    let spec = crate::vendor_direct::spec("claude").unwrap();
    let spawn = || {
        let exe = layout
            .build_dir("claude", crate::active_builds(layout)["claude"])
            .join("bin")
            .join(tool.exe_file());
        aterm_spec::wait::past_text_busy(|| {
            warm_command(layout, &tool, &exe, &spec.warm.unwrap(), [], None)
                .unwrap()
                .spawn()
        })
        .unwrap()
    };
    install(
        layout,
        "claude",
        2_001_280,
        &format!(
            "#!/bin/sh\necho up > '{m}/started'\ncat >/dev/null\necho eof > '{m}/eof'\n\
             exit 3\n",
            m = marks.display()
        ),
    );
    let child = spawn();
    assert!(appears(&marks.join("started")), "the script ran");
    assert_eq!(
        drive(child, Duration::from_millis(300), Duration::from_secs(120)),
        Ended::Exit(3)
    );
    assert!(marks.join("eof").exists());
    install(
        layout,
        "claude",
        2_001_281,
        &format!(
            "#!/bin/sh\ntrap '' TERM\nsleep 120 &\necho $! > '{}/grandchild.tmp'\n\
             mv '{0}/grandchild.tmp' '{0}/grandchild'\nwhile :; do sleep 1; done\n",
            marks.display()
        ),
    );
    let child = spawn();
    assert!(appears(&marks.join("grandchild")), "the script ran");
    let grandchild: libc::pid_t = std::fs::read_to_string(marks.join("grandchild"))
        .unwrap()
        .trim()
        .parse()
        .unwrap();
    assert_eq!(
        drive(child, Duration::from_millis(50), Duration::from_millis(300)),
        Ended::Killed
    );
    let gone = (0..3000).any(|_| {
        // SAFETY: signal 0 only asks whether the pid is addressable.
        let alive = unsafe { libc::kill(grandchild, 0) } == 0;
        if alive {
            std::thread::sleep(Duration::from_millis(20));
        }
        !alive
    });
    assert!(gone, "the group went with it");
}

/// The warm's child holds its stdio and nothing else of this process. A descriptor held
/// without close-on-exec (as another thread's socketpair is before its `F_SETFD`),
/// above 99 so no shell's own open lands on its number, reaches the same shim started
/// by the pass-throughs' spawn, `aterm_uds::spawnfd::transparent` (the negative
/// control: they hand their caller's descriptors on), and not the warm's.
#[cfg(unix)]
#[test]
fn the_warm_child_inherits_its_stdio_and_nothing_else() {
    use std::os::fd::{AsRawFd as _, FromRawFd as _};
    let scratch = scratch("inherit");
    let layout = &scratch.layout;
    let marks = layout.prefix.join("marks");
    std::fs::create_dir_all(&marks).unwrap();
    let tool = ToolName::new("claude").unwrap();
    let spec = crate::vendor_direct::spec("claude").unwrap();
    let null = std::fs::File::open("/dev/null").unwrap();
    // SAFETY: F_DUPFD duplicates a descriptor this test owns onto the lowest free number from 100 (and close-on-exec clear); the new one is owned below.
    let fd = unsafe { libc::fcntl(null.as_raw_fd(), libc::F_DUPFD, 100) };
    assert!(fd >= 100, "F_DUPFD: {}", std::io::Error::last_os_error());
    // SAFETY: `fd` was just opened here and nothing else owns it.
    let _held = unsafe { std::os::fd::OwnedFd::from_raw_fd(fd) };
    install(
        layout,
        "claude",
        2_001_282,
        &format!(
            "#!/bin/sh
if {held}; then w=held; else w=absent; fi
             echo $w > '{m}/fd.tmp'
mv '{m}/fd.tmp' '{m}/fd'
",
            held = aterm_spec::host::sh_held(fd),
            m = marks.display()
        ),
    );
    let seen = || {
        let mark = marks.join("fd");
        assert!(appears(&mark), "the script ran");
        let what = std::fs::read_to_string(&mark).unwrap().trim().to_string();
        std::fs::remove_file(&mark).unwrap();
        what
    };
    let mut control = aterm_uds::spawnfd::transparent(layout.shim(&tool))
        .spawn()
        .unwrap();
    assert_eq!(
        seen(),
        "held",
        "the negative control: the pass-throughs' spawn hands it on"
    );
    assert!(control.wait().unwrap().success());
    let mut child = aterm_spec::wait::past_text_busy(|| {
        let exe = layout
            .build_dir("claude", 2_001_282)
            .join("bin")
            .join(tool.exe_file());
        warm_command(layout, &tool, &exe, &spec.warm.unwrap(), [], None)
            .unwrap()
            .spawn()
    })
    .unwrap();
    assert_eq!(seen(), "absent");
    drop(child.stdin.take());
    let _ = child.wait();
}

/// Each warm that ran or was declined is one `warm` line — its own exit code, `killed`,
/// `spawn-failed`, `opted-out`, `never-run`, `signed-out` — and a stale build runs and logs
/// nothing.
#[cfg(unix)]
#[test]
fn each_warm_is_one_log_line_and_a_stale_build_is_none() {
    let scratch = scratch("record");
    let layout = &scratch.layout;
    let logs = layout.prefix.join("logs");
    let _bound = crate::packages_log::test_bind::bind(logs.clone(), layout.prefix.clone());
    let home = layout.prefix.join("home");
    signed_in_home(&home);
    let spec = crate::vendor_direct::spec("claude").unwrap();
    let request = |build| Request {
        spec,
        prefix: layout.prefix.clone(),
        build,
    };
    let no_env: Vec<(OsString, OsString)> = Vec::new();
    let run = |build, env: &[(OsString, OsString)], home: &Path, deadline| {
        warm(
            &request(build),
            env,
            || Some(Vec::new()),
            Some(home),
            Duration::ZERO,
            deadline,
        )
    };
    install(
        layout,
        "claude",
        2_001_280,
        "#!/bin/sh\ncat >/dev/null\nexit 3\n",
    );
    assert_eq!(
        run(2_001_280, &no_env, &home, Duration::from_secs(120)),
        Some(Ended::Exit(3))
    );
    install(
        layout,
        "claude",
        2_001_281,
        "#!/bin/sh\ntrap '' TERM\nwhile :; do sleep 1; done\n",
    );
    assert_eq!(
        run(2_001_281, &no_env, &home, Duration::from_millis(300)),
        Some(Ended::Killed)
    );
    let before = crate::packages_log::read_tail(&logs, 100).len();
    assert_eq!(run(2_001_280, &no_env, &home, Duration::from_secs(1)), None);
    assert_eq!(crate::packages_log::read_tail(&logs, 100).len(), before);
    let off = vec![(
        OsString::from("CLAUDE_CODE_DISABLE_NONESSENTIAL_TRAFFIC"),
        OsString::from("1"),
    )];
    assert_eq!(
        run(2_001_281, &off, &home, Duration::from_secs(1)),
        Some(Ended::OptedOut)
    );
    assert_eq!(
        run(2_001_281, &no_env, &layout.prefix, Duration::from_secs(1)),
        Some(Ended::NeverRun)
    );
    let signed_out = layout.prefix.join("signed-out");
    std::fs::create_dir_all(signed_out.join(".claude")).unwrap();
    assert_eq!(
        run(2_001_281, &no_env, &signed_out, Duration::from_secs(1)),
        Some(Ended::SignedOut)
    );
    let exe = layout
        .build_dir("claude", 2_001_281)
        .join("bin")
        .join(ToolName::new("claude").unwrap().exe_file());
    {
        use std::os::unix::fs::PermissionsExt as _;
        std::fs::set_permissions(&exe, std::fs::Permissions::from_mode(0o644)).unwrap();
    }
    assert_eq!(crate::active_builds(layout).get("claude"), Some(&2_001_281));
    assert_eq!(
        run(2_001_281, &no_env, &home, Duration::from_secs(1)),
        Some(Ended::Exit(126))
    );
    let outcomes: Vec<(String, String, String)> = crate::packages_log::read_tail(&logs, 100)
        .iter()
        .map(|e| {
            assert_eq!(e.kind, crate::packages_log::kind::WARM);
            assert!(e.get("ms").is_some());
            (
                e.get("program").unwrap().to_string(),
                e.get("build").unwrap().to_string(),
                e.get("outcome").unwrap().to_string(),
            )
        })
        .collect();
    let row = |b: &str, o: &str| ("claude".to_string(), b.to_string(), o.to_string());
    assert_eq!(
        outcomes,
        [
            row("2001281", "exit=126"),
            row("2001281", "signed-out"),
            row("2001281", "never-run"),
            row("2001281", "opted-out"),
            row("2001281", "killed"),
            row("2001280", "exit=3"),
        ]
    );
}

/// The warmed build is held from before its program starts: a program that exits on its
/// own releases it, one killed at the deadline leaves it held for every process, and a
/// later run that never starts takes nothing from that hold.
#[cfg(unix)]
#[test]
fn a_warm_holds_its_build_until_the_program_ended_on_its_own() {
    let scratch = scratch("hold");
    let layout = &scratch.layout;
    let _bound =
        crate::packages_log::test_bind::bind(layout.prefix.join("logs"), layout.prefix.clone());
    let home = layout.prefix.join("home");
    signed_in_home(&home);
    let spec = crate::vendor_direct::spec("claude").unwrap();
    let run = |build, deadline| {
        let request = Request {
            spec,
            prefix: layout.prefix.clone(),
            build,
        };
        warm(
            &request,
            &[],
            || Some(Vec::new()),
            Some(&home),
            Duration::ZERO,
            deadline,
        )
    };
    let mark = |build| {
        crate::store::sidecar_path(
            &layout.build_dir("claude", build),
            crate::store::FIRST_EXEC_MARKS_SUFFIX,
        )
        .unwrap()
    };
    install(
        layout,
        "claude",
        2_001_290,
        &format!(
            "#!/bin/sh\ntest -d '{}' && exit 4\nexit 5\n",
            mark(2_001_290).display()
        ),
    );
    assert_eq!(
        run(2_001_290, Duration::from_secs(120)),
        Some(Ended::Exit(4)),
        "the hold is on disk while the program runs"
    );
    assert!(!mark(2_001_290).exists(), "an exit of its own releases it");
    assert!(!crate::store::held_unproven(
        &layout.build_dir("claude", 2_001_290)
    ));

    install(
        layout,
        "claude",
        2_001_291,
        "#!/bin/sh\ntrap '' TERM\nwhile :; do sleep 1; done\n",
    );
    assert_eq!(
        run(2_001_291, Duration::from_millis(300)),
        Some(Ended::Killed)
    );
    assert!(
        mark(2_001_291).is_dir(),
        "a killed program leaves its build held"
    );
    assert!(crate::store::held_unproven(
        &layout.build_dir("claude", 2_001_291)
    ));

    let exe = layout
        .build_dir("claude", 2_001_291)
        .join("bin")
        .join(ToolName::new("claude").unwrap().exe_file());
    {
        use std::os::unix::fs::PermissionsExt as _;
        std::fs::set_permissions(&exe, std::fs::Permissions::from_mode(0o644)).unwrap();
    }
    assert_eq!(
        run(2_001_291, Duration::from_secs(1)),
        Some(Ended::Exit(126))
    );
    assert!(
        mark(2_001_291).is_dir(),
        "a run that never started leaves the earlier hold standing"
    );
}
