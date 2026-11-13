// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

use super::test_arms::{absent, on, unread};
use super::*;

#[test]
fn the_quiet_moment_needs_every_input_and_each_alone_holds_it() {
    assert!(quiet(true, false, true, true, false), "the quiet moment");
    assert!(!quiet(false, false, true, true, false), "aterm not focused");
    assert!(!quiet(true, true, true, true, false), "typing");
    assert!(
        !quiet(true, false, false, true, false),
        "nobody there for five minutes"
    );
    assert!(
        !quiet(true, false, true, false, false),
        "a launch not yet settled"
    );
    assert!(
        !quiet(true, false, true, true, true),
        "a hand-off under way"
    );
    assert!(QUIET_FOR < PRESENT_WITHIN);
}

fn fda_facts() -> FdaFacts {
    FdaFacts {
        fda: FdaState::Denied,
        declined: false,
        grant_stable: true,
        recent: false,
        devtools_recent: false,
        shown_this_revision: false,
        own: true,
    }
}

#[test]
fn the_full_disk_access_hand_over_is_a_table_of_named_refusals() {
    let hand = Admit::Hand {
        list_terminal: false,
    };
    for origin in [Origin::Settings, Origin::Digest] {
        assert_eq!(admit_fda(origin, Moment::Asked, &fda_facts()), hand);
        let refused_everything = FdaFacts {
            fda: FdaState::Unknown,
            declined: true,
            grant_stable: false,
            recent: true,
            devtools_recent: true,
            shown_this_revision: true,
            own: false,
        };
        assert_eq!(admit_fda(origin, Moment::Asked, &refused_everything), hand);
        let granted = FdaFacts {
            fda: FdaState::Granted,
            ..fda_facts()
        };
        assert_eq!(admit_fda(origin, Moment::Asked, &granted), Admit::PaneOnly);
    }
    let auto = Origin::Automatic;
    assert_eq!(admit_fda(auto, Moment::Asked, &fda_facts()), Admit::Queue);
    assert_eq!(admit_fda(auto, Moment::Quiet, &fda_facts()), hand);
    let cases = [
        (
            FdaFacts {
                fda: FdaState::Granted,
                ..fda_facts()
            },
            Refusal::Granted,
        ),
        (
            FdaFacts {
                fda: FdaState::Unknown,
                ..fda_facts()
            },
            Refusal::Unknown,
        ),
        (
            FdaFacts {
                declined: true,
                ..fda_facts()
            },
            Refusal::Declined,
        ),
        (
            FdaFacts {
                grant_stable: false,
                ..fda_facts()
            },
            Refusal::Unstable,
        ),
        (
            FdaFacts {
                shown_this_revision: true,
                ..fda_facts()
            },
            Refusal::Shown,
        ),
        (
            FdaFacts {
                recent: true,
                ..fda_facts()
            },
            Refusal::Recent,
        ),
    ];
    let act = Origin::Ctl;
    for (facts, why) in cases {
        for moment in [Moment::Asked, Moment::Quiet] {
            assert_eq!(
                admit_fda(auto, moment, &facts),
                Admit::Refuse(why),
                "{facts:?} {moment:?}"
            );
            let for_act = if why == Refusal::Shown {
                // An act asks: the page shown this revision does not hold it.
                if moment == Moment::Quiet {
                    hand
                } else {
                    Admit::Queue
                }
            } else {
                Admit::Refuse(why)
            };
            assert_eq!(
                admit_fda(act, moment, &facts),
                for_act,
                "a client's act: {facts:?} {moment:?}"
            );
        }
    }
    assert_eq!(admit_fda(act, Moment::Asked, &fda_facts()), Admit::Queue);
    let devtools_recent = FdaFacts {
        devtools_recent: true,
        ..fda_facts()
    };
    for moment in [Moment::Asked, Moment::Quiet] {
        assert_eq!(
            admit_fda(auto, moment, &devtools_recent),
            Admit::Queue,
            "{moment:?}"
        );
    }
    let not_own = FdaFacts {
        own: false,
        ..fda_facts()
    };
    assert_eq!(admit_fda(auto, Moment::Asked, &not_own), Admit::Queue);
    assert_eq!(
        admit_fda(auto, Moment::Quiet, &not_own),
        Admit::Refuse(Refusal::NotOwn)
    );
}

/// A page brought up while no access revision stands keeps the marker's revision step for
/// this bundle and Mac (Set up keeps the revision, so its page stays shown); under a revision,
/// or over a word under a version or another bundle's or Mac's, the page's own key is written.
#[test]
fn a_full_disk_access_page_up_with_no_revision_keeps_this_identitys_revision_step() {
    const KEY: &str = "0.137.0@com.aterm.aterm@HOST-A";
    let word = |revision: bool, opened: Option<&str>| fda_opened_word(revision, opened, KEY);
    for kept in [
        "r4@com.aterm.aterm@HOST-A",
        "r4@com.aterm.aterm",
        "r4",
        "r12",
    ] {
        assert_eq!(word(false, Some(kept)), kept, "{kept}: kept");
        assert_eq!(
            word(true, Some(kept)),
            KEY,
            "{kept}: a revision writes its own key"
        );
    }
    for other in [
        "r4@com.aterm.aterm.dev@HOST-A",
        "r4@com.aterm.aterm@HOST-B",
        "0.136.0@com.aterm.aterm@HOST-A",
        "rc1@com.aterm.aterm@HOST-A",
        "r@com.aterm.aterm@HOST-A",
        "",
    ] {
        assert_eq!(word(false, Some(other)), KEY, "{other:?}: gives way");
    }
    assert_eq!(word(false, None), KEY);
    assert_eq!(
        fda_opened_word(
            true,
            Some("r3@com.aterm.aterm@HOST-A"),
            "r4@com.aterm.aterm@HOST-A"
        ),
        "r4@com.aterm.aterm@HOST-A"
    );
}

fn fda_show(bundle: Option<PathBuf>) -> Show {
    Show {
        page: Page::FullDiskAccess,
        origin: Origin::Automatic,
        reading: Reading::UNREAD,
        listed: None,
        bundle,
        skips: false,
        step: None,
    }
}

#[test]
fn the_full_disk_access_panels_words_in_each_state() {
    let now = Instant::now();
    let bundle = PathBuf::from("/Applications/aterm.app");
    assert_eq!(
        FDA_PANEL_WHY,
        format!("{}.", crate::consent_card::FDA_GRANTS),
        "the panel's why is the card's one spelling"
    );
    let mut panel = Panel::new(fda_show(Some(bundle.clone())), now);
    assert_eq!(panel.page(), Page::FullDiskAccess);
    assert!(!panel.readable() && !panel.wants_read(now + PANEL_TICK));
    let mut seen = Vec::new();
    let explain = panel.content();
    assert!(has(&explain, FDA_PANEL_TITLE) && has(&explain, FDA_PANEL_WHY));
    assert!(has(&explain, FDA_PANEL_HOW) && has(&explain, FDA_PANEL_TRADE));
    assert_eq!(
        explain.tile(),
        Some(&Tile {
            bundle: bundle.clone(),
            live: true,
            step: FDA_PANEL_STEP,
        })
    );
    assert_eq!(explain.tile().map(Tile::hint), Some(FDA_PANEL_STEP));
    assert_eq!(
        explain.buttons(),
        [
            Button::OpenPage(Page::FullDiskAccess),
            Button::SwitchedOn,
            Button::NotNow
        ]
    );
    seen.push(explain);
    let plus = Panel::new(fda_show(None), now).content();
    assert!(plus.tile().is_none() && has(&plus, FDA_PANEL_HOW_PLUS));
    seen.push(plus);
    let _ = panel.on(PanelEvent::DragEnded { accepted: false }, now);
    let failed = panel.content();
    assert!(has(&failed, FDA_DROP_FAILED_WORDS));
    seen.push(failed);
    let _ = panel.on(PanelEvent::DragEnded { accepted: true }, now);
    let dropped = panel.content();
    assert!(panel.dropped() && has(&dropped, FDA_PANEL_DROPPED));
    assert_eq!(dropped.tile().map(Tile::hint), Some(TILE_SPENT_HINT));
    seen.push(dropped);
    let effect = panel.granted(now);
    assert!(effect.granted && effect.redraw);
    let done = panel.content();
    assert!(has(&done, FDA_PANEL_DONE) && done.buttons().is_empty());
    seen.push(done);
    // A walk's page with a page after it: the done words wait for that page and name it.
    let next = next_words("Developer Tools");
    panel.waits = Some("Developer Tools");
    let waiting = panel.content();
    assert!(has(&waiting, FDA_PANEL_NEXT_LATER) && has(&waiting, &next));
    assert!(waiting.buttons().is_empty());
    seen.push(waiting);
    panel.later = false;
    let successor = panel.content();
    assert!(has(&successor, FDA_PANEL_NEXT) && has(&successor, &next));
    seen.push(successor);
    assert!(
        seen[..4]
            .iter()
            .all(|content| has(content, FDA_PANEL_LATER)),
        "every page the switch is still to flip on says Later to macOS's sheet"
    );
    let (mut reopen_words, mut names_next) = (0, 0);
    for content in &seen {
        for (text, _) in content.texts() {
            if text == next {
                names_next += 1;
                continue;
            }
            assert!(!text.contains("Developer Tools"), "{text}");
            // The copy rule (DESIGN-macos-tcc-prompts §3.4): aterm never prompts a restart;
            // its Reopen words are the lines that tell the person to decline macOS's.
            let lower = text.to_lowercase();
            if ["reopen", "restart", "relaunch", "quit"]
                .iter()
                .any(|word| lower.contains(word))
            {
                assert!(
                    text == FDA_PANEL_LATER || text == FDA_PANEL_NEXT_LATER,
                    "{text}"
                );
                reopen_words += 1;
            }
        }
        assert!(!content.buttons().contains(&Button::ShowList));
    }
    assert!(
        reopen_words > 0,
        "NEGATIVE CONTROL: the Later lines are the ones it lets by"
    );
    assert_eq!(
        names_next, 2,
        "only the waiting done words name the next page"
    );
}

/// A walk's Full Disk Access page that reads on with a page after it
/// ([`Lane::hold_for_walk`]) keeps its done words up ([`Panel::waits`]): they name that page
/// and ask for Later to macOS's sheet (an update's successor's, which says no Later, to answer
/// macOS first), stand past [`DONE_FOR`] until the walk lets them go, closing done and held
/// for that page, and lapse [`PANEL_FOR`] after they came however long the page stood
/// before. Their close box declines nothing; a walk that loses the page after stops their
/// wait. NEGATIVE CONTROL: the walk's last page shows the done words alone and closes done
/// after DONE_FOR, as before.
#[test]
fn a_walks_full_disk_access_done_words_wait_for_its_next_page() {
    let shown = Instant::now();
    let now = shown + Duration::from_secs(9 * 60);
    let wake: Wake = Arc::new(|| {});
    let up = |cold: bool, next: Option<&'static str>| {
        let mut lane = Lane::windowed_with_fda_for_test(
            test_arms::opening(),
            test_arms::fda_opening(),
            test_arms::showing(),
        );
        if cold {
            lane.note_cold_start();
        }
        lane.hold_for_walk(next);
        let show = Show {
            origin: Origin::Walk,
            step: Some((1, 2)),
            ..fda_show(None)
        };
        assert!(lane.show(show, shown, Arc::clone(&wake)));
        lane.note_fda(FdaState::Granted, now);
        lane
    };
    let next = "Next: Developer Tools.";
    for cold in [true, false] {
        let mut lane = up(cold, Some("Developer Tools"));
        let words = lane.panel().expect("the page is up").content();
        assert!(
            has(&words, FDA_PANEL_DONE) && has(&words, next),
            "{words:?}"
        );
        assert_eq!(
            (
                has(&words, FDA_PANEL_NEXT_LATER),
                has(&words, FDA_PANEL_NEXT)
            ),
            (cold, !cold)
        );
        assert!(!has(&words, FDA_PANEL_LATER) && words.buttons().is_empty());
        assert_eq!(lane.waiting_done(Page::FullDiskAccess), Some(now));
        let panel = lane.panel.as_mut().expect("the page is up");
        for at in [
            now + DONE_FOR,
            shown + PANEL_FOR,
            now + PANEL_FOR - PANEL_TICK,
        ] {
            assert_eq!(panel.turn(at).close, None, "{:?}", at - now);
        }
        assert_eq!(panel.next_turn(), now + PANEL_FOR);
        assert_eq!(panel.turn(now + PANEL_FOR).close, Some(Closed::Lapsed));
        let close_box = panel.on(PanelEvent::CloseBox, now).close;
        assert_eq!(close_box, Some(Closed::Waiting));
        assert!(close_box.is_some_and(|why| !why.declines()));
        lane.let_done_go();
        assert!(lane.panel().is_none());
        assert_eq!(
            lane.take_closed(),
            Some((Page::FullDiskAccess, Closed::Done))
        );
    }
    let mut lane = up(true, Some("Developer Tools"));
    lane.hold_for_walk(None);
    assert_eq!(lane.waiting_done(Page::FullDiskAccess), None);
    assert!(!has(&lane.panel().expect("the page is up").content(), next));

    // The control: the walk's last page.
    let mut lane = up(true, None);
    let words = lane.panel().expect("the page is up").content();
    assert!(has(&words, FDA_PANEL_DONE) && !has(&words, next));
    assert!(!has(&words, FDA_PANEL_NEXT_LATER) && !has(&words, FDA_PANEL_NEXT));
    assert_eq!(lane.waiting_done(Page::FullDiskAccess), None);
    let panel = lane.panel.as_mut().expect("the page is up");
    assert_eq!(panel.turn(now + DONE_FOR).close, Some(Closed::Done));
}

/// The Later line stands on the Full Disk Access page of a process the owner started, whose
/// window reads a grant made now at once (S1). NEGATIVE CONTROL: an update's successor,
/// which can read it late, shows the page without it.
#[test]
fn only_a_process_the_owner_started_says_later_to_macos_sheet() {
    let now = Instant::now();
    let wake: Wake = Arc::new(|| {});
    let mut said = Vec::new();
    for cold in [true, false] {
        let mut lane = Lane::windowed_with_fda_for_test(
            test_arms::opening(),
            test_arms::fda_opening(),
            test_arms::showing(),
        );
        if cold {
            lane.note_cold_start();
        }
        assert!(lane.show(fda_show(None), now, Arc::clone(&wake)));
        let content = lane.panel().expect("the page is up").content();
        assert!(has(&content, FDA_PANEL_TRADE));
        said.push(has(&content, FDA_PANEL_LATER));
    }
    assert_eq!(said, [true, false]);
}

