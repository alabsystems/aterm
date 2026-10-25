// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The Linux lane's port to its guest (`docs/DESIGN-vz-guest-2026-10-23.md` §12): a
//! [`Driver`] in [`crate::linux::Up`] instead of a `Lima`. Its arms are lima's VM (the
//! default until §14's switch), the lane's own VM through `aterm-vz-host`
//! ([`crate::linux_vz_driver::Vz`]), and, in tests and debug builds, a scripted guest
//! (`fake::Guest`).
//!
//! Every lane call site reaches the guest through these six operations: `shell`,
//! `shell_streamed`, `copy_out`, `copy_dir_in`, `copy_dir_out`, and the host disk and
//! version lines `status` and the proof's report print. How a run lets go of the VM is
//! [`Driver::end`]: lima stops a VM this run booted; the vz arm closes its session under
//! §5.1's release rule, `keep` set first for `--keep-up` and `up`.

use std::path::{Path, PathBuf};
use std::sync::OnceLock;

use crate::linux::{Lima, Out};
use crate::{Fail, LAB};

/// Which arm a run uses.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Kind {
    /// lima's `aterm-linux` VM (`limactl`).
    Lima,
    /// The lane's own VM through `aterm-vz-host` and the guest agent.
    Vz,
}

impl Kind {
    /// `lima` or `vz`.
    #[must_use]
    pub fn parse(word: &str) -> Option<Self> {
        match word {
            "lima" => Some(Self::Lima),
            "vz" => Some(Self::Vz),
            _ => None,
        }
    }

    /// Its word.
    #[must_use]
    pub const fn name(self) -> &'static str {
        match self {
            Self::Lima => "lima",
            Self::Vz => "vz",
        }
    }
}

/// The arm `--driver` named, for the rest of this process.
static CHOSEN: OnceLock<Kind> = OnceLock::new();

/// The variable that names the arm.
pub const ENV: &str = "LAB_LINUX_DRIVER";

/// `--driver <word>`, before the verb: this process's arm, over [`ENV`].
///
/// # Errors
/// A word that is no arm's.
pub fn choose(word: &str) -> Result<Kind, Fail> {
    choose_in(&CHOSEN, word)
}

/// [`choose`] into `cell`: the first arm named holds, and a second naming another is a
/// usage error, so `--driver lima vz-smoke` never runs `vz-smoke`'s checks on lima.
///
/// # Errors
/// A word that is no arm's, or another arm than the one `cell` holds.
pub fn choose_in(cell: &OnceLock<Kind>, word: &str) -> Result<Kind, Fail> {
    let kind = Kind::parse(word).ok_or_else(|| {
        Fail::usage(format!(
            "linux --driver {word}: lima or vz ({LAB} linux help)"
        ))
    })?;
    let held = *cell.get_or_init(|| kind);
    if held != kind {
        return Err(Fail::usage(format!(
            "linux: this run's arm is {} (--driver), and this verb runs only the {} arm",
            held.name(),
            kind.name()
        )));
    }
    Ok(kind)
}

/// The arm `env` (the value of [`ENV`]) names: lima when unset or empty.
///
/// # Errors
/// A value that is no arm's.
pub fn kind_of(env: Option<&str>) -> Result<Kind, Fail> {
    match env.filter(|v| !v.is_empty()) {
        None => Ok(Kind::Lima),
        Some(word) => Kind::parse(word).ok_or_else(|| {
            Fail::usage(format!(
                "{ENV}={word}: lima or vz (lima is the default until the switch, §14)"
            ))
        }),
    }
}

/// This run's arm: `--driver`, else [`ENV`], else lima.
///
/// # Errors
/// [`ENV`] names no arm.
pub fn kind() -> Result<Kind, Fail> {
    if let Some(k) = CHOSEN.get() {
        return Ok(*k);
    }
    kind_of(std::env::var(ENV).ok().as_deref())
}

/// Whether lima's run end stops the VM: only one this run booted, and only without `keep`
/// (`--keep-up`, `up`), so a VM another run booted, or one kept up, runs on.
#[must_use]
pub const fn stops_vm(booted_here: bool, keep: bool) -> bool {
    booted_here && !keep
}

/// The VM's disk on this Mac, as `status` and the proof's report say it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct HostDisk {
    /// The bytes it really holds (a sparse file: what the guest wrote and did not trim).
    pub bytes: u64,
    /// Where it is.
    pub path: PathBuf,
}

