// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;

#[test]
fn the_bundle_is_read_off_the_executable_path() {
    assert_eq!(
        own_bundle(Path::new("/Applications/aterm.app/Contents/MacOS/aterm")),
        Some(PathBuf::from("/Applications/aterm.app"))
    );
    assert_eq!(
        own_bundle(Path::new("/x/aterm (dev).app/Contents/MacOS/aterm")),
        Some(PathBuf::from("/x/aterm (dev).app"))
    );
    for bare in [
        "/Users//x/target/debug/aterm",
        "/x/Contents/MacOS/aterm",
        "/x/a.app/Contents/aterm",
        "/x/a.app/MacOS/aterm",
        "/x/a.bundle/Contents/MacOS/aterm",
        "/x/a.app/Contents/MacOS/sub/aterm",
        "aterm",
        "/",
    ] {
        assert_eq!(own_bundle(Path::new(bare)), None, "{bare}");
    }
}

#[test]
fn only_a_path_inside_the_directory_is_under_it() {
    let dir = Path::new("/s/acaffeine-code.noindex");
    assert!(is_under(
        Path::new("/s/acaffeine-code.noindex/l/a.app/Contents/MacOS/aterm"),
        dir
    ));
    assert!(!is_under(dir, dir));
    assert!(!is_under(
        Path::new("/s/acaffeine-code.noindex2/l/aterm"),
        dir
    ));
    assert!(!is_under(Path::new("/Applications/aterm.app"), dir));
}

#[cfg(unix)]
#[test]
fn a_label_names_a_copy_only_as_one_plain_component() {
    assert!(plain_name("com.aterm.aterm.acaffeine"));
    assert!(plain_name("com.aterm.aterm.acaffeine.dev.0123abcd"));
    for bad in ["", ".", "..", "a/b", "/a", "a\0b", "x.partial"] {
        assert!(!plain_name(bad), "{bad:?}");
    }
}

#[cfg(unix)]
fn stand_in_bundle(root: &Path, id: &str) -> PathBuf {
    use std::os::unix::fs::PermissionsExt as _;
    let app = root.join("Fake (dev).app");
    let macos = app.join("Contents").join("MacOS");
    let res = app
        .join("Contents")
        .join("Resources")
        .join("deep")
        .join("er");
    std::fs::create_dir_all(&macos).expect("macos");
    std::fs::create_dir_all(&res).expect("resources");
    std::fs::write(
        app.join("Contents").join("Info.plist"),
        format!(
            "<?xml version=\"1.0\" encoding=\"UTF-8\"?>\n<plist version=\"1.0\">\n<dict>\n\
             \t<key>CFBundleIdentifier</key>\n\t<string>{id}</string>\n</dict>\n</plist>\n"
        ),
    )
    .expect("plist");
    let exe = macos.join("aterm");
    std::fs::write(&exe, "#!/bin/sh\nexit 0\n").expect("exe");
    std::fs::set_permissions(&exe, std::fs::Permissions::from_mode(0o755)).expect("chmod");
    let ro = res.join("ro.txt");
    std::fs::write(&ro, "read only").expect("ro");
    std::fs::set_permissions(&ro, std::fs::Permissions::from_mode(0o444)).expect("chmod");
    std::os::unix::fs::symlink("aterm", macos.join("aterm-alias")).expect("alias");
    std::os::unix::fs::symlink("../../MacOS/aterm", res.join("up")).expect("up");
    std::os::unix::fs::symlink("/nonexistent/abs", res.join("abs")).expect("abs");
    app
}

#[cfg(unix)]
fn assert_same_tree(a: &Path, b: &Path) {
    use std::os::unix::fs::PermissionsExt as _;
    let ma = std::fs::symlink_metadata(a).expect("a");
    let mb = std::fs::symlink_metadata(b).expect("b");
    assert_eq!(ma.file_type(), mb.file_type(), "{}", b.display());
    if ma.file_type().is_symlink() {
        assert_eq!(
            std::fs::read_link(a).expect("a"),
            std::fs::read_link(b).expect("b")
        );
        return;
    }
    assert_eq!(
        ma.permissions().mode() & 0o7777,
        mb.permissions().mode() & 0o7777,
        "{}",
        b.display()
    );
    if ma.is_file() {
        assert_eq!(std::fs::read(a).expect("a"), std::fs::read(b).expect("b"));
        return;
    }
    let names = |dir: &Path| {
        let mut names: Vec<_> = std::fs::read_dir(dir)
            .expect("list")
            .map(|e| e.expect("entry").file_name())
            .collect();
        names.sort();
        names
    };
    let (na, nb) = (names(a), names(b));
    assert_eq!(na, nb, "{}", b.display());
    for name in na {
        assert_same_tree(&a.join(&name), &b.join(&name));
    }
}

