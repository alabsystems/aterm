// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;

#[cfg(unix)]
fn git_ok(args: &[&str]) {
    let out = fetch::run_capture(aterm_uds::os_path!("git"), args).unwrap();
    assert!(
        out.status.success(),
        "git {:?}: {}",
        args,
        String::from_utf8_lossy(&out.stderr)
    );
}

/// r376: livecheck blocks parse as data across the shapes
/// the core tap actually uses (samples verbatim from
/// homebrew-core); interpolation and unknown statements
/// refuse the whole block (fail-closed).
#[test]
fn livecheck_block_parses_as_data() {
    use super::super::formula::LivecheckUrl;
    let spec = parse_livecheck_block(
        "class Jq < Formula\n  livecheck do\n    url :stable\n    regex(/^(?:jq[._-])?v?(\\d+(?:\\.\\d+)+)$/i)\n  end\nend\n",
    )
    .unwrap();
    assert_eq!(spec.url, Some(LivecheckUrl::Symbol("stable".into())));
    assert_eq!(
        spec.regex.as_deref(),
        Some(r"^(?:jq[._-])?v?(\d+(?:\.\d+)+)$")
    );
    assert_eq!(spec.regex_flags, "i");
    assert!(!spec.url_options && !spec.strategy_block);

    let spec = parse_livecheck_block(
        "  livecheck do\n    url \"https://files.radicle.xyz/releases/latest/radicle.json\"\n    strategy :json do |json|\n      json[\"version\"]\n    end\n  end\n",
    )
    .unwrap();
    assert_eq!(
        spec.url,
        Some(LivecheckUrl::Literal(
            "https://files.radicle.xyz/releases/latest/radicle.json".into()
        ))
    );
    assert_eq!(spec.strategy.as_deref(), Some("json"));
    assert!(spec.strategy_block);

    let spec = parse_livecheck_block(
        "  livecheck do\n    regex(%r{<version>v?(\\d+(?:\\.\\d+)+)</version>}i)\n    throttle days: 1\n  end\n",
    )
    .unwrap();
    assert_eq!(
        spec.regex.as_deref(),
        Some(r"<version>v?(\d+(?:\.\d+)+)</version>")
    );
    assert_eq!(spec.throttle_days, Some(1));

    let spec = parse_livecheck_block(
        "  livecheck do\n    skip \"Not actively developed or maintained\"\n  end\n",
    )
    .unwrap();
    assert_eq!(
        spec.skip.as_deref(),
        Some("Not actively developed or maintained")
    );

    // Nested resource block is not the package block, and a
    // block after it still resolves as the package one.
    let none = parse_livecheck_block(
        "  resource \"man\" do\n    livecheck do\n      formula :parent\n    end\n  end\n",
    );
    assert!(none.is_none());
    let spec = parse_livecheck_block(
        "  livecheck do\n    formula \"git\"\n  end\n  resource \"man\" do\n    livecheck do\n      formula :parent\n    end\n  end\n",
    )
    .unwrap();
    assert_eq!(spec.formula_ref.as_deref(), Some("git"));

    // Interpolation and unknown statements refuse, except
    // the version forms a url may carry (r386): stored
    // verbatim for `#{version...}`, rewritten for
    // `#{LATEST_VERSION}`.
    assert!(
        parse_livecheck_block("  livecheck do\n    url \"https://example.com/#{foo}/\"\n  end\n")
            .is_none()
    );
    assert!(parse_livecheck_block("  livecheck do\n    regex(/v#{version}/)\n  end\n").is_none());
    let spec = parse_livecheck_block(
        "  livecheck do\n    url \"https://example.com/channels/#{version.major}/v#{LATEST_VERSION}/x\"\n  end\n",
    )
    .unwrap();
    assert_eq!(
        spec.url,
        Some(super::super::formula::LivecheckUrl::Literal(
            "https://example.com/channels/#{version.major}/v<FORMULA_LATEST_VERSION>/x".into()
        ))
    );
    assert!(
        parse_livecheck_block("  livecheck do\n    url :stable\n    whatever 1\n  end\n").is_none()
    );
    assert!(parse_livecheck_block("class X < Formula\nend\n").is_none());
}

#[test]
fn tap_add_refusals_match_brew() {
    let tmp = aterm_tempfile::tempdir().unwrap();
    let prefix = Prefix::at(&tmp.path().join("prefix"));
    prefix.ensure().unwrap();
    let err = |name: &str, url: Option<&str>, custom: bool, force: bool| {
        add(&prefix, name, url, custom, force).unwrap_err()
    };
    assert!(err("homebrew/services", None, false, false).contains("was deprecated"));
    assert!(err("homebrew/bundle", None, false, false).contains("was deprecated"));
    assert_eq!(
        err("caskroom/fonts", None, false, false),
        "Error: caskroom/fonts was moved. Tap homebrew/cask-fonts instead."
    );
    assert_eq!(
        err("someone/tap", None, true, false),
        "Error: Tap someone/tap with option `--custom-remote` but without a remote URL."
    );
    assert!(err("homebrew/core", None, false, false).contains("--force"));
    assert!(err("homebrew/cask", None, false, false).contains("--force"));
    assert!(err("linuxbrew/core", None, false, false).contains("--force"));
}

