// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;

#[cfg(unix)]
#[test]
fn descriptor_room_preserves_limits_and_reports_the_actual_result() {
    use std::cell::Cell;
    for (soft, hard, want, ceiling, target) in [
        (256, u64::MAX, 4096, 92_160, 4096),
        (256, 1024, 4096, u64::MAX, 1024),
        (256, u64::MAX, 4096, 512, 512),
        (8192, u64::MAX, 4096, 4096, 8192),
        (256, 256, 4096, u64::MAX, 256),
        (256, u64::MAX, 0, u64::MAX, 256),
    ] {
        for refused in [false, true] {
            let current = Cell::new(soft);
            let wrote = Cell::new(false);
            let room = open_file_room_with(
                want,
                ceiling,
                || Ok((current.get(), hard)),
                |next, unchanged_hard| {
                    assert_eq!(unchanged_hard, hard, "the hard limit never changes");
                    assert!(next > soft && next <= hard && next <= ceiling);
                    wrote.set(true);
                    if refused {
                        return Err(std::io::Error::from_raw_os_error(libc::EPERM));
                    }
                    current.set(next);
                    Ok(())
                },
            )
            .unwrap();
            assert_eq!((room.before, room.hard, room.target), (soft, hard, target));
            assert_eq!(wrote.get(), target > soft);
            assert_eq!(room.after, if refused { soft } else { target });
            assert_eq!(room.refusal.is_some(), refused && target > soft);
            if let Some(error) = room.refusal {
                assert_eq!(error.raw_os_error(), Some(libc::EPERM));
            }
        }
    }

    let written = Cell::new(false);
    let room = open_file_room_with(
        4096,
        4096,
        || Ok((if written.get() { 2048 } else { 256 }, u64::MAX)),
        |_, _| {
            written.set(true);
            Ok(())
        },
    )
    .unwrap();
    assert_eq!(room.target, 4096);
    assert_eq!(
        room.after, 2048,
        "report the observed limit, not the request"
    );

    assert!(
        open_file_room_with(
            4096,
            4096,
            || Err(std::io::Error::from_raw_os_error(libc::EIO)),
            |_, _| panic!("an unreadable initial limit authorizes no write"),
        )
        .is_err()
    );
    let written = Cell::new(false);
    assert!(
        open_file_room_with(
            4096,
            4096,
            || {
                if written.get() {
                    Err(std::io::Error::from_raw_os_error(libc::EIO))
                } else {
                    Ok((256, u64::MAX))
                }
            },
            |_, _| {
                written.set(true);
                Ok(())
            },
        )
        .is_err(),
        "a failed readback is never reported as an applied limit"
    );
}

fn stand_in(path: &Path) {
    std::fs::create_dir_all(path.parent().expect("a parent")).expect("mkdir");
    std::fs::write(path, "#!/bin/sh\n").expect("write a stand-in");
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt as _;
        std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o755)).expect("chmod");
    }
}

fn fish_like(fixed: &'static str) -> Tool {
    Tool {
        at: Box::leak(vec![fixed].into_boxed_slice()),
        ..*tool("fish")
    }
}

fn leak(path: &Path) -> &'static str {
    Box::leak(path.to_string_lossy().into_owned().into_boxed_str())
}

#[test]
fn the_pinned_copy_in_the_tooling_cache_comes_before_the_usual_paths() {
    let scratch = aterm_tempfile::tempdir().expect("scratch");
    let fixed = scratch.path().join("usual/bin/fish");
    stand_in(&fixed);
    let row = fish_like(leak(&fixed));
    let tooling = scratch.path().join("tooling.noindex");
    let pin = row.pin.expect("fish is pinned");
    let pinned = tooling.join(format!("fish-{}/bin/fish", pin.version));
    stand_in(&pinned);
    let lookup = Lookup {
        named: None,
        tooling: Some(tooling.clone()),
        path: None,
    };
    assert_eq!(
        row.find_in(&lookup),
        Some(fixed.clone()),
        "an unstamped copy at the pin is not the pin"
    );
    let stamp = tooling
        .join(format!("fish-{}", pin.version))
        .join(PIN_STAMP);
    std::fs::create_dir(&stamp).expect("a folder named like the stamp");
    assert_eq!(row.find_in(&lookup), Some(fixed), "a stamp is a file");
    std::fs::remove_dir(&stamp).expect("rmdir");
    stand_in(&stamp);
    assert_eq!(row.find_in(&lookup), Some(pinned.clone()), "the pin first");

    let named = scratch.path().join("named/fish");
    stand_in(&named);
    let lookup = Lookup {
        named: Some(named.clone().into_os_string()),
        ..lookup
    };
    assert_eq!(
        row.find_in(&lookup),
        Some(named),
        "the variable before the pin"
    );

    let lookup = Lookup {
        named: None,
        tooling: Some(tooling),
        path: None,
    };
    assert_eq!(tool("fish").find_in(&lookup), Some(pinned));
}