/// The guest, as the lane reaches it.
pub enum Driver {
    /// lima's VM.
    Lima(Lima),
    /// The lane's own VM, through its helper's session.
    Vz(Box<crate::linux_vz_driver::Vz>),
    /// A scripted guest, for the lane's own tests.
    #[cfg(test)]
    Fake(fake::Guest),
}

impl std::fmt::Debug for Driver {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(match self {
            Self::Lima(_) => "Driver::Lima",
            Self::Vz(_) => "Driver::Vz",
            #[cfg(test)]
            Self::Fake(_) => "Driver::Fake",
        })
    }
}

impl Driver {
    /// The arm.
    #[must_use]
    pub const fn kind(&self) -> Kind {
        match self {
            Self::Lima(_) => Kind::Lima,
            Self::Vz(_) => Kind::Vz,
            #[cfg(test)]
            Self::Fake(_) => Kind::Vz,
        }
    }

    /// Run `argv` in the guest from `workdir`, captured; `stdin` is written to it when given.
    #[must_use]
    pub fn shell(&self, workdir: &str, argv: &[String], stdin: Option<&str>) -> Out {
        match self {
            Self::Lima(l) => l.shell(workdir, argv, stdin),
            Self::Vz(v) => v.shell(workdir, argv, stdin),
            #[cfg(test)]
            Self::Fake(g) => g.shell(workdir, argv, stdin),
        }
    }

    /// Run `argv` in the guest with its stderr merged into its stdout there, each line shown
    /// here as it comes, prefixed, and kept.
    #[must_use]
    pub fn shell_streamed(&self, workdir: &str, argv: &[String], prefix: &str) -> Out {
        match self {
            Self::Lima(l) => l.shell_streamed(workdir, argv, prefix),
            Self::Vz(v) => v.shell_streamed(workdir, argv, prefix),
            #[cfg(test)]
            Self::Fake(g) => g.shell_streamed(workdir, argv, prefix),
        }
    }

    /// Copy a guest file to this Mac.
    #[must_use]
    pub fn copy_out(&self, guest_path: &str, host_path: &Path) -> Out {
        match self {
            Self::Lima(l) => l.copy_out(guest_path, host_path),
            Self::Vz(v) => v.copy_out(guest_path, host_path),
            #[cfg(test)]
            Self::Fake(g) => g.copy_out(guest_path, host_path),
        }
    }

    /// Copy the directory `host_dir` of this Mac into the guest directory `guest_parent`,
    /// where it lands under its own name.
    #[must_use]
    pub fn copy_dir_in(&self, host_dir: &Path, guest_parent: &str) -> Out {
        match self {
            Self::Lima(l) => l.copy_dir_in(host_dir, guest_parent),
            Self::Vz(v) => v.copy_dir_in(host_dir, guest_parent),
            #[cfg(test)]
            Self::Fake(g) => g.copy_dir_in(host_dir, guest_parent),
        }
    }

    /// Copy the guest directory `guest_dir` into `host_parent` on this Mac, where it lands
    /// under its own name.
    #[must_use]
    pub fn copy_dir_out(&self, guest_dir: &str, host_parent: &Path) -> Out {
        match self {
            Self::Lima(l) => l.copy_dir_out(guest_dir, host_parent),
            Self::Vz(v) => v.copy_dir_out(guest_dir, host_parent),
            #[cfg(test)]
            Self::Fake(g) => g.copy_dir_out(guest_dir, host_parent),
        }
    }

    /// The VM's disk on this Mac, where it reads.
    #[must_use]
    pub fn host_disk(&self) -> Option<HostDisk> {
        match self {
            Self::Lima(l) => {
                let vm = l.state().ok()??;
                let bytes = crate::linux::host_disk_bytes(&vm)?;
                Some(HostDisk {
                    bytes,
                    path: vm.dir,
                })
            }
            Self::Vz(v) => v.host_disk(),
            #[cfg(test)]
            Self::Fake(g) => g.host_disk.clone(),
        }
    }

    /// The driver's own line: `limactl --version`, or the helper, image and agent.
    #[must_use]
    pub fn version(&self) -> String {
        match self {
            Self::Lima(l) => l.version(),
            Self::Vz(v) => v.version(),
            #[cfg(test)]
            Self::Fake(_) => "fake guest".into(),
        }
    }

