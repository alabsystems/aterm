// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

//! Settings ▸ This Mac's page (`native_settings_machine.rs`), driven through the Settings
//! controller and read back from the compiled semantic tree.

use super::*;
use crate::native_app::MachineRequest;
#[cfg(target_os = "macos")]
use crate::native_settings::machine_page::{
    NAME_AUTHORIZE, NAME_AUTHORIZE_BESIDE, NAME_FIELD_NICK, NAME_RANDOM, NAME_REVERT, NAME_SAVE,
    NAME_SYNC, NAME_UNDO,
};
use crate::native_settings::machine_page::{NAME_FIELD_NUMBER, NAME_PRIMARY, NAME_REFRESH};

type Route = (
    NativeRuntime,
    crate::native_app::AppInstanceId,
    crate::native_app::ViewId,
);

/// The requests `effects` asks of the host's name writer.
fn asked(effects: &[AppEffect]) -> Vec<MachineRequest> {
    effects
        .iter()
        .filter_map(|effect| match effect {
            AppEffect::Machine(request) => Some(request.clone()),
            _ => None,
        })
        .collect()
}

/// Publish `state` as the host does.
fn publish((runtime, instance, view): &mut Route, state: MachinePageState, revision: u64) {
    assert!(runtime.replace_settings_machine(state, revision));
    runtime
        .dispatch(*instance, *view, AppEvent::MachineChanged { revision })
        .unwrap();
}

/// A Settings view opened on This Mac, which asks for its one read, then shows `state`.
fn machine_route(state: MachinePageState) -> Route {
    let (mut runtime, instance, view) = setup();
    let outcome = runtime
        .dispatch(
            instance,
            view,
            AppEvent::Action(ActionInvocation {
                id: route_action(SettingsRoute::Machine),
                value: None,
            }),
        )
        .unwrap();
    assert_eq!(asked(&outcome.effects), [MachineRequest::Read]);
    let mut route = (runtime, instance, view);
    publish(&mut route, state, 1);
    route
}

/// Act on `action` with `value` as text, as an accessibility client or `act` sets a field: the
/// requests that asks of the host.
fn act(
    (runtime, instance, view): &mut Route,
    action: &str,
    value: Option<&str>,
) -> Vec<MachineRequest> {
    let outcome = runtime
        .dispatch(
            *instance,
            *view,
            AppEvent::Action(ActionInvocation {
                id: ActionId::new(action),
                value: value.map(|text| SemanticInput::Text(text.to_owned())),
            }),
        )
        .unwrap();
    asked(&outcome.effects)
}

/// The whole page, drawn tall enough that its list holds every card.
fn whole((runtime, instance, view): &Route) -> crate::native_ui::CompiledUi {
    compile_settings_view(runtime, *instance, *view, &view_cx_at(1_200.0, 4_000.0))
}

fn keys(compiled: &crate::native_ui::CompiledUi) -> Vec<String> {
    compiled
        .semantics
        .iter()
        .map(|node| node.key.as_str().to_owned())
        .filter(|key| key.starts_with("name/"))
        .collect()
}

fn unread(why: &str) -> MachinePageState {
    MachinePageState {
        read: PageRead::Unread(why.to_owned()),
        ..MachinePageState::default()
    }
}

/// Opening the page asks the host for one read; another route asks for none.
#[test]
fn this_mac_opens_and_asks_for_one_read() {
    let _ = machine_route(MachinePageState::default());
    let (mut runtime, instance, view) = setup();
    let outcome = runtime
        .dispatch(
            instance,
            view,
            AppEvent::Action(ActionInvocation {
                id: route_action(SettingsRoute::Terminal),
                value: None,
            }),
        )
        .unwrap();
    assert!(
        asked(&outcome.effects).is_empty(),
        "another route reads no name"
    );
}

/// However a view comes to the page it asks for one read: by the sidebar's arrow keys and by a
/// search hit, as by the route's own action. NEGATIVE CONTROL: arriving at another page, and
/// moving within this one, ask for none.
#[test]
fn every_way_onto_the_page_asks_for_one_read() {
    let (mut runtime, instance, view) = setup();
    let mut go = |event: AppEvent| {
        let outcome = runtime.dispatch(instance, view, event).unwrap();
        (
            asked(&outcome.effects),
            settings_state(&runtime, view).route,
        )
    };
    let route = |route: SettingsRoute| {
        AppEvent::Action(ActionInvocation {
            id: route_action(route),
            value: None,
        })
    };
    assert!(go(route(SettingsRoute::Terminal)).0.is_empty());
    assert!(
        go(AppEvent::FocusChanged(Some(UiKey::new(
            "settings/nav/terminal"
        ))))
        .0
        .is_empty()
    );
    // The sidebar: Terminal's next Workspace row is This Mac (`NAVIGATION_GROUPS`).
    assert_eq!(
        go(AppEvent::StepFocus(1)),
        (vec![MachineRequest::Read], SettingsRoute::Machine)
    );
    assert!(
        go(AppEvent::FocusChanged(Some(UiKey::new(NAME_FIELD_NUMBER))))
            .0
            .is_empty(),
        "moving within the page reads nothing"
    );
    assert!(
        go(AppEvent::FocusChanged(Some(UiKey::new(
            "settings/nav/machine"
        ))))
        .0
        .is_empty()
    );
    assert_eq!(
        go(AppEvent::StepFocus(1)),
        (vec![], SettingsRoute::Harness),
        "control: the next row reads no name"
    );
    assert_eq!(
        go(AppEvent::StepFocus(-1)),
        (vec![MachineRequest::Read], SettingsRoute::Machine)
    );
    // A search hit: Settings search lists the page by its words (`route_keywords`).
    assert!(go(route(SettingsRoute::Terminal)).0.is_empty());
    assert_eq!(
        go(AppEvent::Action(ActionInvocation {
            id: ActionId::new("settings/search/page/machine"),
            value: None,
        })),
        (vec![MachineRequest::Read], SettingsRoute::Machine)
    );
    assert_eq!(
        go(AppEvent::Action(ActionInvocation {
            id: ActionId::new("settings/search/page/terminal"),
            value: None,
        })),
        (vec![], SettingsRoute::Terminal),
        "control: a hit on another page reads no name"
    );
}

