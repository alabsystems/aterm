// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;

#[cfg(unix)]
#[test]
fn a_required_launchctl_child_waits_for_its_actual_reply() {
    use std::time::Duration;

    let program = aterm_uds::deadline::stand_in(Path::new("/bin/sh"));
    let argv = ["-c", "printf ready"].map(str::to_owned);
    let output = launchctl_after(&program, &argv, Duration::from_secs(31)).unwrap();
    assert!(output.status.success());
    assert_eq!(output.stdout, b"ready");

    let argv = ["-c", "printf refused >&2; exit 7"].map(str::to_owned);
    let output = launchctl_after(&program, &argv, Duration::from_secs(31)).unwrap();
    assert_eq!(output.status.code(), Some(7));
    assert_eq!(output.stderr, b"refused");

    let root = aterm_tempfile::tempdir().unwrap();
    let marker = root.path().join("must-not-exist");
    let argv = [
        "-c".to_owned(),
        "printf wrote > \"$1\"".to_owned(),
        "sh".to_owned(),
        marker.to_str().unwrap().to_owned(),
    ];
    let error = launchctl_after(&program, &argv, Duration::from_secs(61)).unwrap_err();
    assert_eq!(error.kind(), io::ErrorKind::TimedOut);
    assert!(!marker.exists(), "an expired command is never started");
}

#[test]
fn the_installed_app_and_dev_builds_never_share_a_label() {
    assert_eq!(label_for(Path::new(INSTALLED_EXECUTABLE)), INSTALLED_LABEL);
    let dev = label_for(Path::new("/Users//x/aterm/target/debug/aterm"));
    let short = dev
        .strip_prefix(&format!("{DEV_LABEL_PREFIX}."))
        .expect("the dev prefix");
    assert_eq!(short.len(), 8);
    assert!(
        short
            .bytes()
            .all(|b| b.is_ascii_hexdigit() && !b.is_ascii_uppercase())
    );
    assert_eq!(
        dev,
        label_for(Path::new("/Users//x/aterm/target/debug/aterm"))
    );
    assert_ne!(
        dev,
        label_for(Path::new("/Users//x/aterm2/target/debug/aterm"))
    );
    for other in [
        "/Users//x/Applications/aterm.app/Contents/MacOS/aterm",
        "/Applications/aterm (dev).app/Contents/MacOS/aterm",
    ] {
        assert_ne!(label_for(Path::new(other)), INSTALLED_LABEL, "{other}");
    }
}

/// aterm installs itself in `/Applications`, or in `~/Applications` for a person who cannot
/// write it (aterm-update's first launch): either is the installed app, and a dev build or
/// another bundle is not.
#[test]
fn an_install_in_either_applications_folder_is_installed() {
    let home = Path::new("/Users//x");
    let [system, own] = installed_executables(home);
    assert_eq!(system, Path::new(INSTALLED_EXECUTABLE));
    assert_eq!(
        own,
        Path::new("/Users//x/Applications/aterm.app/Contents/MacOS/aterm")
    );
    assert!(is_installed(&system, home));
    assert!(is_installed(&own, home), "a non-admin's install");
    for not in [
        "/Users//x/aterm/target/debug/aterm",
        "/Applications/aterm (dev).app/Contents/MacOS/aterm",
        "/Users//y/Applications/aterm.app/Contents/MacOS/aterm",
    ] {
        assert!(!is_installed(Path::new(not), home), "{not}");
    }
}

/// The pass keeps the first installed app that has the verb: its own executable without
/// asking; an older aterm is never started as the job; one that did not answer is asked
/// again by the next pass.
#[test]
fn the_pass_keeps_an_installed_app_that_serves_acaffeine() {
    let system = std::path::PathBuf::from(INSTALLED_EXECUTABLE);
    let own = std::path::PathBuf::from("/Users//x/Applications/aterm.app/Contents/MacOS/aterm");
    assert!(matches!(
        serving_install(&[], None, |_| panic!("nothing to ask")),
        Installed::Absent(_)
    ));
    assert_eq!(
        serving_install(std::slice::from_ref(&own), Some(&own), |_| panic!(
            "its own verb"
        )),
        Installed::At(own.clone())
    );
    assert_eq!(
        serving_install(&[system.clone(), own.clone()], None, |exe| Some(exe == own)),
        Installed::At(own.clone()),
        "0.144 in /Applications, a newer aterm in ~/Applications"
    );
    let older = serving_install(std::slice::from_ref(&system), None, |_| Some(false));
    assert!(
        matches!(&older, Installed::Absent(why) if why.contains("no `acaffeine` verb")),
        "{older:?}"
    );
    assert!(matches!(
        serving_install(&[system.clone(), own], None, |exe| (exe != system)
            .then_some(false)),
        Installed::Unanswered(_)
    ));
    assert_eq!(
        serving_install(std::slice::from_ref(&system), None, |_| Some(true)),
        Installed::At(system)
    );
}