#[cfg(unix)]
fn home_of(code: &Path, copy: &Path) -> PathBuf {
    let rest = copy.strip_prefix(code).expect("under the code directory");
    let mut parts = rest.components();
    let (label, id) = (parts.next().expect("label"), parts.next().expect("id"));
    assert_eq!(
        parts.as_path(),
        Path::new("Fake (dev).app/Contents/MacOS/aterm")
    );
    code.join(label).join(id)
}

#[cfg(unix)]
#[test]
fn a_bundle_is_copied_exactly_and_replaces_every_other_entry() {
    use std::os::unix::fs::PermissionsExt as _;
    let root = crate::test_scratch_dir("code");
    let app = stand_in_bundle(root.path(), "com.example.fake");
    let origin = app.join("Contents").join("MacOS").join("aterm");
    let code = root.path().join("state").join("acaffeine-code.noindex");
    std::fs::create_dir_all(&code).expect("code");
    std::fs::set_permissions(&code, std::fs::Permissions::from_mode(0o755)).expect("chmod");
    let target = root.path().join("kept");
    std::fs::create_dir_all(&target).expect("target");
    for dir in ["lbl/1-1", "lbl/1-2.partial", "lbl.partial", "com.other/2-1"] {
        std::fs::create_dir_all(code.join(dir).join("Old.app")).expect("old");
    }
    for ran in ["lbl/1-1", "com.other/2-1"] {
        std::fs::write(code.join(ran).join(RAN), "").expect("ran");
    }
    std::fs::write(code.join(".DS_Store"), "x").expect("stray");
    std::os::unix::fs::symlink(&target, code.join("link")).expect("link");

    let copy = prepare_copy(&origin, &code, "lbl")
        .expect("copy")
        .expect("a bundle");
    let home = home_of(&code, &copy);
    assert_same_tree(&app, &home.join("Fake (dev).app"));
    let entries: Vec<_> = std::fs::read_dir(&code)
        .expect("list")
        .map(|e| e.expect("entry").file_name())
        .collect();
    assert_eq!(entries, [std::ffi::OsString::from("lbl")]);
    let copies: Vec<_> = std::fs::read_dir(code.join("lbl"))
        .expect("list")
        .map(|e| e.expect("entry").path())
        .collect();
    assert_eq!(copies, std::slice::from_ref(&home), "the new copy alone");
    assert!(target.is_dir(), "a link's target is not swept");
    let mode = std::fs::metadata(&code).expect("meta").permissions().mode();
    assert_eq!(mode & 0o777, 0o700);
    #[cfg(target_os = "macos")]
    {
        let code_file = std::fs::File::open(&code).expect("open code");
        assert!(
            aterm_uds::backup::Backups::SYSTEM.excluded(&code_file),
            "{}",
            code.display()
        );
    }

    std::fs::write(app.join("Contents").join("new.txt"), "new").expect("new");
    let again = prepare_copy(&origin, &code, "lbl")
        .expect("copy")
        .expect("a bundle");
    let next = home_of(&code, &again);
    assert_ne!(next, home, "an id of its own");
    assert!(
        home.join("Fake (dev).app").is_dir(),
        "the unproven copy is kept"
    );
    assert_same_tree(&app, &next.join("Fake (dev).app"));
    std::fs::write(home.join(RAN), "").expect("ran");
    let third = prepare_copy(&origin, &code, "lbl")
        .expect("copy")
        .expect("a bundle");
    assert!(!home.exists(), "it ran: replaced");
    assert!(next.is_dir(), "the second copy, unproven, stays");
    assert_eq!(
        sweep(
            &code,
            std::time::SystemTime::now() + aterm_uds::deadline::FIRST_EXEC_HANG,
            None,
        )
        .expect("sweep"),
        Kept::default(),
        "none kept"
    );
    assert!(!next.exists(), "past the first-exec hang: removed");
    assert!(
        !home_of(&code, &third).exists(),
        "and so is the third, by then"
    );
}