/// Settings search lists the page for what its cards hold beside the fields: the rename's undo,
/// the name's History and the next free numbers (`route_keywords`).
/// NEGATIVE CONTROL: words no page carries list none.
#[test]
fn search_finds_this_mac_by_its_undo_history_and_free_numbers() {
    let state = SettingsViewState::new(&Config::default());
    let lists = |query: &str| {
        search_page_hits(&state, SearchQuery::of(&state, query))
            .iter()
            .any(|hit| hit.route == SettingsRoute::Machine)
    };
    for query in ["undo rename", "name history", "free number"] {
        assert!(lists(query), "{query}");
    }
    assert!(!lists("zxqvbn"), "control: no page carries these words");
}

/// `/machine` and `/name` open the page, and so do its words where a section is named; a near
/// miss opens nothing.
#[test]
fn machine_and_name_paths_open_this_page() {
    assert_eq!(SettingsRoute::Machine.path(), "/machine");
    assert_eq!(
        SettingsRoute::from_path("/machine"),
        Some(SettingsRoute::Machine)
    );
    assert_eq!(
        SettingsRoute::from_path("/name"),
        Some(SettingsRoute::Machine)
    );
    assert_eq!(SettingsRoute::from_path("/names"), None);
    assert_eq!(SettingsRoute::from_path("machines!"), None);
    assert_eq!(SettingsRoute::Machine.label(), THIS_MACHINE);
}

/// Settings' page name is "This" and the OS's noun, title-cased, on every OS: "This Mac" names
/// no Mac off macOS, where the noun is "machine".
#[test]
fn the_page_name_is_this_and_the_os_noun() {
    let mut noun = aterm_types::machine_name::OS_NOUN.chars();
    let title: String = noun
        .next()
        .map(|first| first.to_uppercase().chain(noun).collect())
        .unwrap_or_default();
    assert_eq!(THIS_MACHINE, format!("This {title}"));
    assert_eq!(
        THIS_MACHINE.split_whitespace().any(|word| word == "Mac"),
        cfg!(target_os = "macos"),
        "only macOS names a Mac"
    );
}

/// While the read is out the page draws no field and no write: only Refresh.
/// NEGATIVE CONTROL: once a read lands, a free number typed into the field saves.
#[test]
fn a_reading_page_offers_no_save() {
    let mut route = machine_route(MachinePageState::default());
    let drawn = keys(&whole(&route));
    assert!(drawn.iter().any(|key| key == NAME_REFRESH), "{drawn:?}");
    assert!(
        !drawn
            .iter()
            .any(|key| key.starts_with("name/press/") || key.starts_with("name/field/")),
        "{drawn:?}"
    );
    assert!(act(&mut route, "name/press/save", None).is_empty());
    assert!(act(&mut route, NAME_FIELD_NUMBER, Some("m87")).is_empty());
    assert!(act(&mut route, "name/press/save", None).is_empty());
    assert_eq!(act(&mut route, NAME_REFRESH, None), [MachineRequest::Read]);
    #[cfg(target_os = "macos")]
    {
        use crate::machine_title::page::fixture::{Names, found, named, page};
        let scratch = named(aterm_types::machine_name::Class::Chosen, 7, "Heron");
        let names = Names::authorized(Ok(true));
        crate::machine_title::page::fixture::publish_all(&scratch, &names);
        publish(&mut route, page(2, found(&scratch, &[], &names), false), 2);
        assert!(
            act(&mut route, NAME_SAVE, None).is_empty(),
            "nothing typed, nothing pending"
        );
        assert!(act(&mut route, NAME_FIELD_NUMBER, Some("m87")).is_empty());
        assert_eq!(
            act(&mut route, NAME_SAVE, None),
            [MachineRequest::Save {
                rev: 2,
                number: Some("m87".to_owned()),
                nick: None
            }]
        );
    }
}

/// Every state but a read draws whole, with what it is waiting on or why it cannot read, at
/// any size; a read that failed says why.
#[test]
fn an_unread_page_says_why_and_offers_a_refresh() {
    let route = machine_route(unread("the books are locked"));
    let drawn = whole(&route);
    let status = drawn
        .semantics
        .iter()
        .find(|node| node.label.contains("the books are locked"))
        .expect("the page says why");
    assert!(
        status.key.as_str().starts_with("name/facts"),
        "{:?}",
        status.key
    );
    assert!(drawn.semantic(&UiKey::new(NAME_REFRESH)).is_some());
}

/// Off macOS the page says what changes the name and draws no field or write: no field, no
/// primary (whose stable key is `name/primary`), no Authorize Naming… (`name/authorize`), and no
/// control whose action is a `name/press/` write, whatever its key.
#[cfg(not(target_os = "macos"))]
#[test]
fn off_macos_the_page_offers_no_write() {
    let route = machine_route(MachinePageState {
        read: PageRead::Unsupported("`aterm name` in a terminal changes it"),
        ..MachinePageState::default()
    });
    let compiled = whole(&route);
    let drawn = keys(&compiled);
    assert!(
        drawn.iter().any(|key| key.starts_with("name/facts")),
        "{drawn:?}"
    );
    assert!(
        !drawn.iter().any(|key| key == NAME_PRIMARY
            || key == "name/authorize"
            || key.starts_with("name/press/")
            || key.starts_with("name/field/")),
        "{drawn:?}"
    );
    let writes = compiled
        .semantics
        .iter()
        .filter_map(|node| node.action.as_ref())
        .filter(|action| {
            action
                .as_str()
                .starts_with(crate::native_settings::NAME_PRESS_PREFIX)
        })
        .collect::<Vec<_>>();
    assert!(writes.is_empty(), "{writes:?}");
    assert!(
        !SettingsRoute::Machine
            .label()
            .split_whitespace()
            .any(|word| word == "Mac"),
        "no Mac off macOS"
    );
}

#[cfg(target_os = "macos")]
mod read {
    use super::*;
    use crate::machine_rename_sheet::Field;
    use crate::machine_title::page::fixture::{
        Names, Scratch, WIDEST_NICK, found, found_local, found_unread_labels, found_with_history,
        held_by_older_build, named, page, publish_all, rename, renamed, snapshot, with_node,
    };
    use crate::machine_title::page::{PageFound, PageHistory};
    use aterm_agent::machine_name::history::{self, History, NO_HISTORY, NO_ROW, PAGE_ROWS};
    use aterm_types::machine_name::Class;

