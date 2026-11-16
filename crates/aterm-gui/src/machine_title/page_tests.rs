// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

use aterm_agent::machine_name::file;
use aterm_types::machine_name::Class;

use super::fixture::{Names, found, named, nick, num, publish_all, rename, snapshot};
use super::*;
use crate::machine_title::NameUnset;

fn save(rev: u64, number: Option<&str>, nick: Option<&str>) -> MachineRequest {
    MachineRequest::Save {
        rev,
        number: number.map(str::to_owned),
        nick: nick.map(str::to_owned),
    }
}

/// Each request maps to the write Rename This Mac…'s judges make of it: a nickname alone keeps
/// the number, a number moves it, an unchanged seeded number is saved as chosen, a pending
/// chosen one synchronizes, an untouched quiet one writes nothing, New Random Name draws, and
/// Undo Last Rename takes back the newest change the read showed, its number left to the writer
/// where the vault's labels were not read. Each mapping has a control a wrong one would fail.
#[test]
fn page_change_maps_each_request() {
    let present = Names::authorized(Ok(true));
    let seeded = named(Class::Seeded, 7, "Heron");
    let read = snapshot(1, found(&seeded, &[], &present));
    assert!(matches!(
        page_change(&save(1, None, Some("Kestrel")), &read),
        PageWrite::Change(NameChange::Nickname(NickTo::Word(word))) if word == nick("Kestrel")
    ));
    assert!(matches!(
        page_change(&save(1, Some("m14"), None), &read),
        PageWrite::Change(NameChange::Name(to)) if to.num == num(14) && to.nick.is_none()
    ));
    assert!(matches!(
        page_change(&save(1, Some("m14"), Some("Kestrel")), &read),
        PageWrite::Change(NameChange::Name(to)) if to.num == num(14) && to.nick == Some(nick("Kestrel"))
    ));
    assert!(matches!(
        page_change(&save(1, None, None), &read),
        PageWrite::Change(NameChange::Name(to)) if to.num == num(7) && to.nick.is_none()
    ));
    assert!(matches!(
        page_change(&MachineRequest::Random { rev: 1 }, &read),
        PageWrite::Change(NameChange::Nickname(NickTo::Random))
    ));
    // The page refuses with the field's own live line: one judge, one sentence.
    let field = verdict(
        &read.snapshot,
        &read.held,
        &Field::open(Some("m086"), read.held.number_text()),
        &Field::open(None, &read.held.word),
    );
    assert!(!field.renames);
    assert!(
        matches!(page_change(&save(1, Some("m086"), None), &read), PageWrite::Refused(why) if why == field.text),
        "control: what the field refuses is no write"
    );
    assert!(
        matches!(
            page_change(&MachineRequest::Sync { rev: 1 }, &read),
            PageWrite::Refused(_)
        ),
        "control: a seeded number has nothing to synchronize"
    );

    let chosen = named(Class::Chosen, 86, "Heron");
    let pending = snapshot(1, found(&chosen, &[], &present));
    for asked in [save(1, None, None), MachineRequest::Sync { rev: 1 }] {
        assert!(
            matches!(
                page_change(&asked, &pending),
                PageWrite::Change(NameChange::Synchronize(number)) if number == num(86)
            ),
            "{asked:?}"
        );
    }
    assert!(
        matches!(
            page_change(&MachineRequest::Authorize { rev: 1 }, &pending),
            PageWrite::Refused(_)
        ),
        "control: a present permission needs no authorizing"
    );
    let missing = Names::authorized(Ok(false));
    let unauthorized = snapshot(1, found(&chosen, &[], &missing));
    for asked in [save(1, None, None), MachineRequest::Authorize { rev: 1 }] {
        assert!(
            matches!(page_change(&asked, &unauthorized), PageWrite::Authorize),
            "{asked:?}"
        );
    }
    let unknown = Names::authorized(Err("not readable here".to_owned()));
    let unknown = snapshot(1, found(&chosen, &[], &unknown));
    assert!(matches!(
        page_change(&MachineRequest::Authorize { rev: 1 }, &unknown),
        PageWrite::Authorize
    ));

    publish_all(&chosen, &present);
    let quiet = snapshot(1, found(&chosen, &[], &present));
    for asked in [
        save(1, None, None),
        save(1, Some("m86"), Some("Heron")),
        MachineRequest::Sync { rev: 1 },
        MachineRequest::Read,
    ] {
        assert!(
            matches!(page_change(&asked, &quiet), PageWrite::Refused(_)),
            "{asked:?}"
        );
    }

    // Undo Last Rename takes back the newest change the read showed, by that change.
    let renamed = named(Class::Chosen, 7, "Heron");
    rename(&renamed, Class::Chosen, 8, "Puffin");
    let read = snapshot(1, found(&renamed, &[], &present));
    let newest = file::read(&renamed.state)
        .expect("reads")
        .expect("a name")
        .history[0]
        .clone();
    assert!(matches!(
        page_change(&MachineRequest::Undo { rev: 1 }, &read),
        PageWrite::Change(NameChange::Undo(seen)) if seen == newest
    ));
    // A number the vault's labels were not read for is the writer's to check, as Save's is.
    let unread = snapshot(1, super::fixture::found_unread_labels(&renamed));
    assert!(matches!(
        page_change(&MachineRequest::Undo { rev: 1 }, &unread),
        PageWrite::Change(NameChange::Undo(seen)) if seen == newest
    ));
    let first = snapshot(1, found(&named(Class::Chosen, 7, "Heron"), &[], &present));
    assert_eq!(first.undo.seen, None, "control: a first number has no undo");
    assert!(
        matches!(
            page_change(&MachineRequest::Undo { rev: 1 }, &first),
            PageWrite::Refused(why) if why == first.undo.line
        ),
        "control: an unavailable undo is refused in the words the page showed"
    );
}