/// A start again of an unchanged bundle (launchd's retry, a window's kickstart) runs the
/// copy a mug already ran from, copying nothing; one no mug ran from yet is not taken; a
/// changed bundle is copied anew, and the copy that ran is swept.
#[cfg(unix)]
#[test]
fn a_start_again_reuses_the_copy_that_ran() {
    let root = crate::test_scratch_dir("code");
    let app = stand_in_bundle(root.path(), "com.example.fake");
    let origin = app.join("Contents").join("MacOS").join("aterm");
    let code = root.path().join("state").join("acaffeine-code.noindex");
    let copies = || {
        let mut copies: Vec<_> = std::fs::read_dir(code.join("lbl"))
            .expect("list")
            .map(|e| e.expect("entry").path())
            .collect();
        copies.sort();
        copies
    };
    let first = prepare_copy(&origin, &code, "lbl")
        .expect("copy")
        .expect("a bundle");
    let home = home_of(&code, &first);
    let unran = prepare_copy(&origin, &code, "lbl")
        .expect("copy")
        .expect("a bundle");
    assert_ne!(unran, first, "no mug ran from the first: not taken");
    std::fs::write(home.join(RAN), "").expect("ran");
    let before = copies();
    let again = prepare_copy(&origin, &code, "lbl")
        .expect("copy")
        .expect("a bundle");
    assert_eq!(again, first, "the copy that ran is run again");
    assert_eq!(copies(), before, "and nothing is copied");
    assert_same_tree(&app, &home.join("Fake (dev).app"));

    std::fs::write(&origin, "#!/bin/sh\nexit 10\n").expect("a rebuild");
    let rebuilt = prepare_copy(&origin, &code, "lbl")
        .expect("copy")
        .expect("a bundle");
    assert_ne!(rebuilt, first, "a changed bundle is copied anew");
    assert!(!home.exists(), "and the copy that ran is swept");
    assert_same_tree(&app, &home_of(&code, &rebuilt).join("Fake (dev).app"));
}

#[cfg(unix)]
#[test]
fn a_bundle_with_a_fifo_is_refused_and_leaves_no_partial_copy() {
    use std::os::unix::ffi::OsStrExt as _;
    let root = crate::test_scratch_dir("code");
    let app = stand_in_bundle(root.path(), "com.example.fake");
    let fifo = app.join("Contents").join("pipe");
    let name = std::ffi::CString::new(fifo.as_os_str().as_bytes()).expect("no NUL");
    // SAFETY: a NUL-terminated path the call reads and does not retain.
    assert_eq!(unsafe { libc::mkfifo(name.as_ptr(), 0o600) }, 0, "mkfifo");
    let code = root.path().join("code");
    let origin = app.join("Contents").join("MacOS").join("aterm");
    let err = prepare_copy(&origin, &code, "lbl").expect_err("a fifo");
    assert_eq!(err.kind(), io::ErrorKind::InvalidInput, "{err}");
    let entries: Vec<_> = std::fs::read_dir(&code).expect("list").collect();
    assert!(entries.is_empty(), "{entries:?}");
}

#[cfg(unix)]
#[test]
fn a_link_at_the_code_directory_is_refused_before_anything_is_swept() {
    let root = crate::test_scratch_dir("code");
    let app = stand_in_bundle(root.path(), "com.example.fake");
    let origin = app.join("Contents").join("MacOS").join("aterm");
    let elsewhere = root.path().join("elsewhere");
    std::fs::create_dir_all(elsewhere.join("keep")).expect("elsewhere");
    std::fs::write(elsewhere.join("file"), "kept").expect("file");
    let code = root.path().join("code");
    std::os::unix::fs::symlink(&elsewhere, &code).expect("link");
    let err = prepare_copy(&origin, &code, "lbl").expect_err("a link");
    assert_eq!(err.kind(), io::ErrorKind::InvalidInput, "{err}");
    let mut names: Vec<_> = std::fs::read_dir(&elsewhere)
        .expect("list")
        .map(|e| e.expect("entry").file_name())
        .collect();
    names.sort();
    assert_eq!(
        names,
        [std::ffi::OsString::from("file"), "keep".into()],
        "nothing swept"
    );
    let file = root.path().join("code-file");
    std::fs::write(&file, "x").expect("file");
    assert!(prepare_copy(&origin, &file, "lbl").is_err());
    assert_eq!(std::fs::read_to_string(&file).expect("read"), "x");
}

