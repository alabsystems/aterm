// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;
#[cfg(unix)]
use crate::test_programs::assessed;
#[cfg(unix)]
use std::os::unix::fs::PermissionsExt as _;

const TRUST_VV: &str = "rustc 1.99.0-dev (7d5533b2b 2026-09-23)\nbinary: rustc\n\
    commit-hash: 7d5533b2bb9c9417bc09c27f1acd9d0b808f8f15\ncommit-date: 2026-09-23\n\
    host: aarch64-apple-darwin\nrelease: 1.99.0-dev\ntrust: 0.1.0\nLLVM version: 22.1.2\n";
const STOCK_VV: &str = "rustc 1.97.1 (8bab26f4f 2026-07-14)\nbinary: rustc\n\
    commit-hash: 8bab26f4f68e0e26f0bb7960be334d5b520ea452\ncommit-date: 2026-07-14\n\
    host: aarch64-apple-darwin\nrelease: 1.97.1\nLLVM version: 22.1.6\n";

const E9CA4908_CARGO_VV: &str = "cargo 1.99.0-dev (e9ca49080 2026-08-18)\n\
    release: 1.99.0-dev\ncommit-hash: e9ca49080f0f71d4ec8d6bb5a6394ff122031eaa\n\
    commit-date: 2026-08-18\n\
    trust-repo-commit-hash: e9ca49080f0f71d4ec8d6bb5a6394ff122031eaa\n\
    host: aarch64-apple-darwin\nlibgit2: 1.9.4 (sys:0.21.0 vendored)\n\
    os: Mac OS 26.6.2 [64-bit]\n";
const STOCK_CARGO_VV: &str = "cargo 1.97.1 (c980f4866 2026-06-30)\nrelease: 1.97.1\n\
    commit-hash: c980f4866141969fab6254a680546a277789d6f0\ncommit-date: 2026-06-30\n\
    host: aarch64-apple-darwin\nlibgit2: 1.9.2 (sys:0.20.4 vendored)\n\
    ssl: OpenSSL 3.6.2 7 Apr 2026\nos: Mac OS 26.6.2 [64-bit]\n";

fn scratch(label: &str) -> crate::testdir::Dir {
    crate::testdir::dir(&format!("atpkg-upstream-identity-{label}-"))
}

/// A fake that answers `-vV` with `vv` and prints its arguments otherwise.
#[cfg(unix)]
fn fake_vv(vv: &str) -> String {
    let mut body = String::from("#!/bin/sh\nif [ \"$1\" = -vV ]; then\n");
    for line in vv.lines() {
        body.push_str(&format!("echo '{line}'\n"));
    }
    body.push_str("exit 0\nfi\necho \"ran: $*\"\nexit 3\n");
    body
}

/// A fake rustup answering `which [--toolchain T] NAME` from `"$0.d"`: `T`, else the
/// legacy one-line `rust-toolchain` in its working directory, else `"$0.d/default"`;
/// completed with `-aarch64-apple-darwin` as rustup completes a channel. Every question
/// is appended to `"$0.d/asked"`.
#[cfg(unix)]
const FAKE_RUSTUP: &str = "#!/bin/sh\n\
    [ \"$1\" = which ] || { echo 'error: this fake knows only which' >&2; exit 64; }\n\
    echo \"$*\" >> \"$0.d/asked\"\n\
    shift\n\
    if [ \"$1\" = --toolchain ]; then tc=$2; shift 2\n\
    elif [ -f rust-toolchain ]; then IFS= read -r tc < rust-toolchain\n\
    else IFS= read -r tc < \"$0.d/default\"; fi\n\
    for t in \"$tc\" \"$tc-aarch64-apple-darwin\"; do\n\
      if [ -x \"$0.d/toolchains/$t/bin/$1\" ]; then echo \"$0.d/toolchains/$t/bin/$1\"; exit 0; fi\n\
    done\n\
    echo \"error: toolchain '$tc' is not installed\" >&2; exit 1\n";