#[test]
fn a_copy_at_another_version_is_not_the_pin() {
    let scratch = aterm_tempfile::tempdir().expect("scratch");
    let fixed = scratch.path().join("usual/bin/fish");
    stand_in(&fixed);
    let row = fish_like(leak(&fixed));
    let tooling = scratch.path().join("tooling.noindex");
    stand_in(&tooling.join("fish-0.0.1/bin/fish"));
    stand_in(&tooling.join("fish-0.0.1").join(PIN_STAMP));
    let lookup = Lookup {
        named: None,
        tooling: Some(tooling.clone()),
        path: None,
    };
    assert_eq!(
        row.find_in(&lookup),
        Some(fixed),
        "the usual path, not the stale copy"
    );

    let lonely = Tool { at: &[], ..row };
    let path = scratch.path().join("on-path");
    stand_in(&path.join("fish"));
    let lookup = Lookup {
        named: None,
        tooling: Some(tooling),
        path: Some(path.clone().into_os_string()),
    };
    assert_eq!(
        lonely.find_in(&lookup),
        Some(path.join("fish")),
        "then $PATH"
    );
    assert_eq!(
        lonely.find_in(&Lookup {
            path: Some("relative/dir".into()),
            ..lookup
        }),
        None,
        "a relative $PATH entry is no hit"
    );
}

#[test]
fn the_refusal_names_its_provider() {
    let mac = "macos-aarch64";
    let fish = tool("fish").refusal_on(mac);
    assert_eq!(
        fish,
        "no fish here: `targo --unverified verify provision` builds the pinned 4.9.3 into this \
         user's cache (or ATERM_TEST_FISH=<path> names one)"
    );
    assert_eq!(
        tool("bash-5.3").refusal_on(mac),
        "no bash-5.3 here: `targo --unverified verify provision` builds the pinned 5.3.20 into \
         this user's cache (or ATERM_TEST_BASH_5_3=<path> names one)"
    );
    let pwsh = tool("pwsh").refusal_on(mac);
    assert_eq!(
        pwsh,
        "no pwsh here: `targo --unverified verify provision` fetches the pinned 7.6.7 into this \
         user's cache (or ATERM_TEST_PWSH=<path> names one)"
    );
    let lima = tool("limactl").refusal_on("macos-x86_64");
    assert!(
        lima.starts_with("no limactl here: `targo --unverified verify provision --linux` fetches"),
        "{lima}"
    );
    let bindgen = tool("wasm-bindgen").refusal_on("linux-x86_64");
    assert!(
        bindgen
            .contains("`targo --unverified verify provision --record` builds the pinned 0.2.108"),
        "{bindgen}"
    );
    let vim = tool("vim").refusal();
    assert!(
        vim.contains("operating system, and no command adds it"),
        "{vim}"
    );
    let ty = tool("ty").refusal();
    assert!(ty.contains("aterm pkg install --default-set"), "{ty}");
    let python = tool("python3").refusal();
    assert!(
        python.starts_with("no Python 3.11+ here: the Linux guest provides it")
            && python.contains("ATERM_TEST_PYTHON3=<path>"),
        "{python}"
    );
    let zstd = tool("zstd").refusal();
    assert!(
        zstd.contains("aterm provides none: it is the outside reference the tests judge"),
        "{zstd}"
    );
    let cc = tool("cc").refusal();
    assert!(
        cc.contains("`aterm init --only base.clt` installs them")
            && !cc.contains("xcode-select --install"),
        "the CLT row names base.clt, never the dialog: {cc}"
    );
    let gpu = tool("gpu").refusal();
    assert_eq!(gpu, "this host lacks a GPU adapter, and no command adds it");
}

