// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;

/// The schemas these tests seal under: a caller's own, as the release's are its.
const TEST: Schema = Schema {
    walk: b"atpkg-input-seal-test-walk-v1",
    seal: "atpkg-input-seal-test-seal-v1",
};

/// The content digest of labeled `roots` under [`TEST`]'s walk.
fn digest_roots(roots: &[(&str, &Path)]) -> Result<String, String> {
    content_digest(TEST.walk, roots)
}

/// [`digest_roots`], each file opened by `open`.
fn digest_roots_with(roots: &[(&str, &Path)], open: &mut Open<'_>) -> Result<String, String> {
    walk_roots(
        TEST.walk,
        roots,
        Some(Reads {
            open,
            list: &mut list_names,
        }),
        &|| false,
    )?
    .digest
    .ok_or_else(|| "the build input walk read no content".into())
}

/// No latency bound: removing either stop poll lets a cancelled walk complete.
#[test]
fn an_interrupted_seal_stops_before_entries_and_between_file_chunks() {
    let temp = aterm_tempfile::tempdir().unwrap();
    let file = temp.path().join("input");
    std::fs::write(&file, vec![7; 256 * 1024]).unwrap();
    let roots = [("probe", file.as_path())];
    let run = |stopped: &dyn Fn() -> bool| {
        walk_roots(
            TEST.walk,
            &roots,
            Some(Reads {
                open: &mut |p| std::fs::File::open(p),
                list: &mut list_names,
            }),
            stopped,
        )
    };
    let complete = run(&|| false).unwrap();
    assert!(complete.digest.is_some());
    assert!(walk_roots(TEST.walk, &roots, None, &|| true).is_err());
    let polls = std::cell::Cell::new(0);
    assert!(
        run(&|| {
            polls.set(polls.get() + 1);
            polls.get() == 4
        })
        .is_err()
    );
    assert_eq!(polls.get(), 4);
    // Stop machinery never changes the content identity of a completed take.
    let ordinary = walk_roots(
        TEST.walk,
        &roots,
        Some(Reads {
            open: &mut |p| std::fs::File::open(p),
            list: &mut list_names,
        }),
        &|| false,
    )
    .unwrap();
    assert_eq!(complete.digest, ordinary.digest);
    assert_eq!(complete.roots.len(), ordinary.roots.len());
    for (a, b) in complete.roots.iter().zip(&ordinary.roots) {
        assert_eq!((&a.stat, a.entries), (&b.stat, b.entries));
    }
}

#[test]
fn selected_rust_libraries_ignore_other_targets_but_bind_every_used_library() {
    let root = aterm_tempfile::tempdir().unwrap();
    let selected = ["aarch64-apple-darwin", "x86_64-apple-darwin"];
    let used = [
        "lib/libLLVM.dylib",
        "lib/rustlib/aarch64-apple-darwin/lib/std.rlib",
        "lib/rustlib/aarch64-apple-darwin/codegen-backends/backend.dylib",
        "lib/rustlib/x86_64-apple-darwin/lib/std.rlib",
    ];
    let write = |name: &str, bytes: &[u8]| {
        let path = root.path().join(name);
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(path, bytes).unwrap();
    };
    for name in used {
        write(name, b"original");
    }
    let digest = || {
        let roots = rust_library_roots("compiler", root.path(), &selected).unwrap();
        digest_roots(
            &roots
                .iter()
                .map(|(name, path)| (name.as_str(), path.as_path()))
                .collect::<Vec<_>>(),
        )
        .unwrap()
    };
    let original = digest();
    for name in [
        "lib/rustlib/aarch64-unknown-linux-gnu/lib/std.rlib",
        "lib/rustlib/components",
        "lib/rustlib/manifest-rust-std-aarch64-unknown-linux-gnu",
        "lib/rustlib/multirust-config.toml",
    ] {
        write(name, b"installed later");
        assert_eq!(digest(), original, "unrelated installation state: {name}");
    }
    for name in used {
        write(name, b"changed");
        assert_ne!(digest(), original, "selected input: {name}");
        write(name, b"original");
        assert_eq!(digest(), original);
    }
    assert_eq!(
        rust_library_roots("compiler", root.path(), &selected).unwrap(),
        rust_library_roots(
            "compiler",
            root.path(),
            &[selected[1], selected[0], selected[1]]
        )
        .unwrap()
    );
    for targets in [&[][..], &["missing"][..], &["../escape"][..]] {
        assert!(rust_library_roots("compiler", root.path(), targets).is_err());
    }
}

#[test]
fn compiler_host_requires_one_valid_target() {
    assert_eq!(
        host_from_text("rustc 1.0\nhost: aarch64-apple-darwin\n").unwrap(),
        "aarch64-apple-darwin"
    );
    for text in ["", "host: ../escape", "host: ", "host: a\nhost: b"] {
        assert!(host_from_text(text).is_err(), "{text:?}");
    }
}

#[test]
fn an_unreadable_input_never_yields_an_identity() {
    let root = aterm_tempfile::tempdir().unwrap();
    let file = root.path().join("compiler");
    std::fs::write(&file, "compiler").unwrap();
    assert!(digest_roots(&[("compiler", root.path())]).is_ok());
    let error = digest_roots_with(&[("compiler", root.path())], &mut |_| {
        Err(std::io::Error::new(
            std::io::ErrorKind::PermissionDenied,
            "denied",
        ))
    })
    .unwrap_err();
    assert!(error.contains("denied"), "{error}");
}

