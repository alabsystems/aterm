// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Establish a process fixture's image before reading its arguments or environment.

use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

/// Wait under a one-minute hang detector for `pid` to exec `image`. Both the kernel's exec
/// flag and its executable path must agree: a fork still maps its parent's image, and an
/// intermediate `env` may have exec'd before the intended program has. The process's start
/// record ([`crate::process::read_process_start`], which Linux and macOS both keep) must stay the
/// same throughout the wait, and its thread must have run before an argument reader can ask the
/// kernel for that image's initial environment.
///
/// This establishes kernel image identity only. A copied Mach-O must also prove it ran by
/// output ([`super::prints`], [`super::cat_echoes`]) before its file can be removed.
/// # Panics
/// `image` cannot be canonicalized, `pid`'s start cannot be read or changes, or the expected
/// exec and its running thread do not become readable within a minute.
#[track_caller]
pub fn execd(pid: i32, image: &Path) {
    wait_execd(pid, image, || {});
}

#[track_caller]
fn wait_execd(pid: i32, image: &Path, mut waiting: impl FnMut()) {
    let image = std::fs::canonicalize(image).expect("the fixture's image");
    let start = crate::process::read_process_start(pid).expect("the fixture's start");
    let give_up = Instant::now() + Duration::from_secs(60);
    loop {
        let ran = names_image(
            crate::process::read_process_execd(pid),
            crate::process::read_process_path(pid),
            &image,
        ) && crate::process::thread_ran(pid) == Some(true);
        let now = crate::process::read_process_start(pid);
        assert!(
            now.is_none_or(|now| now == start),
            "pid {pid} no longer names the fixture"
        );
        if ran && now == Some(start) {
            return;
        }
        assert!(
            Instant::now() < give_up,
            "pid {pid} never exec'd {}",
            image.display()
        );
        waiting();
        super::poll_yield();
    }
}

fn names_image(execd: Option<bool>, path: Option<PathBuf>, image: &Path) -> bool {
    execd == Some(true) && path.as_deref() == Some(image)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::{BufRead as _, Write as _};

    #[test]
    fn an_exec_flag_and_the_expected_image_are_both_required() {
        let image = Path::new("/fixture/sleep");
        assert!(names_image(Some(true), Some(image.into()), image));
        for flag in [None, Some(false)] {
            assert!(!names_image(flag, Some(image.into()), image));
        }
        for path in [None, Some("/fixture/env".into())] {
            assert!(!names_image(Some(true), path, image));
        }
    }

    /// The pipe holds an adopted background fork before exec, with no scheduling or latency
    /// assumption. The old adopted-pid fixture accepts it while it still maps the shell;
    /// the shared wait releases it only after observing that state, and waits for sleep.
    #[test]
    fn a_shell_pid_line_does_not_prove_its_background_child_execd() {
        let shell = super::super::stand_in(Path::new("/bin/sh"));
        let sleep = super::super::stand_in(Path::new("/bin/sleep"));
        let env = super::super::stand_in(Path::new("/usr/bin/env"));
        struct Owned(crate::spawnfd::Child);
        impl Drop for Owned {
            fn drop(&mut self) {
                let _ = crate::process::kill_group_until_empty(self.0.id() as i32);
                let _ = self.0.wait();
            }
        }
        let mut owned = Owned(
            crate::spawnfd::command(&shell)
                .args([
                    "-c",
                    "exec 3<&0; (read -r go <&3; exec \"$2\" -i \"$1\" 600) & echo $!; exit 0",
                    "fixture",
                ])
                .arg(&sleep)
                .arg(&env)
                .env("ATERM_FIXTURE_BEFORE_EXEC", "inherited")
                .process_group(0)
                .stdin(crate::spawnfd::Stdio::piped())
                .stdout(crate::spawnfd::Stdio::piped())
                .spawn()
                .expect("the gated fixture"),
        );
        let out = owned.0.stdout.take().expect("stdout");
        let (tx, rx) = std::sync::mpsc::channel();
        let reader = std::thread::spawn(move || {
            let mut line = String::new();
            std::io::BufReader::new(out)
                .read_line(&mut line)
                .expect("pid line");
            tx.send(line).expect("the pid receiver");
        });
        let pid: i32 = rx
            .recv_timeout(Duration::from_secs(60))
            .expect("pid line hang detector")
            .trim()
            .parse()
            .expect("pid");
        reader.join().expect("pid reader");
        let give_up = Instant::now() + Duration::from_secs(60);
        while !crate::process::read_process_parent(pid).is_some_and(|parent| parent != owned.0.id())
        {
            assert!(
                Instant::now() < give_up,
                "the gated orphan was never adopted"
            );
            super::super::poll_yield();
        }
        let expected = std::fs::canonicalize(&sleep).expect("sleep");
        let before = crate::process::read_process_path(pid);
        assert_eq!(crate::process::read_process_execd(pid), Some(false));
        assert_ne!(
            before.as_deref(),
            Some(expected.as_path()),
            "negative control: the old immediate image assertion fails"
        );
        let mut released = false;
        wait_execd(pid, &sleep, || {
            if !released {
                owned
                    .0
                    .stdin
                    .as_mut()
                    .expect("gate")
                    .write_all(b"go\n")
                    .expect("release exec");
                released = true;
            }
        });
        assert!(released, "the helper must wait on the pre-exec fork");
        assert_eq!(crate::process::read_process_path(pid), Some(expected));
        assert_eq!(
            marked(pid),
            Some(false),
            "the final image has the scrubbed environment"
        );
        execd(pid, &sleep);
        eprintln!("gated adopted orphan: immediate image fixture 0/1; shared helper 1/1");
    }

    fn marked(pid: i32) -> Option<bool> {
        const MARK: &[u8] = b"ATERM_FIXTURE_BEFORE_EXEC=inherited";
        #[cfg(target_vendor = "apple")]
        {
            let bytes = crate::procargs::read(pid as u32)?;
            Some(
                crate::procargs::parse(&bytes)?
                    .env()
                    .any(|word| word == MARK),
            )
        }
        #[cfg(target_os = "linux")]
        {
            let bytes = std::fs::read(format!("/proc/{pid}/environ")).ok()?;
            Some(bytes.split(|&byte| byte == 0).any(|word| word == MARK))
        }
    }
}
