// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! `verify land`'s rules over scratch repositories with a bare origin: the compose rule and its
//! re-judge, and the stated receipt, each beside the control that shows the rule bites.

use super::*;
use aterm_land_trailer::{Interrupted, KEYS};
use std::cell::RefCell;
use std::path::PathBuf;

#[test]
fn an_ended_intent_wait_starts_no_observation() {
    let root = aterm_tempfile::tempdir().unwrap();
    let plumbing = Git {
        root: root.path(),
        make: &git_cmd,
    };
    let intent = agh::IntentId::parse("67-000000000000").unwrap();
    for interrupted in [false, true] {
        let step = |_: &Path,
                    _: &dyn Fn(&Path) -> Command,
                    _,
                    _: &[std::ffi::OsString],
                    _,
                    _: RemoteMode| {
            panic!("an ended wait must not start another remote step")
        };
        let pause = |_| panic!("an ended wait must not pause");
        let interrupt = || interrupted.then_some(15);
        let wire = Wire::new(&plumbing, &step, &pause, &|_| {}, &interrupt);
        let until = if interrupted {
            Instant::now() + Duration::from_secs(60)
        } else {
            Instant::now() - Duration::from_secs(1)
        };
        assert!(wire.wait_for_intent(&intent, until).is_err());
        let unsettled = wire.unsettled.take();
        if interrupted {
            assert!(unsettled.is_none());
        } else {
            assert!(unsettled.unwrap().contains(intent.as_str()));
        }
    }
}

#[test]
fn intent_observations_share_the_waits_absolute_deadline() {
    use std::cell::Cell;
    use std::os::unix::process::ExitStatusExt as _;
    let root = aterm_tempfile::tempdir().unwrap();
    let plumbing = Git {
        root: root.path(),
        make: &git_cmd,
    };
    let until = Instant::now() + Duration::from_secs(60);
    let reads = Cell::new(0);
    let pauses = Cell::new(0);
    let step = |_: &Path,
                _: &dyn Fn(&Path) -> Command,
                tool,
                _: &[std::ffi::OsString],
                deadline,
                mode: RemoteMode| {
        assert_eq!(mode, RemoteMode::Read, "intent observations only read");
        assert_eq!(
            deadline, until,
            "no observation may acquire a fresh deadline"
        );
        let stdout = if tool == crate::remote::Tool::Git {
            "https://github.com/fixture/repo.git\n".to_owned()
        } else {
            reads.set(reads.get() + 1);
            format!(
                r#"{{"id":"67-000000000000","repo":"fixture/repo","op":"ref.update-atomic","key":"atomic refs","open":{}}}"#,
                reads.get() < 3
            )
        };
        Ok(std::process::Output {
            status: std::process::ExitStatus::from_raw(0),
            stdout: stdout.into_bytes(),
            stderr: Vec::new(),
        })
    };
    let pause = |_| pauses.set(pauses.get() + 1);
    let wire = Wire::new(&plumbing, &step, &pause, &|_| {}, &|| None);
    wire.wait_for_intent(&agh::IntentId::parse("67-000000000000").unwrap(), until)
        .unwrap();
    assert_eq!(reads.get(), 3);
    assert_eq!(pauses.get(), 2);
}

/// A PUSH REFUSED AGAIN BY AN INTENT WHOSE WRITER IS GONE IS MADE AGAIN WITHIN RETRIES: a wait
/// that ended on its writer gone (`closable`) lets the next push's agh read the intent's refs
/// from origin before it begins, and the same intent refusing that push again does not say
/// whether origin read mixed, could not be read, or that agh did not judge the writer gone. So
/// that refusal is a failure that sent nothing, made again after a doubling pause, [`RETRIES`]
/// times in a row at most: a push that then lands is not stopped, and past that bound the push
/// stops naming the intent to settle. An intent that records no writer and settled with an
/// unknown outcome stops the push at once, naming `agh intent retract`. NEGATIVE CONTROLS: the
/// same intent after a wait that ended on it closed is waited on again, as is another intent
/// after a wait that ended on a writer gone, each running to the composition bound with
/// nothing to settle; and an interrupt taken before the push is made again leaves nothing to
/// settle.
#[test]
fn a_push_refused_again_by_an_intent_whose_writer_is_gone_is_made_again_within_retries() {
    use std::cell::Cell;
    use std::os::unix::process::ExitStatusExt as _;
    let root = aterm_tempfile::tempdir().unwrap();
    let plumbing = Git {
        root: root.path(),
        make: &git_cmd,
    };
    let doubling: Vec<Duration> = (0..RETRIES).map(|n| RETRY_PAUSE * (1u32 << n)).collect();
    for case in [
        "same-gone",
        "same-gone-cured",
        "same-gone-interrupted",
        "same-held",
        "same-closed",
        "other-gone",
    ] {
        let attempts = Cell::new(0_usize);
        let shows = Cell::new(0_usize);
        let said = Cell::new(0_usize);
        let pauses = RefCell::new(Vec::new());
        let step = |_: &Path,
                    _: &dyn Fn(&Path) -> Command,
                    tool,
                    args: &[std::ffi::OsString],
                    _,
                    mode: RemoteMode| {
            assert_eq!(mode, RemoteMode::Read, "a wait on an intent only reads");
            let stdout = if tool == crate::remote::Tool::Git {
                "https://github.com/fixture/repo.git\n".to_owned()
            } else {
                shows.set(shows.get() + 1);
                let state = match case {
                    "same-closed" => r#""open":false,"closable":false"#,
                    "same-held" => {
                        r#""open":true,"closable":false,"writer":null,"settled":"unknown""#
                    }
                    _ => r#""open":true,"closable":true"#,
                };
                format!(
                    r#"{{"id":"{}","repo":"fixture/repo","op":"ref.update-atomic","key":"atomic refs",{state}}}"#,
                    args[3].to_str().unwrap()
                )
            };
            Ok(std::process::Output {
                status: std::process::ExitStatus::from_raw(0),
                stdout: stdout.into_bytes(),
                stderr: Vec::new(),
            })
        };
        let pause = |pause: Duration| pauses.borrow_mut().push(pause);
        let say = |_: &str| said.set(said.get() + 1);
        let interrupt = || (case == "same-gone-interrupted" && shows.get() > 0).then_some(15);
        let wire = Wire::new(&plumbing, &step, &pause, &say, &interrupt);
        let mut races = 0;
        let result = push_counting(
            &mut races,
            &|| {
                let attempt = attempts.get();
                attempts.set(attempt + 1);
                if case == "same-gone-cured" && attempt == 2 {
                    return Ok(crate::remote::Pushed::Done);
                }
                let intent = if case == "other-gone" {
                    format!("{}-000000000000", attempt + 1)
                } else {
                    "66-fc387e8b41a0".to_owned()
                };
                Ok(crate::remote::Pushed::Blocked {
                    intent: agh::IntentId::parse(&intent).unwrap(),
                    why: "pre-send".into(),
                })
            },
            &|intent| wire.await_intent(intent),
            &|| Ok(PushObservation::Unchanged),
            &|_| unreachable!("a pre-send obstruction is not a rejection"),
            &|_| unreachable!("a pre-send obstruction is not an unsent write"),
        );
        let unsettled = wire.unsettled.take();
        let counts = (attempts.get(), shows.get(), said.get());
        match case {
            "same-gone" => {
                let why = result.unwrap_err();
                assert!(
                    why.contains("66-fc387e8b41a0")
                        && why.contains(&format!(
                            "stopped after {RETRIES} failures of the push in a row"
                        )),
                    "{why}"
                );
                assert_eq!(counts, (RETRIES + 2, RETRIES + 1, 2 * RETRIES + 1));
                assert_eq!(*pauses.borrow(), doubling);
                let unsettled = unsettled.expect("the held intent is left to settle");
                assert!(
                    unsettled.contains("66-fc387e8b41a0")
                        && unsettled.contains("agh intent retract"),
                    "{unsettled}"
                );
            }
            "same-gone-cured" => {
                assert_eq!(result, Ok(None));
                assert_eq!(counts, (3, 2, 3));
                assert_eq!(*pauses.borrow(), doubling[..1].to_vec());
                assert!(unsettled.is_none());
            }
            "same-gone-interrupted" => {
                let why = result.unwrap_err();
                assert!(why.contains("the landing took signal 15"), "{why}");
                assert_eq!(counts, (2, 1, 1));
                assert!(pauses.borrow().is_empty());
                assert!(unsettled.is_none());
            }
            "same-held" => {
                let why = result.unwrap_err();
                assert!(why.contains("records no writer"), "{why}");
                assert_eq!(counts, (1, 1, 1));
                let unsettled = unsettled.expect("the held intent is left to settle");
                assert!(
                    unsettled.contains("66-fc387e8b41a0")
                        && unsettled.contains("agh intent retract"),
                    "{unsettled}"
                );
            }
            _ => {
                let why = result.unwrap_err();
                assert!(
                    why.contains(&format!("stopped after {COMPOSES}")),
                    "{case}: {why}"
                );
                assert_eq!(counts, (COMPOSES + 1, COMPOSES, COMPOSES), "{case}");
                assert!(pauses.borrow().is_empty(), "{case}");
                assert!(unsettled.is_none(), "{case}");
            }
        }
    }
}

#[path = "land_rejudge_tests.rs"]
mod rejudge;

#[path = "land_metadata_tests.rs"]
mod metadata;

#[path = "land_markdown_tests.rs"]
mod markdown;

#[path = "land_push_tests.rs"]
mod push;

#[path = "land_skew_tests.rs"]
mod skew;

#[path = "land_resolve_tests.rs"]
mod resolving;
#[path = "land_records_tests.rs"]
mod verdicts;

/// A clone's spool in memory, for the scripted landings: a verdict FAIL already kept at its path
/// is kept as it was, as [`crate::land_spool::Dir`] keeps it.
#[derive(Default)]
struct Memory(RefCell<std::collections::BTreeMap<String, crate::land_record::Signed>>);

impl crate::land_spool::Spool for Memory {
    fn put(&self, record: &crate::land_record::Signed) -> Result<(), String> {
        let (_, _, fail) = crate::land_spool::kept_as(&record.path, 0)?;
        let mut kept = self.0.borrow_mut();
        if !(fail && kept.contains_key(&record.path)) {
            kept.insert(record.path.clone(), record.clone());
        }
        Ok(())
    }

    fn pending(&self) -> Result<crate::land_spool::Pending, String> {
        Ok(crate::land_spool::Pending {
            records: self.0.borrow().values().cloned().collect(),
            unreadable: Vec::new(),
        })
    }

    fn done(&self, record: &crate::land_record::Signed) -> Result<(), String> {
        let mut kept = self.0.borrow_mut();
        if kept
            .get(&record.path)
            .is_some_and(|held| held.bytes == record.bytes)
        {
            kept.remove(&record.path);
        }
        Ok(())
    }
}

thread_local! {
    /// Each run record the [`quiet`] landings of this test's thread kept, by path.
    static QUIET: RefCell<std::collections::BTreeMap<String, crate::land_record::Signed>> =
        RefCell::default();
    /// How many [`quiet`] runs this test's thread began: each its own id.
    static QUIET_RUNS: std::cell::Cell<u32> = const { std::cell::Cell::new(0) };
}

/// [`Memory`], each run record it keeps also kept for [`quietly`] to read back.
#[derive(Default)]
struct Observed(Memory);

impl crate::land_spool::Spool for Observed {
    fn put(&self, record: &crate::land_record::Signed) -> Result<(), String> {
        if record.path.starts_with(crate::land_run::DIR) {
            QUIET.with(|kept| {
                kept.borrow_mut()
                    .insert(record.path.clone(), record.clone())
            });
        }
        self.0.put(record)
    }

    fn pending(&self) -> Result<crate::land_spool::Pending, String> {
        self.0.pending()
    }

    fn done(&self, record: &crate::land_record::Signed) -> Result<(), String> {
        self.0.done(record)
    }
}

/// The records of a scripted landing that keeps none: every append holds nothing and says so by
/// succeeding, the spool is [`Memory`] (each run record also kept for [`quietly`]), nothing is
/// signed, the clock stands at [`crate::land_record::tests::NOW`], no interrupt comes, and each
/// run of the thread has its own id.
fn quiet() -> Records<'static> {
    let n = QUIET_RUNS.with(|runs| {
        runs.set(runs.get() + 1);
        runs.get()
    });
    Records {
        append: Box::new(|_: &Path, _: &[crate::land_record::Signed], _: u64| Ok(())),
        spool: Box::new(Observed::default()),
        signer: None,
        host: "scripted".to_owned(),
        now: Box::new(|| crate::land_record::tests::NOW),
        run: crate::land_run::Run {
            id: format!("{n:032x}"),
            ..crate::land_run::tests::run()
        },
        hosting: Box::new(crate::hosting::Unhosted),
        interrupt: Box::new(|| None),
        tally: records::Tally::default(),
        lock: None,
    }
}

/// `steps` as [`told`] gives them.
fn steps(steps: &[(&str, &str, &str)]) -> Vec<Told> {
    steps
        .iter()
        .map(|(kind, result, cause)| {
            (
                (*kind).to_owned(),
                (*result).to_owned(),
                (*cause).to_owned(),
            )
        })
        .collect()
}

/// A run [`quietly`] reads: its `steps` and its landing's `kind`, not resumed.
fn landed(steps: &[(&str, &str, &str)], kind: &str) -> Quiet {
    (self::steps(steps), Some((kind.to_owned(), false)))
}

/// One run [`quietly`] reads: its records in order, as its steps ([`told`]), and its landing's
/// kind and whether it resumed a kept judgment, where it landed.
type Quiet = (Vec<Told>, Option<(String, bool)>);

/// What the [`quiet`] runs of this test's thread recorded, run by run in the order they began.
fn quietly() -> Vec<Quiet> {
    let mut runs: std::collections::BTreeMap<String, Vec<crate::land_run::Record>> =
        std::collections::BTreeMap::new();
    QUIET.with(|kept| {
        for signed in kept.borrow().values() {
            let entry = crate::land_run::Entry::read(
                &signed.path,
                &signed.bytes,
                crate::land_record::Origin::Spool,
            );
            let record = entry.body.expect("a quiet run record reads").record;
            runs.entry(record.run.id.clone()).or_default().push(record);
        }
    });
    runs.into_values()
        .map(|mut records| {
            records.sort_by_key(|record| record.seq);
            let landing = ends(&records).first().and_then(|end| {
                end.landing
                    .as_ref()
                    .map(|landing| (landing.kind.clone(), landing.resumed))
            });
            (told(&records), landing)
        })
        .collect()
}

#[path = "land_issues_tests.rs"]
mod open_issues;

/// This image's digest in the scripted landings, whose every checkout's verifier is this image,
/// so nothing hands over.
const OWN: &str = "0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef";

/// The verifier every scripted checkout builds: this image.
fn own_image() -> Image {
    Image {
        path: PathBuf::from("/scripted/aterm-verify"),
        digest: OWN.into(),
    }
}

/// The scripted landings' lander: no words, this image.
fn own() -> Own<'static> {
    Own {
        words: &[],
        image: OWN,
        at: None,
    }
}

#[test]
fn a_closed_foreign_intent_refreshes_main_before_any_new_attempt() {
    use std::cell::Cell;
    for observed in ["unchanged", "moved", "landed"] {
        let attempts = Cell::new(0);
        let waited = Cell::new(false);
        let refreshed = Cell::new(false);
        let mut races = 2;
        let result = push_counting(
            &mut races,
            &|| {
                let attempt = attempts.get();
                attempts.set(attempt + 1);
                if attempt == 0 {
                    Ok(crate::remote::Pushed::Blocked {
                        intent: agh::IntentId::parse("66-fc387e8b41a0").unwrap(),
                        why: "pre-send".into(),
                    })
                } else {
                    assert!(waited.get() && refreshed.get());
                    assert_eq!(observed, "unchanged");
                    Ok(crate::remote::Pushed::Done)
                }
            },
            &|intent| {
                assert_eq!(intent.as_str(), "66-fc387e8b41a0");
                waited.set(true);
                Ok(())
            },
            &|| {
                assert!(waited.get());
                refreshed.set(true);
                Ok(match observed {
                    "unchanged" => PushObservation::Unchanged,
                    "moved" => PushObservation::Moved("fresh-main".into()),
                    _ => PushObservation::Landed,
                })
            },
            &|_| panic!("a pre-send obstruction is not generic rejection"),
            &|_| panic!("a pre-send obstruction is not an unsent write"),
        );
        assert_eq!(
            result,
            Ok((observed == "moved").then(|| "fresh-main".into()))
        );
        assert_eq!(attempts.get(), if observed == "unchanged" { 2 } else { 1 });
        assert_eq!(races, 3);
    }
}

#[test]
fn foreign_intent_recovery_keeps_one_budget_and_never_retries_unknown_or_refused_writes() {
    use std::cell::Cell;
    let attempts = Cell::new(0);
    let waits = Cell::new(0);
    let mut races = COMPOSES - 2;
    let result = push_counting(
        &mut races,
        &|| {
            attempts.set(attempts.get() + 1);
            Ok(crate::remote::Pushed::Blocked {
                intent: agh::IntentId::parse(&format!("{}-000000000000", attempts.get())).unwrap(),
                why: "pre-send".into(),
            })
        },
        &|_| {
            waits.set(waits.get() + 1);
            Ok(())
        },
        &|| Ok(PushObservation::Unchanged),
        &|_| unreachable!(),
        &|_| unreachable!(),
    );
    assert!(result.is_err());
    assert_eq!((races, attempts.get(), waits.get()), (COMPOSES, 3, 2));

    for mode in ["unknown", "rejected", "read-failed", "deadline"] {
        let attempts = Cell::new(0);
        let mut races = 0;
        let result = push_counting(
            &mut races,
            &|| {
                attempts.set(attempts.get() + 1);
                match mode {
                    "unknown" => Err("unknown own outcome".into()),
                    "rejected" => Ok(crate::remote::Pushed::Rejected("refused in place".into())),
                    _ => Ok(crate::remote::Pushed::Blocked {
                        intent: agh::IntentId::parse("66-fc387e8b41a0").unwrap(),
                        why: "pre-send".into(),
                    }),
                }
            },
            &|_| Err(mode.into()),
            &|| panic!("no refresh after failed wait"),
            &|why| Err(why.into()),
            &|_| panic!("none of these is an unsent write"),
        );
        assert!(result.is_err());
        assert_eq!(attempts.get(), 1);
    }
}

#[test]
fn a_new_composition_attempt_keeps_the_original_push_deadline() {
    use std::cell::Cell;
    let now = Cell::new(0);
    let until = 10;
    let sent = Cell::new(0);
    let attempt = || {
        if now.get() >= until {
            return Err("original deadline".into());
        }
        sent.set(sent.get() + 1);
        Ok(crate::remote::Pushed::Blocked {
            intent: agh::IntentId::parse("66-fc387e8b41a0").unwrap(),
            why: "pre-send".into(),
        })
    };
    let mut races = 0;
    let first = push_counting(
        &mut races,
        &attempt,
        &|_| Ok(()),
        &|| {
            now.set(until);
            Ok(PushObservation::Moved("fresh-main".into()))
        },
        &|_| unreachable!(),
        &|_| unreachable!(),
    );
    assert_eq!(first, Ok(Some("fresh-main".into())));
    // The driver passes the same absolute bound after its clean composition. An expired
    // attempt cannot even inspect another blocker, much less send another atomic batch.
    let second = push_counting(
        &mut races,
        &attempt,
        &|_| panic!("no new wait"),
        &|| panic!("no new observation"),
        &|_| unreachable!(),
        &|_| unreachable!(),
    );
    assert_eq!(second, Err("original deadline".into()));
    assert_eq!((sent.get(), races), (1, 1));
}

/// A WRITE AGH SAYS IT DID NOT SEND IS MADE AGAIN, ONCE ITS PAUSE IS ALLOWED AND MAIN IS READ
/// AFRESH: the same landing goes again only onto a main still where it was. NEGATIVE CONTROLS:
/// a main that moved meanwhile ends the attempt with its tip (the landing is composed anew), one
/// that holds the landing took it, and once no further attempt is allowed nothing more is read
/// or sent.
#[test]
fn an_unsent_push_is_made_again_only_after_its_pause_and_a_fresh_read_of_main() {
    use std::cell::Cell;
    for (observed, allowed) in [
        ("unchanged", 2),
        ("moved", 2),
        ("landed", 2),
        ("unchanged", 1),
        ("unchanged", 0),
    ] {
        let attempts = Cell::new(0);
        let pauses = Cell::new(0);
        let reads = Cell::new(0);
        let mut races = 0;
        let result = push_counting(
            &mut races,
            &|| {
                // Each attempt after the first follows a pause and a read of main.
                assert_eq!(
                    (pauses.get(), reads.get()),
                    (attempts.get(), attempts.get())
                );
                attempts.set(attempts.get() + 1);
                Ok(if attempts.get() <= 2 {
                    crate::remote::Pushed::Unsent("the network went away".into())
                } else {
                    crate::remote::Pushed::Done
                })
            },
            &|_| panic!("an unsent write waits on no intent"),
            &|| {
                assert_eq!(pauses.get(), reads.get() + 1, "read only after the pause");
                reads.set(reads.get() + 1);
                Ok(match observed {
                    "unchanged" => PushObservation::Unchanged,
                    "moved" => PushObservation::Moved("fresh-main".into()),
                    _ => PushObservation::Landed,
                })
            },
            &|_| panic!("an unsent write is no rejection"),
            &|why| {
                assert_eq!(why, "the network went away");
                if pauses.get() == allowed {
                    return Err(format!("{why}; stopped"));
                }
                pauses.set(pauses.get() + 1);
                Ok(())
            },
        );
        let (want, sent) = match (observed, allowed) {
            ("unchanged", 2) => (Ok(None), 3),
            ("unchanged", 1) => (Err("the network went away; stopped".to_owned()), 2),
            ("unchanged", _) => (Err("the network went away; stopped".to_owned()), 1),
            ("moved", _) => (Ok(Some("fresh-main".to_owned())), 1),
            _ => (Ok(None), 1),
        };
        assert_eq!(
            (result, attempts.get()),
            (want, sent),
            "{observed} {allowed}"
        );
        assert_eq!(races, 0, "an unsent write is no composition race");
    }
}

/// A WRITE REFUSED WITH NO EFFECT STOPS THE PUSH, NAMING THE REFUSAL
/// ([`crate::remote::Pushed::Refused`]), whether agh refused it before it sent anything (no
/// credential, no permission, input it refuses) or GitHub rejected the push it received (a
/// branch rule's `[remote rejected]`, which agh reports with no side effect): it is not made
/// again, no pause is taken, main is not read and no intent is waited on, and the error says the
/// push was refused with no effect and nothing is left to settle, never that agh refused it
/// before it sent anything, which GitHub's rejection contradicts. `Wire::push` keeps a write to
/// settle only for an error from the push itself, so this one leaves none.
#[test]
fn a_push_refused_with_no_effect_stops_naming_the_refusal() {
    use std::cell::Cell;
    for refused in [
        "Agh remote operation exited Some(4): routed identity dev is not let in",
        "Agh remote operation exited Some(1): GitHub refused the push: !\trefs/heads/main:refs/heads/\
         main\t[remote rejected] (push declined due to repository rule violations)",
    ] {
        let attempts = Cell::new(0);
        let mut races = 0;
        let result = push_counting(
            &mut races,
            &|| {
                attempts.set(attempts.get() + 1);
                Ok(crate::remote::Pushed::Refused(refused.into()))
            },
            &|_| panic!("a refused write waits on no intent"),
            &|| panic!("a refused write reads no main"),
            &|_| panic!("a refused write is no rejection to settle against main"),
            &|_| panic!("a refused write is not made again"),
        );
        let why = result.expect_err("the push stops");
        assert!(
            why.starts_with(refused)
                && why.contains("refused with no effect (by agh or by GitHub)")
                && why.contains("nothing is left to settle")
                && !why.contains("before it sent anything"),
            "{why}"
        );
        assert_eq!((attempts.get(), races), (1, 0), "{refused}");
    }
}

#[test]
fn resource_controls_cannot_replace_the_landing_check_scope() {
    let args = |values: &[&str]| {
        values
            .iter()
            .map(|value| (*value).to_owned())
            .collect::<Vec<_>>()
    };
    assert_eq!(
        options(&[]),
        Some(LandResources {
            admit: aterm_capacity::Admit::Wait,
            timeout: None,
            test_cores: None,
        })
    );
    assert_eq!(
        options(&args(&["--admit", "now"])),
        Some(LandResources {
            admit: aterm_capacity::Admit::Now,
            timeout: None,
            test_cores: None,
        })
    );
    assert_eq!(
        options(&args(&["--timeout=off", "--admit=wait"])),
        Some(LandResources {
            admit: aterm_capacity::Admit::Wait,
            timeout: Some("off".into()),
            test_cores: None,
        })
    );
    assert_eq!(
        options(&args(&["--admit", "now", "--timeout", "21600"])),
        Some(LandResources {
            admit: aterm_capacity::Admit::Now,
            timeout: Some("21600".into()),
            test_cores: None,
        })
    );
    for flags in [
        &["--test-cores", "4", "--admit=now", "--timeout=21600"][..],
        &["--timeout", "21600", "--test-cores=4", "--admit", "now"],
    ] {
        assert_eq!(
            options(&args(flags)),
            Some(LandResources {
                admit: aterm_capacity::Admit::Now,
                timeout: Some("21600".into()),
                test_cores: std::num::NonZeroU32::new(4),
            })
        );
    }
    for flags in [
        &["--admit"][..],
        &["--admit", "other"],
        &["--timeout"],
        &["--timeout", "later"],
        &["--checks", "fmt"],
        &["-p", "one"],
        &["--base", "HEAD"],
        &["--admit", "now", "--all"],
        &["--admit", "now", "--admit=wait"],
        &["--timeout", "off", "--timeout=30"],
        &["--test-cores"],
        &["--test-cores", "0"],
        &["--test-cores=-1"],
        &["--test-cores=no"],
        &["--test-cores="],
        &["--test-cores=4294967296"],
        &["--test-cores=4", "--checks", "fmt"],
        &["--test-cores=4", "--test-cores", "2"],
        &["--resume"],
        &["--resume", "handoff", "--admit", "now"],
        &["--admit", "now", "--resume", "handoff"],
    ] {
        assert_eq!(options(&args(flags)), None);
    }
    // The handoff's own word is `--resume <file>` alone: with anything else it is usage.
    let scratch = aterm_tempfile::tempdir().expect("scratch");
    for flags in [
        &["--resume"][..],
        &["--admit", "now", "--resume", "handoff"],
    ] {
        assert_eq!(run(&args(flags), scratch.path()), exit::USAGE, "{flags:?}");
    }
    let cmd = judgment_command(
        Path::new("/verifier"),
        Path::new("/checkout"),
        "main-sha",
        None,
        Path::new("/events"),
        &LandResources {
            admit: aterm_capacity::Admit::Now,
            timeout: Some("off".into()),
            test_cores: std::num::NonZeroU32::new(4),
        },
        &crate::EnvSnapshot::default(),
    )
    .unwrap();
    let words: Vec<_> = cmd
        .get_args()
        .map(|arg| arg.to_string_lossy().into_owned())
        .collect();
    #[cfg(unix)]
    assert_eq!(
        &words[..4],
        &[
            aterm_uds::tether::VERB.to_owned(),
            std::process::id().to_string(),
            "/verifier".to_owned(),
            "/verifier".to_owned()
        ]
    );
    let words = &words[if cfg!(unix) { 4 } else { 0 }..];
    // Every judgment of one landing names the landing's own start as when its work was queued,
    // so a re-judge keeps the landing's place in the judgment turn's queue.
    let since = words
        .iter()
        .position(|word| word == crate::cli::QUEUED_SINCE)
        .map(|at| words[at + 1].clone())
        .expect("a judgment names when its landing was queued");
    let since_ms: u64 = since.parse().expect("unix ms");
    assert!(
        since_ms > 0 && since_ms <= aterm_time::unix_millis().unwrap(),
        "{since_ms}"
    );
    let expected: Vec<String> = [
        "--base",
        "main-sha",
        "--events",
        "/events",
        crate::cli::EVENTS_FILE_ONLY,
        "--admit",
        "now",
        crate::cli::QUEUED_SINCE,
        &since,
        "--fail-fast",
        "--timeout",
        "off",
        "--test-cores",
        "4",
    ]
    .into_iter()
    .map(str::to_owned)
    .collect();
    assert_eq!(words, expected);
    // A judgment stops at its first red, and keeps its events in its events file alone.
    let parsed = crate::cli::parse(words).expect("the judgment's command parses");
    assert!(parsed.fail_fast && parsed.events_file_only);
    // The round comes later in the landing than its judgment: a stamp of now would not keep the
    // judgment's place.
    std::thread::sleep(std::time::Duration::from_millis(2));
    // A round runs the dev set as a judgment does, since the tip it merged, and names the judged
    // commit and the file of its shared paths: the child selects its crates from those alone.
    let cmd = judgment_command(
        Path::new("/verifier"),
        Path::new("/checkout"),
        "tip-sha",
        Some(Round {
            judged: "judged-sha",
            shared: Path::new("/shared"),
            readers: None,
            mechanical: true,
        }),
        Path::new("/events"),
        &LandResources {
            admit: aterm_capacity::Admit::Wait,
            timeout: None,
            test_cores: None,
        },
        &crate::EnvSnapshot::default(),
    )
    .unwrap();
    let words: Vec<_> = cmd
        .get_args()
        .map(|arg| arg.to_string_lossy().into_owned())
        .collect();
    let words = &words[if cfg!(unix) { 4 } else { 0 }..];
    assert_eq!(
        words,
        [
            "--base",
            "tip-sha",
            "--judged",
            "judged-sha",
            "--shared",
            "/shared",
            "--events",
            "/events",
            crate::cli::EVENTS_FILE_ONLY,
            "--admit",
            "wait",
            crate::cli::QUEUED_SINCE,
            &since,
            "--fail-fast",
        ]
    );
    let parsed = crate::cli::parse(words).expect("the round's command parses");
    // A round stops at its first red, as a judgment does.
    assert!(parsed.fail_fast && parsed.events_file_only);
    assert_eq!(
        parsed.queued_since,
        Some(since_ms),
        "the round keeps the judgment's place"
    );
    assert_eq!(
        parsed.checks,
        crate::cli::parse(["--base", "tip-sha"]).unwrap().checks
    );
    assert!(parsed.checks.contains(&crate::checks::Check::Test));
    assert_eq!(
        (
            parsed.base.as_deref(),
            parsed.judged.as_deref(),
            parsed.shared,
            parsed.by_hand
        ),
        (
            Some("tip-sha"),
            Some("judged-sha"),
            Some(PathBuf::from("/shared")),
            false
        )
    );
    // The round of a merge Git did not make says so, after its shared paths: the child then
    // judges no shared path's member alone. NEGATIVE CONTROL: the round above, of Git's own merge, does not.
    let cmd = judgment_command(
        Path::new("/verifier"),
        Path::new("/checkout"),
        "tip-sha",
        Some(Round {
            judged: "judged-sha",
            shared: Path::new("/shared"),
            readers: None,
            mechanical: false,
        }),
        Path::new("/events"),
        &LandResources {
            admit: aterm_capacity::Admit::Wait,
            timeout: None,
            test_cores: None,
        },
        &crate::EnvSnapshot::default(),
    )
    .unwrap();
    let by_hand: Vec<_> = cmd
        .get_args()
        .map(|arg| arg.to_string_lossy().into_owned())
        .collect();
    let by_hand = &by_hand[if cfg!(unix) { 4 } else { 0 }..];
    assert_eq!(
        by_hand[..7],
        [
            "--base",
            "tip-sha",
            "--judged",
            "judged-sha",
            "--shared",
            "/shared",
            crate::cli::BY_HAND,
        ]
    );
    assert_eq!(by_hand[7..], words[6..]);
    assert!(!words.iter().any(|word| word == crate::cli::BY_HAND));
    assert!(crate::cli::parse(by_hand).expect("it parses").by_hand);
    // The round of a merge whose shared Markdown a file may read names the file of those
    // readers, after its shared paths: the child judges their crates too. NEGATIVE CONTROL:
    // the first round above, which no file reads, names none.
    let cmd = judgment_command(
        Path::new("/verifier"),
        Path::new("/checkout"),
        "tip-sha",
        Some(Round {
            judged: "judged-sha",
            shared: Path::new("/shared"),
            readers: Some(Path::new("/readers")),
            mechanical: true,
        }),
        Path::new("/events"),
        &LandResources {
            admit: aterm_capacity::Admit::Wait,
            timeout: None,
            test_cores: None,
        },
        &crate::EnvSnapshot::default(),
    )
    .unwrap();
    let reading: Vec<_> = cmd
        .get_args()
        .map(|arg| arg.to_string_lossy().into_owned())
        .collect();
    let reading = &reading[if cfg!(unix) { 4 } else { 0 }..];
    assert_eq!(reading[..6], words[..6]);
    assert_eq!(reading[6..8], [crate::cli::READERS, "/readers"]);
    assert_eq!(reading[8..], words[6..]);
    assert!(!words.iter().any(|word| word == crate::cli::READERS));
    assert_eq!(
        crate::cli::parse(reading).expect("it parses").readers,
        Some(PathBuf::from("/readers"))
    );
}