#[test]
fn no_remedy_says_brew() {
    // DESIGN-init-system §6.5: no remedy says `brew`, and abrew is not aterm's supply line: no
    // remedy runs abrew, and only xcodebuild's names it, as the reason no Xcode is needed.
    for row in TOOLS {
        for platform in [
            "macos-aarch64",
            "macos-x86_64",
            "linux-x86_64",
            "linux-aarch64",
            "windows-x86_64",
        ] {
            let refusal = row.refusal_on(platform);
            let words: Vec<String> = refusal
                .split(|c: char| !c.is_ascii_alphanumeric())
                .filter(|word| !word.is_empty())
                .map(str::to_ascii_lowercase)
                .collect();
            let brew = words
                .iter()
                .any(|word| word == "brew" || word == "homebrew");
            let runs_abrew = refusal.contains("`abrew")
                || words.windows(2).any(|pair| {
                    pair[0] == "abrew"
                        && ["install", "bundle", "tap", "shellenv"].contains(&pair[1].as_str())
                });
            let names_abrew = words.iter().any(|word| word == "abrew");
            assert!(
                !brew && !runs_abrew && (!names_abrew || row.name == "xcodebuild"),
                "{platform}: {refusal}"
            );
        }
    }
}

#[test]
fn every_workspace_row_names_the_run_that_provides_it() {
    let mut seen = 0;
    for row in TOOLS {
        let Provider::Workspace { when } = row.provider else {
            continue;
        };
        seen += 1;
        let refusal = row.refusal_on("macos-aarch64");
        assert!(
            refusal.contains(&format!("`{}`", when.command())),
            "{refusal}"
        );
        for (flag, wanted) in [("--linux", When::Linux), ("--record", When::Record)] {
            assert_eq!(refusal.contains(flag), when == wanted, "{refusal}");
        }
        assert!(!refusal.contains("init --develop"), "{refusal}");
        assert_eq!(row.provided_by(), when.verb());
    }
    assert!(seen >= 8, "the pinned rows: {seen}");
    assert_eq!(When::Linux.verb(), "verify provision --linux");
    assert_eq!(
        When::Always.command(),
        "targo --unverified verify provision"
    );
}

#[test]
fn a_host_the_tree_pins_nothing_for_is_told_so() {
    for name in ["fish", "bash-5.3", "pwsh", "wasm-opt", "limactl"] {
        let row = tool(name);
        let linux = row.refusal_on("linux-x86_64");
        assert!(
            linux.contains("aterm provides none for linux-x86_64:")
                && linux.contains("pins it for macos-aarch64 and macos-x86_64 only")
                && !linux.contains(" fetches the pinned")
                && !linux.contains(" builds the pinned"),
            "{linux}"
        );
        for mac in MACOS_HOSTS {
            let words = row.refusal_on(mac);
            assert!(words.contains("the pinned"), "{mac}: {words}");
        }
    }
    let bindgen = tool("wasm-bindgen").refusal_on("linux-aarch64");
    assert!(bindgen.contains("builds the pinned 0.2.108"), "{bindgen}");
}

#[test]
fn the_linux_lanes_image_is_pinned_for_apple_silicon_only() {
    for (name, what) in [
        ("ubuntu-root", "Ubuntu 24.04's pinned root image"),
        ("ubuntu-kernel", "Ubuntu 24.04's pinned kernel"),
    ] {
        let row = tool(name);
        assert_eq!(
            row.refusal_on("macos-aarch64"),
            format!(
                "no {what} here: `targo --unverified verify provision --linux` fetches the pinned \
                 20260926 into this user's cache (or {}=<path> names one)",
                row.var()
            )
        );
        let intel = row.refusal_on("macos-x86_64");
        assert!(
            intel.contains("aterm provides none for macos-x86_64")
                && intel.contains("pins it for macos-aarch64 only"),
            "{intel}"
        );
    }
    assert_eq!(
        pinned_version("ubuntu-root"),
        pinned_version("ubuntu-kernel"),
        "the root and its kernel are one release"
    );
}