#[test]
fn compiler_library_standard_library_and_sdk_bytes_all_change_identity() {
    let root = aterm_tempfile::tempdir().unwrap();
    let files = [
        "bin/trustc",
        "lib/compiler.dylib",
        "lib/rustlib/target/lib/std.rlib",
        "sdk/usr/include/header.h",
    ];
    for name in files {
        let path = root.path().join(name);
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(path, b"original").unwrap();
    }
    let digest = || digest_roots(&[("inputs", root.path())]).unwrap();
    let original = digest();
    for name in files {
        std::fs::write(root.path().join(name), b"changed!").unwrap();
        assert_ne!(digest(), original, "{name}");
        std::fs::write(root.path().join(name), b"original").unwrap();
        assert_eq!(digest(), original);
    }
}

#[cfg(unix)]
#[test]
fn root_aliases_match_but_changed_link_targets_do_not() {
    use std::os::unix::fs::symlink;
    let root = aterm_tempfile::tempdir().unwrap();
    let payload = root.path().join("payload");
    std::fs::create_dir(&payload).unwrap();
    std::fs::write(payload.join("compiler"), b"one").unwrap();
    let alias = root.path().join("alias");
    symlink(&payload, &alias).unwrap();
    let original = digest_roots(&[("compiler", &payload)]).unwrap();
    assert_eq!(digest_roots(&[("compiler", &alias)]).unwrap(), original);
    let external = root.path().join("external");
    std::fs::write(&external, b"library").unwrap();
    symlink(&external, payload.join("lib")).unwrap();
    let with_link = digest_roots(&[("compiler", &payload)]).unwrap();
    std::fs::write(&external, b"replacement").unwrap();
    assert_ne!(digest_roots(&[("compiler", &payload)]).unwrap(), with_link);
}

/// A link back to a directory the walk is inside counts by its target text, once: the
/// walk ends, the link's presence and its spelling change the identity, and the content
/// it leads back to still does. CONTROL: the same tree without the link.
#[cfg(unix)]
#[test]
fn a_link_back_to_an_enclosing_directory_counts_by_its_target_text() {
    use std::os::unix::fs::symlink;
    let root = aterm_tempfile::tempdir().unwrap();
    let headers = root.path().join("Headers/ruby");
    std::fs::create_dir_all(&headers).unwrap();
    std::fs::write(headers.join("ruby.h"), b"one").unwrap();
    let digest = || digest_roots(&[("sdk", root.path())]).unwrap();
    let without = digest();
    symlink(".", headers.join("ruby")).unwrap();
    let with_dot = digest();
    assert_ne!(with_dot, without, "the link is part of the identity");
    assert_eq!(digest(), with_dot);
    std::fs::remove_file(headers.join("ruby")).unwrap();
    symlink("..", headers.join("ruby")).unwrap();
    assert_ne!(digest(), with_dot, "so is its target text");
    std::fs::write(headers.join("ruby.h"), b"two").unwrap();
    let changed = digest();
    std::fs::write(headers.join("ruby.h"), b"one").unwrap();
    assert_ne!(changed, digest(), "and the content it leads back to");
}

#[test]
fn missing_roots_and_ambiguous_labels_fail_closed() {
    let root = aterm_tempfile::tempdir().unwrap();
    assert!(digest_roots(&[("missing", &root.path().join("missing"))]).is_err());
    assert!(digest_roots(&[("same", root.path()), ("same", root.path())]).is_err());
    assert!(digest_roots(&[]).is_err());
}

/// A dangling link beneath a root counts by its target text: a new target text, or the
/// target appearing, changes the identity. CONTROL: a dangling root still refuses.
#[cfg(unix)]
#[test]
fn a_dangling_input_link_counts_by_its_target_text() {
    use std::os::unix::fs::symlink;
    let base = aterm_tempfile::tempdir().unwrap();
    let root = base.path().join("bin");
    std::fs::create_dir(&root).unwrap();
    std::fs::write(root.join("compiler"), b"compiler").unwrap();
    let digest = || digest_roots(&[("tools", &root)]).unwrap();
    let without = digest();
    let link = root.join("crashlog");
    symlink("../missing", &link).unwrap();
    let dangling = digest();
    assert_ne!(dangling, without, "the link is part of the identity");
    assert_eq!(digest(), dangling);
    std::fs::remove_file(&link).unwrap();
    symlink("../elsewhere", &link).unwrap();
    let retargeted = digest();
    assert_ne!(
        retargeted, dangling,
        "the target text is part of the identity"
    );
    // The target lies outside the root, so only the link can carry its arrival.
    std::fs::write(base.path().join("elsewhere"), b"script").unwrap();
    assert_ne!(
        digest(),
        retargeted,
        "a target that appears changes the identity"
    );
    let dangling_root = base.path().join("root");
    symlink(base.path().join("absent"), &dangling_root).unwrap();
    assert!(digest_roots(&[("tools", &dangling_root)]).is_err());
}

/// A link above the root leads the walk back into the root as a real directory, which
/// refuses, and a loop of links refuses as unresolvable. CONTROL: a link back to an
/// enclosing directory counts (`a_link_back_to_an_enclosing_directory_counts_…`).
#[cfg(unix)]
#[test]
fn a_walk_that_reenters_a_real_directory_or_loops_refuses() {
    use std::os::unix::fs::symlink;
    let base = aterm_tempfile::tempdir().unwrap();
    let root = base.path().join("sdk");
    std::fs::create_dir_all(root.join("lib")).unwrap();
    std::fs::write(root.join("lib/x.h"), b"x").unwrap();
    assert!(digest_roots(&[("sdk", &root)]).is_ok());
    symlink("../..", root.join("lib/up")).unwrap();
    let error = digest_roots(&[("sdk", &root)]).unwrap_err();
    assert!(error.contains("directory cycle"), "{error}");
    std::fs::remove_file(root.join("lib/up")).unwrap();
    symlink("b", root.join("a")).unwrap();
    symlink("a", root.join("b")).unwrap();
    let error = digest_roots(&[("sdk", &root)]).unwrap_err();
    assert!(error.contains("resolve build input"), "{error}");
}

