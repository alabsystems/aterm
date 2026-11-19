// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Tests for [`parse`], the `#!` line as XNU reads it, and on Darwin for [`interpret`] and the
//! spawns through it, judged against the kernel's own interpretation: `std::process::Command`
//! hands the kernel every `#!` file it runs.

use super::*;

/// `bytes` as the kernel's header holds them: the first [`HEADER`], NUL past their end.
fn head(bytes: &[u8]) -> [u8; HEADER] {
    let mut head = [0; HEADER];
    let n = bytes.len().min(HEADER);
    head[..n].copy_from_slice(&bytes[..n]);
    head
}

type Parsed = Result<Vec<Vec<u8>>, &'static str>;

const NOT_SCRIPT: &str = "no #! script";
const ENOEXEC: &str = "ENOEXEC";
const KERNEL: &str = "the kernel's to split";

fn parsed(bytes: &[u8]) -> Parsed {
    match parse(&head(bytes)) {
        Line::NotScript => Err(NOT_SCRIPT),
        Line::Malformed => Err(ENOEXEC),
        Line::Unsure => Err(KERNEL),
        Line::Words(words) => Ok(words.into_iter().map(<[u8]>::to_vec).collect()),
    }
}

fn words(words: &[&str]) -> Parsed {
    Ok(words.iter().map(|word| word.as_bytes().to_vec()).collect())
}

/// **THE `#!` LINE READS AS XNU READS IT** (measured on macOS 26.6 by `posix_spawn`): blanks
/// are `' '` and `'\t'` alone, the line ends at `\n` or `#`, the rest splits into one word per
/// run of non-blanks, and a line with no interpreter or no end within 512 bytes is `ENOEXEC`.
#[test]
fn the_line_reads_as_xnu_reads_it() {
    let rows: &[(&str, &[u8], Parsed)] = &[
        ("one interpreter", b"#!/bin/sh\n", words(&["/bin/sh"])),
        (
            "each run of blanks splits one word from the next",
            b"#!/bin/echo  a   b\tc  \n",
            words(&["/bin/echo", "a", "b", "c"]),
        ),
        (
            "blanks after #! are skipped",
            b"#! \t /bin/sh -e\n",
            words(&["/bin/sh", "-e"]),
        ),
        (
            "# ends the line",
            b"#!/bin/echo first#second\n",
            words(&["/bin/echo", "first"]),
        ),
        (
            "blanks before the # are trimmed",
            b"#!/bin/sh   # a comment\n",
            words(&["/bin/sh"]),
        ),
        (
            "a carriage return is no blank",
            b"#!/bin/sh\r\n",
            words(&["/bin/sh\r"]),
        ),
        (
            "a carriage return stays in its argument",
            b"#!/bin/sh -x\r\n",
            words(&["/bin/sh", "-x\r"]),
        ),
        (
            "a vertical tab is no blank",
            b"#!/bin/echo a\x0bb\n",
            words(&["/bin/echo", "a\x0bb"]),
        ),
        (
            "only the first line counts",
            b"#!/bin/sh\n#!/bin/bash x\n",
            words(&["/bin/sh"]),
        ),
        (
            "a NUL past the line is the body's",
            b"#!/bin/sh\n\0\0rest",
            words(&["/bin/sh"]),
        ),
        ("#! and the end", b"#!\n", Err(ENOEXEC)),
        ("blanks, then #", b"#!   #x\n", Err(ENOEXEC)),
        ("# at once", b"#!#\n", Err(ENOEXEC)),
        ("blanks to the end", b"#!   \t\n", Err(ENOEXEC)),
        (
            "no end of line: the file ends",
            b"#!/bin/echo ",
            Err(ENOEXEC),
        ),
        ("#! alone", b"#!", Err(ENOEXEC)),
        ("a NUL inside the line", b"#!/bin/echo a\0b\n", Err(KERNEL)),
        ("a blank, then a NUL", b"#!/bin/echo \0\n", Err(KERNEL)),
        ("a NUL for the interpreter", b"#!\0/bin/sh\n", Err(KERNEL)),
        ("an empty file", b"", Err(NOT_SCRIPT)),
        (
            "a Mach-O",
            &[0xcf, 0xfa, 0xed, 0xfe, 7, 0, 0, 1],
            Err(NOT_SCRIPT),
        ),
        ("a blank before #!", b" #!/bin/sh\n", Err(NOT_SCRIPT)),
        ("!# reversed", b"!#/bin/sh\n", Err(NOT_SCRIPT)),
        ("# alone", b"#/bin/sh\n", Err(NOT_SCRIPT)),
    ];
    for (what, bytes, want) in rows {
        assert_eq!(parsed(bytes), *want, "{what}: {bytes:?}");
    }
}

/// **THE LINE MUST END WITHIN 512 BYTES**: an end at offset 511 is read, one at 512 is past the
/// header (`ENOEXEC`), whatever follows; and blanks filling the header leave no interpreter.
#[test]
fn the_line_ends_within_the_header() {
    for end in *b"\n#" {
        let mut at_511 = b"#!/bin/echo ".to_vec();
        at_511.resize(511, b'a');
        at_511.push(end);
        let argument = vec![b'a'; 511 - b"#!/bin/echo ".len()];
        assert_eq!(
            parsed(&at_511),
            Ok(vec![b"/bin/echo".to_vec(), argument]),
            "an end at 511"
        );
        let mut at_512 = b"#!/bin/echo ".to_vec();
        at_512.resize(512, b'a');
        at_512.push(end);
        at_512.extend_from_slice(b"more\n");
        assert_eq!(parsed(&at_512), Err(ENOEXEC), "an end at 512");
        let mut trailing = b"#!/bin/sh".to_vec();
        trailing.resize(511, b' ');
        trailing.push(end);
        assert_eq!(parsed(&trailing), words(&["/bin/sh"]), "blanks to 511");
    }
    let mut blanks = b"#!".to_vec();
    blanks.resize(HEADER, b' ');
    blanks.push(b'\n');
    assert_eq!(parsed(&blanks), Err(ENOEXEC), "blanks to the header's end");
}