    /// A name renamed twice by a person, m7 Heron then m8 Puffin, and the History its registry
    /// row would hold, so Undo Last Rename has m7 Heron to give back.
    fn renamed_twice() -> (Scratch, History) {
        renamed(&[(7, "Heron"), (8, "Puffin")])
    }

    /// The words node `key` says, whole.
    fn label_of(drawn: &crate::native_ui::CompiledUi, key: &str) -> String {
        drawn
            .semantic(&UiKey::new(key))
            .unwrap_or_else(|| panic!("the page draws {key}"))
            .label
            .clone()
    }

    /// Undo Last Rename's button: its action as drawn, and whether it is available.
    fn undo_button(route: &Route) -> (String, bool) {
        let drawn = whole(route);
        let node = drawn
            .semantic(&UiKey::new(NAME_UNDO))
            .expect("the page draws Undo Last Rename");
        (
            node.action
                .as_ref()
                .map(|action| action.as_str().to_owned())
                .unwrap_or_default(),
            node.state.as_ref().is_none_or(|state| state.enabled),
        )
    }

    /// Whether the palette's Undo Last Rename is available.
    fn undo_command((runtime, instance, view): &Route) -> bool {
        runtime
            .commands(*instance, *view)
            .unwrap()
            .iter()
            .find(|command| command.id.as_str() == NAME_UNDO)
            .expect("the page's palette lists Undo Last Rename")
            .enabled
    }

    /// The write `key`'s button asks for, without the read it was drawn over.
    fn write_of(drawn: &crate::native_ui::CompiledUi, key: &str) -> Option<String> {
        let action = drawn.semantic(&UiKey::new(key))?.action.as_ref()?;
        Some(
            crate::native_settings::machine_page::edit::split_drawn(action.as_str())
                .0
                .to_owned(),
        )
    }

    /// The primary button's write and whether it is available.
    fn primary(route: &Route) -> (String, bool, String) {
        let drawn = whole(route);
        let node = drawn
            .semantic(&UiKey::new(NAME_PRIMARY))
            .expect("the page draws its primary");
        (
            write_of(&drawn, NAME_PRIMARY).unwrap_or_default(),
            node.state.as_ref().is_none_or(|state| state.enabled),
            node.label.clone(),
        )
    }

    /// DESIGN §5.4: a seeded or automatic number left as it is saves as this Mac's chosen one.
    /// NEGATIVE CONTROL: a chosen number with nothing pending offers no Save.
    #[test]
    fn an_unchanged_seeded_or_automatic_number_offers_save() {
        let names = Names::authorized(Ok(true));
        for class in [Class::Seeded, Class::Auto] {
            let scratch = named(class, 7, "Heron");
            let mut route = machine_route(page(1, found(&scratch, &[], &names), false));
            assert_eq!(primary(&route).0, NAME_SAVE, "{class:?}");
            assert!(primary(&route).1, "{class:?}");
            assert_eq!(
                act(&mut route, NAME_SAVE, None),
                [MachineRequest::Save {
                    rev: 1,
                    number: None,
                    nick: None
                }],
                "{class:?}"
            );
        }
        let scratch = named(Class::Chosen, 7, "Heron");
        publish_all(&scratch, &names);
        let mut route = machine_route(page(1, found(&scratch, &[], &names), false));
        assert_eq!(primary(&route).0, NAME_SAVE);
        assert!(
            !primary(&route).1,
            "a chosen number with nothing pending saves nothing"
        );
        assert!(act(&mut route, NAME_SAVE, None).is_empty());
    }

    /// A chosen number whose publication is pending offers Retry Sync, by its action.
    /// NEGATIVE CONTROL: once published it offers none.
    #[test]
    fn a_pending_chosen_number_offers_retry_sync() {
        let names = Names::authorized(Ok(true));
        let scratch = named(Class::Chosen, 86, "Heron");
        let mut route = machine_route(page(1, found(&scratch, &[], &names), false));
        let (action, enabled, _) = primary(&route);
        assert_eq!((action.as_str(), enabled), (NAME_SYNC, true));
        assert_eq!(
            act(&mut route, NAME_SYNC, None),
            [MachineRequest::Sync { rev: 1 }]
        );
        assert!(
            act(&mut route, NAME_SAVE, None).is_empty(),
            "Save is not the primary"
        );
        publish_all(&scratch, &names);
        publish(&mut route, page(2, found(&scratch, &[], &names), false), 2);
        assert_ne!(primary(&route).0, NAME_SYNC);
        assert!(act(&mut route, NAME_SYNC, None).is_empty());
    }

    /// Naming permission that is missing makes the primary Authorize & Sync, available where
    /// this instance has windows; one that could not be read keeps Retry Sync and offers
    /// Authorize Naming… beside it. Each is keyed on its action, not its words.
    /// NEGATIVE CONTROL: a present permission offers no Authorize.
    #[test]
    fn unknown_authorization_offers_authorize() {
        let scratch = named(Class::Chosen, 86, "Heron");
        let missing = Names::authorized(Ok(false));
        let mut route = machine_route(page(1, found(&scratch, &[], &missing), false));
        let (action, enabled, label) = primary(&route);
        assert_eq!(action, NAME_AUTHORIZE);
        assert!(!enabled, "a headless instance opens no tab");
        assert!(
            label.contains(crate::native_settings::machine_page::edit::AUTHORIZE_NEEDS_A_WINDOW)
        );
        assert!(act(&mut route, NAME_AUTHORIZE, None).is_empty());
        publish(&mut route, page(2, found(&scratch, &[], &missing), true), 2);
        assert_eq!(primary(&route).0, NAME_AUTHORIZE);
        assert!(primary(&route).1);
        assert_eq!(
            act(&mut route, NAME_AUTHORIZE, None),
            [MachineRequest::Authorize { rev: 2 }]
        );

        let unknown = Names::authorized(Err("not readable here".to_owned()));
        let mut route = machine_route(page(1, found(&scratch, &[], &unknown), true));
        assert_eq!(primary(&route).0, NAME_SYNC);
        let drawn = whole(&route);
        let beside = drawn
            .semantic(&UiKey::new(NAME_AUTHORIZE_BESIDE))
            .expect("Authorize Naming… beside Retry Sync");
        assert!(beside.action.is_some());
        assert_eq!(
            write_of(&drawn, NAME_AUTHORIZE_BESIDE).as_deref(),
            Some(NAME_AUTHORIZE)
        );
        assert_eq!(
            act(&mut route, NAME_AUTHORIZE, None),
            [MachineRequest::Authorize { rev: 1 }]
        );

        let present = Names::authorized(Ok(true));
        let mut route = machine_route(page(1, found(&scratch, &[], &present), true));
        assert_ne!(primary(&route).0, NAME_AUTHORIZE);
        assert!(
            whole(&route)
                .semantic(&UiKey::new(NAME_AUTHORIZE_BESIDE))
                .is_none()
        );
        assert!(act(&mut route, NAME_AUTHORIZE, None).is_empty());
    }