/// A ROUND'S DELTA REACHES ITS CHILD WHOLE: the files [`round_files`] writes, named by the words
/// [`judgment_command`] gives the child, are read back by the child ([`crate::round_of`]) as
/// the round the delta is: its judged commit, its shared paths, the files that may read its
/// shared Markdown, and whether Git made the merge. NEGATIVE CONTROL: a delta no file may read
/// writes no readers file and names none, and the child reads no reader.
#[test]
fn a_rounds_delta_reaches_its_child_as_the_round_it_names() {
    let reading = vec![
        "crates/b/tests/rows.rs".to_owned(),
        "crates/c/src/lib.rs".to_owned(),
    ];
    for readers in [reading, Vec::new()] {
        let delta = Delta {
            judged: "judged-sha".to_owned(),
            shared: vec!["crates/b/SPECS.md".to_owned(), "docs/X.md".to_owned()],
            readers: readers.clone(),
            mechanical: true,
        };
        let dir = aterm_tempfile::tempdir().expect("scratch");
        let (shared, read) = round_files(dir.path(), &delta).expect("the round's files");
        assert_eq!(read.is_some(), !readers.is_empty(), "{readers:?}");
        let cmd = judgment_command(
            Path::new("/verifier"),
            Path::new("/checkout"),
            "tip-sha",
            Some(Round {
                judged: &delta.judged,
                shared: &shared,
                readers: read.as_deref(),
                mechanical: delta.mechanical,
            }),
            Path::new("/events"),
            &LandResources {
                admit: aterm_capacity::Admit::Wait,
                timeout: None,
                test_cores: None,
            },
            &crate::EnvSnapshot::default(),
        )
        .unwrap();
        let words: Vec<_> = cmd
            .get_args()
            .map(|arg| arg.to_string_lossy().into_owned())
            .collect();
        let words = &words[if cfg!(unix) { 4 } else { 0 }..];
        let parsed = crate::cli::parse(words).expect("the round's words parse");
        let round = crate::round_of(&parsed, dir.path())
            .expect("the round's files read")
            .expect("a round");
        assert_eq!(
            round,
            crate::changed::Round {
                judged: delta.judged.clone(),
                shared: delta.shared.clone(),
                readers,
                mechanical: true,
            }
        );
    }
}

/// Every git of a scripted landing, the landing's own included. Automatic maintenance is off: a
/// fixture holds a few dozen objects, far below any threshold, so each commit, merge, fetch and
/// push would only start a `git maintenance run --auto` that decides there is nothing to do.
fn git_cmd(dir: &Path) -> Command {
    let mut cmd = aterm_uds::fixture_git::git();
    cmd.current_dir(dir)
        .env("GIT_AUTHOR_NAME", "Andrew Yates")
        .env("GIT_AUTHOR_EMAIL", "test@example.invalid")
        .env("GIT_COMMITTER_NAME", "Andrew Yates")
        .env("GIT_COMMITTER_EMAIL", "test@example.invalid")
        .env("GIT_TERMINAL_PROMPT", "0")
        .env("GIT_CONFIG_COUNT", "1")
        .env("GIT_CONFIG_KEY_0", "maintenance.auto")
        .env("GIT_CONFIG_VALUE_0", "false");
    cmd
}