#[cfg(target_vendor = "apple")]
mod darwin_tests {
    use super::super::darwin::{ExecFacts, Head, head_of};
    use super::super::{Outcome, interpret, interprets_here};
    use crate::spawnfd::tests::{STAGE_ENV, launch_staged};
    use crate::spawnfd::{NativeImage as _, command, kernel_scripts_during, transparent};
    use std::ffi::{CStr, CString, OsStr, OsString};
    use std::io;
    use std::os::fd::OwnedFd;
    use std::os::unix::ffi::OsStrExt as _;
    use std::os::unix::fs::PermissionsExt as _;
    use std::os::unix::process::CommandExt as _;
    use std::os::unix::process::ExitStatusExt as _;
    use std::path::{Path, PathBuf};

    /// Why a test of this layer's own interpretation decides nothing in a sandboxed process.
    const SANDBOXED: &str = "this process runs under a sandbox, whose #! files spawnfd leaves to \
                             the kernel (spawnfd::shebang::interpret), so nothing here judges \
                             spawnfd's own interpretation; run the test unsandboxed";

    /// The `#!` files a spawn hands the kernel ([`kernel_scripts_during`]): `interpreted` where
    /// this layer interprets what it can, `alone` where it leaves each to the kernel (in a
    /// sandboxed process).
    fn handed(interpreted: usize, alone: usize) -> usize {
        if interprets_here() {
            interpreted
        } else {
            alone
        }
    }

    fn write_mode(path: &Path, body: &[u8], mode: u32) {
        if let Some(dir) = path.parent() {
            std::fs::create_dir_all(dir).expect("fixture dir");
        }
        std::fs::write(path, body).expect("write fixture");
        std::fs::set_permissions(path, std::fs::Permissions::from_mode(mode)).expect("chmod");
    }

    fn c(path: &Path) -> CString {
        CString::new(path.as_os_str().as_bytes()).expect("a path without NUL")
    }

    fn euid_is_root() -> bool {
        // SAFETY: `geteuid` takes nothing and cannot fail.
        unsafe { libc::geteuid() == 0 }
    }

    /// **A SHORT FILE'S HEAD IS NUL PAST ITS END**, as the kernel's read leaves it, and a head
    /// that cannot be read is told from no file at all.
    #[test]
    fn a_short_head_reads_nul_past_the_end_and_no_file_is_told_from_an_unread_one() {
        let scratch = aterm_tempfile::tempdir().expect("scratch");
        let short = scratch.path().join("short");
        write_mode(&short, b"#!x", 0o755);
        let mut head = [0xff; super::super::HEADER];
        let Head::Read(_) = head_of(&c(&short), &mut head) else {
            panic!("the short file's head was not read");
        };
        assert_eq!(&head[..3], b"#!x");
        assert!(head[3..].iter().all(|&b| b == 0), "NUL past the end");
        assert_eq!(super::super::parse(&head), super::super::Line::Malformed);
        assert!(matches!(
            head_of(c"/nonexistent/spawnfd-shebang", &mut head),
            Head::NoFile
        ));
        assert!(matches!(
            head_of(&c(scratch.path()), &mut head),
            Head::NoFile
        ));
        let exec_only = scratch.path().join("exec-only");
        write_mode(&exec_only, b"#!/bin/sh\n", 0o111);
        crate::host::could_not_run_as_root("an exec-only file, which root reads");
        assert!(matches!(head_of(&c(&exec_only), &mut head), Head::Unread));
    }

    /// **THE EXEC CHECKS PASS ONLY WHERE EACH FACT DOES, AND `noexec` IS READ AS `mount` READS
    /// IT**: a regular file with an execute bit and no set-id bit, executable by this identity,
    /// on a volume not mounted `noexec` and unquarantined, passes, and each fact alone otherwise
    /// fails it; the facts of a mount point `mount` lists `noexec` say `noexec`. NEGATIVE
    /// CONTROL: those of the scratch directory, where the other tests run their scripts, do not.
    #[test]
    fn exec_checks_pass_only_where_each_fact_does_and_noexec_reads_as_mount_lists_it() {
        let passing = ExecFacts {
            mode: libc::S_IFREG | 0o755,
            executable: true,
            noexec: false,
            quarantined: false,
        };
        assert!(passing.pass());
        let failing = [
            (
                "a directory",
                ExecFacts {
                    mode: libc::S_IFDIR | 0o755,
                    ..passing
                },
            ),
            (
                "no execute bit",
                ExecFacts {
                    mode: libc::S_IFREG | 0o644,
                    ..passing
                },
            ),
            (
                "set-uid",
                ExecFacts {
                    mode: libc::S_IFREG | libc::S_ISUID | 0o755,
                    ..passing
                },
            ),
            (
                "set-gid",
                ExecFacts {
                    mode: libc::S_IFREG | libc::S_ISGID | 0o755,
                    ..passing
                },
            ),
            (
                "not this identity's to execute",
                ExecFacts {
                    executable: false,
                    ..passing
                },
            ),
            (
                "on a noexec volume",
                ExecFacts {
                    noexec: true,
                    ..passing
                },
            ),
            (
                "quarantined",
                ExecFacts {
                    quarantined: true,
                    ..passing
                },
            ),
        ];
        for (what, facts) in failing {
            assert!(!facts.pass(), "{what}");
        }
        let facts_of = |dir: &Path| {
            let open = std::fs::File::open(dir).expect("open the directory");
            ExecFacts::read(&OwnedFd::from(open), &c(dir)).expect("its facts")
        };
        let scratch = aterm_tempfile::tempdir().expect("scratch");
        assert!(
            !facts_of(scratch.path()).noexec,
            "CONTROL: the scratch volume runs programs"
        );
        let listed = command("/sbin/mount").output().expect("mount");
        assert!(listed.status.success(), "mount: {listed:?}");
        let listing = String::from_utf8_lossy(&listed.stdout);
        // Each line reads `<device> on <mount point> (<option>, <option>, ...)`.
        let noexec = listing.lines().find_map(|line| {
            let (_, rest) = line.split_once(" on ")?;
            let (point, options) = rest.rsplit_once(" (")?;
            let options = options.strip_suffix(')')?;
            options
                .split(", ")
                .any(|option| option == "noexec")
                .then(|| PathBuf::from(point))
        });
        let Some(point) = noexec else {
            crate::host::could_not_run(format!(
                "no volume here is mounted noexec (`mount` lists none), so no noexec read is \
                 judged:\n{listing}"
            ));
        };
        assert!(
            facts_of(&point).noexec,
            "{}: mount lists it noexec",
            point.display()
        );
    }

