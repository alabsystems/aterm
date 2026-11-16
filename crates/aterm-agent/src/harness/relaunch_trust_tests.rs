// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

//! A relaunch plan settles its folder's trust under the Claude home its own line names
//! (`launch_trust`), before the line is built.

use std::cell::RefCell;

use super::fable_tests::opts_in;
use super::*;

/// One ask a plan made of its kernel: the line's folder, variables, program and argv.
type Asked = (String, Vec<(String, String)>, PathBuf, Vec<String>);

/// A kernel that reads nothing of the machine, reads the shell as zsh, and notes each folder
/// trust it is asked to settle.
#[derive(Default)]
struct Settling {
    asked: RefCell<Vec<Asked>>,
}

impl Kernel for Settling {
    fn parent(&self, _: u32) -> Option<u32> {
        None
    }
    fn terminal(&self, _: u32) -> crate::harness::upgrade_drive::Fact<(u32, String)> {
        Ok(None)
    }
    fn process_start(&self, _: u32) -> Option<String> {
        None
    }
    fn restart_background(&self, _: u32) -> Vec<String> {
        Vec::new()
    }
    fn restart_held(&self, _: u32) -> Option<Vec<crate::harness::upgrade::Held>> {
        Some(Vec::new())
    }
    fn shell_dialect(&self, _: u32, _: &[super::super::upgrade_drive::ProcRow]) -> Option<Dialect> {
        Some(Dialect::Zsh)
    }
    fn settle_trust(&self, _: &Opts, line: &crate::harness::launch_trust::Line<'_>) {
        self.asked.borrow_mut().push((
            line.cwd.to_string(),
            line.env
                .iter()
                .map(|(name, value)| ((*name).to_string(), (*value).to_string()))
                .collect(),
            line.exe.to_path_buf(),
            line.argv.to_vec(),
        ));
    }
}

/// A PLAN THAT MOVES A SESSION TO A SECOND CLAUDE HOME SETTLES THE FOLDER UNDER THAT HOME: the
/// kernel is asked once, before the line is built, with the line's own folder, program, argv
/// and the Claude home the line sets, so the trust is read and recorded in the profile the
/// relaunched session will read. A line that names no home asks with none (the default
/// profile's). CONTROL: a dry run, which starts nothing, asks nothing.
#[test]
fn a_plan_settles_the_folder_under_the_claude_home_its_line_names() {
    const SESSION: &str = "0b5e7a11-f100-4a2b-9c3d-5e6f70819a2b";
    // A pid and shell no process holds, so no read finds this test among them.
    const PID: u32 = 4_000_000_001;
    const SHELL: u32 = 4_000_000_002;
    let tmp = aterm_tempfile::TempDir::new().expect("tmp");
    let argv: Vec<String> = ["/opt/claude", "--model", "opus"]
        .iter()
        .map(|w| (*w).to_string())
        .collect();
    let launch = |home: Option<&'static str>| Launch {
        session: SESSION,
        resume: true,
        cwd: "/w/project",
        agent: PID,
        argv: &argv,
        exe: Path::new("/opt/claude"),
        mode: None,
        home,
        homedir: None,
        login: None,
        route: accounts::AuthRoute::Outside,
        running: None,
        auth_observed: true,
        launch_env: None,
        serve: Serve::Never,
    };
    let asked = |dry_run: bool, home: Option<&'static str>| {
        let opts = Opts {
            dry_run,
            ..opts_in(tmp.path())
        };
        let kernel = Settling::default();
        // Whatever the plan then answers: the ask comes before its line is built.
        let _ = plan_with_selection(&opts, &launch(home), SHELL, &[], None, false, &kernel);
        kernel.asked.into_inner()
    };
    assert_eq!(
        asked(false, Some("/homes/second")),
        [(
            "/w/project".to_string(),
            vec![(accounts::HOME_ENV.to_string(), "/homes/second".to_string())],
            PathBuf::from("/opt/claude"),
            argv.clone(),
        )]
    );
    assert_eq!(
        asked(false, None),
        [(
            "/w/project".to_string(),
            Vec::new(),
            PathBuf::from("/opt/claude"),
            argv.clone(),
        )]
    );
    assert_eq!(
        asked(true, Some("/homes/second")),
        [],
        "a dry run asks nothing"
    );
}