#[cfg(unix)]
fn link(to: &Path, at: &Path) {
    std::fs::create_dir_all(at.parent().unwrap()).unwrap();
    let _ = std::fs::remove_file(at);
    std::os::unix::fs::symlink(to, at).unwrap();
}

fn ask<'a>(name: &'a str, target: &'a Path, cwd: &'a Path) -> Ask<'a> {
    Ask {
        name,
        target,
        toolchain: None,
        cwd: Some(cwd),
        env_toolchain: None,
        rustup_home: None,
        cache: None,
    }
}

#[test]
fn vv_says_trust_by_its_trust_line_and_stock_by_its_absence() {
    let trust = parse_vv(TRUST_VV).unwrap();
    assert_eq!(trust.trust.as_deref(), Some("0.1.0"));
    assert_eq!(trust.tool, "rustc");
    assert_eq!(trust.host.as_deref(), Some("aarch64-apple-darwin"));
    assert!(Identity::Known(trust).is_trust());
    let stock = parse_vv(STOCK_VV).unwrap();
    assert_eq!(
        (stock.release.as_str(), stock.trust.as_deref()),
        ("1.97.1", None)
    );
    assert!(!Identity::Known(stock).is_trust());
    let cargo = parse_vv("cargo 1.99.0-dev (7d5533b2b 2026-09-23)\nrelease: 1.99.0-dev\ntrust: 0.1.0\ncommit-hash: 7d5533b2bb9c9417bc09c27f1acd9d0b808f8f15\n").unwrap();
    assert_eq!(
        (cargo.tool.as_str(), cargo.trust.as_deref()),
        ("cargo", Some("0.1.0"))
    );
    assert_eq!(parse_vv(""), None);
    assert_eq!(parse_vv("homebrew-style cargo: -vV\n"), None);
    assert_eq!(parse_vv("rustc 1.0\nrelease: \n"), None, "an empty release");
    assert_eq!(
        parse_vv("rustc 1.2.3\nrelease: 1.2.3\ncommit-hash: unknown\n")
            .unwrap()
            .commit,
        None
    );
    assert!(!Identity::Unknown { why: "x".into() }.is_trust());
}

#[test]
fn stock_is_said_only_where_nothing_marks_trust() {
    let named = |vv: &str| Resolved {
        real: PathBuf::from("/x/cargo"),
        rustup: None,
        identity: Identity::Known(parse_vv(vv).unwrap()),
    };
    let seal = named(E9CA4908_CARGO_VV);
    assert!(!seal.is_trust());
    assert!(
        !seal.verdict("cargo").contains("STOCK"),
        "{}",
        seal.verdict("cargo")
    );
    assert!(named(STOCK_CARGO_VV).verdict("cargo").contains("STOCK"));
    for (vv, mark) in [
        (
            "rustc 1.99.0-dev (7d5533b2b 2026-09-23) (trustc 0.1.0)\nrelease: 1.99.0-dev\n",
            "(trustc 0.1.0)",
        ),
        (
            "targo 1.99.0-dev (7d5533b2b 2026-09-23) (targo 0.1.0)\nrelease: 1.99.0-dev\n",
            "(targo 0.1.0)",
        ),
        (
            "rustc 1.99.0-dev\nbinary: trustc\nrelease: 1.99.0-dev\n",
            "binary: trustc",
        ),
        ("rustc 1.99.0-dev\nrelease: 1.99.0-dev\ntrust:\n", "trust:"),
    ] {
        let got = named(vv);
        assert!(!got.is_trust(), "{vv}");
        assert!(
            !got.verdict("rustc").contains("STOCK"),
            "{vv} prints {mark}: {}",
            got.verdict("rustc")
        );
    }
    let marked_trust = "cargo 1.99.0-dev (7d5533b2b 2026-09-23)\nrelease: 1.99.0-dev\n\
        trust: 0.1.0\ncommit-hash: 7d5533b2bb9c9417bc09c27f1acd9d0b808f8f15\n\
        trust-repo-commit-hash: 7d5533b2bb9c9417bc09c27f1acd9d0b808f8f15\n";
    assert!(named(marked_trust).is_trust());
}