    /// **AN INTERPRETED SCRIPT'S ARGV IS THE KERNEL'S**: the line's words, the script's path
    /// exactly as the exec was given it (a relative one stays relative), then the arguments after
    /// the caller's `argv[0]`, which is dropped; the interpreter is named as written. A relative
    /// path whose directory this layer cannot name, a set-uid interpreter, whose bit the kernel's
    /// interpretation ignores, and a quarantined script are the kernel's. NEGATIVE CONTROL: the
    /// same interpreter without its set-uid bit is this layer's.
    #[test]
    fn an_interpreted_scripts_argv_is_the_kernels() {
        if !interprets_here() {
            crate::host::could_not_run(SANDBOXED);
        }
        let scratch = aterm_tempfile::tempdir().expect("scratch");
        let dir = scratch.path();
        write_mode(&dir.join("echo"), b"#!/bin/echo  a   b\tc \n", 0o755);
        let argv = [
            c"custom-zero".to_owned(),
            c"x".to_owned(),
            c"y z".to_owned(),
        ];
        let script = c(&dir.join("echo"));
        let want = |path: &CStr| Outcome::Run {
            path: c"/bin/echo".to_owned(),
            argv: ["/bin/echo", "a", "b", "c"]
                .iter()
                .map(|word| CString::new(*word).unwrap())
                .chain([path.to_owned(), c"x".to_owned(), c"y z".to_owned()])
                .collect(),
        };
        assert_eq!(interpret(&script, &argv, false, None), want(&script));
        assert_eq!(
            interpret(c"echo", &argv, true, Some(dir)),
            want(c"echo"),
            "relative stays relative"
        );
        assert_eq!(
            interpret(c"echo", &argv, true, None),
            Outcome::Kernel,
            "a directory this layer cannot name"
        );

        std::os::unix::fs::symlink("/bin/sh", dir.join("interp")).expect("symlink");
        write_mode(&dir.join("relative"), b"#!./interp -e\n", 0o755);
        assert_eq!(
            interpret(c"relative", &argv[..1], true, Some(dir)),
            Outcome::Run {
                path: c"./interp".to_owned(),
                argv: vec![
                    c"./interp".to_owned(),
                    c"-e".to_owned(),
                    c"relative".to_owned()
                ],
            },
            "a relative interpreter is named as written"
        );

        let image = dir.join("set-id-image");
        write_mode(&image, &[0xcf, 0xfa, 0xed, 0xfe, 7, 0, 0, 1], 0o4755);
        let mode = std::fs::metadata(&image)
            .expect("stat")
            .permissions()
            .mode();
        assert_ne!(
            mode & 0o4000,
            0,
            "the fixture keeps its set-uid bit: {mode:o}"
        );
        let line = [b"#!".as_slice(), image.as_os_str().as_bytes(), b" -x\n"].concat();
        let set_id = c(&dir.join("set-id-interpreted"));
        write_mode(&dir.join("set-id-interpreted"), &line, 0o755);
        assert_eq!(
            interpret(&set_id, &argv[..1], false, None),
            Outcome::Kernel,
            "a set-uid interpreter"
        );
        std::fs::set_permissions(&image, std::fs::Permissions::from_mode(0o755)).expect("chmod");
        assert_eq!(
            interpret(&set_id, &argv[..1], false, None),
            Outcome::Run {
                path: c(&image),
                argv: vec![c(&image), c"-x".to_owned(), set_id.clone()],
            },
            "CONTROL: the interpreter without its set-uid bit"
        );

        let quarantined = dir.join("quarantined");
        write_mode(&quarantined, b"#!/bin/sh\n", 0o755);
        let value = b"0081;00000000;spawnfd-test;";
        let name = c"com.apple.quarantine";
        // SAFETY: NUL-terminated path and name, and `value.len()` readable bytes.
        let set = unsafe {
            libc::setxattr(
                c(&quarantined).as_ptr(),
                name.as_ptr(),
                value.as_ptr().cast(),
                value.len(),
                0,
                0,
            )
        };
        assert_eq!(set, 0, "setxattr: {}", io::Error::last_os_error());
        assert_eq!(
            interpret(&c(&quarantined), &argv, false, None),
            Outcome::Kernel,
            "a quarantined script"
        );
    }

