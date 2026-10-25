// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! ONE BANNER PER BOX: a codex box that raises a program-claim need ([`crate::needs`]) is told
//! by one banner, codex's own notification or aterm's need banner, whichever comes first.
//!
//! codex writes its own notification (OSC 9) for a box just before the box's OSC 7501 `blocked`
//! report: its draw posts the pending notification, then its pre-draw tick writes the report.
//! Under its transcript overlay the report goes first, and the notification waits for the
//! overlay to close. Both orders are judged here in the engine's byte order
//! ([`aterm_core::terminal::Terminal::set_notification_admission`]), never on a clock:
//!
//! - the herald posts no banner for a program-claim need whose program wrote a notification
//!   while the report before the need's own stood, or while the need's own stands: that
//!   notification was the box's; nor for one whose box a banner of the herald's already told
//!   (codex-alab's own ask about it, before a person's Autopilot Off gave the box back);
//! - a notification the program writes while a `blocked` report the herald told stands is that
//!   box's own, already told: it is dropped.
//!
//! The herald's mark lasts only while the GUI shows that box ([`BoxBanner::shown`]): a
//! foreground move off the program (its death included, which writes no `clear`), or any other
//! report shown, lifts it, so a later program's notifications in the tab pass. Every other
//! program notification passes. The herald sets its mark only while a shown `blocked` report
//! stands, which only a program that reports its own boxes (codex) writes, so only such a
//! session's notifications are ever dropped.
//!
//! A seamless update carries only an actual queued herald receipt for its frozen box. The
//! successor matches that source episode to the checkpoint before binding its own local
//! episode, before the reader resumes. A spent need alone carries no banner receipt.

use aterm_core::terminal::program_state::{
    ProgramBlockedKind, ProgramState, ProgramStateReport, ProgramStateWord,
};
use std::sync::{Arc, Mutex, PoisonError};

/// One session's record of who told its box, shared by the engine's admission (the reader
/// thread) and the herald and the sweep (the main thread), each deciding under its lock.
#[derive(Debug, Default)]
pub(crate) struct BoxBanner(Mutex<Snapshot>);

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(crate) struct Snapshot {
    revision: u64,
    /// The report sequence (`ProgramStateReport::seq`) standing at the program's last admitted
    /// notification.
    noticed: Option<u64>,
    /// Where the `blocked` report whose box a banner of the herald's told began
    /// (`ProgramStateReport::began`), while the GUI still shows it: a re-send of that report is
    /// the same box.
    heralded: Option<u64>,
}

/// An actual queued herald receipt, bound to the frozen source box's episode.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct Receipt {
    kind: Option<ProgramBlockedKind>,
    began: u64,
}

impl Receipt {
    #[cfg(any(unix, test))]
    pub(crate) fn row(self) -> String {
        format!(
            "{}@{}",
            self.kind.map_or("unspecified", ProgramBlockedKind::as_str),
            self.began
        )
    }

    pub(crate) fn parse(row: &str) -> Option<Self> {
        let (kind, began) = row.split_once('@')?;
        let kind = match kind {
            "unspecified" => None,
            "permission" => Some(ProgramBlockedKind::Permission),
            "question" => Some(ProgramBlockedKind::Question),
            "auth" => Some(ProgramBlockedKind::Auth),
            _ => return None,
        };
        let began = began.parse::<u64>().ok().filter(|began| *began > 0)?;
        Some(Self { kind, began })
    }
}

impl BoxBanner {
    /// Capture handoff custody before final session guards are taken.
    #[cfg(unix)]
    pub(crate) fn snapshot(&self) -> Snapshot {
        *self.0.lock().unwrap_or_else(PoisonError::into_inner)
    }

