// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Closed inherited stdio is `/dev/null`; open inherited stdio keeps its file identity.
//! Each fresh role inspects its own descriptors without opening them, and a direct native
//! `stat` observes the executed program's descriptors without shell substitutions. Both
//! tethered and untethered spawns, explicit nulls, and warmed preparation are controls.
//!
//! STOCK EXCEPTION: its work is Linux's spawn, and Trust ships its host's std alone. The
//! isolated `lab linux` guest runs it on the checkout's stock pin.

#[cfg(target_os = "linux")]
fn main() {
    use aterm_spec::wait::MUST_HAPPEN;
    use aterm_uds::spawnfd::{Command, Stdio, command};
    use std::io::{Read as _, Write as _};
    use std::os::fd::AsRawFd as _;
    use std::os::unix::fs::MetadataExt as _;

    const ROLE: &str = "ATERM_TETHER_STDIO_CASE";
    const PICK: &str = "ATERM_TETHER_STDIO_PICK";

    #[derive(Clone, Copy, Debug, PartialEq, Eq)]
    struct Case {
        fd: i32,
        explicit_null: bool,
        regular: bool,
        tethered: bool,
        warm: bool,
    }

    impl Case {
        fn name(self) -> String {
            format!(
                "{}:{}:{}:{}:{}",
                self.fd,
                if self.explicit_null {
                    "null"
                } else {
                    "inherit"
                },
                if self.regular { "regular" } else { "closed" },
                if self.tethered { "tethered" } else { "plain" },
                if self.warm { "warm" } else { "fresh" },
            )
        }

        fn parse(text: &str) -> Self {
            let fields: Vec<_> = text.split(':').collect();
            let [fd, source, original, route, preparation] = fields.as_slice() else {
                panic!("five case fields: {text}");
            };
            let case = Self {
                fd: fd.parse().expect("a stdio number"),
                explicit_null: *source == "null",
                regular: *original == "regular",
                tethered: *route == "tethered",
                warm: *preparation == "warm",
            };
            assert!(matches!(case.fd, 0 | 2));
            assert_eq!(case.name(), text, "a canonical case");
            case
        }

        fn route(self, cmd: &mut Command) {
            if self.tethered {
                cmd.require_tether().process_group(0);
            }
        }
    }

    #[derive(Clone, Copy, Debug, PartialEq, Eq)]
    struct Identity {
        mode: u32,
        rdev: u64,
        dev: u64,
        ino: u64,
    }

    impl Identity {
        fn of(metadata: &std::fs::Metadata) -> Self {
            Self {
                mode: metadata.mode(),
                rdev: metadata.rdev(),
                dev: metadata.dev(),
                ino: metadata.ino(),
            }
        }

        fn text(self) -> String {
            format!("{:x} {} {} {}", self.mode, self.rdev, self.dev, self.ino)
        }

        fn parse(text: &str) -> Option<Self> {
            let fields: Vec<_> = text.split_whitespace().collect();
            let [mode, rdev, dev, ino] = fields.as_slice() else {
                return None;
            };
            Some(Self {
                mode: u32::from_str_radix(mode, 16).ok()?,
                rdev: rdev.parse().ok()?,
                dev: dev.parse().ok()?,
                ino: ino.parse().ok()?,
            })
        }
    }

    // These reads never open a descriptor, so observing a closed slot cannot populate it.
    fn slot(fd: i32) -> String {
        // SAFETY: F_GETFD reads only this role's descriptor flags.
        let flags = unsafe { libc::fcntl(fd, libc::F_GETFD) };
        if flags < 0 {
            assert_eq!(
                std::io::Error::last_os_error().raw_os_error(),
                Some(libc::EBADF)
            );
            return "closed EBADF".into();
        }
        // SAFETY: stat is a plain output record; fstat initializes it on success.
        let mut stat: libc::stat = unsafe { std::mem::zeroed() };
        // SAFETY: this role owns fd, and stat is live writable storage.
        assert_eq!(unsafe { libc::fstat(fd, &mut stat) }, 0, "fstat {fd}");
        let identity = Identity {
            mode: stat.st_mode,
            rdev: stat.st_rdev,
            dev: stat.st_dev,
            ino: stat.st_ino,
        };
        let target =
            std::fs::read_link(format!("/proc/self/fd/{fd}")).expect("read descriptor target");
        format!(
            "open {} flags={flags} target={}",
            identity.text(),
            target.display()
        )
    }

    fn started<T>(case: Case, phase: &str, result: std::io::Result<T>) -> Option<T> {
        match result {
            Ok(child) => Some(child),
            Err(error) if case.tethered && error.kind() == std::io::ErrorKind::Unsupported => {
                println!(
                    "UNAVAILABLE phase={phase} source=require_tether arch={}: {error}",
                    std::env::consts::ARCH,
                );
                None
            }
            Err(error) => panic!("{}: spawn: {error}", case.name()),
        }
    }

    fn one_line<'a>(text: &'a str, prefix: &str) -> Result<&'a str, String> {
        let found: Vec<_> = text
            .lines()
            .filter_map(|line| line.strip_prefix(prefix))
            .collect();
        match found.as_slice() {
            [only] => Ok(*only),
            _ => Err(format!("expected one {prefix:?} observation: {found:?}")),
        }
    }

    fn assess_parent(case: Case, text: &str) -> Result<(Identity, Identity), String> {
        let identity = |prefix| {
            Identity::parse(one_line(text, prefix)?)
                .ok_or_else(|| format!("invalid {prefix:?} identity"))
        };
        let null = identity("NULL ")?;
        let regular = identity("FILE ")?;
        if null.mode & libc::S_IFMT != libc::S_IFCHR || regular.mode & libc::S_IFMT != libc::S_IFREG
        {
            return Err("invalid null or regular-file reference type".into());
        }
        let before = one_line(text, "BEFORE ")?;
        let after = one_line(text, "AFTER ")?;
        if before != after {
            return Err(format!(
                "parent descriptor changed: {before:?} -> {after:?}"
            ));
        }
        if case.regular {
            if !before.starts_with(&format!("open {} flags=", regular.text())) {
                return Err(format!(
                    "parent did not retain its regular file: {before:?}"
                ));
            }
        } else if before != "closed EBADF" {
            return Err(format!("parent did not retain its closed slot: {before:?}"));
        }
        Ok((null, regular))
    }

    fn assess(case: Case, text: &str) -> Result<Option<String>, String> {
        let omitted: Vec<_> = text
            .lines()
            .filter_map(|line| line.strip_prefix("UNAVAILABLE "))
            .collect();
        let omission = match omitted.as_slice() {
            [] => None,
            [why]
                if case.tethered
                    && why.starts_with("phase=warm source=require_tether arch=")
                    && case.warm =>
            {
                if text.lines().any(|line| {
                    [
                        "NULL ", "FILE ", "BEFORE ", "AFTER ", "CHILD ", "DIRECT ", "READ ",
                    ]
                    .iter()
                    .any(|prefix| line.starts_with(prefix))
                }) {
                    return Err("warm refusal contradicts descriptor observation evidence".into());
                }
                return Ok(Some((*why).to_string()));
            }
            [why]
                if case.tethered
                    && why.starts_with("phase=observer source=require_tether arch=") =>
            {
                Some(*why)
            }
            _ => return Err(format!("invalid required-tether refusal: {omitted:?}")),
        };
        let (null, regular) = assess_parent(case, text)?;
        if let Some(why) = omission {
            if text.lines().any(|line| {
                ["CHILD ", "DIRECT ", "READ "]
                    .iter()
                    .any(|prefix| line.starts_with(prefix))
            }) {
                return Err("observer refusal contradicts child observation evidence".into());
            }
            return Ok(Some(why.to_string()));
        }
        let expected = if case.explicit_null || !case.regular {
            null
        } else {
            regular
        };
        let actual = Identity::parse(one_line(text, "CHILD ")?).ok_or("invalid child identity")?;
        if actual != expected {
            return Err(format!("child identity {actual:?}, expected {expected:?}"));
        }
        Ok(None)
    }

    fn classifier_controls() {
        let case = Case {
            fd: 0,
            explicit_null: false,
            regular: false,
            tethered: true,
            warm: false,
        };
        let null = Identity {
            mode: libc::S_IFCHR | 0o666,
            rdev: 259,
            dev: 1,
            ino: 3,
        };
        let regular = Identity {
            mode: libc::S_IFREG | 0o600,
            rdev: 0,
            dev: 1,
            ino: 4,
        };
        let references = format!("NULL {}\nFILE {}\n", null.text(), regular.text());
        let parent = format!("{references}BEFORE closed EBADF\nAFTER closed EBADF\n");
        let refusal = "UNAVAILABLE phase=observer source=require_tether arch=x86_64: unsupported\n";
        assert!(
            assess(case, &format!("{parent}{refusal}"))
                .unwrap()
                .is_some()
        );
        let bad_parent = format!(
            "{references}BEFORE open {} flags=0\nAFTER open {} flags=0\n{refusal}",
            regular.text(),
            regular.text()
        );
        assert!(
            assess(case, &bad_parent).is_err(),
            "an observed bad slot must fail even when spawn is unsupported"
        );
        let changed_parent = format!(
            "{references}BEFORE closed EBADF\nAFTER open {} flags=0\n{refusal}",
            regular.text()
        );
        assert!(
            assess(case, &changed_parent).is_err(),
            "a refused spawn must not hide parent descriptor mutation"
        );
        assert!(assess(case, &format!("{references}BEFORE closed EBADF\n{refusal}")).is_err());
        assert!(
            assess(
                case,
                &format!("{parent}CHILD {}\n{refusal}", regular.text())
            )
            .is_err()
        );
        let warm = "UNAVAILABLE phase=warm source=require_tether arch=x86_64: unsupported\n";
        assert!(assess(Case { warm: true, ..case }, warm).unwrap().is_some());
        assert!(assess(case, warm).is_err());
        assert!(assess(Case { warm: true, ..case }, &format!("{parent}{warm}")).is_err());
        assert!(
            assess(
                Case {
                    tethered: false,
                    ..case
                },
                &format!("{parent}{refusal}")
            )
            .is_err()
        );
        assert!(
            assess(case, &format!("{parent}CHILD {}\n", null.text()))
                .unwrap()
                .is_none()
        );
    }

    if let Some(code) = aterm_uds::tether::entry() {
        std::process::exit(if code == std::process::ExitCode::SUCCESS {
            0
        } else {
            1
        });
    }
    if let Ok(role) = std::env::var(ROLE) {
        let case = Case::parse(&role);
        let null = Identity::of(&std::fs::metadata("/dev/null").expect("stat /dev/null"));
        assert_eq!(null.mode & libc::S_IFMT, libc::S_IFCHR);
        let scratch = aterm_tempfile::tempdir().expect("a private fixture directory");
        let regular_path = scratch.path().join("inherited");
        let file = std::fs::File::create(&regular_path).expect("a regular inherited-file control");
        assert!(file.as_raw_fd() > 2, "the control is above stdio");
        let regular = Identity::of(&file.metadata().expect("stat control"));
        if case.warm {
            let mut warm = command("/usr/bin/true");
            warm.stdin(Stdio::null())
                .stdout(Stdio::null())
                .stderr(Stdio::null());
            case.route(&mut warm);
            let Some(status) = started(case, "warm", warm.status()) else {
                return;
            };
            assert!(status.success(), "warm preparation");
        }
        if case.regular {
            // SAFETY: this role exclusively owns the selected stdio; nothing else uses it.
            assert_eq!(unsafe { libc::dup2(file.as_raw_fd(), case.fd) }, case.fd);
        } else {
            // SAFETY: nothing in this fresh role uses its selected stdin or stderr.
            assert_eq!(unsafe { libc::close(case.fd) }, 0, "close {}", case.fd);
        }
        println!("NULL {}", null.text());
        println!("FILE {}", regular.text());
        println!("BEFORE {}", slot(case.fd));
        std::io::stdout()
            .flush()
            .expect("publish pre-spawn evidence");

        let mut child = command("/usr/bin/stat");
        child.args([
            "-L",
            "-c",
            "CHILD %f %r %d %i",
            &format!("/proc/self/fd/{}", case.fd),
        ]);
        let source = || {
            if case.explicit_null {
                Stdio::null()
            } else {
                Stdio::inherit()
            }
        };
        child
            .stdin(if case.fd == 0 {
                source()
            } else {
                Stdio::null()
            })
            .stderr(if case.fd == 2 {
                source()
            } else {
                Stdio::inherit()
            })
            .stdout(Stdio::piped());
        case.route(&mut child);
        // The outer owner supplies a one-minute inactivity detector and complete bounded capture.
        // A fresh piped stdout exercises the same descriptor preparation as the original
        // closed-stdio failures. Only this inner command chooses the examined stdin/stderr.
        let Some(mut child) = started(case, "observer", child.spawn()) else {
            println!("AFTER {}", slot(case.fd));
            return;
        };
        let mut said = Vec::new();
        let read = child
            .stdout
            .take()
            .expect("piped stdout")
            .take(4097)
            .read_to_end(&mut said);
        let status = child.wait().expect("wait");
        println!("AFTER {}", slot(case.fd));
        println!("DIRECT {status:?}");
        std::io::stdout()
            .write_all(&said)
            .expect("retain direct observer output");
        println!("READ {read:?}");
        read.expect("read direct observer output");
        assert!(
            said.len() <= 4096,
            "direct observer output exceeds 4096 bytes"
        );
        assert!(status.success(), "the direct observer ran");
        return;
    }

    let exe = std::env::current_exe().expect("this fixture's executable");
    let selected = std::env::var(PICK).ok().map(|text| Case::parse(&text));
    let mut observations = Vec::new();
    for fd in [0, 2] {
        for explicit_null in [false, true] {
            for regular in [false, true] {
                for tethered in [false, true] {
                    for warm in [false, true] {
                        let case = Case {
                            fd,
                            explicit_null,
                            regular,
                            tethered,
                            warm,
                        };
                        if selected.is_some_and(|picked| picked != case) {
                            continue;
                        }
                        let output = aterm_uds::deadline::output_while_process_progressing_complete_with_limit(
                            command(&exe).env(ROLE, case.name()),
                            MUST_HAPPEN,
                            format!("stdio identity role {}", case.name()),
                            64 * 1024,
                        );
                        println!("CASE {} {output:?}", case.name());
                        observations.push((case, output));
                    }
                }
            }
        }
    }
    let count = observations.len();
    assert_eq!(count, if selected.is_some() { 1 } else { 32 });
    classifier_controls();
    // All returned observations precede verdict assertions: complete output on success,
    // explicit collector errors when a role's output or execution cannot be captured.
    let mut failures = Vec::new();
    let mut unavailable = Vec::new();
    for (case, output) in observations {
        let check = || -> Result<Option<String>, String> {
            let output = output.map_err(|error| error.to_string())?;
            if !output.status.success() {
                return Err(format!("role exited {:?}", output.status));
            }
            let text = std::str::from_utf8(&output.stdout).map_err(|error| error.to_string())?;
            assess(case, text)
        };
        match check() {
            Ok(Some(why)) => unavailable.push((case, why)),
            Ok(None) => {}
            Err(error) => failures.push(format!("{}: {error}", case.name())),
        }
    }
    let translator = aterm_uds::process::translator();
    for (case, why) in &unavailable {
        match &translator {
            Some(translator) => println!(
                "NOT RUN under {translator}: tether_closed_stdio::{}: {why}",
                case.name()
            ),
            None => println!("NOT RUN: tether_closed_stdio::{}: {why}", case.name()),
        }
    }
    // A measured identity failure is never reduced to unavailable by another case's refusal.
    assert!(failures.is_empty(), "{}", failures.join("\n"));
    if !unavailable.is_empty() && translator.is_none() {
        std::process::exit(3);
    }
    println!(
        "stdio identity: {} passed, {} unavailable of {count} closed/open, inherited/null, tethered/plain, fresh/warm cases",
        count - unavailable.len(),
        unavailable.len(),
    );
}

#[cfg(not(target_os = "linux"))]
fn main() {
    println!("tether closed stdio: Linux's spawn not run on this target");
}