#[test]
fn only_a_label_this_crate_makes_is_one() {
    assert!(is_label(INSTALLED_LABEL));
    assert!(is_label(&label_for(Path::new("/w/target/debug/aterm"))));
    assert!(is_label("com.aterm.aterm.acaffeine.dev.0123abcd"));
    for bad in [
        "",
        "com.aterm.aterm",
        "com.aterm.aterm.acaffeine.dev",
        "com.aterm.aterm.acaffeine.dev.",
        "com.aterm.aterm.acaffeine.dev.0123ABCD",
        "com.aterm.aterm.acaffeine.dev.0123abc",
        "com.aterm.aterm.acaffeine.dev.0123abcde",
        "com.aterm.aterm.acaffeine.dev.0123abcg",
        "com.aterm.aterm.acaffeine.x",
        "com.aterm.aterm.acaffeine\n",
    ] {
        assert!(!is_label(bad), "{bad:?}");
    }
}

#[test]
#[cfg(unix)]
fn an_orphan_is_a_mugs_login_plist_whose_program_is_gone() {
    let root = crate::test_scratch_dir("launchd");
    let paths = Paths {
        state_dir: root.path().join("state"),
        home: root.path().join("home"),
        logs_dir: root.path().join("logs"),
        local_volume: |_| true,
    };
    assert_eq!(orphans(&paths), [], "no LaunchAgents at all");
    let present = root.path().join("aterm");
    std::fs::write(&present, "exe").expect("exe");
    let gone = root.path().join("removed-worktree").join("aterm");
    let agents = paths.launch_agents();
    std::fs::create_dir_all(&agents).expect("agents");
    let plant = |label: &str, program: &Path| {
        let text = plist_text(label, program, &paths.log_file(), None);
        std::fs::write(agents.join(format!("{label}.plist")), text).expect("plist");
    };
    let orphan = "com.aterm.aterm.acaffeine.dev.0000000a";
    plant(orphan, &gone);
    plant(INSTALLED_LABEL, &gone);
    plant("com.aterm.aterm.acaffeine.dev.0000000b", &present);
    plant(
        "com.aterm.aterm.acaffeine.dev.0000000c",
        Path::new("/Volumes/Ejected/aterm.app/Contents/MacOS/aterm"),
    );
    plant("com.example.other", &gone);
    let uncertain = present.join("aterm");
    assert!(
        std::fs::symlink_metadata(&uncertain).is_err_and(|e| e.kind() != io::ErrorKind::NotFound)
    );
    plant("com.aterm.aterm.acaffeine.dev.0000000e", &uncertain);
    std::fs::write(
        agents.join("com.aterm.aterm.acaffeine.dev.0000000d.plist"),
        plist_text("x", &gone, &paths.log_file(), None)
            .replace("<string>serve</string>", "<string>status</string>"),
    )
    .expect("plist");
    assert_eq!(
        orphans(&paths),
        [
            (
                INSTALLED_LABEL.to_string(),
                paths.startup_plist(INSTALLED_LABEL)
            ),
            (orphan.to_string(), paths.startup_plist(orphan)),
        ]
    );
    assert!(
        on_local_volume(&agents),
        "a temporary folder is on this Mac"
    );
    #[cfg(target_os = "macos")]
    assert!(
        !on_local_volume(&agents.join("missing")),
        "a path statfs cannot read is not judged local"
    );

    assert_eq!(
        unload_stranded(&paths, is_label, &SERVE_ARGS, |label| {
            panic!("{label} is this Mac's to judge, and so its orphan to retire, never stranded")
        }),
        [],
        "a home on this Mac leaves no job unjudged"
    );

    let in_home = "com.aterm.aterm.acaffeine.dev.0000000f";
    plant(in_home, &paths.home.join("worktree/aterm"));
    let network = Paths {
        local_volume: |_| false,
        ..paths.clone()
    };
    assert_eq!(
        orphans(&network),
        [(in_home.to_string(), paths.startup_plist(in_home))],
        "on a network home only a program inside it is judged gone: one outside may be \
         another Mac's"
    );

    let unloaded = std::cell::RefCell::new(Vec::new());
    prune_orphans_with(&network, |label| {
        unloaded.borrow_mut().push(label.to_string());
        Unload::Unloaded
    });
    assert!(!paths.startup_plist(in_home).exists(), "the orphan goes");
    assert!(
        paths.startup_plist(INSTALLED_LABEL).is_file() && paths.startup_plist(orphan).is_file(),
        "a job whose program outside the home is gone here stays for another Mac"
    );
    assert_eq!(
        unloaded.into_inner(),
        [in_home, INSTALLED_LABEL, orphan],
        "the orphan is unloaded, and so is each job kept whose program is gone here, which \
         this Mac cannot run; none whose program is here, on a volume or unreadable"
    );

    let kept = |label: &str, program: &Path, unload: Option<Unload>| Unjudged {
        label: label.to_string(),
        plist: paths.startup_plist(label),
        program: program.to_path_buf(),
        unload,
    };
    assert_eq!(
        unload_stranded(&network, is_label, &SERVE_ARGS, |label| {
            if label == INSTALLED_LABEL {
                Unload::Running(4242)
            } else {
                Unload::Unloaded
            }
        }),
        [
            kept(INSTALLED_LABEL, &gone, Some(Unload::Running(4242))),
            kept(orphan, &gone, Some(Unload::Unloaded)),
            kept("com.aterm.aterm.acaffeine.dev.0000000b", &present, None),
            kept(
                "com.aterm.aterm.acaffeine.dev.0000000c",
                Path::new("/Volumes/Ejected/aterm.app/Contents/MacOS/aterm"),
                None
            ),
            kept("com.aterm.aterm.acaffeine.dev.0000000e", &uncertain, None),
        ],
        "each job kept carries what its unload did, and only one whose program is gone here \
         has one"
    );

    let unloaded = std::cell::RefCell::new(Vec::new());
    prune_orphans_with(&paths, |label| {
        unloaded.borrow_mut().push(label.to_string());
        Unload::Unloaded
    });
    assert!(
        !paths.startup_plist(INSTALLED_LABEL).exists() && !paths.startup_plist(orphan).exists()
    );
    assert_eq!(
        unloaded.into_inner(),
        [INSTALLED_LABEL, orphan],
        "on a home on this Mac each orphan goes and is unloaded once, and nothing else is"
    );
}