#[cfg(unix)]
#[test]
fn the_probe_reads_the_verb_from_the_exit_status() {
    let hang = std::time::Duration::from_secs(60);
    let args = ["help", "acaffeine"];
    assert_eq!(
        answers_within(Path::new("/usr/bin/true"), args, hang),
        Some(true)
    );
    assert_eq!(
        answers_within(Path::new("/usr/bin/false"), args, hang),
        Some(false)
    );
    let root = crate::test_scratch_dir("code");
    assert_eq!(
        answers_within(&root.path().join("missing"), args, hang),
        None
    );
}

#[cfg(unix)]
#[test]
fn discarding_removes_the_copies_and_never_follows_a_link() {
    let root = crate::test_scratch_dir("code");
    let app = stand_in_bundle(root.path(), "com.example.fake");
    let origin = app.join("Contents").join("MacOS").join("aterm");
    let code = root.path().join("code");
    let copy = prepare_copy(&origin, &code, "lbl")
        .expect("copy")
        .expect("a bundle");
    let home = home_of(&code, &copy);
    assert_eq!(
        discard_copies(&code).expect("an unproven copy is kept, not an error"),
        Kept(vec![home.clone()]),
        "the kept copy is returned to say"
    );
    assert!(home.join("Fake (dev).app").is_dir(), "kept while unproven");
    std::fs::write(home.join(RAN), "").expect("ran");
    assert_eq!(discard_copies(&code).expect("discard"), Kept::default());
    assert!(!code.exists(), "the copy and its directory are gone");
    assert_eq!(
        discard_copies(&code).expect("nothing to discard"),
        Kept::default()
    );
    let elsewhere = root.path().join("elsewhere");
    std::fs::create_dir_all(elsewhere.join("keep")).expect("elsewhere");
    std::os::unix::fs::symlink(&elsewhere, &code).expect("link");
    let err = discard_copies(&code).expect_err("a link");
    assert_eq!(err.kind(), io::ErrorKind::InvalidInput, "{err}");
    assert!(
        elsewhere.join("keep").is_dir(),
        "nothing removed through it"
    );
}

#[test]
fn a_bare_executable_runs_where_it_is() {
    let root = crate::test_scratch_dir("code");
    let exe = root.path().join("target").join("debug").join("aterm");
    let code = root.path().join("code");
    assert!(matches!(prepare_copy(&exe, &code, "lbl"), Ok(None)));
    assert!(!code.exists(), "nothing is made for a bare executable");
}

#[cfg(unix)]
#[test]
fn a_bare_executables_start_sweeps_what_an_earlier_mug_left() {
    let root = crate::test_scratch_dir("code");
    let app = stand_in_bundle(root.path(), "com.example.fake");
    let origin = app.join("Contents").join("MacOS").join("aterm");
    let code = root.path().join("code");
    let ran = home_of(
        &code,
        &prepare_copy(&origin, &code, "other.build")
            .expect("copy")
            .expect("a bundle"),
    );
    let young = home_of(
        &code,
        &prepare_copy(&origin, &code, "lbl")
            .expect("copy")
            .expect("a bundle"),
    );
    aterm_uds::deadline::stamp_exec(&young);
    assert!(ran.is_dir(), "young when the second copy was made: kept");
    std::fs::write(ran.join(RAN), "").expect("ran");
    let exe = root.path().join("target").join("debug").join("aterm");
    assert!(matches!(prepare_copy(&exe, &code, "lbl"), Ok(None)));
    assert!(!ran.exists(), "a copy that ran goes");
    assert!(
        young.join("Fake (dev).app").is_dir(),
        "an unproven copy stays"
    );
    std::fs::write(young.join(RAN), "").expect("ran");
    assert!(matches!(prepare_copy(&exe, &code, "lbl"), Ok(None)));
    assert!(
        !code.exists(),
        "once it ran, it goes, and the directory with it"
    );
}