    /// Let go of the VM at a run's end: with `keep`, it stays up (the vz arm sets `keep`
    /// on its session first); else lima stops a VM this run booted (`booted_here`), and
    /// the vz arm's session closes under the release rule, which powers off a VM nobody
    /// else holds and nobody kept, and waits for its helper to end. `false` only when lima's
    /// stop of a VM this run booted failed: the VM may still run.
    pub fn end(self, booted_here: bool, keep: bool) -> bool {
        match self {
            Self::Lima(l) => {
                if !stops_vm(booted_here, keep) {
                    return true;
                }
                let out = l.stop();
                if out.ok() {
                    println!("linux: VM stopped (this run booted it)");
                } else {
                    eprintln!("aterm-lab: linux: limactl stop failed: {}", out.tail(5));
                }
                out.ok()
            }
            Self::Vz(v) => {
                v.end(booted_here, keep);
                true
            }
            #[cfg(test)]
            Self::Fake(g) => {
                g.end(booted_here, keep);
                true
            }
        }
    }
}

/// The scripted guest: each argv's answer, and every call recorded.
#[cfg(test)]
pub mod fake {
    use std::collections::BTreeMap;
    use std::path::Path;
    use std::sync::{Arc, Mutex};

    use super::HostDisk;
    use crate::linux::Out;

    /// One call the lane made.
    #[derive(Clone, Debug, PartialEq, Eq)]
    pub enum Call {
        /// `shell` (with its stdin) or `shell_streamed` (`merged`).
        Shell {
            /// The argv.
            argv: Vec<String>,
            /// What was written to its stdin.
            stdin: Option<String>,
            /// Whether it was `shell_streamed`.
            merged: bool,
        },
        /// `copy_out`.
        CopyOut(String),
        /// `copy_dir_in`.
        CopyDirIn(String),
        /// `copy_dir_out`.
        CopyDirOut(String),
        /// `end`, with `booted_here` and `keep`.
        End {
            /// Whether the run booted the VM.
            booted_here: bool,
            /// Whether it is kept up.
            keep: bool,
        },
    }

    /// An answer: an exit code, or one of the agent's other three ends.
    #[derive(Clone, Debug, PartialEq, Eq)]
    pub enum Answer {
        /// `Exited(code)`, saying `text`.
        Exited(i32, String),
        /// `Signaled(signal)`.
        Signaled(i32),
        /// `NotStarted(reason)`.
        NotStarted(String),
        /// `Lost(why)`.
        Lost(String),
    }

    /// A scripted guest: the first rule whose words all appear in a call's argv answers it;
    /// anything else exits 0, saying nothing.
    #[derive(Clone, Debug, Default)]
    pub struct Guest {
        rules: Vec<(Vec<String>, Answer)>,
        /// Every call, in order.
        pub calls: Arc<Mutex<Vec<Call>>>,
        /// What `host_disk` says.
        pub host_disk: Option<HostDisk>,
        /// Files `copy_out` writes, by guest path.
        pub files: BTreeMap<String, Vec<u8>>,
    }

    impl Guest {
        /// A guest whose every command exits 0.
        #[must_use]
        pub fn new() -> Self {
            Self::default()
        }

        /// Answer a call whose argv holds every one of `words` with `answer`.
        #[must_use]
        pub fn answer(mut self, words: &[&str], answer: Answer) -> Self {
            self.rules
                .push((words.iter().map(|w| (*w).to_string()).collect(), answer));
            self
        }

        /// The calls so far.
        #[must_use]
        pub fn calls(&self) -> Vec<Call> {
            self.calls
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .clone()
        }

        fn record(&self, call: Call) {
            self.calls
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .push(call);
        }

        fn answer_for(&self, argv: &[String]) -> Out {
            let answer = self
                .rules
                .iter()
                .find(|(words, _)| words.iter().all(|w| argv.contains(w)))
                .map_or(Answer::Exited(0, String::new()), |(_, a)| a.clone());
            let mut out = Out {
                code: None,
                text: String::new(),
                secs: 0.0,
                signal: None,
                not_started: None,
                lost: None,
            };
            match answer {
                Answer::Exited(code, text) => {
                    out.code = Some(code);
                    out.text = text;
                }
                Answer::Signaled(signal) => {
                    out.signal = Some(signal);
                    out.text = crate::linux_vz_driver::signal_words(signal);
                }
                Answer::NotStarted(why) => out.not_started = Some(why),
                Answer::Lost(why) => out.lost = Some(why),
            }
            out
        }