fn git(dir: &Path, args: &[&str]) -> String {
    let mut cmd = git_cmd(dir);
    cmd.args(args);
    let out = aterm_uds::deadline::output_within(&mut cmd, aterm_spec::wait::MUST_HAPPEN)
        .unwrap_or_else(|e| panic!("git {args:?}: {e}"));
    assert!(
        out.status.success(),
        "git {args:?}: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    String::from_utf8_lossy(&out.stdout).trim().to_owned()
}

/// A bare origin whose main holds `a/x.txt`, `b/y.txt` and an eight-line `shared.txt`; a work
/// clone on branch `feature`; and a rival clone that moves main.
struct World {
    _dir: aterm_tempfile::TempDir,
    origin: PathBuf,
    work: PathBuf,
    rival: PathBuf,
}

const SHARED: &str = "1\n2\n3\n4\n5\n6\n7\n8\n";

/// A fresh [`World`], laid from the one [`build_world`] made in this process ([`Laid`]).
fn world() -> World {
    static BUILT: std::sync::OnceLock<Laid> = std::sync::OnceLock::new();
    let built = BUILT.get_or_init(|| {
        let dir = aterm_tempfile::tempdir().expect("scratch");
        build_world(dir.path());
        // The seed only pushed origin's first main; no world reads it.
        std::fs::remove_dir_all(dir.path().join("seed")).expect("the seed");
        Laid::read(dir.path())
    });
    let dir = aterm_tempfile::tempdir().expect("scratch");
    let root = dir.path().to_path_buf();
    built.lay(&root);
    World {
        _dir: dir,
        origin: root.join("origin.git"),
        work: root.join("work"),
        rival: root.join("rival"),
    }
}

/// [`World`]'s repositories under `root`: the bare origin, the seed that pushed its main, and the
/// work and rival clones.
fn build_world(root: &Path) {
    let origin = root.join("origin.git");
    let seed = root.join("seed");
    git(root, &["init", "-q", "--bare", "-b", "main", path(&origin)]);
    git(root, &["init", "-q", "-b", "main", path(&seed)]);
    for (file, text) in [
        ("a/x.txt", "a\n"),
        ("b/y.txt", "b\n"),
        ("shared.txt", SHARED),
    ] {
        write(&seed, file, text);
    }
    git(&seed, &["add", "-A"]);
    git(&seed, &["commit", "-q", "-m", "seed"]);
    git(&seed, &["remote", "add", "origin", path(&origin)]);
    git(&seed, &["push", "-q", "origin", "main"]);
    let work = root.join("work");
    git(root, &["clone", "-q", path(&origin), path(&work)]);
    git(
        root,
        &["clone", "-q", path(&origin), path(&root.join("rival"))],
    );
    git(&work, &["checkout", "-q", "-b", "feature"]);
}

/// A directory of repositories read into memory once, so each [`world`] is a copy of its bytes
/// and modes instead of the two inits, a commit, a push and two clones (with every transport
/// process they start) that made it. The only recorded path Git acts on is a clone's origin in
/// its `config`, which [`Laid::lay`] rewrites to the copy's own (a reflog's `clone: from` message
/// keeps the built path as text nothing reads). The `*.sample` scripts
/// `git init` leaves in `hooks/` are left out: Git runs no hook by that name.
struct Laid {
    /// Where the repositories were built, as the clones' `config` files spell it.
    from: String,
    /// Each entry relative to the root, in walk order (a directory before what it holds).
    entries: Vec<(PathBuf, Node)>,
}

/// One entry of a [`Laid`] tree.
enum Node {
    Dir,
    File { bytes: Vec<u8>, mode: u32 },
}

impl Laid {
    fn read(root: &Path) -> Self {
        fn walk(root: &Path, rel: &Path, entries: &mut Vec<(PathBuf, Node)>) {
            use std::os::unix::fs::PermissionsExt as _;
            let mut names: Vec<_> = std::fs::read_dir(root.join(rel))
                .expect("read the built world")
                .map(|entry| entry.expect("an entry").file_name())
                .collect();
            names.sort();
            for name in names {
                let rel = rel.join(name);
                let meta = std::fs::symlink_metadata(root.join(&rel)).expect("stat");
                assert!(
                    !meta.is_symlink(),
                    "{}: a built world holds no link",
                    rel.display()
                );
                let sample = rel.parent().is_some_and(|dir| dir.ends_with("hooks"))
                    && rel.extension().is_some_and(|ext| ext == "sample");
                if sample {
                    continue;
                }
                if meta.is_dir() {
                    entries.push((rel.clone(), Node::Dir));
                    walk(root, &rel, entries);
                } else {
                    let bytes = std::fs::read(root.join(&rel)).expect("read");
                    let mode = meta.permissions().mode();
                    entries.push((rel, Node::File { bytes, mode }));
                }
            }
        }
        let mut entries = Vec::new();
        walk(root, Path::new(""), &mut entries);
        Self {
            from: path(root).to_owned(),
            entries,
        }
    }

    /// Write every entry under `root` (an empty directory), each `config` naming `root` where it
    /// named the built one.
    fn lay(&self, root: &Path) {
        use std::os::unix::fs::PermissionsExt as _;
        for (rel, entry) in &self.entries {
            let at = root.join(rel);
            let Node::File { bytes, mode } = entry else {
                std::fs::create_dir(&at).expect("a world's directory");
                continue;
            };
            if rel.file_name().is_some_and(|name| name == "config") {
                let text = String::from_utf8(bytes.clone()).expect("a UTF-8 git config");
                std::fs::write(&at, text.replace(&self.from, path(root))).expect("config");
            } else {
                std::fs::write(&at, bytes).expect("a world's file");
            }
            std::fs::set_permissions(&at, std::fs::Permissions::from_mode(*mode)).expect("mode");
        }
    }
}

fn path(p: &Path) -> &str {
    p.to_str().expect("a UTF-8 scratch path")
}

fn write(dir: &Path, file: &str, text: &str) {
    let at = dir.join(file);
    std::fs::create_dir_all(at.parent().expect("a parent")).expect("dirs");
    std::fs::write(at, text).expect("write");
}

/// Commit `text` at `file` in `dir`: the commit.
fn commit(dir: &Path, file: &str, text: &str) -> String {
    write(dir, file, text);
    git(dir, &["add", "-A"]);
    git(dir, &["commit", "-q", "-m", &format!("edit {file}")]);
    git(dir, &["rev-parse", "HEAD"])
}

/// Move origin's main from the rival clone: `text` at `file`, pushed. The new tip.
fn rival_moves(world: &World, file: &str, text: &str) -> String {
    git(&world.rival, &["fetch", "-q", "origin"]);
    git(
        &world.rival,
        &["checkout", "-q", "-B", "main", "origin/main"],
    );
    let tip = commit(&world.rival, file, text);
    git(&world.rival, &["push", "-q", "origin", "main"]);
    tip
}

/// Move origin's main from the rival clone by one more line of `shared.txt` (1-based `line`),
/// over every move before it. The new tip.
fn rival_moves_line(world: &World, line: usize, text: &str) -> String {
    git(&world.rival, &["fetch", "-q", "origin"]);
    git(
        &world.rival,
        &["checkout", "-q", "-B", "main", "origin/main"],
    );
    let now = std::fs::read_to_string(world.rival.join("shared.txt")).expect("shared");
    let mut lines: Vec<&str> = now.lines().collect();
    lines[line - 1] = text;
    let tip = commit(
        &world.rival,
        "shared.txt",
        &format!("{}\n", lines.join("\n")),
    );
    git(&world.rival, &["push", "-q", "origin", "main"]);
    tip
}

fn origin_main(world: &World) -> String {
    git(&world.work, &["ls-remote", "origin", "refs/heads/main"])
        .split_whitespace()
        .next()
        .expect("main")
        .to_owned()
}

/// The receipt git itself reads off `commit`'s trailers, as the harness will.
fn stated(world: &World, commit: &str) -> Option<Stated> {
    let keys: Vec<String> = KEYS.iter().map(|k| format!("key={k}")).collect();
    let format = format!("--format=%(trailers:{},unfold)", keys.join(","));
    Stated::of_trailers(&git(&world.work, &["log", "-1", &format, commit]))
}

fn row(check: &str, word: &str) -> CheckRow {
    CheckRow {
        check: check.into(),
        word: word.into(),
        ..CheckRow::default()
    }
}

fn red(check: &str, ids: &[&str]) -> CheckRow {
    CheckRow {
        check: check.into(),
        word: "FAIL".into(),
        findings: ids.iter().map(|id| (*id).to_owned()).collect(),
        findings_total: ids.len(),
        ..CheckRow::default()
    }
}

/// A check the run's first red stopped: NOT RUN, by the halt ([`CheckRow::stopped`]).
fn stopped(check: &str) -> CheckRow {
    CheckRow {
        stopped: true,
        ..row(check, "NOT RUN")
    }
}

/// Where a landing's closing verdict ([`verdict`]) begins in what it said: the lines before it,
/// and the verdict.
fn split_verdict(said: &[String]) -> (&[String], &[String]) {
    let at = said
        .iter()
        .position(|line| line.starts_with("verify land: VERDICT"))
        .unwrap_or(said.len());
    said.split_at(at)
}

/// A run of `checks` that exited `code` in `secs`, its record whole: its `run-finished` event
/// names that exit and each check's word.
fn ran(code: i32, checks: Vec<CheckRow>, secs: u64) -> Judged {
    let finished = Finished {
        code,
        checks: checks
            .iter()
            .map(|row| (row.check.clone(), row.word.clone()))
            .collect(),
    };
    Judged {
        code,
        checks,
        secs,
        finished: Some(finished),
        ..Judged::default()
    }
}

fn green() -> Judged {
    ran(
        exit::PASS,
        vec![row("fmt", "ok"), row("lint", "skip"), row("test", "ok")],
        7,
    )
}

const THEIRS: &str = "-p b --lib -- old_red";

/// One landing of `world`'s work: the exit, what it said, and each judged base. `judge` sees the
/// base and how many judgments came before.
fn land_with(
    world: &World,
    branch: Reach,
    mut judge: impl FnMut(&str, usize) -> Judged,
) -> (i32, Vec<String>, Vec<String>) {
    land_watching(world, branch, |base, n, _| Ran::Ended(judge(base, n)))
}

/// [`land_with`], whose `judge` also gets the running judgment's poll of main: a call stands
/// for a stage boundary or [`POLL`] passing, and its answer says whether the run ended
/// ([`Ran::Ended`]) or a `true` interrupted it ([`Ran::Interrupted`]).
fn land_watching(
    world: &World,
    branch: Reach,
    judge: impl FnMut(&str, usize, Poll<'_>) -> Ran,
) -> (i32, Vec<String>, Vec<String>) {
    land_through(world, &git_cmd, branch, judge)
}

/// [`land_watching`] through the git seam `git`. A recheck fails the test: these worlds share
/// no Markdown.
fn land_through(
    world: &World,
    git: &dyn Fn(&Path) -> Command,
    branch: Reach,
    judge: impl FnMut(&str, usize, Poll<'_>) -> Ran,
) -> (i32, Vec<String>, Vec<String>) {
    let (code, said, bases, _) = land_rechecking(world, git, branch, judge, |base| {
        panic!("no recheck was expected (base {base})")
    });
    (code, said, bases)
}

/// A landing through [`land_rechecking`]: the exit, what it said, each judged base, and each
/// round's base and the HEAD it ran on.
type Landed = (i32, Vec<String>, Vec<String>, Vec<(String, String)>);

/// Why the scripted landings' shared paths reach every crate ([`whole_reach`]).
const WHOLE: &str = "the scripted reach of a shared path";

/// The reach of a scripted landing whose shared paths reach every crate, so a main that moves
/// into a file a crate may own dooms the judgment, and the branch's own reach is `branch`.
fn whole_reach(branch: &Reach) -> impl Fn(Option<&[String]>) -> Reach + '_ {
    move |paths| match paths {
        Some(_) => Reach::Everything(WHOLE.into()),
        None => branch.clone(),
    }
}

/// A reach of `names`.
fn reaches(names: &[&str]) -> Reach {
    Reach::Crates(names.iter().map(|name| (*name).to_owned()).collect())
}

/// [`land_through`], whose `recheck` stands for each round's run: it sees the base. Also each
/// round's base and the HEAD it ran on. The shared paths reach every crate ([`whole_reach`]), so
/// only shared Markdown no code reads is answered by a round.
fn land_rechecking(
    world: &World,
    git: &dyn Fn(&Path) -> Command,
    branch: Reach,
    judge: impl FnMut(&str, usize, Poll<'_>) -> Ran,
    recheck: impl FnMut(&str) -> Judged,
) -> Landed {
    land_wired(world, git, &live_remote, &no_retry, branch, judge, recheck)
}

/// The pause seam of a landing whose remote never fails: no step is made again.
fn no_retry(pause: Duration) {
    panic!("no remote step was expected to fail and be made again (pause {pause:?})");
}

/// [`land_rechecking`] through the push phase's remote seam `remote` and its `pause`.
fn land_wired(
    world: &World,
    git: &dyn Fn(&Path) -> Command,
    remote: &RemoteStep<'_>,
    pause: &dyn Fn(Duration),
    branch: Reach,
    judge: impl FnMut(&str, usize, Poll<'_>) -> Ran,
    mut recheck: impl FnMut(&str) -> Judged,
) -> Landed {
    let reach = whole_reach(&branch);
    land_seamed(world, git, remote, pause, &reach, judge, |base, _| {
        recheck(base)
    })
}

/// A landing whose shared paths reach what `reach` says (its argument the paths, or `None` for
/// the branch's own change), and whose `recheck` stands for each round's run: it sees the base
/// and the round's delta.
fn land_reaching(
    world: &World,
    reach: &dyn Fn(Option<&[String]>) -> Reach,
    judge: impl FnMut(&str, usize, Poll<'_>) -> Ran,
    recheck: impl FnMut(&str, &Delta) -> Judged,
) -> Landed {
    land_seamed(
        world,
        &git_cmd,
        &live_remote,
        &no_retry,
        reach,
        judge,
        recheck,
    )
}

/// [`land_wired`] with the reach seam `reach` and a `recheck` that sees each round's delta.
fn land_seamed(
    world: &World,
    git: &dyn Fn(&Path) -> Command,
    remote: &RemoteStep<'_>,
    pause: &dyn Fn(Duration),
    reach: &dyn Fn(Option<&[String]>) -> Reach,
    mut judge: impl FnMut(&str, usize, Poll<'_>) -> Ran,
    mut recheck: impl FnMut(&str, &Delta) -> Judged,
) -> Landed {
    let said = RefCell::new(Vec::new());
    let bases = RefCell::new(Vec::new());
    let rechecks = RefCell::new(Vec::new());
    let code = land(
        &world.work,
        &own(),
        &mut Seams {
            git,
            verifier: &mut |_: &Path| Ok(own_image()),
            judge: &mut |root: &Path, verifier: &Path, base: &str, poll: Poll<'_>| {
                assert_eq!(root, world.work);
                assert_eq!(verifier, own_image().path);
                let n = bases.borrow().len();
                bases.borrow_mut().push(base.to_owned());
                judge(base, n, poll)
            },
            records: quiet(),
            reach: &|root: &Path, _: &str, paths: Option<&[String]>| {
                assert_eq!(root, world.work);
                reach(paths)
            },
            recheck: &mut |root: &Path, verifier: &Path, base: &str, delta: &Delta| {
                assert_eq!(root, world.work);
                assert_eq!(verifier, own_image().path);
                let head = self::git(root, &["rev-parse", "HEAD"]);
                assert_eq!(
                    self::git(root, &["rev-parse", "HEAD^1"]),
                    delta.judged,
                    "a round's judged commit is its merge's first parent"
                );
                rechecks.borrow_mut().push((base.to_owned(), head));
                recheck(base, delta)
            },
            autofix: &mut |_: &Path, _: &[String]| Ok(()),
            hand_over: &mut |_: &Image, _: &Path, _: &Path| panic!("no handoff was expected"),
            // Printed too: a panic inside the landing (a remote step made again where none may
            // fail, [`no_retry`]) unwinds past `said`, and what the landing said before it is
            // the only record of which step failed and why.
            say: &mut |line: &str| {
                eprintln!("{line}");
                said.borrow_mut().push(line.to_owned());
            },
            remote,
            pause,
        },
    );
    (
        code,
        said.into_inner(),
        bases.into_inner(),
        rechecks.into_inner(),
    )
}

/// The branch's own reach: the crates `names`.
fn crates(names: &[&str]) -> Reach {
    reaches(names)
}

/// A check that did not run, a run that could not run or ended oddly, and a failed run that
/// named no failed check are never a pass.
#[test]
fn a_run_that_did_not_finish_green_is_never_a_pass() {
    let named = |code: i32, checks: Vec<CheckRow>| decide(&ran(code, checks, 1));
    assert_eq!(named(exit::PASS, green().checks), Decision::default());
    assert_eq!(
        named(exit::COULD_NOT_RUN, vec![row("export-scan", "NOT RUN")]).not_run,
        ["export-scan"]
    );
    assert!(!named(exit::COULD_NOT_RUN, Vec::new()).not_run.is_empty());
    assert!(!named(exit::PASS, Vec::new()).not_run.is_empty());
    assert!(!named(101, green().checks).not_run.is_empty());
    assert!(!named(exit::FAILED, green().checks).counted.is_empty());
}

/// A RECORD NOT WHOLE IS NEVER A PASS: a run whose `run-finished` event names a check no line
/// holds (that line lost), or is missing, or names another exit, did not run whole, however its
/// rows read. CONTROL: the same rows, whole, are a counted red and nothing else.
#[test]
fn a_record_not_whole_is_never_a_pass() {
    let whole = ran(
        exit::FAILED,
        vec![row("fmt", "ok"), red("test", &[THEIRS])],
        1,
    );
    assert_eq!(
        decide(&whole),
        Decision {
            counted: vec!["test".into()],
            ..Decision::default()
        }
    );
    let mut lost = whole.clone();
    if let Some(finished) = lost.finished.as_mut() {
        finished.checks.push(("lint".into(), "FAIL".into()));
    }
    let mut missing = whole.clone();
    missing.finished = None;
    let mut other = whole.clone();
    if let Some(finished) = other.finished.as_mut() {
        finished.code = exit::PASS;
    }
    let mut green_missing = green();
    green_missing.finished = None;
    for judged in [lost, missing, other, green_missing] {
        let d = decide(&judged);
        assert!(!d.not_run.is_empty(), "{judged:?}: {d:?}");
    }
}

/// The row reader takes what verify's own event writer writes: a failed test check's findings
/// and their total.
#[test]
fn a_check_row_reads_verifys_own_check_finished_event() {
    let result = crate::report::CheckResult {
        check: crate::Check::Test,
        outcome: crate::Outcome::Fail(crate::Severity::GateFailed),
        seconds: 3.5,
        detail: "1 test failed".into(),
        findings: vec![crate::ladder::Finding {
            id: THEIRS.into(),
            rerun: None,
            message: Vec::new(),
        }],
        rerun: "targo --unverified verify test".into(),
        stopped: false,
    };
    let line = crate::report::check_event(&result, &[]);
    assert_eq!(
        CheckRow::of_event(line.trim_end()),
        Some(CheckRow {
            detail: "1 test failed".into(),
            took: Some(Duration::from_millis(3500)),
            ..red("test", &[THEIRS])
        })
    );
    // What the halt stopped reads as stopped.
    let halted = crate::report::CheckResult {
        stopped: true,
        ..result.clone()
    };
    let line = crate::report::check_event(&halted, &[]);
    assert!(CheckRow::of_event(line.trim_end()).is_some_and(|row| row.stopped));
    assert_eq!(
        CheckRow::of_event("verify-event: {\"event\":\"stage-finished\"}"),
        None
    );
    let line = crate::report::run_event(&[result], exit::FAILED);
    assert_eq!(
        Finished::of_event(line.trim_end()),
        Some(Finished {
            code: exit::FAILED,
            checks: vec![("test".into(), "FAIL".into())],
        })
    );
    assert_eq!(CheckRow::of_event(line.trim_end()), None);
}

#[test]
fn land_requires_a_completed_terminal_line_but_preserves_later_partial_diagnostics() {
    let dir = aterm_tempfile::tempdir().unwrap();
    let path = dir.path().join("verify.events");
    let events = crate::event_log::EventLog::open(&path).unwrap();
    let result = crate::report::CheckResult {
        check: crate::Check::Fmt,
        outcome: crate::Outcome::Ok,
        seconds: 1.0,
        detail: String::new(),
        findings: Vec::new(),
        rerun: "targo --unverified verify fmt".into(),
        stopped: false,
    };
    events.write(&crate::report::check_event(&result, &[]));
    events.write(&crate::report::run_event(&[result], exit::PASS));
    let complete = std::fs::read_to_string(&path).unwrap();
    let baseline = Judged::from_events(&complete, exit::PASS, 7);
    assert!(baseline.whole());
    assert_eq!(baseline.checks.len(), 1);
    assert_eq!(baseline.secs, 7);
    assert_eq!(decide(&baseline), Decision::default());

    let unframed = complete.strip_suffix('\n').unwrap();
    let missing = Judged::from_events(unframed, exit::PASS, 7);
    assert_eq!(missing.checks, baseline.checks);
    assert!(missing.finished.is_none());
    assert!(!missing.whole());
    let decision = decide(&missing);
    assert!(!decision.not_run.is_empty());

    let crlf = Judged::from_events(&complete.replace('\n', "\r\n"), exit::PASS, 7);
    assert_eq!(crlf, baseline);
    let partial_tail = format!("{complete}verify-event: {{\"schema\":1,\"event\":\"disk\"");
    let with_tail = Judged::from_events(&partial_tail, exit::PASS, 7);
    assert_eq!(with_tail, baseline);
    assert_eq!(decide(&with_tail), Decision::default());
}

/// A green branch lands as one merge of main into it, stating its judgment: the tree judged, the
/// main commit its cone was computed from and each check's row; the checkout's branch is the
/// landing, clean, and its remote-tracking main names it.
#[test]
fn a_green_branch_lands_as_one_merge_stating_its_judgment() {
    let world = world();
    let feature = commit(&world.work, "a/x.txt", "a2\n");
    let main = rival_moves(&world, "b/y.txt", "b2\n");
    let (code, said, bases) = land_with(&world, crates(&["a"]), |_, _| green());
    assert_eq!(code, exit::PASS, "{said:#?}");
    assert_eq!(bases, std::slice::from_ref(&main));
    let landing = origin_main(&world);
    let parents = git(&world.work, &["rev-list", "--parents", "-n", "1", &landing]);
    assert_eq!(parents, format!("{landing} {feature} {main}"));
    let receipt = stated(&world, &landing).expect("the landing states its judgment");
    assert_eq!(receipt.checked.len(), 1);
    assert_eq!(
        receipt.checked[0].tree,
        git(&world.work, &["rev-parse", &format!("{landing}^{{tree}}")])
    );
    assert_eq!(receipt.checked[0].base, main);
    assert_eq!(
        receipt.checked[0]
            .rows
            .iter()
            .map(|r| format!("{}:{}", r.check, r.word))
            .collect::<Vec<_>>(),
        ["fmt:ok", "lint:skip", "test:ok"]
    );
    assert_eq!((receipt.composed_onto, receipt.inherited.len()), (None, 0));
    assert_eq!(git(&world.work, &["rev-parse", "HEAD"]), landing);
    assert_eq!(git(&world.work, &["rev-parse", "origin/main"]), landing);
    assert_eq!(git(&world.work, &["status", "--porcelain"]), "");
    assert_eq!(
        git(&world.work, &["log", "-1", "--format=%s", &landing]),
        "Merge origin/main into feature"
    );
}

/// A landing of `world`'s work whose fixes ([`Seams::autofix`]) turn each doubled space of a
/// file they are handed into one: the exit, what it said, and what the fixes were handed each
/// time. Its judgment asserts that it judges `a/x.txt` fixed and committed.
fn land_fixing(world: &World) -> (i32, Vec<String>, Vec<Vec<String>>) {
    let said = RefCell::new(Vec::new());
    let handed = RefCell::new(Vec::new());
    let code = land(
        &world.work,
        &own(),
        &mut Seams {
            git: &git_cmd,
            records: quiet(),
            verifier: &mut |_: &Path| Ok(own_image()),
            judge: &mut |root: &Path, _: &Path, _: &str, _: Poll<'_>| {
                assert_eq!(
                    std::fs::read_to_string(root.join("a/x.txt")).expect("a/x.txt"),
                    "a 2\n"
                );
                assert_eq!(self::git(root, &["status", "--porcelain"]), "");
                Ran::Ended(green())
            },
            reach: &|_: &Path, _: &str, _: Option<&[String]>| crates(&["a"]),
            recheck: &mut |_: &Path, _: &Path, base: &str, _: &Delta| {
                panic!("no recheck was expected (base {base})")
            },
            autofix: &mut |root: &Path, files: &[String]| {
                handed.borrow_mut().push(files.to_vec());
                for file in files {
                    let text = std::fs::read_to_string(root.join(file)).expect("a handed file");
                    std::fs::write(root.join(file), text.replace("  ", " ")).expect("a fix");
                }
                Ok(())
            },
            hand_over: &mut |_: &Image, _: &Path, _: &Path| panic!("no handoff was expected"),
            say: &mut |line: &str| said.borrow_mut().push(line.to_owned()),
            remote: &live_remote,
            pause: &no_retry,
        },
    );
    (code, said.into_inner(), handed.into_inner())
}

/// THE LANDING'S FIXES ARE COMMITTED INTO ITS MERGE: they are handed what HEAD holds unlike
/// main, what they write is amended into the landing's merge, which keeps its parents and its
/// subject, and the judgment judges the fixed tree, which the landing states. CONTROL: a branch
/// that already holds main's tip merges nothing, so the fix is a commit of its own on the
/// branch, under the landing's merge.
#[test]
fn the_landings_fixes_are_committed_into_its_merge_and_judged() {
    let world = world();
    let feature = commit(&world.work, "a/x.txt", "a  2\n");
    let main = rival_moves(&world, "b/y.txt", "b2\n");
    let (code, said, handed) = land_fixing(&world);
    assert_eq!(code, exit::PASS, "{said:#?}");
    assert_eq!(handed, [vec!["a/x.txt".to_owned()]]);
    let landing = origin_main(&world);
    assert_eq!(
        git(&world.work, &["rev-list", "--parents", "-n", "1", &landing]),
        format!("{landing} {feature} {main}")
    );
    assert_eq!(
        git(&world.work, &["log", "-1", "--format=%s", &landing]),
        "Merge origin/main into feature"
    );
    assert_eq!(
        git(&world.work, &["show", &format!("{landing}:a/x.txt")]),
        "a 2"
    );
    let receipt = stated(&world, &landing).expect("the landing states its judgment");
    assert_eq!(
        receipt.checked[0].tree,
        git(&world.work, &["rev-parse", &format!("{landing}^{{tree}}")])
    );
    assert!(
        said.iter()
            .any(|line| line.ends_with("wrote 1 file(s), committed into the merge: a/x.txt")),
        "{said:#?}"
    );

    let control = self::world();
    let feature = commit(&control.work, "a/x.txt", "a  2\n");
    let main = origin_main(&control);
    let (code, said, handed) = land_fixing(&control);
    assert_eq!(code, exit::PASS, "{said:#?}");
    assert_eq!(handed, [vec!["a/x.txt".to_owned()]]);
    let landing = origin_main(&control);
    assert_eq!(
        git(&control.work, &["show", &format!("{landing}:a/x.txt")]),
        "a 2"
    );
    let fix = git(&control.work, &["rev-parse", &format!("{landing}^1")]);
    assert_eq!(
        git(
            &control.work,
            &["rev-list", "--parents", "-n", "1", &landing]
        ),
        format!("{landing} {fix} {main}")
    );
    assert_eq!(
        git(&control.work, &["log", "-1", "--format=%P %s", &fix]),
        format!("{feature} {AUTOFIX_SUBJECT}")
    );
    assert!(
        said.iter()
            .any(|line| line
                .ends_with("wrote 1 file(s), committed as a commit of their own: a/x.txt")),
        "{said:#?}"
    );
}

/// With no fetch refspec, Git itself does not update origin/main after a push, as with agh.
#[test]
fn a_green_branch_refreshes_main_without_a_fetch_refspec() {
    let world = world();
    commit(&world.work, "a/x.txt", "a2\n");
    git(
        &world.work,
        &["config", "--unset-all", "remote.origin.fetch"],
    );
    let (code, said, _) = land_with(&world, crates(&["a"]), |_, _| green());
    assert_eq!(code, exit::PASS, "{said:#?}");
    let landing = origin_main(&world);
    assert_eq!(git(&world.work, &["rev-parse", "HEAD"]), landing);
    assert_eq!(git(&world.work, &["rev-parse", "origin/main"]), landing);
}

/// A BRANCH THAT ALREADY MERGED MAIN LANDS AS THAT MERGE, RESTATED: its parents and its own
/// message kept, the trailers added, and no second merge made over it. CONTROL:
/// [`a_green_branch_lands_as_one_merge_stating_its_judgment`], a branch that has not merged main,
/// lands as one new merge of it.
#[test]
fn a_branch_that_merged_main_by_hand_lands_as_that_merge_restated() {
    for branch in [Some("feature"), Some("feature\u{a0}"), None] {
        let world = world();
        let feature = commit(&world.work, "a/x.txt", "a2\n");
        let main = rival_moves(&world, "b/y.txt", "b2\n");
        git(&world.work, &["fetch", "-q", "origin"]);
        git(
            &world.work,
            &[
                "merge",
                "-q",
                "--no-ff",
                "-m",
                "Merge main by hand",
                "origin/main",
            ],
        );
        let judged = git(&world.work, &["rev-parse", "HEAD"]);
        match branch {
            Some("feature") => {}
            Some(name) => {
                git(&world.work, &["checkout", "-q", "-b", name]);
            }
            None => {
                git(&world.work, &["checkout", "-q", "--detach"]);
            }
        }
        let (code, said, bases) = land_with(&world, crates(&["a"]), |_, _| green());
        assert_eq!(code, exit::PASS, "{said:#?}");
        assert_eq!(bases, std::slice::from_ref(&main));
        let landing = origin_main(&world);
        assert_eq!(
            git(&world.work, &["rev-list", "--parents", "-n", "1", &landing]),
            format!("{landing} {feature} {main}"),
            "the hand merge's own parents: no second merge"
        );
        assert_eq!(
            git(&world.work, &["log", "-1", "--format=%s", &landing]),
            "Merge main by hand"
        );
        assert!(stated(&world, &landing).is_some(), "{said:#?}");
        if branch != Some("feature") {
            assert_eq!(
                git(&world.work, &["rev-parse", "refs/heads/feature"]),
                judged,
                "adoption must not move the similarly named or detached checkout's other branch"
            );
        }
        assert_eq!(git(&world.work, &["rev-parse", "HEAD"]), landing);
        if let Some(name) = branch {
            assert_eq!(
                git(&world.work, &["rev-parse", &format!("refs/heads/{name}")]),
                landing
            );
        } else {
            let plumbing = Git {
                root: &world.work,
                make: &git_cmd,
            };
            assert_eq!(
                plumbing
                    .status(&["symbolic-ref", "--quiet", "HEAD"], LOCAL)
                    .unwrap()
                    .status
                    .code(),
                Some(1)
            );
        }
    }
}

/// A CHECKOUT THAT MOVES OR CHANGES DURING THE RUN PUSHES NOTHING: the run judged files the
/// landing would not hold. An edit no commit holds, and a commit, made while the run ran, each
/// stop it. CONTROL: the same run, the checkout left alone, lands.
#[test]
fn a_checkout_that_moves_or_changes_during_the_run_pushes_nothing() {
    let world = world();
    commit(&world.work, "a/x.txt", "a2\n");
    let before = origin_main(&world);
    let (code, said, bases) = land_with(&world, crates(&["a"]), |_, _| {
        write(&world.work, "loose.txt", "edited during the run\n");
        green()
    });
    assert_eq!((code, bases.len()), (exit::COULD_NOT_RUN, 1), "{said:#?}");
    assert!(
        said.iter().any(|l| l.contains("changes no commit holds")),
        "{said:#?}"
    );
    assert_eq!(origin_main(&world), before);
    std::fs::remove_file(world.work.join("loose.txt")).expect("tidy");
    let (code, said, _) = land_with(&world, crates(&["a"]), |_, _| {
        commit(&world.work, "a/x.txt", "a3\n");
        green()
    });
    assert_eq!(code, exit::COULD_NOT_RUN, "{said:#?}");
    assert!(said.iter().any(|l| l.contains("HEAD moved")), "{said:#?}");
    assert_eq!(origin_main(&world), before);
    let (code, said, _) = land_with(&world, crates(&["a"]), |_, _| green());
    assert_eq!(code, exit::PASS, "{said:#?}");
    assert_ne!(origin_main(&world), before);
}

/// ONE LANDING RUNS IN A CHECKOUT AT A TIME: while one holds the checkout's landing lock, a
/// second refuses at once and says why; once the first ends, the next takes it.
#[test]
fn a_second_landing_of_a_checkout_refuses_while_one_runs() {
    let world = world();
    let git = Git {
        root: &world.work,
        make: &git_cmd,
    };
    let first = git.hold().expect("the first landing takes the checkout");
    let refused = git.hold().expect_err("a second landing refuses");
    assert!(refused.contains("another landing runs in"), "{refused}");
    drop(first);
    git.hold().expect("once the first ended, the next takes it");
}

/// A LANDING ASKED FOR A COMMIT LANDS THAT COMMIT OR NOTHING (`--at`, which `verify land --tab`
/// gives its worker): where the checkout's HEAD moved on after the ask, nothing is judged or
/// pushed. CONTROL: asked for the HEAD it reads, the same landing judges and lands.
#[test]
fn a_landing_asked_for_a_commit_lands_it_or_nothing() {
    let world = world();
    let asked = commit(&world.work, "a/x.txt", "a2\n");
    let head = commit(&world.work, "a/x.txt", "a3\n");
    let before = origin_main(&world);
    let branch = crates(&["a"]);
    let reach = whole_reach(&branch);
    let judged = RefCell::new(0);
    let land_at = |at: &str| {
        let said = RefCell::new(Vec::new());
        let code = land(
            &world.work,
            &Own {
                words: &[],
                image: OWN,
                at: Some(at),
            },
            &mut Seams {
                git: &git_cmd,
                verifier: &mut |_: &Path| Ok(own_image()),
                judge: &mut |_: &Path, _: &Path, _: &str, _: Poll<'_>| {
                    *judged.borrow_mut() += 1;
                    Ran::Ended(green())
                },
                records: quiet(),
                reach: &|_: &Path, _: &str, paths: Option<&[String]>| reach(paths),
                recheck: &mut |_, _, _, _| panic!("no round was expected"),
                autofix: &mut |_: &Path, _: &[String]| Ok(()),
                hand_over: &mut |_, _, _| panic!("nothing is handed over"),
                say: &mut |line: &str| said.borrow_mut().push(line.to_owned()),
                remote: &live_remote,
                pause: &no_retry,
            },
        );
        (code, said.into_inner())
    };
    let (code, said) = land_at(&asked);
    assert_eq!(
        (code, *judged.borrow()),
        (exit::COULD_NOT_RUN, 0),
        "{said:#?}"
    );
    assert!(
        said.iter()
            .any(|line| line.contains(&format!("HEAD is {head}, not {asked}"))),
        "{said:#?}"
    );
    assert_eq!(origin_main(&world), before, "nothing was pushed");
    let (code, said) = land_at(&head);
    assert_eq!((code, *judged.borrow()), (exit::PASS, 1), "{said:#?}");
    assert_ne!(origin_main(&world), before, "the asked commit landed");
}

/// A PUSH MAIN REFUSED WHILE IT STAYED PUT IS AN ERROR, NEVER A RETRY OR A MOVED MAIN: origin's
/// main is locked, so the push fails and main is where it was; nothing is composed or judged
/// again. CONTROL: unlocked, the same landing lands.
#[test]
fn a_push_main_refused_in_place_is_an_error_never_a_retry() {
    let world = world();
    commit(&world.work, "a/x.txt", "a2\n");
    let before = origin_main(&world);
    let lock = world.origin.join("refs/heads/main.lock");
    std::fs::write(&lock, "").expect("lock origin's main");
    let (code, said, bases) = land_with(&world, crates(&["a"]), |_, _| green());
    assert_eq!((code, bases.len()), (exit::COULD_NOT_RUN, 1), "{said:#?}");
    assert!(
        said.iter().any(|l| l.contains("COULD NOT RUN — git push")),
        "{said:#?}"
    );
    assert!(
        !said
            .iter()
            .any(|l| l.contains("moved to") || l.contains("composing")),
        "{said:#?}"
    );
    assert_eq!(origin_main(&world), before);
    std::fs::remove_file(&lock).expect("unlock");
    let (code, said, _) = land_with(&world, crates(&["a"]), |_, _| green());
    assert_eq!(code, exit::PASS, "{said:#?}");
}

/// A PUSH GIT REPORTED FAILED THAT MAIN TOOK IS LANDED: a connection lost after the server moved
/// main loses only the report, so a main that holds the landing (as its tip, or under commits
/// pushed after it) took it. NEGATIVE CONTROLS: a main still where it was refused it, an error;
/// a main moved without it moved.
#[test]
fn a_push_reported_failed_that_main_took_is_landed() {
    let world = world();
    let onto = origin_main(&world);
    let landing = commit(&world.work, "a/x.txt", "a2\n");
    let plumbing = Git {
        root: &world.work,
        make: &git_cmd,
    };
    let wire = || Wire::new(&plumbing, &live_remote, &no_retry, &|_| {}, &|| None);
    assert_eq!(
        wire().settle(&landing, &onto, "refused"),
        Err("refused".to_owned())
    );
    git(&world.work, &["push", "-q", "origin", "HEAD:main"]);
    assert_eq!(wire().settle(&landing, &onto, "report lost"), Ok(None));
    let past = rival_moves(&world, "b/y.txt", "b2\n");
    assert_eq!(wire().settle(&landing, &onto, "report lost"), Ok(None));
    let elsewhere = commit(&world.work, "a/x.txt", "a3\n");
    assert_eq!(wire().settle(&elsewhere, &onto, "moved"), Ok(Some(past)));
}

/// An uninitialised gitlink is reported before any clone. CONTROL: a checkout with no
/// submodules, as all the landing worlds above are, needs no import and updates locally.
#[test]
fn landing_does_not_clone_an_uninitialised_submodule() {
    let world = world();
    let plumbing = Git {
        root: &world.work,
        make: &git_cmd,
    };
    assert_eq!(plumbing.update_submodules(), Ok(()));
    let main = origin_main(&world);
    write(
        &world.work,
        ".gitmodules",
        "[submodule \"child\"]\npath = child\nurl = ../origin.git\n",
    );
    git(&world.work, &["add", ".gitmodules"]);
    git(
        &world.work,
        &[
            "update-index",
            "--add",
            "--cacheinfo",
            "160000",
            &main,
            "child",
        ],
    );
    git(
        &world.work,
        &["commit", "-qm", "add an uninitialised local child"],
    );
    assert_eq!(
        plumbing.update_submodules(),
        Err("initialise child through agh before verify land".into())
    );
    assert!(!world.work.join("child/.git").exists(), "no clone");
    assert!(git(&world.work, &["submodule", "status"]).starts_with('-'));
}

/// A world whose main vendors a local `child` repository at `vendor/sub`, merged into the work
/// checkout with the submodule initialised there, as a checkout is before verify land: the child,
/// and the commit it pins.
fn vendored_child(world: &World) -> (PathBuf, String) {
    let scratch = world.work.parent().expect("scratch");
    let child = scratch.join("child");
    git(scratch, &["init", "-q", "-b", "main", path(&child)]);
    let first = commit(&child, "lib.txt", "1\n");
    git(
        &world.rival,
        &[
            "-c",
            "protocol.file.allow=always",
            "submodule",
            "add",
            "-q",
            path(&child),
            "vendor/sub",
        ],
    );
    git(&world.rival, &["commit", "-qm", "vendor the child"]);
    git(&world.rival, &["push", "-q", "origin", "main"]);
    merge_main_alone(world);
    git(
        &world.work,
        &[
            "-c",
            "protocol.file.allow=always",
            "submodule",
            "update",
            "--init",
            "--no-fetch",
        ],
    );
    assert_eq!(
        git(&world.work.join("vendor/sub"), &["rev-parse", "HEAD"]),
        first
    );
    (child, first)
}

/// Merge origin's main into the work checkout as the land's agh fetch of main does: never
/// recursing into a submodule, which git's own fetch would do on demand.
fn merge_main_alone(world: &World) {
    git(
        &world.work,
        &["fetch", "-q", "--recurse-submodules=no", "origin"],
    );
    git(
        &world.work,
        &["merge", "-q", "--no-edit", "--no-verify", "origin/main"],
    );
}

/// Main pins `vendor/sub` at `object` (from the rival clone), merged into the work checkout.
fn main_pins(world: &World, object: &str) {
    git(&world.rival, &["fetch", "-q", "origin"]);
    git(&world.rival, &["merge", "-q", "--ff-only", "origin/main"]);
    git(
        &world.rival,
        &[
            "update-index",
            "--cacheinfo",
            &format!("160000,{object},vendor/sub"),
        ],
    );
    git(&world.rival, &["commit", "-qm", "pin the child"]);
    git(&world.rival, &["push", "-q", "origin", "main"]);
    merge_main_alone(world);
}

/// A COMMIT A MERGED MAIN NEWLY PINS IN AN INITIALISED SUBMODULE IS IMPORTED, alone, through
/// that submodule's own origin (here not its tip), into agh's gitlink ref, and checked out; a
/// later pin that does not descend from it replaces the ref. NEGATIVE CONTROLS: a pin that is
/// no commit, and one its origin does not hold, are each named and leave the submodule where it
/// was. CONTROL: a pin the submodule already holds is checked out with no remote at all.
#[test]
fn a_submodule_pin_main_moved_is_imported_through_its_own_origin() {
    let world = world();
    let (child, first) = vendored_child(&world);
    let sub = world.work.join("vendor/sub");
    let plumbing = Git {
        root: &world.work,
        make: &git_cmd,
    };
    let submodule = Git {
        root: &sub,
        make: &git_cmd,
    };
    let second = commit(&child, "lib.txt", "2\n");
    let third = commit(&child, "lib.txt", "3\n");
    main_pins(&world, &second);
    assert_eq!(
        submodule.has_commit(&second),
        Ok(false),
        "main's pin is new here"
    );
    assert_eq!(plumbing.update_submodules(), Ok(()));
    assert_eq!(git(&sub, &["rev-parse", "HEAD"]), second);
    assert_eq!(git(&sub, &["rev-parse", GITLINK_REF]), second);
    assert_eq!(submodule.has_commit(&third), Ok(false), "the pin alone");

    git(&child, &["checkout", "-q", "-b", "side", &first]);
    let side = commit(&child, "side.txt", "s\n");
    main_pins(&world, &side);
    assert_eq!(plumbing.update_submodules(), Ok(()));
    assert_eq!(git(&sub, &["rev-parse", "HEAD"]), side);
    assert_eq!(git(&sub, &["rev-parse", GITLINK_REF]), side, "replaced");

    let blob = git(&child, &["rev-parse", "HEAD:side.txt"]);
    let absent = "1".repeat(40);
    for pin in [&blob, &absent] {
        main_pins(&world, pin);
        let refused = plumbing
            .update_submodules()
            .expect_err("no commit to check out");
        assert!(
            refused.starts_with(&format!(
                "vendor/sub's pinned commit {pin} was not imported: "
            )),
            "{refused}"
        );
        assert_eq!(git(&sub, &["rev-parse", "HEAD"]), side);
    }

    git(&sub, &["remote", "set-url", "origin", "../no-such-origin"]);
    main_pins(&world, &first);
    assert_eq!(plumbing.update_submodules(), Ok(()));
    assert_eq!(git(&sub, &["rev-parse", "HEAD"]), first);
}

/// A LOCAL GIT STEP IS HELD TO ITS OWN PROGRESS, NOT TO THE WALL: a step that works for longer
/// than its window (a shell spinning for five seconds under a two-second window, as a
/// `git submodule status` worked past its minute under a whole-workspace judgment's load) ends
/// with its answer, read for its stdout and read for its status alone. NEGATIVE CONTROL: a step
/// that sleeps is a hang, ended once the window passed with nothing done and named as one.
#[cfg(unix)]
#[test]
fn a_local_git_step_that_works_outlasts_its_window_and_one_that_sleeps_is_a_named_hang() {
    const WINDOW: Duration = Duration::from_secs(2);
    let dir = aterm_tempfile::tempdir().expect("tempdir");
    let shell = |script: &'static str| {
        move |root: &Path| {
            let mut cmd = aterm_uds::spawnfd::command("/bin/bash");
            cmd.current_dir(root).args(["-c", script, "git"]);
            cmd
        }
    };
    let works = shell("while [ $SECONDS -lt 5 ]; do :; done; echo answered");
    let working = Git {
        root: dir.path(),
        make: &works,
    };
    assert_eq!(
        working.ok(&["rev-parse", "HEAD"], WINDOW),
        Ok("answered".to_owned())
    );
    let status = working
        .status(&["merge-base", "--is-ancestor", "a", "b"], WINDOW)
        .map(|out| out.status.success());
    assert_eq!(status, Ok(true));

    let sleeps = shell("exec sleep 600");
    let hung = Git {
        root: dir.path(),
        make: &sleeps,
    };
    for why in [
        hung.ok(&["rev-parse", "HEAD"], WINDOW)
            .expect_err("a sleeping step is a hang"),
        hung.status(&["merge-base", "--is-ancestor", "a", "b"], WINDOW)
            .map(|out| out.status.success())
            .expect_err("a sleeping step is a hang"),
    ] {
        assert!(
            why.contains("made no CPU or I/O progress for 2s while waiting on this git step"),
            "{why}"
        );
    }
}

/// A SUBMODULE WHOSE ORIGIN IS ON GITHUB is asked through agh alone: one exact fetch of the pin
/// into the submodule's own repository (never the superproject's), then the checkout. The agh
/// step here is a stand-in that fetches from the local child, as agh fetches from GitHub.
#[test]
fn a_github_submodules_pin_is_fetched_by_agh_into_the_submodule() {
    let world = world();
    let (child, _) = vendored_child(&world);
    let sub = world.work.join("vendor/sub");
    git(
        &sub,
        &[
            "remote",
            "set-url",
            "origin",
            "https://github.com/Fixture/Sub.git",
        ],
    );
    let second = commit(&child, "lib.txt", "2\n");
    main_pins(&world, &second);
    let plumbing = Git {
        root: &world.work,
        make: &git_cmd,
    };
    let fetches = std::cell::Cell::new(0);
    let remote = |root: &Path, tool, args: &[std::ffi::OsString], until| match tool {
        crate::remote::Tool::Git => crate::remote::run(root, &git_cmd, tool, args, until),
        crate::remote::Tool::Agh => {
            fetches.set(fetches.get() + 1);
            assert_eq!(root, sub, "agh runs for the submodule");
            assert_eq!(&args[..3], ["repo", "fetch", "fixture/sub"]);
            assert_eq!(args[3], sub.as_os_str(), "into the submodule's repository");
            assert_eq!(args[4], "--input");
            let batch: aterm_json::Value =
                aterm_json::from_slice(&std::fs::read(&args[5]).expect("live input"))
                    .expect("JSON");
            assert_eq!(
                batch,
                aterm_json::json!([{"source":second,"into":GITLINK_REF,"expected_oid":second}])
            );
            let wanted = format!("+{second}:{GITLINK_REF}");
            let mut fetch = git_cmd(Path::new(&args[3]));
            fetch.args([
                "-c",
                "protocol.file.allow=always",
                "fetch",
                "-q",
                "--no-tags",
                path(&child),
                &wanted,
            ]);
            aterm_uds::deadline::output_until(&mut fetch, until).map_err(|e| e.to_string())
        }
    };
    assert_eq!(plumbing.update_submodules_through(&remote), Ok(()));
    assert_eq!(fetches.get(), 1);
    assert_eq!(git(&sub, &["rev-parse", "HEAD"]), second);
}

/// A counted red and a dirty checkout push nothing; a branch already on main lands nothing.
#[test]
fn a_red_or_a_dirty_checkout_pushes_nothing() {
    let world = world();
    let before = origin_main(&world);
    let (code, said, bases) = land_with(&world, crates(&["a"]), |_, _| green());
    assert_eq!(
        (code, bases.len()),
        (exit::PASS, 0),
        "nothing to land: {said:#?}"
    );
    commit(&world.work, "a/x.txt", "a2\n");
    write(&world.work, "loose.txt", "not committed\n");
    let (code, _, bases) = land_with(&world, crates(&["a"]), |_, _| green());
    assert_eq!((code, bases.len()), (exit::COULD_NOT_RUN, 0));
    std::fs::remove_file(world.work.join("loose.txt")).expect("tidy");
    let (code, said, _) = land_with(&world, crates(&["a"]), |_, _| {
        ran(exit::FAILED, vec![red("test", &["-p a --lib -- mine"])], 1)
    });
    assert_eq!(code, exit::FAILED, "{said:#?}");
    assert_eq!(origin_main(&world), before, "nothing was pushed");
}

/// A resumed landing stays put when main has advanced past it. Dirty work still refuses;
/// committing new work in that same checkout must judge and land against the advanced main.
#[test]
fn an_already_landed_checkout_resumes_without_absorbing_main_or_losing_new_work() {
    let world = world();
    commit(&world.work, "a/x.txt", "a2\n");
    let (code, said, _) = land_with(&world, crates(&["a"]), |_, _| green());
    assert_eq!(code, exit::PASS, "{said:#?}");
    let landed = origin_main(&world);
    assert_eq!(git(&world.work, &["rev-parse", "HEAD"]), landed);
    let advanced = rival_moves(&world, "b/y.txt", "b2\n");
    assert_ne!(advanced, landed);

    let said = RefCell::new(Vec::new());
    let resume = || {
        land(
            &world.work,
            &own(),
            &mut Seams {
                git: &git_cmd,
                verifier: &mut |_| panic!("an already-landed checkout builds no verifier"),
                judge: &mut |_, _, _, _| panic!("an already-landed checkout needs no judgment"),
                records: quiet(),
                reach: &|_, _, _| panic!("an already-landed checkout needs no reach"),
                recheck: &mut |_, _, _, _| panic!("an already-landed checkout needs no recheck"),
                autofix: &mut |_: &Path, _: &[String]| Ok(()),
                hand_over: &mut |_, _, _| panic!("an already-landed checkout hands nothing over"),
                say: &mut |line: &str| said.borrow_mut().push(line.to_owned()),
                remote: &live_remote,
                pause: &no_retry,
            },
        )
    };
    assert_eq!(resume(), exit::PASS, "{:#?}", said.borrow());
    assert_eq!(git(&world.work, &["rev-parse", "HEAD"]), landed);
    assert_eq!(origin_main(&world), advanced);
    assert!(git(&world.work, &["status", "--porcelain"]).is_empty());
    assert_eq!(
        std::fs::read_to_string(world.work.join("b/y.txt")).expect("old main's file"),
        "b\n",
        "the no-op must not merge newer main into the checkout"
    );

    write(&world.work, "a/x.txt", "a3\n");
    write(&world.work, "loose.txt", "unfinished\n");
    let dirty = git(&world.work, &["status", "--porcelain"]);
    assert_eq!(resume(), exit::COULD_NOT_RUN, "{:#?}", said.borrow());
    assert_eq!(git(&world.work, &["rev-parse", "HEAD"]), landed);
    assert_eq!(origin_main(&world), advanced);
    assert_eq!(git(&world.work, &["status", "--porcelain"]), dirty);
    for (file, text) in [("a/x.txt", "a3\n"), ("loose.txt", "unfinished\n")] {
        assert_eq!(
            std::fs::read_to_string(world.work.join(file)).expect("preserved work"),
            text
        );
    }

    std::fs::remove_file(world.work.join("loose.txt")).expect("finish loose work");
    let new_work = commit(&world.work, "a/x.txt", "a3\n");
    let (code, said, bases) = land_with(&world, crates(&["a"]), |_, _| green());
    assert_eq!(code, exit::PASS, "{said:#?}");
    assert_eq!(
        bases.as_slice(),
        std::slice::from_ref(&advanced),
        "new work must be judged"
    );
    let tip = origin_main(&world);
    assert_ne!(tip, advanced);
    assert_eq!(git(&world.work, &["rev-parse", "HEAD"]), tip);
    for ancestor in [&advanced, &new_work] {
        git(
            &world.work,
            &["merge-base", "--is-ancestor", ancestor, &tip],
        );
    }
    for (file, text) in [("a/x.txt", "a3\n"), ("b/y.txt", "b2\n")] {
        assert_eq!(
            std::fs::read_to_string(world.work.join(file)).expect("landed file"),
            text
        );
    }
    assert!(git(&world.work, &["status", "--porcelain"]).is_empty());
}

/// EVERY RED COUNTS: a red in a crate outside the branch's own (one main may already hold)
/// stops the landing and pushes nothing, as one in the branch's crates does. CONTROL: the same
/// landing, green, lands.
#[test]
fn a_red_outside_the_branchs_crates_counts_as_any_other() {
    let failing = |_: &str, _: usize| {
        ran(
            exit::FAILED,
            vec![row("fmt", "ok"), red("test", &[THEIRS])],
            1,
        )
    };
    for branch in [crates(&["a"]), crates(&["b"])] {
        let world = world();
        commit(&world.work, "a/x.txt", "a2\n");
        let before = origin_main(&world);
        let (code, said, _) = land_with(&world, branch.clone(), failing);
        assert_eq!(code, exit::FAILED, "{branch:?}: {said:#?}");
        assert_eq!(origin_main(&world), before, "{branch:?}");
    }
    let control = self::world();
    commit(&control.work, "a/x.txt", "a2\n");
    let (code, said, _) = land_with(&control, crates(&["a"]), |_, _| green());
    assert_eq!(code, exit::PASS, "{said:#?}");
}

#[test]
fn semantic_git_output_keeps_its_admission_prefix() {
    let world = world();
    let mut text = "-uninitialised prerequisite\n".to_owned();
    text.push_str(&"x".repeat(aterm_uds::deadline::OUTPUT_KEEP + 1));
    text.push('\u{a0}');
    commit(&world.work, "large.txt", &text);
    let plumbing = Git {
        root: &world.work,
        make: &git_cmd,
    };
    let actual = plumbing.local(&["show", "HEAD:large.txt"]).unwrap();
    assert_eq!(actual.len(), text.len(), "semantic output cannot be a tail");
    assert!(actual.starts_with("-uninitialised prerequisite\n"));
}

fn import_git(world: &World, stream: &str) {
    // Import logical Git paths directly: their bytes need not be representable by this
    // host's working-tree filesystem. The real diff-tree protocol must preserve them.
    let input = world._dir.path().join("paths.noindex");
    std::fs::write(&input, stream).unwrap();
    let mut import = git_cmd(&world.origin);
    import.args(["fast-import", "--quiet"]);
    let out = aterm_uds::deadline::output_within_from(
        &mut import,
        aterm_uds::spawnfd::Stdio::from(std::fs::File::open(&input).unwrap()),
        aterm_spec::wait::MUST_HAPPEN,
    )
    .expect("import completes");
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
}

#[test]
fn composition_compares_git_path_bytes_before_rendering_diagnostics() {
    let world = world();
    let base = origin_main(&world);
    let mut stream = String::new();
    for (reference, file) in [
        ("left", "a/\\200.txt"),
        ("right", "a/\\201.txt"),
        ("same", "a/\\200.txt"),
    ] {
        stream.push_str(&format!(
            "commit refs/heads/{reference}\n\
             committer Andrew Yates <test@example.invalid> 1 +0000\n\
             data {}\n{reference}\nfrom {base}\n\
             M 100644 inline \"{file}\"\ndata 2\nx\n\n",
            reference.len()
        ));
    }
    import_git(&world, &stream);
    let plumbing = Git {
        root: &world.origin,
        make: &git_cmd,
    };
    let left = plumbing.files(&base, "refs/heads/left").unwrap();
    let right = plumbing.files(&base, "refs/heads/right").unwrap();
    let same = plumbing.files(&base, "refs/heads/same").unwrap();
    assert_eq!(left.len(), 1);
    assert_eq!(right.len(), 1);
    assert!(
        left.is_disjoint(&right),
        "different Git path bytes are disjoint"
    );
    assert_eq!(
        left.intersection(&same).count(),
        1,
        "the same bytes still overlap"
    );
    assert!(left.contains(b"a/\x80.txt".as_slice()));
    assert_eq!(display_git_path(b"a/\x80.txt"), "\"a/\\x80.txt\"");
    let shown = display_git_path(b"a/\nverify land: LANDED");
    assert_eq!(
        shown.lines().count(),
        1,
        "a path cannot inject a diagnostic row"
    );
    assert!(shown.contains("\\n"));
}

#[test]
fn a_large_git_path_listing_keeps_shared_paths() {
    let world = world();
    let base = origin_main(&world);
    let prefix = format!("z/{}/{}/", "x".repeat(96), "y".repeat(96));
    let mut stream = String::new();
    for (reference, text) in [
        ("early", SHARED.replacen('1', "one", 1)),
        ("many", SHARED.replacen('8', "eight", 1)),
    ] {
        stream.push_str(&format!(
            "commit refs/heads/{reference}\n\
             committer Andrew Yates <test@example.invalid> 1 +0000\n\
             data {}\n{reference}\nfrom {base}\n\
             M 100644 inline shared.txt\ndata {}\n{text}",
            reference.len(),
            text.len()
        ));
        if reference == "many" {
            for index in 0..6000 {
                stream.push_str(&format!(
                    "M 100644 inline {prefix}{index:06}.txt\ndata 2\nx\n"
                ));
            }
        }
        stream.push('\n');
    }
    import_git(&world, &stream);
    let plumbing = Git {
        root: &world.origin,
        make: &git_cmd,
    };
    assert!(
        plumbing
            .merge_tree("refs/heads/early", "refs/heads/many")
            .unwrap()
            .tree()
            .is_some()
    );
    let early = plumbing.files(&base, "refs/heads/early").unwrap();
    let many = plumbing.files(&base, "refs/heads/many").unwrap();
    assert!(
        !early.is_disjoint(&many),
        "a large listing cannot hide the shared path"
    );
    assert_eq!(many.len(), 6001, "the listing is complete");
}

/// THE COMPOSE RULE: a clean merge with disjoint changes or exactly convergent entries
/// composes the pass without another run: one judgment, the landing's parents the judged
/// commit and the new main, `Aterm-Composed-Onto` naming it.
/// NEGATIVE CONTROL: genuinely different shared entries are merged and judged again,
/// even where the merge itself is clean. A lock change that is not the isolated product-version
/// move says so in its refusal; the old line named `Cargo.lock` and stopped, as if no exception
/// had been tried. Where the exception's evidence could not be read, or has a shape it does not
/// read, the refusal says it could not be judged and why; the old line claimed the move was
/// absent even for a version move whose merge tree or child workspace went unread. The run
/// records say which: a compose and a `composed` landing, or the abort that doomed the pass and
/// the re-judge it caused (cause `doomed`). NEGATIVE CONTROL, run once as a mutant that made
/// this test fail: the doomed tip leaving the re-judge no cause (it read `first`).
#[test]
fn a_moved_main_composes_disjoint_or_convergent_changes_else_it_is_rejudged() {
    let world = world();
    commit(&world.work, "a/x.txt", "a2\n");
    let moved = RefCell::new(None);
    let (code, said, bases) = land_with(&world, crates(&["a"]), |_, n| {
        if n == 0 {
            *moved.borrow_mut() = Some(rival_moves(&world, "b/y.txt", "b2\n"));
        }
        green()
    });
    assert_eq!(code, exit::PASS, "{said:#?}");
    assert_eq!(bases.len(), 1, "composed, not judged again: {said:#?}");
    let moved = moved.into_inner().expect("main moved");
    let landing = origin_main(&world);
    let judged = git(&world.work, &["rev-parse", &format!("{landing}^1")]);
    assert_eq!(
        git(&world.work, &["rev-parse", &format!("{landing}^2")]),
        moved
    );
    let receipt = stated(&world, &landing).expect("stated");
    assert_eq!(receipt.composed_onto.as_deref(), Some(moved.as_str()));
    assert_eq!(
        receipt.checked[0].tree,
        git(&world.work, &["rev-parse", &format!("{judged}^{{tree}}")])
    );
    assert_eq!(
        git(&world.work, &["show", &format!("{landing}:b/y.txt")]),
        "b2"
    );
    assert_eq!(
        git(&world.work, &["show", &format!("{landing}:a/x.txt")]),
        "a2"
    );
    assert_eq!(git(&world.work, &["rev-parse", "HEAD"]), landing);
    assert_eq!(git(&world.work, &["status", "--porcelain"]), "");

    let control = self::world();
    commit(&control.work, "shared.txt", &SHARED.replacen('1', "one", 1));
    let first_main = origin_main(&control);
    let (code, said, bases) = land_with(&control, crates(&["a"]), |_, n| {
        if n == 0 {
            rival_moves(&control, "shared.txt", &SHARED.replacen('8', "eight", 1));
        }
        green()
    });
    assert_eq!(code, exit::PASS, "{said:#?}");
    assert_eq!(bases.len(), 2, "judged again: {said:#?}");
    assert_eq!(bases[0], first_main);
    let landing = origin_main(&control);
    let receipt = stated(&control, &landing).expect("stated");
    assert_eq!(receipt.checked.len(), 2);
    assert_eq!(receipt.composed_onto, None);
    let moved_main = git(&control.work, &["rev-parse", &format!("{landing}^2")]);
    assert_eq!(
        bases[1], moved_main,
        "the re-judge's cone is the branch's delta from the moved main"
    );
    assert_eq!(receipt.checked[1].base, moved_main);
    assert_eq!(
        git(&control.work, &["show", &format!("{landing}:shared.txt")]),
        "one\n2\n3\n4\n5\n6\n7\neight"
    );

    // Distinct commits may already agree on the complete overlapping entry.
    for change in ["edit", "delete", "mode"] {
        let world = self::world();
        let converge = |dir: &Path, message: &str| {
            git(dir, &["config", "core.filemode", "false"]);
            match change {
                "delete" => std::fs::remove_file(dir.join("shared.txt")).unwrap(),
                "edit" => write(dir, "shared.txt", "converged\n"),
                _ => {}
            }
            git(dir, &["add", "-A"]);
            if change == "mode" {
                git(dir, &["update-index", "--chmod=+x", "shared.txt"]);
            }
            git(dir, &["commit", "-qm", message]);
            git(dir, &["rev-parse", "HEAD"])
        };
        let checked = converge(&world.work, "the branch's convergent change");
        let moved = RefCell::new(None);
        let (code, said, bases) = land_with(&world, crates(&["a"]), |_, n| {
            if n == 0 {
                git(&world.rival, &["fetch", "-q", "origin"]);
                git(
                    &world.rival,
                    &["checkout", "-q", "-B", "main", "origin/main"],
                );
                let tip = converge(&world.rival, "main's convergent change");
                assert_ne!(checked, tip, "the commits really diverged");
                git(&world.rival, &["push", "-q", "origin", "main"]);
                *moved.borrow_mut() = Some(tip);
            }
            green()
        });
        assert_eq!(code, exit::PASS, "{change}: {said:#?}");
        let moved = moved.into_inner().unwrap();
        let landing = origin_main(&world);
        let entry = git(&world.work, &["ls-tree", &checked, "--", "shared.txt"]);
        assert_eq!(
            git(&world.work, &["ls-tree", &moved, "--", "shared.txt"]),
            entry
        );
        assert_eq!(
            git(&world.work, &["ls-tree", &landing, "--", "shared.txt"]),
            entry
        );
        if change == "delete" {
            assert!(entry.is_empty(), "all three entries are absent");
        } else if change == "mode" {
            assert!(entry.starts_with("100755 blob "));
        }
        assert_eq!(bases.len(), 1, "{change}: composed once: {said:#?}");
        let receipt = stated(&world, &landing).unwrap();
        assert_eq!(receipt.checked.len(), 1);
        assert_eq!(receipt.composed_onto.as_deref(), Some(moved.as_str()));
    }

    // Object equality alone does not cover a different mode or kind of entry.
    for mode in ["100755", "120000"] {
        let world = self::world();
        let base = origin_main(&world);
        let checked = commit(&world.work, "shared.txt", "same object\n");
        write(&world.rival, "shared.txt", "same object\n");
        git(&world.rival, &["add", "-A"]);
        let object = git(&world.rival, &["rev-parse", ":shared.txt"]);
        git(
            &world.rival,
            &["update-index", "--cacheinfo", mode, &object, "shared.txt"],
        );
        git(&world.rival, &["commit", "-qm", "a different entry mode"]);
        let main = git(&world.rival, &["rev-parse", "HEAD"]);
        git(&world.rival, &["push", "-q", "origin", "main"]);
        git(&world.work, &["fetch", "-q", "origin"]);
        assert_eq!(
            git(
                &world.work,
                &["rev-parse", &format!("{checked}:shared.txt")]
            ),
            object
        );
        assert!(
            git(&world.work, &["ls-tree", &checked, "--", "shared.txt"])
                .starts_with("100644 blob ")
        );
        assert!(
            git(&world.work, &["ls-tree", &main, "--", "shared.txt"])
                .starts_with(&format!("{mode} blob "))
        );
        let plumbing = Git {
            root: &world.work,
            make: &git_cmd,
        };
        let different = plumbing
            .paths(&[
                "diff-tree",
                "-r",
                "-t",
                "-z",
                "--name-only",
                "--no-renames",
                "--ignore-submodules=none",
                &checked,
                &main,
            ])
            .unwrap();
        assert!(different.contains(b"shared.txt".as_slice()));
        match plumbing.merge_tree(&main, &checked).unwrap() {
            Compose::Tree(tree) => assert_eq!(
                plumbing.independent_changes(&base, &main, &checked, &tree),
                Ok(Independence::Blocked {
                    paths: vec![b"shared.txt".to_vec()],
                    unread: None,
                    lock: None
                }),
                "{mode}: a clean merge still differs from what was checked"
            ),
            refused => {
                assert_eq!(mode, "120000", "the regular mode change merges cleanly");
                let Compose::Conflict(conflicted) = refused else {
                    panic!("a type change is Git's conflict");
                };
                assert!(
                    conflicted
                        .iter()
                        .any(|path| path.starts_with(b"shared.txt")),
                    "the conflict names the path: {conflicted:?}"
                );
            }
        }
    }

    // Recursive leaf diffs alone omit a newly present parent tree. Its complete
    // entry still differs from the checked deletion, even if the merge is clean.
    let shape = self::world();
    let base = origin_main(&shape);
    std::fs::remove_file(shape.work.join("shared.txt")).unwrap();
    git(&shape.work, &["add", "-A"]);
    git(&shape.work, &["commit", "-qm", "delete the old file"]);
    let checked = git(&shape.work, &["rev-parse", "HEAD"]);
    std::fs::remove_file(shape.rival.join("shared.txt")).unwrap();
    let main = commit(&shape.rival, "shared.txt/child", "a new tree\n");
    git(&shape.rival, &["push", "-q", "origin", "main"]);
    git(&shape.work, &["fetch", "-q", "origin"]);
    let plumbing = Git {
        root: &shape.work,
        make: &git_cmd,
    };
    assert!(
        plumbing
            .files(&base, &main)
            .unwrap()
            .contains(b"shared.txt".as_slice())
    );
    assert!(
        !plumbing
            .files(&checked, &main)
            .unwrap()
            .contains(b"shared.txt".as_slice())
    );
    let tree_entries = plumbing
        .paths(&[
            "diff-tree",
            "-r",
            "-t",
            "-z",
            "--name-only",
            "--no-renames",
            "--ignore-submodules=none",
            &checked,
            &main,
        ])
        .unwrap();
    assert!(tree_entries.contains(b"shared.txt".as_slice()));
    let tree = plumbing
        .merge_tree(&main, &checked)
        .unwrap()
        .tree()
        .map(str::to_owned)
        .expect("the deletion and replacement directory merge cleanly");
    assert_eq!(
        plumbing.independent_changes(&base, &main, &checked, &tree),
        Ok(Independence::Blocked {
            paths: vec![b"shared.txt".to_vec()],
            unread: None,
            lock: None
        }),
        "an absent entry is not a directory"
    );

    // A product-version move does not change the branch's independent lock entry.
    // The real merge must retain both edits; other metadata and uncertain identity do
    // not receive this exception, even when their textual merge is clean.
    const WORKSPACE: &str =
        "[workspace]\nmembers = [\"crates/*\"]\n[workspace.package]\nversion = \"0.1.0\"\n";
    const PRODUCT: &str = "[package]\nname = \"aterm\"\nversion.workspace = true\n\
        [[bin]]\nname = \"aterm\"\npath = \"src/main.rs\"\n\
        [dependencies]\nagent = { path = \"../agent\" }\n";
    const AGENT: &str =
        "[package]\nname = \"agent\"\nversion = \"0.0.0\"\n[dependencies]\nbase = \"1.0.0\"\n";
    const LOCK: &str = "version = 4\n\n\
        [[package]]\nname = \"aterm\"\nversion = \"0.1.0\"\ndependencies = [\"agent\"]\n\n\
        [[package]]\nname = \"agent\"\nversion = \"0.0.0\"\ndependencies = [\"base\"]\n\n\
        [[package]]\nname = \"agh-fake\"\nversion = \"0.0.0\"\n\n\
        [[package]]\nname = \"base\"\nversion = \"1.0.0\"\n\
        source = \"registry+https://example.invalid/index\"\nchecksum = \"old-checksum\"\n";
    for change in [
        "version",
        "dependency",
        "checksum",
        "format",
        "trivia",
        "mismatch",
        "inheritor",
        "mode",
        "unknown",
        "symlink-member",
        "root-package",
        "workspace-override",
        "build-disabled",
        "build-override",
        "build-default",
        "identity-writer",
        "identity-reader",
        "path-member",
        "nested-workspace",
        "nested-override",
        "escaping-path",
        "gitlink-workspace",
        "gitlink-override",
    ] {
        let world = self::world();
        let workspace = if change == "root-package" {
            format!("{WORKSPACE}\n[package]\nname = \"root\"\nversion.workspace = true\n")
        } else if matches!(change, "gitlink-workspace" | "gitlink-override") {
            WORKSPACE.replace(
                "[workspace.package]",
                "exclude = [\"vendor/sub\"]\n[workspace.package]",
            )
        } else {
            WORKSPACE.to_owned()
        };
        let product = match change {
            "build-disabled" => PRODUCT.replace("[package]\n", "[package]\nbuild = false\n"),
            "build-override" => PRODUCT.replace("[package]\n", "[package]\nbuild = \"other.rs\"\n"),
            "build-default" => PRODUCT.replace("[package]\n", "[package]\nbuild = \"build.rs\"\n"),
            _ => PRODUCT.to_owned(),
        };
        for (file, text) in [
            ("Cargo.toml", workspace.as_str()),
            ("Cargo.lock", LOCK),
            ("crates/aterm/Cargo.toml", product.as_str()),
            ("crates/aterm/src/main.rs", "fn main() {}\n"),
            ("crates/aterm/build.rs", "// the fixed identity writer\n"),
            ("crates/aterm-stamp/src/lib.rs", "// runtime identity\n"),
            (
                "crates/agent/Cargo.toml",
                if change == "inheritor" {
                    "[package]\nname = \"agent\"\nversion.workspace = true\n\
                     [dependencies]\nbase = \"1.0.0\"\n"
                } else if change == "workspace-override" {
                    "[package]\nname = \"agent\"\nversion = \"0.0.0\"\nworkspace = \"../..\"\n\
                     [dependencies]\nbase = \"1.0.0\"\n"
                } else {
                    AGENT
                },
            ),
            (
                "crates/agh-fake/Cargo.toml",
                "[package]\nname = \"agh-fake\"\nversion = \"0.0.0\"\n",
            ),
        ] {
            write(&world.rival, file, text);
        }
        if change == "symlink-member" {
            // Exercise Git's immutable symlink shape without requiring OS symlink
            // privileges. The target member would otherwise evade the manifest scan.
            for dir in [&world.rival, &world.work] {
                git(dir, &["config", "core.symlinks", "false"]);
            }
            write(&world.rival, "crates/linked", "../outside");
            write(
                &world.rival,
                "outside/Cargo.toml",
                "[package]\nname = \"hidden\"\nversion.workspace = true\n",
            );
        }
        if matches!(change, "gitlink-workspace" | "gitlink-override") {
            let child = world._dir.path().join("child");
            git(&world.rival, &["init", "-q", "-b", "main", path(&child)]);
            write(
                &child,
                "Cargo.toml",
                "[workspace]\nmembers = [\"crates/helper\"]\n[workspace.package]\nversion = \"7.0.0\"\n",
            );
            write(
                &child,
                "crates/helper/Cargo.toml",
                if change == "gitlink-override" {
                    "[package]\nname = \"helper\"\nversion.workspace = true\nworkspace = \"../../../..\"\n"
                } else {
                    "[package]\nname = \"helper\"\nversion.workspace = true\n"
                },
            );
            write(&child, "crates/helper/src/lib.rs", "pub fn helper() {}\n");
            // Like astream's negative fixture corpus, this manifest is inspected
            // but its deliberately absent target is not in the dependency closure.
            write(
                &child,
                "fixtures/unused/Cargo.toml",
                "[package]\nname = \"unused\"\nversion = \"0.0.0\"\n[dependencies]\nmissing = { path = \"missing\" }\n",
            );
            git(&child, &["add", "-A"]);
            git(&child, &["commit", "-qm", "an independent child workspace"]);
            git(
                &world.rival,
                &[
                    "-c",
                    "protocol.file.allow=always",
                    "submodule",
                    "add",
                    "-q",
                    path(&child),
                    "vendor/sub",
                ],
            );
        }
        git(&world.rival, &["add", "-A"]);
        if change == "symlink-member" {
            let target = git(&world.rival, &["rev-parse", ":crates/linked"]);
            git(
                &world.rival,
                &[
                    "update-index",
                    "--cacheinfo",
                    "120000",
                    &target,
                    "crates/linked",
                ],
            );
        }
        git(
            &world.rival,
            &["commit", "-q", "-m", "the versioned product"],
        );
        git(&world.rival, &["push", "-q", "origin", "main"]);
        git(&world.work, &["fetch", "-q", "origin"]);
        git(&world.work, &["merge", "--ff-only", "origin/main"]);
        if matches!(change, "gitlink-workspace" | "gitlink-override") {
            git(
                &world.work,
                &[
                    "-c",
                    "protocol.file.allow=always",
                    "submodule",
                    "update",
                    "--init",
                    "--no-fetch",
                ],
            );
        }
        if change == "symlink-member" {
            assert!(
                git(&world.work, &["ls-tree", "HEAD", "crates/linked"]).starts_with("120000 blob ")
            );
        }
        let mut own_lock = LOCK.replace(
            "dependencies = [\"base\"]",
            "dependencies = [\"base\", \"agh-fake\"]",
        );
        let mut agent = git(&world.work, &["show", "HEAD:crates/agent/Cargo.toml"]);
        if matches!(
            change,
            "path-member"
                | "nested-workspace"
                | "nested-override"
                | "escaping-path"
                | "gitlink-workspace"
                | "gitlink-override"
        ) {
            let target = match change {
                "escaping-path" => "../../../outside",
                "path-member" => "../../support/helper",
                "gitlink-workspace" | "gitlink-override" => "../../vendor/sub/crates/helper",
                _ => "../../support/workspace/helper",
            };
            agent.push_str(&format!("\n[dependencies.helper]\npath = \"{target}\"\n"));
            own_lock = own_lock.replace(
                "dependencies = [\"base\", \"agh-fake\"]",
                "dependencies = [\"base\", \"agh-fake\", \"helper\"]",
            );
            own_lock.push_str(&format!(
                "\n[[package]]\nname = \"helper\"\nversion = \"{}\"\n",
                if matches!(change, "path-member" | "gitlink-override") {
                    "0.1.0"
                } else if change == "gitlink-workspace" {
                    "7.0.0"
                } else {
                    "0.0.0"
                }
            ));
            if change == "path-member" {
                write(
                    &world.work,
                    "support/helper/Cargo.toml",
                    "[package]\nname = \"helper\"\nversion.workspace = true\n",
                );
            } else if matches!(change, "nested-workspace" | "nested-override") {
                write(
                    &world.work,
                    "support/workspace/Cargo.toml",
                    "[workspace]\nmembers = [\"helper\"]\n[workspace.package]\nversion = \"0.0.0\"\n",
                );
                write(
                    &world.work,
                    "support/workspace/helper/Cargo.toml",
                    if change == "nested-override" {
                        "[package]\nname = \"helper\"\nversion.workspace = true\nworkspace = \"../../..\"\n"
                    } else {
                        "[package]\nname = \"helper\"\nversion.workspace = true\n"
                    },
                );
            }
        }
        write(
            &world.work,
            "crates/agent/Cargo.toml",
            &format!("{agent}\n[dev-dependencies]\nagh-fake = {{ path = \"../agh-fake\" }}\n"),
        );
        commit(&world.work, "Cargo.lock", &own_lock);
        let moved = RefCell::new(None);
        let (code, said, bases) = land_with(&world, crates(&["agent"]), |_, n| {
            if n == 0 {
                git(&world.rival, &["fetch", "-q", "origin"]);
                git(
                    &world.rival,
                    &["checkout", "-q", "-B", "main", "origin/main"],
                );
                let workspace = workspace.replace(
                    "0.1.0",
                    if change == "mismatch" {
                        "0.3.0"
                    } else {
                        "0.2.0"
                    },
                );
                let mut lock = LOCK.replacen("0.1.0", "0.2.0", 1);
                match change {
                    "dependency" => {
                        lock = lock.replace(
                            "dependencies = [\"agent\"]",
                            "dependencies = [\"agent\", \"base\"]",
                        );
                    }
                    "checksum" => lock = lock.replace("old-checksum", "new-checksum"),
                    "format" => lock = lock.replacen("version = 4", "version = 3", 1),
                    "trivia" => lock.insert_str(0, "# another change\n"),
                    "unknown" => lock.push_str("future-policy = true\n"),
                    _ => {}
                }
                if matches!(change, "identity-writer" | "identity-reader") {
                    write(
                        &world.rival,
                        if change == "identity-writer" {
                            "crates/aterm/build.rs"
                        } else {
                            "crates/aterm-stamp/src/lib.rs"
                        },
                        "// changed identity boundary\n",
                    );
                }
                write(&world.rival, "Cargo.toml", &workspace);
                write(&world.rival, "Cargo.lock", &lock);
                git(&world.rival, &["add", "-A"]);
                if change == "mode" {
                    git(&world.rival, &["update-index", "--chmod=+x", "Cargo.lock"]);
                }
                git(
                    &world.rival,
                    &["commit", "-q", "-m", "move product identity"],
                );
                let tip = git(&world.rival, &["rev-parse", "HEAD"]);
                git(&world.rival, &["push", "-q", "origin", "main"]);
                *moved.borrow_mut() = Some(tip);
            }
            green()
        });
        assert_eq!(code, exit::PASS, "{change}: {said:#?}");
        let composed = matches!(
            change,
            "version" | "build-default" | "nested-workspace" | "gitlink-workspace"
        );
        assert_eq!(
            bases.len(),
            if composed { 1 } else { 2 },
            "{change}: {said:#?}"
        );
        if !composed {
            let refused = said
                .iter()
                .find(|line| line.contains("does not compose"))
                .unwrap_or_else(|| panic!("{change}: a refusal line: {said:#?}"));
            // The exception names the difference the read trees decided, or why it could not
            // judge them; a shape it does not read is never stated as an absent move.
            let unread = |why: &str| {
                format!(
                    ", and whether its change is the isolated product-version move that alone \
                     may compose could not be judged: {why}"
                )
            };
            let shapes = |endpoint: &str| {
                unread(&format!(
                    "{endpoint}'s workspace manifests or Cargo.lock are outside the shapes this \
                     exception reads"
                ))
            };
            let lock = match change {
                "dependency" | "checksum" | "trivia" | "unknown" => {
                    ", and its change is not the isolated product-version move that alone may \
                     compose: new main changes Cargo.lock beyond the product version"
                        .to_owned()
                }
                "identity-writer" | "identity-reader" => format!(
                    ", and its change is not the isolated product-version move that alone may \
                     compose: new main's \"{}\" differs from the base's",
                    if change == "identity-writer" {
                        "crates/aterm/build.rs"
                    } else {
                        "crates/aterm-stamp/src/lib.rs"
                    }
                ),
                "format" | "mismatch" | "mode" => shapes("new main"),
                "inheritor" | "root-package" | "workspace-override" | "build-disabled"
                | "build-override" => shapes("the base"),
                "path-member" | "nested-override" | "escaping-path" => shapes("the checked tree"),
                "symlink-member" => unread(
                    "the base: \"crates/linked\" sits directly in crates/ rather than in a \
                     member directory",
                ),
                "gitlink-override" => unread(
                    "submodule \"vendor/sub\": its workspace is outside the shapes this \
                     exception reads",
                ),
                _ => panic!("{change}: an unclassified refusal: {refused}"),
            };
            assert!(
                refused.ends_with(&format!(
                    "touch the branch's \"Cargo.lock\", where the checked tree's entry differs \
                     from new main's or the merge's{lock}; what it changed, with the paths it \
                     shares with the branch, reaches every crate ({WHOLE})"
                )),
                "{change}: a refused lock says what its version exception decided: {refused}"
            );
        }
        let moved = moved.into_inner().unwrap();
        let landing = origin_main(&world);
        let receipt = stated(&world, &landing).unwrap();
        assert_eq!(
            receipt.composed_onto.as_deref(),
            composed.then_some(moved.as_str()),
            "{change}"
        );
        if composed {
            let parents = git(&world.work, &["show", "-s", "--format=%P", &landing]);
            let checked = parents.split_whitespace().next().unwrap();
            let tree = git(&world.work, &["rev-parse", &format!("{landing}^{{tree}}")]);
            assert!(
                receipt_composition(
                    &world.work,
                    &std::env::var_os("PATH").unwrap_or_default(),
                    checked,
                    &moved,
                    &tree,
                )
                .unwrap(),
                "{change}"
            );
            let base = git(&world.work, &["merge-base", checked, &moved]);
            let claims_absent = "is not the isolated product-version move";
            if change == "version" {
                // The positive control composes from the same plumbing.
                let plumbing = Git {
                    root: &world.work,
                    make: &git_cmd,
                };
                let composes = plumbing
                    .independent_changes(&base, &moved, checked, &tree)
                    .unwrap();
                assert!(
                    matches!(&composes, Independence::Composes(why)
                        if why.contains("the isolated product version moved from 0.1.0 to 0.2.0")),
                    "{composes:?}"
                );
                // A merge tree no object store holds: neither the endpoint entries nor the
                // exception's own trees can be read, so nothing decided the move is absent.
                let missing = "0".repeat(tree.len());
                let blocked = plumbing
                    .independent_changes(&base, &moved, checked, &missing)
                    .unwrap();
                let Independence::Blocked {
                    paths,
                    unread: Some(_),
                    ..
                } = &blocked
                else {
                    panic!("an unreadable merge keeps the lock overlap: {blocked:?}");
                };
                assert_eq!(paths, &[b"Cargo.lock".to_vec()]);
                let words = blocked.to_string();
                assert!(
                    !words.contains(claims_absent),
                    "unread evidence decided nothing about the move: {words}"
                );
                assert!(
                    words.contains("(the endpoint entries could not be read")
                        && words.contains(
                            "and whether its change is the isolated product-version move \
                             that alone may compose could not be judged: the merge: git ls-tree"
                        ),
                    "{words}"
                );
            }
            if change == "gitlink-workspace" {
                // A checkout whose submodule was never initialized reads every endpoint entry,
                // but not the child workspace the exception must close over.
                let fresh = world._dir.path().join("fresh");
                git(
                    world._dir.path(),
                    &["clone", "-q", path(&world.origin), path(&fresh)],
                );
                let plumbing = Git {
                    root: &fresh,
                    make: &git_cmd,
                };
                let blocked = plumbing
                    .independent_changes(&base, &moved, checked, &tree)
                    .unwrap();
                let Independence::Blocked {
                    paths,
                    unread: None,
                    ..
                } = &blocked
                else {
                    panic!("the endpoint entries were read: {blocked:?}");
                };
                assert_eq!(paths, &[b"Cargo.lock".to_vec()]);
                let words = blocked.to_string();
                assert!(
                    !words.contains(claims_absent),
                    "an unread child workspace decided nothing about the move: {words}"
                );
                assert!(
                    words.contains("could not be judged: submodule \"vendor/sub\": git rev-parse"),
                    "{words}"
                );
            }
        }
        let lock = git(&world.work, &["show", &format!("{landing}:Cargo.lock")]);
        assert!(lock.contains(
            if matches!(
                change,
                "path-member"
                    | "nested-workspace"
                    | "nested-override"
                    | "escaping-path"
                    | "gitlink-workspace"
                    | "gitlink-override"
            ) {
                "dependencies = [\"base\", \"agh-fake\", \"helper\"]"
            } else {
                "dependencies = [\"base\", \"agh-fake\"]"
            }
        ));
        assert!(lock.contains("version = \"0.2.0\""));
        assert_eq!(git(&world.work, &["status", "--porcelain"]), "");
    }
    // The run records: each composed case its compose and a `composed` landing; each case judged
    // again the abort that doomed its pass and the re-judge it caused, landed `exact`.
    let composed = landed(
        &[
            ("judgment", "pass", "first"),
            ("compose", "composed", "disjoint"),
            ("end", "landed", ""),
        ],
        "composed",
    );
    let doomed = landed(
        &[
            ("judgment", "pass", "first"),
            ("abort", "doomed", "doomed"),
            ("judgment", "pass", "doomed"),
            ("end", "landed", ""),
        ],
        "exact",
    );
    let told = quietly();
    assert!(
        told.contains(&composed)
            && told.contains(&doomed)
            && told.iter().all(|run| *run == composed || *run == doomed),
        "{told:#?}"
    );
}

/// A moved main that converges on one shared path and differs on another is judged again, and
/// the line that says why names only the path that still differs: the convergent one was
/// discarded and did not block composition. Where the endpoint entries cannot be read, nothing
/// is discarded and the refusal says so beside the whole overlap.
/// NEGATIVE CONTROL: the old line printed the raw overlap, naming the convergent `a/x.txt` too
/// ("touch the branch's "a/x.txt", "shared.txt"").
#[test]
fn a_refused_composition_names_only_the_shared_paths_that_still_differ() {
    let world = world();
    write(&world.work, "a/x.txt", "converged\n");
    commit(&world.work, "shared.txt", &SHARED.replacen('1', "one", 1));
    let (code, said, bases) = land_with(&world, crates(&["a"]), |_, n| {
        if n == 0 {
            write(&world.rival, "a/x.txt", "converged\n");
            rival_moves(&world, "shared.txt", &SHARED.replacen('8', "eight", 1));
        }
        green()
    });
    assert_eq!(code, exit::PASS, "{said:#?}");
    assert_eq!(bases.len(), 2, "judged again: {said:#?}");
    let refused = said
        .iter()
        .find(|line| line.contains("does not compose"))
        .unwrap_or_else(|| panic!("a refusal line: {said:#?}"));
    assert!(refused.contains("\"shared.txt\""), "{refused}");
    assert!(
        !refused.contains("a/x.txt"),
        "a convergent path did not block composition: {refused}"
    );

    let unread = self::world();
    let base = origin_main(&unread);
    write(&unread.work, "a/x.txt", "converged\n");
    let checked = commit(&unread.work, "shared.txt", &SHARED.replacen('1', "one", 1));
    write(&unread.rival, "a/x.txt", "converged\n");
    let main = rival_moves(&unread, "shared.txt", &SHARED.replacen('8', "eight", 1));
    git(&unread.work, &["fetch", "-q", "origin"]);
    let plumbing = Git {
        root: &unread.work,
        make: &git_cmd,
    };
    let tree = plumbing
        .merge_tree(&main, &checked)
        .unwrap()
        .tree()
        .map(str::to_owned)
        .expect("a clean merge");
    assert_eq!(
        plumbing.independent_changes(&base, &main, &checked, &tree),
        Ok(Independence::Blocked {
            paths: vec![b"shared.txt".to_vec()],
            unread: None,
            lock: None
        })
    );
    let missing = "0".repeat(tree.len());
    let blocked = plumbing
        .independent_changes(&base, &main, &checked, &missing)
        .unwrap();
    let Independence::Blocked {
        paths,
        unread: Some(_),
        lock: None,
    } = &blocked
    else {
        panic!("an unreadable merge entry keeps the overlap: {blocked:?}");
    };
    assert_eq!(paths, &[b"a/x.txt".to_vec(), b"shared.txt".to_vec()]);
    let words = blocked.to_string();
    assert!(
        words.contains("\"a/x.txt\", \"shared.txt\" (the endpoint entries could not be read"),
        "{words}"
    );
}

#[cfg(unix)]
#[test]
fn a_whitespace_only_conflict_path_is_not_erased() {
    for (file, shown) in [("  ", "\"  \""), ("a/\nnotice.txt", "\"a/\\nnotice.txt\"")] {
        let world = world();
        commit(&world.work, file, "ours\n");
        let moved = rival_moves(&world, file, "theirs\n");
        let (code, said, bases) = land_with(&world, crates(&["a"]), |_, _| green());
        assert_eq!(code, exit::CONFLICT, "{said:#?}");
        assert!(bases.is_empty(), "a conflict starts no checks");
        assert!(said.iter().any(|line| line.contains(shown)), "{said:#?}");
        assert!(
            said.iter().all(|line| !line.contains('\n')),
            "one-line diagnostics: {said:#?}"
        );
        assert_eq!(origin_main(&world), moved, "a conflict pushes nothing");
    }
}

/// SLICE 0's exit test (`docs/DESIGN-landing-economy.md` §2.0, §5): main gains a commit that
/// touches the branch's file while the judgment runs, a file whose reach is every crate, and the
/// first poll after it interrupts the run, naming the tip, the shared file and that reach (a
/// narrower reach interrupts nothing:
/// [`a_tip_whose_shared_paths_reach_less_than_the_branch_does_not_interrupt`]). So does a tip
/// that does not descend from the main the branch merged, though it shares no path with the
/// branch. The interrupted run is no verdict: the receipt
/// states it as interrupted, never as checked, beside the judgment of the moved main, which is
/// merged and judged at once and counts as one re-judge. A poll before main moved interrupts
/// nothing. NEGATIVE CONTROL: a disjoint move mid-run interrupts nothing, a second poll of the
/// same tip reads main but tests nothing (it makes only the git calls of a poll with nothing
/// new), and the pass composes onto the moved main after the push, as before. The run records
/// say so: the abort of the cut judgment, the re-judge it caused (cause `cut`) and an `exact`
/// landing; the control's compose and its `composed` landing. NEGATIVE CONTROLS, each run once as
/// a mutant that made this test fail: the cut leaving the re-judge no cause (it read `first`);
/// a landing onto another main than its judgment merged named `exact`.
#[test]
fn a_main_that_cannot_compose_interrupts_the_judgment_at_its_next_poll() {
    let world = world();
    commit(&world.work, "shared.txt", &SHARED.replacen('1', "one", 1));
    let first_main = origin_main(&world);
    let judged_tree = git(&world.work, &["rev-parse", "HEAD^{tree}"]);
    let (mut polls, mut moved) = (Vec::new(), String::new());
    let (code, said, bases) = land_watching(&world, crates(&["a"]), |_, n, poll| {
        if n == 0 {
            polls.push(poll(&mut || false));
            moved = rival_moves(&world, "shared.txt", &SHARED.replacen('8', "eight", 1));
            polls.push(poll(&mut || false));
            if polls == [false, true] {
                return Ran::Interrupted {
                    secs: 42,
                    queued: false,
                };
            }
        }
        Ran::Ended(green())
    });
    assert_eq!(code, exit::PASS, "{said:#?}");
    assert_eq!(
        polls,
        [false, true],
        "the first poll after the move interrupts"
    );
    assert_eq!(bases, [first_main.clone(), moved.clone()], "{said:#?}");
    let line = format!(
        "verify land: INTERRUPTED the judgment of {judged_tree}: origin/main moved to {moved}, \
         which does not compose: its new commits touch the branch's \"shared.txt\", where the \
         checked tree's entry differs from new main's or the merge's; what it changed, with \
         the paths it shares with the branch, reaches every crate ({WHOLE})"
    );
    assert_eq!(
        said.iter().filter(|said| **said == line).count(),
        1,
        "{said:#?}"
    );
    let landing = origin_main(&world);
    let receipt = stated(&world, &landing).expect("stated");
    assert_eq!(
        receipt.checked.len(),
        1,
        "the interrupted run states no verdict"
    );
    assert_eq!(receipt.checked[0].base, moved);
    assert_eq!(
        receipt.interrupted,
        [Interrupted {
            tree: judged_tree,
            base: first_main,
            tip: moved.clone(),
            secs: 42,
        }]
    );
    assert_eq!((receipt.rejudges(), receipt.secs()), (1, 42 + 7));
    assert_eq!(receipt.composed_onto, None);
    assert_eq!(
        git(&world.work, &["rev-parse", &format!("{landing}^2")]),
        moved
    );
    assert!(
        said.iter()
            .any(|said| said.starts_with("verify land: LANDED")
                && said.contains("(1 re-judge, 1 after an interrupted judgment)")),
        "{said:#?}"
    );
    assert_eq!(git(&world.work, &["status", "--porcelain"]), "");

    let control = self::world();
    commit(&control.work, "a/x.txt", "a2\n");
    let calls = std::cell::Cell::new(0_usize);
    let counting = |root: &Path| {
        calls.set(calls.get() + 1);
        git_cmd(root)
    };
    let (mut polls, mut costs, mut moved) = (Vec::new(), Vec::new(), String::new());
    let (code, said, bases) = land_through(&control, &counting, crates(&["a"]), |_, n, poll| {
        assert_eq!(n, 0, "a disjoint move is never judged again");
        for moves in [false, true, false] {
            if moves {
                moved = rival_moves(&control, "b/y.txt", "b2\n");
            }
            let before = calls.get();
            polls.push(poll(&mut || false));
            costs.push(calls.get() - before);
        }
        Ran::Ended(green())
    });
    assert_eq!(code, exit::PASS, "{said:#?}");
    assert_eq!(polls, [false, false, false], "{said:#?}");
    assert!(
        costs[1] > costs[0] && costs[2] == costs[0],
        "the moved tip is tested once, its second read only read: {costs:?}"
    );
    assert_eq!(bases.len(), 1, "{said:#?}");
    assert!(
        !said.iter().any(|said| said.contains("INTERRUPTED")),
        "{said:#?}"
    );
    let receipt = stated(&control, &origin_main(&control)).expect("stated");
    assert_eq!(receipt.composed_onto.as_deref(), Some(moved.as_str()));
    assert!(receipt.interrupted.is_empty());

    // Main is rewritten while the judgment runs: its new tip holds none of the main the branch
    // merged, and shares no path with the branch.
    let rewritten = self::world();
    let seed = origin_main(&rewritten);
    let merged = rival_moves(&rewritten, "b/y.txt", "b2\n");
    commit(&rewritten.work, "a/x.txt", "a2\n");
    let mut moved = String::new();
    let (code, said, bases) = land_watching(&rewritten, crates(&["a"]), |_, n, poll| {
        if n == 0 {
            git(&rewritten.rival, &["checkout", "-q", "-B", "main", &seed]);
            moved = commit(&rewritten.rival, "c/z.txt", "c\n");
            git(
                &rewritten.rival,
                &["push", "-q", "--force", "origin", "main"],
            );
            assert!(poll(&mut || false), "a tip off the merged main interrupts");
            return Ran::Interrupted {
                secs: 3,
                queued: false,
            };
        }
        Ran::Ended(green())
    });
    assert_eq!(code, exit::PASS, "{said:#?}");
    assert_eq!(bases, [merged.clone(), moved.clone()], "{said:#?}");
    assert!(
        said.iter().any(|said| said.contains("INTERRUPTED")
            && said.contains(&format!(
                "it does not descend from {merged}, the main the branch merged"
            ))),
        "{said:#?}"
    );
    // The run records: an abort of the cut judgment and the re-judge it caused, landed exactly
    // onto the tip it merged; the control's pass composed onto the moved main.
    let cut = landed(
        &[
            ("abort", "cut", "cut"),
            ("judgment", "pass", "cut"),
            ("end", "landed", ""),
        ],
        "exact",
    );
    let composed = landed(
        &[
            ("judgment", "pass", "first"),
            ("compose", "composed", "disjoint"),
            ("end", "landed", ""),
        ],
        "composed",
    );
    assert_eq!(quietly(), [cut.clone(), composed, cut]);
}

/// THE CUT RULE: a tip that shares a path with the branch interrupts the running judgment only
/// where nothing it would produce can stand, that is, where the round of the merge would judge
/// every crate the branch's change reaches: what main changed (the paths it shares with the
/// branch and the ones it does not, asked together) reaches all of them, or none of them (a
/// round that would leave nothing of the branch's cone judges all of it), or the branch's
/// change reaches every crate, whose round is the whole workspace. A narrower reach interrupts
/// nothing: the judgment finishes and stands for the tree it ran on, and the merge is judged by
/// one round. The reach is the cone's crates, dependents included, never its seeds alone: a
/// change of one seed whose cone holds the branch's every crate cuts. A reach that was not read
/// interrupts nothing either: nothing is then known to be lost by letting the judgment finish.
/// NEGATIVE CONTROLS: the same move under a reach that holds the branch's whole reach, or more,
/// interrupts at the first poll and is judged in full, with no round.
#[test]
fn a_tip_whose_shared_paths_reach_less_than_the_branch_does_not_interrupt() {
    // The branch's cone: its seeds (types, ctl, gui) and their dependent, `lab`.
    let cone = reaches(&["types", "ctl", "gui", "lab"]);
    let every = Reach::Everything("a root manifest changed".to_owned());
    let unread = Reach::Unread("the cone was not read".to_owned());
    // Main's reach, the branch's, and what the first poll's line says where it interrupts.
    let none = "reaches none of the crates the branch's change reaches (ctl, gui, lab, types), \
                so a round would judge all of them again";
    let cases: [(Reach, Reach, Option<&str>); 9] = [
        (reaches(&["gui", "lab"]), cone.clone(), None),
        (reaches(&[]), cone.clone(), Some(none)),
        (reaches(&["other"]), cone.clone(), Some(none)),
        (unread.clone(), cone.clone(), None),
        (reaches(&["types", "ctl", "gui", "lab"]), unread, None),
        (
            reaches(&["types", "ctl", "gui", "lab"]),
            cone.clone(),
            Some("reaches every crate the branch's change reaches (ctl, gui, lab, types)"),
        ),
        (
            reaches(&["types", "ctl", "gui", "lab", "other"]),
            cone.clone(),
            Some("reaches every crate the branch's change reaches (ctl, gui, lab, types)"),
        ),
        (
            every.clone(),
            cone,
            Some(
                "what it changed, with the paths it shares with the branch, reaches every crate \
                 (a root manifest changed)",
            ),
        ),
        (
            reaches(&["gui"]),
            every,
            Some(
                "the branch's change reaches every crate (a root manifest changed), so a round \
                 would judge all of it again",
            ),
        ),
    ];
    for (main, branch, cuts) in cases {
        let case = format!("main {main:?}, the branch {branch:?}");
        let world = world();
        commit(&world.work, "shared.txt", &SHARED.replacen('1', "one", 1));
        let judged_at = git(&world.work, &["rev-parse", "HEAD"]);
        let asked = RefCell::new(Vec::new());
        let reach = |paths: Option<&[String]>| {
            asked.borrow_mut().push(paths.map(<[String]>::to_vec));
            match paths {
                None => branch.clone(),
                Some(_) => main.clone(),
            }
        };
        let mut polls = Vec::new();
        let moved = RefCell::new(String::new());
        let (code, said, bases, rounds) = land_reaching(
            &world,
            &reach,
            |_, n, poll| {
                if n == 0 {
                    // Main changes a file of its own, then the branch's.
                    rival_moves(&world, "b/y.txt", "main's own\n");
                    *moved.borrow_mut() =
                        rival_moves(&world, "shared.txt", &SHARED.replacen('8', "eight", 1));
                    polls.push(poll(&mut || false));
                    polls.push(poll(&mut || false));
                    if polls[0] {
                        return Ran::Interrupted {
                            secs: 1,
                            queued: false,
                        };
                    }
                }
                Ran::Ended(green())
            },
            |base, delta| {
                assert_eq!(
                    base,
                    *moved.borrow(),
                    "{case}: the round judges since the tip it merged"
                );
                assert_eq!(
                    (delta.judged.as_str(), delta.shared.as_slice()),
                    (judged_at.as_str(), &["shared.txt".to_owned()][..]),
                    "{case}"
                );
                ran(
                    exit::PASS,
                    vec![
                        row("fmt", "ok"),
                        row("census", "ok"),
                        row("export-scan", "ok"),
                        row("lint", "ok"),
                        row("test", "ok"),
                    ],
                    5,
                )
            },
        );
        assert_eq!(code, exit::PASS, "{case}: {said:#?}");
        assert_eq!(polls, [cuts.is_some(), cuts.is_some()], "{case}: {said:#?}");
        let moved = moved.into_inner();
        let asked = asked.into_inner();
        assert!(
            asked.contains(&Some(vec!["b/y.txt".to_owned(), "shared.txt".to_owned()])),
            "{case}: the reach asked is of main's own change with the shared path: {asked:?}"
        );
        if !matches!(branch, Reach::Unread(_)) {
            assert_eq!(
                asked.iter().filter(|paths| paths.is_none()).count(),
                1,
                "{case}: the branch's reach is read once for the main it merged: {asked:?}"
            );
        }
        let went_on = format!(
            "verify land: origin/main moved to {moved}, which shares shared.txt with the \
             branch: the judgment goes on and stands for the tree it runs on, and a round judges \
             the merge once it has passed"
        );
        let receipt = stated(&world, &origin_main(&world)).expect("stated");
        if let Some(why) = cuts {
            assert_eq!((bases.len(), rounds.len()), (2, 0), "{case}: {said:#?}");
            assert!(
                said.iter()
                    .any(|said| said.contains("INTERRUPTED") && said.ends_with(why)),
                "{case}: {said:#?}"
            );
            assert!(!said.contains(&went_on), "{case}: {said:#?}");
            assert_eq!(
                (receipt.interrupted.len(), receipt.rechecked.len()),
                (1, 0),
                "{case}"
            );
        } else {
            assert_eq!((bases.len(), rounds.len()), (1, 1), "{case}: {said:#?}");
            assert!(
                !said.iter().any(|said| said.contains("INTERRUPTED")),
                "{case}: {said:#?}"
            );
            // The person hears of the round to come when the tip is read, once, though it is
            // polled twice.
            assert_eq!(
                said.iter().filter(|said| **said == went_on).count(),
                1,
                "{case}: {said:#?}"
            );
            assert_eq!(
                (receipt.interrupted.len(), receipt.rejudges()),
                (0, 0),
                "{case}"
            );
            assert_eq!(receipt.rechecked.len(), 1, "{case}");
            assert_eq!(receipt.rechecked[0].rows.len(), 5, "{case}");
            assert_eq!(
                (&receipt.rechecked[0].base, &receipt.rechecked[0].onto),
                (&moved, &moved),
                "{case}"
            );
        }
    }
}

/// No count of re-judges ends a landing: a main that moves into the branch's file during each of
/// the first four judgments interrupts each, and each tip is merged and judged at once; the
/// fifth judgment, which nothing interrupts, lands, its receipt stating the four interrupted
/// judgments and the one that passed. The fourth tip is the one the removed bound of three
/// re-judges stopped at, with the branch merged, its judgment interrupted and nothing pushed.
#[test]
fn a_fourth_tip_that_cannot_compose_is_merged_and_judged() {
    const TIPS: usize = 4;
    let world = world();
    commit(&world.work, "shared.txt", &SHARED.replacen('1', "one", 1));
    let mut tips = Vec::new();
    let (code, said, bases) = land_watching(&world, crates(&["a"]), |_, n, poll| {
        if n == TIPS {
            return Ran::Ended(green());
        }
        tips.push(rival_moves_line(&world, 4 + n, &format!("moved{n}")));
        assert!(
            poll(&mut || false),
            "judgment {n}: the first poll after the move interrupts"
        );
        Ran::Interrupted {
            secs: 1,
            queued: false,
        }
    });
    assert_eq!(code, exit::PASS, "{said:#?}");
    assert_eq!(bases.len(), TIPS + 1, "{said:#?}");
    assert_eq!(bases[1..], tips[..], "each tip is merged and judged");
    assert_eq!(
        said.iter()
            .filter(|said| said.starts_with("verify land: INTERRUPTED"))
            .count(),
        TIPS,
        "{said:#?}"
    );
    assert!(
        !said.iter().any(|said| said.contains("STOPPED")),
        "{said:#?}"
    );
    let landing = origin_main(&world);
    let last = tips.last().expect("main moved");
    assert_eq!(
        git(&world.work, &["rev-parse", &format!("{landing}^2")]),
        *last,
        "the landing merges the last tip"
    );
    let receipt = stated(&world, &landing).expect("stated");
    assert_eq!(
        (receipt.interrupted.len(), receipt.checked.len()),
        (TIPS, 1)
    );
    assert_eq!(receipt.checked[0].base, *last);
    assert_eq!(receipt.rejudges(), TIPS);
    assert_eq!(git(&world.work, &["status", "--porcelain"]), "");
}

/// A poll that cannot read main changes nothing: the judgment goes on, and its reason is said
/// once while it repeats, again once a read in between worked, and not at all when the poll
/// became moot (the run's own answer came, which ends its read). Once main reads again, a tip
/// that does not compose interrupts as ever. NEGATIVE CONTROL: the polls between the failures
/// read main and answer `false`, as nothing moved, and a failure after one says its reason.
#[test]
fn a_poll_that_cannot_read_main_says_so_once_a_run_and_the_judgment_goes_on() {
    let world = world();
    commit(&world.work, "shared.txt", &SHARED.replacen('1', "one", 1));
    let hidden = world.origin.with_extension("hidden");
    let hide = |hidden_now: bool| {
        let (from, to) = if hidden_now {
            (&world.origin, &hidden)
        } else {
            (&hidden, &world.origin)
        };
        std::fs::rename(from, to).expect("origin moves");
    };
    let mut polls = Vec::new();
    let (code, said, bases) = land_watching(&world, crates(&["a"]), |_, n, poll| {
        if n == 0 {
            hide(true);
            polls.push(poll(&mut || false));
            polls.push(poll(&mut || false));
            hide(false);
            polls.push(poll(&mut || false));
            hide(true);
            polls.push(poll(&mut || false));
            hide(false);
            polls.push(poll(&mut || false));
            hide(true);
            polls.push(poll(&mut || true));
            hide(false);
            rival_moves(&world, "shared.txt", &SHARED.replacen('8', "eight", 1));
            polls.push(poll(&mut || false));
            if polls[6] {
                return Ran::Interrupted {
                    secs: 1,
                    queued: false,
                };
            }
        }
        Ran::Ended(green())
    });
    assert_eq!(code, exit::PASS, "{said:#?}");
    assert_eq!(
        polls,
        [false, false, false, false, false, false, true],
        "{said:#?}"
    );
    let unread: Vec<_> = said
        .iter()
        .filter(|said| said.contains("origin/main was not read or tested during the judgment"))
        .collect();
    assert_eq!(unread.len(), 2, "{said:#?}");
    assert!(
        unread.iter().all(|said| said.ends_with("; it goes on")),
        "{said:#?}"
    );
    assert_eq!(bases.len(), 2, "{said:#?}");
}

/// A poll that reads main behind the base it judges over (a sibling worktree's older snapshot
/// of the shared fetch ref, or a main rewound by force) reads no news: main does not move back
/// short of a force-push, and merging a commit the branch holds merges nothing. The judgment
/// goes on, the read is said once as one the poll could not use, and once main reads at its tip
/// again the pass lands. A main that stays rewound is not swallowed: the push leases the main
/// the branch merged, so it is refused, though plain git would take it as a fast-forward, and
/// nothing is pushed. NEGATIVE CONTROL: a tip that does not compose still interrupts
/// ([`a_main_that_cannot_compose_interrupts_the_judgment_at_its_next_poll`]).
#[test]
fn a_poll_that_reads_main_behind_its_base_is_no_news_and_never_interrupts() {
    for stays_rewound in [false, true] {
        let world = world();
        commit(&world.work, "a/x.txt", "a2\n");
        let seed = origin_main(&world);
        let base = rival_moves(&world, "b/y.txt", "b2\n");
        let rewind = |to: &str| git(&world.origin, &["update-ref", "refs/heads/main", to]);
        let mut polls = Vec::new();
        let (code, said, bases) = land_watching(&world, crates(&["a"]), |judged, n, poll| {
            assert_eq!(
                (judged, n),
                (base.as_str(), 0),
                "one judgment, over the base"
            );
            rewind(&seed);
            polls.push(poll(&mut || false));
            polls.push(poll(&mut || false));
            if !stays_rewound {
                rewind(&base);
                polls.push(poll(&mut || false));
            }
            if polls.iter().any(|cut| *cut) {
                return Ran::Interrupted {
                    secs: 1,
                    queued: false,
                };
            }
            Ran::Ended(green())
        });
        assert!(polls.iter().all(|cut| !cut), "{polls:?} {said:#?}");
        assert_eq!(bases, std::slice::from_ref(&base), "{said:#?}");
        assert!(
            !said.iter().any(|said| said.contains("INTERRUPTED")),
            "{said:#?}"
        );
        let stale = format!(
            "verify land: origin/main was not read or tested during the judgment (it read {seed}, \
             which {base} already holds: a stale read, or main was rewound); it goes on"
        );
        assert_eq!(
            said.iter().filter(|said| **said == stale).count(),
            1,
            "{said:#?}"
        );
        if stays_rewound {
            assert_ne!(code, exit::PASS, "{said:#?}");
            assert_eq!(
                origin_main(&world),
                seed,
                "nothing is pushed over a rewound main"
            );
        } else {
            assert_eq!(code, exit::PASS, "{said:#?}");
            let landing = origin_main(&world);
            assert_eq!(
                git(&world.work, &["rev-parse", &format!("{landing}^2")]),
                base,
                "the pass landed with the main it judged over"
            );
        }
    }
}

/// A run on a scripted clock: its stage boundaries at the offsets given, a red reported and
/// its exit at the offsets its cells hold (which a poll may set), and each wait
/// spending all it was given unless the exit comes first. Where `turn` says so, it says
/// `judgment-queued` at the first offset and `judgment-turn` at the second, if any.
struct Scripted {
    clock: std::rc::Rc<std::cell::Cell<Duration>>,
    boundaries: Vec<Duration>,
    turn: Option<(Duration, Option<Duration>)>,
    unpassed: std::rc::Rc<std::cell::Cell<Option<Duration>>>,
    ends: std::rc::Rc<std::cell::Cell<Option<Duration>>>,
    code: i32,
    interrupted: usize,
    /// When the landing takes an interrupt of its own ([`Running::stopped`]); none by default.
    stops: std::rc::Rc<std::cell::Cell<Option<Duration>>>,
    /// How many times the run was asked to end at once ([`Running::end_now`]).
    ended_now: usize,
    /// What its interrupt answers still runs.
    unended: Vec<Unended>,
}

impl Scripted {
    fn new(clock: &std::rc::Rc<std::cell::Cell<Duration>>, boundaries: &[u64]) -> Self {
        clock.set(Duration::ZERO);
        Self {
            clock: clock.clone(),
            boundaries: boundaries
                .iter()
                .copied()
                .map(Duration::from_secs)
                .collect(),
            turn: None,
            unpassed: std::rc::Rc::default(),
            ends: std::rc::Rc::default(),
            code: exit::PASS,
            interrupted: 0,
            stops: std::rc::Rc::default(),
            ended_now: 0,
            unended: Vec::new(),
        }
    }
}

impl Running for Scripted {
    fn wait(&mut self, within: Duration) -> std::io::Result<Option<i32>> {
        assert_eq!(self.interrupted, 0, "an interrupted run is not waited on");
        let now = self.clock.get();
        if let Some(ends) = self.ends.get()
            && ends <= now + within
        {
            self.clock.set(ends.max(now));
            return Ok(Some(self.code));
        }
        self.clock.set(now + within);
        Ok(None)
    }

    fn reported(&mut self) -> Report {
        let now = self.clock.get();
        let said = |at: Option<Duration>| at.filter(|at| *at <= now);
        let queued = self.turn.and_then(|(queued, _)| said(Some(queued)));
        let turn = self.turn.and_then(|(_, turn)| said(turn));
        Report {
            boundaries: self.boundaries.iter().filter(|at| **at <= now).count()
                + usize::from(turn.is_some()),
            unpassed: self.unpassed.get().is_some_and(|at| at <= now),
            queued: queued.is_some() && turn.is_none(),
            waited_ms: turn
                .zip(queued)
                .map(|(turn, queued)| u64::try_from((turn - queued).as_millis()).unwrap()),
        }
    }

    fn interrupt(&mut self) -> Vec<Unended> {
        self.interrupted += 1;
        self.unended.clone()
    }

    fn stopped(&mut self) -> bool {
        self.stops.get().is_some_and(|at| at <= self.clock.get())
    }

    fn end_now(&mut self) {
        self.ended_now += 1;
    }
}

/// THE LANDING'S OWN INTERRUPT ENDS A RUNNING JUDGMENT: read each [`BOUNDARY_TICK`] while it is
/// polled, while its own end is awaited after a red, and as soon as a poll answers, it asks the
/// run to end once ([`Running::end_now`]), interrupts nothing and awaits nothing
/// ([`Ending::Stopped`]); a poll asked meanwhile reads the question moot. NEGATIVE CONTROL: the
/// same run with no interrupt is decided by its own exit, and a mutant of `supervise` that
/// does not read the interrupt between polls (run once) waited for that exit instead.
#[test]
fn a_judgment_ends_at_the_landings_own_interrupt_within_a_tick() {
    let secs = Duration::from_secs;
    let start = Instant::now();
    let clock = std::rc::Rc::new(std::cell::Cell::new(Duration::ZERO));
    let within = |at: Duration| {
        let now = clock.get();
        assert!(now >= at && now < at + BOUNDARY_TICK, "{at:?}: {now:?}");
    };

    // Between polls.
    let mut run = Scripted::new(&clock, &[10, 30]);
    run.ends.set(Some(secs(1000)));
    run.stops.set(Some(secs(12)));
    let ending = supervise(&mut run, &mut || start + clock.get(), &mut |_| false).unwrap();
    assert_eq!(ending, Ending::Stopped);
    assert_eq!((run.ended_now, run.interrupted), (1, 0));
    within(secs(12));

    // While its own end is awaited, after a red.
    let mut run = Scripted::new(&clock, &[10]);
    run.ends.set(Some(secs(1000)));
    run.unpassed.set(Some(secs(5)));
    run.stops.set(Some(secs(40)));
    let ending = supervise(&mut run, &mut || start + clock.get(), &mut |_| {
        panic!("a run that reported a red is not polled")
    })
    .unwrap();
    assert_eq!(ending, Ending::Stopped);
    assert_eq!((run.ended_now, run.interrupted), (1, 0));
    within(secs(40));

    // During a poll that then answers `true`: the interrupt, not the poll, ends it.
    let mut run = Scripted::new(&clock, &[10]);
    run.ends.set(Some(secs(1000)));
    let stops = run.stops.clone();
    let ending = supervise(&mut run, &mut || start + clock.get(), &mut |moot| {
        assert!(!moot(), "nothing answered yet");
        stops.set(Some(clock.get()));
        assert!(moot(), "the interrupt ends the poll's read of main");
        true
    })
    .unwrap();
    assert_eq!(ending, Ending::Stopped);
    assert_eq!((run.ended_now, run.interrupted), (1, 0));

    // NEGATIVE CONTROL: no interrupt.
    let mut run = Scripted::new(&clock, &[10, 30]);
    run.ends.set(Some(secs(1000)));
    let ending = supervise(&mut run, &mut || start + clock.get(), &mut |_| false).unwrap();
    assert_eq!(ending, Ending::Exited(exit::PASS));
    assert_eq!(run.ended_now, 0);
    assert_eq!(clock.get(), secs(1000));
}

/// A running judgment asks its poll at each stage boundary its run reports, within one
/// boundary tick of it, and at least every [`POLL`], on an injected clock; every poll has a new
/// boundary or a [`POLL`] behind it. The first `true` interrupts the run at once, and the run is
/// not waited on again. NEGATIVE CONTROL: polls that answer `false` interrupt nothing, and the
/// run's own exit ends the supervision with its code.
#[test]
fn a_running_judgment_polls_at_each_stage_boundary_and_at_least_every_poll_interval() {
    let secs = Duration::from_secs;
    let start = Instant::now();
    let clock = std::rc::Rc::new(std::cell::Cell::new(Duration::ZERO));
    let ends = secs(1000);
    let mut run = Scripted::new(&clock, &[10, 11, 400]);
    run.ends.set(Some(ends));
    let mut polled = Vec::new();
    let ending = supervise(&mut run, &mut || start + clock.get(), &mut |_| {
        polled.push(clock.get());
        false
    })
    .unwrap();
    assert_eq!(ending, Ending::Exited(exit::PASS));
    assert_eq!(run.interrupted, 0);
    for at in &run.boundaries {
        assert!(
            polled
                .iter()
                .any(|poll| *poll >= *at && *poll < *at + BOUNDARY_TICK),
            "boundary at {at:?} unpolled: {polled:?}"
        );
    }
    let mut before = Duration::ZERO;
    for poll in polled.iter().copied().chain([ends]) {
        assert!(poll - before <= POLL, "{before:?} to {poll:?}: {polled:?}");
        let fresh = run.boundaries.iter().any(|at| before < *at && *at <= poll);
        assert!(
            poll == ends || fresh || poll - before >= POLL,
            "{poll:?} had nothing behind it: {polled:?}"
        );
        before = poll;
    }

    let mut run = Scripted::new(&clock, &[10, 30]);
    let mut asked = 0;
    let ending = supervise(&mut run, &mut || start + clock.get(), &mut |_| {
        asked += 1;
        asked == 2
    })
    .unwrap();
    assert_eq!(
        ending,
        Ending::Interrupted {
            waited: Duration::ZERO,
            queued: false
        }
    );
    assert_eq!((asked, run.interrupted), (2, 1));
    assert!(
        clock.get() >= secs(30) && clock.get() < secs(30) + BOUNDARY_TICK,
        "interrupted at its second boundary's poll: {:?}",
        clock.get()
    );
}

/// A judgment cut while still queued for the judgment turn waited from its `judgment-queued`
/// until the cut, and its cut says it was queued; its `judgment-turn` is a boundary, polled at
/// once, and a cut after it counts the wait the turn named and is not a queued one. A poll is
/// never told whether the run is queued: a tip cuts a queued judgment only where it would cut a
/// running one. NEGATIVE CONTROL: a run that never queued waited nothing (the tests above).
#[test]
fn a_cut_of_a_queued_judgment_counts_its_wait() {
    let secs = Duration::from_secs;
    let start = Instant::now();
    let clock = std::rc::Rc::new(std::cell::Cell::new(Duration::ZERO));
    let mut run = Scripted::new(&clock, &[]);
    run.turn = Some((secs(2), None));
    let mut polls = 0;
    let ending = supervise(&mut run, &mut || start + clock.get(), &mut |_| {
        polls += 1;
        true
    })
    .unwrap();
    assert_eq!(polls, 1);
    let Ending::Interrupted { waited, queued } = ending else {
        panic!("{ending:?}");
    };
    assert!(queued);
    assert!(
        waited + POLL >= clock.get() && waited <= clock.get(),
        "{waited:?} at {:?}",
        clock.get()
    );

    let mut run = Scripted::new(&clock, &[]);
    run.turn = Some((Duration::ZERO, Some(secs(60))));
    let mut polled = Vec::new();
    let ending = supervise(&mut run, &mut || start + clock.get(), &mut |_| {
        polled.push(clock.get());
        true
    })
    .unwrap();
    assert_eq!(polled.len(), 1, "{polled:?}");
    assert!(
        polled[0] >= secs(60) && polled[0] < secs(60) + BOUNDARY_TICK,
        "the turn is a boundary, polled at once: {polled:?}"
    );
    assert_eq!(
        ending,
        Ending::Interrupted {
            waited: secs(60),
            queued: false
        }
    );
}

/// A run that reported a red is decided by its own end, as without the poll: main is not read
/// again, and a `true` that comes as the red is reported, or as the run exits, interrupts
/// nothing; the poll is told the question became moot, so its read of main ends. NEGATIVE
/// CONTROL: the same `true` with neither interrupts the run (the test above), the poll reads the
/// question live before either came, and a boundary before the red is polled.
#[test]
fn a_run_with_a_check_that_did_not_pass_is_decided_by_its_own_end() {
    let start = Instant::now();
    let clock = std::rc::Rc::new(std::cell::Cell::new(Duration::ZERO));
    let mut run = Scripted::new(&clock, &[10, 20, 30]);
    run.unpassed.set(Some(Duration::from_secs(15)));
    run.ends.set(Some(Duration::from_secs(1000)));
    run.code = exit::FAILED;
    let mut polled = Vec::new();
    let ending = supervise(&mut run, &mut || start + clock.get(), &mut |_| {
        polled.push(clock.get());
        clock.get() >= Duration::from_secs(20)
    })
    .unwrap();
    assert_eq!(ending, Ending::Exited(exit::FAILED));
    assert_eq!(run.interrupted, 0);
    assert_eq!(
        polled.len(),
        1,
        "only the boundary before the red: {polled:?}"
    );
    assert_eq!(clock.get(), Duration::from_secs(1000));

    for meanwhile in ["a red", "its exit"] {
        let mut run = Scripted::new(&clock, &[10]);
        run.ends.set(Some(Duration::from_secs(1000)));
        let (unpassed, ends) = (run.unpassed.clone(), run.ends.clone());
        let ending = supervise(&mut run, &mut || start + clock.get(), &mut |moot| {
            assert!(!moot(), "{meanwhile}: the run's answer has not come yet");
            match meanwhile {
                "its exit" => ends.set(Some(clock.get())),
                _ => unpassed.set(Some(clock.get())),
            }
            assert!(moot(), "{meanwhile}: the poll's read of main ends");
            true
        })
        .unwrap();
        assert_eq!(ending, Ending::Exited(exit::PASS), "{meanwhile}");
        assert_eq!(run.interrupted, 0, "{meanwhile}");
    }
}

/// A run's stage boundaries are the complete `stage-finished` records its events file holds,
/// each counted once as the file grows, one split across two reads included; it is unpassed
/// once a complete record names a red, any word but `ok` or `skip`: a `check-finished` row, a
/// `test-binary-finished` record or a decision of a `stage-finished` record, which comes before
/// its check's row does. NEGATIVE CONTROLS: a file not there yet, another event, plain text
/// naming the event, a malformed record and a line that has not ended count nothing, and passed
/// rows, binaries and stages, a split red row's first half and a malformed red row or binary
/// leave it passed.
#[test]
fn a_runs_events_report_its_complete_stage_boundaries_and_any_red() {
    use std::io::Write as _;
    let dir = aterm_tempfile::tempdir().expect("scratch");
    let stage = crate::report::stage_event(
        0,
        Some(crate::checks::Check::Fmt),
        &crate::ladder::Report::new("fmt"),
        Duration::ZERO,
        Duration::ZERO,
        None,
    );
    let mut value: aterm_json::Value =
        aterm_json::from_str(stage.trim_end().strip_prefix("verify-event: ").unwrap()).unwrap();
    value.as_object_mut().unwrap().insert(
        "invocation".into(),
        aterm_json::json!({"id": "12".repeat(16), "source_observation": null, "identity_error": null}),
    );
    let attributed = format!("verify-event: {value}\n");
    let (head, tail) = attributed.split_at(attributed.len() / 2);
    let row = |outcome: &str| {
        format!(
            "verify-event: {{\"schema\":1,\"event\":\"check-finished\",\"check\":\"test\",\
             \"outcome\":\"{outcome}\",\"findings\":[],\"findings_total\":0}}\n"
        )
    };
    let (red, ok, skip) = (row("FAIL"), row("ok"), row("skip"));
    let (red_head, red_tail) = red.split_at(red.len() / 2);
    let report = |boundaries, unpassed| Report {
        boundaries,
        unpassed,
        ..Report::default()
    };
    let events = dir.path().join("verify.events");
    let append = |events: &Path, text: &str| {
        std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(events)
            .unwrap()
            .write_all(text.as_bytes())
            .unwrap();
    };
    let mut tail_of = EventsTail::default();
    assert_eq!(tail_of.read(&events), report(0, false), "no file yet");
    for (text, read) in [
        (ok.as_str(), report(0, false)),
        (head, report(0, false)),
        (tail, report(1, false)),
        (
            "the stage-finished event, said in plain text\n",
            report(1, false),
        ),
        (
            "verify-event: {\"event\":\"stage-finished\"\n",
            report(1, false),
        ),
        (stage.as_str(), report(2, false)),
        ("", report(2, false)),
        (stage.trim_end(), report(2, false)),
        ("\n", report(3, false)),
        (skip.as_str(), report(3, false)),
        (
            "verify-event: {\"event\":\"check-finished\",\"outcome\":\"FAIL\"}\n",
            report(3, false),
        ),
        (red_head, report(3, false)),
        (red_tail, report(3, true)),
        (stage.as_str(), report(4, true)),
        (ok.as_str(), report(4, true)),
    ] {
        append(&events, text);
        assert_eq!(tail_of.read(&events), read, "after {text:?}");
    }
    // The turn's events: queued until the turn, which is a boundary and names the wait.
    let turn_of = dir.path().join("turn.events");
    let mut tail_of = EventsTail::default();
    let prediction = crate::turn::Prediction::default();
    let heavy = aterm_capacity::Heavy {
        since_ms: 1,
        predicted_ms: 600_000,
    };
    append(
        &turn_of,
        &format!(
            "{}\n",
            crate::turn::queued_event(prediction, heavy, "pid 7")
        ),
    );
    assert_eq!(
        tail_of.read(&turn_of),
        Report {
            queued: true,
            ..report(0, false)
        }
    );
    append(
        &turn_of,
        &format!(
            "{}\n",
            crate::turn::turn_event(prediction, heavy, Duration::from_secs(90), true)
        ),
    );
    assert_eq!(
        tail_of.read(&turn_of),
        Report {
            waited_ms: Some(90_000),
            ..report(1, false)
        }
    );
    // A wait that ended without the turn (the ledger unread, its holder stuck) ends the queued
    // state as the turn does: the run judges from then on, and a cut counts only its wait.
    let unheld = dir.path().join("unheld.events");
    let mut tail_of = EventsTail::default();
    for line in [
        crate::turn::queued_event(prediction, heavy, "pid 7"),
        crate::turn::turn_event(prediction, heavy, Duration::from_secs(40), false),
    ] {
        append(&unheld, &format!("{line}\n"));
    }
    assert_eq!(
        tail_of.read(&unheld),
        Report {
            waited_ms: Some(40_000),
            ..report(1, false)
        }
    );
    let not_run = dir.path().join("not-run.events");
    let mut tail_of = EventsTail::default();
    append(&not_run, &row("NOT RUN"));
    assert_eq!(tail_of.read(&not_run), report(0, true));

    let (binary, deciding) = (binary_line, deciding_stage);
    for (case, (red, lines)) in [
        (
            false,
            vec![binary("ok"), binary("skip"), deciding(&["ok", "skip"])],
        ),
        (
            false,
            vec![
                "verify-event: {\"event\":\"test-binary-finished\",\"outcome\":\"FAIL\"}\n"
                    .to_owned(),
                binary("FAIL").trim_end().to_owned(),
            ],
        ),
        (true, vec![binary("ok"), binary("FAIL")]),
        (true, vec![binary("NOT RUN")]),
        (true, vec![deciding(&["ok", "FAIL"])]),
        (true, vec![deciding(&["NOT RUN"])]),
    ]
    .into_iter()
    .enumerate()
    {
        let events = dir.path().join(format!("{case}.events"));
        let mut tail_of = EventsTail::default();
        for line in &lines {
            append(&events, line);
        }
        let stages = lines
            .iter()
            .filter(|line| line.contains("stage-finished") && line.ends_with('\n'))
            .count();
        assert_eq!(tail_of.read(&events), report(stages, red), "{lines:#?}");
    }
}

/// A `test-binary-finished` events line whose outcome is `outcome`.
fn binary_line(outcome: &str) -> String {
    format!(
        "verify-event: {{\"schema\":1,\"event\":\"test-binary-finished\",\
         \"test\":\"-p fixture --lib\",\"outcome\":\"{outcome}\",\"exit_code\":101}}\n"
    )
}

/// A `stage-finished` events line ([`crate::report::stage_event`]) whose decisions have
/// `outcomes`.
fn deciding_stage(outcomes: &[&str]) -> String {
    let stage = crate::report::stage_event(
        0,
        Some(crate::checks::Check::Test),
        &crate::ladder::Report::new("test"),
        Duration::ZERO,
        Duration::ZERO,
        None,
    );
    let mut value: aterm_json::Value =
        aterm_json::from_str(stage.trim_end().strip_prefix("verify-event: ").unwrap()).unwrap();
    value.as_object_mut().unwrap().insert(
        "decisions".into(),
        aterm_json::Value::Array(
            outcomes
                .iter()
                .map(|outcome| aterm_json::json!({"outcome": outcome, "detail": "a row"}))
                .collect(),
        ),
    );
    format!("verify-event: {value}\n")
}

/// [`Scripted`], its events written to a file at their offsets on its clock and read by the
/// production reader ([`EventsTail`]).
struct Written {
    run: Scripted,
    events: std::path::PathBuf,
    lines: std::collections::VecDeque<(Duration, String)>,
    tail: EventsTail,
}

impl Running for Written {
    fn wait(&mut self, within: Duration) -> std::io::Result<Option<i32>> {
        self.run.wait(within)
    }

    fn reported(&mut self) -> Report {
        use std::io::Write as _;
        let now = self.run.clock.get();
        while let Some((_, line)) = self.lines.front().filter(|(at, _)| *at <= now).cloned() {
            self.lines.pop_front();
            std::fs::OpenOptions::new()
                .create(true)
                .append(true)
                .open(&self.events)
                .unwrap()
                .write_all(line.as_bytes())
                .unwrap();
        }
        self.tail.read(&self.events)
    }

    fn interrupt(&mut self) -> Vec<Unended> {
        self.run.interrupt()
    }
}

/// A red test binary or stage, which a check reports before its own row, ends the polling as a
/// red row does: through the production events reader, a run that reported either is decided by
/// its own end, though every poll after it would answer `true`. NEGATIVE CONTROL: the same run
/// with a passing binary in its place is interrupted at the next boundary.
#[test]
fn a_red_test_binary_or_stage_its_check_has_not_yet_rowed_ends_the_polling() {
    let secs = Duration::from_secs;
    let start = Instant::now();
    let clock = std::rc::Rc::new(std::cell::Cell::new(Duration::ZERO));
    let dir = aterm_tempfile::tempdir().expect("scratch");
    for (case, (red, ending)) in [
        (binary_line("FAIL"), Ending::Exited(exit::FAILED)),
        (
            deciding_stage(&["ok", "FAIL"]),
            Ending::Exited(exit::FAILED),
        ),
        (
            binary_line("ok"),
            Ending::Interrupted {
                waited: Duration::ZERO,
                queued: false,
            },
        ),
    ]
    .into_iter()
    .enumerate()
    {
        let mut run = Scripted::new(&clock, &[]);
        run.ends.set(Some(secs(1000)));
        run.code = exit::FAILED;
        let boundary = deciding_stage(&["ok"]);
        let mut run = Written {
            run,
            events: dir.path().join(format!("{case}.events")),
            lines: [
                (10, boundary.clone()),
                (15, red),
                (20, boundary.clone()),
                (30, boundary),
            ]
            .into_iter()
            .map(|(at, line)| (secs(at), line))
            .collect(),
            tail: EventsTail::default(),
        };
        let mut polled = Vec::new();
        let got = supervise(&mut run, &mut || start + clock.get(), &mut |_| {
            polled.push(clock.get());
            clock.get() >= secs(20)
        })
        .unwrap();
        assert_eq!(got, ending, "case {case}: {polled:?}");
        let interrupted = run.run.interrupted;
        if matches!(ending, Ending::Interrupted { .. }) {
            assert_eq!((polled.len(), interrupted), (2, 1), "{polled:?}");
        } else {
            assert_eq!((polled.len(), interrupted), (1, 0), "{polled:?}");
            assert_eq!(clock.get(), secs(1000));
        }
    }
}

/// What a signal to a judgment's group does not reach, by a read of the process table: every
/// process outside the group that its members started, and theirs in turn at any depth (a
/// stage group, its separately grouped helper and that helper's child), each
/// by its pid and birth. Read again once the group is gone, it is those of them still running (an
/// exec in place, which moves an Apple pid version, included) and what they started since.
/// NEGATIVE CONTROLS: the group's own members, the land process that started it, a bystander
/// and its child are never named, nor a later process given an ended one's pid (another birth),
/// and with nothing known an empty group names nothing.
#[test]
fn what_a_judgments_group_started_outside_it_is_read_at_any_depth_by_pid_and_birth() {
    use aterm_uds::process::{Birth, Identity, Lineage};
    let row = |pid: i32, birth: u64, version: i32, parent: u32, pgid: u32| Lineage {
        identity: Identity {
            pid,
            birth: Birth(birth),
            idversion: Some(version),
        },
        uniqueid: birth,
        parent,
        pgid,
        launcher_version: None,
        tty: None,
    };
    let names = |found: &[Started]| -> BTreeSet<(i32, u64)> {
        found.iter().map(|one| (one.pid, one.birth.0)).collect()
    };
    let group = 100;
    let first = [
        // The land process, leading its own group, and the judgment's verifier
        // with a compiler in its group, which started a process in a group of its own.
        row(50, 1, 1, 1, 50),
        row(100, 2, 1, 50, 100),
        row(101, 3, 1, 100, 100),
        row(102, 4, 1, 101, 102),
        // A stage group, its separately grouped helper, and the helper's child.
        row(200, 5, 1, 100, 200),
        row(300, 6, 1, 200, 300),
        row(301, 7, 1, 300, 300),
        // A bystander and its child.
        row(400, 8, 1, 1, 400),
        row(401, 9, 1, 400, 400),
    ];
    let found = beyond(&first, Some(group), &[]);
    assert_eq!(
        names(&found),
        BTreeSet::from([(102, 4), (200, 5), (300, 6), (301, 7)])
    );
    // The group is gone: launchd adopted the stage, which exec'd in place; the compiler's
    // process ended and a later one took its pid; the stage started another child.
    let again = [
        row(50, 1, 1, 1, 50),
        row(102, 20, 1, 1, 102),
        row(200, 5, 2, 1, 200),
        row(300, 6, 1, 200, 300),
        row(301, 7, 1, 300, 300),
        row(302, 21, 1, 300, 302),
        row(400, 8, 1, 1, 400),
        row(401, 9, 1, 400, 400),
    ];
    assert_eq!(
        names(&beyond(&again, Some(group), &found)),
        BTreeSet::from([(200, 5), (300, 6), (301, 7), (302, 21)])
    );
    assert!(beyond(&again, Some(group), &[]).is_empty());
    // Its leader reaped, the group's id names it no more: a later process that leads a group of
    // that id, and its child, are nobody's, and what is known is read as before.
    let reused = [row(100, 30, 1, 1, 100), row(103, 31, 1, 100, 100)];
    let later: Vec<_> = again.iter().chain(&reused).cloned().collect();
    assert_eq!(
        names(&beyond(&later, None, &found)),
        BTreeSet::from([(200, 5), (300, 6), (301, 7), (302, 21)])
    );
    assert!(beyond(&later, None, &[]).is_empty());
}

/// A process table on a scripted clock ([`Outside`]): each process runs until the offset given
/// (`None`: it never ends), and one may start another at an offset.
struct Table {
    at: Duration,
    start: Instant,
    /// (pid, when it ends).
    ends: Vec<(i32, Option<Duration>)>,
    /// (the pid that starts it, when, the pid started, when that ends).
    starts: Vec<(i32, Duration, i32, Option<Duration>)>,
    named: Vec<(Duration, Vec<i32>, Duration)>,
}

impl Table {
    fn new(ends: &[(i32, Option<u64>)]) -> Self {
        Self {
            at: Duration::ZERO,
            start: Instant::now(),
            ends: ends
                .iter()
                .map(|(pid, end)| (*pid, end.map(Duration::from_secs)))
                .collect(),
            starts: Vec::new(),
            named: Vec::new(),
        }
    }

    fn started(pid: i32) -> Started {
        Started {
            pid,
            birth: aterm_uds::process::Birth(u64::from(pid.unsigned_abs())),
        }
    }

    fn runs(&self, pid: i32) -> bool {
        self.ends
            .iter()
            .any(|(one, end)| *one == pid && end.is_none_or(|end| self.at < end))
    }

    fn pids(left: &[Started]) -> Vec<i32> {
        let mut pids: Vec<i32> = left.iter().map(|one| one.pid).collect();
        pids.sort_unstable();
        pids
    }
}

impl Outside for Table {
    fn now(&mut self) -> Instant {
        self.start + self.at
    }

    fn left(&mut self, known: &[Started]) -> Vec<Started> {
        let mut left: Vec<Started> = known
            .iter()
            .copied()
            .filter(|one| self.runs(one.pid))
            .collect();
        for (parent, at, pid, end) in self.starts.clone() {
            if at <= self.at && known.contains(&Self::started(parent)) && self.runs(parent) {
                if !self.ends.iter().any(|(one, _)| *one == pid) {
                    self.ends.push((pid, end));
                }
                if self.runs(pid) && !left.contains(&Self::started(pid)) {
                    left.push(Self::started(pid));
                }
            }
        }
        left
    }

    fn nap(&mut self) {
        self.at += OUTSIDE_TICK;
    }

    fn named(&mut self, left: &[Started], bound: Duration) {
        self.named.push((self.at, Self::pids(left), bound));
    }
}

/// The wait for what an interrupted judgment started outside its group ends when all of it is
/// gone, and says nothing: each end renews the grace, so a chain that ends a step at a time
/// within it is awaited to its last. What does not end is named once the grace passed with no
/// end, awaited the bound more in all, and answered as still running: the landing's COULD NOT
/// RUN. NEGATIVE CONTROLS: one that ends inside the bound answers nothing, though it was named;
/// and with no bound (a test's hang detector as the grace) nothing is named.
#[test]
fn what_an_interrupted_judgment_started_is_awaited_named_and_bounded() {
    let secs = Duration::from_secs;
    let (grace, bound) = (secs(20), secs(120));
    let all = |table: &Table| -> Vec<Started> {
        table
            .ends
            .iter()
            .map(|(pid, _)| Table::started(*pid))
            .collect()
    };

    // A chain: each ends within the grace of the one before, the last 51s in.
    let mut chain = Table::new(&[(1, Some(15)), (2, Some(33)), (3, Some(51))]);
    assert!(await_ended(all(&chain), grace, bound, &mut chain).is_empty());
    assert_eq!(chain.at, secs(51), "it returns as the last one ends");
    assert!(chain.named.is_empty(), "{:?}", chain.named);

    // A compiler that ignores its end: named when the grace passes, awaited the bound, answered.
    let mut stuck = Table::new(&[(1, Some(5)), (2, None)]);
    let left = await_ended(all(&stuck), grace, bound, &mut stuck);
    assert_eq!(Table::pids(&left), [2]);
    assert_eq!(stuck.named, [(secs(25), vec![2], bound)]);
    assert_eq!(stuck.at, secs(25) + bound, "the bound is not renewed");

    // What an awaited process starts meanwhile is awaited too, and renews nothing.
    let mut spawning = Table::new(&[(1, Some(30)), (2, Some(10))]);
    spawning.starts = vec![(1, secs(12), 3, None), (3, secs(40), 4, Some(secs(60)))];
    let left = await_ended(all(&spawning), grace, bound, &mut spawning);
    assert_eq!(Table::pids(&left), [3]);
    assert_eq!(spawning.named, [(secs(50), vec![3, 4], bound)]);
    assert_eq!(spawning.at, secs(50) + bound);

    // NEGATIVE CONTROL: named, and gone inside the bound.
    let mut slow = Table::new(&[(1, Some(90))]);
    assert!(await_ended(all(&slow), grace, bound, &mut slow).is_empty());
    assert_eq!(slow.named, [(secs(20), vec![1], bound)]);
    assert_eq!(slow.at, secs(90));

    // NEGATIVE CONTROL: no bound names nothing and answers when the grace passes.
    let mut unbounded = Table::new(&[(1, None)]);
    let left = await_ended(all(&unbounded), grace, Duration::ZERO, &mut unbounded);
    assert_eq!(Table::pids(&left), [1]);
    assert!(unbounded.named.is_empty());
    assert_eq!(unbounded.at, grace);

    // Nothing started outside the group: no read of the clock is waited on.
    let mut none = Table::new(&[]);
    assert!(await_ended(Vec::new(), grace, bound, &mut none).is_empty());
    assert_eq!(none.at, Duration::ZERO);
}

/// An interrupted run whose processes did not all end is no plain interruption: the supervision
/// answers what still runs. NEGATIVE CONTROL: with all of it ended it is [`Ending::Interrupted`].
#[test]
fn an_interrupt_that_leaves_processes_running_ends_the_supervision_naming_them() {
    let start = Instant::now();
    let clock = std::rc::Rc::new(std::cell::Cell::new(Duration::ZERO));
    let stuck = vec![Unended {
        pid: 41,
        word: "trustc".to_owned(),
    }];
    for (unended, ending) in [
        (stuck.clone(), Ending::Unended(stuck)),
        (
            Vec::new(),
            Ending::Interrupted {
                waited: Duration::ZERO,
                queued: false,
            },
        ),
    ] {
        let mut run = Scripted::new(&clock, &[10]);
        run.unended = unended;
        let got = supervise(&mut run, &mut || start + clock.get(), &mut |_| true).unwrap();
        assert_eq!(got, ending);
        assert_eq!(run.interrupted, 1);
    }
}

/// A judgment a moved main interrupted, whose compiler did not end, ends the landing COULD NOT
/// RUN, naming it: nothing judges the checkout again beside a process that may still write its
/// build directory, and nothing is pushed. NEGATIVE CONTROL: the same interruption with every
/// process ended merges the moved main and judges it at once, and lands.
#[test]
fn an_interrupted_judgment_that_left_a_process_running_ends_the_landing_could_not_run() {
    for stuck in [true, false] {
        let world = world();
        commit(&world.work, "shared.txt", &SHARED.replacen('1', "one", 1));
        let judged_tree = git(&world.work, &["rev-parse", "HEAD^{tree}"]);
        let mut moved = String::new();
        let (code, said, bases) = land_watching(&world, crates(&["a"]), |_, n, poll| {
            if n == 0 {
                moved = rival_moves(&world, "shared.txt", &SHARED.replacen('8', "eight", 1));
                assert!(poll(&mut || false), "the moved main does not compose");
                return if stuck {
                    Ran::Unended {
                        secs: 42,
                        left: vec![
                            Unended {
                                pid: 41,
                                word: "trustc".to_owned(),
                            },
                            Unended {
                                pid: 40,
                                word: "targo".to_owned(),
                            },
                        ],
                    }
                } else {
                    Ran::Interrupted {
                        secs: 42,
                        queued: false,
                    }
                };
            }
            Ran::Ended(green())
        });
        if !stuck {
            assert_eq!((code, bases.len()), (exit::PASS, 2), "{said:#?}");
            assert!(
                !said.iter().any(|said| said.contains("still running")),
                "{said:#?}"
            );
            continue;
        }
        assert_eq!(code, exit::COULD_NOT_RUN, "{said:#?}");
        assert_eq!(bases.len(), 1, "no judgment follows it: {said:#?}");
        assert_eq!(origin_main(&world), moved, "nothing was pushed");
        let line = format!(
            "verify land: COULD NOT RUN — the interrupted judgment of {judged_tree} left 2 \
             processes still running, which did not end within 120s of being named: trustc \
             (pid 41), targo (pid 40); nothing was pushed, and nothing is judged beside what may \
             still write the build directory. Run verify land again once they are gone"
        );
        assert_eq!(
            said.iter().filter(|said| **said == line).count(),
            1,
            "{said:#?}"
        );
    }
}

/// Main's release commit and a peer's changelog entry arrive during the judgment of a branch
/// that wrote its own entry as a file under `changes/`: the release rewrites CHANGELOG.md and
/// deletes the fragment it shipped, the peer adds one beside the branch's, and none of those
/// paths is the branch's, so the pass composes onto the moved main with one judgment.
/// NEGATIVE CONTROL: a branch that edits CHANGELOG.md itself (a correction to a released
/// entry) shares that path with the release commit and is judged again. CHANGELOG.md is in the
/// no-reach class, but this world holds no copy of the no-reach rule, so the Markdown recheck
/// (`land_markdown_tests`) is not open to it; in the live tree code names CHANGELOG.md, so
/// `no_reach::may_read` judges a shared one in full there too.
#[test]
fn a_release_commit_and_a_peer_fragment_compose_with_a_branch_fragment() {
    const SHIPPED: &str = "- **Shipped.** In the window the release rolls.\n";
    const BRANCH: &str = "- **Branch.** The branch's own entry.\n";
    const PEER: &str = "- **Peer.** Landed while the branch was judged.\n";
    let changelog = "# Changelog\n\n## [Unreleased]\n\n## [0.1.0] - 2026-11-01\n\n\
                     - Old one.\n\n- Old two.\n\n- Old three.\n\n- Old four.\n";
    let rolled = changelog.replace(
        "## [Unreleased]\n\n",
        &format!("## [Unreleased]\n\n## [0.2.0] - 2026-11-12\n\n### Fixed\n\n{SHIPPED}\n"),
    );
    for in_changelog in [false, true] {
        let world = world();
        // The window's notes on main: one shipped-to-be fragment, an empty [Unreleased].
        git(&world.rival, &["fetch", "-q", "origin"]);
        git(
            &world.rival,
            &["checkout", "-q", "-B", "main", "origin/main"],
        );
        write(&world.rival, "CHANGELOG.md", changelog);
        commit(&world.rival, "changes/fixed/sa-shipped.md", SHIPPED);
        git(&world.rival, &["push", "-q", "origin", "main"]);
        git(&world.work, &["fetch", "-q", "origin"]);
        git(&world.work, &["merge", "-q", "--ff-only", "origin/main"]);
        write(&world.work, "a/x.txt", "a2\n");
        if in_changelog {
            // A correction to a released entry, far enough from the roll to merge cleanly.
            commit(
                &world.work,
                "CHANGELOG.md",
                &changelog.replace("- Old four.", "- Old four, corrected."),
            );
        } else {
            commit(&world.work, "changes/fixed/sb-branch.md", BRANCH);
        }
        let moved = RefCell::new(None);
        let (code, said, bases) = land_with(&world, crates(&["a"]), |_, n| {
            if n == 0 {
                git(&world.rival, &["fetch", "-q", "origin"]);
                git(
                    &world.rival,
                    &["checkout", "-q", "-B", "main", "origin/main"],
                );
                write(&world.rival, "CHANGELOG.md", &rolled);
                git(&world.rival, &["rm", "-q", "changes/fixed/sa-shipped.md"]);
                commit(&world.rival, "RELEASES.ledger", "1 0.2.0\n");
                let tip = commit(&world.rival, "changes/fixed/sc-peer.md", PEER);
                git(&world.rival, &["push", "-q", "origin", "main"]);
                *moved.borrow_mut() = Some(tip);
            }
            green()
        });
        assert_eq!(code, exit::PASS, "{said:#?}");
        let landing = origin_main(&world);
        let receipt = stated(&world, &landing).expect("stated");
        if in_changelog {
            assert_eq!(
                bases.len(),
                2,
                "a shared CHANGELOG.md is judged again: {said:#?}"
            );
            assert_eq!(receipt.composed_onto, None);
            continue;
        }
        assert_eq!(bases.len(), 1, "composed, not judged again: {said:#?}");
        let moved = moved.into_inner().expect("main moved");
        assert_eq!(receipt.composed_onto.as_deref(), Some(moved.as_str()));
        let listed = git(
            &world.work,
            &["ls-tree", "-r", "--name-only", &landing, "--", "changes/"],
        );
        assert_eq!(
            listed.lines().collect::<Vec<_>>(),
            ["changes/fixed/sb-branch.md", "changes/fixed/sc-peer.md"],
            "the shipped fragment stays deleted; both unreleased entries are files"
        );
        assert_eq!(
            git(&world.work, &["show", &format!("{landing}:CHANGELOG.md")]),
            rolled.trim_end()
        );
    }
}

/// A merge commit git refuses to write (here a signing failure) aborts only the merge this
/// landing opened and restores the exact clean branch tip, so a retry never inherits
/// MERGE_HEAD or a staged merge.
#[test]
fn a_refused_merge_commit_aborts_the_merge_cleanly() {
    let world = world();
    commit(&world.work, "a/x.txt", "a2\n");
    rival_moves(&world, "b/y.txt", "b2\n");
    let before = git(&world.work, &["rev-parse", "HEAD"]);
    let remote = origin_main(&world);
    git(&world.work, &["config", "gpg.program", "false"]);
    let refusing = |dir: &Path| {
        let mut command = git_cmd(dir);
        command.args(["-c", "commit.gpgSign=true"]);
        command
    };
    let said = RefCell::new(Vec::new());
    let code = land(
        &world.work,
        &own(),
        &mut Seams {
            git: &refusing,
            verifier: &mut |_: &Path| panic!("a refused merge builds no verifier"),
            judge: &mut |_: &Path, _: &Path, _: &str, _: Poll<'_>| {
                panic!("a refused merge is never judged")
            },
            records: quiet(),
            reach: &|_: &Path, _: &str, _: Option<&[String]>| {
                panic!("a refused merge reads no reach")
            },
            recheck: &mut |_: &Path, _: &Path, _: &str, _: &Delta| {
                panic!("a refused merge is never rechecked")
            },
            autofix: &mut |_: &Path, _: &[String]| Ok(()),
            hand_over: &mut |_: &Image, _: &Path, _: &Path| {
                panic!("a refused merge hands nothing over")
            },
            say: &mut |line: &str| said.borrow_mut().push(line.to_owned()),
            remote: &live_remote,
            pause: &no_retry,
        },
    );
    let said = said.into_inner();
    assert_eq!(code, exit::COULD_NOT_RUN, "{said:#?}");
    assert!(
        said.iter()
            .any(|line| line.contains("uncommitted merge was aborted")),
        "{said:#?}"
    );
    assert_eq!(git(&world.work, &["rev-parse", "HEAD"]), before);
    assert_eq!(origin_main(&world), remote);
    assert_eq!(git(&world.work, &["status", "--porcelain"]), "");
    assert!(
        !world.work.join(".git/MERGE_HEAD").exists(),
        "MERGE_HEAD survived the aborted merge"
    );
}

/// A commit's stdout is diagnostic, even beyond the complete-data capture limit. The actual
/// commit status and the resulting repository decide whether its merge completed.
#[cfg(unix)]
#[test]
fn a_merge_commit_does_not_treat_diagnostic_stdout_as_a_data_protocol() {
    let world = world();
    commit(&world.work, "a/x.txt", "a2\n");
    let tip = git(&world.work, &["rev-parse", "HEAD"]);
    let main = rival_moves(&world, "b/y.txt", "b2\n");
    git(&world.work, &["fetch", "-q", "origin"]);
    let shell = aterm_uds::deadline::stand_in(Path::new("/bin/sh"));
    let noisy = |dir: &Path| {
        let git = git_cmd(dir);
        let mut command = aterm_uds::spawnfd::command(&shell);
        command.current_dir(dir);
        for (key, value) in git.get_envs() {
            match value {
                Some(value) => command.env(key, value),
                None => command.env_remove(key),
            };
        }
        command
            .args([
                "-c",
                r#"b4_git=$1
shift
if [ "$1" != commit ]; then exec "$b4_git" "$@"; fi
"$b4_git" "$@"
b4_status=$?
b4_padding=x
for b4_step in 1 2 3 4 5 6 7 8 9 10 11 12 13 14 15 16 17 18 19 20; do
    b4_padding=$b4_padding$b4_padding
done
for b4_block in 1 2 3 4 5 6 7 8 9 10 11 12 13 14 15 16 17; do
    printf '%s' "$b4_padding"
done
exit "$b4_status""#,
                "land-diagnostic-git",
            ])
            .arg(git.get_program());
        command
    };
    let merged = Git {
        root: &world.work,
        make: &noisy,
    }
    .merge(&main, "merge with diagnostic stdout");
    match merged {
        Ok(Merge::Clean { resolved }) => assert!(resolved.is_empty(), "{resolved:?}"),
        Ok(Merge::Conflict { paths, .. }) => panic!("unexpected conflicts: {paths:?}"),
        Err(why) => panic!("{why}"),
    }
    let parents = git(&world.work, &["show", "-s", "--format=%P", "HEAD"]);
    assert_eq!(parents, format!("{tip} {main}"));
    assert_eq!(git(&world.work, &["status", "--porcelain"]), "");
    assert!(!world.work.join(".git/MERGE_HEAD").exists());
}

/// A fatal Git merge (here a held index lock) leaves the branch's bytes, HEAD and the remote
/// unchanged and no merge in progress: git 2.56 writes no merge state under it, and the state
/// git 2.43 writes land forgets.
#[test]
fn a_fatal_git_merge_leaves_the_branch_tip() {
    let world = world();
    commit(&world.work, "a/x.txt", "a2\n");
    rival_moves(&world, "b/y.txt", "b2\n");
    let before = git(&world.work, &["rev-parse", "HEAD"]);
    let remote = origin_main(&world);
    let lock = world.work.join(".git/index.lock");
    std::fs::write(&lock, "held by the regression fixture\n").unwrap();

    let (code, said, judged) = land_with(&world, crates(&["a"]), |_, _| {
        panic!("a fatal merge is never judged")
    });
    assert_ne!(code, exit::PASS, "{said:#?}");
    assert!(judged.is_empty(), "{judged:#?}");
    assert_eq!(git(&world.work, &["rev-parse", "HEAD"]), before);
    assert_eq!(origin_main(&world), remote);
    assert_eq!(std::fs::read(world.work.join("a/x.txt")).unwrap(), b"a2\n");
    assert!(!world.work.join(".git/MERGE_HEAD").exists());
    assert_eq!(
        std::fs::read_to_string(&lock).unwrap(),
        "held by the regression fixture\n"
    );
    std::fs::remove_file(lock).unwrap();
    assert_eq!(git(&world.work, &["status", "--porcelain"]), "");
}

/// The merge state git 2.43 leaves under a held index lock, written by hand so every git meets
/// it: with the checkout unchanged land forgets it and the tip stands; with a staged change, or
/// with no merge state at all, it says which and forgets nothing.
#[test]
fn an_own_merge_the_index_lock_kept_from_aborting_is_forgotten_only_while_nothing_changed() {
    const STATE: [&str; 3] = ["MERGE_HEAD", "MERGE_MSG", "MERGE_MODE"];
    let world = world();
    commit(&world.work, "a/x.txt", "a2\n");
    rival_moves(&world, "b/y.txt", "b2\n");
    git(&world.work, &["fetch", "-q", "origin"]);
    let tip = git(&world.work, &["rev-parse", "HEAD"]);
    let main = origin_main(&world);
    let dot_git = world.work.join(".git");
    let lock = dot_git.join("index.lock");
    let reader = Git {
        root: &world.work,
        make: &git_cmd,
    };
    let lock_merge_state = || {
        std::fs::write(dot_git.join("MERGE_HEAD"), format!("{main}\n")).unwrap();
        std::fs::write(dot_git.join("MERGE_MSG"), "Merge origin/main\n").unwrap();
        std::fs::write(dot_git.join("MERGE_MODE"), "no-ff").unwrap();
        std::fs::write(&lock, "held by the regression fixture\n").unwrap();
    };

    lock_merge_state();
    let said = reader.abort_own_merge(&tip, &main, "probe".into());
    assert!(
        said.contains("was forgotten and the branch tip restored"),
        "{said}"
    );
    for name in STATE {
        assert!(!dot_git.join(name).exists(), "{name} survived: {said}");
    }
    assert_eq!(git(&world.work, &["rev-parse", "HEAD"]), tip);
    assert_eq!(
        std::fs::read_to_string(&lock).unwrap(),
        "held by the regression fixture\n"
    );
    std::fs::remove_file(&lock).unwrap();

    let said = reader.abort_own_merge(&tip, &main, "probe".into());
    assert!(said.contains("git wrote no merge state"), "{said}");

    std::fs::write(world.work.join("staged.txt"), "staged\n").unwrap();
    git(&world.work, &["add", "staged.txt"]);
    lock_merge_state();
    let said = reader.abort_own_merge(&tip, &main, "probe".into());
    assert!(said.contains("could not be aborted either"), "{said}");
    assert!(dot_git.join("MERGE_HEAD").exists(), "{said}");
    std::fs::remove_file(&lock).unwrap();
}

/// [`Memory`] shared between a scripted landing's records and the test that reads them.
struct Shared(std::rc::Rc<Memory>);

impl crate::land_spool::Spool for Shared {
    fn put(&self, record: &crate::land_record::Signed) -> Result<(), String> {
        self.0.put(record)
    }

    fn pending(&self) -> Result<crate::land_spool::Pending, String> {
        self.0.pending()
    }

    fn done(&self, record: &crate::land_record::Signed) -> Result<(), String> {
        self.0.done(record)
    }
}

/// A spool that takes signal 15 for the landing as it keeps its first run record of `kind`, so
/// the interrupt comes just after that step's record.
struct Tripwire {
    spool: Box<dyn crate::land_spool::Spool>,
    kind: &'static str,
    signal: std::rc::Rc<std::cell::Cell<Option<i32>>>,
}

impl crate::land_spool::Spool for Tripwire {
    fn put(&self, record: &crate::land_record::Signed) -> Result<(), String> {
        if record.path.starts_with(crate::land_run::DIR)
            && record.path.contains(&format!(".{}.", self.kind))
        {
            self.signal.set(Some(15));
        }
        self.spool.put(record)
    }

    fn pending(&self) -> Result<crate::land_spool::Pending, String> {
        self.spool.pending()
    }

    fn done(&self, record: &crate::land_record::Signed) -> Result<(), String> {
        self.spool.done(record)
    }
}

/// Where a scripted landing's records go, read back by the test: a journal in memory (each
/// append's records, unless `refuse` is set), one clone's spool in memory, and the signal the
/// landing's interrupt reads (none until a test sets one).
#[derive(Default)]
struct Store {
    journal: std::rc::Rc<RefCell<Vec<crate::land_record::Signed>>>,
    spool: std::rc::Rc<Memory>,
    refuse: std::rc::Rc<std::cell::Cell<bool>>,
    signal: std::rc::Rc<std::cell::Cell<Option<i32>>>,
}

/// A run's id: 32 of `c`.
fn run_id(c: char) -> String {
    std::iter::repeat_n(c, 32).collect()
}

impl Store {
    /// The records of run `run` ([`run_id`]), started a minute before
    /// [`crate::land_record::tests::NOW`], kept here.
    fn records(&self, run: char) -> Records<'static> {
        let journal = self.journal.clone();
        let refuse = self.refuse.clone();
        let signal = self.signal.clone();
        Records {
            append: Box::new(
                move |_: &Path, records: &[crate::land_record::Signed], _: u64| {
                    if refuse.get() {
                        return Err("the scripted journal refuses appends".to_owned());
                    }
                    journal.borrow_mut().extend(records.iter().cloned());
                    Ok(())
                },
            ),
            spool: Box::new(Shared(self.spool.clone())),
            signer: None,
            host: "scripted".to_owned(),
            now: Box::new(|| crate::land_record::tests::NOW),
            run: crate::land_run::Run {
                id: run_id(run),
                started: crate::land_record::tests::NOW - 60,
                ..crate::land_run::tests::run()
            },
            hosting: Box::new(crate::hosting::Unhosted),
            interrupt: Box::new(move || signal.get()),
            tally: records::Tally::default(),
            lock: None,
        }
    }

    /// Every run record the journal and the spool hold, read as the census reads them.
    fn entries(&self) -> Vec<crate::land_run::Entry> {
        let spooled = crate::land_spool::Spool::pending(&*self.spool)
            .expect("the spool")
            .records;
        let journal = self.journal.borrow();
        [
            (journal.as_slice(), crate::land_record::Origin::Journal),
            (spooled.as_slice(), crate::land_record::Origin::Spool),
        ]
        .into_iter()
        .flat_map(|(signed, origin)| {
            signed
                .iter()
                .filter(|signed| signed.path.starts_with(crate::land_run::DIR))
                .map(move |signed| {
                    crate::land_run::Entry::read(&signed.path, &signed.bytes, origin)
                })
        })
        .collect()
    }

    /// Run `run`'s records the journal holds and those the spool holds, each read and in order.
    fn runs(&self, run: char) -> [Vec<crate::land_run::Record>; 2] {
        let read = |signed: &[crate::land_record::Signed]| {
            let mut records: Vec<crate::land_run::Record> = signed
                .iter()
                .filter(|signed| signed.path.starts_with(crate::land_run::DIR))
                .map(|signed| {
                    let entry = crate::land_run::Entry::read(
                        &signed.path,
                        &signed.bytes,
                        crate::land_record::Origin::Journal,
                    );
                    entry.body.expect("a run record reads").record
                })
                .filter(|record| record.run.id == run_id(run))
                .collect();
            records.sort_by_key(|record| record.seq);
            records
        };
        let spooled = crate::land_spool::Spool::pending(&*self.spool)
            .expect("the spool")
            .records;
        [read(&self.journal.borrow()), read(&spooled)]
    }
}