// abrew refuses to run on Windows (Homebrew is macOS/Linux software).
#[cfg(unix)]
#[test]
fn tap_add_mismatch_and_custom_remote() {
    let tmp = aterm_tempfile::tempdir().unwrap();
    let origin = tmp.path().join("origin");
    std::fs::create_dir(&origin).unwrap();
    let origin_s = origin.to_string_lossy().into_owned();
    git_ok(&["-C", &origin_s, "init", "-q", "-b", "main"]);
    std::fs::write(origin.join("f.rb"), b"# tap").unwrap();
    git_ok(&["-C", &origin_s, "add", "f.rb"]);
    git_ok(&[
        "-C",
        &origin_s,
        "-c",
        "user.email=t@example.invalid",
        "-c",
        "user.name=t",
        "commit",
        "-qm",
        "one",
    ]);
    let prefix = Prefix::at(&tmp.path().join("prefix"));
    prefix.ensure().unwrap();
    let (tap, newly) = add(&prefix, "someone/tap", Some(&origin_s), false, false).unwrap();
    assert!(newly);
    // Re-tap with a different URL refuses with brew's mismatch text.
    let error = add(&prefix, "someone/tap", Some("/elsewhere"), false, false).unwrap_err();
    assert_eq!(
        error,
        format!("Error: Tap someone/tap remote mismatch.\n{origin_s} != /elsewhere")
    );
    // --custom-remote repoints the tap at the new remote.
    let mirror = tmp.path().join("mirror");
    let mirror_s = mirror.to_string_lossy().into_owned();
    git_ok(&["clone", "-q", &origin_s, &mirror_s]);
    add(&prefix, "someone/tap", Some(&mirror_s), true, false).unwrap();
    let remote = git_out(&tap.path, &["remote", "get-url", "origin"]).unwrap();
    assert_eq!(remote, mirror_s);
}

// abrew refuses to run on Windows (Homebrew is macOS/Linux software).
#[cfg(unix)]
#[test]
fn repair_follows_an_upstream_default_branch_rename() {
    let tmp = aterm_tempfile::tempdir().unwrap();
    let origin = tmp.path().join("origin");
    std::fs::create_dir(&origin).unwrap();
    let origin_s = origin.to_string_lossy().into_owned();
    git_ok(&["-C", &origin_s, "init", "-q", "-b", "trunk"]);
    std::fs::write(origin.join("f.rb"), b"# tap").unwrap();
    git_ok(&["-C", &origin_s, "add", "f.rb"]);
    git_ok(&[
        "-C",
        &origin_s,
        "-c",
        "user.email=t@example.invalid",
        "-c",
        "user.name=t",
        "commit",
        "-qm",
        "one",
    ]);
    let prefix = Prefix::at(&tmp.path().join("prefix"));
    prefix.ensure().unwrap();
    let tap_dir = prefix.taps_dir().join("someone/homebrew-tap");
    std::fs::create_dir_all(tap_dir.parent().unwrap()).unwrap();
    let tap_s = tap_dir.to_string_lossy().into_owned();
    git_ok(&["clone", "-q", &origin_s, &tap_s]);
    // Stale state: local branch and origin/HEAD still say master.
    git_ok(&["-C", &tap_s, "branch", "-m", "trunk", "master"]);
    git_ok(&[
        "-C",
        &tap_s,
        "symbolic-ref",
        "refs/remotes/origin/HEAD",
        "refs/remotes/origin/master",
    ]);
    repair(&prefix).unwrap();
    let branches = git_out(&tap_dir, &["branch", "--format=%(refname:short)"]).unwrap();
    assert_eq!(branches, "trunk");
    assert_eq!(origin_branch_name(&tap_dir).as_deref(), Some("trunk"));
    // A second repair is a quiet no-op.
    repair(&prefix).unwrap();
    // No taps at all is fine too.
    let empty = Prefix::at(&tmp.path().join("empty"));
    empty.ensure().unwrap();
    repair(&empty).unwrap();
}

