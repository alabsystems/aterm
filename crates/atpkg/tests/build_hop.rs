// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

//! BUILD-ADMIT's shim hop through the real `atpkg` binary (`atpkg __build_admit --shim …`): a
//! claimed build runs IN THE FRONT'S PID under a claim its holder keeps, a call that claims
//! nothing becomes its program at once, and a holder that does not arm leaves the build under
//! the front with its claim. Each run claims in a scratch ledger (`LEDGER_ENV`, a debug build's,
//! which admits at once) under a scratch `HOME` and state root, on a stand-in `targo` that says
//! what it finds, may leave a child of its own running (`HOP_CHILD`: an idle shell, or a working
//! one that holds memory), waits for a line, and says once more whether its claim is held.

#![cfg(target_os = "macos")]

use std::io::Write as _;
use std::path::{Path, PathBuf};

use aterm_spec::wait::{MUST_HAPPEN, until_within};

/// What the stand-in found at its start, one `name value` line each, and at its end (`$HOP_OUT.end`:
/// the claim file in its pid, or `none`), then, with `HOP_PROBE=signals`, sends itself SIGPIPE and
/// SIGINT and says `survived` (`$HOP_OUT.signals`) where it lived. `HOP_CHILD=idle` leaves a shell waiting for a line on
/// the stand-in's stdin, `HOP_CHILD=heavy` the suite's [`standin_touches_and_waits`]; either is
/// named on the `child` line.
const STANDIN: &str = r#"#!/bin/sh
held=none
for f in "$HOP_LEDGER"/*-"$$"-*.claim; do [ -e "$f" ] && held=$f; done
fds=
if [ "$held" != none ]; then
  for fd in 0 1 2 3 4 5 6 7 8 9 10 11 12 13 14 15 16 17 18 19 20; do
    [ /dev/fd/$fd -ef "$held" ] && fds="$fds $fd"
  done
fi
child=none
case "${HOP_CHILD-}" in
  idle) exec 3<&0; sh -c 'read x' <&3 >/dev/null 2>&1 & child=$! ;;
  heavy) exec 3<&0; "$HOP_SUITE" --exact build_hop::standin_touches_and_waits --test-threads=1 \
    --nocapture <&3 >/dev/null 2>&1 & child=$! ;;
esac
{
  echo "pid $$"
  echo "ppid $PPID"
  echo "held $held"
  echo "fds$fds"
  echo "cover ${ATERM_CAPACITY_COVER-unset}"
  echo "child $child"
  echo "args $*"
} > "$HOP_OUT.tmp" && mv "$HOP_OUT.tmp" "$HOP_OUT"
read line
end=none
for f in "$HOP_LEDGER"/*-"$$"-*.claim; do [ -e "$f" ] && end=$f; done
echo "$end" > "$HOP_OUT.end.tmp" && mv "$HOP_OUT.end.tmp" "$HOP_OUT.end"
if [ "${HOP_PROBE-}" = signals ]; then
  kill -PIPE $$
  kill -INT $$
  echo survived > "$HOP_OUT.signals"
fi
exit "${HOP_EXIT:-0}"
"#;

/// Set, the suite is [`standin_touches_and_waits`]: the MiB it touches.
const TOUCH: &str = "ATPKG_HOP_STANDIN_TOUCH";

/// Where [`standin_touches_and_waits`] says it touched its memory.
const TOUCHED: &str = "ATPKG_HOP_STANDIN_TOUCHED";

/// A recorded peak beyond the host's capacity, never allocated by a stand-in. Four PiB
/// remains within the signed and exact JSON integer range while forcing the ALONE premise.
const ALONE_BOOK_PEAK: u64 = 1_u64 << 52;

/// The working child a killed build leaves (`HOP_CHILD=heavy`): touches the MiB [`TOUCH`] names,
/// says so in the file [`TOUCHED`] names, and holds them until its stdin ends. Run alone, it does
/// nothing.
#[test]
fn standin_touches_and_waits() {
    use std::io::Read as _;
    let Some(mib) = std::env::var(TOUCH)
        .ok()
        .and_then(|v| v.parse::<usize>().ok())
    else {
        return;
    };
    let held = vec![1u8; mib << 20];
    if let Some(said) = std::env::var_os(TOUCHED) {
        let _ = std::fs::write(said, "touched\n");
    }
    let mut rest = Vec::new();
    let _ = std::io::stdin().read_to_end(&mut rest);
    assert_eq!(held.len(), mib << 20);
}

struct Scratch {
    dir: aterm_tempfile::TempDir,
}

impl Scratch {
    fn new() -> Self {
        let dir = aterm_tempfile::Builder::new()
            .prefix("atpkg-build-hop-")
            .tempdir()
            .expect("scratch");
        let s = Self { dir };
        std::fs::create_dir_all(s.home()).expect("home");
        std::fs::create_dir_all(s.ledger()).expect("ledger");
        let targo = s.targo();
        std::fs::write(&targo, STANDIN).expect("the stand-in");
        std::fs::set_permissions(&targo, std::os::unix::fs::PermissionsExt::from_mode(0o755))
            .expect("runnable");
        s
    }

    fn root(&self) -> PathBuf {
        self.dir.path().canonicalize().expect("the scratch's path")
    }

    fn home(&self) -> PathBuf {
        self.root().join("home")
    }

    fn ledger(&self) -> PathBuf {
        self.root().join("ledger")
    }

    fn state(&self) -> PathBuf {
        self.root().join("state")
    }

    fn targo(&self) -> PathBuf {
        self.root().join("targo")
    }

    fn out(&self) -> PathBuf {
        self.root().join("seen")
    }

    /// Where the holder notes its meter's reads ([`atpkg::build_admit::hold::READS_ENV`]).
    fn reads(&self) -> PathBuf {
        self.root().join("reads")
    }

    fn trace(&self) -> PathBuf {
        self.root().join("holder-trace.json")
    }

    fn trace_value(&self) -> Option<aterm_json::Value> {
        aterm_json::from_slice(&std::fs::read(self.trace()).ok()?).ok()
    }

    fn write_evidence(&self, name: &str, value: &aterm_json::Value) -> std::io::Result<()> {
        use std::os::unix::fs::OpenOptionsExt as _;
        let bytes = aterm_json::to_vec(value).map_err(std::io::Error::other)?;
        let mut file = std::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .mode(0o600)
            .open(self.root().join(name))?;
        file.write_all(&bytes)
    }

    fn latest_meter(&self) -> Option<aterm_json::Value> {
        self.trace_value()?
            .get("events")?
            .as_array()?
            .iter()
            .rev()
            .find(|event| event.get("kind").and_then(aterm_json::Value::as_str) == Some("meter"))
            .cloned()
    }

    /// Once the holder's meter has read `pid` descended from the front (a line of
    /// [`Self::reads`] names it).
    fn read_in_tree(&self, pid: u32) {
        let pid = pid.to_string();
        until_within(MUST_HAPPEN, "the holder's read of the child", || {
            let text = std::fs::read_to_string(self.reads()).ok()?;
            text.lines()
                .any(|l| l.split(' ').skip(1).any(|p| p == pid))
                .then_some(())
        });
    }

    /// What the stand-in found at its end: the claim file in its pid, or none.
    fn held_at_end(&self) -> Option<PathBuf> {
        let mut end = self.out().into_os_string();
        end.push(".end");
        let text = until_within(MUST_HAPPEN, "the stand-in's last report", || {
            std::fs::read_to_string(&end).ok()
        });
        Some(text.trim().to_owned())
            .filter(|t| t != "none")
            .map(PathBuf::from)
    }

    /// The front on `args`, its stand-in told by `env`, started with its stdin piped.
    fn front(&self, args: &[&str], env: &[(&str, &str)]) -> aterm_uds::spawnfd::Child {
        self.front_ignoring(None, args, env)
    }

    /// [`Self::front`], started by a shell that ignores `ignored` (`trap '' <ignored>`) and execs
    /// it, where one is named.
    fn front_ignoring(
        &self,
        ignored: Option<&str>,
        args: &[&str],
        env: &[(&str, &str)],
    ) -> aterm_uds::spawnfd::Child {
        self.front_command(ignored, args, env)
            .spawn()
            .expect("the front")
    }

    /// [`Self::front`], started in a session of its own (`setsid`), as aterm starts each tab's
    /// shell: none of its ancestors is in its session.
    fn front_in_its_own_session(
        &self,
        args: &[&str],
        env: &[(&str, &str)],
    ) -> aterm_uds::spawnfd::Child {
        self.front_command(None, args, env)
            .setsid(true)
            .spawn()
            .expect("the front")
    }

    /// The command [`Self::front_ignoring`] spawns.
    fn front_command(
        &self,
        ignored: Option<&str>,
        args: &[&str],
        env: &[(&str, &str)],
    ) -> aterm_uds::spawnfd::Command {
        let atpkg = aterm_tempfile::bin_exe("atpkg");
        let mut cmd = match ignored {
            None => aterm_uds::spawnfd::command(&atpkg),
            Some(ignored) => {
                let mut sh = aterm_uds::spawnfd::command("/bin/sh");
                sh.args(["-c", &format!("trap '' {ignored}; exec \"$0\" \"$@\"")])
                    .arg(&atpkg);
                sh
            }
        };
        cmd.args(["__build_admit", "--shim", "targo", "-"])
            .arg(self.targo())
            .args(args)
            .env_clear()
            .env("PATH", "/usr/bin:/bin")
            .env("HOME", self.home())
            .env("ATERM_STATE_HOME", self.state())
            .env(atpkg::build_admit::LEDGER_ENV, self.ledger())
            .env("HOP_LEDGER", self.ledger())
            .env("HOP_OUT", self.out())
            .env(atpkg::build_admit::hold::READS_ENV, self.reads())
            // A holder that arms may arm late on a loaded machine: the front waits the hang
            // detector for it.
            .env(
                atpkg::build_admit::HOLDER_ARMS_WITHIN_ENV,
                MUST_HAPPEN.as_millis().to_string(),
            )
            .env(
                "HOP_SUITE",
                std::env::current_exe().expect("the suite's own path"),
            )
            .current_dir(self.root())
            .stdin(aterm_uds::spawnfd::Stdio::piped())
            .stdout(aterm_uds::spawnfd::Stdio::null())
            .stderr(aterm_uds::spawnfd::Stdio::piped());
        for (k, v) in env {
            cmd.env(k, v);
        }
        cmd
    }

    /// What the stand-in found, once it said it.
    fn seen(&self) -> Seen {
        let text = until_within(MUST_HAPPEN, "the stand-in's report", || {
            std::fs::read_to_string(self.out()).ok()
        });
        let line = |name: &str| {
            text.lines()
                .find_map(|l| l.strip_prefix(name)?.strip_prefix(' ').or(Some("")))
                .unwrap_or_default()
                .trim()
                .to_owned()
        };
        Seen {
            pid: line("pid").parse().expect("its pid"),
            ppid: line("ppid").parse().expect("its parent"),
            held: Some(line("held"))
                .filter(|h| h != "none")
                .map(PathBuf::from),
            fds: line("fds"),
            cover: line("cover"),
            child: line("child").parse().ok(),
        }
    }

    fn listed(&self) -> Vec<aterm_capacity::Record> {
        aterm_capacity::Ledger::at(self.ledger())
            .list()
            .expect("the scratch ledger")
    }

    fn alone(&self) -> aterm_uds::spawnfd::Child {
        let key = aterm_capacity::book::Key {
            stage: "targo check".to_owned(),
            checkout: self.root().to_string_lossy().into_owned(),
            crates: Some(vec!["big".to_owned()]),
            variant: None,
        };
        std::fs::create_dir_all(self.state()).expect("state");
        aterm_capacity::book::Records::keep_unless_light(
            &self.state().join(aterm_capacity::book::TARGO_RECORD),
            key.clone(),
            ALONE_BOOK_PEAK,
            1,
        )
        .expect("the book");
        let report = self.root().join("alone");
        let mut child = self.front(
            &["--unverified", "check", "-p", "big"],
            &[
                ("ATERM_ADMIT", "now"),
                ("HOP_OUT", report.to_str().expect("UTF-8")),
            ],
        );
        until_within(MUST_HAPPEN, "the unrelated build's report", || {
            if let Some(status) = child.try_wait().expect("the unrelated front's status") {
                let mut err = String::new();
                if let Some(mut stderr) = child.stderr.take() {
                    std::io::Read::read_to_string(&mut stderr, &mut err).expect("front stderr");
                }
                panic!(
                    "unrelated front exited {status}: {err}; key={key:?}; cwd={:?}; claims={:?}",
                    self.root(),
                    self.listed()
                );
            }
            std::fs::metadata(&report).ok()
        });
        let listed = self.listed();
        if !(listed.len() == 1 && listed[0].held.is_some_and(|held| held.alone)) {
            let report = std::fs::read_to_string(&report);
            let (status, err) = finish(child);
            panic!(
                "unrelated build was not ALONE: {listed:?}; report={report:?}; key={key:?}; cwd={:?}; end={status} {err}",
                self.root()
            );
        }
        child
    }
}

/// A fresh successful read of this exact live front and child, at the production threshold.
/// A historical PID, a lifetime peak or an unreadable new sample is not the prerequisite.
fn measured_survivor(
    sample: &aterm_json::Value,
    front: u32,
    serial: u64,
    child: &aterm_capacity::tree_meter::Member,
    after: u64,
) -> bool {
    sample.get("front_pid").and_then(aterm_json::Value::as_u64) == Some(u64::from(front))
        && sample
            .get("front_serial")
            .and_then(aterm_json::Value::as_u64)
            == Some(serial)
        && sample
            .get("sample_sequence")
            .and_then(aterm_json::Value::as_u64)
            .is_some_and(|sequence| sequence > after)
        && sample.get("available").and_then(aterm_json::Value::as_bool) == Some(true)
        && sample
            .get("root_lived")
            .and_then(aterm_json::Value::as_bool)
            == Some(true)
        && sample
            .get("members")
            .and_then(aterm_json::Value::as_array)
            .is_some_and(|members| {
                members.iter().any(|member| {
                    member.get("pid").and_then(aterm_json::Value::as_u64)
                        == Some(u64::from(child.pid))
                        && member.get("start").and_then(aterm_json::Value::as_u64)
                            == Some(child.start)
                        && member.get("eligible").and_then(aterm_json::Value::as_bool) == Some(true)
                        && member
                            .get("footprint")
                            .and_then(aterm_json::Value::as_u64)
                            .is_some_and(|bytes| bytes >= child.footprint)
                })
            })
}

#[test]
fn a_measured_survivor_requires_a_fresh_identity_and_current_footprint() {
    let child = aterm_capacity::tree_meter::Member {
        pid: 42,
        start: 7,
        footprint: atpkg::build_admit::hold::SURVIVORS_IDLE_UNDER,
        peak: 0,
    };
    let good = aterm_json::json!({
        "front_pid": 41, "front_serial": 6, "sample_sequence": 2,
        "available": true, "root_lived": true,
        "members": [{"pid": 42, "start": 7, "eligible": true,
            "footprint": child.footprint, "peak": 1_u64 << 40}],
    });
    assert!(measured_survivor(&good, 41, 6, &child, 1));
    assert!(
        !measured_survivor(&good, 41, 6, &child, 2),
        "not a new read"
    );
    assert!(!measured_survivor(&good, 41, 8, &child, 1), "a new front");
    assert!(
        !measured_survivor(
            &good,
            41,
            6,
            &aterm_capacity::tree_meter::Member { start: 8, ..child },
            1
        ),
        "a reused child PID"
    );
    for key in ["available", "root_lived"] {
        let mut bad = good.clone();
        bad.as_object_mut()
            .unwrap()
            .insert(key.into(), false.into());
        assert!(!measured_survivor(&bad, 41, 6, &child, 1), "{key}");
    }
    for (key, value) in [
        ("eligible", false.into()),
        ("footprint", (child.footprint - 1).into()),
    ] {
        let mut bad = good.clone();
        bad.as_object_mut()
            .unwrap()
            .get_mut("members")
            .unwrap()
            .as_array_mut()
            .unwrap()[0]
            .as_object_mut()
            .unwrap()
            .insert(key.into(), value);
        assert!(
            !measured_survivor(&bad, 41, 6, &child, 1),
            "{key}, despite a high lifetime peak"
        );
    }
}

#[derive(Debug)]
struct Seen {
    pid: u32,
    ppid: u32,
    held: Option<PathBuf>,
    fds: String,
    cover: String,
    child: Option<u32>,
}

/// Let the stand-in end, and read how the front ended, within the hang detector.
fn finish(mut child: aterm_uds::spawnfd::Child) -> (std::process::ExitStatus, String) {
    if let Some(mut stdin) = child.stdin.take() {
        let _ = stdin.write_all(b"go\n");
    }
    let status = until_within(MUST_HAPPEN, "the front's end", || {
        child.try_wait().ok().flatten()
    });
    let mut err = String::new();
    if let Some(mut e) = child.stderr.take() {
        let _ = std::io::Read::read_to_string(&mut e, &mut err);
    }
    (status, err)
}

fn stem(path: &Path) -> String {
    path.file_stem()
        .and_then(|s| s.to_str())
        .unwrap_or_default()
        .to_owned()
}

/// A CLAIMED BUILD RUNS IN ITS FRONT'S PID UNDER A CLAIM ITS HOLDER KEEPS: the stand-in is the
/// front's own process (its pid, the test its parent), the ledger holds a claim in that pid while
/// the stand-in holds none of the claim's descriptors and no cover marker was handed it, the claim
/// is still there at the stand-in's last line, once the holder's meter has read its tree, and once
/// it exits the claim is let go and its peak kept in the book. NEGATIVE CONTROL (by hand:
/// `front_exec` execing without `hand_over`, the claim handed off to no holder): the ledger lists
/// no claim while the stand-in runs; and (by hand: the holder's watch ending at once, as one whose
/// kevent failed did before) the stand-in finds no claim at its end.
#[test]
fn a_claimed_build_runs_in_its_fronts_pid_under_its_holders_claim() {
    let s = Scratch::new();
    let child = s.front(&["--unverified", "test", "-p", "a"], &[]);
    let front = child.id();
    let seen = s.seen();
    assert_eq!(
        (seen.pid, seen.ppid),
        (front, std::process::id()),
        "{seen:?}"
    );
    let held = seen.held.clone().expect("a claim in its own pid");
    let listed = s.listed();
    assert_eq!(listed.len(), 1, "{listed:?}");
    assert_eq!((listed[0].pid, listed[0].held.is_some()), (front, true));
    assert_eq!(listed[0].cover_marker(), stem(&held));
    assert_eq!(
        seen.fds, "",
        "the build holds none of the claim's descriptors"
    );
    assert_eq!(seen.cover, "unset", "nothing else handed it");
    s.read_in_tree(front);
    let (status, err) = finish(child);
    assert!(status.success(), "{status} {err}");
    assert_eq!(
        s.held_at_end().as_deref(),
        Some(held.as_path()),
        "the claim is held until the build's last line"
    );
    until_within(MUST_HAPPEN, "the claim let go", || {
        s.listed().is_empty().then_some(())
    });
    let book = s.state().join(aterm_capacity::book::TARGO_RECORD);
    let kept = until_within(MUST_HAPPEN, "the peak kept", || {
        std::fs::read_to_string(&book).ok()
    });
    assert!(kept.contains("targo test"), "{kept}");
}

/// A build killed in its front's pid dies as a plain call's would (the signal is its status, no
/// layer between), and its claim is let go once nothing it left runs. A front handed a cover
/// marker that covers nothing (`none`) hands its build the claim's own, so the builds it starts
/// are covered by it. NEGATIVE CONTROL: the front handed no marker hands none (the first test).
#[test]
fn a_killed_build_dies_of_its_signal_and_its_claim_ends() {
    let s = Scratch::new();
    let mut child = s.front(
        &["--unverified", "build"],
        &[("ATERM_CAPACITY_COVER", "none")],
    );
    let seen = s.seen();
    let held = seen.held.clone().expect("a claim in its own pid");
    assert_eq!(seen.cover, stem(&held), "the claim's own marker");
    child.kill().expect("killed");
    let (status, _) = finish(child);
    assert_eq!(
        std::os::unix::process::ExitStatusExt::signal(&status),
        Some(libc::SIGKILL)
    );
    until_within(MUST_HAPPEN, "the claim let go", || {
        s.listed().is_empty().then_some(())
    });
}

/// A call that claims nothing becomes its program at once, in the front's pid, and lays nothing.
/// NEGATIVE CONTROL: a claiming call lays its claim (the first test).
#[test]
fn a_call_that_claims_nothing_becomes_its_program() {
    let s = Scratch::new();
    let child = s.front(&["--unverified", "run", "-p", "a"], &[]);
    let front = child.id();
    let seen = s.seen();
    assert_eq!((seen.pid, seen.held.as_deref()), (front, None), "{seen:?}");
    assert!(s.listed().is_empty());
    let (status, err) = finish(child);
    assert!(status.success(), "{status} {err}");
}

/// A HOLDER THAT DOES NOT ARM LEAVES THE BUILD UNDER ITS FRONT, CLAIMED: the stand-in is the
/// front's child, the claim held in the front's pid while the build runs, the front says why in
/// one line, and the build's exit is the front's; so too when the holder panicked once it had
/// adopted the claim (`UNARMED_PANICS`), whose unwinding hands the claim off as a return does.
/// NEGATIVE CONTROL: an armed holder's build is the front's own process (the first test); and (by
/// hand: the holder's `Unarmed` guard letting its claim go on drop instead of handing it off) the
/// ledger lists no claim while the build runs.
#[test]
fn a_holder_that_does_not_arm_leaves_the_build_under_its_front() {
    for seam in ["1", atpkg::build_admit::hold::UNARMED_PANICS] {
        let s = Scratch::new();
        let child = s.front(
            &["--unverified", "check"],
            &[
                (atpkg::build_admit::hold::UNARMED_ENV, seam),
                ("HOP_EXIT", "3"),
            ],
        );
        let front = child.id();
        let seen = s.seen();
        assert_eq!(
            seen.ppid, front,
            "{seam}: its parent is the front: {seen:?}"
        );
        assert_ne!(seen.pid, front);
        let listed = s.listed();
        assert_eq!(listed.len(), 1, "{seam}: {listed:?}");
        assert_eq!(listed[0].pid, front, "{seam}: the front keeps the claim");
        let (status, err) = finish(child);
        assert_eq!(status.code(), Some(3), "{seam}: {err}");
        assert!(err.contains("did not arm"), "{seam}: {err}");
        assert!(s.listed().is_empty(), "{seam}: the front let it go");
    }
}

/// How a front's admission went, as [`admitted_or_waits`] read it.
#[derive(Debug, PartialEq, Eq)]
enum Went {
    /// Its build started: the stand-in said what it found.
    Ran,
    /// It said it waits (`capacity: … waits (…)`), which it says at its first look that waits.
    Waits,
    /// It exited first, neither building nor saying it waits: its status and what it said.
    Exited(String, Vec<String>),
}

/// Whether `child`'s build started (its stand-in wrote `report`), it said it waits, or it exited
/// without either, whichever comes first, within the hang detector. Takes its stderr.
fn admitted_or_waits(child: &mut aterm_uds::spawnfd::Child, report: &Path) -> Went {
    use std::io::BufRead as _;
    let err = child.stderr.take().expect("its stderr");
    let (tx, rx) = std::sync::mpsc::channel();
    std::thread::spawn(move || {
        for line in std::io::BufReader::new(err).lines().map_while(Result::ok) {
            if tx.send(Some(line)).is_err() {
                return;
            }
        }
        let _ = tx.send(None);
    });
    let mut eof = false;
    let mut lines = Vec::new();
    until_within(MUST_HAPPEN, "the front's build or its wait", || {
        if std::fs::metadata(report).is_ok() {
            return Some(Went::Ran);
        }
        for msg in rx.try_iter() {
            match msg {
                Some(line) => {
                    if line.contains(" waits (") {
                        return Some(Went::Waits);
                    }
                    lines.push(line);
                }
                None => eof = true,
            }
        }
        if eof && let Some(status) = child.try_wait().expect("the front's status") {
            if std::fs::metadata(report).is_ok() {
                return Some(Went::Ran);
            }
            return Some(Went::Exited(status.to_string(), std::mem::take(&mut lines)));
        }
        None
    })
}

/// Cargo's environment words alone never force a front beside an ALONE claim: without a
/// verified active Cargo dependency it waits, and SIGTERM ends the wait without a build.
#[test]
fn a_front_with_cargo_words_waits_without_a_verified_build_dependency() {
    let s = Scratch::new();
    let seen_at = |name: &str| s.root().join(name);
    let alone = s.alone();
    let mut plain = s.front(
        &["--unverified", "check", "-p", "small"],
        &[
            ("ATERM_ADMIT", "wait"),
            ("CARGO_MANIFEST_DIR", s.root().to_str().expect("UTF-8")),
            ("CARGO_PKG_NAME", "small"),
            ("OUT_DIR", s.root().to_str().expect("UTF-8")),
            ("HOP_OUT", seen_at("plain").to_str().expect("UTF-8")),
        ],
    );
    assert_eq!(
        admitted_or_waits(&mut plain, &seen_at("plain")),
        Went::Waits,
        "a front with cargo's words alone was admitted beside the claim held alone"
    );
    // SAFETY: `kill` with two integers; the front is this test's live child, unreaped.
    unsafe { libc::kill(i32::try_from(plain.id()).expect("a pid"), libc::SIGTERM) };
    let (status, _) = finish(plain);
    assert!(!status.success(), "{status}");
    assert!(
        std::fs::metadata(seen_at("plain")).is_err(),
        "its build never started"
    );
    let (status, err) = finish(alone);
    assert!(status.success(), "{status} {err}");
}

/// An ordinary ancestor's open file cannot force a waiting front beside an unrelated ALONE
/// claim. The shell holds each file open before the probe proves it unlocked, then starts the
/// real front with ATERM_ADMIT=wait and no Cargo words. A foreign Cargo-lock basename is no
/// authority; a normal filename is the control. Cleanup precedes the assertions so an old
/// production failure leaves neither the ALONE claim nor a front running.
#[test]
fn an_ordinary_ancestors_unlocked_lock_file_cannot_force_admission() {
    let s = Scratch::new();
    let alone = s.alone();
    let mut observed = Vec::new();
    for relative in [
        "ordinary/.cargo-lock",
        "foreign/.cargo-build-lock",
        "ordinary/notes",
    ] {
        let lock = s.root().join(relative);
        std::fs::create_dir_all(lock.parent().expect("parent")).expect("folder");
        let ready = lock.with_extension("opened");
        let report = lock.with_extension("report");
        let mut command = aterm_uds::spawnfd::command("/bin/sh");
        command
            .args([
                "-c",
                "exec 9>\"$1\"; printf ready >\"$2\"; read start; shift 2; \"$@\"; exit $?",
                "sh",
            ])
            .arg(&lock)
            .arg(&ready)
            .arg(aterm_tempfile::bin_exe("atpkg"))
            .args(["__build_admit", "--shim", "targo", "-"])
            .arg(s.targo())
            .args(["--unverified", "check", "-p", "small"])
            .env_clear()
            .env("PATH", "/usr/bin:/bin")
            .env("HOME", s.home())
            .env("ATERM_STATE_HOME", s.state())
            .env(atpkg::build_admit::LEDGER_ENV, s.ledger())
            .env("ATERM_ADMIT", "wait")
            .env("HOP_LEDGER", s.ledger())
            .env("HOP_OUT", &report)
            .current_dir(s.root())
            .process_group(0)
            .stdin(aterm_uds::spawnfd::Stdio::piped())
            .stdout(aterm_uds::spawnfd::Stdio::null())
            .stderr(aterm_uds::spawnfd::Stdio::piped());
        let mut child = command.spawn().expect("ordinary ancestor");
        until_within(MUST_HAPPEN, "the ancestor's open descriptor", || {
            std::fs::metadata(&ready).ok()
        });
        let unlocked = aterm_uds::ownerlock::probe_any(&lock);
        child
            .stdin
            .as_mut()
            .expect("stdin")
            .write_all(b"start\n")
            .expect("start front");
        let went = admitted_or_waits(&mut child, &report);
        if went == Went::Waits {
            let group = i32::try_from(child.id()).expect("group");
            // SAFETY: this test's own fresh process group, unreaped; it contains only its shell
            // and waiting front, and no build has started or holder armed.
            unsafe { libc::kill(-group, libc::SIGTERM) };
        }
        let _ = finish(child);
        observed.push((relative, unlocked, went, report.exists()));
    }
    let (status, err) = finish(alone);
    assert!(status.success(), "{status} {err}");
    until_within(MUST_HAPPEN, "all claims let go", || {
        s.listed().is_empty().then_some(())
    });
    for (relative, unlocked, went, reported) in observed {
        assert_eq!(
            unlocked,
            aterm_uds::ownerlock::Owner::Dead,
            "{relative}: unlocked"
        );
        assert_eq!(went, Went::Waits, "{relative}: a filename forced admission");
        assert!(!reported, "{relative}: no build started");
    }
}

/// A HELD LOCK BOUNDS THE WAIT ONLY OF A FRONT IN ITS HOLDER'S SESSION. A genuinely held but
/// foreign lock is no authority: with no covering claim and an unrelated ALONE claim, a front in
/// its holder's session gives a bounded, named refusal instead of forcing admission or entering a
/// permanent wait cycle. A front its holder started in a session of its own, as aterm starts each
/// tab's shell, waits as any front does: what hosts a session is no build that waits on it, and
/// aterm holds a profile's Cargo locks while its disk watch reclaims that profile's
/// `incremental/`. The holding ancestor is an isolated child of this suite (so concurrent tests
/// in the shared runner process inherit no open `.cargo-lock`), not Cargo. NEGATIVE CONTROL: the
/// in-session leg, the same holder and lock refusing; and the own-session leg exited 75 too while
/// the front walked every ancestor up to launchd. Cleanup precedes the outcome assertions.
#[test]
fn a_held_lock_bounds_the_wait_only_of_a_front_in_its_holders_session() {
    const ISOLATED_ENV: &str = "ATPKG_HOP_ISOLATED_LOCK_ANCESTOR";
    if std::env::var_os(ISOLATED_ENV).is_none() {
        let exe = std::env::current_exe().expect("the suite's own path");
        let mut cmd = aterm_uds::spawnfd::command(exe);
        cmd.args([
            "--exact",
            "build_hop::a_held_lock_bounds_the_wait_only_of_a_front_in_its_holders_session",
            "--test-threads=1",
            "--nocapture",
        ])
        .env(ISOLATED_ENV, "1");
        let out = aterm_uds::deadline::output_within(&mut cmd, MUST_HAPPEN)
            .expect("isolated ordinary-ancestor process");
        assert!(
            out.status.success(),
            "isolated ordinary-ancestor run failed ({}):\n{}\n{}",
            out.status,
            String::from_utf8_lossy(&out.stdout),
            String::from_utf8_lossy(&out.stderr),
        );
        return;
    }
    let s = Scratch::new();
    let alone = s.alone();
    let file = s.root().join(".cargo-lock");
    let held = aterm_toolchain::record_lock::RecordLock::try_lock(
        std::fs::File::create(&file).expect("foreign file"),
        4096,
    )
    .expect("foreign lock held by this ordinary ancestor");
    let hosted = s.root().join("hosted");
    let mut own = s.front_in_its_own_session(
        &["--unverified", "check", "-p", "small"],
        &[
            ("ATERM_ADMIT", "wait"),
            ("HOP_OUT", hosted.to_str().expect("UTF-8")),
        ],
    );
    let went = admitted_or_waits(&mut own, &hosted);
    if went == Went::Waits {
        // SAFETY: `kill` with two integers; the front is this test's live child, unreaped.
        unsafe { libc::kill(i32::try_from(own.id()).expect("a pid"), libc::SIGTERM) };
    }
    let _ = finish(own);
    let mut child = s.front(
        &["--unverified", "check", "-p", "small"],
        &[("ATERM_ADMIT", "wait")],
    );
    let exited = until_within(
        MUST_HAPPEN,
        "the front's refusal or an incorrect build",
        || {
            child
                .try_wait()
                .ok()
                .flatten()
                .map(Some)
                .or_else(|| s.out().exists().then_some(None))
        },
    );
    let (_, err) = finish(child);
    drop(held);
    let (status, alone_err) = finish(alone);
    assert!(status.success(), "{status} {alone_err}");
    until_within(MUST_HAPPEN, "all claims let go", || {
        s.listed().is_empty().then_some(())
    });
    assert_eq!(
        went,
        Went::Waits,
        "a front in a session of its own took its host's held lock for an outer build's"
    );
    assert!(!hosted.exists(), "its build never started");
    assert_eq!(exited.and_then(|s| s.code()), Some(75), "{err}");
    assert!(!s.out().exists(), "no unclaimed build started");
    assert!(
        err.contains("ownership is unproven") && err.contains("no build started"),
        "{err}"
    );
}

/// The front's end once its pid was killed, within the hang detector, its stdin kept open for what
/// it left running.
fn killed(child: &mut aterm_uds::spawnfd::Child) -> std::process::ExitStatus {
    child.kill().expect("the build killed");
    until_within(MUST_HAPPEN, "the killed build's end", || {
        child.try_wait().ok().flatten()
    })
}

/// A KILLED BUILD'S CLAIM IS KEPT WHILE ITS SURVIVOR WORKS: the stand-in leaves a child holding
/// 96 MiB, and once a fresh holder sample measures that exact child at the survivor threshold
/// or above in its live tree, the build is killed by its pid. The claim stays held in the
/// front's pid, credited with that child, until the child is
/// told to end, and is let go after. NEGATIVE CONTROL (by hand: the holder skipping its survivor
/// phase after a death by a signal): the ledger is empty right after the kill.
#[test]
fn a_killed_builds_claim_is_kept_while_its_survivor_works() {
    let s = Scratch::new();
    let touched = s.root().join("touched");
    let mut child = s.front(
        &["--unverified", "build"],
        &[
            ("HOP_CHILD", "heavy"),
            (TOUCH, "96"),
            (TOUCHED, touched.to_str().expect("UTF-8")),
            (
                atpkg::build_admit::hold::TRACE_ENV,
                s.trace().to_str().expect("UTF-8"),
            ),
        ],
    );
    let front = child.id();
    let mut identity = None;
    let mut tell = None;
    let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        let seen = s.seen();
        let survivor = seen.child.expect("the stand-in's child");
        let serial = aterm_uds::process::process_serial(i32::try_from(front).expect("front pid"))
            .expect("the live front");
        let child_serial =
            aterm_uds::process::process_serial(i32::try_from(survivor).expect("child pid"))
                .expect("the live child");
        identity = Some((survivor, child_serial));
        until_within(MUST_HAPPEN, "the child's memory touched", || {
            std::fs::metadata(&touched).ok()
        });
        let usage = until_within(MUST_HAPPEN, "the exact child's accounting identity", || {
            let group = u32::try_from(aterm_sysprobe::kproc::record(survivor)?.pgid).ok()?;
            aterm_sysprobe::group_usage(group)?
                .into_iter()
                .find(|m| m.pid == survivor)
        });
        let expected = aterm_capacity::tree_meter::Member {
            pid: survivor,
            start: usage.start,
            footprint: atpkg::build_admit::hold::SURVIVORS_IDLE_UNDER,
            peak: 0,
        };
        let after = s
            .latest_meter()
            .and_then(|sample| {
                sample
                    .get("sample_sequence")
                    .and_then(aterm_json::Value::as_u64)
            })
            .unwrap_or(0);
        let measured = until_within(
            MUST_HAPPEN,
            "a fresh eligible child sample above the survivor threshold",
            || {
                let sample = s.latest_meter()?;
                measured_survivor(&sample, front, serial, &expected, after).then_some(sample)
            },
        );
        s.write_evidence("prekill.json", &measured)
            .expect("retain the exact pre-kill observation");
        assert_eq!(
            aterm_uds::process::process_serial(i32::try_from(survivor).expect("child pid")),
            Some(child_serial)
        );
        tell = Some(child.stdin.take().expect("the child's stdin"));
        let status = killed(&mut child);
        assert_eq!(
            std::os::unix::process::ExitStatusExt::signal(&status),
            Some(libc::SIGKILL)
        );
        until_within(MUST_HAPPEN, "the survivor credited", || {
            let listed = s.listed();
            (listed.len() == 1
                && listed[0].pid == front
                && listed[0].held.is_some()
                && listed[0].credit == [survivor])
            .then_some(())
        });
        drop(tell.take());
        until_within(MUST_HAPPEN, "the claim let go", || {
            s.listed().is_empty().then_some(())
        });
        until_within(MUST_HAPPEN, "the holder's final bookkeeping", || {
            s.trace_value()?
                .get("events")?
                .as_array()?
                .iter()
                .any(|event| {
                    event.get("kind").and_then(aterm_json::Value::as_str) == Some("complete")
                        && event.get("front_pid").and_then(aterm_json::Value::as_u64)
                            == Some(u64::from(front))
                        && event
                            .get("front_serial")
                            .and_then(aterm_json::Value::as_u64)
                            == Some(serial)
                })
                .then_some(())
        });
        eprintln!(
            "pre-kill measured survivor: {measured}; final holder trace: {:?}",
            s.trace_value()
        );
    }));
    if let Err(panic) = result {
        // Preserve the state at failure before EOF changes the survivor's state. The fixture
        // owns every path here; no installed state or other test's ledger is read.
        let before = aterm_json::json!({
            "front_pid": front, "child_identity": identity, "trace": s.trace_value(),
            "ledger": format!("{:?}", aterm_capacity::Ledger::at(s.ledger()).list()),
            "front_status": format!("{:?}", child.try_wait()),
        });
        let _ = s.write_evidence("failure.json", &before);
        drop(tell.take());
        drop(child.stdin.take());
        if matches!(child.try_wait(), Ok(None)) {
            let _ = child.kill();
            let _ = child.wait();
        }
        let original_identity_no_longer_observed = identity.is_none_or(|(pid, serial)| {
            aterm_spec::wait::within(MUST_HAPPEN, aterm_capacity::tree_meter::READ_EVERY, || {
                (aterm_uds::process::process_serial(i32::try_from(pid).ok()?) != Some(serial))
                    .then_some(())
            })
            .is_some()
        });
        let _ = s.write_evidence(
            "cleanup.json",
            &aterm_json::json!({"original_identity_no_longer_observed": original_identity_no_longer_observed}),
        );
        let retained = s.dir.keep();
        eprintln!(
            "survivor failure evidence retained at {}: {before}",
            retained.display()
        );
        std::panic::resume_unwind(panic);
    }
}

/// AN IDLE SURVIVOR LETS A KILLED BUILD'S CLAIM GO WHILE IT LIVES: the stand-in leaves a shell
/// waiting for a line (as a background job does, which a terminal's Ctrl-C does not reach), the
/// holder's meter reads it in the tree, and once the build is killed by its pid the claim is let
/// go while that shell still lives. NEGATIVE CONTROL (by hand: the holder keeping the claim for
/// any survivor, as before): the claim stays until the shell is told to end, and this test's hang
/// detector fires.
#[test]
fn an_idle_survivor_lets_a_killed_builds_claim_go() {
    let s = Scratch::new();
    let mut child = s.front(&["--unverified", "build"], &[("HOP_CHILD", "idle")]);
    let seen = s.seen();
    let idle = seen.child.expect("the stand-in's child");
    s.read_in_tree(idle);
    let tell = child.stdin.take().expect("the child's stdin");
    killed(&mut child);
    until_within(MUST_HAPPEN, "the claim let go", || {
        s.listed().is_empty().then_some(())
    });
    assert!(
        aterm_uds::process::process_serial(i32::try_from(idle).expect("a pid")).is_some(),
        "the idle shell still lives"
    );
    drop(tell);
}

/// A BUILD KEEPS THE SIGNALS ITS FRONT WAS STARTED IGNORING, through the real binary (linked with
/// chained fixups on Apple silicon, as the shipped image is: `build.rs`): a front a shell started
/// ignoring SIGPIPE and SIGINT claims, hands off and execs, and its build, sending itself each,
/// lives to say so. NEGATIVE CONTROL (by hand: the load-time read's constructor section dropped,
/// so nothing is put back and std's exec runs, which sets SIGPIPE to its default): the build dies
/// of the SIGPIPE it sends itself.
#[test]
fn a_build_keeps_the_signals_its_front_was_started_ignoring() {
    let s = Scratch::new();
    let child = s.front_ignoring(
        Some("PIPE INT"),
        &["--unverified", "build"],
        &[("HOP_PROBE", "signals")],
    );
    assert!(s.seen().held.is_some(), "claimed");
    let (status, err) = finish(child);
    assert!(status.success(), "{status} {err}");
    let mut said = s.out().into_os_string();
    said.push(".signals");
    assert_eq!(
        std::fs::read_to_string(said)
            .expect("the build's word")
            .trim(),
        "survived"
    );
}

/// A complete store build `build` under `prefix` (its ready marker beside it) whose `targo` writes
/// `ran <build>` to `$HOP_RAN`, and the fold of its tree (`atpkg::tree::tree_root`).
fn lock_store_build(prefix: &Path, build: u64) -> String {
    let dir = atpkg::trust_lock::build_dir(prefix, build);
    std::fs::create_dir_all(dir.join("bin")).expect("bin");
    std::fs::write(atpkg::trust_lock::ready_marker(prefix, build), "ok\n").expect("ready");
    let targo = dir.join("bin/targo");
    std::fs::write(
        &targo,
        format!("#!/bin/sh\necho \"ran {build} $*\" > \"$HOP_RAN\"\n"),
    )
    .expect("the stand-in");
    std::fs::set_permissions(&targo, std::os::unix::fs::PermissionsExt::from_mode(0o755))
        .expect("runnable");
    atpkg::tree::tree_root(&dir).expect("its fold")
}

/// THE SHIM HOP IN A LOCKED TREE (A14): the real front, handed the build its shim was laid for,
/// runs the build the tree's `trust.lock` names instead, from a directory under the tree; with
/// no lock it runs what it was handed (the negative control), and a lock whose build the store
/// holds with another tree starts nothing (exit 75).
#[test]
fn the_hop_runs_the_build_a_trees_lock_names() {
    let s = Scratch::new();
    let prefix = s.root().join("pkg");
    let locked_root = lock_store_build(&prefix, 16387);
    lock_store_build(&prefix, 16500);
    let laid = atpkg::trust_lock::build_dir(&prefix, 16500).join("bin/targo");
    let tree = s.root().join("checkout");
    let cwd = tree.join("crates/one");
    std::fs::create_dir_all(&cwd).expect("checkout");
    let ran = s.root().join("ran");
    let hop = |cwd: &Path| {
        let _ = std::fs::remove_file(&ran);
        let out = aterm_uds::spawnfd::command(aterm_tempfile::bin_exe("atpkg"))
            .args(["__build_admit", "--shim", "targo", "-"])
            .arg(&laid)
            .args(["--unverified", "run", "-p", "a"])
            .env_clear()
            .env("PATH", "/usr/bin:/bin")
            .env("HOME", s.home())
            .env("ATERM_STATE_HOME", s.state())
            .env(atpkg::build_admit::LEDGER_ENV, s.ledger())
            .env("HOP_RAN", &ran)
            .current_dir(cwd)
            .stdin(aterm_uds::spawnfd::Stdio::null())
            .stdout(aterm_uds::spawnfd::Stdio::null())
            .stderr(aterm_uds::spawnfd::Stdio::piped())
            .output()
            .expect("the front");
        (
            out.status.code(),
            std::fs::read_to_string(&ran).unwrap_or_default(),
            String::from_utf8_lossy(&out.stderr).into_owned(),
        )
    };
    let (code, said, _) = hop(&cwd);
    assert_eq!(
        (code, said.trim()),
        (Some(0), "ran 16500 --unverified run -p a"),
        "no lock"
    );

    let lock = atpkg::trust_lock::TrustLock {
        build: 16387,
        tree_root: locked_root,
        compiler_commit: "29ebb403de7a102850d6cb10a62d5cc3baf723ed".to_owned(),
        receipt: "SEED-MINT sha256:00".to_owned(),
        stock_toolchain: "1.99.1".to_owned(),
    };
    std::fs::write(
        tree.join(atpkg::trust_lock::LOCK_FILE),
        atpkg::trust_lock::render_body(&lock),
    )
    .expect("the lock");
    let (code, said, _) = hop(&cwd);
    assert_eq!(
        (code, said.trim()),
        (Some(0), "ran 16387 --unverified run -p a"),
        "locked"
    );

    let mut other = lock.clone();
    other.tree_root = "e".repeat(64);
    std::fs::write(
        tree.join(atpkg::trust_lock::LOCK_FILE),
        atpkg::trust_lock::render_body(&other),
    )
    .expect("the lock");
    let (code, said, stderr) = hop(&cwd);
    assert_eq!((code, said.as_str()), (Some(75), ""), "{stderr}");
    assert!(stderr.contains("no build started"), "{stderr}");
}

/// A LOCKED BUILD THE STORE LACKS IS INSTALLED BY THE HOP, into the store it resolves from: the
/// real front spawns `atpkg install --no-init trust --build <N> --wait-lock …`, whose words it
/// parses (a registry with no index refuses the install, not its usage), with its output on
/// stderr and none on the build's stdout, and starts no build when the install fails. A shim laid
/// in another store than this atpkg installs into is refused before any install.
#[test]
fn an_absent_locked_build_is_installed_by_the_hop_into_its_store() {
    let s = Scratch::new();
    let registry = s.root().join("registry");
    std::fs::create_dir_all(&registry).expect("an empty registry");
    let tree = s.root().join("checkout");
    std::fs::create_dir_all(&tree).expect("checkout");
    let lock = atpkg::trust_lock::TrustLock {
        build: 16999,
        tree_root: "4".repeat(64),
        compiler_commit: "29ebb403de7a102850d6cb10a62d5cc3baf723ed".to_owned(),
        receipt: "SEED-MINT sha256:00".to_owned(),
        stock_toolchain: "1.99.1".to_owned(),
    };
    std::fs::write(
        tree.join(atpkg::trust_lock::LOCK_FILE),
        atpkg::trust_lock::render_body(&lock),
    )
    .expect("the lock");
    let ran = s.root().join("ran");
    let hop = |prefix: &Path| {
        lock_store_build(prefix, 16500);
        let laid = atpkg::trust_lock::build_dir(prefix, 16500).join("bin/targo");
        let out = aterm_uds::spawnfd::command(aterm_tempfile::bin_exe("atpkg"))
            .args(["__build_admit", "--shim", "targo", "-"])
            .arg(&laid)
            .args(["--unverified", "metadata"])
            .env_clear()
            .env("PATH", "/usr/bin:/bin")
            .env("HOME", s.home())
            .env("ATERM_STATE_HOME", s.state())
            .env(atpkg::build_admit::LEDGER_ENV, s.ledger())
            .env("ATPKG_REGISTRY", format!("dir:{}", registry.display()))
            .env("HOP_RAN", &ran)
            .current_dir(&tree)
            .stdin(aterm_uds::spawnfd::Stdio::null())
            .output()
            .expect("the front");
        (
            out.status.code(),
            String::from_utf8_lossy(&out.stdout).into_owned(),
            String::from_utf8_lossy(&out.stderr).into_owned(),
        )
    };

    let home_store = atpkg::toolchain::atpkg_prefix(&s.home(), None);
    let (code, stdout, stderr) = hop(&home_store);
    assert_eq!(code, Some(75), "{stderr}");
    assert_eq!(stdout, "", "the install says nothing on the build's stdout");
    assert!(stderr.contains("installing Trust build 16999"), "{stderr}");
    assert!(stderr.contains("its install failed"), "{stderr}");
    assert!(
        stderr.contains("exit status: 1"),
        "the child parsed its words and refused the install, not its usage (2): {stderr}"
    );
    for usage in [
        "one program at a time",
        "takes a build number",
        "--build stages",
    ] {
        assert!(!stderr.contains(usage), "a usage refusal: {stderr}");
    }
    assert!(stderr.contains("no build started"), "{stderr}");
    assert!(!ran.exists(), "no build ran");

    // Negative control: a shim laid in a store this atpkg does not install into.
    let (code, stdout, stderr) = hop(&s.root().join("pkg"));
    assert_eq!((code, stdout.as_str()), (Some(75), ""), "{stderr}");
    assert!(stderr.contains("installs into"), "{stderr}");
    assert!(!stderr.contains("installing Trust build"), "{stderr}");
    assert!(!ran.exists(), "no build ran");
}

#[test]
fn the_front_says_it_honours_locks() {
    let out = aterm_uds::spawnfd::command(aterm_tempfile::bin_exe("atpkg"))
        .args(["__build_admit", atpkg::trust_lock::LOCK_CAPABILITY_FLAG])
        .output()
        .expect("the front");
    assert!(out.status.success());
    assert_eq!(
        out.stdout,
        format!("{}\n", atpkg::trust_lock::LOCK_CAPABILITY).as_bytes()
    );
    assert!(atpkg::trust_lock::helper_honours_lock(
        &aterm_tempfile::bin_exe("atpkg")
    ));
    // Negative control: a front that takes the flag for its target does not honour locks.
    assert!(!atpkg::trust_lock::helper_honours_lock(Path::new(
        "/usr/bin/false"
    )));

    // What verify reads of this Mac's shim: one that hops through this front runs locked builds;
    // one whose helper is an older front runs the build it was laid for.
    let s = Scratch::new();
    let prefix = s.root().join("pkg");
    let target = atpkg::trust_lock::build_dir(&prefix, 16500).join("bin/targo");
    let lay = |helper: &Path| {
        std::fs::create_dir_all(prefix.join("bin")).expect("bin");
        let hop = atpkg::build_cache::registered::shim_hop(
            helper,
            "targo",
            &target.to_string_lossy(),
            None,
        );
        std::fs::write(
            prefix.join("bin/targo"),
            format!("#!/bin/sh\n{hop}exec '{}' \"$@\"\n", target.display()),
        )
        .expect("the shim");
        atpkg::trust_lock::shim_answer(&prefix)
    };
    assert_eq!(
        lay(&aterm_tempfile::bin_exe("atpkg")),
        atpkg::trust_lock::ShimAnswer::Locked
    );
    assert_eq!(
        lay(Path::new("/usr/bin/false")),
        atpkg::trust_lock::ShimAnswer::Laid {
            target: target.clone(),
            build: Some(16500)
        }
    );
}