    /// The line under the fields is Rename This Mac…'s verdict on them, without the panel's
    /// naming status: the page's own read (local files alone) observes no macOS name and
    /// discovers no fleet, so the line says it is reading or discovering neither.
    /// NEGATIVE CONTROL: the panel's line over the same read and fields still waits on both,
    /// and ends with the same verdict.
    #[test]
    fn the_line_judges_the_fields_and_waits_on_no_read_the_page_never_makes() {
        let scratch = named(Class::Chosen, 7, "Heron");
        let mut route = machine_route(page(1, found_local(&scratch, &[]), false));
        assert!(act(&mut route, NAME_FIELD_NUMBER, Some("m14")).is_empty());
        let drawn = whole(&route);
        let line = &drawn
            .semantics
            .iter()
            .find(|node| node.key.as_str().starts_with("name/line"))
            .expect("the live line")
            .label;
        let read = snapshot(1, found_local(&scratch, &[]));
        let number = Field::open(Some("m14"), read.held.number_text());
        let nick = Field::open(None, &read.held.word);
        let verdict =
            crate::machine_rename_sheet::verdict(&read.snapshot, &read.held, &number, &nick);
        assert!(
            line.contains(&verdict.text),
            "{line:?} lacks {:?}",
            verdict.text
        );
        let panel = crate::machine_rename_sheet::line(&read.snapshot, &read.held, &number, &nick);
        assert!(panel.text.ends_with(&verdict.text), "{:?}", panel.text);
        for waits in ["reading current name", "discovering"] {
            assert!(!line.contains(waits), "{line:?}");
            assert!(panel.text.contains(waits), "control: {:?}", panel.text);
        }
    }

    /// A number the vault's labels hold for another Mac leaves Save unavailable, the live line
    /// saying why. NEGATIVE CONTROL: a free number saves, carrying the typed words.
    #[test]
    fn a_number_the_read_refuses_disables_save() {
        let names = Names::authorized(Ok(true));
        let scratch = named(Class::Chosen, 7, "Heron");
        publish_all(&scratch, &names);
        let mut route = machine_route(page(1, found(&scratch, &[20], &names), false));
        assert!(act(&mut route, NAME_FIELD_NUMBER, Some("m20")).is_empty());
        let (action, enabled, _) = primary(&route);
        assert_eq!((action.as_str(), enabled), (NAME_SAVE, false));
        assert!(act(&mut route, NAME_SAVE, None).is_empty());
        assert!(act(&mut route, NAME_FIELD_NUMBER, Some("m14")).is_empty());
        assert_eq!(
            act(&mut route, NAME_SAVE, None),
            [MachineRequest::Save {
                rev: 1,
                number: Some("m14".to_owned()),
                nick: None
            }]
        );
        assert!(act(&mut route, NAME_REVERT, None).is_empty());
        assert!(
            act(&mut route, NAME_SAVE, None).is_empty(),
            "Revert gives the fields back"
        );
        assert_eq!(
            act(&mut route, NAME_RANDOM, None),
            [MachineRequest::Random { rev: 1 }]
        );
    }

    /// LAST CHANGE says what Undo Last Rename gives back, in the words the read made of it, and
    /// its button, its palette command and its press are available exactly where the read
    /// offers an undo and no page write is out; a number the vault's labels were not read for is
    /// left for the undo to check, as Save leaves one. NEGATIVE CONTROLS: a page write out, the
    /// vault's label on the number it would give back, another Mac's claim on it (an older
    /// build's, today's holder), a first number, whose undo would leave this Mac unnumbered, a
    /// History not read, and a name no registry row is this Mac's, each draw it unavailable,
    /// say why and press nothing; and with `edit::undoes` ignoring the read's offer
    /// (`!machine.busy` alone), the refused states draw it available and the loop fails.
    #[test]
    fn undo_says_what_it_gives_back_and_presses_only_where_it_can() {
        let names = Names::authorized(Ok(true));
        let (scratch, shown) = renamed_twice();
        let read_with =
            |held: &[u32]| found_with_history(&scratch, held, &names, Ok(shown.clone()));
        let mut route = machine_route(page(1, read_with(&[]), false));
        assert_eq!(undo_button(&route), (format!("{NAME_UNDO}@1"), true));
        assert_eq!(
            write_of(&whole(&route), NAME_UNDO).as_deref(),
            Some(NAME_UNDO)
        );
        let read = snapshot(1, read_with(&[]));
        assert!(read.undo.undo.is_some());
        assert_eq!(label_of(&whole(&route), "name/undo/line"), read.undo.line);
        assert!(undo_command(&route));
        assert_eq!(
            act(&mut route, NAME_UNDO, None),
            [MachineRequest::Undo { rev: 1 }]
        );

        let unread = machine_route(page(
            1,
            PageFound {
                history: Ok(shown.clone()),
                ..found_unread_labels(&scratch)
            },
            false,
        ));
        assert!(
            undo_button(&unread).1,
            "control: unread labels leave the undo to check its number"
        );
        assert!(undo_command(&unread));

        let busy = MachinePageState {
            busy: true,
            ..page(1, read_with(&[]), false)
        };
        let labelled = page(1, read_with(&[7]), false);
        let (holder, held_shown) = renamed_twice();
        held_by_older_build(&holder, 7);
        let held = page(
            1,
            found_with_history(&holder, &[], &names, Ok(held_shown.clone())),
            false,
        );
        let (first, born) = renamed(&[(7, "Heron")]);
        let first = page(
            1,
            found_with_history(&first, &[], &names, Ok(born.clone())),
            false,
        );
        let why = "no answer within 2 s";
        let unread_history = page(
            1,
            found_with_history(&scratch, &[], &names, Err(why.to_owned())),
            false,
        );
        let no_row = page(1, found(&scratch, &[], &names), false);
        for (what, state, read) in [
            ("busy", busy, Ok(shown.clone())),
            ("labelled", labelled, Ok(shown.clone())),
            ("held", held, Ok(held_shown)),
            ("first", first, Ok(born)),
            ("history unread", unread_history, Err(why.to_owned())),
            ("no row", no_row, Ok(History::default())),
        ] {
            // What refuses the undo, as `aterm name undo` would say it from the same read.
            let refused = match (&state.read, &read) {
                (PageRead::Read(shown), Ok(row)) => history::undo_of(row).err().or_else(|| {
                    let undo = history::undo_of(row).ok()?;
                    shown.snapshot.preview()?.undo(&undo).err()
                }),
                _ => None,
            };
            let mut route = machine_route(state);
            assert!(!undo_button(&route).1, "{what}");
            assert!(!undo_command(&route), "{what}");
            let line = label_of(&whole(&route), "name/undo/line");
            match (refused, &read) {
                (Some(refused), _) => assert!(
                    line.ends_with(&crate::machine_rename_sheet::sentence(&refused.why)),
                    "{what}: {line:?} ends with {:?}",
                    refused.why
                ),
                (None, Err(why)) => assert!(line.contains(why), "{what}: {line:?}"),
                (None, Ok(_)) => {
                    assert_eq!(what, "busy", "only a write out leaves the undo unrefused");
                }
            }
            assert!(act(&mut route, NAME_UNDO, None).is_empty(), "{what}");
        }
    }