/// A write names the read it was pressed over; one the page has since replaced is refused.
/// NEGATIVE CONTROL: the current read's own write passes.
#[test]
fn a_stale_rev_is_refused() {
    let present = Names::authorized(Ok(true));
    let seeded = named(Class::Seeded, 7, "Heron");
    let read = snapshot(2, found(&seeded, &[], &present));
    for asked in [
        save(1, None, Some("Kestrel")),
        MachineRequest::Random { rev: 1 },
        MachineRequest::Sync { rev: 1 },
        MachineRequest::Authorize { rev: 3 },
        MachineRequest::Undo { rev: 1 },
    ] {
        assert!(
            matches!(page_change(&asked, &read), PageWrite::Refused(why) if why == STALE),
            "{asked:?}"
        );
    }
    assert!(matches!(
        page_change(&save(2, None, Some("Kestrel")), &read),
        PageWrite::Change(_)
    ));
}

/// The page's Save and New Random Name, each resolved against the read the page showed
/// ([`page_change`]) and set through the name writer's dispatch ([`write_change`]), record a
/// History entry made by a person on this node: Save as `named`, New Random Name as `drawn`.
/// The first write mints this Mac's node key, so the read after it names the node in its Node
/// fact. NEGATIVE CONTROL: the read before any write names no node.
#[test]
fn a_page_write_is_the_persons_on_this_node_in_history() {
    use crate::machine_title::write_change;
    use aterm_agent::machine_name::file::{self, Why};
    use aterm_net::fleet_member::name_record::{By, ByKind, node_of};
    use aterm_secrets::client::LabLabels;
    let names = Names::authorized(Ok(true));
    let seeded = named(Class::Seeded, 7, "Heron");
    let node_fact = |read: &PageSnapshot| {
        read.facts
            .iter()
            .find(|(fact, _)| fact == "Node")
            .map(|(_, shown)| shown.clone())
            .expect("a read with a name shows its Node")
    };
    let write = |read: &PageSnapshot, asked: &MachineRequest, at: u64| {
        let PageWrite::Change(change) = page_change(asked, read) else {
            panic!("{asked:?} writes over {read:?}");
        };
        write_change(
            &seeded.state,
            &seeded.config,
            &change,
            &names,
            || Ok(LabLabels::default()),
            || Ok(None),
            at,
        )
        .unwrap_or_else(|why| panic!("{asked:?} is set: {why}"));
    };

    let before = snapshot(1, found(&seeded, &[], &names));
    assert_eq!(
        before.snapshot.preview().and_then(|read| read.node()),
        None,
        "control: a Mac with no node key"
    );
    assert!(node_of(&node_fact(&before)).is_none());

    write(&before, &save(1, None, None), super::fixture::NOW + 2);
    let me = aterm_net::fleet_member::read_node_identity(&seeded.state)
        .expect("reads the node key")
        .expect("the page's Save minted this Mac's node key");
    let by = Some(By {
        node: node_of(me.node()).expect("a node id"),
        kind: ByKind::Person,
    });
    let saved = file::read(&seeded.state).unwrap().expect("a name");
    assert_eq!(
        (saved.history[0].why, saved.history[0].by),
        (Why::Named, by)
    );
    let after = snapshot(2, found(&seeded, &[], &names));
    let node = after.snapshot.preview().and_then(|read| read.node());
    assert_eq!(node, by.map(|by| by.node), "the read names the node minted");
    assert_eq!(Some(node_fact(&after)), node.map(|node| node.to_string()));

    write(
        &after,
        &MachineRequest::Random { rev: 2 },
        super::fixture::NOW + 3,
    );
    let drawn = file::read(&seeded.state).unwrap().expect("a name");
    assert_eq!(
        (drawn.history[0].why, drawn.history[0].by),
        (Why::Drawn, by)
    );
    assert_eq!(
        drawn.history[1], saved.history[0],
        "the Save's entry stays below it"
    );
}