    /// Keep this receipt state fixed through Commit without waiting under session guards.
    #[cfg(unix)]
    pub(crate) fn try_guard(&self) -> Option<std::sync::MutexGuard<'_, Snapshot>> {
        match self.0.try_lock() {
            Ok(guard) => Some(guard),
            Err(std::sync::TryLockError::Poisoned(poison)) => Some(poison.into_inner()),
            Err(std::sync::TryLockError::WouldBlock) => None,
        }
    }

    /// A receipt only for the exact frozen blocked report whose herald really queued.
    #[cfg(any(unix, test))]
    pub(crate) fn carry(&self, report: ProgramStateReport) -> (Option<Receipt>, Snapshot) {
        let told = self.0.lock().unwrap_or_else(PoisonError::into_inner);
        let receipt = report
            .state
            .filter(|state| state.state == ProgramStateWord::Blocked)
            .filter(|_| report.began > 0 && told.heralded == Some(report.began))
            .map(|state| Receipt {
                kind: state.kind,
                began: report.began,
            });
        (receipt, *told)
    }

    /// Bind a receipt to the restored local episode before the reader can admit output.
    /// A new box, an old checkpoint, or a mismatched source keeps ordinary fallback.
    pub(crate) fn adopt(&self, receipt: Option<Receipt>, report: ProgramStateReport) {
        let Some(receipt) = receipt else { return };
        if report.restored_from != Some(receipt.began)
            || report.state
                != Some(ProgramState {
                    state: ProgramStateWord::Blocked,
                    kind: receipt.kind,
                })
        {
            return;
        }
        let mut told = self.0.lock().unwrap_or_else(PoisonError::into_inner);
        if told.heralded != Some(report.began) {
            told.heralded = Some(report.began);
            told.revision = told.revision.wrapping_add(1);
        }
    }

    /// The engine's admission for this session's program notifications, written while
    /// `report` stands: `false` for the box the herald told.
    pub(crate) fn admit(&self, report: &ProgramStateReport) -> bool {
        let mut told = self.0.lock().unwrap_or_else(PoisonError::into_inner);
        let blocked = report
            .state
            .is_some_and(|state| state.state == ProgramStateWord::Blocked);
        if blocked && told.heralded == Some(report.began) {
            return false;
        }
        told.noticed = Some(report.seq);
        told.revision = told.revision.wrapping_add(1);
        true
    }

    /// [`Self::admit`] as the engine takes it.
    pub(crate) fn admission(
        self: &Arc<Self>,
    ) -> impl FnMut(&ProgramStateReport) -> bool + Send + 'static {
        let banner = Arc::clone(self);
        move |report| banner.admit(report)
    }

    /// The sweep's turn, at every move of the report the GUI shows: `report` is where the shown
    /// `blocked` report began, `None` while the GUI shows no box (another report, none, or the
    /// floor a foreground move sets). The herald's mark stands only on the box shown, so a box
    /// the GUI no longer shows drops nothing.
    pub(crate) fn shown(&self, report: Option<u64>) {
        let mut told = self.0.lock().unwrap_or_else(PoisonError::into_inner);
        if told.heralded.is_some() && told.heralded != report {
            told.heralded = None;
            told.revision = told.revision.wrapping_add(1);
        }
    }

    /// The herald's turn on the box whose `blocked` report began at engine sequence `report`
    /// ([`crate::needs::Need::report`]): `decide` is told whether the box was told already (the
    /// program's own notification was written while the report before it stood, or since, or a
    /// banner of the herald's told it) and answers what it decided and whether a banner went
    /// out, which marks the box told. No notification is admitted while it decides.
    pub(crate) fn herald<T>(&self, report: u64, decide: impl FnOnce(bool) -> (T, bool)) -> T {
        let mut told = self.0.lock().unwrap_or_else(PoisonError::into_inner);
        let by_program = told
            .noticed
            .is_some_and(|seq| seq.saturating_add(1) >= report);
        let (decided, posted) = decide(by_program || told.heralded == Some(report));
        if posted {
            told.heralded = Some(report);
            told.revision = told.revision.wrapping_add(1);
        }
        decided
    }

    /// The herald's turn on a need of another kind a person answers at the box (an ask, such as
    /// codex-alab's own) while the box whose `blocked` report began at `report` is shown:
    /// `decide` answers what it decided and whether a banner went out, which tells that box
    /// too. No notification is admitted while it decides.
    pub(crate) fn herald_beside<T>(&self, report: u64, decide: impl FnOnce() -> (T, bool)) -> T {
        let mut told = self.0.lock().unwrap_or_else(PoisonError::into_inner);
        let (decided, posted) = decide();
        if posted {
            told.heralded = Some(report);
            told.revision = told.revision.wrapping_add(1);
        }
        decided
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use aterm_core::terminal::Terminal;

    const WORKING: &[u8] = b"\x1b]7501;state=working:app=codex\x1b\\";
    const BLOCKED_PERMISSION: &[u8] = b"\x1b]7501;state=blocked:app=codex:kind=permission\x1b\\";
    const BOX_NOTICE: &[u8] = b"\x1b]9;Approval requested: rm -rf build\x07";
    const TURN_DONE: &[u8] = b"\x1b]9;Agent turn complete\x07";
    const DONE: &[u8] = b"\x1b]7501;state=done:app=codex\x1b\\";

    /// A codex engine with the session's admission and a callback that counts what passes.
    fn engine() -> (Terminal, Arc<BoxBanner>, Arc<Mutex<Vec<String>>>) {
        let mut term = Terminal::new(24, 80);
        let banner = Arc::new(BoxBanner::default());
        let passed = Arc::new(Mutex::new(Vec::new()));
        let sink = Arc::clone(&passed);
        term.set_notification_admission(banner.admission());
        term.set_notification_callback(move |body| sink.lock().unwrap().push(body.to_string()));
        let _ = term.apply_config(&aterm_core::config::TerminalConfig {
            allow_notifications: true,
            ..aterm_core::config::TerminalConfig::default()
        });
        (term, banner, passed)
    }

    /// The herald's turn as `App::herald_session` takes it: post unless the program told it.
    fn herald(banner: &BoxBanner, report: u64) -> bool {
        banner.herald(report, |by_program| (!by_program, !by_program))
    }

    #[test]
    fn an_unreadable_optional_episode_keeps_the_carried_program_report() {
        const BODY: &str = "state=blocked:app=codex:kind=permission";
        for identity in [
            aterm_json::json!(-1),
            aterm_json::json!(0),
            aterm_json::json!(true),
            aterm_json::json!("wrong"),
            aterm_json::json!([]),
            aterm_json::json!({}),
            aterm_json::Value::Null,
        ] {
            let read: aterm_core::terminal::OutputStateRepr =
                aterm_json::from_value(aterm_json::json!({
                    "program_state": BODY,
                    "program_state_began": identity,
                }))
                .expect("the optional identity never refuses the output state");
            assert_eq!(read.program_state.as_deref(), Some(BODY));
            assert_eq!(read.program_state_began, None);
            let mut checkpoint = Terminal::new(4, 20).checkpoint_carry(0).expect("Ground");
            checkpoint.output_state = read;
            let report = Terminal::from_checkpoint(&checkpoint).program_state();
            assert!(
                report.state.is_some(),
                "the program's blocked report still restores"
            );
            assert_eq!(
                report.restored_from, None,
                "only receipt inheritance is refused"
            );
        }
    }

    /// Hydration can precede the first GUI projection: bind only the exact queued
    /// source receipt, and let new/resolved/old/unidentified boxes keep fallback.
    #[test]
    fn a_carried_receipt_is_bound_before_projection_to_only_its_restored_box() {
        let (mut source, banner, _) = engine();
        source.process(WORKING);
        source.process(BLOCKED_PERMISSION);
        assert!(herald(&banner, source.program_state().began));
        let receipt = banner
            .carry(source.program_state())
            .0
            .expect("the herald queued this box");
        let checkpoint = source.checkpoint_carry(0).expect("Ground");
        assert_eq!(
            checkpoint.output_state.program_state_began,
            Some(receipt.began)
        );
        for case in [
            "same",
            "resend",
            "absent",
            "old",
            "wrong-source",
            "wrong-kind",
            "new-same-kind",
            "resolved",
            "reset",
        ] {
            let mut checkpoint = checkpoint.clone();
            if case == "old" {
                checkpoint.output_state.program_state_began = None;
            }
            let mut restored = Terminal::from_checkpoint(&checkpoint);
            match case {
                "resend" => restored.process(BLOCKED_PERMISSION),
                "new-same-kind" => restored.process(&[WORKING, BLOCKED_PERMISSION].concat()),
                "resolved" => restored.process(WORKING),
                "reset" => restored.process(b"\x1bc"),
                _ => {}
            }
            let receipt = match case {
                "absent" => None,
                "wrong-source" => Some(Receipt {
                    began: receipt.began + 1,
                    ..receipt
                }),
                "wrong-kind" => Some(Receipt {
                    kind: Some(ProgramBlockedKind::Question),
                    ..receipt
                }),
                _ => Some(receipt),
            };
            let adopted = Arc::new(BoxBanner::default());
            adopted.adopt(receipt, restored.program_state());
            restored.set_notification_admission(adopted.admission());
            let passed = Arc::new(Mutex::new(Vec::new()));
            let sent = passed.clone();
            restored
                .set_notification_callback(move |body| sent.lock().unwrap().push(body.to_string()));
            let _ = restored.apply_config(&aterm_core::config::TerminalConfig {
                allow_notifications: true,
                ..Default::default()
            });
            restored.process(BOX_NOTICE);
            assert_eq!(
                passed.lock().unwrap().is_empty(),
                matches!(case, "same" | "resend"),
                "{case}: identity binding precedes any projection"
            );
        }
        for invalid in [
            "",
            "permission@0",
            "permission@-1",
            "permission@wrong",
            "later@1",
            "permission@1@2",
        ] {
            assert_eq!(Receipt::parse(invalid), None, "{invalid}");
        }
        assert_eq!(Receipt::parse(&receipt.row()), Some(receipt));
    }

    /// A successor's first unhonoured projection can be None to None while an
    /// inherited marker still stands. That projection must release the marker.
    #[test]
    fn a_first_sweep_off_codex_releases_an_inherited_box_receipt() {
        let (mut source, source_banner, _) = engine();
        source.process(&[WORKING, BLOCKED_PERMISSION].concat());
        assert!(herald(&source_banner, source.program_state().began));
        let receipt = source_banner.carry(source.program_state()).0;
        let checkpoint = source.checkpoint_carry(0).expect("Ground");
        for (program, inherited) in [("codex", true), ("claude", false), ("claude", true)] {
            let mut app = crate::App::headless_for_test();
            let session = app.pool.get(0).unwrap();
            let term = session.term.clone();
            let ctx = session.ctx.clone();
            let passed = Arc::new(Mutex::new(Vec::new()));
            {
                let mut term = crate::term_lock(&term);
                term.restore_checkpoint(&checkpoint);
                if inherited {
                    ctx.box_banner.adopt(receipt, term.program_state());
                }
                term.set_notification_admission(ctx.box_banner.admission());
                let sink = passed.clone();
                term.set_notification_callback(move |body| {
                    sink.lock().unwrap().push(body.to_string())
                });
                let _ = term.apply_config(&aterm_core::config::TerminalConfig {
                    allow_notifications: true,
                    ..Default::default()
                });
            }
            {
                let mut timeline = ctx.timeline.lock().unwrap();
                timeline.note_foreground_group(4242);
                timeline.set_program(4242, Some(program.into()));
            }
            let _ = app.observe_session_statuses(std::time::Instant::now());
            assert_eq!(
                app.session_status.program_state(0).is_some(),
                program == "codex",
                "{program}/{inherited}: the actual gate projection"
            );
            crate::term_lock(&term).process(BOX_NOTICE);
            assert_eq!(
                passed.lock().unwrap().len(),
                usize::from(program != "codex"),
                "{program}/{inherited}: only the still-shown Codex box drops its notice"
            );
        }
    }

    #[test]
    fn disabled_projection_cannot_revive_a_pending_box_receipt() {
        let (mut source, source_banner, _) = engine();
        source.process(&[WORKING, BLOCKED_PERMISSION].concat());
        assert!(herald(&source_banner, source.program_state().began));
        let receipt = source_banner.carry(source.program_state()).0;
        let checkpoint = source.checkpoint_carry(0).expect("Ground");
        for path in [
            "disabled-sweep",
            "disabled-config",
            "reenabled-before-adopt",
        ] {
            let mut app = crate::App::headless_for_test();
            let session = app.pool.get(0).unwrap();
            let term = session.term.clone();
            let ctx = session.ctx.clone();
            let passed = Arc::new(Mutex::new(Vec::new()));
            let report = {
                let mut term = crate::term_lock(&term);
                term.restore_checkpoint(&checkpoint);
                let report = term.program_state();
                ctx.box_banner.adopt(receipt, report);
                term.set_notification_admission(ctx.box_banner.admission());
                let sink = passed.clone();
                term.set_notification_callback(move |body| {
                    sink.lock().unwrap().push(body.to_string())
                });
                let _ = term.apply_config(&aterm_core::config::TerminalConfig {
                    allow_notifications: true,
                    ..Default::default()
                });
                report
            };
            ctx.timeline
                .lock()
                .unwrap()
                .note_program_state(report.state, report.began);
            app.status_carry.insert(
                ctx.self_id.as_str().to_string(),
                crate::status_item::StatusCarry {
                    box_receipt: receipt,
                    ..Default::default()
                },
            );
            app.config.tab_status = Some(false);
            if path == "disabled-sweep" {
                let _ = app.observe_session_statuses(std::time::Instant::now());
            } else {
                app.reconfigure_session_status();
                assert!(
                    !app.session_factory.tab_status,
                    "new adopted readers also skip inheritance while disabled"
                );
            }
            assert_eq!(
                app.status_carry[ctx.self_id.as_str()].box_receipt,
                None,
                "{path}: a stale shown report cannot later revive this receipt"
            );
            if path == "reenabled-before-adopt" {
                app.config.tab_status = Some(true);
                app.reconfigure_session_status();
                assert!(app.session_factory.tab_status);
            }
            app.herald_session(0);
            crate::term_lock(&term).process(BOX_NOTICE);
            assert_eq!(
                passed.lock().unwrap().len(),
                1,
                "{path}: the program notification keeps fallback after status adoption"
            );
        }
    }

    /// codex's order: its notification, then the box's `blocked` report, in one write. The
    /// herald posts nothing for the box; the notification passed. One banner.
    #[test]
    fn a_box_codex_told_first_gets_no_need_banner() {
        let (mut term, banner, passed) = engine();
        term.process(WORKING);
        term.process(&[BOX_NOTICE, BLOCKED_PERMISSION].concat());
        let report = term.program_state().began;
        assert!(!herald(&banner, report), "codex's banner told the box");
        assert_eq!(passed.lock().unwrap().len(), 1, "codex's banner passed");
        term.process(WORKING);
        term.process(BLOCKED_PERMISSION);
        assert!(
            herald(&banner, term.program_state().began),
            "CONTROL: a later box codex wrote no notification for is aterm's to tell"
        );
    }

    /// The overlay's order: the `blocked` report, the herald's banner, then codex's notification
    /// for the same box, which is dropped; once the box is answered, codex's next notification
    /// passes. One banner.
    #[test]
    fn a_box_the_herald_told_first_drops_codexs_late_notification() {
        let (mut term, banner, passed) = engine();
        term.process(WORKING);
        term.process(BLOCKED_PERMISSION);
        assert!(
            herald(&banner, term.program_state().began),
            "aterm tells it"
        );
        term.process(BOX_NOTICE);
        assert!(
            passed.lock().unwrap().is_empty(),
            "codex's late banner is dropped"
        );
        term.process(&[WORKING, TURN_DONE, DONE].concat());
        assert_eq!(
            *passed.lock().unwrap(),
            vec!["Agent turn complete".to_string()],
            "the turn's own notification passes"
        );
    }

    /// The herald held back (rate limit) when codex's late notification came: the notification
    /// passes and the herald, deciding later, posts nothing. One banner.
    #[test]
    fn a_box_codex_told_while_the_herald_waited_gets_no_need_banner() {
        let (mut term, banner, passed) = engine();
        term.process(WORKING);
        term.process(BLOCKED_PERMISSION);
        let report = term.program_state().began;
        assert!(
            !banner.herald(report, |by_program| {
                assert!(!by_program);
                (false, false)
            }),
            "PRECONDITION: the herald held it back"
        );
        term.process(BOX_NOTICE);
        assert_eq!(passed.lock().unwrap().len(), 1, "codex's banner passed");
        assert!(!herald(&banner, report), "codex told the box meanwhile");
    }

    /// A notification written while a box the herald told stands is dropped, so the next box's
    /// own notification, written in the same frame before its report, is dropped with it: the
    /// herald tells that box too (its program's notification never passed).
    #[test]
    fn a_dropped_notification_tells_no_box() {
        let (mut term, banner, passed) = engine();
        term.process(BLOCKED_PERMISSION);
        assert!(herald(&banner, term.program_state().began));
        term.process(
            &[
                b"\x1b]9;Question: which branch?\x07".as_slice(),
                b"\x1b]7501;state=blocked:app=codex:kind=question\x1b\\",
            ]
            .concat(),
        );
        assert!(
            passed.lock().unwrap().is_empty(),
            "written under the told box"
        );
        assert!(
            herald(&banner, term.program_state().began),
            "the question is aterm's to tell"
        );
    }

    /// codex re-sends the box's report (after an external editor, under its cache's forced
    /// edge): the same box, so its late notification is still dropped.
    #[test]
    fn a_box_sent_again_is_the_box_the_herald_told() {
        let (mut term, banner, passed) = engine();
        term.process(WORKING);
        term.process(BLOCKED_PERMISSION);
        assert!(herald(&banner, term.program_state().began));
        term.process(BLOCKED_PERMISSION);
        term.process(BOX_NOTICE);
        assert!(passed.lock().unwrap().is_empty(), "the same box, told once");
    }

    /// codex re-sends an unchanged `working` (after an external program or a suspend), then
    /// writes the box's notification and its report: the notification told the box, judged by
    /// the report sequence standing at it, never by where that `working` began.
    #[test]
    fn a_notification_under_a_re_sent_report_told_the_box() {
        let (mut term, banner, passed) = engine();
        term.process(WORKING);
        term.process(WORKING);
        term.process(&[BOX_NOTICE, BLOCKED_PERMISSION].concat());
        assert_eq!(passed.lock().unwrap().len(), 1, "codex's banner passed");
        assert!(
            !herald(&banner, term.program_state().began),
            "a notification under a re-sent report told the box"
        );
    }

    /// The herald's mark stands only on the box the GUI shows: once the sweep shows none (a
    /// foreground move off a codex that died at the box, writing no `clear`), the engine's
    /// stale `blocked` report drops nothing. CONTROL: while the box is shown, codex's late
    /// notification for it is dropped.
    #[test]
    fn a_box_the_gui_no_longer_shows_drops_nothing() {
        let (mut term, banner, passed) = engine();
        term.process(WORKING);
        term.process(BLOCKED_PERMISSION);
        let report = term.program_state().began;
        banner.shown(Some(report));
        assert!(herald(&banner, report), "aterm tells it");
        banner.shown(Some(report));
        term.process(BOX_NOTICE);
        assert!(
            passed.lock().unwrap().is_empty(),
            "CONTROL: the box shown drops its late notification"
        );
        banner.shown(None);
        term.process(b"\x1b]9;build finished\x07");
        assert_eq!(
            *passed.lock().unwrap(),
            vec!["build finished".to_string()],
            "a box no longer shown drops nothing"
        );
    }

    /// A banner of another kind a person answers at the box (codex-alab's own ask about it)
    /// tells the box: the program-claim need it raises once the ask is gone (a person's
    /// Autopilot Off) is told already, and codex's late notification for it is dropped.
    /// CONTROL: an ask the herald posted no banner for tells nothing.
    #[test]
    fn a_box_an_ask_told_is_told() {
        let (mut term, banner, passed) = engine();
        term.process(WORKING);
        term.process(BLOCKED_PERMISSION);
        let report = term.program_state().began;
        banner.herald_beside(report, || ((), false));
        assert!(
            banner.herald(report, |told| (!told, false)),
            "CONTROL: an ask with no banner told nothing"
        );
        banner.herald_beside(report, || ((), true));
        assert!(!herald(&banner, report), "the ask's banner told the box");
        term.process(BOX_NOTICE);
        assert!(
            passed.lock().unwrap().is_empty(),
            "the told box drops codex's late notification"
        );
    }

    /// Negative control: a session whose herald never told a box (no program-claim need, so
    /// every non-codex session) drops nothing.
    #[test]
    fn a_session_the_herald_marked_nothing_on_drops_nothing() {
        let (mut term, _banner, passed) = engine();
        for bytes in [
            WORKING,
            BOX_NOTICE,
            BLOCKED_PERMISSION,
            BOX_NOTICE,
            TURN_DONE,
        ] {
            term.process(bytes);
        }
        assert_eq!(passed.lock().unwrap().len(), 3);
    }
}