#[test]
fn a_pin_lives_under_its_package() {
    let t = Path::new("/t");
    assert_eq!(
        tool("wasm-opt").pinned_in(t),
        Some(PathBuf::from("/t/binaryen-133/bin/wasm-opt"))
    );
    assert_eq!(
        tool("limactl").pinned_in(t),
        Some(PathBuf::from("/t/lima-2.2.1/bin/limactl"))
    );
    assert_eq!(
        tool("wasm-bindgen").pinned_root(t),
        Some(PathBuf::from(format!(
            "/t/wasm-bindgen-{}",
            pinned_version("wasm-bindgen")
        )))
    );
    assert_eq!(
        tool("rustup").pinned_in(t),
        Some(PathBuf::from("/t/rustup-1.29.2/bin/rustup"))
    );
}

/// The `rustup` row's copy `verify provision` places: `bin/rustup` and its stamp.
fn placed_rustup(tooling: &Path) -> PathBuf {
    let exe = tool("rustup").pinned_in(tooling).expect("rustup is pinned");
    stand_in(&exe);
    stand_in(
        &tool("rustup")
            .pinned_root(tooling)
            .expect("rustup is pinned")
            .join(PIN_STAMP),
    );
    exe
}

/// RUSTUP IS THE ROW'S STAMPED COPY BEFORE ANY ON `$PATH`, AND NEVER HOMEBREW'S KEG: the one
/// rustup finder is the `rustup` row's resolver, whose places are `$ATERM_TEST_RUSTUP`, the
/// copy `verify provision` placed with its stamp, and `$PATH`. NEGATIVE CONTROLS: a copy
/// without its stamp is passed over for `$PATH`, and the scratch keg is a file the resolver
/// would take, since it answers once a person puts the keg's own directory on `$PATH`.
#[test]
fn rustup_is_the_stamped_copy_before_path_and_never_the_keg() {
    let scratch = aterm_tempfile::tempdir().expect("scratch");
    let row = tool("rustup");
    assert!(
        row.at.is_empty(),
        "the row lists no fixed place: {:?}",
        row.at
    );
    let keg = scratch.path().join("opt/homebrew/opt/rustup/bin/rustup");
    stand_in(&keg);
    let tooling = scratch.path().join("tooling.noindex");
    let on_path = scratch.path().join("on-path");
    stand_in(&on_path.join("rustup"));
    let bare = Lookup {
        named: None,
        tooling: Some(tooling.clone()),
        path: None,
    };
    assert_eq!(
        row.find_in(&bare),
        None,
        "nothing answers but the row's places"
    );
    let with_path = Lookup {
        path: Some(on_path.clone().into_os_string()),
        ..bare.clone()
    };
    assert_eq!(row.find_in(&with_path), Some(on_path.join("rustup")));

    let exe = row.pinned_in(&tooling).expect("rustup is pinned");
    stand_in(&exe);
    assert_eq!(
        row.find_in(&with_path),
        Some(on_path.join("rustup")),
        "a copy without its stamp is not the pin"
    );
    assert_eq!(placed_rustup(&tooling), exe);
    assert_eq!(row.find_in(&with_path), Some(exe.clone()), "the copy first");
    assert_eq!(row.find_in(&bare), Some(exe));

    let keg_dir = keg.parent().expect("the keg's bin").as_os_str().to_owned();
    assert_eq!(
        row.find_in(&Lookup {
            tooling: None,
            path: Some(keg_dir),
            named: None,
        }),
        Some(keg),
        "the planted keg is a file a lookup would take"
    );
}