#[test]
fn untap_refuses_while_tap_packages_are_installed() {
    fn payload(p: &Prefix, kind: Kind, version: &str, full: Option<&str>) -> PathBuf {
        let dir = match kind {
            Kind::Formula => p.keg("tool", version),
            Kind::Cask => p.cask_dir("tool", version),
        };
        std::fs::create_dir_all(&dir).unwrap();
        let mut fields = vec![
            ("installed_by", "abrew"),
            (
                match kind {
                    Kind::Formula => "name",
                    Kind::Cask => "token",
                },
                "tool",
            ),
            ("version", version),
        ];
        if let Some(full) = full {
            fields.push(("full_name", full));
        }
        p.write_receipt(&dir, &fields).unwrap();
        dir
    }
    fn tapped(p: &Prefix) -> PathBuf {
        let tap = p.taps_dir().join("someone/homebrew-tap");
        std::fs::create_dir_all(&tap).unwrap();
        std::fs::write(tap.join("KEEP"), b"tap recipes").unwrap();
        tap
    }
    fn refused(p: &Prefix, tap: &std::path::Path) -> String {
        let error = remove(p, "someone/tap").unwrap_err();
        assert_eq!(std::fs::read(tap.join("KEEP")).unwrap(), b"tap recipes");
        error
    }
    let tmp = aterm_tempfile::tempdir().unwrap();
    for kind in [Kind::Formula, Kind::Cask] {
        let p = Prefix::at(&tmp.path().join(kind.word()));
        p.ensure().unwrap();
        let official = match kind {
            Kind::Formula => "homebrew/core/tool",
            Kind::Cask => "homebrew/cask/tool",
        };
        let old = payload(&p, kind, "1.0", Some("someone/tap/tool"));
        let current = payload(&p, kind, "2.0", Some(official));
        let tap = tapped(&p);
        let error = refused(&p, &tap);
        assert!(error.contains("Refusing to untap someone/tap"), "{error}");
        assert!(error.contains("someone/tap/tool"), "{error}");
        assert!(old.is_dir() && current.is_dir());
        payload(&p, kind, "1.0", Some("someone/homebrew-tap/tool"));
        assert!(refused(&p, &tap).contains("someone/tap/tool"));
        std::fs::remove_dir_all(&old).unwrap();
        let receipt = current.join("INSTALL_RECEIPT.json");
        let official_bytes = std::fs::read(&receipt).unwrap();
        remove(&p, "someone/tap").unwrap();
        assert!(
            !tap.exists(),
            "an unrelated installed version is no use of this tap"
        );
        assert_eq!(std::fs::read(&receipt).unwrap(), official_bytes);

        let tap = tapped(&p);
        let key = match kind {
            Kind::Formula => "name",
            Kind::Cask => "token",
        };
        for full in [
            "null",
            "42",
            "\"\"",
            "\"other\"",
            "\"someone/tap/other\"",
            "\"someone/tool\"",
            "\"../tap/tool\"",
        ] {
            let bytes = format!(
                "{{\"installed_by\":\"abrew\",\"{key}\":\"tool\",\"version\":\"2.0\",\"full_name\":{full}}}"
            );
            std::fs::write(&receipt, bytes.as_bytes()).unwrap();
            let _ = refused(&p, &tap);
            assert_eq!(std::fs::read(&receipt).unwrap(), bytes.as_bytes());
        }
        for bytes in [b"{".as_slice(), b"[]".as_slice()] {
            std::fs::write(&receipt, bytes).unwrap();
            let _ = refused(&p, &tap);
            assert_eq!(std::fs::read(&receipt).unwrap(), bytes);
        }
        std::fs::remove_file(&receipt).unwrap();
        let _ = refused(&p, &tap);
        std::fs::create_dir(&receipt).unwrap();
        let _ = refused(&p, &tap);
        assert!(receipt.is_dir());
        std::fs::remove_dir(&receipt).unwrap();
        let oversized = vec![b' '; 64 * 1024 + 1];
        std::fs::write(&receipt, &oversized).unwrap();
        let _ = refused(&p, &tap);
        assert_eq!(
            std::fs::metadata(&receipt).unwrap().len(),
            oversized.len() as u64
        );
        std::fs::remove_file(&receipt).unwrap();

        #[cfg(unix)]
        {
            let external = tmp.path().join(format!("{}-receipt", kind.word()));
            std::fs::write(&external, &official_bytes).unwrap();
            std::os::unix::fs::symlink(&external, &receipt).unwrap();
            let _ = refused(&p, &tap);
            assert_eq!(std::fs::read_link(&receipt).unwrap(), external);
            assert_eq!(std::fs::read(&external).unwrap(), official_bytes);
            std::fs::remove_file(&receipt).unwrap();
            payload(&p, kind, "2.0", Some(official));
            for version_only in [false, true] {
                let row = if version_only {
                    current.clone()
                } else {
                    current.parent().unwrap().to_path_buf()
                };
                let held = tmp
                    .path()
                    .join(format!("{}-held-{version_only}", kind.word()));
                std::fs::rename(&row, &held).unwrap();
                std::os::unix::fs::symlink(&held, &row).unwrap();
                let _ = refused(&p, &tap);
                assert_eq!(std::fs::read_link(&row).unwrap(), held);
                std::fs::remove_file(&row).unwrap();
                std::fs::rename(&held, &row).unwrap();
            }
        }
        for full in [None, Some("tool")] {
            payload(&p, kind, "2.0", full);
            let tap = tapped(&p);
            remove(&p, "someone/tap").unwrap();
            assert!(!tap.exists());
            assert!(current.is_dir());
        }
    }
}