/// What a call reported, as a [`Bench`] keeps it.
#[cfg(unix)]
#[derive(Clone, Debug, PartialEq)]
enum Said {
    /// A call's one [`Report::Hashed`], [`Report::Rechecked`] or [`Report::Adopted`]: its mode
    /// and its entries.
    Call(&'static str, u64),
    /// A note: [`Report::Changed`]'s reason, [`Report::Unsettled`], [`Report::NotKept`]'s reason.
    Note(String),
}

#[cfg(unix)]
/// A scratch tree of inputs, and one machine's view of it: what every process opens and says.
struct Bench {
    _dir: aterm_tempfile::TempDir,
    root: PathBuf,
    /// A caller's seal directory.
    kept: PathBuf,
    os_build: Option<String>,
    /// [`Facts::trust_build`].
    trust_build: Option<String>,
    /// Take a new seal on every call, as every call did before seals.
    fresh: bool,
    /// Whether the inputs were last changed long before each call (an hour, by its clock), as
    /// a release's toolchain was; otherwise the call's clock is the wall clock.
    settled: bool,
    opened: usize,
    said: Vec<Said>,
}

#[cfg(unix)]
impl Bench {
    fn new() -> Self {
        use std::os::unix::fs::PermissionsExt as _;

        let dir = aterm_tempfile::tempdir().unwrap();
        let root = dir.path().join("inputs");
        for (name, bytes) in [
            ("bin/trustc", "trustc"),
            ("lib/std.rlib", "std"),
            ("sdk/usr/include/stdio.h", "header"),
        ] {
            let path = root.join(name);
            std::fs::create_dir_all(path.parent().unwrap()).unwrap();
            std::fs::write(&path, bytes).unwrap();
            std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o644)).unwrap();
        }
        std::os::unix::fs::symlink("trustc", root.join("bin/rustc")).unwrap();
        Self {
            root,
            kept: dir.path().join("state/inputs-seal"),
            _dir: dir,
            os_build: Some("25B78".into()),
            trust_build: Some(format!("16387 {}", "4".repeat(64))),
            fresh: false,
            settled: true,
            opened: 0,
            said: Vec::new(),
        }
    }

    /// `process`'s identity now.
    fn ask(&mut self, process: &mut Process) -> Result<String, String> {
        let Self {
            root,
            os_build,
            trust_build,
            fresh,
            settled,
            opened,
            said,
            ..
        } = self;
        let mut roots = || Ok(vec![("inputs".to_string(), root.clone())]);
        let mut facts = |_: &[Root]| Facts {
            os_build: os_build.clone(),
            model: Some("Mac0,0".into()),
            sdk_settings: None,
            trust_build: trust_build.clone(),
        };
        let mut open = |path: &Path| {
            *opened += 1;
            crate::metadata_io::open_regular(path)
        };
        let hour = if *settled { 3_600_000_000_000 } else { 0 };
        let mut now = || now_ns() + hour;
        let mut report = |report: Report<'_>| {
            said.push(match report {
                Report::Hashed { entries, secs } => {
                    assert!(secs.is_finite() && secs >= 0.0, "{report:?}");
                    Said::Call("hashed", entries)
                }
                Report::Rechecked { entries, secs } => {
                    assert!(secs.is_finite() && secs >= 0.0, "{report:?}");
                    Said::Call("rechecked", entries)
                }
                Report::Adopted { entries, secs, .. } => {
                    assert!(secs.is_finite() && secs >= 0.0, "{report:?}");
                    Said::Call("adopted", entries)
                }
                Report::Changed(why) | Report::NotKept(why) => Said::Note(why.to_string()),
                Report::Unsettled => Said::Note("unsettled".into()),
            });
        };
        process.identity(
            &mut Sources {
                roots: &mut roots,
                facts: &mut facts,
                open: &mut open,
                now: &mut now,
                stopped: &|| false,
                report: &mut report,
            },
            *fresh,
        )
    }

    /// This machine's sources, for a question that hashes nothing, with `stopped` as the caller's
    /// interrupt; whatever it reports is kept as a note.
    fn quiet<T>(&mut self, stopped: bool, ask: impl FnOnce(&mut Sources<'_>) -> T) -> T {
        let Self {
            root,
            os_build,
            trust_build,
            opened,
            said,
            ..
        } = self;
        let mut roots = || Ok(vec![("inputs".to_string(), root.clone())]);
        let mut facts = |_: &[Root]| Facts {
            os_build: os_build.clone(),
            model: Some("Mac0,0".into()),
            sdk_settings: None,
            trust_build: trust_build.clone(),
        };
        let mut open = |path: &Path| {
            *opened += 1;
            crate::metadata_io::open_regular(path)
        };
        let mut now = now_ns;
        ask(&mut Sources {
            roots: &mut roots,
            facts: &mut facts,
            open: &mut open,
            now: &mut now,
            stopped: &|| stopped,
            report: &mut |report| said.push(Said::Note(format!("{report:?}"))),
        })
    }

    /// Each call so far, as its mode and its entries.
    fn events(&self) -> Vec<(String, u64)> {
        self.said
            .iter()
            .filter_map(|said| match said {
                Said::Call(mode, entries) => Some(((*mode).to_string(), *entries)),
                Said::Note(_) => None,
            })
            .collect()
    }

    /// How many calls so far hashed the inputs, and how many rechecked a seal by stat (a held
    /// one's recheck, or a kept one's adoption).
    fn counts(&self) -> (usize, usize) {
        let mut counts = (0, 0);
        for (mode, entries) in self.events() {
            assert!(entries > 0, "{mode}");
            match mode.as_str() {
                "hashed" => counts.0 += 1,
                _ => counts.1 += 1,
            }
        }
        counts
    }

    /// Whether some note so far contains `text`.
    fn noted(&self, text: &str) -> bool {
        self.said
            .iter()
            .any(|said| matches!(said, Said::Note(note) if note.contains(text)))
    }
}