/// A page write's failure is the page's notice, and the Rename panel's next press shows
/// nothing of it; one a later page write superseded is dropped.
/// NEGATIVE CONTROL: a panel write's failure is still kept for the panel, not the page.
#[test]
fn a_failed_page_save_sets_the_page_notice_not_the_panels() {
    let mut app = crate::App::headless_for_test();
    app.machine_page.sets = 2;
    app.machine_page_not_set(1, "Last attempt: superseded".to_owned());
    assert_eq!(app.machine_page.state.notice, None);
    app.machine_page_not_set(2, "Last attempt: the lock is busy".to_owned());
    assert_eq!(
        app.machine_page.state.notice.as_deref(),
        Some("Last attempt: the lock is busy")
    );
    assert_eq!(
        app.machine_unset, None,
        "the panel keeps nothing of the page's"
    );

    app.machine_sets = 1;
    let unset = NameUnset {
        generation: 1,
        asked: crate::machine_title::Asked {
            number: Some("m14".to_owned()),
            nick: None,
        },
        why: "Last attempt: the panel's lock is busy".to_owned(),
    };
    app.machine_name_not_set(unset.clone());
    assert_eq!(app.machine_unset, Some(unset));
    assert_eq!(
        app.machine_page.state.notice.as_deref(),
        Some("Last attempt: the lock is busy"),
        "the page keeps nothing of the panel's"
    );
}

/// Only the newest read asked for lands: read 1 after read 2 was asked for is dropped.
#[test]
fn a_page_read_lands_only_for_its_newest_request() {
    let present = Names::authorized(Ok(true));
    let seeded = named(Class::Seeded, 7, "Heron");
    let mut app = crate::App::headless_for_test();
    app.machine_page.reads = 2;
    app.machine_page.state.busy = true;
    app.machine_page_read(1, found(&seeded, &[], &present));
    assert!(matches!(app.machine_page.state.read, PageRead::Reading));
    assert!(
        app.machine_page.state.busy,
        "a superseded read settles nothing"
    );
    app.machine_page_read(2, found(&seeded, &[], &present));
    assert!(matches!(&app.machine_page.state.read, PageRead::Read(read) if read.rev == 2));
    assert!(!app.machine_page.state.busy);
}