#[cfg(unix)]
#[test]
fn a_label_that_is_not_a_plain_name_is_refused() {
    let root = crate::test_scratch_dir("code");
    let app = stand_in_bundle(root.path(), "com.example.fake");
    let origin = app.join("Contents").join("MacOS").join("aterm");
    for bad in ["", "..", "a/b"] {
        assert!(
            prepare_copy(&origin, &root.path().join("code"), bad).is_err(),
            "{bad:?}"
        );
    }
}

#[cfg(unix)]
#[test]
fn the_bundle_identifier_is_read_off_its_info_plist() {
    let root = crate::test_scratch_dir("code");
    let app = stand_in_bundle(root.path(), "com.example.a&amp;b");
    assert_eq!(bundle_identifier(&app).as_deref(), Some("com.example.a&b"));
    assert_eq!(bundle_identifier(&root.path().join("Missing.app")), None);
}

#[test]
fn a_plist_string_is_the_keys_string_value_only() {
    let plist = |body: &str| {
        format!(
            "<?xml version=\"1.0\"?>\n<plist version=\"1.0\">\n<dict>\n{body}</dict>\n</plist>\n"
        )
    };
    let id = |body: &str| plist_string(&plist(body), "CFBundleIdentifier");
    assert_eq!(
        id("\t<key>CFBundleIdentifier</key>\n\t<string>com.aterm.aterm</string>\n").as_deref(),
        Some("com.aterm.aterm")
    );
    assert_eq!(
        id("<key>CFBundleIdentifier</key><string>a&lt;b&gt;&quot;c&apos;&amp;lt;</string>")
            .as_deref(),
        Some("a<b>\"c'&lt;")
    );
    assert_eq!(
        id("<key>CFBundleName</key><string>aterm</string>\
            <key>CFBundleIdentifier</key><string>x.y</string>")
        .as_deref(),
        Some("x.y")
    );
    for bad in [
        "",
        "<key>CFBundleName</key><string>aterm</string>",
        "<key>CFBundleIdentifier</key><integer>7</integer>",
        "<key>CFBundleIdentifier</key><string></string>",
        "<key>CFBundleIdentifier</key><string>unterminated",
        "<key>CFBundleIdentifier</key><array><string>x</string></array>",
        "<key>CFBundleIdentifier</key><string>a<b/>c</string>",
    ] {
        assert_eq!(id(bad), None, "{bad:?}");
    }
}

#[test]
fn a_plist_array_is_its_strings_only() {
    let job = crate::launchd::plist_text(
        "l",
        Path::new("/A&B <x>/aterm"),
        Path::new("/l.log"),
        Some("com.aterm.aterm"),
    );
    assert_eq!(
        plist_strings(&job, "ProgramArguments"),
        Some(vec![
            "/A&B <x>/aterm".to_string(),
            "acaffeine".to_string(),
            "serve".to_string()
        ])
    );
    assert_eq!(
        plist_strings("<key>A</key> <array>\n</array>", "A"),
        Some(Vec::new())
    );
    for bad in [
        "<key>A</key><string>x</string>",
        "<key>A</key><array><string>x</string><integer>1</integer></array>",
        "<key>A</key><array><string>x</string>",
        "<key>A</key><array><string>x<y/></string></array>",
        "<key>B</key><array><string>x</string></array>",
        "<!-- unclosed <key>A</key><array><string>x</string></array>",
        "<!-- bad -- comment --><key>A</key><array><string>x</string></array>",
        "<key<!-- split -->>A</key><array><string>x</string></array>",
    ] {
        assert_eq!(plist_strings(bad, "A"), None, "{bad:?}");
    }
    assert_eq!(
        plist_string(
            "<!-- <key>CFBundleIdentifier</key><string>spoof</string> -->\
             <key>CFBundleIdentifier</key><!-- note --><string>com.aterm.real</string>",
            "CFBundleIdentifier",
        )
        .as_deref(),
        Some("com.aterm.real"),
        "XML comments must not shadow real plist keys"
    );
    assert_eq!(
        plist_strings(
            "<!-- <key>A</key><array><string>spoof</string></array> -->\
             <key>A</key><array><!-- first --><string>one</string><!-- second --><string>two</string></array>",
            "A",
        ),
        Some(vec!["one".to_string(), "two".to_string()]),
        "XML comments inside arrays must be skipped cleanly"
    );
}