#[cfg(unix)]
/// A process started as handed `handed`, keeping its seal in `armed`.
fn process(handed: Option<&Path>, armed: Option<&Path>) -> Process {
    let mut process = Process::unstarted(TEST);
    if let Some(dir) = armed {
        process.arm(dir.to_path_buf());
    }
    process.begin(handed.map(Path::to_path_buf), None);
    process
}

#[cfg(unix)]
fn facts(os_build: &str) -> Facts {
    Facts {
        os_build: Some(os_build.into()),
        model: Some("Mac0,0".into()),
        sdk_settings: None,
        trust_build: Some(format!("16387 {}", "4".repeat(64))),
    }
}

/// A seal's recheck stats every entry, opens no input and holds while nothing changed; the
/// identity is the content digest's on this machine. CONTROL: the take before it opened
/// every file, the one a link resolves to again.
#[cfg(unix)]
#[test]
fn a_seal_rechecked_by_stat_opens_no_input_and_holds() {
    let mut bench = Bench::new();
    let mut process = process(None, None);
    let taken = bench.ask(&mut process).unwrap();
    assert_eq!(bench.opened, 4, "three files and the link's file");
    assert_eq!(bench.counts(), (1, 0));
    for _ in 0..3 {
        assert_eq!(bench.ask(&mut process).unwrap(), taken);
    }
    assert_eq!(bench.opened, 4, "a recheck opens no input");
    assert_eq!(bench.counts(), (1, 3));
    let call = |mode: &str| (mode.to_string(), 10);
    assert_eq!(
        bench.events(),
        [
            call("hashed"),
            call("rechecked"),
            call("rechecked"),
            call("rechecked")
        ],
        "every call walks the 10 entries"
    );
    let digest = digest_roots(&[("inputs", &bench.root)]).unwrap();
    assert_eq!(taken, identity_under(TEST, &digest, &facts("25B78")));
}

/// A caller told to stop still learns whether its seal held: [`Process::holds`] walks every
/// entry by status whatever the caller's interrupt says, opens no input, reports nothing, and
/// answers for the held identity alone. NEGATIVE CONTROLS: the interruptible recheck the
/// ordinary call makes (`Seal::recheck_by_stat`, by the caller's interrupt) ends at its first
/// entry once the stop is set, as does `Seal::recheck_by_stat_until` told to stop under a caller
/// that is not (the walk that dropped an interrupted round's rows); another identity, a process
/// that holds no seal and a changed input are each not held.
#[cfg(unix)]
#[test]
fn a_held_seal_is_rechecked_to_its_end_after_an_interrupt() {
    let mut bench = Bench::new();
    let mut process = process(None, None);
    let taken = bench.ask(&mut process).unwrap();
    let opened = bench.opened;
    let said = bench.said.len();
    let seal = process.seal.clone().unwrap();

    let interrupted = Rechecked::Changed("build input walk interrupted".into());
    let ordinary = bench.quiet(true, |src| {
        let roots = (src.roots)().unwrap();
        seal.recheck_by_stat(&roots, src)
    });
    assert_eq!(
        ordinary, interrupted,
        "the ordinary recheck stops with the caller"
    );
    let until = bench.quiet(false, |src| {
        let roots = (src.roots)().unwrap();
        seal.recheck_by_stat_until(&roots, src, &|| true)
    });
    assert_eq!(
        until, interrupted,
        "the stop it is given is the one it asks"
    );
    assert!(
        bench.quiet(true, |src| process.holds(src, &taken)),
        "held after an interrupt"
    );
    assert!(bench.quiet(false, |src| process.holds(src, &taken)));
    assert_eq!(bench.opened, opened, "a status recheck opens no input");
    assert_eq!(
        bench.said.len(),
        said,
        "and reports nothing: {:?}",
        bench.said
    );

    assert!(!bench.quiet(true, |src| process.holds(src, &"0".repeat(64))));
    assert!(!bench.quiet(true, |src| Process::unstarted(TEST).holds(src, &taken)));
    std::fs::write(bench.root.join("lib/std.rlib"), "another std").unwrap();
    assert!(
        !bench.quiet(true, |src| process.holds(src, &taken)),
        "a changed input is not held"
    );
}

#[cfg(unix)]
/// What one change does to `process`'s seal: it hashes again exactly once, and the next call
/// holds. Its identity.
fn after_change(bench: &mut Bench, process: &mut Process, change: impl FnOnce(&Path)) -> String {
    let (hashed, _) = bench.counts();
    change(&bench.root);
    let identity = bench.ask(process).unwrap();
    assert_eq!(
        bench.counts().0,
        hashed + 1,
        "hashed again: {:?}",
        bench.said
    );
    assert_eq!(bench.ask(process).unwrap(), identity);
    assert_eq!(bench.counts().0, hashed + 1, "then holds");
    identity
}