#[test]
fn local_remotes_are_the_only_file_transport() {
    for local in ["/tmp/tap", "./tap", "../tap", "~/tap", "file:///tmp/tap"] {
        assert!(is_local_remote(local), "{local}");
        assert!(
            git_protocol_args(true).contains(&"protocol.file.allow=always"),
            "{local}"
        );
    }
    for remote in [
        "https://github.com/me/homebrew-tools.git",
        "git@github.com:me/homebrew-tools.git",
        "ssh://git@example.com/x.git",
        "ext::sh -c true",
        "fd::0",
    ] {
        assert!(!is_local_remote(remote), "{remote}");
    }
    assert!(
        !git_protocol_args(false).contains(&"protocol.file.allow=always"),
        "non-local remotes never allow the file transport"
    );
}

#[test]
fn rb_parse_as_data() {
    let rb = "class Foo < Formula\n  desc \"A foo\"\n  homepage \"https://foo.example\"\n  url \"https://foo.example/foo-2.1.tar.gz\"\n  sha256 \"aaaa\"\n  depends_on \"openssl@3\"\nend\n";
    let f = parse_rb_as_data(rb, "me/tap/foo");
    assert_eq!(f.name, "foo");
    assert_eq!(f.version, "2.1");
    assert_eq!(f.stable_sha256, "aaaa");
    assert_eq!(f.dependencies, ["openssl@3"]);
}

#[test]
fn rb_parse_service_block_as_data() {
    let rb = "class Foo < Formula\n  url \"https://foo.example/foo-2.1.tar.gz\"\n  service do\n    run [opt_bin/\"foo\", \"--serve\", \"-c\", etc/\"foo.conf\"]\n    require_root true\n    environment_variables FOO: \"bar\"\n    log_path var/\"log/foo.log\"\n    error_log_path var/\"log/foo.err\"\n    interval 3600\n    name macos: \"com.example.foo\"\n  end\nend\n";
    let f = parse_rb_as_data(rb, "me/tap/foo");
    let svc = f.service.expect("service block parsed");
    assert_eq!(
        svc.run,
        [
            "$HOMEBREW_PREFIX/opt/foo/bin/foo",
            "--serve",
            "-c",
            "$HOMEBREW_PREFIX/etc/foo.conf",
        ]
    );
    assert!(svc.require_root);
    assert_eq!(
        svc.environment_variables,
        [("FOO".to_string(), "bar".to_string())]
    );
    assert_eq!(
        svc.log_path.as_deref(),
        Some("$HOMEBREW_PREFIX/var/log/foo.log")
    );
    assert_eq!(
        svc.error_log_path.as_deref(),
        Some("$HOMEBREW_PREFIX/var/log/foo.err")
    );
    assert_eq!(svc.interval, Some(3600));
    assert_eq!(svc.name.as_deref(), Some("com.example.foo"));
    // An interpolated run is beyond data-only parsing.
    let rb = "class Foo < Formula\n  service do\n    run [opt_bin/\"foo\", \"--opt=#{etc}/x\"]\n  end\nend\n";
    assert!(parse_rb_as_data(rb, "me/tap/foo").service.is_none());
}

