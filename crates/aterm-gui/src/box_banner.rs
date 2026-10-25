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

use aterm_core::terminal::program_state::{ProgramStateReport, ProgramStateWord};
use std::sync::{Arc, Mutex, PoisonError};

/// One session's record of who told its box, shared by the engine's admission (the reader
/// thread) and the herald and the sweep (the main thread), each deciding under its lock.
#[derive(Debug, Default)]
pub(crate) struct BoxBanner(Mutex<Told>);

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
struct Told {
    /// The report sequence (`ProgramStateReport::seq`) standing at the program's last admitted
    /// notification.
    noticed: Option<u64>,
    /// Where the `blocked` report whose box a banner of the herald's told began
    /// (`ProgramStateReport::began`), while the GUI still shows it: a re-send of that report is
    /// the same box.
    heralded: Option<u64>,
}

impl BoxBanner {
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
        if told.heralded != report {
            told.heralded = None;
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