#[cfg(unix)]
fn set_modified(path: &Path, when: std::time::SystemTime) {
    std::fs::File::options()
        .write(true)
        .open(path)
        .unwrap()
        .set_times(std::fs::FileTimes::new().set_modified(when))
        .unwrap();
}

/// A write of the same size whose modification time is put back hashes the inputs again,
/// once, into another identity: only the status-change time tells, and APFS stamps it in
/// nanoseconds. (A jiffy clock, Linux's before 6.13, can stamp that write with the time the
/// take saw; a take of inputs that changed that recently is never held, as
/// `a_take_of_inputs_that_just_changed_is_never_held` shows.) NEGATIVE CONTROL: leave
/// `ctime_ns` out of [`StatKey`]'s encoding and the write holds by stat over bytes no take
/// read.
#[cfg(target_os = "macos")]
#[test]
fn a_write_with_its_modification_time_put_back_hashes_again() {
    let mut bench = Bench::new();
    let mut process = process(None, None);
    let original = bench.ask(&mut process).unwrap();
    let written = after_change(&mut bench, &mut process, |root| {
        let trustc = root.join("bin/trustc");
        let modified = std::fs::metadata(&trustc).unwrap().modified().unwrap();
        std::fs::write(&trustc, "TRUSTC").unwrap();
        set_modified(&trustc, modified);
    });
    assert_ne!(written, original, "a write is another identity");
}

/// Each change a status shows hashes the inputs again, once: a touch, a chmod, a replacement
/// by identical bytes, a new entry, a retargeted link, and a rewrite of a file outside the
/// root that a link inside it resolves to. The identity follows the content: the same after a
/// touch or a replacement, another after a chmod, a new entry, a retarget or the rewrite.
/// NEGATIVE CONTROL: record a constant status for what a link resolves to and the rewrite
/// outside the root holds by stat over bytes no take read.
#[cfg(unix)]
#[test]
fn every_change_a_status_shows_hashes_the_inputs_again_once() {
    use std::os::unix::fs::PermissionsExt as _;
    let mut bench = Bench::new();
    let mut process = process(None, None);
    let original = bench.ask(&mut process).unwrap();
    let touched = after_change(&mut bench, &mut process, |root| {
        set_modified(
            &root.join("lib/std.rlib"),
            std::time::UNIX_EPOCH + std::time::Duration::from_secs(1_000_000_000),
        );
    });
    assert_eq!(touched, original, "a touch changes no content");
    let chmodded = after_change(&mut bench, &mut process, |root| {
        std::fs::set_permissions(
            root.join("lib/std.rlib"),
            std::fs::Permissions::from_mode(0o600),
        )
        .unwrap();
    });
    assert_ne!(chmodded, touched, "a mode is content");
    let replaced = after_change(&mut bench, &mut process, |root| {
        let staged = root.join("lib/.std.rlib.new");
        std::fs::write(&staged, "std").unwrap();
        std::fs::set_permissions(&staged, std::fs::Permissions::from_mode(0o600)).unwrap();
        std::fs::rename(&staged, root.join("lib/std.rlib")).unwrap();
    });
    assert_eq!(replaced, chmodded, "identical bytes in another inode");
    let added = after_change(&mut bench, &mut process, |root| {
        std::fs::write(root.join("lib/extra.rlib"), "extra").unwrap();
    });
    assert_ne!(added, replaced);
    let retargeted = after_change(&mut bench, &mut process, |root| {
        std::fs::remove_file(root.join("bin/rustc")).unwrap();
        std::os::unix::fs::symlink("../lib/std.rlib", root.join("bin/rustc")).unwrap();
    });
    assert_ne!(retargeted, added);
    // The Command Line Tools' usr/bin/python3 resolves into their Library: no root holds it,
    // so only the link's record of its resolved status can tell its rewrite.
    let outside = bench.root.parent().unwrap().join("outside/python3.9");
    let linked = after_change(&mut bench, &mut process, |root| {
        std::fs::create_dir_all(outside.parent().unwrap()).unwrap();
        std::fs::write(&outside, "python").unwrap();
        std::os::unix::fs::symlink(&outside, root.join("bin/python3")).unwrap();
    });
    assert_ne!(linked, retargeted);
    let rewritten = after_change(&mut bench, &mut process, |_| {
        std::fs::write(&outside, "python 3.9.6").unwrap();
    });
    assert_ne!(
        rewritten, linked,
        "the bytes a link resolves to are content"
    );
}

/// Another OS build is another identity, hashed again though no input's status moved: an
/// update mid-run is caught, not taken for the same inputs. CONTROL: the same build holds.
#[cfg(unix)]
#[test]
fn another_os_build_hashes_again_into_another_identity() {
    let mut bench = Bench::new();
    let mut process = process(None, None);
    let before = bench.ask(&mut process).unwrap();
    assert_eq!(bench.ask(&mut process).unwrap(), before);
    assert_eq!(bench.counts(), (1, 1));
    bench.os_build = Some("25C5".into());
    let opened = bench.opened;
    let after = bench.ask(&mut process).unwrap();
    assert_ne!(after, before);
    assert_eq!(bench.counts(), (2, 1));
    assert!(bench.opened > opened, "it read the inputs again");
}