/// A run record's kind, result and cause word, as [`told`] reads it.
type Told = (String, String, String);

/// Each record's kind, result and cause word, in order.
fn told(records: &[crate::land_run::Record]) -> Vec<Told> {
    records
        .iter()
        .map(|record| {
            (
                record.kind.clone(),
                record.result.clone(),
                record
                    .cause
                    .as_ref()
                    .map_or_else(String::new, |cause| cause.word.clone()),
            )
        })
        .collect()
}

/// Whether `line` is an INTERRUPTED line naming `signal`: the word and the signal's number, not
/// the sentence around them.
fn interrupted_by(line: &str, signal: i32) -> bool {
    let signal = signal.to_string();
    line.contains("INTERRUPTED")
        && line
            .split(|c: char| !c.is_ascii_alphanumeric())
            .any(|word| word == signal)
}

/// The end records of `records`.
fn ends(records: &[crate::land_run::Record]) -> Vec<&crate::land_run::Record> {
    records
        .iter()
        .filter(|record| record.kind == "end")
        .collect()
}

/// Land `world`'s branch (its crate `a`) with `records`, each judgment run
/// by `judge` and each remote step by `remote`: its exit and what it said.
fn land_kept(
    world: &World,
    records: Records<'_>,
    mut judge: impl FnMut(Poll<'_>) -> Ran,
    remote: &RemoteStep<'_>,
) -> (i32, Vec<String>) {
    let said = RefCell::new(Vec::new());
    let branch = crates(&["a"]);
    let reach = whole_reach(&branch);
    let code = land(
        &world.work,
        &own(),
        &mut Seams {
            git: &git_cmd,
            autofix: &mut |_: &Path, _: &[String]| Ok(()),
            verifier: &mut |_: &Path| Ok(own_image()),
            judge: &mut |_: &Path, _: &Path, _: &str, poll: Poll<'_>| judge(poll),
            records,
            reach: &|_: &Path, _: &str, paths: Option<&[String]>| reach(paths),
            recheck: &mut |_: &Path, _: &Path, base: &str, _: &Delta| {
                panic!("no round was expected (base {base})")
            },
            hand_over: &mut |_: &Image, _: &Path, _: &Path| panic!("no handoff was expected"),
            say: &mut |line: &str| said.borrow_mut().push(line.to_owned()),
            remote,
            pause: &no_retry,
        },
    );
    (code, said.into_inner())
}

/// JM-01's exit (`docs/DESIGN-judged-main-release-pump-2026-11-11.md` §3.1): EVERY RUN END
/// WRITES ONE RECORD. Over the scripted seams, a run that lands (0), one that goes red (1), one
/// that could not run (3), one stopped by main's races at the push (75) and one interrupted by a
/// signal each leave exactly one end record, naming its exit, beside one record for each step
/// it took: the judgment and its result and red item keys, each compose. The landed one names
/// its landing commit, the one main holds, and its kind. The interrupted run's end record names
/// the signal, is written on its way out and kept in the clone, not appended (the run dies of
/// the signal), and it judged nothing; the next run's one append carries it. A failed append
/// leaves the exit, and what main holds, as they were without it: the run's records stay
/// spooled, said, and the next run (which finds nothing to land) appends them. The census of
/// every record these runs left prints the values they planted.
///
/// NEGATIVE CONTROLS, each run as a mutant of the code it guards, which made this test fail:
/// `land` without its `records::finish` (nothing the run recorded was appended, and it wrote no
/// end record); `finish` appending an
/// interrupted run's records (the interrupted run's end record was in the journal before the
/// next run); a failed append returning [`exit::COULD_NOT_RUN`] from `land` (the refused run's
/// exit was 3, not 0).
#[test]
fn every_run_end_writes_one_record() {
    use crate::land_record::tests::NOW;
    let remote: &RemoteStep<'_> = &live_remote;
    // Every record the runs below leave, journal and spool, for the census.
    let mut planted = Vec::new();

    // 0: landed.
    let store = Store::default();
    let world = world();
    commit(&world.work, "a/x.txt", "a2\n");
    let (code, said) = land_kept(&world, store.records('1'), |_| Ran::Ended(green()), remote);
    assert_eq!(code, exit::PASS, "{said:#?}");
    let [journal, spooled] = store.runs('1');
    assert!(spooled.is_empty(), "the run's append held its records");
    assert_eq!(
        told(&journal),
        [
            ("judgment".into(), "pass".into(), "first".into()),
            ("end".into(), "landed".into(), String::new())
        ]
    );
    let end = ends(&journal)[0];
    assert_eq!((end.exit, end.signal, end.seq), (Some(exit::PASS), None, 1));
    let landing = end.landing.as_ref().expect("the landing");
    assert_eq!(landing.commit, origin_main(&world));
    assert_eq!((landing.kind.as_str(), landing.resumed), ("exact", false));
    assert_eq!(end.secs, 60, "from the run's start to its end");
    let landed_tree = git(
        &world.work,
        &["rev-parse", &format!("{}^{{tree}}", landing.commit)],
    );

    planted.extend(store.entries());

    // 1: red, its item keyed by package, target and test path.
    let store = Store::default();
    let world = self::world();
    commit(&world.work, "a/x.txt", "a2\n");
    let (code, said) = land_kept(
        &world,
        store.records('2'),
        |_| {
            Ran::Ended(ran(
                exit::FAILED,
                vec![row("fmt", "ok"), red("test", &["-p a --lib -- t::one"])],
                3,
            ))
        },
        remote,
    );
    assert_eq!(code, exit::FAILED, "{said:#?}");
    let [journal, _] = store.runs('2');
    assert_eq!(
        told(&journal),
        [
            ("judgment".into(), "fail".into(), "first".into()),
            ("end".into(), "not-landed".into(), String::new())
        ]
    );
    let item = &journal[0].items[0];
    assert_eq!(
        (
            item.check.as_str(),
            item.package.as_deref(),
            item.target.as_deref(),
            item.test.as_deref()
        ),
        ("test", Some("a"), Some("lib"), Some("t::one"))
    );
    assert_eq!(ends(&journal)[0].exit, Some(exit::FAILED));

    planted.extend(store.entries());

    // 3: could not run.
    let store = Store::default();
    let world = self::world();
    commit(&world.work, "a/x.txt", "a2\n");
    let (code, said) = land_kept(
        &world,
        store.records('3'),
        |_| {
            Ran::Ended(ran(
                exit::COULD_NOT_RUN,
                vec![row("fmt", "ok"), row("test", "NOT RUN")],
                3,
            ))
        },
        remote,
    );
    assert_eq!(code, exit::COULD_NOT_RUN, "{said:#?}");
    let [journal, _] = store.runs('3');
    assert_eq!(
        told(&journal),
        [
            ("judgment".into(), "not-run".into(), "first".into()),
            ("end".into(), "not-landed".into(), String::new())
        ]
    );
    assert_eq!(ends(&journal)[0].exit, Some(exit::COULD_NOT_RUN));

    planted.extend(store.entries());

    // 75: main moves before every push, one race past the bound.
    let store = Store::default();
    let world = self::world();
    commit(&world.work, "a/x.txt", "a2\n");
    let moves = std::cell::Cell::new(0);
    let racing: &RemoteStep<'_> = &|root, git, tool, args, until, mode| {
        if args.iter().any(|word| word == "push") {
            moves.set(moves.get() + 1);
            rival_moves(&world, "b/y.txt", &format!("b moved {}\n", moves.get()));
        }
        live_remote(root, git, tool, args, until, mode)
    };
    let (code, said) = land_kept(&world, store.records('4'), |_| Ran::Ended(green()), racing);
    assert_eq!(code, STOPPED, "{said:#?}");
    let [journal, _] = store.runs('4');
    let composes = journal.iter().filter(|r| r.kind == "compose").count();
    assert_eq!(composes, COMPOSES, "{:#?}", told(&journal));
    let end = ends(&journal);
    assert_eq!(end.len(), 1, "{:#?}", told(&journal));
    assert_eq!((end[0].exit, end[0].kept), (Some(STOPPED), true));

    planted.extend(store.entries());

    // An interrupt: the signal comes while the judgment runs, which ends with no verdict.
    let store = Store::default();
    let world = self::world();
    commit(&world.work, "a/x.txt", "a2\n");
    let signal = store.signal.clone();
    let (code, said) = land_kept(
        &world,
        store.records('5'),
        |_| {
            signal.set(Some(15));
            Ran::Interrupted {
                secs: 1,
                queued: false,
            }
        },
        remote,
    );
    assert_eq!(code, exit::COULD_NOT_RUN, "{said:#?}");
    assert!(
        said.iter().any(|line| interrupted_by(line, 15)),
        "{said:#?}"
    );
    let [journal, spooled] = store.runs('5');
    assert!(journal.is_empty(), "an interrupted run appends nothing");
    assert_eq!(
        told(&spooled),
        [("end".into(), "not-landed".into(), String::new())],
        "no verdict, no judgment record: the end alone, kept in the clone"
    );
    assert_eq!(
        (spooled[0].exit, spooled[0].signal),
        (Some(exit::COULD_NOT_RUN), Some(15))
    );
    store.signal.set(None);
    let (code, said) = land_kept(&world, store.records('6'), |_| Ran::Ended(green()), remote);
    assert_eq!(code, exit::PASS, "{said:#?}");
    let [journal, spooled] = store.runs('5');
    assert_eq!(
        (ends(&journal).len(), spooled.len()),
        (1, 0),
        "the next run appended the interrupted one's end"
    );
    assert_eq!(ends(&store.runs('6')[0]).len(), 1);

    planted.extend(store.entries());

    // A failed append changes nothing the landing does.
    let store = Store::default();
    let world = self::world();
    commit(&world.work, "a/x.txt", "a2\n");
    store.refuse.set(true);
    let (code, said) = land_kept(&world, store.records('7'), |_| Ran::Ended(green()), remote);
    assert_eq!(code, exit::PASS, "{said:#?}");
    assert_eq!(
        git(
            &world.work,
            &["rev-parse", &format!("{}^{{tree}}", origin_main(&world))]
        ),
        landed_tree,
        "main holds the same landing as without the failed append"
    );
    assert!(
        said.iter()
            .any(|line| line.contains("(the scripted journal refuses appends)")),
        "the refusal is said: {said:#?}"
    );
    let [journal, spooled] = store.runs('7');
    assert!(journal.is_empty());
    assert_eq!(ends(&spooled).len(), 1);
    store.refuse.set(false);
    let (code, said) = land_kept(&world, store.records('8'), |_| Ran::Ended(green()), remote);
    assert_eq!(code, exit::PASS, "{said:#?}");
    let [journal, spooled] = store.runs('7');
    assert_eq!((told(&journal).len(), spooled.len()), (2, 0));
    let [journal, _] = store.runs('8');
    assert_eq!(
        told(&journal),
        [("end".into(), "nothing".into(), String::new())],
        "nothing to land: the end alone"
    );
    assert_eq!(journal[0].at, NOW);
    planted.extend(store.entries());

    // The census of these runs prints what they did: eight runs, one red first judgment of the
    // five that judged and one that did not run, one race lost at the push bound, four ends
    // without a landing (the run that found nothing to land is none), three exact landings a
    // minute each, and the stopped run's composes.
    let since = NOW - 86_400;
    assert_eq!(
        crate::land_census::census(&planted, &[], since),
        [
            format!(
                "verify land census since {} ({since}): 8 runs from 1 hosts; 0 records do not \
                 read",
                aterm_types::rfc3339::format_rfc3339(since)
            ),
            "  r (first judgments red): 1/5 = 20.0%; 1 did not run".to_owned(),
            "  runs that lost a race: 1/8 = 12.5%".to_owned(),
            "  runs that ended without landing: 4 (exit 1: 1, exit 3: 1, exit 75: 1, signal 15: \
             1)"
            .to_owned(),
            "  minutes to main: p50 1.0, p95 1.0 (over 3 landed runs)".to_owned(),
            "  landings by kind: exact 3 (0 resumed a kept judgment)".to_owned(),
            "  re-judges by cause: none".to_owned(),
            "  rounds by cause: none".to_owned(),
            format!("  composes: {COMPOSES}; aborts: none"),
            "  main tips with no receipt: 0 of 0".to_owned(),
            format!(
                "  receipted landings with no run record: 0 of 0 since {} (the earliest run \
                 record read)",
                aterm_types::rfc3339::format_rfc3339(NOW - 60)
            ),
        ]
    );
}