/// A CHECK THAT FINDS NO RUSTUP SAYS WHERE THE ROW LOOKED AND NAMES ITS PROVIDER ONCE, and a
/// variable that names no file is said, not a panic. NEGATIVE CONTROL: `find_in`, which a
/// test gate calls, still panics on that variable.
#[test]
fn a_rustup_the_row_cannot_find_is_absent_naming_verify_provision_once() {
    let scratch = aterm_tempfile::tempdir().expect("scratch");
    let row = tool("rustup");
    let tooling = scratch.path().join("tooling.noindex");
    let none = Lookup {
        named: None,
        tooling: Some(tooling.clone()),
        path: Some(scratch.path().join("empty").into_os_string()),
    };
    let absent = row.locate_in(&none).expect_err("no rustup anywhere");
    assert_eq!(
        absent.why,
        format!(
            "no rustup (looked in $ATERM_TEST_RUSTUP, the stamped 1.29.2 at {} and $PATH)",
            tooling.join("rustup-1.29.2/bin/rustup").display()
        )
    );
    assert_eq!(
        absent.fix,
        "`targo --unverified verify provision` fetches the pinned 1.29.2 into this user's cache"
    );
    let said = absent.to_string();
    assert_eq!(said.matches("verify provision").count(), 1, "{said}");
    assert!(!said.contains("brew"), "{said}");

    let missing = scratch.path().join("no-such-rustup");
    let named = Lookup {
        named: Some(missing.clone().into_os_string()),
        ..none
    };
    let absent = row
        .locate_in(&named)
        .expect_err("a variable naming nothing");
    assert_eq!(
        absent.why,
        format!("ATERM_TEST_RUSTUP points at {missing:?}, which is not a file")
    );
    assert!(
        absent
            .fix
            .starts_with("unset ATERM_TEST_RUSTUP or point it at a rustup"),
        "the variable is what to change, since it outranks the copy: {}",
        absent.fix
    );
    assert_eq!(
        absent.fix.matches("verify provision").count(),
        1,
        "{}",
        absent.fix
    );
    assert!(
        std::panic::catch_unwind(|| row.find_in(&named)).is_err(),
        "a test gate's lookup still panics on it"
    );
    placed_rustup(&tooling);
    assert!(row.locate_in(&named).is_err(), "the variable comes first");
    assert_eq!(
        row.locate_in(&Lookup {
            named: None,
            ..named
        }),
        Ok(tooling.join("rustup-1.29.2/bin/rustup"))
    );
}

#[test]
fn the_usual_paths_come_before_path() {
    let scratch = aterm_tempfile::tempdir().expect("scratch");
    let usual = scratch.path().join("usual/bin/fish");
    stand_in(&usual);
    let on_path = scratch.path().join("on-path");
    stand_in(&on_path.join("fish"));
    let row = fish_like(leak(&usual));
    let lookup = Lookup {
        named: None,
        tooling: None,
        path: Some(on_path.clone().into_os_string()),
    };
    assert_eq!(
        row.find_in(&lookup),
        Some(usual.clone()),
        "the usual path first"
    );
    std::fs::remove_file(&usual).expect("remove the usual copy");
    assert_eq!(
        row.find_in(&lookup),
        Some(on_path.join("fish")),
        "then $PATH"
    );
}

/// A FILE WITH NO EXECUTE BIT IS PASSED OVER, on `$PATH` and at a usual path alike, for the
/// next place that holds a program, as a shell's own `$PATH` search passes it over: a
/// mode-0644 `rustup` early on `$PATH` is not the rustup the lanes run. NEGATIVE CONTROL: the
/// same file with its execute bit back is the answer again.
#[cfg(unix)]
#[test]
fn a_file_without_an_execute_bit_is_passed_over_for_the_next_place() {
    use std::os::unix::fs::PermissionsExt as _;
    let scratch = aterm_tempfile::tempdir().expect("scratch");
    let (first, second) = (scratch.path().join("a"), scratch.path().join("b"));
    stand_in(&first.join("rustup"));
    stand_in(&second.join("rustup"));
    let mode = |path: &Path, bits: u32| {
        std::fs::set_permissions(path, std::fs::Permissions::from_mode(bits)).expect("chmod");
    };
    mode(&first.join("rustup"), 0o644);
    let path = std::env::join_paths([&first, &second]).expect("a PATH");
    let lookup = Lookup {
        named: None,
        tooling: None,
        path: Some(path),
    };
    let row = tool("rustup");
    assert_eq!(row.find_in(&lookup), Some(second.join("rustup")));
    mode(&first.join("rustup"), 0o755);
    assert_eq!(
        row.find_in(&lookup),
        Some(first.join("rustup")),
        "control: with its execute bit the first is taken"
    );

    let usual = scratch.path().join("usual/bin/fish");
    stand_in(&usual);
    stand_in(&second.join("fish"));
    mode(&usual, 0o644);
    let fish = fish_like(leak(&usual));
    let lookup = Lookup {
        path: Some(second.clone().into_os_string()),
        ..lookup
    };
    assert_eq!(fish.find_in(&lookup), Some(second.join("fish")));
    mode(&usual, 0o755);
    assert_eq!(
        fish.find_in(&lookup),
        Some(usual),
        "control: the usual path"
    );
}