/// An idle job is booted out; one with a process is left loaded and booted out never, as a
/// bootout would end that process; one launchd has not loaded, or whose state cannot be read,
/// is not booted out either. A refused bootout of a job launchd no longer has, as when another
/// pass booted it out first, finds it not loaded; one of a job still loaded, or whose state
/// cannot be read again, is a failure that says why.
#[test]
#[cfg(unix)]
fn only_a_job_with_no_process_is_unloaded() {
    let booted = std::cell::Cell::new(0);
    let boot = |answer: io::Result<()>| {
        let booted = &booted;
        move || {
            booted.set(booted.get() + 1);
            answer
        }
    };
    let reads = |states: Vec<io::Result<JobState>>| {
        let mut states = states.into_iter();
        move || {
            states
                .next()
                .expect("launchd's state is read no more often than planned")
        }
    };
    let idle = || Ok(JobState::Loaded { pid: None });
    assert_eq!(
        unload_idle_with(reads(vec![idle()]), boot(Ok(()))),
        Unload::Unloaded
    );
    assert_eq!(booted.replace(0), 1, "an idle job is booted out");
    for (again, unload) in [
        (idle(), Unload::Failed("refused".to_string())),
        (
            Ok(JobState::Loaded { pid: Some(4242) }),
            Unload::Failed("refused".to_string()),
        ),
        (
            Err(io::Error::other("hung")),
            Unload::Failed("refused".to_string()),
        ),
        (Ok(JobState::Absent), Unload::NotLoaded),
    ] {
        assert_eq!(
            unload_idle_with(
                reads(vec![idle(), again]),
                boot(Err(io::Error::other("refused")))
            ),
            unload
        );
        assert_eq!(booted.replace(0), 1, "{unload:?} after one bootout");
    }
    for (state, unload) in [
        (
            Ok(JobState::Loaded { pid: Some(4242) }),
            Unload::Running(4242),
        ),
        (Ok(JobState::Absent), Unload::NotLoaded),
        (
            Err(io::Error::other("hung")),
            Unload::Failed("hung".to_string()),
        ),
    ] {
        assert_eq!(unload_idle_with(reads(vec![state]), boot(Ok(()))), unload);
        assert_eq!(booted.get(), 0, "{unload:?} is never booted out");
    }
}

#[test]
fn only_a_program_inside_a_network_home_is_judged_here() {
    let home = Path::new("/Network/Users//x");
    let inside = Path::new("/Network/Users//x/Applications/aterm.app/Contents/MacOS/aterm");
    let outside = Path::new("/Applications/aterm.app/Contents/MacOS/aterm");
    assert!(judged(inside, home, true) && judged(outside, home, true));
    assert!(judged(inside, home, false));
    assert!(!judged(outside, home, false));
}