/// A landing resumed from another image's handoff goes on as the same run: its records name the
/// run's id and start and go on from the ordinal the handoff carries, as the sender's would
/// have. NEGATIVE CONTROL: a handoff of the schema before carries no run, and the receiver's
/// records are a run of its own, from its first ordinal. Mutant run: `resume` not adopting the
/// carried run wrote the end under the receiver's own id.
#[test]
fn a_resumed_landing_records_as_the_run_that_handed_it_over() {
    let world = world();
    commit(&world.work, "a/x.txt", "a2\n");
    let head = git(&world.work, &["rev-parse", "HEAD"]);
    for carried in [true, false] {
        let store = Store::default();
        let handoff = Handoff {
            root: world.work.clone(),
            // Handed to another image: the receiver refuses it before anything runs.
            image: "f".repeat(64),
            words: Vec::new(),
            step: aterm_land::Step::Qualify,
            hop: 1,
            at: head.clone(),
            base: head.clone(),
            composes: 0,
            cone: head.clone(),
            branch: None,
            stated: Stated::default(),
            run: carried.then(|| handoff::Carried {
                id: run_id('c'),
                started: 1_000,
                seq: 4,
                cause: Some(crate::land_run::Cause::new("cut", &head, "a tip")),
            }),
        };
        let said = RefCell::new(Vec::new());
        let code = resume(
            &world.work,
            &own(),
            handoff,
            &mut Seams {
                git: &git_cmd,
                autofix: &mut |_: &Path, _: &[String]| Ok(()),
                verifier: &mut |_: &Path| Ok(own_image()),
                judge: &mut |_: &Path, _: &Path, _: &str, _: Poll<'_>| panic!("nothing runs"),
                records: store.records('d'),
                reach: &|_: &Path, _: &str, _: Option<&[String]>| reaches(&["a"]),
                recheck: &mut |_: &Path, _: &Path, _: &str, _: &Delta| panic!("nothing runs"),
                hand_over: &mut |_: &Image, _: &Path, _: &Path| panic!("no handoff"),
                say: &mut |line: &str| said.borrow_mut().push(line.to_owned()),
                remote: &live_remote,
                pause: &no_retry,
            },
        );
        assert_eq!(code, exit::COULD_NOT_RUN, "{:#?}", said.borrow());
        let (own_run, other) = if carried { ('c', 'd') } else { ('d', 'c') };
        let [journal, _] = store.runs(own_run);
        let end = ends(&journal);
        assert_eq!(end.len(), 1, "{carried}");
        let expected = if carried {
            (4, 1_000)
        } else {
            (0, NOW_STARTED)
        };
        assert_eq!((end[0].seq, end[0].run.started), expected, "{carried}");
        assert!(store.runs(other)[0].is_empty(), "{carried}");
    }
}