#[test]
fn picked_follows_rustups_order_and_names_only_a_match() {
    let dir = scratch("picked");
    let home = dir.join("rustup");
    std::fs::create_dir_all(&home).unwrap();
    std::fs::write(
        home.join("settings.toml"),
        "version = \"12\"\ndefault_toolchain = \"trust\"\n\n[overrides]\n",
    )
    .unwrap();
    let target = dir.join("rustc");
    let base = Ask {
        rustup_home: Some(&home),
        ..ask("rustc", &target, &dir)
    };
    let host = Some("aarch64-apple-darwin");
    assert_eq!(picked(&base, "trust", host), Picked::Default);
    assert_eq!(
        picked(&base, "stable-aarch64-apple-darwin", host),
        Picked::Unsaid,
        "not the default, and nothing nearer names it: rustup's reason is not guessed"
    );
    assert_eq!(
        picked(
            &Ask {
                toolchain: Some("1.97.1"),
                ..base
            },
            "1.97.1-aarch64-apple-darwin",
            host
        ),
        Picked::CommandLine
    );
    assert_eq!(
        picked(
            &Ask {
                env_toolchain: Some("stable"),
                ..base
            },
            "stable-aarch64-apple-darwin",
            host
        ),
        Picked::Environment
    );
    assert_eq!(
        picked(
            &Ask {
                env_toolchain: Some(""),
                ..base
            },
            "trust",
            host
        ),
        Picked::Default,
        "an empty $RUSTUP_TOOLCHAIN is unset"
    );
    std::fs::write(
        dir.join("rust-toolchain.toml"),
        "[toolchain]\nchannel = \"1.97.1\"\n",
    )
    .unwrap();
    let file = dir.join("rust-toolchain.toml");
    assert_eq!(
        picked(&base, "1.97.1-aarch64-apple-darwin", host),
        Picked::File(file.clone())
    );
    assert_eq!(
        picked(&base, "1.97.1-aarch64-apple-darwin", None),
        Picked::Unsaid,
        "no host reported: the channel is not completed by guesswork"
    );
    assert_eq!(
        picked(&base, "trust", host),
        Picked::Default,
        "a file naming another toolchain is not the reason"
    );
}

/// A copy that is not a rustup proxy is itself: its `-vV` names it, Trust or stock, and
/// anything that does not answer as a Rust tool is not known, never Trust.
#[cfg(unix)]
#[test]
fn a_plain_copy_is_named_by_its_own_vv() {
    let dir = scratch("plain");
    for (vv, trust) in [(TRUST_VV, true), (STOCK_VV, false)] {
        let at = dir.join("bin").join("rustc");
        link(&assessed("rustc", &fake_vv(vv)), &at);
        let got = resolve(&ask("rustc", &at, &dir));
        assert_eq!(got.is_trust(), trust, "{got:?}");
        assert_eq!(got.rustup, None);
        assert_eq!(got.real, std::fs::canonicalize(&at).unwrap());
    }
    let at = dir.join("bin").join("cargo");
    link(
        &assessed(
            "not-rust",
            "#!/bin/sh\necho \"homebrew-style cargo: $*\"\nexit 3\n",
        ),
        &at,
    );
    let got = resolve(&ask("cargo", &at, &dir));
    assert_eq!(
        got.identity,
        Identity::Unknown {
            why: format!("{} -vV exited 3", at.display())
        }
    );
    let at = dir.join("bin").join("rustc");
    link(&assessed("not-rust-0", "#!/bin/sh\necho hello\n"), &at);
    let Identity::Unknown { why } = resolve(&ask("rustc", &at, &dir)).identity else {
        panic!("an answer with no release line is not known");
    };
    assert!(why.ends_with("-vV printed no `release:` line"), "{why}");
}