/// A read that failed keeps the editor over the last name shown, judged as unread, and says
/// why; with no name ever shown it says why alone.
#[test]
fn a_failed_read_keeps_the_last_name_shown() {
    let present = Names::authorized(Ok(true));
    let seeded = named(Class::Seeded, 7, "Heron");
    let unread = || PageFound {
        snapshot: Snapshot::Unread("the books are locked".to_owned()),
        fleet: None,
    };
    assert!(matches!(
        page_read(1, unread(), None, None),
        PageRead::Unread(why) if why == "the books are locked"
    ));
    let shown = snapshot(1, found(&seeded, &[], &present));
    let PageRead::Read(kept) = page_read(2, unread(), None, Some(&shown)) else {
        panic!("the editor stays");
    };
    assert_eq!(kept.held.name, "m7");
    assert!(!kept.held.read, "judged as the view, not a read");
    assert!(
        kept.facts
            .iter()
            .any(|(_, fact)| fact.contains("the books are locked"))
    );
}

/// The page shows this Mac's identity, so only the owner drives it while it is in front.
/// NEGATIVE CONTROL: another Settings page asks only config-write authority.
#[test]
fn this_mac_is_an_owner_only_front_surface() {
    use crate::control::FrontControlSurface;
    use crate::native_settings::SettingsRoute;
    let mut app = crate::App::headless_for_test();
    assert!(app.open_settings_tab(SettingsRoute::Machine));
    assert_eq!(
        app.front_control_surface(),
        FrontControlSurface::OwnerSettings
    );
    assert!(app.open_settings_tab(SettingsRoute::Terminal));
    assert_eq!(
        app.front_control_surface(),
        FrontControlSurface::NativeSettings
    );
}

/// A Settings view holding the page reads again at each title read, on screen or not: a page in
/// a covered window (or a background tab) shows the new name when it comes back to the front.
/// NEGATIVE CONTROL: a Settings tab on another page reads nothing.
#[test]
fn a_held_page_follows_a_machine_name_wake_on_screen_or_not() {
    use crate::native_settings::SettingsRoute;
    let wid = crate::WindowId(0);
    let view = || crate::machine_title::MachineView {
        rev: 3,
        name: "m7".to_owned(),
        nick: "Heron".to_owned(),
        word: "Heron".to_owned(),
        fleet: InFleet::Out,
    };
    let mut app = crate::App::headless_for_test();
    assert!(app.open_settings_tab(SettingsRoute::Machine));
    assert_eq!(app.machine_page.reads, 1, "opening reads once");
    app.machine_name_read(view());
    assert_eq!(app.machine_page.reads, 2, "on screen");
    app.windows.get_mut(&wid).expect("a window").occluded = true;
    assert!(!app.view_on_screen(|view| app.machine_page_in(view)));
    app.machine_name_read(view());
    assert_eq!(app.machine_page.reads, 3, "covered, it still follows");

    let mut app = crate::App::headless_for_test();
    assert!(app.open_settings_tab(SettingsRoute::Terminal));
    app.machine_name_read(view());
    assert_eq!(app.machine_page.reads, 0);
}