#[test]
fn tap_name_parse() {
    assert_eq!(
        parse_tap_name("me/homebrew-tools"),
        Some(("me".into(), "tools".into()))
    );
    assert_eq!(parse_tap_name("bogus"), None);
    for invalid in [
        "../x",
        "./x",
        "/x",
        "me/..",
        "me/.",
        "me/homebrew-",
        "me/homebrew-..",
        "me/x/y",
        "me\\x/repo",
        "me/repo\\x",
        "me/repo\0",
    ] {
        assert_eq!(parse_tap_name(invalid), None, "{invalid:?}");
    }
    let tmp = aterm_tempfile::TempDir::new().unwrap();
    let p = Prefix::at(&tmp.path().join("prefix"));
    p.ensure().unwrap();
    let outside = p.root.join("Library/homebrew-x");
    std::fs::create_dir(&outside).unwrap();
    std::fs::write(outside.join("marker"), b"keep").unwrap();
    assert!(remove(&p, "../x").is_err());
    assert!(lookup(&p, "../x/marker").is_none());
    assert_eq!(std::fs::read(outside.join("marker")).unwrap(), b"keep");
    let missing = p.taps_dir().join("missing-user");
    assert!(remove(&p, "missing-user/x").is_err());
    assert!(!missing.exists(), "untap never creates a parent");
    let (ordinary, update) = mutation_path(&p, "me", "tools", true).unwrap();
    assert!(!update);
    std::fs::create_dir(&ordinary).unwrap();
    std::fs::create_dir(ordinary.join(".git")).unwrap();
    assert!(mutation_path(&p, "me", "tools", true).unwrap().1);
    std::fs::write(ordinary.join("payload"), b"ordinary").unwrap();
    remove(&p, "me/homebrew-tools").unwrap();
    assert!(!ordinary.exists());
    let regular = p.taps_dir().join("me/homebrew-regular");
    std::fs::write(&regular, b"keep regular").unwrap();
    assert!(remove(&p, "me/regular").is_err());
    assert!(mutation_path(&p, "me", "regular", true).is_err());
    assert_eq!(std::fs::read(&regular).unwrap(), b"keep regular");
    #[cfg(unix)]
    for (label, suffix) in [
        ("root", "Library/Taps/me/homebrew-x"),
        ("Library", "Taps/me/homebrew-x"),
        ("Taps", "me/homebrew-x"),
        ("user", "homebrew-x"),
        ("leaf", ""),
    ] {
        let p = Prefix::at(&tmp.path().join(label));
        p.ensure().unwrap();
        let external = tmp.path().join(format!("external-{label}"));
        let victim = external.join(suffix);
        std::fs::create_dir_all(&victim).unwrap();
        std::fs::write(victim.join("marker"), b"keep").unwrap();
        let alias = match label {
            "root" => p.root.clone(),
            "Library" => p.root.join("Library"),
            "Taps" => p.taps_dir(),
            "user" => p.taps_dir().join("me"),
            _ => p.taps_dir().join("me/homebrew-x"),
        };
        if let Some(parent) = alias.parent() {
            std::fs::create_dir_all(parent).unwrap();
        }
        if alias.is_dir() {
            std::fs::remove_dir_all(&alias).unwrap();
        }
        std::os::unix::fs::symlink(&external, &alias).unwrap();
        assert!(remove(&p, "me/x").is_err(), "{label}");
        assert!(mutation_path(&p, "me", "x", true).is_err(), "{label}");
        assert_eq!(std::fs::read(victim.join("marker")).unwrap(), b"keep");
        assert_eq!(std::fs::read_link(&alias).unwrap(), external);
    }
    #[cfg(unix)]
    {
        let (path, _) = mutation_path(&p, "me", "worktree", true).unwrap();
        std::fs::create_dir(&path).unwrap();
        let external = tmp.path().join("external-git");
        std::fs::create_dir(&external).unwrap();
        std::fs::write(external.join("marker"), b"keep").unwrap();
        std::os::unix::fs::symlink(&external, path.join(".git")).unwrap();
        assert!(mutation_path(&p, "me", "worktree", true).is_err());
        assert_eq!(std::fs::read(external.join("marker")).unwrap(), b"keep");
    }
    assert_eq!(std::fs::read(outside.join("marker")).unwrap(), b"keep");
}

#[test]
fn migration_target_reads_local_tap_migrations() {
    let tmp = aterm_tempfile::tempdir().unwrap();
    let p = Prefix::at(tmp.path());
    p.ensure().unwrap();
    assert_eq!(migration_target_tap(&p, "example/tap", "aztfy"), None);
    let dir = p.taps_dir().join("example/homebrew-tap");
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(
        dir.join("tap_migrations.json"),
        br#"{"aztfy": "homebrew/core", "old": "other/tap/renamed"}"#,
    )
    .unwrap();
    assert_eq!(
        migration_target_tap(&p, "example/tap", "aztfy").as_deref(),
        Some("homebrew/core")
    );
    assert_eq!(
        migration_target_tap(&p, "example/homebrew-tap", "old").as_deref(),
        Some("other/tap")
    );
    assert_eq!(migration_target_tap(&p, "example/tap", "missing"), None);
}

#[cfg(unix)]
#[test]
fn tap_link_hook_links_manpages_and_setting_gated_completions() {
    let tmp = aterm_tempfile::tempdir().unwrap();
    let p = Prefix::at(tmp.path());
    p.ensure().unwrap();
    let root = p.taps_dir().join("someone/homebrew-tools");
    for dir in ["manpages", "completions/zsh", ".git"] {
        std::fs::create_dir_all(root.join(dir)).unwrap();
    }
    std::fs::write(root.join("manpages/tool.1"), b"man").unwrap();
    std::fs::write(root.join("completions/zsh/_tool"), b"#compdef tool").unwrap();
    let tap = Tap {
        user: "someone".to_string(),
        repo: "tools".to_string(),
        path: root.clone(),
    };
    // Unlinked setting: manpages link, completions do not.
    link_completions_and_manpages(&p, &tap);
    assert!(p.root.join("share/man/man1/tool.1").exists());
    assert!(!p.root.join("share/zsh/site-functions/_tool").exists());
    assert!(completions_message_shown(&p));
    // Linked setting: completions follow.
    set_completions_linked(&p, true).unwrap();
    link_completions_and_manpages(&p, &tap);
    assert!(p.root.join("share/zsh/site-functions/_tool").exists());
    // Untap removes both link sets.
    remove(&p, "someone/tools").unwrap();
    assert!(!p.root.join("share/man/man1/tool.1").exists());
    assert!(!p.root.join("share/zsh/site-functions/_tool").exists());
    assert!(!root.exists());
}