#[test]
fn a_full_disk_access_hand_over_goes_first_and_declines_only_its_own_page() {
    let now = Instant::now();
    let mut lane = Lane::windowed_with_fda_for_test(
        test_arms::opening(),
        test_arms::fda_opening(),
        test_arms::showing(),
    );
    lane.queue(Origin::Automatic, now);
    #[expect(clippy::disallowed_methods, reason = "the lane's own test")]
    lane.queue_fda(Origin::Automatic, now + Duration::from_secs(1));
    assert_eq!(lane.fda_queued(), Some(Origin::Automatic));
    let later = now + Duration::from_secs(30);
    assert!(
        !lane.hand_over_due(later),
        "Developer Tools waits behind it"
    );
    assert!(lane.fda_hand_over_due(later));
    assert_eq!(
        lane.next_deadline(later, || true),
        Some(now + Duration::from_secs(1)),
        "only the Full Disk Access look is armed"
    );
    assert_eq!(lane.take_queued(later, true), None);
    assert_eq!(lane.take_queued_fda(later, false), None, "not quiet");
    assert_eq!(lane.take_queued_fda(later, true), Some(Origin::Automatic));
    let wake: Wake = Arc::new(|| {});
    assert!(lane.show(fda_show(None), later, Arc::clone(&wake)));
    assert!(!lane.hand_over_due(later + RECHECK_EVERY * 10));
    let (_, _, declines_before) = test_arms::fda_counts();
    let devtools_declines = test_arms::decline_notes();
    lane.press_for_test(PanelEvent::NotNow);
    let asks = lane.tick_panel(later, None, &wake);
    assert_eq!(asks.page, Page::FullDiskAccess);
    assert!(lane.panel().is_none());
    assert_eq!(test_arms::fda_counts().2, declines_before + 1);
    assert_eq!(test_arms::decline_notes(), devtools_declines);
    assert_eq!(
        lane.take_fda_ended(),
        Some(FdaEnded {
            why: Closed::NotNow,
            dropped: false
        })
    );
    assert_eq!(lane.take_fda_ended(), None);
    assert!(
        lane.hand_over_due(later + RECHECK_EVERY),
        "Developer Tools' turn at its next look"
    );
}

/// A System State step is held only by a Not now under its own revision, written cold or
/// warm; the control verb also by any Not now inside its span; an owner's press by neither.
#[test]
fn a_not_now_holds_a_system_state_step_for_its_own_revision_only() {
    let key = "r4@com.aterm.aterm@UUID";
    let this = (key.to_owned(), 10);
    let cold = (atpkg::devtools::revision_key(4), 10);
    let earlier = (atpkg::devtools::revision_key(3), 10);
    let version = ("0.105.0".to_owned(), 10);
    for (origin, last, held, want) in [
        (Origin::Automatic, None, false, false),
        (Origin::Automatic, Some(&this), false, true),
        (Origin::Automatic, Some(&this), true, true),
        (Origin::Automatic, Some(&cold), false, true),
        (Origin::Automatic, Some(&earlier), true, false),
        (Origin::Automatic, Some(&earlier), false, false),
        (Origin::Automatic, Some(&version), true, false),
        (Origin::Automatic, Some(&version), false, false),
        (Origin::SystemStateAct, Some(&this), false, true),
        (Origin::SystemStateAct, Some(&cold), true, true),
        (Origin::SystemStateAct, Some(&earlier), true, false),
        (Origin::SystemStateAct, Some(&version), true, false),
        (Origin::Ctl, Some(&earlier), true, true),
        (Origin::Ctl, Some(&earlier), false, false),
        (Origin::Ctl, Some(&this), false, true),
        (Origin::Ctl, Some(&version), true, true),
        (Origin::Settings, Some(&this), true, false),
        (Origin::Digest, Some(&this), true, false),
    ] {
        assert_eq!(
            declined_for(origin, key, last, held),
            want,
            "{origin:?} after {last:?} (held {held})"
        );
    }
}

/// A Not now under an approved revision is handed back once as its decline; with no
/// revision, nothing is.
#[test]
fn a_not_now_is_handed_back_once_as_the_revisions_decline() {
    let now = Instant::now();
    let wake: Wake = Arc::new(|| {});
    for (revision, want) in [
        (Some(4), vec![(4, Access::FullDiskAccess)]),
        (None, Vec::new()),
    ] {
        let mut lane = Lane::windowed_with_fda_for_test(
            test_arms::opening(),
            test_arms::fda_opening(),
            test_arms::showing(),
        );
        lane.set_marks(Marks {
            devtools: "r4".to_owned(),
            fda: "r4@com.aterm.test@HERE".to_owned(),
            revision,
        });
        assert!(lane.show(fda_show(None), now, Arc::clone(&wake)));
        assert!(lane.take_declines().is_empty(), "nothing said yet");
        lane.press_for_test(PanelEvent::NotNow);
        let _ = lane.tick_panel(now, None, &wake);
        assert!(lane.panel().is_none());
        assert_eq!(lane.take_declines(), want, "revision {revision:?}");
        assert!(lane.take_declines().is_empty(), "handed back once");
    }
}

#[test]
fn a_grant_ends_the_full_disk_access_wait_and_panel() {
    let now = Instant::now();
    let mut lane = Lane::windowed_with_fda_for_test(
        test_arms::opening(),
        test_arms::fda_opening(),
        test_arms::showing(),
    );
    #[expect(clippy::disallowed_methods, reason = "the lane's own test")]
    lane.queue_fda(Origin::Automatic, now);
    lane.note_fda(FdaState::Denied, now);
    lane.note_fda(FdaState::Unknown, now);
    assert!(lane.fda_waiting());
    lane.note_fda(FdaState::Granted, now);
    assert!(!lane.fda_waiting());
    assert!(lane.show(fda_show(None), now, Arc::new(|| {})));
    lane.note_reading(
        test_arms::reading(DevToolGrant::Granted, test_arms::on()),
        now,
    );
    assert_eq!(lane.panel().map(Panel::phase), Some(Phase::Explain));
    lane.note_fda(FdaState::Granted, now);
    assert_eq!(lane.panel().map(Panel::phase), Some(Phase::Done));
}

#[test]
fn a_cold_start_settles_in_seconds_and_a_successor_waits_out_the_launch_settle() {
    let lane = Lane::windowed_for_test(Reach::inert(), DevToolsHelperArm::inert());
    let born = lane.born;
    assert!(!lane.settled(born + COLD_START_SETTLE));
    assert!(lane.settled(born + crate::consent_card::LAUNCH_SETTLE));
    let mut cold = Lane::windowed_for_test(Reach::inert(), DevToolsHelperArm::inert());
    cold.note_cold_start();
    let born = cold.born;
    assert!(!cold.settled(born));
    assert!(cold.settled(born + COLD_START_SETTLE));
    assert!(COLD_START_SETTLE < QUIET_FOR);
}

#[test]
fn the_origin_grant_and_decline_table() {
    let facts = |grant, list, declined, grant_stable, recent| Facts {
        grant,
        list,
        declined,
        grant_stable,
        recent,
        fda_on: true,
        shown_this_revision: false,
    };
    let presses = [Origin::Settings, Origin::Digest, Origin::Walk];
    let asked = [Origin::Automatic, Origin::Ctl];
    for declined in [false, true] {
        for grant_stable in [false, true] {
            for recent in [false, true] {
                for origin in presses {
                    let at = |grant, list| {
                        admit(
                            origin,
                            Moment::Asked,
                            &facts(grant, list, declined, grant_stable, recent),
                        )
                    };
                    assert_eq!(at(DevToolGrant::Granted, on()), Admit::PaneOnly);
                    assert_eq!(
                        at(DevToolGrant::NotGranted, absent(2)),
                        Admit::Hand {
                            list_terminal: false
                        }
                    );
                    assert_eq!(
                        at(DevToolGrant::NotGranted, absent(0)),
                        Admit::Hand {
                            list_terminal: true
                        },
                        "an empty list: Terminal first, so the page exists"
                    );
                    assert_eq!(
                        at(DevToolGrant::Unknown, unread()),
                        Admit::Hand {
                            list_terminal: false
                        },
                        "unread: the panel's UNREAD variant, no spctl"
                    );
                }
                for origin in asked {
                    for moment in [Moment::Asked, Moment::Quiet] {
                        let at = |grant, list| {
                            admit(
                                origin,
                                moment,
                                &facts(grant, list, declined, grant_stable, recent),
                            )
                        };
                        assert_eq!(
                            at(DevToolGrant::Granted, on()),
                            Admit::Refuse(Refusal::Granted)
                        );
                        assert_eq!(
                            at(DevToolGrant::Unknown, unread()),
                            Admit::Refuse(Refusal::Unknown),
                            "fail-closed"
                        );
                        let want = if declined {
                            Admit::Refuse(Refusal::Declined)
                        } else if !grant_stable {
                            Admit::Refuse(Refusal::Unstable)
                        } else if recent {
                            Admit::Refuse(Refusal::Recent)
                        } else if moment == Moment::Asked {
                            Admit::Queue
                        } else {
                            Admit::Hand {
                                list_terminal: false,
                            }
                        };
                        assert_eq!(
                            at(DevToolGrant::NotGranted, absent(2)),
                            want,
                            "{origin:?} {moment:?} declined={declined} \
                             stable={grant_stable} recent={recent}"
                        );
                    }
                }
            }
        }
    }
    for origin in [Origin::Automatic, Origin::Ctl] {
        for moment in [Moment::Asked, Moment::Quiet] {
            assert_eq!(
                admit(
                    origin,
                    moment,
                    &facts(DevToolGrant::NotGranted, on(), false, true, false)
                ),
                Admit::Refuse(Refusal::Listed),
                "{origin:?} {moment:?}"
            );
        }
    }
    assert_eq!(
        admit(
            Origin::Automatic,
            Moment::Quiet,
            &facts(DevToolGrant::NotGranted, absent(0), false, true, false)
        ),
        Admit::Hand {
            list_terminal: true
        }
    );
    // The page already up in this revision holds only the window's own step, judged as it
    // would open.
    let shown = Facts {
        shown_this_revision: true,
        ..facts(DevToolGrant::NotGranted, absent(2), false, true, false)
    };
    for moment in [Moment::Asked, Moment::Quiet] {
        assert_eq!(
            admit(Origin::Automatic, moment, &shown),
            Admit::Refuse(Refusal::Shown)
        );
    }
    assert_eq!(admit(Origin::Ctl, Moment::Asked, &shown), Admit::Queue);
    assert_eq!(
        admit(Origin::Settings, Moment::Asked, &shown),
        Admit::Hand {
            list_terminal: false
        }
    );
    // Without Full Disk Access the window's own waits (the list could not be read
    // back); a control verb's and an owner's press do not.
    let no_fda = Facts {
        fda_on: false,
        ..facts(DevToolGrant::NotGranted, absent(2), false, true, false)
    };
    for moment in [Moment::Asked, Moment::Quiet] {
        assert_eq!(
            admit(Origin::Automatic, moment, &no_fda),
            Admit::Refuse(Refusal::NoFullDiskAccess)
        );
    }
    assert_eq!(admit(Origin::Ctl, Moment::Asked, &no_fda), Admit::Queue);
    assert_eq!(
        admit(Origin::Settings, Moment::Asked, &no_fda),
        Admit::Hand {
            list_terminal: false
        }
    );
    assert_eq!(Origin::Digest.reopen_after_s(), None);
    assert_eq!(Origin::Settings.reopen_after_s(), None);
    assert_eq!(
        Origin::Automatic.reopen_after_s(),
        Some(atpkg::devtools::REOPEN_AFTER_S)
    );
    assert_eq!(
        Origin::Ctl.reopen_after_s(),
        Some(atpkg::devtools::REOPEN_AFTER_S)
    );
    assert_eq!(Origin::Walk.reopen_after_s(), None);
    assert!(
        Origin::Digest.owner_press()
            && Origin::Settings.owner_press()
            && Origin::Walk.owner_press()
    );
    assert!(!Origin::Automatic.owner_press() && !Origin::Ctl.owner_press());
}

#[test]
fn the_opened_marker_ages_in_whole_minutes_and_a_far_future_stamp_says_nothing() {
    let now = 1_791_381_501;
    assert_eq!(minutes_since(now, now), Some(0));
    assert_eq!(minutes_since(now - 59, now), Some(0));
    assert_eq!(minutes_since(now - 60, now), Some(1));
    assert_eq!(minutes_since(now - 3 * 86_400, now), Some(4_320));
    assert_eq!(
        minutes_since(now + 600, now),
        Some(0),
        "a clock a little behind"
    );
    assert_eq!(
        minutes_since(now + atpkg::devtools::STAMP_SKEW_S, now),
        Some(0)
    );
    assert_eq!(
        minutes_since(now + atpkg::devtools::STAMP_SKEW_S + 1, now),
        None,
        "written under a clock since corrected"
    );
}

fn show(list: DevToolList, bundle: Option<PathBuf>) -> Show {
    Show {
        page: Page::DeveloperTools,
        origin: Origin::Digest,
        reading: test_arms::reading(DevToolGrant::NotGranted, list),
        listed: None,
        bundle,
        skips: false,
        step: None,
    }
}

fn has(content: &Content, words: &str) -> bool {
    content.texts().any(|(text, _)| text == words)
}