/// A rustup proxy is asked `rustup which` in the caller's directory: the default, a
/// file pin, a `+<toolchain>` — each named with its toolchain and why — and a toolchain
/// rustup cannot find is not known, in rustup's own words.
#[cfg(unix)]
#[test]
fn a_rustup_proxy_is_resolved_through_rustup_which() {
    let dir = scratch("proxy");
    let cargo_bin = dir.join("cargo-bin");
    let rustup = assessed("rustup", FAKE_RUSTUP);
    link(&rustup, &cargo_bin.join("rustup"));
    link(&rustup, &cargo_bin.join("rustc"));
    let answers = cargo_bin.join("rustup.d");
    std::fs::create_dir_all(&answers).unwrap();
    std::fs::write(answers.join("default"), "trust\n").unwrap();
    let trust = assessed("rustc", &fake_vv(TRUST_VV));
    let stock = assessed("rustc", &fake_vv(STOCK_VV));
    link(&trust, &answers.join("toolchains/trust/bin/rustc"));
    link(
        &stock,
        &answers.join("toolchains/1.97.1-aarch64-apple-darwin/bin/rustc"),
    );
    let home = dir.join("rustup-home");
    std::fs::create_dir_all(&home).unwrap();
    std::fs::write(
        home.join("settings.toml"),
        "default_toolchain = \"trust\"\n",
    )
    .unwrap();
    let work = dir.join("work");
    std::fs::create_dir_all(&work).unwrap();
    let target = cargo_bin.join("rustc");
    let base = Ask {
        rustup_home: Some(&home),
        ..ask("rustc", &target, &work)
    };

    let got = resolve(&base);
    assert_eq!(
        got.rustup,
        Some(Rustup {
            toolchain: "trust".into(),
            picked: Picked::Default,
        })
    );
    assert!(got.is_trust(), "{got:?}");
    assert_eq!(got.real, trust, "the toolchain's file, canonical");

    let got = resolve(&Ask {
        toolchain: Some("1.97.1"),
        ..base
    });
    assert!(!got.is_trust(), "{got:?}");
    assert_eq!(
        got.rustup
            .as_ref()
            .map(|r| (r.toolchain.as_str(), &r.picked)),
        Some(("1.97.1-aarch64-apple-darwin", &Picked::CommandLine))
    );
    assert_eq!(got.real, stock);

    let pinned = dir.join("pinned");
    std::fs::create_dir_all(&pinned).unwrap();
    std::fs::write(pinned.join("rust-toolchain"), "1.97.1\n").unwrap();
    let got = resolve(&Ask {
        cwd: Some(&pinned),
        ..base
    });
    assert_eq!(
        got.rustup.map(|r| r.picked),
        Some(Picked::File(pinned.join("rust-toolchain")))
    );
    assert!(!got.identity.is_trust());

    let got = resolve(&Ask {
        toolchain: Some("nonexist"),
        ..base
    });
    assert_eq!(
        got,
        Resolved {
            real: target.clone(),
            rustup: None,
            identity: Identity::Unknown {
                why: "`rustup which --toolchain nonexist rustc`: error: toolchain 'nonexist' is not installed".into(),
            },
        }
    );
}