#[cfg(unix)]
#[test]
fn the_fingerprint_follows_the_info_plist_or_the_bare_executable() {
    let root = crate::test_scratch_dir("code");
    let app = stand_in_bundle(root.path(), "com.example.fake");
    let origin = app.join("Contents").join("MacOS").join("aterm");
    let before = code_fingerprint(&origin).expect("print");
    assert_eq!(code_fingerprint(&origin), Some(before.clone()), "stable");
    std::fs::write(app.join("Contents").join("other"), "x").expect("other");
    std::fs::write(&origin, "#!/bin/sh\nexit 1\n").expect("exe");
    assert_eq!(code_fingerprint(&origin), Some(before.clone()));
    let plist = app.join("Contents").join("Info.plist");
    let mut text = std::fs::read_to_string(&plist).expect("plist");
    text.push_str("<!-- 1.1 -->\n");
    std::fs::write(&plist, text).expect("plist");
    let after = code_fingerprint(&origin).expect("print");
    assert_ne!(after, before);
    std::fs::write(
        &plist,
        vec![b'x'; usize::try_from(PLIST_CAP).expect("cap") + 1],
    )
    .expect("big");
    assert_eq!(code_fingerprint(&origin), None, "over the cap");
    std::fs::remove_file(&plist).expect("rm");
    assert_eq!(code_fingerprint(&origin), None, "mid-swap: no reading");

    let bare = root.path().join("aterm");
    assert_eq!(code_fingerprint(&bare), None);
    std::fs::write(&bare, "one").expect("bare");
    let one = code_fingerprint(&bare).expect("print");
    assert_eq!(one.len(), 8 + 16);
    std::fs::write(&bare, "three").expect("bare");
    let three = code_fingerprint(&bare).expect("print");
    assert_ne!(three, one, "the length changed");
    let later = std::fs::metadata(&bare)
        .and_then(|m| m.modified())
        .expect("mtime")
        + std::time::Duration::from_secs(5);
    std::fs::File::options()
        .write(true)
        .open(&bare)
        .and_then(|f| f.set_modified(later))
        .expect("set mtime");
    assert_ne!(
        code_fingerprint(&bare).expect("print"),
        three,
        "the time changed"
    );
    std::fs::remove_file(&bare).expect("rm");
    std::fs::create_dir(&bare).expect("mkdir");
    assert_eq!(code_fingerprint(&bare), None);
}

#[test]
fn a_process_outside_the_code_directory_does_not_run_from_a_copy() {
    let root = crate::test_scratch_dir("code");
    assert!(!runs_from_copy(root.path()));
    assert!(!runs_from_copy(&root.path().join("missing")));
    assert_eq!(origin_exe(root.path()), own_exe());
}

#[cfg(target_os = "macos")]
mod handover {
    use std::path::{Path, PathBuf};
    use std::time::Instant;

    use crate::code;
    use crate::instance::{self, Instance};

    const DIR_ENV: &str = "ATERM_CAFFEINE_TEST_HANDOVER_DIR";

    #[test]
    fn handover_child() {
        let Some(dir) = std::env::var_os(DIR_ENV).map(PathBuf::from) else {
            return;
        };
        let lock = dir.join("acaffeine.lock");
        let code_dir = dir.join("code");
        if code::runs_from_copy(&code_dir) {
            code::mark_ran(&code_dir);
            let origin = code::origin_exe(&code_dir);
            let adopted = Instance::adopt(&lock);
            let stdin_null = {
                use std::os::fd::AsFd as _;
                use std::os::unix::fs::MetadataExt as _;
                let stdin = std::io::stdin();
                let held = std::fs::File::from(stdin.as_fd().try_clone_to_owned().expect("stdin"));
                let null = std::fs::metadata("/dev/null").expect("null");
                held.metadata().expect("stdin").rdev() == null.rdev()
            };
            let report = format!(
                "pid={}\nadopted={}\norigin={}\nvariable={}\nstdin_null={stdin_null}\nholder={}\n",
                std::process::id(),
                adopted.is_some(),
                origin.map_or_else(|| "-".to_string(), |p| p.display().to_string()),
                std::env::var_os(instance::LOCK_FD_ENV).is_some(),
                instance::running_pid(&lock).map_or_else(|| "-".to_string(), |p| p.to_string()),
            );
            std::fs::write(dir.join("report"), report).expect("report");
            drop(adopted);
        } else {
            let me = std::env::current_exe()
                .and_then(std::fs::canonicalize)
                .expect("exe");
            let held = Instance::try_hold(&lock).expect("lock").expect("free");
            std::fs::write(dir.join("first"), std::process::id().to_string()).expect("first");
            let copy = code::prepare_copy(&me, &code_dir, "lbl")
                .expect("copy")
                .expect("a bundle");
            let plain = std::fs::read_to_string(dir.join("mode")).is_ok_and(|m| m == "plain");
            let failed = if plain {
                code::reexec(&copy, &me)
            } else {
                code::reexec_holding(&copy, &me, &held)
            };
            panic!("the re-exec into {} failed: {failed}", copy.display());
        }
    }

