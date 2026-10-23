// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;

/// The preflight works on injected process limits: no test changes the
/// runner's limits, and a successful write is never its own readback proof.
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

/// A file at `path` (its directory made): what the resolver looks for is a file.
fn stand_in(path: &Path) {
    std::fs::create_dir_all(path.parent().expect("a parent")).expect("mkdir");
    std::fs::write(path, "#!/bin/sh\n").expect("write a stand-in");
}

/// A row like the fish row, whose usual install path is a stand-in this test owns, so the
/// order is measured on any host (this Mac's /opt/homebrew/bin/fish included).
fn fish_like(fixed: &'static str) -> Tool {
    Tool {
        at: Box::leak(vec![fixed].into_boxed_slice()),
        ..*tool("fish")
    }
}

fn leak(path: &Path) -> &'static str {
    Box::leak(path.to_string_lossy().into_owned().into_boxed_str())
}

/// §10 S2: a stand-in at `fish-<pinned>/bin/fish` in a scratch tooling dir, stamped as
/// `verify provision` stamps what it places, is found ahead of the usual install paths,
/// and `$ATERM_TEST_FISH` still comes first. NEGATIVE CONTROL: the same copy without its
/// stamp (laid by hand) is passed over for the usual path.
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

    // The real row, over the same tooling dir: what every live fish test resolves.
    let lookup = Lookup {
        named: None,
        tooling: Some(tooling),
        path: None,
    };
    assert_eq!(tool("fish").find_in(&lookup), Some(pinned));
}

/// A copy at another version is never the pin: the resolver reads exactly the pinned one,
/// so a stale build is never what a test runs.
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

/// The refusal names the provider's one command, never a hand install, and never a command
/// that does not exist (#89): a pinned tool's is the `verify provision` run that provides
/// it, with the variable that names another copy after it.
#[test]
fn the_refusal_names_its_provider() {
    let mac = "macos-aarch64";
    let fish = tool("fish").refusal_on(mac);
    assert_eq!(
        fish,
        "no fish here: `targo --unverified verify provision` builds the pinned 4.9.3 into this \
         user's cache (or ATERM_TEST_FISH=<path> names one)"
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
    // Nothing provides Python 3.11+ until S11, and the words say the floor: macOS's own
    // python3 is here, and older.
    let python = tool("python3").refusal();
    assert!(
        python.starts_with("no Python 3.11+ here: aterm provides none: S11's")
            && python.contains("ATERM_TEST_PYTHON3=<path>"),
        "{python}"
    );
    let zstd = tool("zstd").refusal();
    assert!(
        zstd.contains("aterm provides none: only a rostered publish host"),
        "{zstd}"
    );
    let cc = tool("cc").refusal();
    assert!(
        cc.contains("`aterm init --only base.clt` installs them")
            && !cc.contains("xcode-select --install"),
        "the CLT row names base.clt, never the dialog: {cc}"
    );
    let gpu = tool("gpu").refusal();
    assert_eq!(
        gpu,
        "this host lacks a GPU wgpu can open, and no command adds it"
    );
    for refusal in [fish, pwsh, lima, bindgen, vim, ty, python, zstd, cc, gpu] {
        assert!(!refusal.contains("brew"), "no remedy says brew: {refusal}");
    }
}

/// Every pinned row's remedy, on a host it is pinned for, is the `verify provision` run its
/// `when` names, with that run's flag and no other, and no row names `aterm init
/// --develop` before S8 lands it. NEGATIVE CONTROL: a `--linux` row's words without the
/// flag would send a person to a run that skips it.
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
        assert!(
            !refusal.contains("init --develop") && !refusal.contains("brew"),
            "{refusal}"
        );
        assert_eq!(row.provided_by(), when.verb());
    }
    assert!(seen >= 8, "the pinned rows: {seen}");
    assert_eq!(When::Linux.verb(), "verify provision --linux");
    assert_eq!(
        When::Always.command(),
        "targo --unverified verify provision"
    );
}

/// On a host the tree pins nothing for, a pinned row's refusal says aterm provides none
/// there and which hosts the run covers, rather than naming a run that would answer
/// "nothing is pinned". NEGATIVE CONTROL: the same rows on a Mac name their run, and a row
/// built wherever targo runs (wasm-bindgen) names it on Linux too.
#[test]
fn a_host_the_tree_pins_nothing_for_is_told_so() {
    for name in ["fish", "pwsh", "wasm-opt", "limactl"] {
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

/// A pin's directory is its package's, so a program a package carries under another name
/// resolves where `verify provision` lays the package.
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
    assert_eq!(tool("rustup").pinned_in(t), None);
}

/// §8.3's third step before its fourth: a row's usual install path is found ahead of
/// the same name on `$PATH`, and `$PATH` only once no usual path holds it, so a host
/// whose PATH leads with another copy still tests the one the row names.
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

/// §10 S2: a missing vim ends COULD NOT RUN, with the provider in the words, never a pass
/// and never a FAIL.
///
/// The words are read from the pure halves ([`Tool::refusal`], [`refusals_for`]), never
/// from a caught COULD NOT RUN: that panic's sentinel lands in this test's own output,
/// which would record a regression here as the machine's refusal. The one caught panic
/// comes last and is asserted only to carry the sentinel, which a defect cannot then hide.
#[test]
fn a_missing_tool_ends_could_not_run_naming_its_provider() {
    // The lookup injected empty: no variable, no tooling cache, no usual path, no $PATH.
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
    // Absolute on every host the test runs on: `/x` has no drive, so on Windows it
    // is relative, and the XDG rule ignores it.
    let xdg = Path::new(env!("CARGO_MANIFEST_DIR"));
    assert_eq!(
        user_cache("macos", Some(home), Some(xdg), "browser"),
        Some(PathBuf::from("/h/Library/Caches/aterm/browser.noindex"))
    );
    assert_eq!(
        user_cache("linux", Some(home), Some(xdg), TOOLING),
        Some(xdg.join("aterm/tooling"))
    );
    // A relative XDG_CACHE_HOME is ignored, as the spec says.
    assert_eq!(
        user_cache("linux", Some(home), Some(Path::new("rel")), "browser"),
        Some(PathBuf::from("/h/.cache/aterm/browser"))
    );
    assert_eq!(user_cache("linux", None, None, "browser"), None);
}
