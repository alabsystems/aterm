// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

//! The Security panel's *Move to Trash* for conflicting copies of aterm
//! (design §3.7, the third repair).
//!
//! A copy on disk that claims aterm's bundle id with a different code
//! requirement resets the Full Disk Access grant for every copy whenever it
//! asks (`aterm_containment::consent`, "The claimant census"). The panel lists
//! such copies and, for an ad-hoc or unsigned one beside a running copy whose
//! grant survives updates, offers to move it to the Trash. This is the worker
//! behind that button.
//!
//! An owner gesture only: the press is confirmed in an AppKit alert no control
//! verb can answer, and there is no verb, config key or automatic trigger. Off
//! the main thread it re-checks before touching anything — a fresh census, the
//! copy's identity again right before the move, whether a process runs from it
//! — and then moves it with `/usr/bin/trash`, the Finder's own move, so the
//! copy stays recoverable.

use std::io::Read as _;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use aterm_containment::consent::{self, Claimants, Retired};

/// The trash tool macOS 15 and later ship.
pub(crate) const TRASH_TOOL: &str = "/usr/bin/trash";

/// How long one move may take. Within a volume it is a rename; across
/// volumes it is a copy.
const TRASH_CEILING: Duration = Duration::from_secs(120);

/// The worker's reaches, injected so a headless instance and every unit test
/// get arms that read nothing and move nothing.
#[derive(Clone, Copy)]
pub(crate) struct RetireArms {
    census: fn(&str) -> Claimants,
    unchanged: fn(&consent::Claimant, &str) -> bool,
    in_use: fn(&Path) -> Option<bool>,
    trash: fn(&Path) -> Result<(), String>,
}

impl RetireArms {
    const fn live() -> Self {
        Self {
            census: consent::claimants_for,
            unchanged: consent::still_claims,
            in_use: atpkg::gc::runs_from,
            trash: trash_with_tool,
        }
    }

    const fn inert() -> Self {
        Self {
            census: inert_census,
            unchanged: inert_unchanged,
            in_use: inert_in_use,
            trash: inert_trash,
        }
    }
}

fn inert_census(_bundle_id: &str) -> Claimants {
    Claimants::default()
}

const fn inert_unchanged(_copy: &consent::Claimant, _bundle_id: &str) -> bool {
    false
}

const fn inert_in_use(_path: &Path) -> Option<bool> {
    None
}

fn inert_trash(_path: &Path) -> Result<(), String> {
    Err("this instance does not move files".to_string())
}

/// One owner press's outcome, per copy.
pub(crate) type RetireReport = Vec<(PathBuf, Retired)>;

#[derive(Default)]
struct Slot {
    live: bool,
    report: Option<RetireReport>,
    arrived: bool,
}

/// At most one worker, and the last report.
pub(crate) struct RetireState {
    arms: RetireArms,
    shared: Arc<Mutex<Slot>>,
}

impl RetireState {
    /// `inert()` for a headless instance, the live arms otherwise.
    pub(crate) fn new(headless: bool) -> Self {
        Self {
            arms: if headless {
                RetireArms::inert()
            } else {
                RetireArms::live()
            },
            shared: Arc::default(),
        }
    }

    #[cfg(test)]
    fn with_arms(arms: RetireArms) -> Self {
        Self {
            arms,
            shared: Arc::default(),
        }
    }

    /// The worker's slot, locked. A GUARD-RETURNING helper: registered in the
    /// lock-order census's `GUARD_HELPERS` (identity `shared`, the same `Arc` the
    /// worker thread locks by that name), so its callers' holds are graphed.
    fn retire_slot(&self) -> std::sync::MutexGuard<'_, Slot> {
        self.shared.lock().unwrap_or_else(|p| p.into_inner())
    }

    /// Start the worker for the copies the owner was shown. `false` when one is
    /// already running or the thread could not be spawned. `wake` runs on the
    /// worker once the report is stored.
    pub(crate) fn start(
        &self,
        bundle_id: String,
        plan: Vec<PathBuf>,
        wake: impl FnOnce() + Send + 'static,
    ) -> bool {
        {
            let mut slot = self.retire_slot();
            if slot.live {
                return false;
            }
            slot.live = true;
        }
        let arms = self.arms;
        let shared = Arc::clone(&self.shared);
        let spawned = std::thread::Builder::new()
            .name("aterm-claimant-retire".to_owned())
            .spawn(move || {
                let fresh = (arms.census)(&bundle_id);
                let report = consent::retire_claimants(
                    &plan,
                    &fresh,
                    |copy| (arms.unchanged)(copy, &bundle_id),
                    arms.in_use,
                    arms.trash,
                );
                {
                    let mut slot = shared.lock().unwrap_or_else(|p| p.into_inner());
                    slot.live = false;
                    slot.report = Some(report);
                    slot.arrived = true;
                }
                wake();
            });
        if spawned.is_err() {
            self.retire_slot().live = false;
            return false;
        }
        true
    }

    /// Whether a worker is running.
    pub(crate) fn is_live(&self) -> bool {
        self.retire_slot().live
    }

    /// The last completed press's report.
    pub(crate) fn report(&self) -> Option<RetireReport> {
        self.retire_slot().report.clone()
    }

    /// Whether a report arrived since the last call.
    pub(crate) fn take_arrival(&self) -> bool {
        std::mem::take(&mut self.retire_slot().arrived)
    }
}