    /// HISTORY lists this Mac's registry row's writes as `aterm name history` does, one row per
    /// line of [`History::lines`] over the same History, newest first, keyed by place; in place
    /// of rows, one row says there is no row ([`NO_ROW`]), no write ([`NO_HISTORY`]), or that
    /// History was not read and why.
    /// NEGATIVE CONTROL: with the HISTORY card drawing its rows oldest first
    /// (`lines.iter().rev()`), `name/history/0` says m7 Heron's write and its label assertion
    /// fails.
    #[test]
    fn history_lists_each_change_as_aterm_name_history_does() {
        let names = Names::authorized(Ok(true));
        let (scratch, shown) = renamed_twice();
        let drawn_with = |history: Result<History, String>| {
            whole(&machine_route(page(
                1,
                found_with_history(&scratch, &[], &names, history),
                false,
            )))
        };
        let drawn = drawn_with(Ok(shown.clone()));
        let lines = shown.lines(Some(PAGE_ROWS));
        assert_eq!(lines.len(), 2, "{lines:?}");
        // HISTORY's rows, without its card's heading.
        let rows_of = |drawn: &crate::native_ui::CompiledUi| {
            keys(drawn)
                .into_iter()
                .filter(|key| {
                    key.strip_prefix("name/history/")
                        .is_some_and(|row| row != "heading")
                })
                .collect::<Vec<_>>()
        };
        let rows = rows_of(&drawn);
        assert_eq!(rows.len(), lines.len(), "{rows:?}");
        for (at, line) in lines.iter().enumerate() {
            assert_eq!(label_of(&drawn, &format!("name/history/{at}")), *line);
        }
        assert!(
            label_of(&drawn, "name/history/0").contains("Heron -> Puffin"),
            "newest first"
        );

        let sentence = crate::machine_rename_sheet::sentence;
        let why = "no answer within 2 s";
        for (history, said) in [
            (Ok(History::default()), sentence(NO_ROW)),
            (
                Ok(crate::machine_title::page::fixture::history(vec![])),
                sentence(NO_HISTORY),
            ),
            (
                Err(why.to_owned()),
                sentence(&format!("History was not read ({why})")),
            ),
        ] {
            let drawn = drawn_with(history);
            assert_eq!(rows_of(&drawn), ["name/history/none"]);
            assert_eq!(label_of(&drawn, "name/history/none"), said);
        }
    }

    /// A typed number another Mac's label holds names the next free numbers above it on the live
    /// line, the read's own, and leaves Save unavailable.
    /// NEGATIVE CONTROL: a free number saves.
    #[test]
    fn a_held_number_names_the_next_free_numbers_and_disables_save() {
        use aterm_agent::machine_name::next_free_words;
        let names = Names::authorized(Ok(true));
        let scratch = named(Class::Chosen, 7, "Heron");
        publish_all(&scratch, &names);
        let mut route = machine_route(page(1, found(&scratch, &[20], &names), false));
        assert!(act(&mut route, NAME_FIELD_NUMBER, Some("m20")).is_empty());
        let read = snapshot(1, found(&scratch, &[20], &names));
        let preview = read.snapshot.preview().expect("a read with a name");
        let refused = preview
            .judge(aterm_types::machine_name::Number::new(20).unwrap())
            .expect_err("the vault holds m20");
        let free = preview.next_free(
            aterm_types::machine_name::Number::new(20).unwrap(),
            &refused,
        );
        let named = crate::machine_rename_sheet::sentence(
            &next_free_words(&free).expect("words for the free numbers"),
        );
        let line = label_of(&whole(&route), "name/line");
        assert!(line.ends_with(&named), "{line:?} ends with {named:?}");
        assert_eq!(primary(&route).0, NAME_SAVE);
        assert!(!primary(&route).1, "m20 is another Mac's");
        assert!(act(&mut route, NAME_SAVE, None).is_empty());
        assert!(act(&mut route, NAME_FIELD_NUMBER, Some("m14")).is_empty());
        assert!(primary(&route).1, "control: m14 is free");
        assert!(!label_of(&whole(&route), "name/line").contains(&named));
    }