#[test]
fn the_plist_names_the_job_and_escapes_every_string() {
    let text = plist_text(
        "com.aterm.aterm.acaffeine.dev.0123abcd",
        Path::new("/Users//x/A&B <\"q'>.app/Contents/MacOS/aterm"),
        Path::new("/Users//x/Library/Logs/aterm/acaffeine.log"),
        Some("com.aterm.aterm"),
    );
    assert!(text.starts_with("<?xml version=\"1.0\" encoding=\"UTF-8\"?>\n<!DOCTYPE plist"));
    assert!(text.ends_with("</dict>\n</plist>\n"));
    assert!(text.contains(
        "\t<key>Label</key>\n\t<string>com.aterm.aterm.acaffeine.dev.0123abcd</string>\n"
    ));
    assert!(text.contains(
        "\t<array>\n\
         \t\t<string>/Users//x/A&amp;B &lt;&quot;q&apos;&gt;.app/Contents/MacOS/aterm</string>\n\
         \t\t<string>acaffeine</string>\n\
         \t\t<string>serve</string>\n\
         \t</array>\n"
    ));
    assert!(!text.contains("A&B"), "an unescaped ampersand");
    assert!(text.contains("\t<key>RunAtLoad</key>\n\t<true/>\n"));
    assert!(text.contains(
        "\t<key>KeepAlive</key>\n\t<dict>\n\t\t<key>SuccessfulExit</key>\n\t\t<false/>\n\t</dict>\n"
    ));
    assert!(
        !text.contains("<key>KeepAlive</key>\n\t<true/>"),
        "never KeepAlive true: Quit must end the job"
    );
    assert!(text.contains("\t<key>ProcessType</key>\n\t<string>Interactive</string>\n"));
    assert!(text.contains("\t<key>LimitLoadToSessionType</key>\n\t<string>Aqua</string>\n"));
    assert!(text.contains(
        "\t<key>StandardErrorPath</key>\n\t<string>/Users//x/Library/Logs/aterm/acaffeine.log</string>\n"
    ));
    assert!(text.contains(
        "\t<key>AssociatedBundleIdentifiers</key>\n\t<array>\n\t\t<string>com.aterm.aterm</string>\n\t</array>\n"
    ));
    let bare = plist_text("l", Path::new("/t/aterm"), Path::new("/l.log"), None);
    assert!(!bare.contains("AssociatedBundleIdentifiers"));
}

/// `plutil -lint` reads the plist as launchd will: well formed, with and without the
/// bundle row and with every character escaping covers.
#[cfg(target_os = "macos")]
#[test]
fn the_plist_is_well_formed_by_plutil() {
    let dir = crate::test_scratch_dir("launchd");
    let lint = |name: &str, text: &str| {
        let path = dir.path().join(name);
        std::fs::write(&path, text).expect("write");
        let out = aterm_uds::deadline::output_until(
            aterm_uds::spawnfd::command("/usr/bin/plutil")
                .arg("-lint")
                .arg(&path),
            std::time::Instant::now() + std::time::Duration::from_secs(60),
        )
        .expect("plutil");
        (
            out.status.success(),
            String::from_utf8_lossy(&out.stdout).into_owned(),
        )
    };
    let full = plist_text(
        "com.aterm.aterm.acaffeine",
        Path::new("/A&B <\"q'>/x.app/Contents/MacOS/aterm"),
        Path::new("/l/acaffeine & log.log"),
        Some("com.aterm.<aterm>"),
    );
    let (ok, said) = lint("full.plist", &full);
    assert!(ok, "{said}\n{full}");
    let bare = plist_text("l", Path::new("/t/aterm"), Path::new("/l.log"), None);
    let (ok, said) = lint("bare.plist", &bare);
    assert!(ok, "{said}\n{bare}");
    let broken = full.replace("A&amp;B", "A&B");
    let (ok, _) = lint("broken.plist", &broken);
    assert!(!ok, "the control lints");
}

#[test]
fn the_launchctl_argv_is_plain() {
    let domain = gui_domain(501);
    assert_eq!(domain, "gui/501");
    assert_eq!(print_argv(&domain, "l.x"), ["print", "gui/501/l.x"]);
    assert_eq!(
        bootstrap_argv(&domain, Path::new("/h/Library/LaunchAgents/l.x.plist")),
        ["bootstrap", "gui/501", "/h/Library/LaunchAgents/l.x.plist"]
    );
    assert_eq!(kickstart_argv(&domain, "l.x"), ["kickstart", "gui/501/l.x"]);
    assert_eq!(bootout_argv(&domain, "l.x"), ["bootout", "gui/501/l.x"]);
    assert_eq!(print_disabled_argv(&domain), ["print-disabled", "gui/501"]);
    assert_eq!(enable_argv(&domain, "l.x"), ["enable", "gui/501/l.x"]);
}

#[test]
fn a_disabled_label_is_its_own_row() {
    let text = "\tdisabled services = {\n\
                \t\t\"com.aterm.aterm.acaffeine.dev.0000000d\" => enabled\n\
                \t\t\"com.aterm.aterm.acaffeine\" => disabled\n\
                \t\t\"com.apple.Siri.agent\" => disabled\n\
                \t}\n";
    assert!(print_disabled_lists(text, "com.aterm.aterm.acaffeine"));
    assert!(!print_disabled_lists(
        text,
        "com.aterm.aterm.acaffeine.dev.0000000d"
    ));
    assert!(
        !print_disabled_lists(text, "com.aterm.aterm"),
        "a prefix of a listed label is not listed"
    );
    assert!(print_disabled_lists("\"l.x\" => true\n", "l.x"));
    assert!(!print_disabled_lists("\"l.x\" => false\n", "l.x"));
    assert!(!print_disabled_lists("", "l.x"));
}

