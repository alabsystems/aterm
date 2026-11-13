// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;

#[test]
fn arches_name_their_triples_and_targets() {
    assert_eq!(Arch::parse("arm64"), Some(Arch::Aarch64));
    assert_eq!(Arch::parse("amd64"), Some(Arch::X86_64));
    assert_eq!(Arch::parse("riscv64"), None);
    assert!(Arch::Aarch64.target_args().is_empty());
    assert_eq!(
        Arch::X86_64.target_args(),
        ["--target", "x86_64-unknown-linux-gnu"]
    );
    assert_eq!(Arch::Aarch64.bin_dir("/t"), "/t/debug");
    assert_eq!(
        Arch::X86_64.bin_dir("/t"),
        "/t/x86_64-unknown-linux-gnu/debug"
    );
}

#[test]
fn curl_development_is_native_and_old_foreign_headers_are_replaced_by_apt() {
    for x86_64 in [false, true] {
        let packages = packages_for(x86_64);
        assert!(packages.iter().any(|name| name == "libcurl4-openssl-dev"));
        assert!(
            !packages
                .iter()
                .any(|name| name == "libcurl4-openssl-dev:amd64")
        );
        let plan = provision_plan_for(None, x86_64);
        let install = plan
            .iter()
            .find(|step| step.label == "apt-get install")
            .unwrap();
        assert!(
            install
                .argv
                .iter()
                .any(|word| word == "libcurl4-openssl-dev")
        );
        assert_eq!(
            install
                .argv
                .iter()
                .any(|word| word == "libcurl4-openssl-dev:amd64-"),
            x86_64
        );
        assert!(
            !install
                .argv
                .iter()
                .any(|word| word == "--autoremove" || word == "--auto-remove")
        );
        assert!(!install.argv.iter().any(|word| word == "libcurl4t64:amd64-"));
    }
}

#[test]
fn every_library_is_installed_for_both_architectures() {
    let all = packages_for(true);
    for lib in DEV_LIBRARIES.iter().chain(MESA) {
        assert!(all.iter().any(|p| p == lib), "{lib}");
        assert!(
            all.iter().any(|p| *p == format!("{lib}:amd64")),
            "{lib}:amd64"
        );
    }
    for tool in TOOLS {
        assert!(
            !all.contains(&format!("{tool}:amd64")),
            "{tool} is the guest's own"
        );
    }
    for lib in BROWSER_AMD64 {
        assert!(all.contains(&format!("{lib}:amd64")), "{lib}:amd64");
    }
    for lib in BROWSER_ARM64 {
        assert!(all.iter().any(|p| p == lib), "{lib}");
    }
    let first_foreign = all.iter().position(|p| p.ends_with(":amd64"));
    assert!(
        all[first_foreign.expect("amd64 rows")..]
            .iter()
            .all(|p| p.ends_with(":amd64"))
    );
}

#[test]
fn each_source_names_one_architecture() {
    for (text, arch) in [(SOURCES_ARM64, "arm64"), (SOURCES_AMD64, "amd64")] {
        let stanzas = text.split("\n\n").count();
        assert_eq!(
            text.matches(&format!("Architectures: {arch}\n")).count(),
            stanzas
        );
    }
}

/// A guest's [`STAMP`] holds this value: a fold that moved it would provision every guest
/// again.
#[test]
fn a_stamped_fingerprint_does_not_move() {
    let steps = [
        step("one", &["apt-get", "update"], None),
        step("two", &["sh", "-c", "cat"], Some("é\n")),
    ];
    assert_eq!(fingerprint(&steps), "97746caca6ffb01a");
    assert_eq!(fingerprint(&[]), "cbf29ce484222325");
}