/// A pointer's press is bound to the read its frame was drawn over. A Save drawn over read 1,
/// pressed after read 2 landed and before any frame showed it, no longer matches the live tree,
/// so dispatch drops it; sent past that check, the host refuses it as stale. Either way the
/// read the person never saw is not written.
/// NEGATIVE CONTROL: the Save drawn over read 2 reaches the host's writer step.
#[test]
fn a_press_over_a_frame_drawn_before_a_newer_read_writes_nothing() {
    use crate::native_app::{ActionInvocation, AppEvent, EventResult};
    use crate::native_settings::SettingsRoute;
    use crate::native_ui::UiKey;
    let wid = crate::WindowId(0);
    let present = Names::authorized(Ok(true));
    let seeded = named(Class::Seeded, 7, "Heron");
    let mut app = crate::App::headless_for_test();
    // Tall enough that the page's actions stay on screen under a notice.
    if let Some(mut window) = app.windows.get_mut(&wid) {
        window.cols = 160;
        window.rows = 120;
    }
    assert!(app.open_settings_tab(SettingsRoute::Machine));
    let seq = app.machine_page.reads;
    app.machine_page_read(seq, found(&seeded, &[], &present));
    let primary = UiKey::new("name/primary");
    let drawn = |app: &crate::App| {
        let compiled = app.compiled_native_ui(wid).unwrap();
        compiled
            .semantic(&primary)
            .filter(|node| node.state.is_none_or(|state| state.enabled))
            .and_then(|node| node.action.clone())
            .unwrap_or_else(|| {
                let drawn = compiled
                    .semantics
                    .iter()
                    .map(|node| (node.key.as_str(), node.action.as_ref(), node.state))
                    .collect::<Vec<_>>();
                panic!("no available primary: {drawn:?}")
            })
    };
    let first = drawn(&app);
    rename(&seeded, Class::Seeded, 12, "Heron");
    app.machine_page.reads += 1;
    let seq = app.machine_page.reads;
    app.machine_page_read(seq, found(&seeded, &[], &present));
    let (_, view) = app.active_native_view(wid).expect("Settings");
    let press = |app: &mut crate::App, id| {
        app.dispatch_native_control_action(
            wid,
            view,
            primary.clone(),
            ActionInvocation { id, value: None },
            false,
        )
    };
    assert!(
        press(&mut app, first.clone()).is_err(),
        "the frame's press no longer matches the live tree"
    );
    assert_eq!(
        app.machine_page.state.notice, None,
        "nothing reached the host"
    );
    let _ = app.dispatch_native_view_event(
        wid,
        view,
        AppEvent::Action(ActionInvocation {
            id: first.clone(),
            value: None,
        }),
    );
    assert_eq!(app.machine_page.state.notice.as_deref(), Some(STALE));

    let second = drawn(&app);
    assert_ne!(second, first);
    assert_eq!(press(&mut app, second), Ok(EventResult::Handled));
    assert_eq!(
        app.machine_page.state.notice,
        Some(format!("Last attempt: {NO_WRITER}")),
        "the current read's Save reached the writer step"
    );
}

/// A write's failure and its pending publication go back to the surface that asked, each as
/// that surface's own write: the panel keeps it with what was asked, the page shows it. A
/// pending macOS step tells the page, which cannot retry it, that Rename This Mac… does.
/// NEGATIVE CONTROL: each origin's wake is not the other's, and the panel's pending notice
/// names no other surface.
#[test]
fn a_failure_goes_back_to_the_surface_that_asked() {
    use crate::Wake;
    use crate::machine_title::{Asked, not_set_wake, pending_wake};
    use aterm_agent::machine_name::publish::Pending;
    let asked = Asked {
        number: Some("m14".to_owned()),
        nick: None,
    };
    let why = || "Last attempt: the lock is busy".to_owned();
    assert!(matches!(
        not_set_wake(Origin::Panel, 4, &asked, why()),
        Wake::MachineNameNotSet(unset)
            if unset.generation == 4 && unset.asked == asked && unset.why == why()
    ));
    assert!(matches!(
        not_set_wake(Origin::Page, 4, &asked, why()),
        Wake::MachinePage(PageWake::NotSet(4, text)) if text == why()
    ));
    assert!(!matches!(
        not_set_wake(Origin::Page, 4, &asked, why()),
        Wake::MachineNameNotSet(_)
    ));
    assert!(!matches!(
        not_set_wake(Origin::Panel, 4, &asked, why()),
        Wake::MachinePage(_)
    ));

    let names = Pending::SystemNames("scutil was refused".to_owned());
    assert!(matches!(
        pending_wake(Origin::Page, 5, &names, None),
        Wake::MachinePage(PageWake::NotSet(5, text))
            if text.contains("scutil was refused") && text.ends_with(RETRIED_IN_THE_PANEL)
    ));
    assert!(matches!(
        pending_wake(Origin::Panel, 5, &names, None),
        Wake::MachineNameNotSet(unset)
            if unset.why.contains("scutil was refused")
                && !unset.why.contains(RETRIED_IN_THE_PANEL)
                && unset.asked.number.is_none()
                && unset.asked.nick.is_none()
    ));
    let policy = Pending::Policy("aterm.toml is locked".to_owned());
    assert!(
        matches!(
            pending_wake(Origin::Page, 6, &policy, None),
            Wake::MachinePage(PageWake::NotSet(6, text))
                if !text.contains(RETRIED_IN_THE_PANEL)
        ),
        "control: the page's own Retry Sync retries the policy"
    );
}