/// rustup's answer is cached by everything rustup reads to give it: asked once, then
/// answered from the cache — until a toolchain file appears nearer, `settings.toml`
/// changes, or the answer is no longer a file; a failure is never cached.
#[cfg(unix)]
#[test]
fn a_rustup_answer_is_cached_by_what_rustup_reads() {
    let dir = scratch("which-cache");
    let cargo_bin = dir.join("cargo-bin");
    let rustup = assessed("rustup", FAKE_RUSTUP);
    link(&rustup, &cargo_bin.join("rustup"));
    link(&rustup, &cargo_bin.join("cargo"));
    let answers = cargo_bin.join("rustup.d");
    std::fs::create_dir_all(&answers).unwrap();
    std::fs::write(answers.join("default"), "trust\n").unwrap();
    let trust = answers.join("toolchains/trust/bin/cargo");
    link(&assessed("cargo", &fake_vv(TRUST_VV)), &trust);
    link(
        &assessed("cargo", &fake_vv(STOCK_VV)),
        &answers.join("toolchains/1.97.1-aarch64-apple-darwin/bin/cargo"),
    );
    let home = dir.join("rustup-home");
    std::fs::create_dir_all(home.join("toolchains")).unwrap();
    std::fs::write(
        home.join("settings.toml"),
        "default_toolchain = \"trust\"\n",
    )
    .unwrap();
    let work = dir.join("work").join("crate");
    std::fs::create_dir_all(&work).unwrap();
    let cache = dir.join(CACHE_FILE);
    let target = cargo_bin.join("cargo");
    let base = Ask {
        rustup_home: Some(&home),
        cache: Some(&cache),
        ..ask("cargo", &target, &work)
    };
    let asked = || {
        std::fs::read_to_string(answers.join("asked"))
            .unwrap_or_default()
            .lines()
            .count()
    };
    assert!(resolve(&base).is_trust());
    assert_eq!(asked(), 1);
    assert!(resolve(&base).is_trust(), "the cached answer");
    assert_eq!(asked(), 1, "answered from the cache");
    std::fs::write(work.join("rust-toolchain"), "1.97.1\n").unwrap();
    let got = resolve(&base);
    assert_eq!(asked(), 2);
    assert!(!got.is_trust(), "{got:?}");
    assert!(!resolve(&base).is_trust());
    assert_eq!(asked(), 2, "cached again");
    std::fs::remove_file(work.join("rust-toolchain")).unwrap();
    assert!(resolve(&base).is_trust());
    assert_eq!(asked(), 2, "the first key's answer still stands");
    std::fs::write(
        home.join("settings.toml"),
        "default_toolchain = \"trust\"\nprofile = \"minimal\"\n",
    )
    .unwrap();
    assert!(resolve(&base).is_trust());
    assert_eq!(asked(), 3);
    std::fs::remove_file(&trust).unwrap();
    let got = resolve(&base);
    assert_eq!(asked(), 4);
    assert!(matches!(got.identity, Identity::Unknown { .. }), "{got:?}");
    let _ = resolve(&base);
    assert_eq!(asked(), 5, "a failure is never cached");
}

/// A link named rustup is a proxy whatever its directory; a plain copy with its own bytes
/// is not; a copy of rustup's bytes is.
#[cfg(unix)]
#[test]
fn a_proxy_is_known_by_its_file() {
    let dir = scratch("proxy-file");
    let rustup = dir.join("rustup");
    std::fs::write(&rustup, "#!/bin/sh\nexit 0\n").unwrap();
    let named = dir.join("named").join("cargo");
    link(&rustup, &named);
    assert_eq!(
        rustup_proxy(&named, &std::fs::canonicalize(&named).unwrap()),
        Some(std::fs::canonicalize(&rustup).unwrap())
    );
    let copy = dir.join("cargo");
    std::fs::copy(&rustup, &copy).unwrap();
    assert_eq!(
        rustup_proxy(&copy, &copy),
        Some(rustup.clone()),
        "rustup's bytes"
    );
    let other = dir.join("rustc");
    std::fs::write(&other, "#!/bin/sh\nexit 1\n").unwrap();
    assert_eq!(rustup_proxy(&other, &other), None, "its own bytes");
    let lone = dir.join("lone").join("rustc");
    std::fs::create_dir_all(lone.parent().unwrap()).unwrap();
    std::fs::write(&lone, "#!/bin/sh\nexit 0\n").unwrap();
    assert_eq!(rustup_proxy(&lone, &lone), None, "no rustup beside it");

    let cargo_bin = dir.join("cargo-home").join("bin");
    std::fs::create_dir_all(&cargo_bin).unwrap();
    std::fs::copy(&rustup, cargo_bin.join("rustup")).unwrap();
    std::fs::hard_link(cargo_bin.join("rustup"), cargo_bin.join("cargo")).unwrap();
    let via = dir.join("userbin").join("cargo");
    link(&cargo_bin.join("cargo"), &via);
    let theirs = std::fs::canonicalize(cargo_bin.join("rustup")).unwrap();
    assert_eq!(
        rustup_proxy(&via, &std::fs::canonicalize(&via).unwrap()),
        Some(theirs.clone()),
        "the inode of the rustup beside the file"
    );
    std::fs::copy(&rustup, cargo_bin.join("rustc")).unwrap();
    let via = dir.join("userbin").join("rustc");
    link(&cargo_bin.join("rustc"), &via);
    assert_eq!(
        rustup_proxy(&via, &std::fs::canonicalize(&via).unwrap()),
        Some(theirs),
        "the bytes of the rustup beside the file"
    );
}