#[test]
fn git_redirect_parsing_and_refusal_match_brew() {
    let stderr = "warning: redirecting to https://github.com/New/tools.git/\nFrom https://github.com/old/tools\n";
    assert_eq!(
        redirected_remotes(stderr).collect::<Vec<_>>(),
        vec!["https://github.com/New/tools.git/"]
    );
    // Non-ASCII prefix whose Unicode `.to_lowercase()` expands UTF-8 byte length (`İ` -> `i\u{307}`)
    // must not shift or panic the byte slice into `line`.
    assert_eq!(
        unsafe_redirect(
            "https://github.com/old/tools",
            "İhtar: Redirecting to https://github.com/New/tools.git/\n",
        )
        .as_deref(),
        Some("https://github.com/New/tools.git/")
    );
    // Multi-hop redirect where the first hop canonicalizes `.git/` (`same_remote`) and the
    // second hop crosses to a new repository must still be refused with the new target.
    let multi_hop = "warning: redirecting to https://github.com/old/tools.git/\n\
                     warning: redirecting to https://github.com/New/tools.git/\n";
    assert_eq!(
        unsafe_redirect("https://github.com/old/tools", multi_hop).as_deref(),
        Some("https://github.com/New/tools.git/")
    );
    assert_eq!(
        unsafe_redirect(
            "https://github.com/old/tools",
            "warning: redirecting to https://github.com/old/tools.git/\n",
        ),
        None
    );
    assert_eq!(redirected_remotes("From https://x\n").next(), None);
    assert!(same_remote(
        "git@github.com:Old/Tools.git",
        "https://github.com/old/tools/"
    ));
    assert!(same_remote(
        "https://github.com/old/tools",
        "https://github.com/old/tools.git"
    ));
    assert!(!same_remote(
        "https://github.com/old/tools",
        "https://github.com/new/tools"
    ));
    assert!(!same_remote("", "https://github.com/new/tools"));
    assert!(!same_remote("   ", "   "));
    assert_eq!(
        redirect_refusal("old/tools", "https://github.com/new/tools"),
        "Error: Refusing automatic redirect from old/tools to https://github.com/new/tools.\n\
         Untap old/tools, then tap the new location directly to review its source and trust."
    );
}
/// r384: resource stanzas parse in file order with
/// other-OS blocks uncollected; interpolation, a missing
/// url or an unterminated block refuse the file.
#[test]
fn resource_stanzas_parse_in_file_order() {
    let text = "class Py < Formula\n  resource \"wheel\" do\n    url \"https://files.pythonhosted.org/packages/a/wheel-0.45.0.tar.gz\"\n    sha256 \"x\"\n  end\n\n  on_linux do\n    resource \"linonly\" do\n      url \"https://example.com/linonly-1.0.tar.gz\"\n    end\n  end\n\n  resource \"packaging\" do\n    url \"https://files.pythonhosted.org/packages/b/packaging-26.3.tar.gz\"\n    version \"26.3\"\n  end\nend\n";
    let got = parse_resource_stanzas(text).unwrap();
    let names: Vec<&str> = got.iter().map(|(n, _, _, _)| n.as_str()).collect();
    if cfg!(target_os = "linux") {
        assert_eq!(names, ["wheel", "linonly", "packaging"]);
    } else {
        assert_eq!(names, ["wheel", "packaging"]);
    }
    assert_eq!(got[0].2, None);
    assert_eq!(got[got.len() - 1].2.as_deref(), Some("26.3"));
    assert!(
        parse_resource_stanzas(
            "  resource \"a\" do\n    url \"https://x/#{version}.tgz\"\n  end\n"
        )
        .is_none()
    );
    assert!(parse_resource_stanzas("  resource \"a\" do\n    sha256 \"x\"\n  end\n").is_none());
    assert!(
        parse_resource_stanzas("  resource \"a\" do\n    url \"https://x/a-1.tgz\"\n").is_none()
    );
    assert_eq!(
        parse_resource_stanzas("class A < Formula\nend\n").unwrap(),
        Vec::new()
    );
}

/// r388: `user_agent:` url options parse (multi-line and
/// same-line); any other option still sets `url_options`.
#[test]
fn livecheck_user_agent_option_parses() {
    let spec = parse_livecheck_block(
        "class M < Formula\n  livecheck do\n    url \"https://example.com/dl\",\n        user_agent: :browser\n    regex(/v(\\d+)/)\n  end\nend\n",
    )
    .unwrap();
    assert_eq!(spec.user_agent.as_deref(), Some(":browser"));
    assert!(!spec.url_options);
    assert_eq!(spec.regex.as_deref(), Some(r"v(\d+)"));
    let spec = parse_livecheck_block(
        "  livecheck do\n    url \"https://example.com/dl\", user_agent: \"Mozilla/9.9 Probe, Agent\"\n  end\n",
    )
    .unwrap();
    assert_eq!(spec.user_agent.as_deref(), Some("Mozilla/9.9 Probe, Agent"));
    assert!(!spec.url_options);
    let spec = parse_livecheck_block(
        "  livecheck do\n    url \"https://example.com/dl\", using: :post\n  end\n",
    )
    .unwrap();
    assert!(spec.url_options);
    assert_eq!(spec.user_agent, None);
}