#[test]
fn the_panels_words_in_each_state_and_variant() {
    use atpkg::devtools::{HELPER_HOW, HELPER_TITLE, done, only_aterm, trade, why};
    let t0 = Instant::now();
    let bundle = PathBuf::from("/Applications/aterm.app");
    let mut panel = Panel::new(show(absent(2), Some(bundle.clone())), t0);
    let explain = panel.content();
    let texts: Vec<&str> = explain.texts().map(|(t, _)| t).collect();
    assert_eq!(texts[0], HELPER_TITLE);
    assert_eq!(texts[1], why(false));
    assert!(texts[1].contains("should"), "D14 unmeasured");
    assert_eq!(texts[2], checked_words(Some(3)).unwrap());
    assert!(has(&explain, HELPER_HOW) && has(&explain, only_aterm(false)));
    assert!(has(&explain, trade(false)));
    assert!(!has(&explain, UNREAD_WORDS) && !has(&explain, DROP_FAILED_WORDS));
    assert_eq!(
        explain.tile(),
        Some(&Tile {
            bundle: bundle.clone(),
            live: true,
            step: atpkg::devtools::HELPER_STEP,
        })
    );
    assert_eq!(
        explain.buttons(),
        [Button::OpenPage(Page::DeveloperTools), Button::NotNow],
        "no Done before a grant"
    );
    assert_eq!(explain.blocks.last(), Some(&Block::Link(STATUS_LINK)));
    assert!(atpkg::devtools::HELPER_STEP.contains("Developer Tools list"));
    assert!(DROP_FAILED_WORDS.contains("Developer Tools list"));
    let words_only = Panel::new(show(absent(2), None), t0).content();
    assert_eq!(words_only.tile(), None);
    assert!(has(&words_only, atpkg::devtools::HELPER_HOW_PLUS));
    assert!(!has(&words_only, HELPER_HOW));
    assert!(
        words_only
            .texts()
            .all(|(text, _)| !text.to_lowercase().contains("drag")),
        "{words_only:?}"
    );
    let off = test_arms::list(DevToolListing::Off, DevToolListLabel::Read, Some(2));
    let mut listed_off = Panel::new(show(off, Some(bundle.clone())), t0);
    let words = listed_off.content();
    assert!(has(&words, atpkg::devtools::HELPER_SWITCH_ON) && !has(&words, HELPER_HOW));
    assert_eq!(words.tile(), None, "aterm is already listed");
    assert!(listed_off.on_read(absent(2), t0).redraw, "it left the list");
    assert!(has(&listed_off.content(), HELPER_HOW));
    assert!(listed_off.content().tile().is_some());
    let mut listed = show(absent(0), None);
    listed.listed = Some(Ok(()));
    assert!(has(&Panel::new(listed, t0).content(), only_aterm(true)));
    let unread_panel = Panel::new(show(unread(), None), t0);
    assert!(!unread_panel.readable());
    let words = unread_panel.content();
    assert!(has(&words, UNREAD_WORDS));
    let probe_off = Panel::new(
        show(
            DevToolList::refused(DevToolListLabel::RefusedDisabled),
            None,
        ),
        t0,
    );
    assert!(!probe_off.readable());
    let off_words = probe_off.content();
    assert!(!has(&off_words, UNREAD_WORDS));
    assert!(off_words.texts().any(|(text, tone)| tone == Tone::Notice
        && text.contains(atpkg::devtools::list_reason(
            DevToolListLabel::RefusedDisabled
        ))
        && !text.contains("Full Disk Access")));
    assert_eq!(off_words.buttons(), words.buttons());
    let pending = Panel::new(show(DevToolList::pending(), None), t0);
    assert!(
        pending.readable()
            && pending.content().buttons()
                == [Button::OpenPage(Page::DeveloperTools), Button::NotNow]
    );
    assert_eq!(
        words.buttons(),
        [Button::ShowList, Button::SwitchedOn, Button::NotNow]
    );
    assert_eq!(
        words
            .buttons()
            .iter()
            .map(|b| b.label())
            .collect::<Vec<_>>(),
        [
            "No Developer Tools list? Show it",
            "I've switched it on",
            "Not now"
        ]
    );
    assert!(
        panel
            .on(PanelEvent::DragEnded { accepted: false }, t0)
            .redraw
    );
    assert_eq!(panel.phase(), Phase::DropFailed);
    assert!(has(&panel.content(), DROP_FAILED_WORDS));
    assert!(panel.content().tile().is_some_and(|t| t.live));
    let _ = panel.on(PanelEvent::DragEnded { accepted: true }, t0);
    assert_eq!(panel.phase(), Phase::Explain);
    assert!(panel.content().tile().is_some_and(|t| !t.live), "one drop");
    let spent = panel.content();
    assert_eq!(spent.tile().map(Tile::hint), Some(TILE_SPENT_HINT));
    assert_eq!(
        explain.tile().map(Tile::hint),
        Some(atpkg::devtools::HELPER_STEP)
    );
    assert!(!has(&spent, HELPER_HOW) && has(&spent, &dropped_how()));
    assert!(!dropped_how().to_lowercase().contains("drag"));
    assert_eq!(
        panel.on(PanelEvent::DragEnded { accepted: true }, t0),
        Effect::default(),
        "a second drag changes nothing"
    );
    assert!(
        !panel
            .on_read(absent(2), t0 + DROP_CONFIRM_WITHIN / 2)
            .redraw
    );
    assert_eq!(panel.turn(t0 + DROP_CONFIRM_WITHIN * 2), Effect::default());
    assert_eq!(panel.phase(), Phase::Explain, "time alone judges nothing");
    assert!(panel.on_read(absent(2), t0 + DROP_CONFIRM_WITHIN).redraw);
    assert_eq!(panel.phase(), Phase::DroppedUnconfirmed);
    let unconfirmed = panel.content();
    assert!(has(&unconfirmed, DROP_UNCONFIRMED_WORDS));
    assert!(!has(&unconfirmed, HELPER_HOW) && has(&unconfirmed, &dropped_how()));
    let mut landed_off = Panel::new(show(absent(2), Some(bundle.clone())), t0);
    let _ = landed_off.on(PanelEvent::DragEnded { accepted: true }, t0);
    let _ = landed_off.on_read(off, t0 + DROP_CONFIRM_WITHIN * 2);
    let words = landed_off.content();
    assert_eq!(landed_off.phase(), Phase::Explain);
    assert!(has(&words, atpkg::devtools::HELPER_SWITCH_ON));
    assert!(!has(&words, DROP_UNCONFIRMED_WORDS));
    let mut late = panel.clone();
    let _ = late.on_read(off, t0 + DROP_CONFIRM_WITHIN * 3);
    assert_eq!(late.phase(), Phase::Explain);
    assert!(!has(&late.content(), DROP_UNCONFIRMED_WORDS));
    let effect = panel.on_read(on(), t0 + DROP_CONFIRM_WITHIN);
    assert!(effect.granted && effect.redraw);
    assert_eq!(panel.phase(), Phase::Done);
    let done_words = panel.content();
    assert_eq!(
        done_words.texts().collect::<Vec<_>>(),
        [(HELPER_TITLE, Tone::Title), (done(false), Tone::Done)]
    );
    assert!(done_words.buttons().is_empty());
}

#[test]
fn a_failed_spctl_run_says_why_and_offers_show_it_again() {
    use atpkg::devtools::only_aterm;
    let t0 = Instant::now();
    let mut failed = show(absent(0), None);
    failed.listed = Some(Err("spctl exited 1".to_string()));
    let mut panel = Panel::new(failed, t0);
    assert!(
        panel.readable(),
        "the list was read: not the UNREAD variant"
    );
    let words = panel.content();
    assert!(
        has(&words, &show_list_failed_words("spctl exited 1")),
        "{words:?}"
    );
    assert_eq!(words.buttons(), [Button::ShowList, Button::NotNow]);
    assert!(has(&words, only_aterm(false)), "nothing was listed");
    let asked = panel.on(Button::ShowList.event(), t0);
    assert!(asked.list_terminal && asked.redraw);
    assert!(!has(
        &panel.content(),
        &show_list_failed_words("spctl exited 1")
    ));
    let open = [Button::OpenPage(Page::DeveloperTools), Button::NotNow];
    let _ = panel.on_listed(&Ok(()));
    let listed = panel.content();
    assert_eq!(listed.buttons(), open);
    assert!(has(&listed, only_aterm(true)));
    let unrun = Panel::new(show(absent(0), None), t0).content();
    assert_eq!(unrun.buttons(), [Button::ShowList, Button::NotNow]);
    let mut ran = show(absent(0), None);
    ran.listed = Some(Ok(()));
    assert_eq!(Panel::new(ran, t0).content().buttons(), open);
    assert_eq!(
        Panel::new(show(absent(2), None), t0).content().buttons(),
        open
    );
    let mut emptied = Panel::new(show(absent(0), None), t0);
    assert!(emptied.on_read(absent(1), t0 + PANEL_TICK).redraw);
    assert_eq!(emptied.content().buttons(), open);
}

/// The words after a failed Show it name Show it, which runs the same `spctl` again, and no
/// command for the person to type; they stand only while Show it does. A list read with a
/// row since (the person ran `spctl` in a tab after all, or added aterm) takes Show it away,
/// and the words with it.
/// NEGATIVE CONTROL: words kept until a Show it press (the panel before) still say "press
/// Show it" beside Open Developer Tools and Not now.
#[test]
fn the_failed_show_it_words_never_name_a_button_that_is_gone() {
    let t0 = Instant::now();
    let mut failed = show(absent(0), None);
    failed.listed = Some(Err("spctl exited 1".to_string()));
    let mut panel = Panel::new(failed, t0);
    let why = show_list_failed_words("spctl exited 1");
    assert!(why.contains("Show it") && why.contains(atpkg::devtools::PANE));
    assert!(!why.contains(atpkg::devtools::LIST_TERMINAL), "{why}");
    assert!(has(&panel.content(), &why));
    assert!(panel.on_read(absent(1), t0 + PANEL_TICK).redraw);
    let read = panel.content();
    assert_eq!(
        read.buttons(),
        [Button::OpenPage(Page::DeveloperTools), Button::NotNow]
    );
    assert!(!has(&read, &why), "{read:?}");
}

/// A panel that gives way to a person's press elsewhere leaves nothing of its walk queued to
/// open over what the press raises: the walk's queued hand-overs go. The close is a
/// give-way, no Not now. Another origin's queued ask stays.
/// NEGATIVE CONTROL: a give-way that leaves the lane's queue alone keeps the walk's queued
/// Full Disk Access page, which opens at the next moment.
#[test]
fn a_panel_that_gives_way_leaves_nothing_of_its_walk_to_open() {
    let mut lane =
        Lane::windowed_with_fda_for_test(test_reach(), test_arms::fda_opening(), test_arm());
    let wake = nudge();
    let t0 = Instant::now();
    #[expect(clippy::disallowed_methods, reason = "the lane's own test")]
    lane.queue_fda(Origin::Walk, t0);
    let mut page = show(test_arms::absent(2), None);
    page.origin = Origin::Walk;
    page.step = Some((1, 2));
    assert!(lane.show(page, t0, Arc::clone(&wake)));
    lane.give_way();
    assert!(lane.panel().is_none());
    assert_eq!(
        lane.take_closed(),
        Some((Page::DeveloperTools, Closed::GaveWay))
    );
    assert!(lane.take_declines().is_empty(), "no Not now");
    assert_eq!(lane.fda_queued(), None, "the walk's queued page went");

    let mut other = Lane::windowed_for_test(test_reach(), test_arm());
    other.queue(Origin::Ctl, t0);
    other.give_way();
    assert_eq!(other.queued(), Some(Origin::Ctl), "another's ask stays");
}

/// An Open pressed while System Settings has a sheet or dialog up (macOS's Quit & Reopen
/// sheet over the Full Disk Access page, where the link went untaken) waits: the panel says
/// why, asks nothing of the window while the sheet stands, and asks the page once it is
/// gone, once.
/// NEGATIVE CONTROL: an Open passed straight to the window asks the page under the sheet
/// (the first tick's `open_page`), where System Settings leaves it untaken.
#[test]
fn an_open_under_a_sheet_waits_for_it() {
    use std::sync::atomic::{AtomicBool, Ordering};
    static BUSY: AtomicBool = AtomicBool::new(true);
    let arm = DevToolsHelperArm {
        settings_busy: || BUSY.load(Ordering::SeqCst),
        ..test_arm()
    };
    let mut lane = Lane::windowed_for_test(test_reach(), arm);
    let wake = nudge();
    let t0 = Instant::now();
    assert!(lane.show(show(test_arms::absent(2), None), t0, Arc::clone(&wake)));
    let _ = lane.tick_panel(t0, None, &wake);
    events_of(&lane).post(PanelEvent::OpenPage);
    let asks = lane.tick_panel(t0 + PANEL_TICK, None, &wake);
    assert!(!asks.open_page, "not under the sheet");
    let waits = lane.panel().map(Panel::content).expect("the panel stays");
    assert!(has(&waits, OPEN_WAITS_WORDS), "{waits:?}");
    assert!(
        !lane.tick_panel(t0 + PANEL_TICK * 2, None, &wake).open_page,
        "still under it"
    );
    BUSY.store(false, Ordering::SeqCst);
    assert!(
        lane.tick_panel(t0 + PANEL_TICK * 3, None, &wake).open_page,
        "the sheet is gone"
    );
    let clear = lane.panel().map(Panel::content).expect("the panel stays");
    assert!(!has(&clear, OPEN_WAITS_WORDS));
    assert!(
        !lane.tick_panel(t0 + PANEL_TICK * 4, None, &wake).open_page,
        "once"
    );
}

/// Every page the switch is still to flip on offers a way back to its page: Show it where
/// the list may be missing (unread, or read whole and empty with nothing listed yet), and
/// otherwise its page's Open, whose press asks the page again and closes nothing. The panel
/// e2e145 met (Golden Gate, a list read once `spctl` listed Terminal, System Settings left on
/// another page) now has Open Developer Tools beside Not now.
/// NEGATIVE CONTROL: the panel before it, whose read list left Not now alone, has no button
/// that brings a page up, and fails every Developer Tools case but the two with Show it.
#[test]
fn every_page_offers_a_way_back_to_its_page() {
    let t0 = Instant::now();
    let bundle = PathBuf::from("/Applications/aterm.app");
    let mut ran = show(absent(1), None);
    ran.listed = Some(Ok(()));
    let mut e2e145 = Panel::new(ran, t0);
    assert_eq!(
        e2e145.content().buttons(),
        [Button::OpenPage(Page::DeveloperTools), Button::NotNow]
    );
    assert_eq!(
        Button::OpenPage(Page::DeveloperTools).label(),
        "Open Developer Tools"
    );
    let asked = e2e145.on(Button::OpenPage(Page::DeveloperTools).event(), t0);
    assert!(asked.open_page && asked.close.is_none(), "{asked:?}");
    assert_eq!(e2e145.phase(), Phase::Explain, "the panel stays");

    let off = test_arms::list(DevToolListing::Off, DevToolListLabel::Read, Some(2));
    let unread_list = unread();
    let pending = DevToolList::pending();
    let mut pages = vec![
        Panel::new(fda_show(Some(bundle.clone())), t0),
        Panel::new(fda_show(None), t0),
    ];
    for list in [absent(2), absent(0), off, unread_list, pending] {
        pages.push(Panel::new(show(list, Some(bundle.clone())), t0));
    }
    pages.push(Panel::new(
        Show {
            page: Page::Accessibility,
            ..fda_show(Some(bundle.clone()))
        },
        t0,
    ));
    for panel in pages {
        let buttons = panel.content().buttons().to_vec();
        let opens = buttons.iter().any(|button| match button {
            Button::OpenPage(page) => *page == panel.page(),
            Button::ShowList => panel.page() == Page::DeveloperTools,
            _ => false,
        });
        assert!(opens, "{:?}: {buttons:?}", panel.page());
    }
}

/// A walk page counts from its creation as a look that found the person there: a delayed
/// first look that finds them away cannot spend its lifetime. Ordinary uncounted panels
/// still lapse normally, and a walk with observed present time still spends that time.
#[test]
fn a_walk_pages_first_absent_count_spends_no_unobserved_time() {
    let t0 = Instant::now();
    let walk = || Show {
        origin: Origin::Walk,
        step: Some((1, 2)),
        ..fda_show(None)
    };
    let mut ordinary = Panel::new(fda_show(None), t0);
    assert_eq!(
        ordinary.turn(t0 + PANEL_FOR).close,
        Some(Closed::Lapsed),
        "an ordinary uncounted panel keeps its lifetime"
    );

    let mut present = Panel::new(walk(), t0);
    present.count_time(t0, true);
    present.count_time(t0 + PANEL_FOR - Duration::from_secs(1), true);
    assert_eq!(
        present.turn(t0 + PANEL_FOR - Duration::from_secs(1)).close,
        None
    );
    present.count_time(t0 + PANEL_FOR, true);
    assert_eq!(
        present.turn(t0 + PANEL_FOR).close,
        Some(Closed::Lapsed),
        "a full lifetime with observed presence still lapses"
    );

    for delay in [PANEL_FOR, PANEL_FOR * 3] {
        let mut page = Panel::new(walk(), t0);
        let absent = t0 + delay;
        page.count_time(absent, false);
        assert_eq!(
            page.turn(absent).close,
            None,
            "a first absent count after {delay:?} cannot spend unobserved time"
        );
        let back = absent + PANEL_FOR;
        page.count_time(back, true);
        assert_eq!(page.turn(back).close, None, "the person just came back");
        page.count_time(back + PANEL_FOR - Duration::from_secs(1), true);
        assert_eq!(
            page.turn(back + PANEL_FOR - Duration::from_secs(1)).close,
            None
        );
        page.count_time(back + PANEL_FOR, true);
        assert_eq!(
            page.turn(back + PANEL_FOR).close,
            Some(Closed::Lapsed),
            "the person's subsequent full lifetime still counts"
        );
    }
}

/// An uncounted stretch moves done words that wait for the walk's next page only by its part
/// after they came: a grant read at the look that found the person back after an hour away
/// leaves them starting at that look, and a stretch the person spends away after them moves
/// them by its whole length.
#[test]
fn done_words_move_only_by_the_uncounted_time_after_they_came() {
    let t0 = Instant::now();
    let mut page = Panel::new(
        Show {
            origin: Origin::Walk,
            step: Some((1, 2)),
            ..fda_show(None)
        },
        t0,
    );
    page.waits = Some("Developer Tools");
    let away = t0 + Duration::from_secs(1);
    page.count_time(away, false);
    let back = away + Duration::from_secs(60 * 60);
    let _ = page.granted(back);
    page.count_time(back, true);
    assert_eq!(page.done_at, Some(back), "the hour away came before them");
    page.count_time(back + DONE_FOR, true);
    assert_eq!(page.done_at, Some(back), "counted time moves nothing");
    let gone = back + DONE_FOR + Duration::from_secs(1);
    page.count_time(gone, false);
    page.count_time(gone + Duration::from_secs(30), true);
    assert_eq!(
        page.done_at,
        Some(back + Duration::from_secs(31)),
        "the stretch from the last present look to the return, all after them"
    );
}