/// A plain copy (not rustup's proxy) is asked as it will run, `+<toolchain>` and all, since a
/// wrapper around a proxy passes it on. A copy that does not take a `+<toolchain>` answers as
/// it would run: not known, never Trust.
#[cfg(unix)]
#[test]
fn a_plain_copy_is_asked_with_the_toolchain_it_is_handed() {
    let dir = scratch("plus");
    let mut body = String::from(
        "#!/bin/sh\ntc=trust\ncase \"$1\" in +*) tc=${1#+}; shift;; esac\n\
         if [ \"$1\" = -vV ]; then\nif [ \"$tc\" = trust ]; then\n",
    );
    for line in TRUST_VV.lines() {
        body.push_str(&format!("echo '{line}'\n"));
    }
    body.push_str("else\n");
    for line in STOCK_VV.lines() {
        body.push_str(&format!("echo '{line}'\n"));
    }
    body.push_str("fi\nexit 0\nfi\necho \"ran $tc: $*\"\nexit 3\n");
    let at = dir.join("bin").join("cargo");
    link(&assessed("cargo-wrapper", &body), &at);
    let cache = dir.join(CACHE_FILE);
    let base = Ask {
        cache: Some(&cache),
        ..ask("cargo", &at, &dir)
    };
    let got = resolve(&Ask {
        toolchain: Some("1.97.1"),
        ..base
    });
    assert!(!got.is_trust(), "{got:?}");
    assert!(matches!(got.identity, Identity::Known(_)), "{got:?}");
    assert_eq!(got.rustup, None);
    assert!(
        resolve(&base).is_trust(),
        "the same wrapper, handed nothing"
    );
    assert!(
        resolve(&Ask {
            toolchain: Some("trust"),
            ..base
        })
        .is_trust()
    );
    let own = dir.join("own").join("cargo");
    link(&assessed("cargo", &fake_vv(STOCK_VV)), &own);
    let got = resolve(&Ask {
        toolchain: Some("1.97.1"),
        ..ask("cargo", &own, &dir)
    });
    assert_eq!(
        got.identity,
        Identity::Unknown {
            why: format!("{} +1.97.1 -vV exited 3", own.display())
        }
    );
}

/// A mise/proto-style dispatcher symlinked as `cargo` and `rustc` picks the tool by `argv[0]`
/// and directory, so an answer cached under its one path would serve the other name.
#[cfg(unix)]
#[test]
fn a_compiled_dispatcher_is_never_answered_from_the_cache() {
    let dir = scratch("dispatcher");
    let dispatcher = dir.join("tool").join("bin").join("dispatch");
    macho(&dispatcher);
    let real = std::fs::canonicalize(&dispatcher).unwrap();
    let cargo = dir.join("shims").join("cargo");
    link(&dispatcher, &cargo);
    let cache = dir.join(CACHE_FILE);
    let stamp = stamp_of(&std::fs::metadata(&real).unwrap()).unwrap();
    let vv = parse_vv(TRUST_VV).unwrap();
    cache_put(
        &cache,
        KIND_IDENTITY,
        real.to_str().unwrap(),
        &[
            &stamp,
            &vv.tool,
            &vv.release,
            vv.commit.as_deref().unwrap(),
            vv.host.as_deref().unwrap(),
            vv.trust.as_deref().unwrap(),
            "",
        ],
        MAX_IDENTITIES,
    );
    let got = resolve(&Ask {
        cache: Some(&cache),
        ..ask("cargo", &cargo, &dir)
    });
    assert!(
        !got.is_trust(),
        "asked, never read from another name's answer: {got:?}"
    );
    assert!(
        matches!(&got.identity, Identity::Unknown { why } if why.starts_with(&cargo.display().to_string())),
        "the dispatcher itself was asked (a header that cannot run: not known) — {got:?}"
    );
}