/// When [`Store::records`]' runs start.
const NOW_STARTED: u64 = crate::land_record::tests::NOW - 60;

/// Land `world`'s branch as [`land_kept`] does, its verifier built by `verifier` and each
/// judgment ending as `judged`: its exit and what it said.
fn land_built(
    world: &World,
    records: Records<'_>,
    verifier: &mut dyn FnMut(&Path) -> Result<Image, String>,
    judged: &dyn Fn() -> Judged,
    remote: &RemoteStep<'_>,
) -> (i32, Vec<String>) {
    let said = RefCell::new(Vec::new());
    let branch = crates(&["a"]);
    let reach = whole_reach(&branch);
    let code = land(
        &world.work,
        &own(),
        &mut Seams {
            git: &git_cmd,
            autofix: &mut |_: &Path, _: &[String]| Ok(()),
            verifier,
            judge: &mut |_: &Path, _: &Path, _: &str, _: Poll<'_>| Ran::Ended(judged()),
            records,
            reach: &|_: &Path, _: &str, paths: Option<&[String]>| reach(paths),
            recheck: &mut |_: &Path, _: &Path, base: &str, _: &Delta| {
                panic!("no round was expected (base {base})")
            },
            hand_over: &mut |_: &Image, _: &Path, _: &Path| panic!("no handoff was expected"),
            say: &mut |line: &str| said.borrow_mut().push(line.to_owned()),
            remote,
            pause: &no_retry,
        },
    );
    (code, said.into_inner())
}