#[test]
fn the_panels_state_machine() {
    let t0 = Instant::now();
    let mut panel = Panel::new(show(absent(2), None), t0);
    let not_now = panel.clone().on(Button::NotNow.event(), t0);
    assert_eq!(not_now.close, Some(Closed::NotNow));
    assert!(Closed::NotNow.declines());
    for closed in [
        Closed::SwitchedOn,
        Closed::Done,
        Closed::Lapsed,
        Closed::SettingsGone,
        Closed::Replaced,
    ] {
        assert!(!closed.declines(), "{closed:?}");
    }
    for button in Button::ALL {
        assert_eq!(Button::from_tag(button.tag()), Some(button));
    }
    assert_eq!(Button::from_tag(-1), None, "the status link's tag");
    assert_eq!(Button::from_tag(Button::ALL.len() as isize), None);
    let switched = panel.clone().on(PanelEvent::SwitchedOn, t0);
    assert_eq!(switched.close, Some(Closed::SwitchedOn));
    assert!(switched.recheck);
    assert!(panel.clone().on(PanelEvent::Status, t0).open_status);
    assert!(!panel.wants_read(t0), "the show's own reading is fresh");
    assert!(panel.wants_read(t0 + PANEL_TICK));
    panel.read_out = true;
    assert!(!panel.wants_read(t0 + PANEL_TICK), "one at a time");
    let _ = panel.on_read(absent(2), t0 + PANEL_TICK);
    assert!(!panel.wants_read(t0 + PANEL_TICK));
    let _ = panel.on(PanelEvent::DragEnded { accepted: true }, t0 + PANEL_TICK);
    assert!(panel.wants_read(t0 + PANEL_TICK), "a drop reads at once");
    for transient in [
        DevToolListLabel::TimedOut,
        DevToolListLabel::ToolFailed,
        DevToolListLabel::SpawnFailed,
        DevToolListLabel::OutputUnreadable,
        DevToolListLabel::OpenFailed,
        DevToolListLabel::Pending,
    ] {
        let mut tried = panel.clone();
        tried.read_out = true;
        let effect = tried.on_read(DevToolList::refused(transient), t0 + PANEL_TICK * 2);
        assert!(!effect.redraw && tried.readable(), "{transient:?}");
        assert!(tried.wants_read(t0 + PANEL_TICK * 3), "{transient:?}");
    }
    let failed = DevToolList::refused(DevToolListLabel::ToolFailed);
    let mut failing = panel.clone();
    for n in 1..READ_FAILURES_LASTING {
        let _ = failing.on_read(failed, t0 + PANEL_TICK * (1 + n));
    }
    let _ = failing.on_read(absent(2), t0 + PANEL_TICK * 9);
    for n in 1..READ_FAILURES_LASTING {
        let _ = failing.on_read(failed, t0 + PANEL_TICK * (9 + n));
        assert!(failing.readable(), "{n} failed read(s) in a row");
    }
    let effect = failing.on_read(failed, t0 + PANEL_TICK * 20);
    assert!(effect.redraw && !failing.readable());
    assert!(!failing.wants_read(t0 + PANEL_TICK * 30), "no more reads");
    assert!(
        failing
            .content()
            .texts()
            .any(|(text, tone)| tone == Tone::Notice
                && text.contains(atpkg::devtools::list_reason(DevToolListLabel::ToolFailed)))
    );
    let effect = panel.on_read(unread(), t0 + PANEL_TICK * 2);
    assert!(effect.redraw && !panel.readable());
    assert!(!panel.wants_read(t0 + PANEL_TICK * 9));
    assert_eq!(
        panel.turn(t0 + DROP_CONFIRM_WITHIN * 2),
        Effect::default(),
        "an unread list confirms no drop"
    );
    let asked = panel.on(PanelEvent::ShowList, t0);
    assert!(asked.list_terminal);
    let _ = panel.on_listed(&Err("spctl exited 1".to_string()));
    assert!(
        panel
            .content()
            .texts()
            .any(|(t, tone)| tone == Tone::Notice && t.contains("press + below the list"))
    );
    let _ = panel.on_listed(&Ok(()));
    assert!(has(&panel.content(), atpkg::devtools::only_aterm(true)));
    let t1 = t0 + Duration::from_secs(30);
    assert!(panel.granted(t1).granted);
    assert_eq!(panel.granted(t1), Effect::default(), "once");
    assert_eq!(panel.next_turn(), t1 + DONE_FOR);
    assert_eq!(panel.turn(t1), Effect::default());
    assert_eq!(panel.turn(t1 + DONE_FOR).close, Some(Closed::Done));
    assert_eq!(
        panel.clone().on(PanelEvent::NotNow, t1).close,
        Some(Closed::Done),
        "closing the done words is no decline"
    );
    let mut idle = Panel::new(show(absent(2), None), t0);
    assert_eq!(idle.next_turn(), t0 + PANEL_FOR);
    assert_eq!(idle.turn(t0 + PANEL_FOR).close, Some(Closed::Lapsed));
}

const ROW_CHANGED_AT: i64 = 1_791_000_000;

fn on_since(changed_at: i64) -> DevToolList {
    DevToolList {
        modified_at_s: Some(changed_at),
        ..on()
    }
}

fn denied_after_the_row() -> DevToolProbe {
    DevToolProbe {
        state: aterm_containment::DevToolState::Denied,
        requests: Some(1),
        not_allowed: Some(1),
        answered_at_ms: Some(ROW_CHANGED_AT * 1_000 + 60_000),
        ..DevToolProbe::refused(aterm_containment::DevToolProbeLabel::Observed)
    }
}

fn joined(list: DevToolList) -> Reading {
    let log = denied_after_the_row();
    Reading {
        grant: devtool_grant(&list, &log),
        list,
        log,
        subject: Some(test_arms::SUBJECT),
    }
}

#[test]
fn a_row_already_on_while_macos_still_checks_is_never_done_until_it_changes() {
    use atpkg::devtools::{HELPER_HOW, HELPER_LISTED_ON};
    let t0 = Instant::now();
    let reading = joined(on_since(ROW_CHANGED_AT));
    assert_eq!(reading.grant, DevToolGrant::NotGranted, "ListedOn");
    let bundle = Some(PathBuf::from("/Applications/aterm.app"));
    let mut panel = Panel::new(
        Show {
            page: Page::DeveloperTools,
            origin: Origin::Digest,
            reading,
            listed: None,
            bundle: bundle.clone(),
            skips: false,
            step: None,
        },
        t0,
    );
    let words = panel.content();
    assert!(has(&words, HELPER_LISTED_ON) && !has(&words, HELPER_HOW));
    assert_eq!(words.tile(), None, "aterm is already listed");
    assert!(words.texts().all(|(text, _)| !text.contains("Drag")));
    let again = panel.on_read(on_since(ROW_CHANGED_AT), t0 + PANEL_TICK);
    assert!(!again.granted && !again.redraw);
    assert_eq!(panel.phase(), Phase::Explain);
    let toggled = on_since(ROW_CHANGED_AT + 3_600);
    let effect = panel.on_read(toggled, t0 + PANEL_TICK * 2);
    assert!(effect.granted && panel.phase() == Phase::Done);
    let mut held = Panel::new(
        Show {
            page: Page::DeveloperTools,
            origin: Origin::Digest,
            reading,
            listed: None,
            bundle: bundle.clone(),
            skips: false,
            step: None,
        },
        t0,
    );
    let newer = DevToolProbe {
        answered_at_ms: Some((ROW_CHANGED_AT + 7_200) * 1_000),
        ..denied_after_the_row()
    };
    let _ = held.note_reading(
        &Reading {
            log: newer,
            ..reading
        },
        t0,
    );
    assert!(!held.on_read(toggled, t0 + PANEL_TICK).granted);
    assert!(held.on_read(absent(2), t0 + PANEL_TICK * 2).redraw);
    assert!(has(&held.content(), HELPER_HOW) && held.content().tile().is_some());
    let mut adding = Panel::new(
        Show {
            page: Page::DeveloperTools,
            origin: Origin::Digest,
            reading: joined(absent(2)),
            listed: None,
            bundle,
            skips: false,
            step: None,
        },
        t0,
    );
    assert!(has(&adding.content(), HELPER_HOW));
    let added = on_since(ROW_CHANGED_AT + 600);
    assert!(adding.on_read(added, t0 + PANEL_TICK).granted);
    assert_eq!(adding.phase(), Phase::Done);
}

#[test]
fn the_panels_read_of_an_unchanged_row_on_grants_nothing() {
    let t0 = Instant::now();
    let reach = Reach {
        read_list: |_, _| on_since(ROW_CHANGED_AT),
        ..test_arms::opening()
    };
    let mut lane = Lane::windowed_for_test(reach, test_arm());
    let wake = nudge();
    let reading = joined(on_since(ROW_CHANGED_AT));
    lane.note_reading(reading, t0);
    lane.queue(Origin::Automatic, t0);
    assert!(lane.show(
        Show {
            page: Page::DeveloperTools,
            origin: Origin::Digest,
            reading,
            listed: None,
            bundle: None,
            skips: false,
            step: None,
        },
        t0,
        Arc::clone(&wake),
    ));
    let t1 = t0 + PANEL_TICK;
    let _ = lane.tick_panel(t1, Some(ProbeGate::on()), &wake);
    assert!(lane.panel().is_some_and(|p| p.read_out), "one read out");
    let mut asks = Asks::default();
    until(|| {
        asks = lane.tick_panel(t1, Some(ProbeGate::on()), &wake);
        lane.panel().is_some_and(|p| !p.read_out)
    });
    assert!(!asks.granted, "nothing ends the digest line");
    assert_eq!(lane.panel().map(Panel::phase), Some(Phase::Explain));
    assert_eq!(
        lane.reading().map(|r| r.grant),
        Some(DevToolGrant::NotGranted)
    );
    assert_eq!(lane.queued(), Some(Origin::Automatic), "still waiting");
    assert_eq!(
        reading.with_list(on_since(ROW_CHANGED_AT + 600)).grant,
        DevToolGrant::Granted
    );
    assert_eq!(
        Reading::UNREAD.with_list(on_since(ROW_CHANGED_AT)).grant,
        DevToolGrant::Granted
    );
}

#[test]
fn a_panel_shown_before_the_first_reading_learns_its_subject_from_it() {
    use atpkg::devtools::{HELPER_HOW, HELPER_HOW_PLUS};
    for (offered, how) in [(true, HELPER_HOW), (false, HELPER_HOW_PLUS)] {
        let reach = if offered {
            Reach {
                drag_bundle: |subject| subject.map(|_| PathBuf::from("/Applications/aterm.app")),
                ..test_arms::opening()
            }
        } else {
            test_arms::opening()
        };
        let mut lane = Lane::windowed_for_test(reach, test_arm());
        let wake = nudge();
        let t0 = Instant::now();
        assert!(lane.show(
            Show {
                page: Page::DeveloperTools,
                origin: Origin::Settings,
                reading: Reading::UNREAD,
                listed: None,
                bundle: None,
                skips: false,
                step: None,
            },
            t0,
            Arc::clone(&wake),
        ));
        let words = lane.panel().map(Panel::content).expect("shown");
        assert_eq!(words.tile(), None);
        assert!(has(&words, HELPER_HOW_PLUS), "no icon: + leads");
        let _ = lane.tick_panel(t0 + PANEL_TICK, Some(ProbeGate::on()), &wake);
        assert!(
            lane.panel().is_some_and(|p| !p.read_out),
            "no subject: no read"
        );
        lane.note_reading(
            test_arms::reading(DevToolGrant::NotGranted, absent(2)),
            t0 + PANEL_TICK,
        );
        let panel = lane.panel().expect("still up");
        assert!(!panel.wants_subject());
        let words = panel.content();
        assert_eq!(words.tile().is_some(), offered, "offered={offered}");
        assert!(has(&words, how), "offered={offered}: {words:?}");
        let _ = lane.tick_panel(t0 + PANEL_TICK * 2, Some(ProbeGate::on()), &wake);
        assert!(
            lane.panel().is_some_and(|p| p.read_out),
            "the subject is known: the list is read"
        );
    }
}

struct Fixed;

impl Measure for Fixed {
    fn text_height(&self, text: &str, _tone: Tone, width: f64) -> f64 {
        let per_line = (width / 6.0).floor().max(1.0);
        let lines = (text.chars().count() as f64 / per_line).ceil().max(1.0);
        lines * 16.0
    }

    fn button_width(&self, title: &str) -> f64 {
        title.chars().count() as f64 * 7.0 + 24.0
    }
}

#[test]
fn the_layout_stacks_the_words_and_wraps_the_buttons() {
    let t0 = Instant::now();
    let bundle = Some(PathBuf::from("/Applications/aterm.app"));
    for (content, one_row) in [
        (
            Panel::new(show(absent(2), bundle.clone()), t0).content(),
            true,
        ),
        (Panel::new(show(unread(), bundle), t0).content(), false),
    ] {
        let layout = layout(&content, &Fixed);
        assert_eq!(layout.w, PANEL_W);
        let frame = Rect {
            x: 0.0,
            y: 0.0,
            w: layout.w,
            h: layout.h,
        };
        for (i, (part, a)) in layout.parts.iter().enumerate() {
            assert!(a.w > 0.0 && a.h > 0.0, "{part:?}");
            assert!(
                a.x >= INSET - 1e-9 && a.max_x() <= PANEL_W - INSET + 1e-9,
                "{part:?} {a:?}"
            );
            assert!(frame.holds(*a), "{part:?}");
            for (other, b) in &layout.parts[i + 1..] {
                assert_eq!(a.overlap(*b), 0.0, "{part:?} overlaps {other:?}");
            }
        }
        let bottom = layout
            .parts
            .iter()
            .map(|(_, r)| r.max_y())
            .fold(0.0, f64::max);
        assert_eq!(layout.h, bottom + INSET);
        let at = |want: Part| {
            layout
                .parts
                .iter()
                .find(|(part, _)| *part == want)
                .map(|(_, r)| *r)
                .unwrap()
        };
        let icon = at(Part::Icon);
        assert_eq!((icon.w, icon.h), (ICON, ICON));
        for beside in [Part::Caption, Part::Hint, Part::Path] {
            assert!(at(beside).x > icon.max_x(), "{beside:?} right of the icon");
        }
        let texts: Vec<Rect> = layout
            .parts
            .iter()
            .filter(|(part, _)| matches!(part, Part::Text(_)))
            .map(|(_, r)| *r)
            .collect();
        assert!(texts.windows(2).all(|w| w[0].max_y() <= w[1].y));
        let buttons: Vec<Rect> = layout
            .parts
            .iter()
            .filter(|(part, _)| matches!(part, Part::Button(_)))
            .map(|(_, r)| *r)
            .collect();
        assert_eq!(buttons.len(), content.buttons().len());
        assert!(
            buttons
                .iter()
                .any(|b| (b.max_x() - (PANEL_W - INSET)).abs() < 1e-9),
            "right-aligned"
        );
        let rows: std::collections::BTreeSet<u64> = buttons.iter().map(|b| b.y.to_bits()).collect();
        let inner = PANEL_W - 2.0 * INSET;
        let across = content
            .buttons()
            .iter()
            .map(|button| Fixed.button_width(button.label()).min(inner))
            .sum::<f64>()
            + SPACING * (content.buttons().len() as f64 - 1.0);
        assert_eq!(across <= inner, one_row, "{:?}", content.buttons());
        if one_row {
            assert_eq!(rows.len(), 1, "buttons that fit share a row");
        } else {
            assert!(rows.len() > 1, "three wide buttons wrap at 360 pt");
        }
    }
}