#[test]
fn the_pid_is_the_services_own_line() {
    let running = "gui/501/com.aterm.aterm.acaffeine = {\n\
                   \tactive count = 1\n\
                   \tpath = /Users//x/Library/LaunchAgents/com.aterm.aterm.acaffeine.plist\n\
                   \tstate = running\n\
                   \n\
                   \tenvironment = {\n\
                   \t\tXPC_SERVICE_NAME => com.aterm.aterm.acaffeine\n\
                   \t}\n\
                   \n\
                   \truns = 1\n\
                   \tpid = 1688\n\
                   \timmediate reason = non-ipc demand\n\
                   }\n";
    assert_eq!(print_pid(running), Some(1688));
    let stopped = "gui/501/l = {\n\tstate = not running\n\truns = 1\n\tlast exit code = 0\n}\n";
    assert_eq!(print_pid(stopped), None);
    let nested = "gui/501/l = {\n\tsection = {\n\t\tpid = 9\n\t}\n\tstate = not running\n}\n";
    assert_eq!(
        print_pid(nested),
        None,
        "a nested section's pid is not the job's"
    );
    assert_eq!(print_pid("\tpid = 0\n"), None);
    assert_eq!(print_pid("\tpid = x\n"), None);
    assert_eq!(print_pid("\tpid = 77  \n"), Some(77));
}

#[cfg(unix)]
#[test]
fn startup_is_the_login_plist_on_disk() {
    use std::os::unix::fs::PermissionsExt as _;
    let root = crate::test_scratch_dir("launchd");
    let paths = Paths {
        state_dir: root.path().join("state"),
        home: root.path().join("home"),
        logs_dir: root.path().join("logs"),
        local_volume: |_| true,
    };
    let label = "com.aterm.aterm.acaffeine.dev.0123abcd";
    let program = std::path::PathBuf::from("/t/target/debug/aterm");
    assert!(!startup_enabled(&paths, label));
    {
        set_startup(&paths, label, &program, false).expect("off");
        set_startup(&paths, label, &program, true).expect("on");
        assert!(startup_enabled(&paths, label));
        let plist = paths.startup_plist(label);
        assert_eq!(
            std::fs::read_to_string(&plist).expect("read"),
            plist_text(label, &program, &paths.log_file(), None)
        );
        let mode = std::fs::metadata(&plist)
            .expect("meta")
            .permissions()
            .mode();
        assert_eq!(mode & 0o777, 0o644);
        assert!(paths.logs_dir.is_dir());
        set_startup(&paths, label, &program, true).expect("on");
        let names: Vec<_> = std::fs::read_dir(plist.parent().expect("dir"))
            .expect("list")
            .map(|e| e.expect("entry").file_name())
            .collect();
        assert_eq!(names, [std::ffi::OsString::from(format!("{label}.plist"))]);
        set_startup(&paths, label, &program, false).expect("off");
        assert!(!startup_enabled(&paths, label));
        assert!(!plist.exists());
    }
}

/// Login Items' and launchd's say count only over a plist that is there: with none,
/// startup is off whatever they say. launchd's disable reads first: it is lifted first.
#[cfg(unix)]
#[test]
fn a_login_plist_login_items_blocks_is_blocked() {
    let doors = |blocks: fn(&Path) -> bool, disabled: fn(&str) -> bool| StartupDoors {
        blocks,
        disabled,
        enable: |_| panic!("reading startup lifts nothing"),
    };
    let root = crate::test_scratch_dir("launchd");
    let paths = Paths {
        state_dir: root.path().join("state"),
        home: root.path().join("home"),
        logs_dir: root.path().join("logs"),
        local_volume: |_| true,
    };
    let label = "com.aterm.aterm.acaffeine.dev.0123abcd";
    let program = std::path::PathBuf::from("/t/target/debug/aterm");
    let off = startup(&paths, label, &doors(|_| true, |_| true));
    assert_eq!(off, Startup::Off);
    set_startup(&paths, label, &program, true).expect("on");
    let on = startup(&paths, label, &doors(|_| false, |_| false));
    assert_eq!(on, Startup::On);
    let blocked = startup(&paths, label, &doors(|_| true, |_| false));
    assert_eq!(blocked, Startup::Blocked);
    let disabled = startup(&paths, label, &doors(|_| true, |_| true));
    assert_eq!(disabled, Startup::Disabled);
    let asked = startup(
        &paths,
        label,
        &doors(
            |plist| {
                plist.ends_with("Library/LaunchAgents/com.aterm.aterm.acaffeine.dev.0123abcd.plist")
            },
            |label| label != "com.aterm.aterm.acaffeine.dev.0123abcd",
        ),
    );
    assert_eq!(
        asked,
        Startup::Blocked,
        "asked about the plist and its label"
    );
    let words = [
        Startup::Off,
        Startup::On,
        Startup::Blocked,
        Startup::Disabled,
    ]
    .map(Startup::word);
    assert_eq!(words, ["off", "on", "blocked", "disabled"]);
}