/// Another Trust build is another identity, hashed again though no input's status moved: a
/// seal names the build it covers. CONTROL: the same build holds. NEGATIVE CONTROL: drop
/// [`Facts::trust_build`] from [`identity_under`], and both builds read one identity.
#[cfg(unix)]
#[test]
fn another_trust_build_hashes_again_into_another_identity() {
    let mut bench = Bench::new();
    let mut process = process(None, None);
    let before = bench.ask(&mut process).unwrap();
    assert_eq!(bench.ask(&mut process).unwrap(), before);
    assert_eq!(bench.counts(), (1, 1));
    bench.trust_build = Some(format!("16500 {}", "5".repeat(64)));
    let after = bench.ask(&mut process).unwrap();
    assert_ne!(after, before);
    assert_eq!(bench.counts(), (2, 1));
    assert!(bench.noted("Trust build (16387 "), "{:?}", bench.said);
}

/// The Trust build of a selected toolchain is the store build its `bin` is, with the tree
/// root recorded for that tree, `unrecorded` without one. NEGATIVE CONTROLS: a `bin` outside
/// the store, another program's build, and a directory below a build's `bin` name none.
#[cfg(target_os = "macos")]
#[test]
fn the_selected_toolchain_names_the_store_build_it_is() {
    let dir = aterm_tempfile::tempdir().unwrap();
    let prefix = dir.path().canonicalize().unwrap();
    let laid = prefix.join("store/trust/16387.noindex");
    std::fs::create_dir_all(laid.join("bin/deeper")).unwrap();
    let ay = prefix.join("store/ay/18/bin");
    std::fs::create_dir_all(&ay).unwrap();
    let elsewhere = prefix.join("checkout/bin");
    std::fs::create_dir_all(&elsewhere).unwrap();
    let bin = laid.join("bin");
    assert_eq!(
        selected_trust_build(&prefix, &bin).as_deref(),
        Some("16387 unrecorded")
    );
    let root = "4".repeat(64);
    crate::trust_lock::write_tree_root_record(&laid, &root).unwrap();
    assert_eq!(
        selected_trust_build(&prefix, &bin),
        Some(format!("16387 {root}"))
    );
    for none in [elsewhere, ay, bin.join("deeper")] {
        assert_eq!(
            selected_trust_build(&prefix, &none),
            None,
            "{}",
            none.display()
        );
    }
}

/// A machine whose OS build could not be read keeps no seal and adopts none: its process
/// holds its own, and another process or a later run hashes for itself, as `None` would match
/// the `None` of a run after an OS update. CONTROL: with the OS build read, the same calls keep
/// a seal that a second process adopts. NEGATIVE CONTROL: drop [`Facts::complete`] from
/// [`Process::keep`] and [`load`], and the second process adopts the first's.
#[cfg(unix)]
#[test]
fn a_seal_of_facts_unread_is_neither_kept_nor_adopted() {
    let mut bench = Bench::new();
    let state = bench.kept.clone();
    bench.os_build = None;
    let mut first = process(None, Some(&state));
    let identity = bench.ask(&mut first).unwrap();
    assert_eq!(bench.ask(&mut first).unwrap(), identity);
    assert_eq!(bench.counts(), (1, 1), "held in the process that took it");
    assert_eq!(first.saved, None, "and kept nowhere");
    let mut second = process(None, Some(&state));
    assert_eq!(bench.ask(&mut second).unwrap(), identity);
    assert_eq!(
        bench.counts(),
        (2, 1),
        "the second process hashes for itself"
    );
    let mut unread = first.seal.clone().unwrap();
    let path = state.join("unread.toml");
    save(&unread, &path).unwrap();
    assert_eq!(
        load(TEST, &path),
        None,
        "a kept seal of unread facts is no seal"
    );
    unread.facts = facts("25B78");
    unread.identity = identity_under(TEST, &unread.digest, &unread.facts);
    save(&unread, &path).unwrap();
    assert_eq!(load(TEST, &path).as_ref(), Some(&unread));

    bench.os_build = Some("25B78".into());
    let mut kept = process(None, Some(&state));
    bench.ask(&mut kept).unwrap();
    assert!(kept.saved.is_some(), "read facts keep their seal");
    let mut adopting = process(None, Some(&state));
    bench.ask(&mut adopting).unwrap();
    assert_eq!(
        bench.events().last().map(|(mode, _)| mode.as_str()),
        Some("adopted")
    );
}

/// A take of inputs whose newest status change is less than [`SETTLE_NS`] old is not held:
/// a write in the same tick of a coarse clock could leave that status as the take saw it. So
/// the next call hashes again, and nothing is kept for another process; once the inputs have
/// settled, a take holds and is kept. NEGATIVE CONTROL: drop the settle check from
/// [`Seal::hashed`] and the second call rechecks by stat.
#[cfg(unix)]
#[test]
fn a_take_of_inputs_that_just_changed_is_never_held() {
    let mut bench = Bench::new();
    let state = bench.kept.clone();
    bench.settled = false;
    let mut process = process(None, Some(&state));
    let identity = bench.ask(&mut process).unwrap();
    assert_eq!(
        (process.seal.is_none(), process.saved.as_ref()),
        (true, None)
    );
    assert_eq!(bench.ask(&mut process).unwrap(), identity);
    assert_eq!(bench.counts(), (2, 0), "hashed again");
    bench.settled = true;
    assert_eq!(bench.ask(&mut process).unwrap(), identity);
    assert_eq!(bench.ask(&mut process).unwrap(), identity);
    assert_eq!(bench.counts(), (3, 1), "settled, the take holds");
    assert!(process.saved.is_some(), "and is kept");
}