#[test]
fn the_panel_goes_beside_system_settings_and_never_over_its_list() {
    let (w, h) = (PANEL_W, 400.0);
    let main = Rect {
        x: 0.0,
        y: 0.0,
        w: 1728.0,
        h: 1080.0,
    };
    let inside = |r: Rect, screen: Rect| screen.holds(r);
    let s = Rect {
        x: 200.0,
        y: 300.0,
        w: 800.0,
        h: 600.0,
    };
    let r = place(w, h, Some(s), &[main]);
    assert_eq!(r.x, s.max_x() + PLACE_GAP);
    assert_eq!(r.max_y(), s.max_y(), "tops aligned");
    assert!(inside(r, main));
    let s = Rect { x: 500.0, ..s };
    let s = Rect {
        w: main.max_x() - 500.0,
        ..s
    };
    let r = place(w, h, Some(s), &[main]);
    assert_eq!(r.max_x(), s.x - PLACE_GAP);
    assert!(inside(r, main));
    let s = Rect {
        x: 0.0,
        y: 100.0,
        w: main.w,
        h: 400.0,
    };
    let r = place(w, h, Some(s), &[main]);
    assert_eq!((r.x, r.y), (s.x, s.max_y() + PLACE_GAP));
    assert!(inside(r, main));
    let s = Rect { y: 500.0, ..s };
    let r = place(w, h, Some(s), &[main]);
    assert_eq!(r.max_y(), s.y - PLACE_GAP);
    let s = Rect {
        x: 200.0,
        y: 0.0,
        w: main.w - 200.0,
        h: main.h,
    };
    let r = place(w, h, Some(s), &[main]);
    assert!(inside(r, main));
    assert!(r.max_x() <= s.x + SETTINGS_SIDEBAR_W, "{r:?}");
    assert_eq!(r.y, s.y + PLACE_GAP, "the sidebar's lower part");
    let second = Rect {
        x: main.max_x(),
        y: 0.0,
        w: 1920.0,
        h: 1080.0,
    };
    let s = Rect {
        x: second.x + 100.0,
        y: 200.0,
        w: 800.0,
        h: 600.0,
    };
    let r = place(w, h, Some(s), &[main, second]);
    assert!(inside(r, second));
    let r = place(w, h, None, &[main, second]);
    assert!(inside(r, main));
    assert_eq!(
        (r.max_x(), r.max_y()),
        (main.max_x() - PLACE_GAP, main.max_y() - PLACE_GAP)
    );
    let cg = Rect {
        x: 10.0,
        y: 100.0,
        w: 800.0,
        h: 600.0,
    };
    assert_eq!(
        cg_to_cocoa(cg, 1080.0),
        Rect {
            x: 10.0,
            y: 380.0,
            w: 800.0,
            h: 600.0
        }
    );
}

#[test]
fn a_reveal_comes_in_front_only_while_system_settings_is() {
    assert!(
        may_reveal(true, false, false),
        "Settings in front, panel out"
    );
    assert!(
        !may_reveal(false, false, false),
        "a retry after another app came forward"
    );
    assert!(!may_reveal(true, true, false), "already in front");
    assert!(!may_reveal(true, false, true), "its tile's drag runs");
}

#[test]
fn a_minimized_system_settings_hides_the_panel_until_its_window_returns() {
    let window = Rect {
        x: 200.0,
        y: 300.0,
        w: 800.0,
        h: 600.0,
    };
    assert_eq!(
        place_turn(true, true, true, Some(window)),
        PlaceTurn::Place(Some(window))
    );
    assert_eq!(
        place_turn(true, true, true, None),
        PlaceTurn::Hide,
        "minimized"
    );
    assert_eq!(
        place_turn(false, true, true, Some(window)),
        PlaceTurn::Return(window),
        "un-minimized"
    );
    assert_eq!(place_turn(false, true, true, None), PlaceTurn::Nothing);
    assert_eq!(place_turn(true, true, false, None), PlaceTurn::Place(None));
    assert_eq!(
        place_turn(false, false, true, Some(window)),
        PlaceTurn::Nothing
    );
}

#[test]
fn system_settings_password_window_under_the_panel_orders_it_out_until_it_goes() {
    let main = Rect {
        x: 200.0,
        y: 300.0,
        w: 740.0,
        h: 625.0,
    };
    let dialog = Rect {
        x: main.x + 240.0,
        y: main.y + 138.0,
        w: 260.0,
        h: 349.0,
    };
    let window = |layer, alpha, bounds| SettingsWindow {
        number: 0,
        layer,
        alpha,
        bounds,
    };
    let both = [window(0, 1.0, main), window(0, 1.0, dialog)];
    let level = 3;
    assert_eq!(settings_main(&both), Some(0));
    assert_eq!(
        settings_main(&[both[1], both[0]]),
        Some(1),
        "the 740 × 625 window, wherever it is listed"
    );
    assert_eq!(
        settings_main(&both[1..]),
        None,
        "the password window alone is no main window"
    );
    let over = Rect {
        x: main.x + 300.0,
        y: main.y + 50.0,
        w: PANEL_W,
        h: 540.0,
    };
    assert!(under_panel(&both, over, level), "moved over the window");
    let screen = Rect {
        x: 0.0,
        y: 0.0,
        w: 1728.0,
        h: 1117.0,
    };
    let beside = place(PANEL_W, 540.0, Some(main), &[screen]);
    assert!(!under_panel(&both, beside, level), "where `place` puts it");
    assert!(
        !under_panel(&both[..1], over, level),
        "the main window alone is no dialog: a panel moved over the list stays"
    );
    assert!(
        !under_panel(&[both[0], window(0, 0.0, dialog)], over, level),
        "transparent"
    );
    assert!(
        !under_panel(&[both[0], window(8, 1.0, dialog)], over, level),
        "the + button's file chooser floats above the panel"
    );
    assert!(
        under_panel(&[both[0], window(3, 1.0, dialog)], over, level),
        "at the panel's own level: out"
    );
    assert!(
        !under_panel(&[both[0], window(0, 1.0, main)], over, level),
        "a companion on the main window's own frame is no dialog"
    );
    assert!(settings_busy(&both, level), "wherever the panel stands");
    assert!(!settings_busy(&both[..1], level));
    assert!(
        !settings_busy(&both[1..], level),
        "no main window to be busy over"
    );
    assert!(!settings_busy(&[both[0], window(8, 1.0, dialog)], level));
    assert_eq!(cover_turn(true, false, true, true), Cover::Out);
    assert_eq!(cover_turn(true, false, true, false), Cover::Nothing);
    assert_eq!(
        cover_turn(false, true, true, true),
        Cover::Nothing,
        "still under: it stays out"
    );
    assert_eq!(
        cover_turn(false, true, true, false),
        Cover::Back,
        "gone: back"
    );
    assert_eq!(
        cover_turn(false, true, false, false),
        Cover::Nothing,
        "another app in front: the activation observer's"
    );
    assert_eq!(
        cover_turn(false, false, true, false),
        Cover::Nothing,
        "out for another reason: not this turn's to bring back"
    );
}

/// System Settings asking is known off screen by identity, not inferred from the list of
/// every window: a question seen on screen (Quit & Reopen's sheet over its main window) is
/// remembered by both windows' numbers, and while the main window is off screen (on another
/// Space beside a full-screen aterm, hidden, minimized) it stands only while that same window
/// is listed over that same main one. It is gone, and forgotten, once either number leaves the
/// list (System Settings quit lists neither) or once the main window is back on screen without
/// it. A reading taken while System Settings is still the active app (its main window
/// minimized) counts as the person's latest with it, and [`ASKING_KEPT_FOR`] after that one,
/// still listed, the question is [`Asking::Lapsed`], never gone, until a reading shows it
/// gone. NEGATIVE CONTROLS: a window over the main one that was never seen asking on screen
/// (one System Settings ordered out and keeps, or a new sheet seen nowhere) asks nothing and
/// the list of every window is not read with nothing remembered; with the main window on
/// screen the on-screen reading decides and that list is never read; a reading off screen with
/// System Settings not the active app leaves the bound counting from the last one with it.
#[test]
fn system_settings_asking_is_known_off_screen_by_the_window_seen_asking() {
    let main = Rect {
        x: 200.0,
        y: 300.0,
        w: 740.0,
        h: 625.0,
    };
    let sheet = Rect {
        x: main.x + 240.0,
        y: main.y + 400.0,
        w: 260.0,
        h: 180.0,
    };
    let window = |number, bounds| SettingsWindow {
        number,
        layer: 0,
        alpha: 1.0,
        bounds,
    };
    // The measured numbers: the main window keeps #57, each sheet gets a new one.
    let asking = vec![window(57, main), window(82, sheet)];
    let answered = vec![window(57, main)];
    let unread = || -> Vec<SettingsWindow> { panic!("every window is read only when needed") };
    let t0 = Instant::now();
    let seen = SeenAsking {
        asking: 82,
        main: 57,
        at: t0,
    };
    assert_eq!(
        settings_asking(&asking, 0, None, t0, false, unread),
        (Asking::Asks, Some(seen)),
        "on screen: asks, remembered by both numbers"
    );
    assert_eq!(
        settings_asking(&answered, 0, Some(seen), t0, true, unread),
        (Asking::No, None),
        "on screen without it: gone, forgotten"
    );
    let off_screen: &[SettingsWindow] = &[];
    let later = t0 + Duration::from_secs(30);
    assert_eq!(
        settings_asking(off_screen, 0, Some(seen), later, false, || asking.clone()),
        (Asking::Asks, Some(seen)),
        "off screen, the same sheet still listed over the same main window: it stands, and \
         the bound still counts from the person's last reading with System Settings"
    );
    assert_eq!(
        settings_asking(off_screen, 0, Some(seen), later, true, || asking.clone()),
        (Asking::Asks, Some(SeenAsking { at: later, ..seen })),
        "minimized, System Settings still the active app: the bound counts from this reading"
    );
    assert_eq!(
        settings_asking(off_screen, 0, Some(seen), later, false, || answered.clone()),
        (Asking::No, None),
        "off screen, its number gone: answered"
    );
    assert_eq!(
        settings_asking(off_screen, 0, Some(seen), later, false, Vec::new),
        (Asking::No, None),
        "System Settings quit: gone"
    );
    assert_eq!(
        settings_asking(off_screen, 0, Some(seen), later, false, || {
            vec![window(60, main), window(82, sheet)]
        }),
        (Asking::No, None),
        "its main window gone (another opened in its place): gone"
    );
    assert_eq!(
        settings_asking(off_screen, 0, Some(seen), later, false, || {
            vec![window(57, main), window(82, sheet), window(98, sheet)]
        }),
        (Asking::Asks, Some(seen)),
        "a window kept beside it changes nothing while the one seen stands"
    );
    assert_eq!(
        settings_asking(off_screen, 0, Some(seen), later, false, || {
            vec![window(57, main), window(98, sheet)]
        }),
        (Asking::No, None),
        "a kept window over the main one is no question seen: the one seen went"
    );
    let past = t0 + ASKING_KEPT_FOR;
    assert_eq!(
        settings_asking(off_screen, 0, Some(seen), past, false, || asking.clone()),
        (Asking::Lapsed, Some(seen)),
        "past the bound, still listed: lapsed, not gone, and still remembered"
    );
    assert_eq!(
        settings_asking(off_screen, 0, Some(seen), past, false, || answered.clone()),
        (Asking::No, None),
        "past the bound, its number gone: gone"
    );
    assert_eq!(
        settings_asking(&answered, 0, Some(seen), past, true, unread),
        (Asking::No, None),
        "past the bound, read on screen without it: gone"
    );
    assert_eq!(
        settings_asking(off_screen, 0, None, later, false, unread),
        (Asking::No, None),
        "never seen asking: windows off screen are not read"
    );
    assert_eq!(
        settings_asking(
            &[window(57, main), window(0, sheet)],
            0,
            None,
            t0,
            false,
            unread
        ),
        (Asking::Asks, None),
        "on screen with no number to know it by: it asks now, and nothing is remembered"
    );
}

/// System Settings is read while the person is with it: the lane installs the watch once, in
/// a windowed instance only; System Settings becoming the active app is read at the next
/// turn, and every [`SETTINGS_FRONT_READ`] after (the lane's deadline names each) until it
/// resigns, which is read once more and ends the cadence; each post wakes the lane. So a
/// question it asks is read on screen before the person can take its windows off screen.
/// NEGATIVE CONTROLS: a turn with nothing posted and System Settings not the active app reads
/// nothing; a turn before the cadence's next reading reads nothing; a lane that is not
/// windowed installs no watch.
#[test]
fn system_settings_is_read_while_the_person_is_with_it() {
    use std::cell::{Cell, RefCell};
    use std::sync::atomic::{AtomicU32, Ordering};
    thread_local! {
        static MOVES: RefCell<Option<SettingsMoves>> = const { RefCell::new(None) };
        static WATCHES: Cell<u32> = const { Cell::new(0) };
        static READS: Cell<u32> = const { Cell::new(0) };
    }
    let arm = DevToolsHelperArm {
        watch_settings: |moves| {
            WATCHES.with(|n| n.set(n.get() + 1));
            MOVES.with(|slot| *slot.borrow_mut() = Some(moves));
            true
        },
        settings_asks: || {
            READS.with(|n| n.set(n.get() + 1));
            false
        },
        ..DevToolsHelperArm::inert()
    };
    let woken = Arc::new(AtomicU32::new(0));
    let wake = || -> Wake {
        let woken = Arc::clone(&woken);
        Arc::new(move || {
            woken.fetch_add(1, Ordering::SeqCst);
        })
    };
    let reads = || READS.with(Cell::get);
    let t0 = Instant::now();
    let mut lane = Lane::windowed_for_test(test_reach(), arm);
    lane.watch_settings_moves(wake());
    lane.watch_settings_moves(wake());
    assert_eq!(WATCHES.with(Cell::get), 1, "installed once");
    assert!(lane.watch_tried());
    assert!(!lane.read_settings_moves(t0), "nothing posted");
    assert_eq!(reads(), 0, "nothing posted: nothing read");
    assert_eq!(lane.next_deadline(t0, || true), None, "no cadence");
    let moves = MOVES
        .with(|slot| slot.borrow().clone())
        .expect("the watch's handle");
    moves.came();
    assert_eq!(woken.load(Ordering::SeqCst), 1, "the post wakes the lane");
    assert!(lane.read_settings_moves(t0), "came: read");
    assert_eq!(reads(), 1, "read at the turn after System Settings came");
    assert_eq!(
        lane.next_deadline(t0, || true),
        Some(t0 + SETTINGS_FRONT_READ),
        "the cadence's next reading wakes the park"
    );
    assert!(!lane.read_settings_moves(t0 + SETTINGS_FRONT_READ / 2));
    assert_eq!(reads(), 1, "before the cadence's next reading: none");
    let due = t0 + SETTINGS_FRONT_READ;
    assert!(lane.read_settings_moves(due), "the cadence");
    assert_eq!(reads(), 2, "read again while it stays the active app");
    assert_eq!(
        lane.next_deadline(due, || true),
        Some(due + SETTINGS_FRONT_READ)
    );
    moves.left();
    assert!(lane.read_settings_moves(due), "left: read");
    assert_eq!(reads(), 3, "read at the turn after the person left");
    assert!(!lane.read_settings_moves(due + SETTINGS_FRONT_READ * 4));
    assert_eq!(reads(), 3, "left: the cadence ends");
    assert_eq!(lane.next_deadline(due, || true), None);
    moves.left();
    moves.left();
    assert!(lane.read_settings_moves(due));
    assert_eq!(reads(), 4, "two posts before a turn: one read");

    let mut headless = Lane::with_arms(false, test_reach(), FdaReach::inert(), arm);
    headless.watch_settings_moves(wake());
    assert_eq!(
        WATCHES.with(Cell::get),
        1,
        "no watch in a lane not windowed"
    );
}