#[cfg(unix)]
#[test]
fn a_startup_plist_names_the_programs_bundle() {
    let root = crate::test_scratch_dir("launchd");
    let app = root.path().join("X.app");
    std::fs::create_dir_all(app.join("Contents").join("MacOS")).expect("bundle");
    std::fs::write(
        app.join("Contents").join("Info.plist"),
        "<plist><dict><key>CFBundleIdentifier</key><string>com.x.y</string></dict></plist>",
    )
    .expect("plist");
    let paths = Paths {
        state_dir: root.path().join("state"),
        home: root.path().join("home"),
        logs_dir: root.path().join("logs"),
        local_volume: |_| true,
    };
    let program = app.join("Contents").join("MacOS").join("aterm");
    set_startup(&paths, "l", &program, true).expect("on");
    let text = std::fs::read_to_string(paths.startup_plist("l")).expect("read");
    assert!(text.contains("<string>com.x.y</string>"), "{text}");
}

#[cfg(unix)]
#[test]
fn a_session_plist_sits_in_a_private_directory() {
    use std::os::unix::fs::PermissionsExt as _;
    let root = crate::test_scratch_dir("launchd");
    let paths = Paths {
        state_dir: root.path().join("state"),
        home: root.path().join("home"),
        logs_dir: root.path().join("logs"),
        local_volume: |_| true,
    };
    let plist = paths.session_plist("l");
    write_session_plist(&paths, &plist, "l", Path::new("/t/aterm"), None).expect("write");
    let mode = |p: &Path| std::fs::metadata(p).expect("meta").permissions().mode() & 0o777;
    assert_eq!(mode(plist.parent().expect("dir")), 0o700);
    assert_eq!(mode(&plist), 0o644);
    assert_eq!(
        std::fs::read_to_string(&plist).expect("read"),
        plist_text("l", Path::new("/t/aterm"), &paths.log_file(), None)
    );

    let aliased = Paths {
        state_dir: root.path().join("alias-state"),
        ..paths.clone()
    };
    std::fs::create_dir_all(&aliased.state_dir).expect("alias state");
    let outside = root.path().join("outside");
    std::fs::create_dir(&outside).expect("outside");
    std::fs::set_permissions(&outside, std::fs::Permissions::from_mode(0o750))
        .expect("outside mode");
    let marker = outside.join("keep");
    std::fs::write(&marker, b"unrelated bytes").expect("marker");
    let alias = aliased.state_dir.join("acaffeine");
    std::os::unix::fs::symlink(&outside, &alias).expect("directory alias");
    assert!(
        write_session_plist(
            &aliased,
            &aliased.session_plist("l"),
            "l",
            Path::new("/t/aterm"),
            None,
        )
        .is_err()
    );
    assert_eq!(mode(&outside), 0o750);
    assert_eq!(std::fs::read(&marker).expect("marker"), b"unrelated bytes");
    assert!(!outside.join("l.plist").exists());
    assert!(
        std::fs::symlink_metadata(&alias)
            .expect("alias retained")
            .file_type()
            .is_symlink()
    );
}

#[test]
#[cfg(unix)]
fn a_sweep_takes_what_cut_short_plist_writes_left_beside_the_mugs_jobs() {
    let root = crate::test_scratch_dir("launchd");
    let paths = Paths {
        state_dir: root.path().join("state"),
        home: root.path().join("home"),
        logs_dir: root.path().join("logs"),
        local_volume: |_| true,
    };
    assert!(sweep_plist_temps(&paths).is_empty(), "no folders at all");
    let ago = std::time::SystemTime::now() - std::time::Duration::from_secs(2 * 60 * 60);
    let plant = |dir: &Path, plist: &str, salt: &str, dated: bool| {
        std::fs::create_dir_all(dir).expect("dir");
        let temp = dir.join(format!(".{plist}.tmp.4294967295.{salt}"));
        let file = std::fs::File::create(&temp).expect("plant");
        if dated {
            file.set_modified(ago).expect("date");
        }
        temp
    };
    let (session, old, agents) = (
        paths.session_dir(),
        crate::legacy::session_dir(&paths),
        paths.launch_agents(),
    );
    let cut_short = [
        plant(
            &session,
            "com.aterm.aterm.acaffeine.dev.0123abcd.plist",
            "0123abcd",
            true,
        ),
        plant(&old, "com.aterm.aterm.caffeine.plist", "0123abcd", true),
        plant(&agents, "com.aterm.aterm.acaffeine.plist", "0123abcd", true),
        plant(&agents, "com.aterm.aterm.acaffeine.plist", "89abcdef", true),
        plant(
            &agents,
            "com.aterm.aterm.caffeine.dev.0123abcd.plist",
            "0123abcd",
            true,
        ),
    ];
    let kept = [
        plant(
            &session,
            "com.aterm.aterm.acaffeine.dev.0123abcd.plist",
            "fedcba98",
            false,
        ),
        plant(&agents, "com.example.other.plist", "0123abcd", true),
        plant(
            &agents,
            "com.aterm.aterm.acaffeine.dev.0123ABCD.plist",
            "0123abcd",
            true,
        ),
        plant(&agents, "com.aterm.aterm.acaffeine", "0123abcd", true),
    ];
    let swept = sweep_plist_temps(&paths);
    let answered: Vec<_> = swept
        .iter()
        .map(|(plist, gone)| (plist.clone(), *gone.as_ref().expect("swept")))
        .collect();
    assert_eq!(
        answered,
        [
            (
                session.join("com.aterm.aterm.acaffeine.dev.0123abcd.plist"),
                1
            ),
            (old.join("com.aterm.aterm.caffeine.plist"), 1),
            (agents.join("com.aterm.aterm.acaffeine.plist"), 2),
            (
                agents.join("com.aterm.aterm.caffeine.dev.0123abcd.plist"),
                1
            ),
        ]
    );
    for temp in &cut_short {
        assert!(!temp.exists(), "{}", temp.display());
    }
    for temp in &kept {
        assert!(temp.exists(), "{}", temp.display());
    }

    use std::os::unix::fs::PermissionsExt as _;
    let stuck = plant(
        &session,
        "com.aterm.aterm.acaffeine.plist",
        "0123abcd",
        true,
    );
    let mode = |bits| {
        std::fs::set_permissions(&session, std::fs::Permissions::from_mode(bits)).expect("chmod");
    };
    mode(0o300);
    let swept = sweep_plist_temps(&paths);
    mode(0o700);
    let answered: Vec<_> = swept
        .iter()
        .map(|(dir, gone)| (dir.clone(), gone.as_ref().map_err(io::Error::kind).copied()))
        .collect();
    assert_eq!(answered, [(session, Err(io::ErrorKind::PermissionDenied))]);
    assert!(stuck.exists(), "an unlisted folder's stay");
}