#[test]
fn the_fingerprint_moves_with_the_plan() {
    let plan = provision_plan(None);
    assert_eq!(fingerprint(&plan), fingerprint(&provision_plan(None)));
    assert_eq!(fingerprint(&plan).len(), 16);
    let mut changed = plan.clone();
    let install = plan.iter().position(|s| s.label == "apt-get install");
    changed[install.expect("an install step")]
        .argv
        .push("extra-package".into());
    assert_ne!(fingerprint(&plan), fingerprint(&changed));
    let mut sources = plan.clone();
    sources[0].stdin = Some(SOURCES_AMD64.into());
    assert_ne!(fingerprint(&plan), fingerprint(&sources));
    let words = provision_plan(Some("a\nb\n".into()));
    assert_ne!(fingerprint(&plan), fingerprint(&words));
    assert_ne!(
        fingerprint(&words),
        fingerprint(&provision_plan(Some("a\nc\n".into())))
    );
}

#[test]
fn a_guest_without_rosetta_is_provisioned_for_aarch64_alone() {
    let native = provision_plan_for(None, false);
    assert!(native.iter().all(|s| !s.label.starts_with("amd64")));
    let install = native.iter().find(|s| s.label == "apt-get install");
    assert!(
        install
            .expect("an install step")
            .argv
            .iter()
            .all(|a| !a.ends_with(":amd64"))
    );
    let rust = native.iter().find(|s| s.label == "rust toolchain");
    assert!(
        !rust
            .expect("a toolchain step")
            .argv
            .iter()
            .any(|a| a == Arch::X86_64.triple())
    );
    assert_eq!(provision_plan(None).len(), native.len() + 3);
    assert_eq!(provision_plan(None), provision_plan_for(None, true));
}

/// The host's web2 goes to the guest at its own path, its words as the step's
/// input; a host without one lays nothing there, and those gates say so.
#[test]
fn the_guest_gets_the_hosts_web2_at_its_own_path() {
    let plan = provision_plan(Some("aa\naal\n".into()));
    let web2 = plan.last().expect("a plan");
    assert_eq!(web2.label, "web2");
    assert_eq!(web2.argv.last().map(String::as_str), Some(WEB2));
    assert_eq!(web2.stdin.as_deref(), Some("aa\naal\n"));
    assert!(
        provision_plan(None)
            .iter()
            .all(|s| s.label != "web2" && !s.argv.iter().any(|a| a == WEB2))
    );
}

/// One recipe builds both bashes, each linked where its tool row looks first: the 5.3 is the
/// `bash-5.3` row's pin, whose Linux refusal names this build, each file of the one table
/// its Mac build reads too passed with its sha256, the archive first.
#[test]
fn the_guest_builds_the_pinned_bash_5_3_with_the_3_2_recipe() {
    let five = aterm_uds::host::tool("bash-5.3");
    assert!(five.in_guest);
    let plan = provision_plan(None);
    for (label, bash, row) in [
        ("bash 3.2", BASH_3_2, aterm_uds::host::tool("bash-3.2")),
        ("bash 5.3", BASH_5_3, five),
    ] {
        assert_eq!(row.at.first(), Some(&bash.path().as_str()), "{label}");
        let step = plan.iter().find(|s| s.label == label).expect(label);
        assert_eq!(step.stdin.as_deref(), Some(BASH_INSTALL), "{label}");
        assert_eq!(step.argv[3..5], [bash.version, bash.link], "{label}");
        let files: Vec<String> = std::iter::once(&bash.source)
            .chain(bash.patches)
            .map(|file| format!("{}={}", file.sha256, file.url))
            .collect();
        assert_eq!(step.argv[5..], files, "{label}");
    }
}

#[test]
fn the_x86_64_environment_is_a_cross_environment() {
    let native = cargo_env(Arch::Aarch64, "/t", true);
    assert!(native.iter().all(|(k, _)| k != "CARGO_BUILD_TARGET"));
    assert!(
        native.iter().all(|(k, _)| k != "RUST_TEST_THREADS"),
        "aarch64 runs its tests in parallel"
    );
    assert!(native.contains(&("RUSTUP_TOOLCHAIN".into(), RUST.into())));
    let cross = cargo_env(Arch::X86_64, "/t", true);
    assert!(cross.contains(&(
        "CARGO_TARGET_X86_64_UNKNOWN_LINUX_GNU_LINKER".into(),
        "x86_64-linux-gnu-gcc".into()
    )));
    assert!(cross.contains(&(
        "CARGO_BUILD_TARGET".into(),
        "x86_64-unknown-linux-gnu".into()
    )));
    assert!(
        cross.contains(&("RUST_TEST_THREADS".into(), "1".into())),
        "Rosetta runs x86_64 tests one at a time"
    );
    let pinned_by_args = cargo_env(Arch::X86_64, "/t", false);
    assert!(
        pinned_by_args
            .iter()
            .all(|(k, _)| k != "CARGO_BUILD_TARGET")
    );
    assert_eq!(
        with_env(&[("A".into(), "1".into())], &["cargo".into()]),
        ["env", "A=1", "cargo"]
    );
}