    type Answer = Result<(Option<i32>, String, String), (io::ErrorKind, Option<i32>)>;

    fn answer(run: io::Result<std::process::Output>) -> Answer {
        run.map(|out| {
            (
                out.status.code(),
                String::from_utf8_lossy(&out.stdout).into_owned(),
                String::from_utf8_lossy(&out.stderr).into_owned(),
            )
        })
        .map_err(|error| (error.kind(), error.raw_os_error()))
    }

    struct Row {
        what: &'static str,
        program: OsString,
        args: Vec<&'static str>,
        cwd: Option<PathBuf>,
        path: Option<OsString>,
        arg0: Option<&'static str>,
        /// The `#!` files the spawn hands the kernel where this layer interprets what it can.
        kernel: usize,
        /// The `#!` files the spawn hands the kernel where this layer leaves each to it.
        alone: usize,
    }

    fn row(what: &'static str, program: impl AsRef<OsStr>, args: &[&'static str]) -> Row {
        Row {
            what,
            program: program.as_ref().to_owned(),
            args: args.to_vec(),
            cwd: None,
            path: None,
            arg0: None,
            kernel: 0,
            alone: 1,
        }
    }

    impl Row {
        fn cwd(mut self, dir: &Path) -> Self {
            self.cwd = Some(dir.to_owned());
            self
        }
        fn path(mut self, path: OsString) -> Self {
            self.path = Some(path);
            self
        }
        fn arg0(mut self, arg0: &'static str) -> Self {
            self.arg0 = Some(arg0);
            self
        }
        fn kernel(mut self, handed: usize) -> Self {
            self.kernel = handed;
            self
        }
        fn alone(mut self, handed: usize) -> Self {
            self.alone = handed;
            self
        }
    }

    /// **A SPAWN OF A `#!` FILE ANSWERS AS THE KERNEL'S INTERPRETATION ANSWERS, AND HANDS THE
    /// KERNEL NONE IT CAN INTERPRET**: each row's output, exit and error match
    /// `std::process::Command`'s, whose spawn has the kernel interpret the file, and the count of
    /// files handed the kernel to interpret is 0 wherever this layer reproduces the kernel.
    /// NEGATIVE CONTROL: the rows this layer leaves to the kernel (no execute permission, set-id,
    /// a NUL in the line, an interpreter it cannot read, a script it cannot read) count each. In
    /// a sandboxed process, where this layer leaves every one to the kernel, each counts.
    #[test]
    fn a_scripts_spawn_answers_as_the_kernels_interpretation() {
        let scratch = aterm_tempfile::tempdir().expect("scratch");
        let dir = scratch.path();
        let at = |name: &str| dir.join(name);
        let print = b"printf '[%s]' \"$0\" \"$@\"; echo\n";
        let script = |line: &[u8]| [line, print.as_slice()].concat();
        write_mode(&at("words"), b"#!/bin/echo  a   b\tc  \n", 0o755);
        write_mode(&at("comment"), b"#!/bin/echo first#second\n", 0o755);
        write_mode(&at("env"), &script(b"#!/usr/bin/env sh\n"), 0o755);
        write_mode(&at("sh"), &script(b"#!/bin/sh\n"), 0o755);
        write_mode(&at("rel/script"), &script(b"#!./interp\n"), 0o755);
        std::os::unix::fs::symlink("/bin/sh", at("rel/interp")).expect("symlink");
        write_mode(&at("exec-only"), &script(b"#!/bin/sh\n"), 0o111);
        write_mode(&at("unrunnable"), &script(b"#!/bin/sh\n"), 0o644);
        write_mode(&at("setuid"), &script(b"#!/bin/sh\n"), 0o4755);
        write_mode(&at("inner"), &script(b"#!/bin/sh\n"), 0o755);
        let nested = [b"#!".as_slice(), at("inner").as_os_str().as_bytes(), b"\n"].concat();
        write_mode(&at("nested"), &script(&nested), 0o755);
        write_mode(&at("malformed"), &script(b"#!\n"), 0o755);
        write_mode(&at("unended"), b"#!/bin/sh", 0o755);
        write_mode(&at("nul"), b"#!/bin/echo a\0b\n", 0o755);
        write_mode(&at("carriage"), &script(b"#!/bin/sh\r\n"), 0o755);
        write_mode(
            &at("missing/tool"),
            &script(b"#!/nonexistent/interp\n"),
            0o755,
        );
        write_mode(&at("good/tool"), &script(b"#!/bin/sh\n"), 0o755);
        let search = [at("missing"), at("good")]
            .iter()
            .map(|dir| dir.as_os_str().to_owned())
            .collect::<Vec<_>>()
            .join(OsStr::new(":"));
        let unreadable = usize::from(!euid_is_root());
        let rows = [
            row("a Mach-O", "/bin/echo", &["plain"]).alone(0),
            row("multi-word arguments", at("words"), &["x", "y z"]),
            row("# ends the line", at("comment"), &["x"]),
            row("#!/usr/bin/env sh", at("env"), &["x"]),
            row("#!/bin/sh", at("sh"), &["x", ""]),
            row("arg0 on a script is dropped", at("sh"), &["x"]).arg0("custom-zero"),
            row(
                "a relative interpreter, from the working directory",
                at("rel/script"),
                &["x"],
            )
            .cwd(&at("rel")),
            row(
                "a nested interpreter runs the script under /bin/sh",
                at("nested"),
                &["x"],
            ),
            row(
                "a line with no interpreter runs under /bin/sh",
                at("malformed"),
                &["x"],
            ),
            row("a line with no end runs under /bin/sh", at("unended"), &[]),
            row(
                "a missing interpreter passes over the first candidate on PATH",
                "tool",
                &["x"],
            )
            .path(search)
            .kernel(1)
            .alone(2),
            row(
                "an exec-only script, which only the kernel reads",
                at("exec-only"),
                &["x"],
            )
            .kernel(unreadable),
            row("a script without an execute bit", at("unrunnable"), &[]).kernel(1),
            row("a set-id script", at("setuid"), &["x"]).kernel(1),
            row("a NUL in the line", at("nul"), &["x"]).kernel(1),
            row("a carriage return in the interpreter", at("carriage"), &[]).kernel(1),
        ];
        for row in &rows {
            let mut theirs = std::process::Command::new(&row.program);
            let mut ours = command(&row.program);
            theirs.args(&row.args);
            ours.args(&row.args);
            if let Some(dir) = &row.cwd {
                theirs.current_dir(dir);
                ours.current_dir(dir);
            }
            if let Some(path) = &row.path {
                theirs.env("PATH", path);
                ours.env("PATH", path);
            }
            if let Some(arg0) = row.arg0 {
                theirs.arg0(arg0);
                ours.arg0(arg0);
            }
            let theirs = answer(theirs.output());
            let (ours, count) = kernel_scripts_during(|| answer(ours.output()));
            assert_eq!(ours, theirs, "{}: ours, then the kernel's", row.what);
            assert_eq!(
                count,
                handed(row.kernel, row.alone),
                "{}: files handed the kernel",
                row.what
            );
        }
    }

    /// **A SET-ID INTERPRETER RUNS WITH THE KERNEL'S CREDENTIALS**: XNU takes an interpreted
    /// exec's set-id from the script alone, so a set-gid interpreter of a script with no set-id
    /// bit runs with this process's group, and a spawn through this layer answers as the kernel's
    /// interpretation does, handing the kernel the script. The interpreter is a set-gid copy of
    /// `env`, whose `-u` takes the script's path as the name it unsets, so it runs `id -g`, which
    /// prints its effective group. NEGATIVE CONTROL: the interpreter run as itself prints the
    /// group its set-gid bit gives it.
    #[test]
    fn a_set_id_interpreter_runs_with_the_kernels_credentials() {
        // SAFETY: `getegid` takes nothing and cannot fail.
        let egid = unsafe { libc::getegid() };
        // SAFETY: a count of 0 and a null list ask for the number of groups alone.
        let count = unsafe { libc::getgroups(0, std::ptr::null_mut()) };
        let mut groups = vec![0; usize::try_from(count).expect("a count of groups")];
        // SAFETY: `groups` holds `count` writable entries, which the call fills.
        let got = unsafe { libc::getgroups(count, groups.as_mut_ptr()) };
        groups.truncate(usize::try_from(got).expect("this process's groups"));
        let Some(other) = groups.into_iter().find(|&group| group != egid) else {
            crate::host::could_not_run(
                "this process is in no group but its effective one, so no set-gid interpreter's \
                 group differs from it",
            );
        };
        let scratch = aterm_tempfile::tempdir().expect("scratch");
        let mut kept = crate::deadline::KeptUntilRan::new(scratch.keep());
        let env = kept.path().join("env");
        crate::deadline::ad_hoc_copy(Path::new("/usr/bin/env"), &env);
        std::os::unix::fs::chown(&env, None, Some(other)).expect("chgrp the interpreter");
        std::fs::set_permissions(&env, std::fs::Permissions::from_mode(0o2755)).expect("chmod");
        let mode = std::fs::metadata(&env).expect("stat").permissions().mode();
        if mode & 0o2000 == 0 {
            // Nothing has been started from the directory, so it goes.
            kept.ran();
            crate::host::could_not_run(format!(
                "the copy of env kept no set-gid bit (mode {mode:o}): this process may not make \
                 a set-gid file here (a sandbox clears it), so no set-gid interpreter is judged"
            ));
        }
        let script = kept.path().join("script");
        let line = [b"#!".as_slice(), env.as_os_str().as_bytes(), b" -u\n"].concat();
        write_mode(&script, &line, 0o755);
        let id = ["/usr/bin/id", "-g"];
        let theirs = answer(std::process::Command::new(&script).args(id).output());
        let (ours, count) = kernel_scripts_during(|| answer(command(&script).args(id).output()));
        let itself = answer(command(&env).arg("-u").arg(&script).args(id).output());
        if [&theirs, &ours, &itself]
            .iter()
            .all(|run| matches!(run, Ok((_, out, _)) if !out.is_empty()))
        {
            kept.ran();
        }
        assert_eq!(
            theirs,
            Ok((Some(0), format!("{egid}\n"), String::new())),
            "the kernel's interpretation"
        );
        assert_eq!(ours, theirs, "ours, then the kernel's");
        assert_eq!(count, 1, "the script is handed the kernel");
        assert_eq!(
            itself,
            Ok((Some(0), format!("{other}\n"), String::new())),
            "CONTROL: the interpreter run as itself"
        );
    }

    /// The kernel's own answer to a `posix_spawn` of `path` with no argument and an empty
    /// environment, after a chdir file action to `cwd` and in process group `pgroup` where given:
    /// the child's exit code, or the spawn's errno. Nothing here reads or interprets the file.
    fn kernel_spawn(
        path: &CStr,
        cwd: Option<&CStr>,
        pgroup: Option<i32>,
    ) -> Result<Option<i32>, i32> {
        type Attr = *mut std::ffi::c_void;
        type Actions = *mut std::ffi::c_void;
        const POSIX_SPAWN_SETPGROUP: i16 = 0x0002;
        unsafe extern "C" {
            fn posix_spawnattr_init(attr: *mut Attr) -> i32;
            fn posix_spawnattr_destroy(attr: *mut Attr) -> i32;
            fn posix_spawnattr_setflags(attr: *mut Attr, flags: i16) -> i32;
            fn posix_spawnattr_setpgroup(attr: *mut Attr, pgroup: i32) -> i32;
            fn posix_spawn_file_actions_init(actions: *mut Actions) -> i32;
            fn posix_spawn_file_actions_destroy(actions: *mut Actions) -> i32;
            fn posix_spawn_file_actions_addchdir_np(
                actions: *mut Actions,
                path: *const std::ffi::c_char,
            ) -> i32;
            fn posix_spawn(
                pid: *mut i32,
                path: *const std::ffi::c_char,
                actions: *const Actions,
                attr: *const Attr,
                argv: *const *const std::ffi::c_char,
                envp: *const *const std::ffi::c_char,
            ) -> i32;
        }
        let argv = [path.as_ptr(), std::ptr::null()];
        let envp = [std::ptr::null()];
        let mut attr: Attr = std::ptr::null_mut();
        let mut actions: Actions = std::ptr::null_mut();
        let mut pid = 0;
        // SAFETY: each object is initialised before use and destroyed once; the path, the
        // directory and both NULL-terminated arrays outlive the calls, none of which retains them.
        let rc = unsafe {
            assert_eq!(posix_spawnattr_init(&mut attr), 0);
            assert_eq!(posix_spawn_file_actions_init(&mut actions), 0);
            if let Some(pgroup) = pgroup {
                assert_eq!(
                    posix_spawnattr_setflags(&mut attr, POSIX_SPAWN_SETPGROUP),
                    0
                );
                assert_eq!(posix_spawnattr_setpgroup(&mut attr, pgroup), 0);
            }
            if let Some(cwd) = cwd {
                assert_eq!(
                    posix_spawn_file_actions_addchdir_np(&mut actions, cwd.as_ptr()),
                    0
                );
            }
            let rc = posix_spawn(
                &mut pid,
                path.as_ptr(),
                &actions,
                &attr,
                argv.as_ptr(),
                envp.as_ptr(),
            );
            posix_spawn_file_actions_destroy(&mut actions);
            posix_spawnattr_destroy(&mut attr);
            rc
        };
        if rc != 0 {
            return Err(rc);
        }
        let mut status = 0;
        // SAFETY: our own unreaped child and one writable status.
        assert_eq!(unsafe { libc::waitpid(pid, &mut status, 0) }, pid);
        Ok(std::process::ExitStatus::from_raw(status).code())
    }

    /// **A NATIVE-IMAGE SPAWN STILL RUNS A `#!` FILE, AND FAILS WHAT THE KERNEL FAILS**: a
    /// well-formed script runs under its interpreter, and a line with no interpreter or with a
    /// `#!` interpreter fails with `ENOEXEC`, as the kernel's `posix_spawn` fails it, never under
    /// a shell, with none handed the kernel. The kernel's chdir file action fails before it reads
    /// the file, so in a missing working directory the refusal is the kernel's to make, and the
    /// spawn answers its `ENOENT`, handing it the script; a process group the child may not join
    /// fails after the read (a Mach-O there answers `EPERM`), so there the refusal stays this
    /// layer's, `ENOEXEC` as the kernel's. NEGATIVE CONTROL: the kernel's own `posix_spawn` of
    /// each script answers as ours does, and `pspawn`'s counts as one handed it.
    #[test]
    fn a_native_image_spawn_interprets_and_refuses_as_the_kernel_does() {
        let scratch = aterm_tempfile::tempdir().expect("scratch");
        let at = |name: &str| scratch.path().join(name);
        write_mode(&at("runs"), b"#!/bin/sh\nexit 7\n", 0o755);
        write_mode(&at("inner"), b"#!/bin/sh\nexit 0\n", 0o755);
        let nested = [
            b"#!".as_slice(),
            at("inner").as_os_str().as_bytes(),
            b"\nexit 0\n",
        ]
        .concat();
        write_mode(&at("nested"), &nested, 0o755);
        write_mode(&at("malformed"), b"#!\nexit 0\n", 0o755);
        let native = |path: &Path, cwd: Option<&Path>, pgroup: Option<i32>| {
            kernel_scripts_during(|| {
                let mut spawn = command(path);
                spawn
                    .native_image(true)
                    .stdout(crate::spawnfd::Stdio::null())
                    .stderr(crate::spawnfd::Stdio::null());
                if let Some(dir) = cwd {
                    spawn.current_dir(dir);
                }
                if let Some(pgroup) = pgroup {
                    spawn.process_group(pgroup);
                }
                spawn
                    .status()
                    .map(|status| status.code())
                    .map_err(|error| error.raw_os_error().unwrap_or(0))
            })
        };
        let pspawn = |path: &Path| {
            let path = path.to_str().expect("a UTF-8 scratch path");
            kernel_scripts_during(|| {
                crate::pspawn::spawn_cloexec_default(path, &[])
                    .map(|pid| {
                        let pid = i32::try_from(pid).expect("a pid");
                        let mut status = 0;
                        // SAFETY: our own unreaped child and one writable status.
                        assert_eq!(unsafe { libc::waitpid(pid, &mut status, 0) }, pid);
                        std::process::ExitStatus::from_raw(status).code()
                    })
                    .map_err(|error| error.raw_os_error().unwrap_or(0))
            })
        };
        let interpreted = handed(0, 1);
        assert_eq!(native(&at("runs"), None, None), (Ok(Some(7)), interpreted));
        assert_eq!(pspawn(&at("runs")), (Ok(Some(7)), 1), "CONTROL");
        let missing = at("missing");
        // Launchd's process group, in another session, is one no child of this one may join.
        let foreign = 1;
        assert_eq!(
            kernel_spawn(c"/usr/bin/true", None, Some(foreign)),
            Err(libc::EPERM),
            "CONTROL: a Mach-O in a group it may not join"
        );
        for name in ["nested", "malformed"] {
            let path = at(name);
            assert_eq!(
                native(&path, None, None),
                (Err(libc::ENOEXEC), interpreted),
                "{name}"
            );
            assert_eq!(pspawn(&path), (Err(libc::ENOEXEC), 1), "{name}: CONTROL");
            let theirs = kernel_spawn(&c(&path), Some(&c(&missing)), None);
            assert_eq!(
                theirs,
                Err(libc::ENOENT),
                "{name}: the kernel's, in {missing:?}"
            );
            assert_eq!(
                native(&path, Some(&missing), None),
                (theirs, 1),
                "{name}: in {missing:?}"
            );
            let theirs = kernel_spawn(&c(&path), None, Some(foreign));
            assert_eq!(
                theirs,
                Err(libc::ENOEXEC),
                "{name}: the kernel's, in group {foreign}"
            );
            assert_eq!(
                native(&path, None, Some(foreign)),
                (theirs, interpreted),
                "{name}: in group {foreign}"
            );
        }
    }

    /// **A PASS-THROUGH SPAWN INTERPRETS A `#!` FILE AS THE KERNEL DOES**: its exit is std's,
    /// on the program's path and on a bare name's search, with none handed the kernel.
    #[test]
    fn a_pass_through_spawn_interprets_as_the_kernel_does() {
        let scratch = aterm_tempfile::tempdir().expect("scratch");
        let script = scratch.path().join("bin/exits");
        write_mode(&script, b"#!/bin/sh\nexit \"$1\"\n", 0o755);
        let search = scratch.path().join("bin");
        let theirs = std::process::Command::new(&script)
            .arg("7")
            .status()
            .expect("std's run")
            .code();
        assert_eq!(theirs, Some(7));
        let (ours, count) =
            kernel_scripts_during(|| transparent(&script).arg("7").status().expect("ours"));
        assert_eq!((ours.code(), count), (theirs, handed(0, 1)));
        let (ours, count) = kernel_scripts_during(|| {
            transparent("exits")
                .env("PATH", &search)
                .arg("9")
                .status()
                .expect("ours, searched")
        });
        assert_eq!((ours.code(), count), (Some(9), handed(0, 1)));
    }

    /// **A SCRIPT STARTED SUSPENDED IS ITS INTERPRETER, HELD AT ITS EXEC**: it has not run until
    /// resumed, and none is handed the kernel to interpret. The child is resumed and reaped
    /// before anything is judged, so a failed judgment leaves no stopped child behind.
    #[test]
    fn a_script_started_suspended_runs_only_once_resumed() {
        let scratch = aterm_tempfile::tempdir().expect("scratch");
        let marker = scratch.path().join("ran");
        let script = scratch.path().join("marks");
        write_mode(&script, b"#!/bin/sh\nprintf ran > \"$1\"\n", 0o755);
        let (mut held, count) = kernel_scripts_during(|| {
            command(&script)
                .arg(&marker)
                .stdout(crate::spawnfd::Stdio::null())
                .stderr(crate::spawnfd::Stdio::null())
                .start_suspended(true)
                .spawn()
                .expect("spawn suspended")
        });
        let ran_held = marker.exists();
        held.resume().expect("resume");
        let status = held.wait().expect("wait");
        assert_eq!(count, handed(0, 1));
        assert!(!ran_held, "a held script ran");
        assert!(status.success(), "{status:?}");
        assert_eq!(std::fs::read_to_string(&marker).expect("the marker"), "ran");
    }

    const PREPARED: &str = "spawnfd::shebang::tests::darwin_tests::an_exec_in_place_interprets_a_script_when_prepared_and_refuses_at_dispatch";

    /// **AN EXEC IN PLACE INTERPRETS A SCRIPT WHEN PREPARED, AND REFUSES AT DISPATCH**: a prepared
    /// exec of a `#!` file captures its interpreter and the kernel's argv before any grant,
    /// handing the kernel none to interpret, and a line the kernel refuses fails the dispatch,
    /// not the preparation, with `ENOEXEC`; dispatched, the prepared script replaces the process
    /// as its interpreter. A pass-through's exec does the same, and so does a command's exec
    /// (the tether's exec of its program is a pass-through's command's): an argument past the
    /// kernel's `ARG_MAX` fails each with `E2BIG` once it has chosen its image, so each returns
    /// and is counted, and none hands the kernel the script. Staged: each exec replaces its stage.
    #[test]
    fn an_exec_in_place_interprets_a_script_when_prepared_and_refuses_at_dispatch() {
        let stage = std::env::var(STAGE_ENV).ok();
        let script = std::env::var_os("ATERM_UDS_SHEBANG_SCRIPT").map(PathBuf::from);
        match (stage.as_deref(), script) {
            (Some("shebang-prepared"), Some(script)) => {
                let malformed = script.with_file_name("malformed");
                let (refused, count) = kernel_scripts_during(|| {
                    transparent(&malformed).prepare_exec().expect("prepared")
                });
                assert_eq!(count, 0);
                let error = refused.exec_until(u64::MAX);
                assert_eq!(error.raw_os_error(), Some(libc::ENOEXEC), "{error}");
                let mut command = transparent(&script);
                command.arg0("custom-zero").arg("captured");
                let (prepared, count) =
                    kernel_scripts_during(|| command.prepare_exec().expect("prepared"));
                assert_eq!(count, 0);
                let (image, path) = prepared.captured();
                assert_eq!(path.to_bytes(), b"/bin/echo");
                let argv: Vec<&[u8]> = image.argv.iter().map(|word| word.to_bytes()).collect();
                let want: [&[u8]; 4] = [
                    b"/bin/echo",
                    b"STAGED-prepared-exec",
                    script.as_os_str().as_bytes(),
                    b"captured",
                ];
                assert_eq!(argv, want);
                let error = prepared.exec_until(u64::MAX);
                panic!("the prepared exec returned: {error}");
            }
            (Some("shebang-transparent"), Some(script)) => {
                let error = transparent(&script)
                    .arg0("custom-zero")
                    .arg("passed")
                    .exec();
                panic!("the pass-through exec returned: {error}");
            }
            (Some("shebang-exec"), Some(script)) => {
                let huge = "x".repeat(2 << 20);
                let (by_command, command_count) =
                    kernel_scripts_during(|| command(&script).arg(&huge).exec());
                let (by_tether, tether_count) =
                    kernel_scripts_during(|| transparent(&script).into_command().arg(&huge).exec());
                let (by_pass, pass_count) =
                    kernel_scripts_during(|| transparent(&script).arg(&huge).exec());
                println!(
                    "SHEBANG-EXEC {:?} {command_count} {:?} {tether_count} {:?} {pass_count}",
                    by_command.raw_os_error(),
                    by_tether.raw_os_error(),
                    by_pass.raw_os_error(),
                );
                let error = command(&script).arg0("custom-zero").arg("execd").exec();
                panic!("the exec returned: {error}");
            }
            _ => {}
        }
        if !interprets_here() {
            crate::host::could_not_run(SANDBOXED);
        }
        let scratch = aterm_tempfile::tempdir().expect("scratch");
        let script = scratch.path().join("echoes");
        write_mode(&script, b"#!/bin/echo STAGED-prepared-exec\n", 0o755);
        write_mode(&scratch.path().join("malformed"), b"#!\n", 0o755);
        for (stage, last) in [
            ("shebang-prepared", "captured"),
            ("shebang-transparent", "passed"),
            ("shebang-exec", "execd"),
        ] {
            let mut launch = std::process::Command::new(std::env::current_exe().expect("test"));
            launch
                .args(["--exact", PREPARED, "--nocapture", "--test-threads=1"])
                .env("ATERM_UDS_SHEBANG_SCRIPT", &script);
            let out = launch_staged(launch, stage);
            let want = format!("STAGED-prepared-exec {} {last}\n", script.display());
            assert!(out.ends_with(&want), "{stage}: {out:?}");
            if stage == "shebang-exec" {
                let e2big = Some(libc::E2BIG);
                let counted = format!("SHEBANG-EXEC {e2big:?} 0 {e2big:?} 0 {e2big:?} 0\n");
                assert!(out.contains(&counted), "{stage}: {out:?}");
            }
        }
    }

    const SANDBOX_TEST: &str =
        "spawnfd::shebang::tests::darwin_tests::a_sandboxed_process_leaves_scripts_to_the_kernel";

    /// **A SANDBOXED PROCESS LEAVES ITS SCRIPTS TO THE KERNEL**, whose interpretation applies the
    /// sandbox's exec rules to the script: under `sandbox-exec`, a script this layer interprets
    /// unsandboxed is handed the kernel, and still answers as it would. NEGATIVE CONTROL: the
    /// same spawn unsandboxed hands the kernel none. A test process sandboxed already is the
    /// sandboxed case itself, and its unsandboxed control could not run.
    #[test]
    fn a_sandboxed_process_leaves_scripts_to_the_kernel() {
        if let (Ok("shebang-sandboxed"), Some(script)) = (
            std::env::var(STAGE_ENV).as_deref(),
            std::env::var_os("ATERM_UDS_SHEBANG_SCRIPT"),
        ) {
            assert!(super::super::darwin::sandboxed(), "sandbox-exec sandboxes");
            let (out, count) =
                kernel_scripts_during(|| command(&script).arg("x").output().expect("runs"));
            assert_eq!(count, 1);
            print!("{}", String::from_utf8_lossy(&out.stdout));
            return;
        }
        let scratch = aterm_tempfile::tempdir().expect("scratch");
        let script = scratch.path().join("echoes");
        write_mode(&script, b"#!/bin/echo STAGED-sandboxed\n", 0o755);
        let want = format!("STAGED-sandboxed {} x\n", script.display());
        let (out, count) =
            kernel_scripts_during(|| command(&script).arg("x").output().expect("runs"));
        assert_eq!(String::from_utf8_lossy(&out.stdout), want);
        if super::super::darwin::sandboxed() {
            assert_eq!(
                count, 1,
                "sandboxed, this layer hands the kernel the script"
            );
            crate::host::could_not_run(
                "the unsandboxed control: this test process runs under a sandbox already",
            );
        }
        assert_eq!(count, 0, "CONTROL: unsandboxed, this layer interprets it");
        if !Path::new("/usr/bin/sandbox-exec").exists() {
            crate::host::could_not_run("/usr/bin/sandbox-exec, the macOS sandbox's launcher");
        }
        let mut launch = std::process::Command::new("/usr/bin/sandbox-exec");
        launch
            .args(["-p", "(version 1)(allow default)"])
            .arg(std::env::current_exe().expect("the test binary"))
            .args(["--exact", SANDBOX_TEST, "--nocapture", "--test-threads=1"])
            .env("ATERM_UDS_SHEBANG_SCRIPT", &script);
        let out = launch_staged(launch, "shebang-sandboxed");
        assert!(out.contains(&want), "{out:?}");
    }
}