        /// As [`super::Driver::shell`].
        #[must_use]
        pub fn shell(&self, _workdir: &str, argv: &[String], stdin: Option<&str>) -> Out {
            self.record(Call::Shell {
                argv: argv.to_vec(),
                stdin: stdin.map(str::to_string),
                merged: false,
            });
            self.answer_for(argv)
        }

        /// As [`super::Driver::shell_streamed`].
        #[must_use]
        pub fn shell_streamed(&self, _workdir: &str, argv: &[String], _prefix: &str) -> Out {
            self.record(Call::Shell {
                argv: argv.to_vec(),
                stdin: None,
                merged: true,
            });
            self.answer_for(argv)
        }

        /// As [`super::Driver::copy_out`]: writes the scripted file.
        #[must_use]
        pub fn copy_out(&self, guest_path: &str, host_path: &Path) -> Out {
            self.record(Call::CopyOut(guest_path.into()));
            let mut out = self.answer_for(&[guest_path.to_string()]);
            match self.files.get(guest_path) {
                Some(bytes) => {
                    if let Err(e) = std::fs::write(host_path, bytes) {
                        out.code = Some(1);
                        out.text = e.to_string();
                    }
                }
                None => {
                    out.code = Some(1);
                    out.text = format!("cat: {guest_path}: No such file or directory\n");
                }
            }
            out
        }

        /// As [`super::Driver::copy_dir_in`].
        #[must_use]
        pub fn copy_dir_in(&self, host_dir: &Path, _guest_parent: &str) -> Out {
            self.record(Call::CopyDirIn(host_dir.display().to_string()));
            self.answer_for(&[host_dir.display().to_string()])
        }

        /// As [`super::Driver::copy_dir_out`].
        #[must_use]
        pub fn copy_dir_out(&self, guest_dir: &str, _host_parent: &Path) -> Out {
            self.record(Call::CopyDirOut(guest_dir.into()));
            self.answer_for(&[guest_dir.to_string()])
        }

        /// As [`super::Driver::end`].
        pub fn end(&self, booted_here: bool, keep: bool) {
            self.record(Call::End { booted_here, keep });
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_arm_is_lima_unless_the_variable_names_vz() {
        assert_eq!(kind_of(None).unwrap(), Kind::Lima);
        assert_eq!(kind_of(Some("")).unwrap(), Kind::Lima);
        assert_eq!(kind_of(Some("lima")).unwrap(), Kind::Lima);
        assert_eq!(kind_of(Some("vz")).unwrap(), Kind::Vz);
        let fail = kind_of(Some("qemu")).unwrap_err();
        assert!(
            fail.msg
                .as_deref()
                .is_some_and(|m| m.starts_with("LAB_LINUX_DRIVER=qemu: lima or vz")),
            "{:?}",
            fail.msg
        );
        assert_eq!(Kind::Vz.name(), "vz");
        assert_eq!(Kind::parse(Kind::Lima.name()), Some(Kind::Lima));
    }

    #[test]
    fn a_verb_that_runs_one_arm_refuses_a_run_that_chose_the_other() {
        let cell = OnceLock::new();
        assert_eq!(choose_in(&cell, "lima").unwrap(), Kind::Lima);
        // The negative control: the same arm named again holds.
        assert_eq!(choose_in(&cell, "lima").unwrap(), Kind::Lima);
        let fail = choose_in(&cell, "vz").unwrap_err();
        assert_eq!(fail.code, 2, "a usage error");
        assert!(
            fail.msg
                .as_deref()
                .is_some_and(|m| m.contains("this run's arm is lima")),
            "{:?}",
            fail.msg
        );
        assert_eq!(cell.get(), Some(&Kind::Lima), "the first arm holds");
        let fresh = OnceLock::new();
        assert_eq!(choose_in(&fresh, "vz").unwrap(), Kind::Vz);
        assert!(choose_in(&fresh, "qemu").is_err());
    }

    #[test]
    fn lima_stops_only_a_vm_this_run_booted_and_did_not_keep() {
        assert!(stops_vm(true, false));
        assert!(!stops_vm(true, true), "--keep-up and up keep it");
        assert!(!stops_vm(false, false), "another run's VM runs on");
        assert!(!stops_vm(false, true));
    }
}