#[test]
fn the_list_read_waits_while_system_settings_shows_its_password_window() {
    use std::sync::atomic::{AtomicBool, AtomicU32, Ordering};
    static BUSY: AtomicBool = AtomicBool::new(true);
    static READS: AtomicU32 = AtomicU32::new(0);
    let reach = Reach {
        read_list: |_, _| {
            READS.fetch_add(1, Ordering::SeqCst);
            test_arms::absent(2)
        },
        ..test_reach()
    };
    let arm = DevToolsHelperArm {
        settings_busy: || BUSY.load(Ordering::SeqCst),
        ..test_arm()
    };
    let mut lane = Lane::windowed_for_test(reach, arm);
    let wake = nudge();
    let t0 = Instant::now();
    assert!(lane.show(show(test_arms::absent(2), None), t0, Arc::clone(&wake)));
    let gate = Some(ProbeGate::on());
    let _ = lane.tick_panel(t0, gate, &wake);
    events_of(&lane).post(PanelEvent::DragEnded { accepted: true });
    let mut at = t0;
    for _ in 0..12 {
        at += PANEL_TICK;
        let _ = lane.tick_panel(at, gate, &wake);
    }
    assert_eq!(
        READS.load(Ordering::SeqCst),
        0,
        "a drop and twelve ticks while the password window is up: no read"
    );
    BUSY.store(false, Ordering::SeqCst);
    let _ = lane.tick_panel(at + PANEL_TICK, gate, &wake);
    until(|| READS.load(Ordering::SeqCst) == 1);
}

#[test]
fn the_verdict_refresh_runs_on_a_window_one_at_a_time_and_never_headless() {
    use std::sync::atomic::{AtomicBool, AtomicU32, Ordering};
    static RUNS: AtomicU32 = AtomicU32::new(0);
    static HELD: AtomicBool = AtomicBool::new(true);
    let mut headless = Lane::new(true);
    assert!(!headless.refresh_verdict(), "headless: nothing runs");
    assert_eq!(headless.refreshes_started(), 0);
    let reach = Reach {
        refresh_verdict: || {
            RUNS.fetch_add(1, Ordering::SeqCst);
            until(|| !HELD.load(Ordering::SeqCst));
            Ok(())
        },
        ..test_reach()
    };
    let mut lane = Lane::windowed_for_test(reach, test_arm());
    assert!(lane.refresh_verdict());
    until(|| RUNS.load(Ordering::SeqCst) == 1);
    assert!(
        !lane.refresh_verdict(),
        "one at a time: a second waits for the first"
    );
    assert_eq!(lane.refreshes_started(), 1);
    HELD.store(false, Ordering::SeqCst);
    until(|| !lane.refreshing.load(Ordering::SeqCst));
    assert!(lane.refresh_verdict(), "the next switch-on runs again");
    until(|| RUNS.load(Ordering::SeqCst) == 2);
    assert_eq!(lane.refreshes_started(), 2);
}

/// A refresh that took keeps when it ended, the grant's effect time; one that did not settle
/// keeps none and is taken once, to be asked again; a reading that finds aterm not granted
/// forgets the effect time, whether a whole reading or a list read alone says so.
#[test]
fn a_refresh_that_took_keeps_its_end_and_one_that_did_not_settle_is_taken_once() {
    use atpkg::devtools::RefreshFailed;
    use std::sync::atomic::Ordering;
    let run = |refresh_verdict: fn() -> Result<(), RefreshFailed>| {
        let mut lane = Lane::windowed_for_test(
            Reach {
                refresh_verdict,
                ..test_reach()
            },
            test_arm(),
        );
        let before = aterm_time::unix_millis().unwrap_or(0);
        assert!(lane.refresh_verdict());
        until(|| !lane.refreshing.load(Ordering::SeqCst));
        (lane, before)
    };
    let (mut took, before) = run(|| Ok(()));
    assert!(
        took.took_at().is_some_and(|at| at >= before),
        "{:?} from {before}",
        took.took_at()
    );
    assert!(!took.take_unsettled(), "it took");
    took.note_reading(
        test_arms::reading(DevToolGrant::Granted, test_arms::on()),
        Instant::now(),
    );
    assert!(took.took_at().is_some(), "a granted reading keeps it");
    took.note_reading(
        test_arms::reading(DevToolGrant::NotGranted, test_arms::on()),
        Instant::now(),
    );
    assert_eq!(took.took_at(), None, "not granted: the effect time is gone");
    let (mut listed, _) = run(|| Ok(()));
    listed.note_list(test_arms::on());
    assert!(
        listed.took_at().is_some(),
        "a list that reads aterm on keeps it"
    );
    listed.note_list(test_arms::absent(2));
    assert_eq!(
        listed.took_at(),
        None,
        "a list that reads aterm absent: the effect time is gone"
    );

    let (unsettled, _) = run(|| Err(RefreshFailed::NotSettled(None)));
    assert_eq!(unsettled.took_at(), None, "not settled: no effect time");
    assert!(unsettled.take_unsettled(), "asked again");
    assert!(!unsettled.take_unsettled(), "once");

    let (failed, _) = run(|| Err(RefreshFailed::Failed("codesign: denied".into())));
    assert_eq!(failed.took_at(), None);
    assert!(!failed.take_unsettled(), "a failure is not asked again");
}

/// A refresh that took while a reading found aterm not granted says nothing of the grant: the
/// owner turned aterm off while its prime ran, so it keeps no effect time. The next refresh
/// keeps its own.
#[test]
fn a_refresh_that_took_while_aterm_read_not_granted_keeps_no_effect_time() {
    use std::sync::atomic::{AtomicBool, Ordering};
    static HELD: AtomicBool = AtomicBool::new(true);
    let mut lane = Lane::windowed_for_test(
        Reach {
            refresh_verdict: || {
                until(|| !HELD.load(Ordering::SeqCst));
                Ok(())
            },
            ..test_reach()
        },
        test_arm(),
    );
    assert!(lane.refresh_verdict());
    lane.note_reading(
        test_arms::reading(DevToolGrant::NotGranted, test_arms::on()),
        Instant::now(),
    );
    HELD.store(false, Ordering::SeqCst);
    until(|| !lane.refreshing.load(Ordering::SeqCst));
    assert_eq!(
        lane.took_at(),
        None,
        "turned off while it ran: no effect time"
    );
    assert!(lane.refresh_verdict());
    until(|| !lane.refreshing.load(Ordering::SeqCst));
    assert!(lane.took_at().is_some(), "the next refresh keeps its own");
}

/// Only the first of a run of refreshes that did not settle waits to be asked again: a queue
/// that stays backed up gets one more prime, never one every few minutes. A refresh that took
/// ends the run.
#[test]
fn only_the_first_of_a_run_of_refreshes_that_did_not_settle_is_asked_again() {
    use atpkg::devtools::RefreshFailed;
    use std::sync::atomic::{AtomicBool, Ordering};
    static SETTLES: AtomicBool = AtomicBool::new(false);
    let mut lane = Lane::windowed_for_test(
        Reach {
            refresh_verdict: || {
                if SETTLES.load(Ordering::SeqCst) {
                    Ok(())
                } else {
                    Err(RefreshFailed::NotSettled(None))
                }
            },
            ..test_reach()
        },
        test_arm(),
    );
    let asked_again = |lane: &mut Lane| {
        assert!(lane.refresh_verdict());
        until(|| !lane.refreshing.load(Ordering::SeqCst));
        lane.take_unsettled()
    };
    assert!(asked_again(&mut lane), "the first that did not settle");
    assert!(!asked_again(&mut lane), "the second in a row is not");
    assert!(!asked_again(&mut lane), "nor the third");
    SETTLES.store(true, Ordering::SeqCst);
    assert!(!asked_again(&mut lane), "one that took");
    SETTLES.store(false, Ordering::SeqCst);
    assert!(asked_again(&mut lane), "ends the run");
}

#[test]
fn a_drop_taken_outside_system_settings_spends_nothing() {
    let window = Rect {
        x: 200.0,
        y: 300.0,
        w: 800.0,
        h: 600.0,
    };
    let landed = |x, y| drop_landed(Some(window), x, y, 64.0, 64.0);
    assert!(landed(500.0, 500.0), "on the list's window");
    assert!(landed(window.x, window.max_y()), "its edge");
    assert!(!landed(window.max_x() + 6.0, 500.0), "the gap");
    assert!(!landed(500.0, 100.0), "below it");
    assert!(
        landed(500.0, window.y - 40.0),
        "the image's corner below the edge"
    );
    assert!(
        landed(window.x - 40.0, 500.0),
        "the image's corner left of it"
    );
    assert!(!landed(window.x - 100.0, 500.0), "more than an image away");
    assert!(
        drop_landed(None, 0.0, 0.0, 64.0, 64.0),
        "no window read: the list's"
    );
    let t0 = Instant::now();
    let bundle = PathBuf::from("/Applications/aterm.app");
    let mut panel = Panel::new(show(absent(2), Some(bundle)), t0);
    assert!(panel.on(PanelEvent::DroppedOutside, t0).redraw);
    assert_eq!(panel.phase(), Phase::DroppedOutside);
    let words = panel.content();
    assert!(has(&words, DROP_OUTSIDE_WORDS));
    assert!(words.tile().is_some_and(|tile| tile.live), "not spent");
    assert!(has(&words, atpkg::devtools::HELPER_HOW), "drag or +, again");
    let _ = panel.on(PanelEvent::DragEnded { accepted: true }, t0);
    assert!(panel.content().tile().is_some_and(|tile| !tile.live));
    assert_eq!(panel.on(PanelEvent::DroppedOutside, t0), Effect::default());
    assert!(DROP_UNCONFIRMED_WORDS.contains("Finder") && DROP_UNCONFIRMED_WORDS.contains("Trash"));
    assert!(DROP_OUTSIDE_WORDS.contains("Trash"));
}

#[test]
fn the_tile_drags_only_the_installed_aterm_it_reads_for() {
    let root = || PathBuf::from("/Applications/aterm.app");
    let id = Some("com.aterm.aterm");
    assert_eq!(accepted_bundle(root(), true, id, id), Some(root()));
    assert_eq!(
        accepted_bundle(root(), false, id, id),
        None,
        "not installed"
    );
    assert_eq!(accepted_bundle(root(), true, None, id), None, "no id read");
    assert_eq!(
        accepted_bundle(root(), true, Some("com.aterm.aterm.dev"), id),
        None,
        "another bundle"
    );
    assert_eq!(accepted_bundle(root(), true, id, None), None, "no subject");
}

#[test]
fn the_tiles_icon_is_made_at_first_and_again_only_when_its_appearance_changes() {
    assert!(icon_due(None, "NSAppearanceNameAqua"), "never asked");
    assert!(
        !icon_due(Some("NSAppearanceNameAqua"), "NSAppearanceNameAqua"),
        "same look"
    );
    assert!(
        icon_due(Some("NSAppearanceNameAqua"), "NSAppearanceNameDarkAqua"),
        "the tile turned dark: its icon style follows"
    );
    assert!(icon_due(None, ""), "an unnamed look is still asked once");
    assert!(!icon_due(Some(""), ""));
}

fn test_arm() -> DevToolsHelperArm {
    test_arms::showing()
}

fn test_reach() -> Reach {
    test_arms::opening()
}

fn nudge() -> Wake {
    Arc::new(|| {})
}

fn until(mut done: impl FnMut() -> bool) {
    let start = Instant::now();
    while !done() {
        assert!(start.elapsed() < Duration::from_secs(60), "hang");
        std::thread::sleep(Duration::from_millis(1));
    }
}