#[test]
fn displays_start_their_servers_and_point_clients_at_them() {
    let rt = "/run/user/501";
    let config = Config::from_created("/tmp/aterm-lane-config.ABCdef1234\n").unwrap();
    let x = Display::Xvfb.units(rt, &config);
    assert_eq!(x.len(), 1);
    assert_eq!(x[0].socket, "/tmp/.X11-unix/X91");
    assert_eq!(
        Display::Xvfb.client_env(rt),
        ("DISPLAY".into(), ":91".into())
    );
    let w = Display::Weston.units(rt, &config);
    assert_eq!(w[0].socket, "/run/user/501/aterm-lane-weston");
    assert_eq!(
        Display::Weston.client_env(rt).1,
        "/run/user/501/aterm-lane-weston"
    );
    let xw = Display::Xwayland.units(rt, &config);
    assert_eq!(xw.len(), 2, "weston first, then Xwayland inside it");
    assert_eq!(xw[0], w[0]);
    assert_eq!(xw[1].socket, "/tmp/.X11-unix/X92");
    assert!(xw[1].env.contains(&(
        "WAYLAND_DISPLAY".into(),
        "/run/user/501/aterm-lane-weston".into()
    )));
    assert_eq!(Display::Mutter.protocol(), "Wayland");
    let wx = Display::WestonX11.units(rt, &config);
    assert_eq!(wx.len(), 2, "Xvfb first, then weston inside it");
    assert_eq!(wx[0], x[0]);
    assert!(wx[1].argv.contains(&"--backend=x11".to_string()));
    assert!(wx[1].env.contains(&("DISPLAY".into(), ":91".into())));
    assert_eq!(
        Display::WestonX11.client_env(rt),
        (
            "WAYLAND_DISPLAY".into(),
            "/run/user/501/aterm-lane-weston-x11".into()
        )
    );
    assert_eq!(wx[1].socket, Display::WestonX11.client_env(rt).1);
    assert_eq!(Display::WestonX11.seat(), Some(Seat::Xtest));
    assert_eq!(Display::Mutter.seat(), Some(Seat::RemoteDesktop));
    assert_eq!(Display::Weston.seat(), None);
    assert!(!Display::PROOF.contains(&Display::WestonX11));
    for d in Display::ALL {
        assert!(Display::PROOF.contains(&d.proof_display()), "{d:?}");
    }
    assert_eq!(Display::WestonX11.proof_display(), Display::Weston);
    for d in Display::ALL {
        assert_eq!(Display::parse(d.name()), Some(d));
        for u in d.units(rt, &config) {
            assert!(u.name.starts_with(UNIT_PREFIX));
        }
    }
}