/// An executable stand-in for a Claude Code build at `path`, which writes its arguments to
/// `<out>/argv`, records the trust of the folder it runs in as Claude Code does, answers that it
/// did, and goes when its stdin ends.
fn stand_in(path: &Path, out: &Path) {
    use std::os::unix::fs::PermissionsExt as _;
    std::fs::create_dir_all(path.parent().expect("the build's folder")).expect("its folder");
    std::fs::create_dir_all(out).expect("its notes");
    std::fs::write(
        path,
        format!(
            "#!/bin/sh\nprintf '%s\\n' \"$@\" > '{}/argv'\nIFS= read -r line\n\
             printf '{{\"projects\":{{\"%s\":{{\"hasTrustDialogAccepted\":true}}}}}}\\n' \
             \"$(pwd -P)\" > \"$CLAUDE_CONFIG_DIR/.claude.json\"\n\
             echo '{{\"type\":\"control_response\",\"response\":{{\"subtype\":\"success\",\
             \"request_id\":\"aterm-trust\",\"response\":{{\"claude_code_version\":\"2.1.323\",\
             \"workspace_trust_recorded\":true}}}}}}'\ncat > /dev/null\n",
            out.display()
        ),
    )
    .expect("the stand-in");
    std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o755)).expect("its mode");
}