/// The window's own step keeps its place in each page's queue, and another origin's ask it
/// holds in its place rides with it, with what its own `spctl` run answered: one that comes
/// while it waits, the one it replaces, or the one a step it replaces carried. The window
/// takes the rider once the step is given back and judges it by its own rules
/// ([`Lane::take_rider`]); a client's rider lapses as it would alone, and one that waits
/// again with the step, or for the step's `spctl` run, keeps its time unless the step carries
/// a newer ask. The control verb's rider is also taken at its own quiet moment while the
/// step waits on ([`Lane::take_quiet_rider`]); a System State act stays with the step.
#[test]
fn the_window_s_own_step_carries_the_ask_it_holds_in_its_place() {
    fn who(rider: Option<Rider>) -> Option<(Origin, Instant)> {
        rider.map(|rider| (rider.origin, rider.at))
    }
    let t0 = Instant::now();
    let t1 = t0 + RECHECK_EVERY;
    let ctl = Origin::Ctl;
    let mut lane = Lane::windowed_for_test(Reach::inert(), DevToolsHelperArm::inert());
    lane.queue(Origin::Automatic, t0);
    assert_eq!(lane.take_queued(t0, true), Some((Origin::Automatic, None)));
    assert_eq!(lane.take_rider(t0), None, "nothing rode");
    lane.queue(Origin::Automatic, t0);
    lane.queue(ctl, t1);
    assert_eq!(lane.queued(), Some(Origin::Automatic));
    assert_eq!(lane.take_queued(t0, true), Some((Origin::Automatic, None)));
    assert_eq!(who(lane.take_rider(t0)), Some((ctl, t1)));
    assert_eq!(lane.take_rider(t0), None, "taken once");
    lane.queue(Origin::SystemStateAct, t0);
    lane.queue(Origin::Automatic, t1);
    lane.queue(Origin::Automatic, t0 + RECHECK_EVERY * 2);
    assert_eq!(
        lane.take_queued(t0, true),
        Some((Origin::Automatic, None)),
        "the step took the act's place"
    );
    assert_eq!(who(lane.take_rider(t0)), Some((Origin::SystemStateAct, t0)));
    lane.queue(Origin::Automatic, t0);
    lane.queue_after_listing(ctl, t0, Err("spctl exited 1".to_string()));
    let _ = lane.take_queued(t0, true);
    assert_eq!(
        lane.take_rider(t0 + ASKED_LAPSE),
        None,
        "a client's ask lapses as it would alone"
    );
    lane.queue(Origin::Automatic, t0);
    lane.queue_after_listing(ctl, t0, Ok(()));
    let _ = lane.take_queued(t0, true);
    assert_eq!(
        lane.take_rider(t0).map(|rider| rider.listed),
        Some(Some(Ok(()))),
        "the rider keeps its own run's answer"
    );
    lane.queue(ctl, t0);
    lane.queue_after_listing(Origin::Automatic, t1, Ok(()));
    assert_eq!(
        lane.take_queued(t1, true),
        Some((Origin::Automatic, Some(Ok(()))))
    );
    assert_eq!(
        who(lane.take_rider(t1)),
        Some((ctl, t0)),
        "the ask a listed step took the place of"
    );
    lane.queue(Origin::Automatic, t0);
    lane.queue(Origin::SystemStateAct, t0);
    assert_eq!(
        lane.take_quiet_rider(t0),
        None,
        "a client's act waits for the step moment with the step"
    );
    let _ = lane.take_queued(t0, true);
    assert_eq!(
        who(lane.take_rider(t0)),
        Some((Origin::SystemStateAct, t0)),
        "and still rides it"
    );
    lane.queue(Origin::Automatic, t0);
    lane.queue_after_listing(ctl, t0, Ok(()));
    let rider = lane.take_quiet_rider(t0).expect("the control verb's ask");
    assert_eq!(
        (rider.origin, rider.listed),
        (ctl, Some(Ok(()))),
        "the control verb's keeps its own quiet moment, and its run's answer"
    );
    assert_eq!(lane.queued(), Some(Origin::Automatic), "the step waits on");
    assert_eq!(lane.take_quiet_rider(t0), None, "taken once");
    lane.queue(ctl, t0);
    assert_eq!(lane.take_quiet_rider(t0 + ASKED_LAPSE), None, "lapsed");
    let _ = lane.take_queued(t0, true);
    assert_eq!(lane.take_rider(t0), None, "and gone");
    lane.queue(Origin::Automatic, t0);
    lane.queue(ctl, t0);
    let _ = lane.take_queued(t0, true);
    let rider = lane.take_rider(t0).expect("the client's ask");
    lane.queue(Origin::Automatic, t1);
    lane.ride(rider.clone());
    let _ = lane.take_queued(t1, true);
    assert_eq!(
        who(lane.take_rider(t0)),
        Some((ctl, t0)),
        "it waited again as asked"
    );
    lane.queue(Origin::Automatic, t1);
    lane.queue(Origin::SystemStateAct, t1);
    lane.ride(rider.clone());
    let _ = lane.take_queued(t1, true);
    assert_eq!(
        who(lane.take_rider(t1)),
        Some((Origin::SystemStateAct, t1)),
        "the step keeps the newer ask"
    );
    lane.ride_listing(rider.clone());
    assert_eq!(
        lane.take_listing_rider(t0),
        Some(rider.clone()),
        "it waited for the run"
    );
    assert_eq!(lane.take_listing_rider(t0), None, "taken once");
    lane.ride_listing(rider);
    assert_eq!(lane.take_listing_rider(t0 + ASKED_LAPSE), None, "lapsed");

    let queue_fda = |lane: &mut Lane, origin: Origin, at: Instant| {
        #[expect(clippy::disallowed_methods, reason = "the lane's own test")]
        lane.queue_fda(origin, at);
    };
    queue_fda(&mut lane, Origin::Automatic, t0);
    queue_fda(&mut lane, Origin::SystemStateAct, t1);
    assert_eq!(lane.take_queued_fda(t0, true), Some(Origin::Automatic));
    assert_eq!(
        who(lane.take_fda_rider(t0)),
        Some((Origin::SystemStateAct, t1))
    );
    queue_fda(&mut lane, Origin::SystemStateAct, t0);
    queue_fda(&mut lane, Origin::Automatic, t0);
    assert_eq!(lane.take_queued_fda(t0, true), Some(Origin::Automatic));
    let rider = lane
        .take_fda_rider(t0)
        .expect("the act the step took the place of");
    assert_eq!(who(Some(rider.clone())), Some((Origin::SystemStateAct, t0)));
    queue_fda(&mut lane, Origin::Automatic, t0);
    lane.ride_fda(rider.clone());
    assert_eq!(lane.take_queued_fda(t0, true), Some(Origin::Automatic));
    assert_eq!(lane.take_fda_rider(t0), Some(rider), "it waited again");
    assert_eq!(lane.take_rider(t0), None, "each page keeps its own");
}

/// A page that comes up answers what waits for it, except the window's own step while no
/// access revision stands: that page is not the revision's, so the step waits on for it,
/// without the ask it carried (the page was that ask's), and looks again only once the
/// page's ten minutes are past, so it is not refused as recent: a look that finds no moment
/// meanwhile, another step of its own taking its place, a nearer look asked of it
/// (`Lane::defer`, a reading still out) or a nearer page keeps that. Once a revision
/// stands, a kept step whose page that revision brought up already looks at once. Under a
/// revision, or for another origin's ask, the page answers it.
#[test]
fn a_page_up_with_no_revision_leaves_the_window_s_own_step_waiting() {
    let t0 = Instant::now();
    let ten = Duration::from_secs(atpkg::devtools::REOPEN_AFTER_S);
    let ctl = Origin::Ctl;
    let mut lane = Lane::windowed_for_test(Reach::inert(), DevToolsHelperArm::inert());
    lane.queue(Origin::Automatic, t0);
    lane.queue(ctl, t0);
    lane.page_came_up(false, t0 + ten);
    assert_eq!(lane.queued(), Some(Origin::Automatic), "the step waits on");
    assert_eq!(lane.kept(), [true, false]);
    assert_eq!(lane.take_queued(t0, false), None, "no moment");
    assert!(
        !lane.hand_over_due(t0 + ten - RECHECK_EVERY),
        "within the page's ten minutes"
    );
    lane.queue_after_listing(Origin::Automatic, t0, Ok(()));
    assert!(
        !lane.hand_over_due(t0 + ten - RECHECK_EVERY),
        "its own step in its place keeps the wait"
    );
    lane.defer(t0 + RECHECK_EVERY);
    lane.wait_out_page(t0 + RECHECK_EVERY);
    assert!(
        !lane.hand_over_due(t0 + ten - RECHECK_EVERY),
        "a nearer look or page keeps the wait"
    );
    assert_eq!(
        lane.next_deadline(t0, || true),
        Some(t0 + ten),
        "and wakes for it"
    );
    assert!(lane.hand_over_due(t0 + ten), "then it looks");
    let _ = lane.take_queued(t0 + ten, true);
    assert_eq!(lane.take_rider(t0), None, "the page was the ask's");
    lane.queue(Origin::Automatic, t0);
    lane.page_came_up(false, t0 + ten);
    lane.revision_stands(t0, [false, false]);
    assert_eq!(lane.kept(), [false, false], "asked once");
    assert!(!lane.hand_over_due(t0), "not shown: it waits out the page");
    lane.page_came_up(false, t0 + ten);
    lane.revision_stands(t0, [true, false]);
    assert!(lane.hand_over_due(t0), "shown: it looks at once");
    lane.page_came_up(true, t0);
    assert_eq!(lane.queued(), None, "a revision's page answers its step");
    lane.queue(ctl, t0);
    lane.page_came_up(false, t0);
    assert_eq!(lane.queued(), None, "and any other ask");

    let queue_fda = |lane: &mut Lane, origin: Origin, at: Instant| {
        #[expect(clippy::disallowed_methods, reason = "the lane's own test")]
        lane.queue_fda(origin, at);
    };
    queue_fda(&mut lane, Origin::Automatic, t0);
    queue_fda(&mut lane, Origin::SystemStateAct, t0);
    lane.fda_page_came_up(false, t0 + ten);
    assert_eq!(
        lane.fda_queued(),
        Some(Origin::Automatic),
        "the step waits on"
    );
    assert_eq!(lane.kept(), [false, true]);
    assert_eq!(lane.take_queued_fda(t0, false), None, "no moment");
    assert!(!lane.fda_hand_over_due(t0 + ten - RECHECK_EVERY));
    lane.defer_fda(t0 + RECHECK_EVERY);
    lane.fda_wait_out_page(t0 + RECHECK_EVERY);
    assert!(
        !lane.fda_hand_over_due(t0 + ten - RECHECK_EVERY),
        "a nearer look or page keeps the wait"
    );
    lane.defer_fda(t0 + ten + RECHECK_EVERY);
    assert!(!lane.fda_hand_over_due(t0 + ten), "a later one moves it");
    assert!(lane.fda_hand_over_due(t0 + ten + RECHECK_EVERY));
    assert_eq!(
        lane.take_queued_fda(t0 + ten + RECHECK_EVERY, true),
        Some(Origin::Automatic)
    );
    assert_eq!(lane.take_fda_rider(t0), None, "the page was the act's");
    queue_fda(&mut lane, Origin::Automatic, t0);
    lane.fda_page_came_up(true, t0);
    assert_eq!(lane.fda_queued(), None);
}

/// A page a step waits out (a new access revision's step behind a page under ten minutes
/// old, `App::rearm_devtools_step`) holds it whatever else asks it to look: a reading still
/// out (`Lane::defer`), a look that finds no moment, or another step of its own taking its
/// place with its `spctl` run's answer. Only the reading's own look moves freely, and only
/// after the page.
#[test]
fn a_page_the_step_waits_out_holds_every_nearer_look() {
    let t0 = Instant::now();
    let ten = Duration::from_secs(atpkg::devtools::REOPEN_AFTER_S);
    let mut lane = Lane::windowed_for_test(Reach::inert(), DevToolsHelperArm::inert());
    lane.queue(Origin::Automatic, t0);
    assert!(!lane.waits_out_a_page());
    lane.wait_out_page(t0 + ten);
    assert!(lane.waits_out_a_page());
    lane.queue(Origin::Automatic, t0);
    lane.defer(t0 + PANEL_TICK);
    assert!(!lane.hand_over_due(t0 + ten - RECHECK_EVERY), "a reading");
    assert_eq!(lane.take_queued(t0, false), None);
    assert!(!lane.hand_over_due(t0 + ten - RECHECK_EVERY), "no moment");
    lane.queue_after_listing(Origin::Automatic, t0, Ok(()));
    assert!(!lane.hand_over_due(t0 + ten - RECHECK_EVERY), "its own run");
    assert!(lane.waits_out_a_page());
    assert!(lane.hand_over_due(t0 + ten));
    lane.defer(t0 + ten + PANEL_TICK);
    assert!(!lane.hand_over_due(t0 + ten), "a reading after the page");
    assert!(lane.hand_over_due(t0 + ten + PANEL_TICK));
    lane.queue(Origin::Ctl, t0);
    assert!(lane.waits_out_a_page(), "an ask rides the step");
    let _ = lane.take_queued(t0 + ten + PANEL_TICK, true);
    lane.queue(Origin::Ctl, t0);
    lane.wait_out_page(t0 + ten);
    assert!(!lane.waits_out_a_page(), "only the window's own step");

    let queue_fda = |lane: &mut Lane, origin: Origin, at: Instant| {
        #[expect(clippy::disallowed_methods, reason = "the lane's own test")]
        lane.queue_fda(origin, at);
    };
    queue_fda(&mut lane, Origin::Automatic, t0);
    lane.fda_wait_out_page(t0 + ten);
    lane.defer_fda(t0 + PANEL_TICK);
    assert!(
        !lane.fda_hand_over_due(t0 + ten - RECHECK_EVERY),
        "a reading"
    );
    assert_eq!(lane.take_queued_fda(t0, false), None);
    assert!(
        !lane.fda_hand_over_due(t0 + ten - RECHECK_EVERY),
        "no moment"
    );
    assert!(lane.fda_hand_over_due(t0 + ten));
}

#[test]
fn a_waiting_hand_over_looks_every_few_seconds_only_while_focused() {
    let t0 = Instant::now();
    let mut lane = Lane::windowed_for_test(Reach::inert(), DevToolsHelperArm::inert());
    assert_eq!(lane.next_deadline(t0, || true), None, "nothing waits");
    lane.queue(Origin::Ctl, t0);
    lane.queue(Origin::Automatic, t0);
    lane.queue(Origin::Ctl, t0);
    assert_eq!(
        lane.queued(),
        Some(Origin::Automatic),
        "the window's own wins"
    );
    assert_eq!(lane.next_deadline(t0, || true), Some(t0));
    assert_eq!(lane.next_deadline(t0, || false), None, "unfocused: nothing");
    assert!(lane.hand_over_due(t0));
    assert_eq!(lane.take_queued(t0, false), None);
    assert!(!lane.hand_over_due(t0 + RECHECK_EVERY / 2));
    assert_eq!(lane.next_deadline(t0, || true), Some(t0 + RECHECK_EVERY));
    let late = t0 + ASKED_LAPSE * 4;
    assert_eq!(
        lane.take_queued(late, true),
        Some((Origin::Automatic, None)),
        "the window's own never lapses"
    );
    assert_eq!(lane.queued(), None);
    lane.queue(Origin::Ctl, t0);
    assert_eq!(lane.take_queued(t0 + ASKED_LAPSE, true), None);
    assert_eq!(lane.queued(), None, "lapsed");
    // The Full Disk Access queue keeps the same order: the window's own outranks a
    // client's act, which lapses while the window's own waits on.
    let queue_fda = |lane: &mut Lane, origin: Origin, at: Instant| {
        #[expect(clippy::disallowed_methods, reason = "the lane's own test")]
        lane.queue_fda(origin, at);
    };
    queue_fda(&mut lane, Origin::Ctl, t0);
    queue_fda(&mut lane, Origin::Ctl, t0 + RECHECK_EVERY);
    assert_eq!(lane.fda_queued(), Some(Origin::Ctl));
    assert_eq!(lane.take_queued_fda(t0 + ASKED_LAPSE, false), None);
    assert_eq!(lane.fda_queued(), None, "a client's act lapsed");
    queue_fda(&mut lane, Origin::Ctl, t0);
    queue_fda(&mut lane, Origin::Automatic, t0);
    queue_fda(&mut lane, Origin::Ctl, t0);
    assert_eq!(
        lane.fda_queued(),
        Some(Origin::Automatic),
        "the window's own wins"
    );
    assert_eq!(
        lane.take_queued_fda(late, true),
        Some(Origin::Automatic),
        "and never lapses"
    );
    lane.queue(Origin::Automatic, t0);
    lane.note_reading(
        Reading {
            grant: DevToolGrant::Granted,
            list: on(),
            ..Reading::UNREAD
        },
        t0,
    );
    assert_eq!(lane.queued(), None);
    lane.queue_after_listing(Origin::Ctl, t0, Ok(()));
    assert!(!lane.hand_over_due(t0), "the moment was judged just now");
    assert!(lane.hand_over_due(t0 + RECHECK_EVERY));
    assert_eq!(
        lane.take_queued(t0 + RECHECK_EVERY, true),
        Some((Origin::Ctl, Some(Ok(()))))
    );
    lane.queue(Origin::Automatic, t0);
    lane.queue_after_listing(Origin::Ctl, t0, Ok(()));
    assert_eq!(
        lane.take_queued(t0, true),
        Some((Origin::Automatic, Some(Ok(())))),
        "the window's own keeps its rank, with the run's answer"
    );
    let mut inert = Lane::new(true);
    assert!(!inert.windowed());
    inert.queue(Origin::Automatic, t0);
    assert_eq!(inert.next_deadline(t0, || true), None);
}