#[test]
fn a_missing_tool_ends_could_not_run_naming_its_provider() {
    let nowhere = Tool {
        at: &[],
        ..*tool("vim")
    };
    assert_eq!(nowhere.find_in(&Lookup::default()), None);
    assert!(
        nowhere
            .refusal()
            .starts_with("no vim here: it comes with the operating system"),
        "{}",
        nowhere.refusal()
    );
    assert_eq!(
        refusals_for(&["fish", "zsh"]),
        Some(format!(
            "that lane did not run: {}; {}",
            tool("fish").refusal(),
            tool("zsh").refusal()
        )),
        "every absent tool, in order"
    );
    assert_eq!(refusals_for(&[]), None);
    could_not_run_without(&[]);
    let refused = std::panic::catch_unwind(|| nowhere.need_in(&Lookup::default()));
    let carried = refused.as_ref().err().and_then(|payload| {
        payload
            .downcast_ref::<String>()
            .map(|words| words.contains(COULD_NOT_RUN))
    });
    assert_eq!(
        carried,
        Some(true),
        "need_in ends a missing tool COULD NOT RUN"
    );
}

#[test]
fn a_tools_variable_is_its_upper_snake_name() {
    assert_eq!(tool("fish").var(), "ATERM_TEST_FISH");
    assert_eq!(tool("wasm-opt").var(), "ATERM_TEST_WASM_OPT");
    assert_eq!(tool("fish").needs_label(), "fish (verify provision)");
    assert_eq!(
        tool("limactl").needs_label(),
        "limactl (verify provision --linux)"
    );
    assert_eq!(tool("firefox").needs_label(), "firefox (verify provision)");
    assert_eq!(
        tool("wasm-bindgen").needs_label(),
        "wasm-bindgen (verify provision --record)"
    );
    assert_eq!(tool("zsh").needs_label(), "zsh (OS)");
}

#[test]
fn the_user_cache_follows_each_platform_convention() {
    let home = Path::new("/h");
    let xdg = Path::new(env!("CARGO_MANIFEST_DIR"));
    assert_eq!(
        user_cache("macos", Some(home), Some(xdg), "browser"),
        Some(PathBuf::from("/h/Library/Caches/aterm/browser.noindex"))
    );
    assert_eq!(
        user_cache("linux", Some(home), Some(xdg), TOOLING),
        Some(xdg.join("aterm/tooling"))
    );
    assert_eq!(
        user_cache("linux", Some(home), Some(Path::new("rel")), "browser"),
        Some(PathBuf::from("/h/.cache/aterm/browser"))
    );
    assert_eq!(user_cache("linux", None, None, "browser"), None);
}

/// A SITE SPAWNS THE OS'S OWN PROGRAM BY THE ONE PATH ITS ROW LISTS (§9 A41): the first of
/// the row's places, evaluated at compile time, so a site names a program and never a path
/// of its own. NEGATIVE CONTROLS: a row the OS does not provide (fish, pinned by the tree),
/// a root tool whose row lists no path on purpose (pmset, which only aterm-machines may
/// spell), and a name no row carries are refused, where a compile-time use would not build.
#[test]
fn an_os_program_is_spawned_by_its_rows_one_path() {
    const CURL: &str = crate::os_path!("curl");
    const GIT: &str = crate::os_path!("git");
    if cfg!(unix) {
        assert_eq!(CURL, "/usr/bin/curl");
        assert_eq!(GIT, "/usr/bin/git");
        for name in [
            "lsof",
            "osascript",
            "ps",
            "perl",
            "nc",
            "sysctl",
            "launchctl",
        ] {
            let path = os_path(name);
            assert_eq!(path, tool(name).at[0], "{name}");
            assert!(Path::new(path).is_absolute(), "{name}: {path}");
        }
    } else {
        assert_eq!((CURL, GIT), ("curl", "git"));
    }
    for refused in ["fish", "pmset", "no-such-program"] {
        let caught = std::panic::catch_unwind(|| os_path(refused));
        assert!(caught.is_err(), "{refused} has no OS path");
    }
}