/// The commit the checkout's kept judgment is recorded in ([`KEPT`]), if one is.
fn kept_record(world: &World) -> Option<String> {
    let out = git_cmd(&world.work)
        .args(["rev-parse", "--verify", "--quiet", KEPT])
        .output()
        .expect("git runs");
    out.status
        .success()
        .then(|| String::from_utf8_lossy(&out.stdout).trim().to_owned())
}

/// AN INTERRUPT STOPS THE LANDING BEFORE IT CHANGES ANYTHING: whichever step's check of it
/// comes first (the verifier's build, the push, the merge of a main that rejected the push, the
/// first judgment of a landing that holds an earlier run's kept judgment), the landing says one
/// INTERRUPTED line naming the signal, pushes nothing, leaves HEAD where it was with no merge
/// begun and the kept judgment as it was, could not run, and spools exactly one end record
/// naming the signal, appending nothing.
///
/// NEGATIVE CONTROLS, each run once as a mutant of the check it guards, which made this test
/// fail: `built` saying a stopped build as one that did not build (no INTERRUPTED line);
/// `publish` without its check (main took the landing); `Host::merge` without its check (HEAD
/// moved to the merge of the rejecting tip); `Host::qualify` without its check (the earlier
/// run's kept judgment was dropped).
#[test]
fn an_interrupt_stops_the_landing_before_it_changes_anything() {
    let once = |said: &[String]| {
        assert_eq!(
            said.iter().filter(|line| interrupted_by(line, 15)).count(),
            1,
            "{said:#?}"
        );
    };
    let ended = |store: &Store, run: char| {
        let [journal, spooled] = store.runs(run);
        assert!(journal.is_empty(), "an interrupted run appends nothing");
        let end = ends(&spooled);
        assert_eq!(end.len(), 1, "{:#?}", told(&spooled));
        assert_eq!(
            (end[0].exit, end[0].signal),
            (Some(exit::COULD_NOT_RUN), Some(15))
        );
        end[0].kept
    };

    // The verifier's build: the interrupt stops it, and it is said as the interrupt.
    let store = Store::default();
    let world = world();
    commit(&world.work, "a/x.txt", "a2\n");
    let main = origin_main(&world);
    let signal = store.signal.clone();
    let (code, said) = land_built(
        &world,
        store.records('1'),
        &mut |_: &Path| {
            signal.set(Some(15));
            Err("the landing was interrupted while it built the checkout's verifier".into())
        },
        &green,
        &live_remote,
    );
    assert_eq!(code, exit::COULD_NOT_RUN, "{said:#?}");
    once(&said);
    assert!(
        !said.iter().any(|line| line.contains("did not build")),
        "{said:#?}"
    );
    assert_eq!(origin_main(&world), main);
    assert!(!ended(&store, '1'));

    // The push: a judgment that passed is kept, and nothing is written.
    let store = Store::default();
    let world = self::world();
    let judged = commit(&world.work, "a/x.txt", "a2\n");
    let main = origin_main(&world);
    let mut records = store.records('2');
    records.spool = Box::new(Tripwire {
        spool: records.spool,
        kind: "judgment",
        signal: store.signal.clone(),
    });
    let (code, said) = land_built(
        &world,
        records,
        &mut |_: &Path| Ok(own_image()),
        &green,
        &live_remote,
    );
    assert_eq!(code, exit::COULD_NOT_RUN, "{said:#?}");
    once(&said);
    assert_eq!(origin_main(&world), main, "nothing was pushed");
    assert_eq!(git(&world.work, &["rev-parse", "HEAD"]), judged);
    let kept = kept_record(&world).expect("the passed judgment is kept");
    assert!(
        ended(&store, '2'),
        "the end record says the judgment is kept"
    );

    // The first judgment of a later commit: the earlier run's kept judgment is not dropped.
    let store = Store::default();
    commit(&world.work, "a/x.txt", "a3\n");
    store.signal.set(Some(15));
    let (code, said) = land_built(
        &world,
        store.records('3'),
        &mut |_: &Path| panic!("nothing is built after the interrupt"),
        &green,
        &live_remote,
    );
    assert_eq!(code, exit::COULD_NOT_RUN, "{said:#?}");
    once(&said);
    assert_eq!(kept_record(&world), Some(kept), "the kept judgment stays");
    ended(&store, '3');

    // The merge of a main that rejected the push and does not compose (it shares a path whose
    // round would judge every crate): HEAD stays the judged commit, with no merge begun.
    let store = Store::default();
    let world = self::world();
    let shared: String = SHARED.replacen('1', "one", 1);
    let judged = commit(&world.work, "shared.txt", &shared);
    let signal = store.signal.clone();
    let moved = std::cell::Cell::new(false);
    let racing: &RemoteStep<'_> = &|root, git, tool, args, until, mode| {
        if args.iter().any(|word| word == "push") && !moved.replace(true) {
            rival_moves_line(&world, 8, "eight");
            signal.set(Some(15));
        }
        live_remote(root, git, tool, args, until, mode)
    };
    let (code, said) = land_built(
        &world,
        store.records('4'),
        &mut |_: &Path| Ok(own_image()),
        &green,
        racing,
    );
    assert_eq!(code, exit::COULD_NOT_RUN, "{said:#?}");
    once(&said);
    assert!(moved.get(), "main rejected the push");
    assert_eq!(
        git(&world.work, &["rev-parse", "HEAD"]),
        judged,
        "{said:#?}"
    );
    assert!(
        !world.work.join(".git/MERGE_HEAD").exists(),
        "no merge was begun"
    );
    assert!(kept_record(&world).is_some(), "the judgment stays kept");
    assert!(ended(&store, '4'));
    let [_, spooled] = store.runs('4');
    assert_eq!(
        told(&spooled),
        [
            ("judgment".into(), "pass".into(), "first".into()),
            ("abort".into(), "doomed".into(), "doomed".into()),
            ("end".into(), "not-landed".into(), String::new())
        ]
    );
}