#[test]
fn the_panels_lane_reads_the_list_and_closes_when_done() {
    let t0 = Instant::now();
    let mut lane = Lane::windowed_for_test(test_reach(), test_arm());
    let wake = nudge();
    assert!(lane.show(show(absent(2), None), t0, Arc::clone(&wake)));
    assert_eq!(lane.next_deadline(t0, || false), Some(t0), "the first turn");
    let asks = lane.tick_panel(t0, Some(ProbeGate::on()), &wake);
    assert_eq!(asks, Asks::default());
    assert_eq!(
        lane.next_deadline(t0, || false),
        Some(t0 + PANEL_TICK),
        "every PANEL_TICK while shown, focused or not"
    );
    let t1 = t0 + PANEL_TICK;
    let mut asks = lane.tick_panel(t1, Some(ProbeGate::on()), &wake);
    assert!(lane.panel().is_some_and(|p| p.read_out), "one read out");
    until(|| {
        asks = lane.tick_panel(t1, Some(ProbeGate::on()), &wake);
        asks.granted
    });
    assert_eq!(lane.panel().map(Panel::phase), Some(Phase::Done));
    let _ = lane.tick_panel(t1 + DONE_FOR, Some(ProbeGate::on()), &wake);
    assert!(lane.panel().is_none(), "done closes itself");
    assert_eq!(lane.next_deadline(t1, || true), None);
    let shared = Arc::clone(&lane.shared);
    assert!(lane.show(show(absent(2), None), t0, Arc::clone(&wake)));
    let stale = PanelEvents {
        panel: lane.generation - 1,
        shared: Arc::clone(&shared),
        wake: Arc::clone(&wake),
    };
    stale.post(PanelEvent::NotNow);
    let _ = lane.tick_panel(t0, None, &wake);
    assert!(
        lane.panel().is_some(),
        "a replaced panel's press is dropped"
    );
    let events = PanelEvents {
        panel: lane.generation,
        shared,
        wake: Arc::clone(&wake),
    };
    events.post(PanelEvent::Status);
    assert!(lane.tick_panel(t0, None, &wake).open_status);
    assert_eq!(
        test_arms::decline_notes(),
        0,
        "done and replaced: no decline"
    );
    events.post(PanelEvent::NotNow);
    let _ = lane.tick_panel(t0, None, &wake);
    assert!(lane.panel().is_none(), "Not now closes it");
    assert_eq!(test_arms::decline_notes(), 1, "and is the owner's decline");
    let mut lane = Lane::windowed_for_test(
        test_reach(),
        DevToolsHelperArm {
            settings_running: || false,
            ..test_arm()
        },
    );
    assert!(lane.show(show(unread(), None), t0, Arc::clone(&wake)));
    let _ = lane.tick_panel(t0, None, &wake);
    assert!(
        lane.panel().is_some(),
        "System Settings may still be starting"
    );
    let _ = lane.tick_panel(t0 + SETTINGS_GRACE, None, &wake);
    assert!(lane.panel().is_none());
    let mut inert = Lane::windowed_for_test(test_reach(), DevToolsHelperArm::inert());
    assert!(!inert.show(show(absent(2), None), t0, wake));
    assert!(inert.panel().is_none());
    assert_eq!(inert.next_deadline(t0, || true), None);
}

#[test]
fn a_replaced_panels_read_cannot_overwrite_its_successors_completion() {
    use std::sync::mpsc::{self, Sender};
    type Read = (String, Sender<DevToolList>);
    static READS: Mutex<Option<Sender<Read>>> = Mutex::new(None);
    struct Reset;
    impl Drop for Reset {
        fn drop(&mut self) {
            *READS.lock_or_recover() = None;
        }
    }
    fn read(_gate: ProbeGate, subject: &str) -> DevToolList {
        let (answer, rx) = mpsc::channel();
        if let Some(reads) = READS.lock_or_recover().as_ref() {
            let _ = reads.send((subject.to_owned(), answer));
        }
        rx.recv_timeout(Duration::from_secs(60))
            .unwrap_or_else(|_| unread())
    }
    let (reads, requests) = mpsc::channel();
    *READS.lock_or_recover() = Some(reads);
    let _reset = Reset;
    let (completed, completions) = mpsc::channel();
    let wake: Wake = Arc::new(move || {
        let _ = completed.send(());
    });
    let reach = Reach {
        read_list: read,
        ..test_reach()
    };
    let mut lane = Lane::windowed_for_test(reach, test_arm());
    let t0 = Instant::now();
    let mut old = show(absent(2), None);
    old.reading.subject = Some("old-panel");
    assert!(lane.show(old, t0, Arc::clone(&wake)));
    let t1 = t0 + PANEL_TICK;
    let _ = lane.tick_panel(t1, Some(ProbeGate::on()), &wake);
    let (subject, old_answer) = requests.recv_timeout(Duration::from_secs(60)).unwrap();
    assert_eq!(subject, "old-panel");

    let mut new = show(absent(2), None);
    new.reading.subject = Some("new-panel");
    assert!(lane.show(new, t1, Arc::clone(&wake)));
    let t2 = t1 + PANEL_TICK;
    let _ = lane.tick_panel(t2, Some(ProbeGate::on()), &wake);
    let (subject, new_answer) = requests.recv_timeout(Duration::from_secs(60)).unwrap();
    assert_eq!(subject, "new-panel");
    assert!(lane.panel().is_some_and(|panel| panel.read_out));

    new_answer.send(on()).unwrap();
    completions.recv_timeout(Duration::from_secs(60)).unwrap();
    old_answer.send(absent(2)).unwrap();
    completions.recv_timeout(Duration::from_secs(60)).unwrap();
    let asks = lane.tick_panel(t2, Some(ProbeGate::on()), &wake);
    assert!(
        asks.granted,
        "the successor's completed grant must reach the park"
    );
    let panel = lane.panel().unwrap();
    assert_eq!(panel.phase(), Phase::Done);
    assert!(!panel.read_out, "the completed read must be retired");
}

#[test]
fn a_move_is_the_owners_only_on_the_same_screens() {
    let r = |x: f64, y: f64, w: f64, h: f64| Rect { x, y, w, h };
    let laptop = [r(0.0, 0.0, 1728.0, 1117.0)];
    let docked = [r(0.0, 0.0, 1728.0, 1117.0), r(1728.0, 0.0, 2560.0, 1440.0)];
    let placed = r(2300.0, 500.0, 360.0, 564.0);
    assert_eq!(drift(placed, &docked, placed, &docked), Drift::Still);
    assert_eq!(
        drift(placed, &docked, r(2300.4, 499.7, 360.0, 564.0), &docked),
        Drift::Still
    );
    assert_eq!(
        drift(placed, &docked, r(2100.0, 420.0, 360.0, 564.0), &docked),
        Drift::Owner
    );
    assert_eq!(
        drift(placed, &docked, r(1300.0, 500.0, 360.0, 564.0), &laptop),
        Drift::Screens
    );
    assert_eq!(drift(placed, &docked, placed, &laptop), Drift::Still);
}

#[test]
fn the_terminal_listing_runs_once_and_lands_in_the_slot() {
    let mut lane = Lane::windowed_for_test(test_reach(), test_arm());
    let t0 = Instant::now();
    assert!(lane.start_listing(Listing::HandOver(Origin::Digest), nudge()));
    assert!(
        lane.start_listing(Listing::ShowList, nudge()),
        "rides the first"
    );
    assert!(
        lane.next_deadline(t0, || false).is_some(),
        "re-read while out"
    );
    let mut taken = None;
    until(|| {
        taken = lane.take_listed();
        taken.is_some()
    });
    let digest_waits = Waiting {
        digest: true,
        ..Waiting::default()
    };
    assert_eq!(
        taken,
        Some((
            Listing::HandOver(Origin::Digest),
            Ok(()),
            digest_waits.clone()
        ))
    );
    assert_eq!(lane.listing(), None);
    assert_eq!(lane.next_deadline(t0, || false), None);
    assert!(lane.start_listing(Listing::ShowList, nudge()));
    assert!(lane.start_listing(Listing::HandOver(Origin::Settings), nudge()));
    assert_eq!(lane.listing(), Some(Listing::HandOver(Origin::Settings)));
    assert!(lane.start_listing(Listing::ShowList, nudge()));
    assert_eq!(lane.listing(), Some(Listing::HandOver(Origin::Settings)));
    let mut taken = None;
    until(|| {
        taken = lane.take_listed();
        taken.is_some()
    });
    assert_eq!(
        taken,
        Some((
            Listing::HandOver(Origin::Settings),
            Ok(()),
            Waiting::default()
        )),
        "one run, the hand-over's"
    );
}

/// A `spctl` run started ahead of a queued hand-over's moment is kept for it, once, and
/// nothing starts a second; an ask made while one is out rides it and takes its end.
/// Negative control: with no run ahead nothing is kept, so the moment would make one.
#[test]
fn a_run_ahead_of_the_moment_is_kept_for_the_hand_over_once() {
    use std::sync::atomic::{AtomicU32, Ordering};
    static RUNS: AtomicU32 = AtomicU32::new(0);
    let mut reach = test_reach();
    reach.list_terminal = || {
        RUNS.fetch_add(1, Ordering::SeqCst);
        Ok(())
    };
    let ended = |lane: &mut Lane| {
        until(|| {
            assert_eq!(lane.take_listed(), None, "a run ahead gives nothing back");
            !lane.listing_ahead()
        });
    };
    let mut lane = Lane::windowed_for_test(reach, test_arm());
    assert!(!lane.take_listed_ahead(), "the control: nothing ran ahead");
    lane.list_ahead(nudge());
    assert!(lane.listing_ahead(), "started before the moment");
    assert_eq!(lane.listing(), None, "no hand-over waits on it");
    lane.list_ahead(nudge());
    ended(&mut lane);
    assert_eq!(RUNS.load(Ordering::SeqCst), 1, "one run");
    lane.list_ahead(nudge());
    assert!(!lane.listing_ahead(), "Terminal is listed: no run");
    assert!(lane.take_listed_ahead(), "kept for the hand-over");
    assert!(!lane.take_listed_ahead(), "spent once");

    lane.list_ahead(nudge());
    assert!(lane.start_listing(Listing::HandOver(Origin::Automatic), nudge()));
    assert_eq!(lane.listing(), Some(Listing::HandOver(Origin::Automatic)));
    let mut taken = None;
    until(|| {
        taken = lane.take_listed();
        taken.is_some()
    });
    assert_eq!(
        taken,
        Some((
            Listing::HandOver(Origin::Automatic),
            Ok(()),
            Waiting::default()
        )),
        "the ask that rode it takes its end"
    );
    assert!(!lane.take_listed_ahead(), "and nothing is kept");
    assert_eq!(RUNS.load(Ordering::SeqCst), 2, "no run of its own");
}

#[test]
fn a_stronger_ask_riding_a_listing_takes_its_end_and_presses_wait_on_it() {
    let view = crate::tab_model::ViewId::from_stored(7);
    let wid = crate::WindowId(3);
    let mut lane = Lane::windowed_for_test(test_reach(), test_arm());
    lane.wait_in_settings([(wid, view)]);
    assert_eq!(lane.waiting, Waiting::default(), "no run out: nobody waits");
    assert!(lane.start_listing(Listing::HandOver(Origin::Automatic), nudge()));
    assert!(lane.start_listing(Listing::HandOver(Origin::Digest), nudge()));
    assert_eq!(
        lane.listing(),
        Some(Listing::HandOver(Origin::Digest)),
        "the owner's press outranks the window's own"
    );
    assert!(lane.start_listing(Listing::HandOver(Origin::Ctl), nudge()));
    assert!(lane.start_listing(Listing::ShowList, nudge()));
    assert_eq!(
        lane.listing(),
        Some(Listing::HandOver(Origin::Digest)),
        "weaker asks ride without taking it over"
    );
    assert!(lane.start_listing(Listing::HandOver(Origin::Settings), nudge()));
    assert_eq!(
        lane.listing(),
        Some(Listing::HandOver(Origin::Digest)),
        "an equal one rides too: the first press's panel"
    );
    lane.wait_in_settings([(wid, view)]);
    let mut taken = None;
    until(|| {
        taken = lane.take_listed();
        taken.is_some()
    });
    assert_eq!(
        taken,
        Some((
            Listing::HandOver(Origin::Digest),
            Ok(()),
            Waiting {
                digest: true,
                settings: vec![(wid, view)],
            }
        ))
    );
    assert_eq!(lane.waiting, Waiting::default(), "taken with the run");
    assert!(lane.start_listing(Listing::HandOver(Origin::Settings), nudge()));
    assert!(lane.start_listing(Listing::HandOver(Origin::Digest), nudge()));
    assert_eq!(lane.listing(), Some(Listing::HandOver(Origin::Settings)));
    let mut taken = None;
    until(|| {
        taken = lane.take_listed();
        taken.is_some()
    });
    assert!(taken.is_some_and(|(_, _, waiting)| waiting.digest));
    assert!(Listing::ShowList.rank() < Listing::HandOver(Origin::Automatic).rank());
    assert!(Listing::HandOver(Origin::Ctl).rank() < Listing::HandOver(Origin::Settings).rank());
}

#[test]
fn the_panel_asks_whether_system_settings_is_in_front_once_per_tick() {
    thread_local! {
        static ASKED: std::cell::Cell<u32> = const { std::cell::Cell::new(0) };
    }
    fn asked() -> u32 {
        ASKED.with(std::cell::Cell::get)
    }
    let arm = DevToolsHelperArm {
        settings_frontmost: || {
            ASKED.with(|c| c.set(c.get() + 1));
            false
        },
        ..test_arm()
    };
    let mut lane = Lane::windowed_for_test(test_reach(), arm);
    let wake = nudge();
    let t0 = Instant::now();
    assert!(lane.show(show(absent(2), None), t0, Arc::clone(&wake)));
    let gate = Some(ProbeGate::on());
    let _ = lane.tick_panel(t0, gate, &wake);
    assert_eq!(asked(), 0);
    let t1 = t0 + PANEL_TICK;
    for k in 0..200_u32 {
        let _ = lane.tick_panel(t1 + Duration::from_millis(u64::from(k)), gate, &wake);
    }
    assert_eq!(
        asked(),
        1,
        "two hundred loop turns inside one tick: one look"
    );
    let _ = lane.tick_panel(t1 + PANEL_TICK, gate, &wake);
    assert_eq!(asked(), 2, "the next tick looks again");
    events_of(&lane).post(PanelEvent::DragEnded { accepted: true });
    let _ = lane.tick_panel(t1 + PANEL_TICK + Duration::from_millis(10), gate, &wake);
    assert_eq!(asked(), 3, "a drop's read is not held to the cadence");
    assert!(
        lane.panel().is_some_and(|p| !p.read_asked()),
        "not in front: the drop's read falls back to the cadence"
    );
    let _ = lane.tick_panel(t1 + PANEL_TICK + Duration::from_millis(20), gate, &wake);
    assert_eq!(asked(), 3, "and asks no more until the next tick");
}

#[test]
fn the_decline_marker_is_read_once_and_again_after_a_not_now() {
    thread_local! {
        static READS: std::cell::Cell<u32> = const { std::cell::Cell::new(0) };
    }
    fn reads() -> u32 {
        READS.with(std::cell::Cell::get)
    }
    let reach = Reach {
        declined: || {
            READS.with(|c| c.set(c.get() + 1));
            Some(test_arms::DECLINED)
        },
        ..test_arms::opening()
    };
    let mut lane = Lane::windowed_for_test(reach, test_arm());
    for _ in 0..3 {
        assert_eq!(lane.declined_marker(), Some(test_arms::DECLINED));
    }
    assert_eq!(reads(), 1, "once, not per park");
    lane.forget_opened();
    let _ = lane.declined_marker();
    assert_eq!(reads(), 2, "a focus gain reads it again");
    let wake = nudge();
    let t0 = Instant::now();
    assert!(lane.show(show(absent(2), None), t0, Arc::clone(&wake)));
    events_of(&lane).post(PanelEvent::NotNow);
    let _ = lane.tick_panel(t0, None, &wake);
    assert!(lane.panel().is_none(), "Not now closes it");
    let _ = lane.declined_marker();
    let _ = lane.declined_marker();
    assert_eq!(reads(), 3, "the lane's own decline: read again, once");
}

fn events_of(lane: &Lane) -> PanelEvents {
    PanelEvents {
        panel: lane.generation,
        shared: Arc::clone(&lane.shared),
        wake: nudge(),
    }
}
