// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;

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

/// §10 S2: a stand-in at `fish-<pinned>/bin/fish` in a scratch tooling dir is found ahead
/// of the usual install paths, and `$ATERM_TEST_FISH` still comes first.
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

/// The refusal names the provider's one command, never a hand install: the pinned fish is
/// `verify provision`'s, which `aterm init --develop` runs.
#[test]
fn the_refusal_names_its_provider() {
    let fish = tool("fish").refusal();
    assert!(
        fish.starts_with("no fish here: `targo --unverified verify provision` builds the pinned ")
            && fish.contains("(`aterm init --develop` runs it)")
            && fish.contains("ATERM_TEST_FISH=<path>"),
        "{fish}"
    );
    let pwsh = tool("pwsh").refusal();
    assert!(
        pwsh.contains("verify provision` fetches the pinned "),
        "{pwsh}"
    );
    let bindgen = tool("wasm-bindgen").refusal();
    assert!(
        bindgen.contains("verify provision --record`") && !bindgen.contains("init --develop"),
        "a --record row is not init's: {bindgen}"
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
    assert!(zstd.contains("aterm provides none: S12's"), "{zstd}");
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
    for refusal in [fish, pwsh, bindgen, vim, ty, python, zstd, cc, gpu] {
        assert!(!refusal.contains("brew"), "no remedy says brew: {refusal}");
    }
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