/// A LANDING THAT PANICS STILL ENDS ITS RUN: its end record, naming [`PANICKED`], is written
/// (and, with no interrupt, appended) before the panic goes on, so the process dies of it as
/// without the record. NEGATIVE CONTROL, run once as a mutant: `land` calling `drive` with no
/// `catch_unwind` left the run with no end record.
#[test]
fn a_landing_that_panics_writes_its_end_record() {
    let store = Store::default();
    let world = world();
    commit(&world.work, "a/x.txt", "a2\n");
    let ran = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        land_kept(
            &world,
            store.records('1'),
            |_| panic!("a scripted fault mid-judgment"),
            &live_remote,
        )
    }));
    assert!(ran.is_err(), "the panic goes on past the record");
    let [journal, spooled] = store.runs('1');
    assert!(spooled.is_empty(), "{:#?}", told(&spooled));
    let end = ends(&journal);
    assert_eq!(end.len(), 1, "{:#?}", told(&journal));
    assert_eq!((end[0].exit, end[0].signal), (Some(PANICKED), None));
}

/// The record a judgment that stopped at its first red keeps, as the verifier writes it: `fmt`
/// passed, and the test check failed at `red` (a case of a crate outside the branch's) with the
/// binaries `unstarted` the stop kept from starting. Its rows come from [`crate::report::judge`]
/// and the events from [`crate::report::check_event`] and [`crate::report::run_event`], so the
/// halt's NOT RUN rows sit among the test check's findings as the verifier puts them there.
fn halted_test_run(red: &str, unstarted: &[&str]) -> Judged {
    let words = "stopped: the run failed at test";
    let mut fmt = crate::ladder::Report::new("fmt");
    fmt.pass("fmt");
    let mut test = crate::ladder::Report::new("test");
    test.fail_with_findings(
        red.split(" -- ").next().unwrap_or(red).to_owned(),
        vec![crate::ladder::Finding {
            id: red.to_owned(),
            rerun: None,
            message: Vec::new(),
        }],
    );
    for binary in unstarted {
        test.cannot_run(format!("{binary} — {words}"));
    }
    let results = [
        crate::report::judge(
            crate::Check::Fmt,
            &[&fmt],
            1.0,
            "",
            "rerun".into(),
            Some(words),
        ),
        crate::report::judge(
            crate::Check::Test,
            &[&test],
            60.0,
            "",
            "rerun".into(),
            Some(words),
        ),
    ];
    let mut text = crate::report::check_event(&results[0], &[&fmt]);
    text.push_str(&crate::report::check_event(&results[1], &[&test]));
    text.push_str(&crate::report::run_event(&results, exit::FAILED));
    Judged::from_events(&text, exit::FAILED, 61)
}

/// A RED A STOPPED RUN MET COUNTS: a judgment that stopped at its first red cut short that red's
/// own binary and kept what had not started from running; its red counts all the same, whether
/// the stopped binary was the last running (no other row) or others had not started (NOT RUN rows
/// among its findings), and wherever the red is.
#[test]
fn a_red_a_stopped_run_met_counts() {
    for unstarted in [&[][..], &["-p z --lib", "-p y --test t"][..]] {
        let judged = halted_test_run(THEIRS, unstarted);
        assert!(judged.whole(), "{judged:?}");
        let decision = decide(&judged);
        assert_eq!(decision.counted, ["test"], "{unstarted:?}: {decision:?}");
        assert!(decision.not_run.is_empty(), "{unstarted:?}: {decision:?}");
    }
}

/// WHAT A COUNTED RED STOPPED DECIDES NOTHING: the checks the stop kept from running beside a
/// counted red are named as stopped, and the red ends the landing. Beside no counted red they
/// were never judged, so they did not run. NEGATIVE CONTROL: a NOT RUN the machine made, not the
/// stop, is not run whatever else failed.
#[test]
fn what_a_counted_red_stopped_is_named_and_beside_none_did_not_run() {
    let judged = ran(
        exit::FAILED,
        vec![row("fmt", "ok"), red("test", &[THEIRS]), stopped("doc")],
        1,
    );
    let counted = decide(&judged);
    assert_eq!(counted.counted, ["test"]);
    assert_eq!(counted.stopped, ["doc"]);
    assert!(counted.not_run.is_empty(), "{counted:?}");
    let uncounted = ran(
        exit::COULD_NOT_RUN,
        vec![row("fmt", "ok"), stopped("doc")],
        1,
    );
    let cut = decide(&uncounted);
    assert!(cut.counted.is_empty() && cut.stopped.is_empty(), "{cut:?}");
    assert!(
        cut.not_run.iter().any(|why| why.starts_with("doc (")),
        "{cut:?}"
    );
    let machine = ran(
        exit::FAILED,
        vec![red("test", &[THEIRS]), row("doc", "NOT RUN")],
        1,
    );
    let decision = decide(&machine);
    assert!(decision.stopped.is_empty(), "{decision:?}");
    assert_eq!(decision.not_run, ["doc"]);
}

/// A LANDING STOPS AT ITS FIRST RED, whatever main's own state: nothing is pushed, and the
/// landing says what the stop kept from running, in its lines and in its closing verdict of at
/// most nine lines.
#[test]
fn a_landing_stops_at_its_first_red() {
    let world = world();
    commit(&world.work, "a/x.txt", "a2\n");
    let before = origin_main(&world);
    let said = RefCell::new(Vec::new());
    let code = land(
        &world.work,
        &own(),
        &mut Seams {
            git: &git_cmd,
            records: quiet(),
            verifier: &mut |_: &Path| Ok(own_image()),
            judge: &mut |_: &Path, _: &Path, _: &str, _: Poll<'_>| {
                // The stop keeps what would have come after the red from running.
                Ran::Ended(ran(
                    exit::FAILED,
                    vec![row("fmt", "ok"), red("test", &[THEIRS]), stopped("doc")],
                    1,
                ))
            },
            reach: &|_: &Path, _: &str, _: Option<&[String]>| panic!("no reach was expected"),
            recheck: &mut |_: &Path, _: &Path, _: &str, _: &Delta| {
                panic!("no recheck was expected")
            },
            autofix: &mut |_: &Path, _: &[String]| Ok(()),
            hand_over: &mut |_: &Image, _: &Path, _: &Path| panic!("no handoff was expected"),
            say: &mut |line: &str| said.borrow_mut().push(line.to_owned()),
            remote: &live_remote,
            pause: &no_retry,
        },
    );
    let said = said.into_inner();
    let (before_verdict, verdict) = split_verdict(&said);
    assert!(
        (1..=9).contains(&verdict.len()),
        "one verdict of at most nine lines: {said:#?}"
    );
    assert_eq!(code, exit::FAILED, "{said:#?}");
    assert_eq!(origin_main(&world), before, "nothing was pushed");
    assert!(
        before_verdict
            .iter()
            .any(|line| line.starts_with("verify land: STOPPED") && line.contains("doc")),
        "{said:#?}"
    );
    assert!(
        verdict.iter().any(|line| line.contains("doc NOT RUN")),
        "{said:#?}"
    );
}

/// THE VERDICT IS ONE SHORT BLOCK: landed or not with the commit, each check of the last
/// judgment with its word (and its time where it ran), its first red and where, what the stop
/// kept from running,
/// the full log, and the command that reads the run's events. NEGATIVE CONTROL: a landing where
/// nothing ran or landed here says none (its last line said it all).
#[test]
fn the_verdict_names_the_landing_each_check_the_first_counted_red_and_where_the_log_is() {
    let timed = |row: CheckRow, secs: u64| CheckRow {
        took: Some(Duration::from_secs(secs)),
        ..row
    };
    let ours = "-p a --lib -- t";
    let mut judged = ran(
        exit::FAILED,
        vec![
            timed(row("fmt", "ok"), 3),
            timed(red("test", &[THEIRS, ours]), 95),
            stopped("doc"),
        ],
        98,
    );
    let case = |id: &str, at: &str| crate::event_log::Red {
        what: id.replace(" -- ", ": "),
        id: Some(id.to_owned()),
        at: Some(at.to_owned()),
    };
    judged.reds = vec![
        case(THEIRS, "crates/b/src/lib.rs:4"),
        case(ours, "crates/a/src/lib_tests.rs:9"),
    ];
    judged.kept = Some(crate::event_log::Kept {
        log: PathBuf::from("/logs/run.log"),
        events: Some(PathBuf::from("/logs/run.log.events")),
    });
    let decision = decide(&judged);
    let last = || Last {
        round: false,
        tree: "tree1".into(),
        base: "base1".into(),
        judged: judged.clone(),
        decision: decision.clone(),
    };
    let tally = Tally {
        last: Some(last()),
        ..Tally::default()
    };
    let lines = verdict(exit::FAILED, Some("head1"), &tally);
    assert!((1..=9).contains(&lines.len()), "{lines:#?}");
    assert!(
        lines[0].contains("NOT LANDED")
            && lines[0].contains("exit 1")
            && lines[0].contains("head1"),
        "{lines:#?}"
    );
    let line_with = |words: &[&str]| {
        lines
            .iter()
            .filter(|line| words.iter().all(|word| line.contains(word)))
            .count()
    };
    assert_eq!(
        line_with(&["fmt ok 3.0s", "test FAIL 1m35s", "doc NOT RUN"]),
        1,
        "every check on one line: {lines:#?}"
    );
    assert_eq!(
        line_with(&["-p b --lib: old_red", "crates/b/src/lib.rs:4"]),
        1,
        "the first red, and where: {lines:#?}"
    );
    assert_eq!(
        line_with(&["crates/a/src/lib_tests.rs:9"]),
        0,
        "only the first red: {lines:#?}"
    );
    assert_eq!(line_with(&["/logs/run.log"]), 2, "the log, and its events");
    assert_eq!(
        line_with(&["verify status --events /logs/run.log.events"]),
        1
    );
    let landed = Tally {
        last: Some(last()),
        landed: Some("landing1".into()),
    };
    assert!(verdict(exit::PASS, Some("landing1"), &landed)[0].contains("LANDED landing1"));
    assert!(verdict(exit::COULD_NOT_RUN, None, &Tally::default()).is_empty());
}

/// A RED LANDING SAYS WHAT THE FOLLOWER LAST SAID OF MAIN, IN ONE LINE: the commit and its
/// verdict with where it was judged and each red, so a red main already has reads as main's.
/// Where the follower judged nothing of main's history, or the journal could not be read, the
/// line says that. NEGATIVE CONTROL: an unread journal is never reported as no verdict.
#[test]
fn a_red_landing_says_the_followers_newest_verdict_on_main() {
    let record = crate::judged::Record {
        commit: "c0ffee".into(),
        tree: "t".into(),
        host: "m86".into(),
        verifier: String::new(),
        controller: String::new(),
        inputs: String::new(),
        token: "one".into(),
        state: "red".into(),
        cause: String::new(),
        started: 1,
        updated: 2,
        lease_until: 0,
        judge: crate::judged::FOLLOWER.into(),
        repair: String::new(),
        repair_assignment: String::new(),
        repair_started: 0,
        rows: vec![crate::judged::Row {
            id: "tree".into(),
            outcome: "red".into(),
            log: String::new(),
            red_items: vec!["aterm-gui --lib a::b".into()],
            suspects: vec![],
            fixed_by: None,
            decisions: vec![],
        }],
    };
    assert_eq!(
        mains_verdict(Ok(Some(record))),
        "verify land: the follower's newest verdict on main (refs/aterm/judged): c0ffee red on m86: 1 red: aterm-gui --lib a::b"
    );
    assert_eq!(
        mains_verdict(Ok(None)),
        "verify land: the follower has filed no verdict on main (refs/aterm/judged)"
    );
    assert_eq!(
        mains_verdict(Err("journal away".into())),
        "verify land: the follower's verdict on main was not read: journal away"
    );
}

/// The finished run's time includes its tree preflight, excludes only its recorded turn wait,
/// and never mistakes a historical queue rank or an incomplete event tail for elapsed waiting.
#[test]
fn completed_judgment_seconds_keep_tree_work_apart_from_queue_wait() {
    let events = "verify-event: {\"schema\":1,\"event\":\"judgment-turn\",\"since_ms\":1,\"preflight_ms\":2000,\"waited_ms\":3000,\"held\":true}\n";
    assert_eq!(judging_seconds(events, Duration::from_secs(9)), 6);
    assert_eq!(
        judging_seconds(events.trim_end(), Duration::from_secs(9)),
        9
    );
    assert_eq!(judging_seconds("", Duration::from_secs(9)), 9);
    assert_eq!(judging_seconds(events, Duration::from_secs(1)), 0);
}