/// In a process that is not its account's own (a lab instance's private state root), the
/// registered macOS naming operation refuses the page's save and the panel's Retry Sync alike
/// (`publish::apply_registered`), so the page's notice says why and names no surface to retry
/// in. NEGATIVE CONTROL: the same pending in the account's own aterm names Rename This Mac….
#[test]
fn a_process_not_its_accounts_own_names_no_surface_to_retry_in() {
    use crate::Wake;
    use crate::machine_title::pending_wake;
    use aterm_agent::machine_name::publish::Pending;
    let not_own = aterm_types::dirs::not_own_account_with(true, 501, None, None);
    let why = not_own.expect("a private state root is not the account's own");
    // apply_registered's admission refusal, which synchronize_system_names reports as
    // macOS's names still pending.
    let refused = Pending::SystemNames(format!(
        "macOS naming is left to this account's own aterm: {why}"
    ));
    assert!(
        matches!(
            pending_wake(Origin::Page, 7, &refused, not_own),
            Wake::MachinePage(PageWake::NotSet(7, text))
                if text.contains(why) && !text.contains(RETRIED_IN_THE_PANEL)
        ),
        "a surface that is refused the same way is not named"
    );
    assert!(
        matches!(
            pending_wake(Origin::Page, 7, &refused, None),
            Wake::MachinePage(PageWake::NotSet(7, text)) if text.ends_with(RETRIED_IN_THE_PANEL)
        ),
        "control: the account's own aterm names the panel"
    );
}

/// Authorize Naming…'s result goes back to the surface whose press opened it, even where both
/// surfaces' counters stand at the same write.
/// NEGATIVE CONTROL: the panel's result reaches the panel and leaves the page's notice alone.
#[test]
fn an_authorization_result_goes_back_to_the_surface_that_asked() {
    let mut app = crate::App::headless_for_test();
    app.machine_page.sets = 1;
    app.machine_sets = 1;
    app.machine_name_authorization_finished(Origin::Page, 1, Err("cancelled".to_owned()));
    assert_eq!(
        app.machine_page.state.notice.as_deref(),
        Some("Last attempt: cancelled")
    );
    assert_eq!(
        app.machine_unset, None,
        "the panel keeps nothing of the page's"
    );

    let mut app = crate::App::headless_for_test();
    app.machine_page.sets = 1;
    app.machine_sets = 1;
    app.machine_name_authorization_finished(Origin::Panel, 1, Err("cancelled".to_owned()));
    assert_eq!(
        app.machine_unset.as_ref().map(|unset| unset.why.as_str()),
        Some("Last attempt: cancelled")
    );
    assert_eq!(
        app.machine_page.state.notice, None,
        "the page keeps nothing of the panel's"
    );
}