#[test]
fn print_pid_rejects_signed_numbers() {
    assert_eq!(print_pid("\tpid = 42\n"), Some(42));
    assert_eq!(print_pid("\tpid = +42\n"), None);
    assert_eq!(print_pid("\tpid = -1\n"), None);
}

/// STARTUP-OFF AND LOGIN PLIST CHECKS REFUSE DIRECTORY AND FILE SYMLINKS: `choose_startup`
/// writes `.startup-off` atomically (`0600`) inside a validated `0700` session directory
/// without following symlinks, and `startup_enabled`, `startup`, and `startup_turned_off`
/// inspect `symlink_metadata` rather than following symlinks.
/// NEGATIVE CONTROL: a regular `.startup-off` and regular login plist round-trip cleanly.
#[cfg(unix)]
#[test]
fn choose_startup_and_startup_readers_refuse_symlinks() {
    use std::os::unix::fs::PermissionsExt as _;
    let root = crate::test_scratch_dir("launchd");
    let paths = Paths {
        state_dir: root.path().join("state"),
        home: root.path().join("home"),
        logs_dir: root.path().join("logs"),
        local_volume: |_| true,
    };
    let label = "com.aterm.aterm.acaffeine.dev.0123abcd";
    let program = Path::new("/t/aterm");
    let doors = StartupDoors {
        blocks: |_| false,
        disabled: |_| false,
        enable: |_| Ok(()),
    };

    choose_startup(&paths, label, program, false).expect("record off");
    assert!(startup_turned_off(&paths, label));
    let record = paths.startup_off_file(label);
    let mode = |p: &Path| {
        std::fs::symlink_metadata(p)
            .expect("meta")
            .permissions()
            .mode()
            & 0o777
    };
    assert_eq!(mode(record.parent().expect("session dir")), 0o700);
    assert_eq!(mode(&record), 0o600);

    let keep = root.path().join("keep");
    std::fs::write(&keep, b"preserve").expect("seed keep");
    std::fs::remove_file(&record).expect("remove regular record");
    std::os::unix::fs::symlink(&keep, &record).expect("symlink record");
    assert!(
        !startup_turned_off(&paths, label),
        "symlinked startup-off record must not count as regular"
    );
    choose_startup(&paths, label, program, false).expect("atomic replace over symlink");
    assert_eq!(std::fs::read(&keep).expect("keep intact"), b"preserve");
    assert!(startup_turned_off(&paths, label));

    choose_startup(&paths, label, program, true).expect("record on");
    assert!(!startup_turned_off(&paths, label));
    assert!(startup_enabled(&paths, label));
    assert_eq!(startup(&paths, label, &doors), Startup::On);

    let plist = paths.startup_plist(label);
    let fake_plist = root.path().join("fake.plist");
    std::fs::write(&fake_plist, b"<plist/>").expect("fake plist");
    std::fs::remove_file(&plist).expect("remove regular plist");
    std::os::unix::fs::symlink(&fake_plist, &plist).expect("symlink plist");
    assert!(!startup_enabled(&paths, label));
    assert_eq!(startup(&paths, label, &doors), Startup::Off);

    let aliased = Paths {
        state_dir: root.path().join("alias-state"),
        ..paths.clone()
    };
    std::fs::create_dir_all(&aliased.state_dir).expect("alias state");
    let outside = root.path().join("outside");
    std::fs::create_dir(&outside).expect("outside");
    std::os::unix::fs::symlink(&outside, aliased.session_dir()).expect("symlink session dir");
    let outside_record = outside.join(format!("{label}.startup-off"));
    std::fs::write(&outside_record, b"do-not-delete").expect("seed outside record");
    assert!(
        choose_startup(&aliased, label, program, false).is_err(),
        "choose_startup(off) must refuse symlinked session_dir"
    );
    assert!(
        choose_startup(&aliased, label, program, true).is_err(),
        "choose_startup(on) must refuse symlinked session_dir without unlinking outside record"
    );
    assert!(
        outside_record.exists(),
        "outside record must remain untouched"
    );

    let outside_agents = root.path().join("outside-agents");
    std::fs::create_dir(&outside_agents).expect("outside agents");
    let outside_plist = outside_agents.join(format!("{label}.plist"));
    let gone_program = root.path().join("missing-aterm");
    std::fs::write(
        &outside_plist,
        plist_text(label, &gone_program, &paths.log_file(), None),
    )
    .expect("seed outside login plist");
    let outside_temp = outside_agents.join(format!(".{label}.plist.tmp.4294967295.0123abcd"));
    let temp_file = std::fs::File::create(&outside_temp).expect("seed outside temp");
    temp_file
        .set_modified(std::time::SystemTime::now() - std::time::Duration::from_secs(7200))
        .expect("age outside temp");

    for (case, make_home) in [
        ("symlinked home", {
            let target_home = root.path().join("target-home");
            std::fs::create_dir_all(target_home.join("Library")).expect("target Library");
            std::os::unix::fs::symlink(&outside_agents, target_home.join("Library/LaunchAgents"))
                .expect("link target LaunchAgents");
            let h = root.path().join("symlink-home");
            std::os::unix::fs::symlink(&target_home, &h).expect("symlink home");
            h
        }),
        ("symlinked Library", {
            let target_lib = root.path().join("target-lib");
            std::fs::create_dir(&target_lib).expect("target lib");
            std::os::unix::fs::symlink(&outside_agents, target_lib.join("LaunchAgents"))
                .expect("link LaunchAgents");
            let h = root.path().join("home-with-symlink-lib");
            std::fs::create_dir(&h).expect("home");
            std::os::unix::fs::symlink(&target_lib, h.join("Library")).expect("symlink Library");
            h
        }),
        ("symlinked LaunchAgents", {
            let h = root.path().join("home-with-symlink-agents");
            std::fs::create_dir_all(h.join("Library")).expect("home Library");
            std::os::unix::fs::symlink(&outside_agents, h.join("Library/LaunchAgents"))
                .expect("symlink LaunchAgents");
            h
        }),
    ] {
        let aliased_home = Paths {
            state_dir: root
                .path()
                .join(format!("state-{}", case.replace(' ', "-"))),
            home: make_home,
            logs_dir: root.path().join("logs"),
            local_volume: |_| true,
        };
        assert!(!startup_enabled(&aliased_home, label), "{case}");
        assert_eq!(
            startup(&aliased_home, label, &doors),
            Startup::Off,
            "{case}"
        );
        assert!(orphans(&aliased_home).is_empty(), "{case}");
        assert!(
            set_startup(&aliased_home, label, program, true).is_err(),
            "{case}: set_startup(on) must refuse symlinked LaunchAgents ancestry"
        );
        assert!(
            set_startup(&aliased_home, label, program, false).is_err(),
            "{case}: set_startup(off) must refuse symlinked LaunchAgents ancestry"
        );
        assert!(
            choose_startup(&aliased_home, label, program, true).is_err(),
            "{case}"
        );
        assert!(
            choose_startup(&aliased_home, label, program, false).is_err(),
            "{case}"
        );
        let swept = sweep_plist_temps(&aliased_home);
        assert_eq!(swept.len(), 1, "{case}");
        assert!(swept[0].1.is_err(), "{case}");
        assert!(
            outside_plist.exists() && outside_temp.exists(),
            "{case}: outside plist and temp must remain untouched"
        );
    }

    let outside_logs = root.path().join("outside-logs");
    std::fs::create_dir(&outside_logs).expect("outside logs");
    let symlink_logs = root.path().join("symlink-logs");
    std::os::unix::fs::symlink(&outside_logs, &symlink_logs).expect("symlink logs");
    let aliased_logs = Paths {
        state_dir: root.path().join("state-aliased-logs"),
        home: root.path().join("home-aliased-logs"),
        logs_dir: symlink_logs,
        local_volume: |_| true,
    };
    assert!(
        set_startup(&aliased_logs, label, program, true).is_err(),
        "set_startup(on) must refuse symlinked logs_dir"
    );
    assert!(
        !aliased_logs.startup_plist(label).exists(),
        "no login plist written when logs_dir is a symlink"
    );

    for bad_label in ["", ".", "..", "../escape", "a/b", "a\\b", "x.partial"] {
        assert!(set_startup(&paths, bad_label, program, true).is_err());
        assert!(choose_startup(&paths, bad_label, program, true).is_err());
        assert!(choose_startup(&paths, bad_label, program, false).is_err());
        assert!(!startup_enabled(&paths, bad_label));
        assert!(!startup_turned_off(&paths, bad_label));
        assert_eq!(startup(&paths, bad_label, &doors), Startup::Off);
    }
}