    /// Settings' own Undo Last Change (`settings/undo`, Cmd-Z: the undo of a settings edit) and
    /// this page's Undo Last Rename are two commands, told apart by their ids and by the words
    /// after their page's name, and the page's button says its command's words. Only Settings'
    /// holds a key: Cmd-Z on this page, with Undo Last Rename available and a number typed,
    /// asks nothing of the name writer.
    /// NEGATIVE CONTROL: the page's own command asks for the undo.
    #[test]
    fn undo_last_rename_is_not_settings_undo() {
        let names = Names::authorized(Ok(true));
        let (scratch, shown) = renamed_twice();
        let mut route = machine_route(page(
            1,
            found_with_history(&scratch, &[], &names, Ok(shown)),
            false,
        ));
        let commands = route.0.commands(route.1, route.2).unwrap();
        let of = |id: &str| {
            let mut listed = commands.iter().filter(|command| command.id.as_str() == id);
            let command = listed
                .next()
                .unwrap_or_else(|| panic!("the palette lists {id}"));
            assert!(listed.next().is_none(), "{id} is listed once");
            command
        };
        // A command's words without the page it names first (`Settings: `, `This Mac: `).
        let words = |title: &str| {
            title
                .split_once(": ")
                .map_or(title, |(_, words)| words)
                .to_owned()
        };
        let (settings, rename) = (of("settings/undo"), of(NAME_UNDO));
        assert_ne!(settings.id, rename.id);
        assert_ne!(settings.title, rename.title);
        assert_ne!(words(&settings.title), words(&rename.title));
        assert_eq!(label_of(&whole(&route), NAME_UNDO), words(&rename.title));
        assert!(settings.shortcut.is_some(), "Cmd-Z is Settings' undo");
        assert_eq!(rename.shortcut, None, "the rename's undo takes no key");
        assert!(rename.enabled);

        let undo_key = |route: &mut Route| {
            let (runtime, instance, view) = route;
            let outcome = runtime
                .dispatch(*instance, *view, AppEvent::TextInput(TextInputEvent::Undo))
                .unwrap();
            asked(&outcome.effects)
        };
        assert!(undo_key(&mut route).is_empty(), "Cmd-Z renames nothing");
        assert!(act(&mut route, NAME_FIELD_NUMBER, Some("m14")).is_empty());
        assert!(
            undo_key(&mut route).is_empty(),
            "Cmd-Z over a typed number renames nothing"
        );
        assert_eq!(
            act(&mut route, NAME_UNDO, None),
            [MachineRequest::Undo { rev: 1 }],
            "control: the page's own command asks for the undo"
        );
    }

    /// The primary is the page's default only while it acts on words the person typed: drawn as
    /// Settings' Primary button (the accent) and the compiled tree's default action, which
    /// Return presses where the keyboard is in no control; the page's other buttons stay
    /// Secondary. NEGATIVE CONTROL: a Save with nothing to save, one the line refuses, and a
    /// primary that acts on nothing typed (an unchanged seeded number's Save, Retry Sync), which
    /// a Return on arrival would otherwise press, are drawn as the rest are and are no default.
    #[test]
    fn the_primary_is_the_pages_default_only_over_typed_words() {
        let style = |drawn: &crate::native_ui::CompiledUi, key: &str| {
            drawn
                .paint
                .iter()
                .find(|node| node.key.as_str() == key)
                .and_then(|node| match &node.content {
                    UiContent::Button(control) => Some(control.style),
                    _ => None,
                })
                .unwrap_or_else(|| panic!("{key} paints a button"))
        };
        let default = |drawn: &crate::native_ui::CompiledUi| {
            drawn.default_action.as_ref().map(|(key, action)| {
                (
                    key.as_str().to_owned(),
                    crate::native_settings::machine_page::edit::split_drawn(action.as_str())
                        .0
                        .to_owned(),
                )
            })
        };
        let names = Names::authorized(Ok(true));
        let scratch = named(Class::Chosen, 7, "Heron");
        publish_all(&scratch, &names);
        let mut route = machine_route(page(1, found(&scratch, &[20], &names), false));
        let drawn = whole(&route);
        assert!(!primary(&route).1, "a chosen number with nothing pending");
        assert_eq!(style(&drawn, NAME_PRIMARY), StyleRef::Secondary);
        assert_eq!(default(&drawn), None, "a dead Save is no default");
        assert!(act(&mut route, NAME_FIELD_NUMBER, Some("m20")).is_empty());
        let drawn = whole(&route);
        assert_eq!(primary(&route).0, NAME_SAVE);
        assert!(!primary(&route).1, "m20 is another Mac's");
        assert_eq!(style(&drawn, NAME_PRIMARY), StyleRef::Secondary);
        assert_eq!(default(&drawn), None, "a refused Save is no default");
        assert!(act(&mut route, NAME_FIELD_NUMBER, Some("m14")).is_empty());
        let drawn = whole(&route);
        assert!(primary(&route).1, "m14 is free");
        assert_eq!(style(&drawn, NAME_PRIMARY), StyleRef::Primary);
        assert_eq!(
            default(&drawn),
            Some((NAME_PRIMARY.to_owned(), NAME_SAVE.to_owned()))
        );
        for key in [NAME_RANDOM, NAME_REVERT, NAME_REFRESH] {
            assert_eq!(style(&drawn, key), StyleRef::Secondary, "{key}");
        }
        // An unchanged seeded number's Save and a pending chosen number's Retry Sync act with
        // nothing typed: available, but drawn as the rest are and no default.
        let seeded = named(Class::Seeded, 7, "Heron");
        let mut route = machine_route(page(1, found(&seeded, &[], &names), false));
        let drawn = whole(&route);
        assert_eq!(primary(&route).0, NAME_SAVE);
        assert!(primary(&route).1, "an unchanged seeded number saves");
        assert_eq!(style(&drawn, NAME_PRIMARY), StyleRef::Secondary);
        assert_eq!(
            default(&drawn),
            None,
            "a Save of nothing typed is no default"
        );
        assert!(act(&mut route, NAME_FIELD_NICK, Some("Wren")).is_empty());
        let drawn = whole(&route);
        assert_eq!(style(&drawn, NAME_PRIMARY), StyleRef::Primary);
        assert_eq!(
            default(&drawn),
            Some((NAME_PRIMARY.to_owned(), NAME_SAVE.to_owned())),
            "a typed nickname's Save is the default"
        );
        let pending = named(Class::Chosen, 86, "Heron");
        let route = machine_route(page(1, found(&pending, &[], &names), false));
        let drawn = whole(&route);
        assert_eq!(primary(&route).0, NAME_SYNC);
        assert!(primary(&route).1, "a pending chosen number retries");
        assert_eq!(style(&drawn, NAME_PRIMARY), StyleRef::Secondary);
        assert_eq!(default(&drawn), None, "Retry Sync is no default");
    }