/// A file written between the status the walk took and its read refuses the take: the bytes
/// read are no version that status names. CONTROL: the same walk with no write succeeds.
/// NEGATIVE CONTROL: drop the descriptor's status check after the read and the take passes.
#[test]
fn an_input_written_while_it_is_hashed_refuses_the_take() {
    let root = aterm_tempfile::tempdir().unwrap();
    std::fs::write(root.path().join("compiler"), b"compiler").unwrap();
    let roots = [("inputs", root.path())];
    assert!(digest_roots(&roots).is_ok());
    let error = digest_roots_with(&roots, &mut |path: &Path| {
        std::fs::write(path, b"written while it was hashed").unwrap();
        crate::metadata_io::open_regular(path)
    })
    .expect_err("refused");
    assert!(error.contains("changed while it was hashed"), "{error}");
}

/// A directory replaced between the status the walk took and its listing refuses the take,
/// and on APFS so does an entry added there: the names listed are no version that status
/// names. CONTROL: the same walk with nothing changed succeeds. NEGATIVE CONTROL: drop the
/// directory's status check after its listing and both takes pass.
#[cfg(unix)]
#[test]
fn a_directory_changed_while_it_is_listed_refuses_the_take() {
    let root = aterm_tempfile::tempdir().unwrap();
    let lib = root.path().join("lib");
    std::fs::create_dir(&lib).unwrap();
    std::fs::write(lib.join("std.rlib"), b"std").unwrap();
    let roots = [("inputs", root.path())];
    let lib = lib.canonicalize().unwrap();
    let take = |change: &mut dyn FnMut(&Path)| {
        walk_roots(
            TEST.walk,
            &roots,
            Some(Reads {
                open: &mut crate::metadata_io::open_regular,
                list: &mut |dir: &Path| {
                    if dir == lib {
                        change(dir);
                    }
                    list_names(dir)
                },
            }),
            &|| false,
        )
        .map(|walked| walked.digest)
    };
    assert!(take(&mut |_| {}).is_ok());
    type Change = Box<dyn FnMut(&Path)>;
    let mut changes: Vec<Change> = vec![Box::new(|dir: &Path| {
        let old = dir.with_extension("old");
        std::fs::rename(dir, &old).unwrap();
        std::fs::create_dir(dir).unwrap();
        std::fs::write(dir.join("std.rlib"), b"std").unwrap();
        std::fs::remove_dir_all(&old).unwrap();
    })];
    if cfg!(target_os = "macos") {
        changes.push(Box::new(|dir: &Path| {
            std::fs::write(dir.join("added.rlib"), b"added").unwrap();
        }));
    }
    for mut change in changes {
        let error = take(&mut *change).expect_err("refused");
        assert!(error.contains("changed while it was hashed"), "{error}");
    }
}

/// A kept seal reads back as written, 0600 in a 0700 directory; a file of another seal
/// schema, of another content walk's schema, with an identity its digest and facts do not
/// give, readable by others, in a directory others can read, or not TOML is no seal.
/// CONTROL: the round trip, restored after each. NEGATIVE CONTROL: leave the walk schema
/// out of [`identity_under`] and another walk's seal is adopted.
#[cfg(unix)]
#[test]
fn a_kept_seal_round_trips_and_any_other_file_is_no_seal() {
    use std::os::unix::fs::PermissionsExt as _;
    let mode = |path: &Path| std::fs::metadata(path).unwrap().permissions().mode() & 0o777;
    let chmod = |path: &Path, mode: u32| {
        std::fs::set_permissions(path, std::fs::Permissions::from_mode(mode)).unwrap();
    };
    let mut bench = Bench::new();
    let dir = bench.kept.clone();
    let mut process = process(None, Some(&dir));
    bench.ask(&mut process).unwrap();
    let path = process.saved.clone().expect("kept");
    assert_eq!(path.parent(), Some(dir.as_path()));
    assert_eq!((mode(&path), mode(&dir)), (0o600, 0o700));
    let seal = process.seal.clone().unwrap();
    assert_eq!(load(TEST, &path).as_ref(), Some(&seal));
    let edits: [fn(&mut Seal); 4] = [
        |s| s.schema = "atpkg-input-seal-test-seal-v0".into(),
        |s| s.identity = "0".repeat(64),
        |s| s.digest = "0".repeat(64),
        // Kept by a process of another content walk: its digest is that walk's.
        |s| {
            let other = Schema {
                walk: b"atpkg-input-seal-test-walk-v0",
                ..TEST
            };
            s.identity = identity_under(other, &s.digest, &s.facts);
        },
    ];
    for edit in edits {
        let mut other = seal.clone();
        edit(&mut other);
        save(&other, &path).unwrap();
        assert_eq!(load(TEST, &path), None);
        save(&seal, &path).unwrap();
        assert_eq!(load(TEST, &path).as_ref(), Some(&seal));
    }
    chmod(&path, 0o644);
    assert_eq!(load(TEST, &path), None);
    chmod(&path, 0o600);
    chmod(&dir, 0o755);
    assert_eq!(load(TEST, &path), None);
    chmod(&dir, 0o700);
    assert_eq!(load(TEST, &path).as_ref(), Some(&seal));
    std::fs::write(&path, "not = [a seal").unwrap();
    assert_eq!(load(TEST, &path), None);
    assert_eq!(load(TEST, &dir.join("absent.toml")), None);
}