    fn stand_in(root: &Path) -> PathBuf {
        let macos = root.join("T.app").join("Contents").join("MacOS");
        std::fs::create_dir_all(&macos).expect("bundle");
        std::fs::write(
            root.join("T.app").join("Contents").join("Info.plist"),
            "<?xml version=\"1.0\" encoding=\"UTF-8\"?>\n<plist version=\"1.0\"><dict>\
             <key>CFBundleIdentifier</key><string>com.aterm.acaffeine.handovertest</string>\
             </dict></plist>\n",
        )
        .expect("plist");
        let exe = macos.join("handover");
        std::fs::copy(std::env::current_exe().expect("exe"), &exe).expect("copy exe");
        exe
    }

    fn run_child(mode: &str) -> (String, String, PathBuf, aterm_uds::deadline::KeptUntilRan) {
        let mut root =
            aterm_uds::deadline::KeptUntilRan::new(crate::test_scratch_dir("code").keep());
        let exe = stand_in(root.path());
        let work = root.path().join("work");
        std::fs::create_dir_all(&work).expect("work");
        std::fs::write(work.join("mode"), mode).expect("mode");
        let out = aterm_uds::deadline::output_until(
            aterm_uds::spawnfd::command(&exe)
                .args([
                    "code::tests::handover::handover_child",
                    "--exact",
                    "--test-threads=1",
                ])
                .env(DIR_ENV, &work),
            Instant::now() + 2 * aterm_uds::deadline::FIRST_EXEC_HANG,
        );
        let first = std::fs::read_to_string(work.join("first"));
        let report = std::fs::read_to_string(work.join("report"));
        if first.is_ok() && report.is_ok() {
            root.ran();
        }
        let out = out.unwrap_or_else(|e| {
            panic!(
                "the child from {}: {e}; its copies stay under {} unless both ran",
                exe.display(),
                root.path().display()
            )
        });
        assert!(
            out.status.success(),
            "{}\n{}",
            String::from_utf8_lossy(&out.stdout),
            String::from_utf8_lossy(&out.stderr)
        );
        (
            first.expect("first"),
            report.expect("the copy ran"),
            std::fs::canonicalize(&exe).expect("origin"),
            root,
        )
    }

    #[test]
    fn the_lock_is_handed_over_across_the_reexec_into_the_copy() {
        let (first, report, origin, root) = run_child("handover");
        assert_eq!(
            report,
            format!(
                "pid={first}\nadopted=true\norigin={}\nvariable=false\nstdin_null=true\nholder={first}\n",
                origin.display()
            )
        );
        let work = root.path().join("work");
        let copies: Vec<_> = std::fs::read_dir(work.join("code").join("lbl"))
            .expect("the label's copies")
            .map(|e| e.expect("entry").path())
            .collect();
        let [home] = copies.as_slice() else {
            panic!("one copy: {copies:?}");
        };
        assert!(
            home.join("T.app")
                .join("Contents")
                .join("MacOS")
                .join("handover")
                .is_file()
        );
        assert!(home.join(code::RAN).is_file(), "the copy said it ran");
        assert_eq!(instance::running_pid(&work.join("acaffeine.lock")), None);
        drop(root);
    }

    #[test]
    fn a_plain_reexec_carries_no_lock() {
        let (first, report, origin, root) = run_child("plain");
        assert_eq!(
            report,
            format!(
                "pid={first}\nadopted=false\norigin={}\nvariable=false\nstdin_null=true\nholder=-\n",
                origin.display()
            )
        );
        drop(root);
    }
}