/// A Mach-O header by its magic, at `at`: a compiled binary to [`CacheKey::of`], never
/// meant to run.
#[cfg(unix)]
fn macho(at: &Path) {
    std::fs::create_dir_all(at.parent().unwrap()).unwrap();
    std::fs::write(at, [0xcf, 0xfa, 0xed, 0xfe, 0, 0, 0, 0]).unwrap();
    std::fs::set_permissions(at, std::fs::Permissions::from_mode(0o755)).unwrap();
}

/// A keyed file is asked by its own path, so the answer cached under it is that file's: here
/// the reached path runs a Trust fake while the keyed file is a header that cannot run, and
/// its own answer (none) is what counts; nothing is cached.
#[cfg(unix)]
#[test]
fn a_keyed_file_is_asked_by_its_own_path() {
    let dir = scratch("keyed");
    let rustc = dir.join("tc").join("bin").join("rustc");
    macho(&rustc);
    std::fs::create_dir_all(dir.join("tc").join("lib")).unwrap();
    std::fs::write(
        dir.join("tc").join("lib").join("librustc_driver-0a.dylib"),
        "",
    )
    .unwrap();
    let key = CacheKey::of(&rustc, "rustc").expect("a toolchain's own rustc");
    let elsewhere = dir.join("moved").join("rustc");
    link(&assessed("rustc", &fake_vv(TRUST_VV)), &elsewhere);
    let cache = dir.join(CACHE_FILE);
    let ask = Ask {
        cache: Some(&cache),
        ..ask("rustc", &elsewhere, &dir)
    };
    let got = identity_of(&ask, &elsewhere, &rustc, None);
    assert!(
        matches!(&got, Identity::Unknown { why } if why.starts_with(&rustc.display().to_string())),
        "the keyed file itself was asked — {got:?}"
    );
    assert_eq!(
        key.lookup(&cache),
        None,
        "no other file's answer under its key"
    );
}