/// Move `path` to the Trash with [`TRASH_TOOL`], bounded by [`TRASH_CEILING`].
/// The error is the system's own reason when it gave one.
fn trash_with_tool(path: &Path) -> Result<(), String> {
    use std::process::{Command, Stdio};
    // Census paths are absolute, so the argument can never read as an option.
    if !path.is_absolute() {
        return Err("not an absolute path".to_string());
    }
    let mut child = Command::new(TRASH_TOOL)
        .arg(path)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::piped())
        .spawn()
        .map_err(|e| format!("{TRASH_TOOL} could not run: {e}"))?;
    let deadline = Instant::now() + TRASH_CEILING;
    let status = loop {
        match child.try_wait() {
            Ok(Some(status)) => break status,
            Ok(None) if Instant::now() < deadline => std::thread::sleep(Duration::from_millis(50)),
            Ok(None) | Err(_) => {
                let _ = child.kill();
                let _ = child.wait();
                return Err(format!(
                    "the move did not finish within {} s",
                    TRASH_CEILING.as_secs()
                ));
            }
        }
    };
    if status.success() {
        return Ok(());
    }
    let mut stderr = String::new();
    if let Some(pipe) = child.stderr.take() {
        let _ = pipe.take(8 * 1024).read_to_string(&mut stderr);
    }
    Err(
        trash_refusal(&stderr).unwrap_or_else(|| match status.code() {
            Some(code) => format!("{TRASH_TOOL} exited {code}"),
            None => format!("{TRASH_TOOL} was ended by a signal"),
        }),
    )
}

/// The system's reason from the tool's stderr: the first quoted description
/// after `Code=<n>`, e.g. `The file “x.app” doesn’t exist.`
fn trash_refusal(stderr: &str) -> Option<String> {
    let after = &stderr[stderr.find("Code=")?..];
    let open = after.find('"')? + 1;
    let len = after[open..].find('"')?;
    let reason = after[open..open + len].trim();
    (!reason.is_empty()).then(|| reason.to_owned())
}

#[cfg(test)]
mod tests {
    use super::*;

    const DEV_ID: &str = "designated => identifier \"x\" and anchor apple generic and \
                          certificate leaf[subject.OU] = \"T\"";

    fn fake_census(_id: &str) -> Claimants {
        let claimant = |path: &str, dr: &str, running: bool| consent::Claimant {
            path: PathBuf::from(path),
            dr: consent::classify_dr(dr),
            dr_text: dr.to_string(),
            signing: if running { "developer-id" } else { "adhoc" },
            team: None,
            running,
        };
        Claimants {
            found: vec![
                claimant("/A/x.app", DEV_ID, true),
                claimant("/A/x.app.rollback", "designated => cdhash H\"aa\"", false),
            ],
            enumeration: consent::Enumeration::Complete,
        }
    }

    fn same(_copy: &consent::Claimant, _bundle_id: &str) -> bool {
        true
    }

    fn idle(_path: &Path) -> Option<bool> {
        Some(false)
    }

    fn fake_trash(_path: &Path) -> Result<(), String> {
        Ok(())
    }

    fn wait_for_report(state: &RetireState) -> RetireReport {
        let deadline = Instant::now() + Duration::from_secs(10);
        while state.is_live() && Instant::now() < deadline {
            std::thread::sleep(Duration::from_millis(5));
        }
        state.report().expect("the worker stored a report")
    }

    /// The worker re-runs the census, moves what it still offers, stores the
    /// report, and wakes the loop once.
    #[test]
    fn the_worker_moves_what_a_fresh_census_still_offers_and_reports_it() {
        let state = RetireState::with_arms(RetireArms {
            census: fake_census,
            unchanged: same,
            in_use: idle,
            trash: fake_trash,
        });
        let (tx, rx) = std::sync::mpsc::channel();
        let plan = vec![
            PathBuf::from("/A/x.app.rollback"),
            PathBuf::from("/A/x.app"),
        ];
        assert!(state.start("x".to_string(), plan, move || tx.send(()).unwrap()));
        rx.recv_timeout(Duration::from_secs(10))
            .expect("the worker wakes the loop");
        assert_eq!(
            wait_for_report(&state),
            vec![
                (PathBuf::from("/A/x.app.rollback"), Retired::Moved),
                (PathBuf::from("/A/x.app"), Retired::NotOffered),
            ]
        );
        assert!(state.take_arrival());
        assert!(!state.take_arrival(), "one arrival per report");
    }