/// r387: a tap formula's version detects from the file
/// name's version token, not digits inside the name.
#[test]
fn version_from_url_uses_version_token() {
    let f = parse_rb_as_data(
        "class Gate403 < Formula\n  url \"https://example.com/gate403-1.0.tar.gz\"\nend\n",
        "probeuser/probe/gate403",
    );
    assert_eq!(f.version, "1.0");
    let f = parse_rb_as_data(
        "class Foo < Formula\n  url \"https://example.com/foo-v2.10.0.tgz\"\nend\n",
        "probeuser/probe/foo",
    );
    assert_eq!(f.version, "2.10.0");
}

/// r385: a resource's own livecheck block is extracted and parsed,
/// and its `url` line is never mistaken for the resource's.
#[test]
fn resource_livecheck_block_parses() {
    use super::super::formula::LivecheckUrl;
    let text = "class Py < Formula\n  resource \"wheel\" do\n    livecheck do\n      url \"https://files.pythonhosted.org/packages/c/flit_core-9.9.tar.gz\"\n    end\n\n    url \"https://files.pythonhosted.org/packages/a/wheel-0.45.0.tar.gz\"\n    sha256 \"x\"\n  end\n\n  resource \"man\" do\n    url \"https://example.com/man-1.0.tar.gz\"\n\n    livecheck do\n      formula :parent\n    end\n  end\nend\n";
    let got = parse_resource_stanzas(text).unwrap();
    assert_eq!(got.len(), 2);
    assert_eq!(
        got[0].1,
        "https://files.pythonhosted.org/packages/a/wheel-0.45.0.tar.gz"
    );
    let spec = got[0].3.as_ref().expect("resource livecheck parsed");
    assert_eq!(
        spec.url,
        Some(LivecheckUrl::Literal(
            "https://files.pythonhosted.org/packages/c/flit_core-9.9.tar.gz".into()
        ))
    );
    let spec = got[1].3.as_ref().expect("parent ref parsed");
    assert_eq!(spec.formula_ref.as_deref(), Some(":parent"));
    // A livecheck block the package parser refuses (an unknown
    // statement) refuses the file.
    let bad = "  resource \"a\" do\n    url \"https://x/a-1.tgz\"\n\n    livecheck do\n      frobnicate \"x\"\n    end\n  end\n";
    assert!(parse_resource_stanzas(bad).is_none());
}

/// r397: `:json` strategy blocks reduce to data shapes for
/// the enumerable bodies; anything else stays block-only.
#[test]
fn json_strategy_block_shapes() {
    use super::super::formula::{JsonKey, JsonShape};
    let spec = |body: &str| {
        let text = format!(
            "class J < Formula\n  livecheck do\n    url \"https://example.com/x.json\"\n{body}\n  end\nend\n"
        );
        parse_livecheck_block(&text)
    };
    let shape = |body: &str| spec(body).and_then(|s| s.json_block.map(|b| b.shape));
    assert_eq!(
        shape("    strategy :json do |json|\n      json[\"version\"]\n    end"),
        Some(JsonShape::Dig(vec![JsonKey::Key("version".into())]))
    );
    assert_eq!(
        shape("    strategy :json do |json|\n      json.dig(\"a\", \"b\", 0)\n    end"),
        Some(JsonShape::Dig(vec![
            JsonKey::Key("a".into()),
            JsonKey::Key("b".into()),
            JsonKey::Idx(0),
        ]))
    );
    assert_eq!(
        shape(
            "    strategy :json do |json|\n      json[\"releases\"]&.map { |item| item[\"version\"] }\n    end"
        ),
        Some(JsonShape::MapKey(
            vec![JsonKey::Key("releases".into())],
            Some("version".into())
        ))
    );
    assert_eq!(
        shape(
            "    strategy :json do |json, regex|\n      json.dig(\"a\", \"url\")&.[](regex, 1)\n    end"
        ),
        Some(JsonShape::DigRegex(vec![
            JsonKey::Key("a".into()),
            JsonKey::Key("url".into()),
        ]))
    );
    // Regex shapes without the regex parameter do not reduce.
    assert_eq!(
        shape("    strategy :json do |json|\n      json[\"a\"]&.[](regex, 1)\n    end"),
        None
    );
    // A guarded body reduces since r400 (see
    // `json_guarded_map_shapes`).
    let s = spec(
        "    strategy :json do |json|\n      json[\"x\"]&.map do |item|\n        next if item[\"s\"] != \"y\"\n\n        item[\"version\"]\n      end\n    end",
    )
    .unwrap();
    assert!(s.strategy_block && s.json_block.is_some());
}