/// Every weston the lane starts reads a `weston.ini` its screen writes
/// first, in the proof and in the baseline, so none starts with its
/// shell's fade over the pixels a row holds; a screen with no weston
/// writes none.
#[test]
fn every_weston_reads_the_ini_its_screen_writes() {
    let rt = "/run/user/501";
    let config = Config::from_created("/tmp/aterm-lane-config.ABCdef1234\n").unwrap();
    let ini_path = config.weston_ini();
    let config_arg = format!("--config={ini_path}");
    let screens = Screen::BASELINE.into_iter().chain(Screen::DECK);
    for screen in screens {
        let westons = |units: Vec<Unit>| {
            units
                .into_iter()
                .filter(|u| u.argv.first().is_some_and(|a| a == "weston"))
                .count()
        };
        let reads = |units: Vec<Unit>| {
            units
                .iter()
                .filter(|u| u.argv.contains(&config_arg))
                .count()
        };
        let ini = |files: Vec<(String, String)>| {
            files
                .into_iter()
                .filter(|(path, _)| path == &ini_path)
                .map(|(_, text)| text)
                .collect::<Vec<_>>()
        };
        let n = westons(screen.units(rt, &config));
        assert_eq!(n, usize::from(screen.display.runs_weston()), "{screen:?}");
        assert_eq!(reads(screen.units(rt, &config)), n, "{screen:?}");
        assert_eq!(reads(screen.baseline_units(rt, &config)), n, "{screen:?}");
        let written = vec![WESTON_INI_TEXT.to_string(); n];
        assert_eq!(ini(screen.files(&config)), written, "{screen:?}");
        assert_eq!(ini(screen.baseline_files(&config)), written, "{screen:?}");
    }
    let other = Config::from_created("/tmp/aterm-lane-config.9876ZYXwvu\n").unwrap();
    for screen in Screen::BASELINE.into_iter().chain(Screen::DECK) {
        for (run, foreign) in [(&config, &other), (&other, &config)] {
            for (path, _) in screen
                .files(run)
                .into_iter()
                .chain(screen.baseline_files(run))
            {
                assert!(run.contains(&path), "{screen:?}: {path}");
                assert!(!foreign.contains(&path), "another run owns {path}");
            }
            if let Some(mutter) = screen.baseline_config(run) {
                let keyfile = format!("{mutter}/glib-2.0/settings/keyfile");
                let files = screen.baseline_files(run);
                assert!(files.iter().any(|(path, _)| path == &keyfile));
                for unit in screen
                    .baseline_units(rt, run)
                    .into_iter()
                    .chain(screen.gnome_shell(rt, run))
                {
                    assert!(
                        unit.env
                            .contains(&("XDG_CONFIG_HOME".into(), mutter.clone()))
                    );
                }
            }
            assert!(run.contains(&format!("{}/glib-2.0/settings/keyfile", run.ibus())));
        }
    }
    for unbound in [
        "/tmp/aterm-lane-weston-config/weston.ini".into(),
        format!("{}-sibling/weston.ini", config.root),
        format!("{}/../foreign", config.root),
        format!("{}//weston.ini", config.root),
        config.root.clone(),
    ] {
        assert!(!config.contains(&unbound), "{unbound}");
    }
    for invalid in [
        "",
        "/tmp/aterm-lane-config.123",
        "/tmp/aterm-lane-config.ABCdef1234/child\n",
        "/tmp/aterm-lane-config.ABCdef1234\nwarning\n",
        "/tmp/aterm-lane-config.ABCdef1234\n\n",
    ] {
        assert!(Config::from_created(invalid).is_none(), "{invalid:?}");
    }
}

/// The baseline's `us intl` goes on the X server it names as soon as that
/// server listens: under weston's x11 backend before weston, which reads
/// the keymap once as it starts.
#[test]
fn the_baseline_keymap_follows_the_x_server_it_sets() {
    let rt = "/run/user/501";
    let config = Config::from_created("/tmp/aterm-lane-config.ABCdef1234\n").unwrap();
    for (screen, server) in [
        (Screen::plain(Display::Xvfb), 0),
        (Screen::plain(Display::Xwayland), 1),
        (Screen::plain(Display::WestonX11), 0),
    ] {
        let (socket, step) = screen.baseline_keymap().expect("an X display");
        let units = screen.baseline_units(rt, &config);
        let at = units.iter().position(|u| u.socket == socket);
        assert_eq!(at, Some(server), "{}", screen.name());
        assert!(step.argv.contains(&"intl".to_string()));
    }
    assert!(Screen::plain(Display::Mutter).baseline_keymap().is_none());
    assert!(Screen::plain(Display::Weston).baseline_keymap().is_none());
}