/// THE LIVE KERNEL'S PLAN STARTS ONLY A CLAUDE CODE BUILD OF ATPKG'S OWN STORE, AND ONLY IN A
/// SESSION NOTHING HOLDS. A plan made as the window makes it (`plan`, over the live kernel)
/// reads the tab from the drive's own socket, starts the store build headless with atpkg's warm
/// arguments under the Claude home the line names, and books the trust it recorded in the tab's
/// ledger; the plan after it finds the record and asks the window nothing. CONTROLS: a relaunch
/// whose program lies outside that store (a person's wrapper, another install, a test's
/// stand-in, here a build of the very same shape in another store) is never started, and the
/// window is not even asked; and under a hold on the session (a person took the tab over), or
/// a `[harness]` file that trusts no folder, the store build is not started either.
#[test]
fn a_live_plan_starts_only_a_store_build_and_only_in_a_session_nothing_holds() {
    const SESSION: &str = "0b5e7a11-f100-4a2b-9c3d-5e6f70819a2b";
    const SHELL: u32 = 999_999;
    let tmp = aterm_tempfile::TempDir::new().expect("tmp");
    let root = std::fs::canonicalize(tmp.path()).expect("resolved");
    let store = root.join("store/claude");
    let build = store.join("7/bin/claude");
    let outside = root.join("other/store/claude/7/bin/claude");
    stand_in(&build, &root.join("ran-store"));
    stand_in(&outside, &root.join("ran-outside"));
    let (folder, claude_home, state) = (
        root.join("work"),
        root.join("claude-home"),
        root.join("state"),
    );
    for dir in [&folder, &claude_home] {
        std::fs::create_dir_all(dir).expect("a folder");
    }
    let hold = std::sync::Arc::new(std::sync::Mutex::new("1"));
    let says = std::sync::Arc::clone(&hold);
    let (sock, asked) =
        crate::harness::fake_instance(&root.join("t.sock"), "a-token", move |line| {
            match line.split_whitespace().find(|w| !w.starts_with('@')) {
                Some("meta") => "OK cwd=%2Fw trusted_cwd=- autopilot=-".to_string(),
                Some("status") => format!(
                    "OK schema=1 sid=s-1 hold={} supervisor=- autopilot=cruise",
                    says.lock().expect("the hold")
                ),
                _ => "ERR unscripted".to_string(),
            }
        });
    let opts = Opts {
        dry_run: false,
        sock: Some(sock),
        aterm_state: Some(state.clone()),
        ..opts_in(&root)
    };
    let (folder_s, home_s) = (
        folder.to_string_lossy().into_owned(),
        claude_home.to_string_lossy().into_owned(),
    );
    let plan_of = |exe: &Path| {
        let argv = vec![
            exe.to_string_lossy().into_owned(),
            "--model".to_string(),
            "opus".to_string(),
        ];
        let launch = Launch {
            session: SESSION,
            resume: true,
            cwd: &folder_s,
            agent: 0,
            argv: &argv,
            exe,
            mode: None,
            home: Some(&home_s),
            homedir: None,
            login: None,
            route: accounts::AuthRoute::Outside,
            running: None,
            auth_observed: true,
            launch_env: None,
            serve: Serve::Never,
        };
        TOKEN_BUILDS.with(|seam| *seam.borrow_mut() = Some(store.clone()));
        // Whatever the plan then answers: the folder is settled before its line is built.
        let _ = plan(&opts, &launch, SHELL, &[(SHELL, 1, "zsh".into())]);
        TOKEN_BUILDS.with(|seam| *seam.borrow_mut() = None);
    };
    let asked_of = || asked.lock().expect("the asks").clone();
    let ran = |name: &str| root.join(name).join("argv").exists();
    let profile = claude_home.join(".claude.json");
    let ledger = state.join("drive/s-1.jsonl");

    // Under a hold: the window is read, and nothing is started.
    plan_of(&build);
    assert_eq!(asked_of(), ["@s-1 meta", "@s-1 status"]);
    assert!(!ran("ran-store"), "started under a hold");
    assert!(!profile.exists() && !ledger.exists());

    // A build outside atpkg's own store: never started, and nothing is read for it.
    *hold.lock().expect("the hold") = "0";
    plan_of(&outside);
    assert_eq!(
        asked_of().len(),
        2,
        "the window was asked for another's build"
    );
    assert!(!ran("ran-outside"), "started a build outside atpkg's store");
    assert!(!profile.exists() && !ledger.exists());

    // The store build, under a `[harness]` file that trusts no folder: read, and not started.
    let policy = root.join("aterm.toml");
    std::fs::write(&policy, "[harness]\napprove = \"none\"\n").expect("the policy file");
    let under = |file: Option<&Path>| {
        crate::harness::launch_trust::POLICY_FILE
            .with(|seam| *seam.borrow_mut() = file.map(Path::to_path_buf));
    };
    under(Some(&policy));
    plan_of(&build);
    under(None);
    assert_eq!(asked_of().len(), 4);
    assert!(!ran("ran-store"), "started under approve = \"none\"");
    assert!(!profile.exists() && !ledger.exists());

    // The store build, in a session nothing holds, under the table's defaults.
    plan_of(&build);
    assert_eq!(asked_of().len(), 6);
    let warm = atpkg::vendor_direct::spec("claude")
        .and_then(|spec| spec.warm)
        .expect("Claude Code's warm start");
    assert_eq!(
        std::fs::read_to_string(root.join("ran-store/argv"))
            .expect("the store build ran")
            .lines()
            .collect::<Vec<_>>(),
        warm.argv
    );
    assert!(crate::harness::launch_trust::recorded(&profile, &folder));
    let rows = std::fs::read_to_string(&ledger).expect("the tab's ledger");
    assert_eq!(rows.lines().count(), 1, "{rows}");
    assert!(
        rows.contains("\"rule_id\":\"trust-any@v1\"")
            && rows.contains("recorded before launch by Claude Code 2.1.323"),
        "{rows}"
    );

    // The plan after it finds the record: no read of the window, no second row.
    plan_of(&build);
    assert_eq!(asked_of().len(), 6);
    assert_eq!(
        std::fs::read_to_string(&ledger)
            .expect("the tab's ledger")
            .lines()
            .count(),
        1
    );
}