    /// Escape in a field that holds typed words gives it back to the read, as Revert does for
    /// both, and a second Escape changes no field. Before Settings' Escape arm passed it to the
    /// page, that arm took the key first and the typed words stayed (seen in a windowed instance:
    /// an Escape after typing left "Conchaaa" in the Nickname field and Save available).
    #[test]
    fn escape_gives_a_typed_field_back_to_the_read() {
        let names = Names::authorized(Ok(true));
        let scratch = named(Class::Chosen, 7, "Heron");
        publish_all(&scratch, &names);
        let mut route = machine_route(page(1, found(&scratch, &[], &names), false));
        let value = |route: &Route| match &whole(route)
            .semantic(&UiKey::new(NAME_FIELD_NICK))
            .expect("the field")
            .value
        {
            SemanticValue::Text(text) => text.clone(),
            other => panic!("the Nickname field holds {other:?}"),
        };
        let escape = |(runtime, instance, view): &mut Route| {
            runtime
                .dispatch(
                    *instance,
                    *view,
                    AppEvent::TextInput(TextInputEvent::Cancel),
                )
                .unwrap();
        };
        assert!(act(&mut route, NAME_FIELD_NICK, Some("Kestrel")).is_empty());
        assert_eq!(value(&route), "Kestrel");
        assert!(primary(&route).1, "Kestrel saves");
        escape(&mut route);
        assert_eq!(value(&route), "Heron", "Escape gives the field back");
        assert!(!primary(&route).1, "nothing is left to save");
        assert!(
            act(&mut route, NAME_SAVE, None).is_empty(),
            "and Save asks for nothing"
        );
        escape(&mut route);
        assert_eq!(value(&route), "Heron", "a second Escape changes no field");
    }

    #[test]
    fn a_spelling_that_displays_the_saved_name_leaves_only_a_numeric_edit_to_save() {
        let names = Names::authorized(Ok(true));
        let scratch = named(Class::Chosen, 7, "Dog");
        publish_all(&scratch, &names);
        let mut route = machine_route(page(1, found(&scratch, &[], &names), false));
        assert!(act(&mut route, NAME_FIELD_NICK, Some("dog")).is_empty());
        assert!(!primary(&route).1);
        assert!(act(&mut route, NAME_FIELD_NUMBER, Some("8")).is_empty());
        assert_eq!(
            act(&mut route, NAME_SAVE, None),
            [MachineRequest::Save {
                rev: 1,
                number: Some("8".to_owned()),
                nick: None,
            }]
        );
        rename(&scratch, Class::Chosen, 8, "Puffin");
        publish(&mut route, page(2, found(&scratch, &[], &names), false), 2);
        assert_eq!(
            whole(&route)
                .semantic(&UiKey::new(NAME_FIELD_NICK))
                .unwrap()
                .value,
            SemanticValue::Text("Puffin".into())
        );
    }

    /// A field the person did not type into follows each read; a typed one keeps its words.
    #[test]
    fn an_untouched_field_follows_a_new_read_and_a_typed_one_does_not() {
        let names = Names::authorized(Ok(true));
        let scratch = named(Class::Chosen, 7, "Heron");
        publish_all(&scratch, &names);
        let mut route = machine_route(page(1, found(&scratch, &[], &names), false));
        // The keyboard reaches the Number field, and Kestrel is typed into Nickname.
        assert!(act(&mut route, NAME_FIELD_NUMBER, None).is_empty());
        assert!(act(&mut route, NAME_FIELD_NICK, Some("Kestrel")).is_empty());
        rename(&scratch, Class::Chosen, 8, "Puffin");
        publish(&mut route, page(2, found(&scratch, &[], &names), false), 2);
        let value = |route: &Route, key: &str| match &whole(route)
            .semantic(&UiKey::new(key))
            .expect("the field")
            .value
        {
            SemanticValue::Text(text) => text.clone(),
            other => panic!("{key} holds {other:?}"),
        };
        assert_eq!(value(&route, NAME_FIELD_NUMBER), "8", "untouched: follows");
        assert_eq!(value(&route, NAME_FIELD_NICK), "Kestrel", "typed: kept");
        assert_eq!(
            act(&mut route, NAME_SAVE, None),
            [MachineRequest::Save {
                rev: 2,
                number: None,
                nick: Some("Kestrel".to_owned())
            }]
        );
    }

    /// What one of [`widest`]'s states types: the Number field's text and, where it types one,
    /// the Nickname field's.
    type Typed = (String, Option<String>);

    /// A full History of the widest lines: [`PAGE_ROWS`] writes, each a revert by `m999999`
    /// that wrote the largest numbers and the widest nickname over two values each, written at
    /// once with another write and replaced since, with both cells of this Mac's still on their
    /// way to the row.
    fn widest_history() -> History {
        use crate::machine_title::page::fixture::{cell, history, write};
        use aterm_db::att::EntryKind;
        use aterm_db::file::Renamed;
        let writes = (0..PAGE_ROWS)
            .map(|at| {
                let op = u8::try_from(at + 1).expect("a small op");
                let widest = |column: Renamed, value: &str, replaced: [&str; 2]| {
                    let mut written = cell(column, value, false, &replaced);
                    written.unseen = vec![[0xff; 32]];
                    written
                };
                let mut written = write(
                    op,
                    "m999999",
                    EntryKind::Explicit,
                    vec![
                        widest(Renamed::Label, "m999999", ["m999998", "m999997"]),
                        widest(Renamed::Nickname, WIDEST_NICK, [WIDEST_NICK, WIDEST_NICK]),
                    ],
                );
                written.reverts = Some([0xee; 32]);
                written
            })
            .rev()
            .collect();
        History {
            pending: vec![Renamed::Label, Renamed::Nickname],
            ..history(writes)
        }
    }