/// Only a toolchain's own file is cached, keyed by its file and its driver: not a script, a
/// binary under another name, or one in a `bin/` with no driver beside it or in no `bin/`. A
/// key that changed misses.
#[cfg(unix)]
#[test]
fn the_cache_keys_only_a_toolchains_own_file_by_its_file_and_driver() {
    let dir = scratch("cache");
    let bin = dir.join("tc").join("bin");
    let lib = dir.join("tc").join("lib");
    std::fs::create_dir_all(&lib).unwrap();
    let rustc = bin.join("rustc");
    macho(&rustc);
    assert_eq!(
        CacheKey::of(&rustc, "rustc"),
        None,
        "no librustc_driver beside it: not known to be a toolchain's file"
    );
    let driver = lib.join("librustc_driver-0123abcd.dylib");
    std::fs::write(&driver, "old").unwrap();
    let script = bin.join("cargo");
    std::fs::write(&script, "#!/bin/sh\nexec rustup-proxied-cargo \"$@\"\n").unwrap();
    assert_eq!(
        CacheKey::of(&script, "cargo"),
        None,
        "a script is never cached"
    );
    assert_eq!(
        CacheKey::of(&rustc, "cargo"),
        None,
        "a binary asked by another name is not asked as itself"
    );
    let dispatcher = bin.join("dispatch");
    macho(&dispatcher);
    assert_eq!(CacheKey::of(&dispatcher, "rustc"), None, "{dispatcher:?}");
    let shim = dir.join("tc").join("shims").join("rustc");
    macho(&shim);
    assert_eq!(CacheKey::of(&shim, "rustc"), None, "not in a bin/");
    let key = CacheKey::of(&rustc, "rustc").expect("a toolchain's own rustc has a key");
    assert!(
        key.stamp.contains(";librustc_driver-0123abcd.dylib="),
        "{key:?}"
    );

    let cache = dir.join(CACHE_FILE);
    let vv = parse_vv(TRUST_VV).unwrap();
    assert_eq!(key.lookup(&cache), None, "empty");
    key.store(&cache, &vv);
    assert_eq!(key.lookup(&cache), Some(vv.clone()));
    std::fs::write(&driver, "rebuilt").unwrap();
    let rebuilt = CacheKey::of(&rustc, "rustc").unwrap();
    assert_ne!(rebuilt, key);
    assert_eq!(rebuilt.lookup(&cache), None);
    let stock = parse_vv(STOCK_VV).unwrap();
    rebuilt.store(&cache, &stock);
    assert_eq!(key.lookup(&cache), None, "replaced");
    assert_eq!(rebuilt.lookup(&cache), Some(stock.clone()));
    cache_put(&cache, KIND_WHICH, "a which key", &["/a/answer"], MAX_WHICH);
    for n in 0..(MAX_IDENTITIES + 8) {
        let other = CacheKey {
            path: format!("/other/{n}"),
            stamp: "1:2:3:4.5".into(),
        };
        other.store(&cache, &stock);
    }
    let text = std::fs::read_to_string(&cache).unwrap();
    assert_eq!(text.lines().count(), MAX_IDENTITIES + 1, "{text}");
    assert_eq!(
        cache_get(&cache, KIND_WHICH, "a which key"),
        Some(vec![String::from("/a/answer")])
    );
    let seal = parse_vv(E9CA4908_CARGO_VV).unwrap();
    rebuilt.store(&cache, &seal);
    assert_eq!(rebuilt.lookup(&cache), Some(seal));
    cache_put(&cache, KIND_WHICH, "tab", &["/a\tb"], MAX_WHICH);
    assert_eq!(cache_get(&cache, KIND_WHICH, "tab"), None);
}

#[test]
fn a_trust_checkout_is_known_by_its_stage0() {
    let dir = scratch("checkout");
    let root = dir.join("trust");
    let deep = root.join("compiler").join("rustc_driver").join("src");
    std::fs::create_dir_all(&deep).unwrap();
    std::fs::create_dir_all(root.join("src")).unwrap();
    std::fs::write(root.join("x.py"), "").unwrap();
    std::fs::write(
        root.join("src").join("stage0"),
        "# comment\ndist_server=x\ncompiler_dist_channel = trust\n",
    )
    .unwrap();
    assert_eq!(trust_checkout(&deep), Some(root.clone()));
    assert_eq!(trust_checkout(&root), Some(root.clone()));
    let tool = root.join("src").join("tools").join("x");
    std::fs::create_dir_all(&tool).unwrap();
    std::fs::write(tool.join("x.py"), "").unwrap();
    assert_eq!(trust_checkout(&tool), Some(root.clone()));
    let upstream = root.join("vendor").join("rust");
    std::fs::create_dir_all(upstream.join("src")).unwrap();
    std::fs::write(upstream.join("x.py"), "").unwrap();
    std::fs::write(
        upstream.join("src").join("stage0"),
        "compiler_dist_channel=nightly\n",
    )
    .unwrap();
    assert_eq!(trust_checkout(&upstream), None);
    assert_eq!(trust_checkout(&dir), None);
    assert!(!stage0_is_trust("compiler_dist_channel=trusty\n"));
    assert!(!stage0_is_trust("rustfmt_dist_channel=trust\n"));
}