/// r398: `:xml` strategy blocks reduce to data shapes for
/// the enumerable bodies; anything else stays block-only.
#[test]
fn xml_strategy_block_shapes() {
    use super::super::formula::XmlShape;
    let spec = |body: &str| {
        let text = format!(
            "class X < Formula\n  livecheck do\n    url \"https://example.com/x.xml\"\n{body}\n  end\nend\n"
        );
        parse_livecheck_block(&text)
    };
    let shape = |body: &str| spec(body).and_then(|s| s.xml_block.map(|b| b.shape));
    assert_eq!(
        shape(
            "    strategy :xml do |xml|\n      xml.get_elements(\"//properties/ver\").map(&:text)\n    end"
        ),
        Some(XmlShape::Texts(vec!["properties".into(), "ver".into()]))
    );
    assert_eq!(
        shape(
            "    strategy :xml do |xml, regex|\n      xml.get_elements(\"//Contents/Key\").filter_map do |item|\n        item.text&.[](regex, 1)\n      end\n    end"
        ),
        Some(XmlShape::TextsRegex(vec!["Contents".into(), "Key".into()]))
    );
    // A non-descendant path does not reduce.
    assert_eq!(
        shape(
            "    strategy :xml do |xml|\n      xml.get_elements(\"Contents/Key\").map(&:text)\n    end"
        ),
        None
    );
}

/// r399: `:yaml` strategy blocks reduce to data shapes for
/// the dig/index bodies; anything else stays block-only.
#[test]
fn yaml_strategy_block_shapes() {
    use super::super::formula::{JsonKey, YamlShape};
    let spec = |body: &str| {
        let text = format!(
            "class X < Formula\n  livecheck do\n    url \"https://example.com/x.yaml\"\n{body}\n  end\nend\n"
        );
        parse_livecheck_block(&text)
    };
    let shape = |body: &str| spec(body).and_then(|s| s.yaml_block.map(|b| b.shape));
    assert_eq!(
        shape(
            "    strategy :yaml do |yaml, regex|\n      yaml.dig(\"importers\", \".\", \"version\")&.[](regex, 1)\n    end"
        ),
        Some(YamlShape::DigRegex(vec![
            JsonKey::Key("importers".into()),
            JsonKey::Key(".".into()),
            JsonKey::Key("version".into()),
        ]))
    );
    assert_eq!(
        shape("    strategy :yaml do |yaml|\n      yaml.dig(\"items\", 1)\n    end"),
        Some(YamlShape::Dig(vec![
            JsonKey::Key("items".into()),
            JsonKey::Idx(1),
        ]))
    );
    // A computed body does not reduce.
    assert_eq!(
        shape(
            "    strategy :yaml do |yaml|\n      yaml.dig(\"a\").to_s.gsub(\"-\", \"_\")\n    end"
        ),
        None
    );
    // The regex-capture form needs the regex parameter.
    assert_eq!(
        shape("    strategy :yaml do |yaml|\n      yaml.dig(\"a\")&.[](regex, 1)\n    end"),
        None
    );
}

/// r400: `:json` guarded map bodies reduce to data:
/// `next if/unless` guards (or one postfix guard) over
/// enumerable field conditions around a simple
/// projection.
#[test]
fn json_guarded_map_shapes() {
    use super::super::formula::{JsonCond, JsonGuard, JsonKey, JsonLit, JsonShape, MapGuard};
    let shape = |body: &str| {
        let text = format!(
            "class X < Formula\n  livecheck do\n    url \"https://example.com/x.json\"\n{body}\n  end\nend\n"
        );
        parse_livecheck_block(&text).and_then(|s| s.json_block.map(|b| b.shape))
    };
    assert_eq!(
        shape(
            "    strategy :json do |json|\n      json[\"releases\"]&.map do |release|\n        next unless release[\"release_number\"]&.start_with?(version.major_minor)\n        next if release[\"status\"] != \"stable\"\n\n        release[\"release_number\"]\n      end\n    end"
        ),
        Some(JsonShape::MapGuarded(
            vec![JsonKey::Key("releases".into())],
            Box::new(MapGuard {
                item_key: Some("release_number".into()),
                regex: false,
                guards: vec![
                    JsonGuard {
                        unless: true,
                        cond: JsonCond::StartsWithMajorMinor("release_number".into()),
                    },
                    JsonGuard {
                        unless: false,
                        cond: JsonCond::Ne("status".into(), JsonLit::Str("stable".into())),
                    },
                ],
            }),
        ))
    );
    // Postfix guard: `unless` on the value line skips
    // when the condition holds (`next if` polarity).
    assert_eq!(
        shape(
            "    strategy :json do |json, regex|\n      json.filter_map { |item| item[\"tag_name\"]&.[](regex, 1) unless item[\"upcoming_release\"] }\n    end"
        ),
        Some(JsonShape::MapGuarded(
            vec![],
            Box::new(MapGuard {
                item_key: Some("tag_name".into()),
                regex: true,
                guards: vec![JsonGuard {
                    unless: false,
                    cond: JsonCond::Truthy("upcoming_release".into()),
                }],
            }),
        ))
    );
    // A computed guard does not reduce.
    assert_eq!(
        shape(
            "    strategy :json do |json|\n      json.map do |item|\n        next if item[\"files\"].none? { |f| f[\"stable\"] }\n        item[\"version\"]\n      end\n    end"
        ),
        None
    );
}