    /// The inert arms read nothing and move nothing: with no census, nothing is
    /// offered, so the one planned copy is left in place.
    #[test]
    fn the_inert_arms_move_nothing() {
        let state = RetireState::new(true);
        assert!(state.start(
            "x".to_string(),
            vec![PathBuf::from("/A/x.app.rollback")],
            || {}
        ));
        assert_eq!(
            wait_for_report(&state),
            vec![(PathBuf::from("/A/x.app.rollback"), Retired::NotOffered)]
        );
    }

    /// A second press while a worker runs starts no second worker.
    #[test]
    fn a_second_press_while_live_starts_nothing() {
        fn slow_census(id: &str) -> Claimants {
            std::thread::sleep(Duration::from_millis(200));
            fake_census(id)
        }
        let state = RetireState::with_arms(RetireArms {
            census: slow_census,
            unchanged: same,
            in_use: idle,
            trash: fake_trash,
        });
        assert!(state.start("x".to_string(), Vec::new(), || {}));
        assert!(!state.start("x".to_string(), Vec::new(), || {}));
        let _ = wait_for_report(&state);
    }

    #[test]
    fn the_refusal_is_the_systems_own_words() {
        let stderr = "2026-09-23 22:29:30.491 trash[79297:53920104] # Error attempting to move \
                      /x/Nope.app to the trash folder, Error Domain=NSCocoaErrorDomain Code=4 \
                      \"The file “Nope.app” doesn’t exist.\" UserInfo={NSURL=file:///x/Nope.app}";
        assert_eq!(
            trash_refusal(stderr).as_deref(),
            Some("The file “Nope.app” doesn’t exist.")
        );
        assert_eq!(trash_refusal("no code here"), None);
        assert_eq!(trash_refusal("Code=4 \"\""), None);
    }

    /// No control module can start a move: the entry points appear in none of
    /// them. `app act` is generic, so its refusal is pinned by behaviour in
    /// `app_control`'s tests; this pins the vocabulary.
    #[test]
    fn no_control_module_names_the_retire_entry_points() {
        const ENTRY_POINTS: &[&str] = &[
            "begin_claimant_retire",
            "consent_retire",
            "retire_claimants",
        ];
        let dir = Path::new(env!("CARGO_MANIFEST_DIR")).join("src");
        let mut scanned = 0usize;
        for entry in std::fs::read_dir(&dir).expect("read aterm-gui/src") {
            let path = entry.expect("dir entry").path();
            let name = path
                .file_name()
                .and_then(|n| n.to_str())
                .unwrap_or_default();
            if !(name.starts_with("control") || name == "app_control.rs") || !name.ends_with(".rs")
            {
                continue;
            }
            scanned += 1;
            let src = std::fs::read_to_string(&path).expect("read a control module");
            for token in ENTRY_POINTS {
                assert!(
                    !src.contains(token),
                    "{name} names `{token}`: a program in a session must not be able to move \
                     a copy of the app"
                );
            }
        }
        assert!(
            scanned >= 3,
            "scanned {scanned} control modules: the fence matched nothing"
        );
        assert!(
            include_str!("lib.rs").contains("fn begin_claimant_retire"),
            "the entry point moved; update ENTRY_POINTS"
        );
    }

    /// The live tool, on a path that is not there: it runs, moves nothing, and
    /// the refusal is its own reason. (A successful move is not exercised here:
    /// it would put a file in this account's real Trash on every run.)
    #[cfg(target_os = "macos")]
    #[test]
    fn the_trash_tool_names_why_it_could_not_move_a_copy() {
        if !Path::new(TRASH_TOOL).is_file() {
            eprintln!("{TRASH_TOOL} absent (macOS before 15): skipped");
            return;
        }
        let missing = std::env::temp_dir()
            .canonicalize()
            .unwrap()
            .join(format!("aterm-retire-missing-{}.app", std::process::id()));
        let refused = trash_with_tool(&missing).unwrap_err();
        // The system's own words, in whatever language this Mac speaks, name
        // the file — never the generic exit-status fallback.
        assert!(refused.contains("aterm-retire-missing"), "{refused}");
        assert!(!refused.contains(TRASH_TOOL), "{refused}");
        assert!(trash_with_tool(Path::new("relative.app")).is_err());
    }
}