    /// The states a read leaves the page in, at their widest: the widest nickname on the largest
    /// number with a node key (its Node fact is `node-` and 32 hex digits), a 200-character
    /// entry in each field and the line's refusal of it, the longest primary label, that of
    /// a missing naming rule in an instance with no window, an undo of the widest name, a full
    /// History ([`PAGE_ROWS`] writes, each cell still on its way too) of the widest lines a write
    /// makes (the largest numbers and the widest nickname, each written at once with another
    /// write, replaced since, over two values, in a revert by the widest Mac name), and a held
    /// number typed, its next free numbers named.
    fn widest() -> Vec<(&'static str, MachinePageState, Option<Typed>)> {
        let names = Names::authorized(Err("not readable here".to_owned()));
        let scratch = named(Class::Chosen, 999_999, WIDEST_NICK);
        with_node(&scratch);
        let read = page(1, found(&scratch, &[], &names), true);
        let missing = Names::authorized(Ok(false));
        let headless = page(1, found(&scratch, &[], &missing), false);
        let long = "W".repeat(200);
        let (undone, shown) = renamed(&[(999_999, WIDEST_NICK), (999_998, WIDEST_NICK)]);
        with_node(&undone);
        let undo = page(1, found_with_history(&undone, &[], &names, Ok(shown)), true);
        assert!(
            matches!(&undo.read, PageRead::Read(read) if read.undo.undo.is_some()),
            "an undo of the widest name is offered"
        );
        let full = page(
            1,
            found_with_history(&scratch, &[], &names, Ok(widest_history())),
            true,
        );
        let rows = match &full.read {
            PageRead::Read(read) => match &read.history {
                Some(PageHistory::Rows(rows)) => rows.len(),
                _ => 0,
            },
            _ => 0,
        };
        assert_eq!(
            rows,
            PAGE_ROWS + 2,
            "a full History: the newest writes and both cells on their way"
        );
        let held = page(1, found(&scratch, &[999_990], &names), true);
        vec![
            ("reading", MachinePageState::default(), None),
            (
                "unread",
                unread(&"the vault's roster is held by another aterm; ".repeat(6)),
                None,
            ),
            (
                "unsupported",
                MachinePageState {
                    read: PageRead::Unsupported(
                        "`aterm name` in a terminal changes this machine's number and nickname",
                    ),
                    ..MachinePageState::default()
                },
                None,
            ),
            ("read", read.clone(), None),
            ("authorize headless", headless, None),
            (
                "typed",
                MachinePageState {
                    notice: Some(format!("Last attempt: {}", "the lock is busy; ".repeat(8))),
                    ..read
                },
                Some((long.clone(), Some(long))),
            ),
            ("undo available", undo, None),
            ("history full", full, None),
            ("held typed", held, Some(("999990".to_owned(), None))),
        ]
    }

    /// The page in each state draws whole at three text scales in a wide, a compact and a
    /// phone-width window: nothing overflows or is clipped, and every hit is on its paint. The
    /// one field the keyboard is in scrolls its caret's end into view, as every typed field
    /// does, so its own text-field line is that scroll, not a cut: it must still paint words.
    #[test]
    fn this_mac_remains_whole_in_wide_compact_and_scaled_settings() {
        at_each_text_scale(&[1.0, 1.25, 2.0], "This Mac page", |scale| {
            for (what, state, typed) in widest() {
                let mut route = machine_route(state);
                let mut scrolls = None;
                if let Some((number, nick)) = &typed {
                    let _ = act(&mut route, NAME_FIELD_NUMBER, Some(number));
                    if let Some(nick) = nick {
                        let _ = act(&mut route, NAME_FIELD_NICK, Some(nick));
                        scrolls = Some(format!(
                            "paint-text key=\"{NAME_FIELD_NICK}\" kind=text-field"
                        ));
                    }
                }
                for (width, height) in [(1_200.0, 820.0), (584.0, 700.0), (320.0, 568.0)] {
                    let cx = view_cx_at(width, height);
                    let (runtime, instance, view) = &mut route;
                    each_screen(runtime, *instance, *view, &cx, |compiled, top| {
                        let context = format!("{what} {scale}\u{d7} {width}\u{d7}{height} {top}");
                        compiled
                            .validate_parity()
                            .unwrap_or_else(|error| panic!("{context}: {error:?}"));
                        let scrolled = |line: &String| {
                            scrolls.as_ref().is_some_and(|field| {
                                line.starts_with(field.as_str())
                                    && line.contains("clip-truncated=false")
                                    && line.contains("painted=\"W")
                            })
                        };
                        let failures: Vec<String> = compiled
                            .fit_failures()
                            .into_iter()
                            .filter(|line| !line.contains("key=\"settings/nav/"))
                            .filter(|line| !scrolled(line))
                            .collect();
                        assert!(
                            failures.is_empty(),
                            "{context}: text overflow or clip:\n{}",
                            failures.join("\n")
                        );
                        assert_all_hits_material_and_contained(compiled, &context);
                        // The fit audit judges words by width alone, and a clip equal to a
                        // node's own box is no cut, so each line of the page's words must
                        // also hold its own text's line band (`word_line_band`, 1.2 em).
                        for node in &compiled.paint {
                            let UiContent::Text(spec) = &node.content else {
                                continue;
                            };
                            if !node.key.as_str().starts_with("name/") {
                                continue;
                            }
                            let px = crate::native_ui::text_paint_metrics(spec.role, spec.style).0;
                            assert!(
                                node.rect.height + 0.5 >= 1.2 * px,
                                "{context}: {:?} is {} tall for {px}px words",
                                node.key,
                                node.rect.height
                            );
                        }
                    });
                }
            }
        });
    }

    /// Read-state twin of `every_route_reaches_every_control_by_scrolling_at_every_window_size`,
    /// whose sweep draws this page only while it reads: every control the read page draws is
    /// reached by scrolling, at each of that sweep's window sizes.
    #[test]
    fn a_read_page_reaches_every_control_by_scrolling() {
        for (width, height) in [(584.0, 348.0), (738.0, 418.0), (1_074.0, 684.0)] {
            for (what, state, _) in widest() {
                let mut route = machine_route(state);
                let (runtime, instance, view) = &mut route;
                let expected = page_controls(&compile_settings_view(
                    runtime,
                    *instance,
                    *view,
                    &view_cx_at(width, 4_000.0),
                ));
                let mut reached = BTreeSet::new();
                each_screen(
                    runtime,
                    *instance,
                    *view,
                    &view_cx_at(width, height),
                    |compiled, top| {
                        let context = format!("{what} {width}\u{d7}{height} screen {top}");
                        compiled
                            .validate_parity()
                            .unwrap_or_else(|error| panic!("{context}: {error:?}"));
                        assert_all_hits_material_and_contained(compiled, &context);
                        for hit in &compiled.hits {
                            assert!(
                                hit_matches_paint(compiled, hit),
                                "{context}: {:?} is clipped",
                                hit.key
                            );
                        }
                        reached.extend(page_controls(compiled));
                    },
                );
                let missing = expected.difference(&reached).collect::<Vec<_>>();
                assert!(
                    missing.is_empty(),
                    "{what} {width}\u{d7}{height}: never reached {missing:?}"
                );
            }
        }
    }
}