/// A pre-freeze's kept seal is located by its authenticated identity, handed to the real
/// cutter, rechecked without opening an input, and persisted under the real release state.
/// A changed input rejects that handed record and is hashed anew. NEGATIVE CONTROL: using
/// the handed seal without [`Seal::recheck_by_stat`] would retain the old identity.
#[cfg(unix)]
#[test]
fn a_prefrozen_seal_is_adopted_by_step_one_only_after_its_recheck() {
    let mut bench = Bench::new();
    let temp = aterm_tempfile::tempdir().unwrap();
    let prepared_dir = temp.path().join("prepared/inputs-seal");
    let real_dir = temp.path().join("real/inputs-seal");
    let roots = vec![("inputs".to_string(), bench.root.clone())];

    let mut preparation = process(None, Some(&prepared_dir));
    let identity = bench.ask(&mut preparation).unwrap();
    let handed = matching_kept(TEST, &prepared_dir, &identity, &roots).expect("prepared seal");
    assert_eq!(Some(handed.clone()), preparation.saved);
    assert!(matching_kept(TEST, &prepared_dir, &"0".repeat(64), &roots).is_none());

    bench.said.clear();
    let opened = bench.opened;
    let mut step_one = process(Some(&handed), Some(&real_dir));
    assert_eq!(bench.ask(&mut step_one).unwrap(), identity);
    assert_eq!(bench.opened, opened, "the adoption only stats its inputs");
    assert_eq!(bench.events(), [("adopted".to_string(), 10)]);
    assert_eq!(
        step_one.saved.as_deref().and_then(Path::parent),
        Some(real_dir.as_path()),
        "step 1 persists the rechecked record in the real state"
    );

    std::fs::write(
        bench.root.join("lib/std.rlib"),
        b"changed after preparation",
    )
    .unwrap();
    bench.said.clear();
    let changed_state = temp.path().join("changed/inputs-seal");
    let mut changed = process(Some(&handed), Some(&changed_state));
    let changed_identity = bench.ask(&mut changed).unwrap();
    assert_ne!(changed_identity, identity);
    assert_eq!(bench.events(), [("hashed".to_string(), 10)]);
}

#[cfg(unix)]
/// `n` calls of `process`, each `identity`.
fn asks(bench: &mut Bench, process: &mut Process, n: usize, identity: &str) {
    for _ in 0..n {
        assert_eq!(bench.ask(process).unwrap(), identity);
    }
}

#[cfg(unix)]
/// One release's calls for the identity, in the processes that make them (its callers,
/// counted per step): the launching cutter's step 1; the cutter it re-runs as, handed the
/// seal, at its step 1, the build step, verify's before and after, and each step's refresh
/// of its facts; and the children each is handed it: the build child, the pre-promote check
/// and the cut child. `mid_run` runs before the cut child starts. The identity.
fn one_release(bench: &mut Bench, state: &Path, mid_run: impl FnOnce(&Path)) -> String {
    let mut launcher = process(None, Some(state));
    let identity = bench.ask(&mut launcher).unwrap();
    let mut cutter = process(launcher.saved.as_deref(), Some(state));
    asks(bench, &mut cutter, 1, &identity);
    let handed = cutter.saved.clone();
    let mut build = process(handed.as_deref(), None);
    asks(bench, &mut build, 2, &identity);
    asks(bench, &mut cutter, 2, &identity);
    asks(bench, &mut cutter, 4, &identity);
    asks(bench, &mut cutter, 2, &identity);
    let mut check = process(handed.as_deref(), None);
    asks(bench, &mut check, 1, &identity);
    asks(bench, &mut cutter, 2, &identity);
    asks(bench, &mut cutter, 2, &identity);
    mid_run(&bench.root);
    let mut cut = process(handed.as_deref(), None);
    asks(bench, &mut cut, 3, &identity);
    asks(bench, &mut cutter, 2, &identity);
    asks(bench, &mut cutter, 2, &identity);
    identity
}

/// One release asks for the identity 24 times across five processes, and hashes its inputs
/// once; a second release on the same inputs hashes none. A status that moves mid-run costs
/// one hash more, by the process that saw it, whose seal the others then adopt. CONTROL:
/// with every call taking a new seal, as each did before seals, the release hashes 24 times.
#[cfg(unix)]
#[test]
fn one_release_hashes_its_inputs_once_and_a_warm_one_not_at_all() {
    let mut bench = Bench::new();
    let state = bench.kept.clone();
    let before = aterm_tempfile::tempdir().unwrap();
    bench.fresh = true;
    let identity = one_release(&mut bench, before.path(), |_| {});
    assert_eq!(bench.counts(), (24, 0), "before: every call hashed");
    bench.fresh = false;
    bench.said.clear();
    assert_eq!(one_release(&mut bench, &state, |_| {}), identity);
    assert_eq!(bench.counts(), (1, 23), "one hash per release");
    bench.said.clear();
    assert_eq!(one_release(&mut bench, &state, |_| {}), identity);
    assert_eq!(bench.counts(), (0, 24), "none from a kept seal");
    bench.said.clear();
    let touched = one_release(&mut bench, &state, |root| {
        std::fs::File::options()
            .write(true)
            .open(root.join("bin/trustc"))
            .unwrap()
            .set_times(std::fs::FileTimes::new().set_modified(std::time::UNIX_EPOCH))
            .unwrap();
    });
    assert_eq!(touched, identity, "a touch changes no content");
    assert_eq!(
        bench.counts(),
        (1, 23),
        "the cut child's hash, adopted by the cutter"
    );
}