/// The page offers the undo and the next free numbers `aterm name` takes from the same books,
/// both ways: Undo Last Rename names the title `undo_last`'s dry run over the change it showed
/// leaves, and a number the vault holds names exactly the next free numbers the read finds, each
/// of which the set takes, while the set refuses the held number with the line's own refusal.
/// NEGATIVE CONTROL: the held number is not among those named, and the set refuses it.
#[test]
fn the_page_offers_the_undo_and_free_numbers_aterm_name_takes() {
    use super::fixture::{NOW, labels};
    use aterm_agent::machine_name::{NameTo, Refused, next_free_words, set_name, undo_last};
    use aterm_net::fleet_member::name_record::ByKind;
    let present = Names::authorized(Ok(true));
    let renamed = named(Class::Chosen, 7, "Heron");
    rename(&renamed, Class::Chosen, 8, "Puffin");
    let read = snapshot(1, found(&renamed, &[], &present));
    let seen = read.undo.seen.as_ref().expect("an undo is offered");
    let undone = undo_last(
        &renamed.state,
        Some(seen),
        || Ok(labels(&[])),
        ByKind::Person,
        || Ok(None),
        NOW + 2,
        true,
    )
    .expect("the undo's dry run");
    assert!(
        read.undo.line.contains(&undone.name.title()),
        "{:?} names {:?}",
        read.undo.line,
        undone.name.title()
    );

    let scratch = named(Class::Chosen, 7, "Heron");
    let read = snapshot(1, found(&scratch, &[20], &present));
    let preview = read.snapshot.preview().expect("a read with a name");
    let refused = preview.judge(num(20)).expect_err("the vault holds m20");
    let free = preview.next_free(num(20), &refused);
    assert!(!free.is_empty());
    assert!(
        !free.contains(&num(20)),
        "control: the held number is not named"
    );
    let line = verdict(
        &read.snapshot,
        &read.held,
        &Field::open(Some("m20"), read.held.number_text()),
        &Field::open(None, &read.held.word),
    );
    assert!(!line.renames);
    assert_eq!(
        line.text,
        format!(
            "{} {}",
            sentence(&refused.why),
            sentence(&next_free_words(&free).expect("words for the free numbers"))
        )
    );
    let set = |n: u32| {
        set_name(
            &scratch.state,
            &NameTo {
                num: num(n),
                nick: None,
            },
            || Ok(labels(&[20])),
            ByKind::Person,
            || Ok(None),
            NOW + 2,
            true,
        )
    };
    for taken in &free {
        set(taken.get()).unwrap_or_else(|why| panic!("{taken} is free: {why}"));
    }
    let held = set(20).expect_err("control: the set refuses the held number");
    assert_eq!(Refused::of(&held), Some(&refused));
}

/// History on the page is the name file's History, one `aterm name history` line per change,
/// newest first. NEGATIVE CONTROL: a failed read that keeps the editor over the name shown
/// before lists no History and offers no undo, since it read no name.
#[test]
fn the_pages_history_is_the_files() {
    use aterm_agent::machine_name::file::Change;
    let present = Names::authorized(Ok(true));
    let renamed = named(Class::Chosen, 7, "Heron");
    rename(&renamed, Class::Chosen, 8, "Puffin");
    let read = snapshot(1, found(&renamed, &[], &present));
    let kept = file::read(&renamed.state)
        .expect("reads")
        .expect("a name")
        .history;
    assert_eq!(kept.len(), 3, "the first name, m7 Heron and m8 Puffin");
    assert_eq!(
        read.history,
        Some(kept.iter().map(Change::line).collect::<Vec<_>>())
    );

    let unread = PageFound {
        snapshot: Snapshot::Unread("the books are locked".to_owned()),
        fleet: None,
    };
    let PageRead::Read(failed) = page_read(2, unread, None, Some(&read)) else {
        panic!("the editor stays");
    };
    assert_eq!(failed.history, None);
    assert_eq!(failed.undo.seen, None);
}