#[test]
fn a_unit_is_a_collected_user_unit_with_its_environment() {
    let unit = Unit {
        name: "aterm-lane-x".into(),
        env: vec![("K".into(), "v w".into())],
        argv: vec!["prog".into(), "--flag".into()],
        socket: "/s".into(),
    };
    assert_eq!(
        run_unit(&unit, Some("/home")),
        [
            "systemd-run",
            "--user",
            "--quiet",
            "--collect",
            "--unit=aterm-lane-x",
            "--working-directory=/home",
            "--setenv=K=v w",
            "--",
            "prog",
            "--flag"
        ]
    );
    assert_eq!(wait_socket("/s", 60)[..2], ["timeout", "60"]);
    assert_eq!(wait_socket("/s", 60).last().map(String::as_str), Some("/s"));
    let lines = wait_lines("/log", "it's \"joined\"", 2, 60);
    assert_eq!(lines[..2], ["timeout", "60"]);
    assert_eq!(lines[lines.len() - 3..], ["/log", "it's \"joined\"", "2"]);
    assert!(!lines[4].contains("joined"), "the text is an argument");
    let name = wait_name("org.freedesktop.portal.Desktop", 60);
    assert_eq!(name[..2], ["timeout", "60"]);
    assert_eq!(
        name.last().map(String::as_str),
        Some("org.freedesktop.portal.Desktop")
    );
}

/// The clipboard's second client: its owner is a lane unit, so the
/// teardown after each display stops it however the row ended, and the
/// text reaches xclip as an argument, never as shell source.
#[test]
fn the_clipboard_owner_is_a_lane_unit_on_the_display() {
    let owner = clipboard_owner(":91", Selection::Clipboard, "A'B");
    assert!(owner.name.starts_with(UNIT_PREFIX));
    assert_eq!(owner.env, [("DISPLAY".to_string(), ":91".to_string())]);
    assert_eq!(owner.argv[4], "A'B");
    assert_eq!(owner.argv.last().map(String::as_str), Some("clipboard"));
    assert!(!owner.argv[2].contains("A'B"));
    for argv in [
        read_clipboard(":91", Selection::Primary, 60),
        wait_clipboard(":91", Selection::Primary, "A'B", 60),
    ] {
        assert_eq!(argv[..4], ["env", "DISPLAY=:91", "timeout", "60"]);
    }
    assert_eq!(
        wait_clipboard(":91", Selection::Clipboard, "A'B", 60)[8],
        "A'B"
    );
}

#[test]
fn only_an_enabled_registration_runs_x86_64() {
    let measured = "enabled\ninterpreter /mnt/lima-rosetta/rosetta\nflags: OCF\noffset 0\n";
    assert!(rosetta_registered(measured));
    assert!(!rosetta_registered(
        &measured.replace("enabled", "disabled")
    ));
    assert!(!rosetta_registered(
        "cat: /proc/sys/fs/binfmt_misc/rosetta: No such file"
    ));
}

#[test]
fn the_rosetta_cache_is_emptied_with_its_daemon_stopped_and_stamped() {
    let steps = rosetta_cache_reset("abc-123\n");
    let labels: Vec<&str> = steps.iter().map(|s| s.label).collect();
    assert_eq!(
        labels,
        [
            "stop rosettad",
            "empty the AOT cache",
            "stamp the boot",
            "start rosettad"
        ]
    );
    assert!(steps[1].argv.contains(&ROSETTA_CACHE.to_string()));
    assert_eq!(steps[2].stdin.as_deref(), Some("abc-123\n"));
    assert_eq!(
        steps[2].argv.last().map(String::as_str),
        Some(ROSETTA_CACHE_BOOT)
    );
}

#[test]
fn only_a_unix_address_is_an_a11y_bus() {
    assert_eq!(
        parse_a11y_address("s \"unix:path=/run/user/501/at-spi/bus_0,guid=ab\"\n"),
        Some("unix:path=/run/user/501/at-spi/bus_0,guid=ab")
    );
    assert_eq!(parse_a11y_address("s \"\""), None);
    assert_eq!(parse_a11y_address("Call failed: no such name"), None);
}
