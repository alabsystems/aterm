// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;

/// 900 and 901 ← 800 ← 700 (a shell, pid 700) ← 1, each its own group
/// but 800.
fn parent(pid: i32) -> Option<i32> {
    match pid {
        900 | 901 => Some(800),
        800 => Some(700),
        700 => Some(1),
        _ => None,
    }
}

fn group(pid: i32) -> Option<i32> {
    Some(if pid == 800 { 650 } else { pid })
}

fn comm(pid: i32) -> Option<String> {
    matches!(pid, 900 | 901).then(|| "python3".to_string())
}

fn client(pid: u32) -> Who {
    Who::client_with(Some(pid), 42, &parent, &group, &comm)
}

/// Tab `local` names a typist `tab-<local>`, `s-<local>` at facts; the
/// edge's session is tab 3.
fn named(who: &Who, shells: &[(u64, i32)]) -> Named {
    let sid = SessionId::parse("tqn5r6e0148").expect("a tab id");
    name(who, shells, &|s| (*s == sid).then_some(3), &|local| {
        Some(Named {
            words: format!("tab-{local}"),
            facts: Some(format!("s-{local}")),
            by_parent: false,
        })
    })
}

#[test]
fn a_client_is_read_while_connected_and_named_by_its_tab() {
    let who = client(900);
    assert_eq!(
        who,
        Who::Process {
            pid: 900,
            comm: Some("python3".to_string()),
            lineage: vec![(900, Some(900)), (800, Some(650)), (700, Some(700))],
            cut: false,
        },
        "the lineage stops before pid 1"
    );
    let by_shell = named(&who, &[(5, 700)]);
    assert_eq!(
        (by_shell.words.as_str(), by_shell.by_parent),
        ("tab-5", true)
    );
    assert_eq!(by_shell.facts.as_deref(), Some("s-5"));
    assert_eq!(named(&who, &[(6, 650)]).words, "tab-6", "a process group");
    assert!(
        !named(&who, &[(8, 900)]).by_parent,
        "the shell is the process itself"
    );
    let alone = named(&who, &[(7, 4242)]);
    assert_eq!(alone.words, "python3 (pid 900)", "no tab");
    assert_eq!(alone.facts, None, "a program's name is no fact");
    assert_eq!(
        named(&client(42), &[(5, 700)]).words,
        "aterm",
        "this process"
    );
    let gone = Who::client_with(Some(77), 42, &|_| None, &|_| None, &|_| None);
    assert_eq!(named(&gone, &[]).words, "pid 77");
    let unread = Who::client_with(None, 42, &parent, &group, &comm);
    assert_eq!(named(&unread, &[]).words, "a script");
}

/// A chain of parents longer than aterm reads is cut: a client 33 processes above launchd keeps
/// the first 32, its last ancestor unread, and says so ([`Who::lineage_cut`]), through the
/// process groups dropped too ([`Who::parent_chain`]).
///
/// CONTROL: a chain of 32 that ends at launchd and a chain whose parent could not be read are
/// read whole, so neither is cut; nor is a caller with no lineage.
#[test]
fn a_lineage_longer_than_aterm_reads_is_cut() {
    let chain = |top: i32| {
        move |p: i32| match p {
            1000..1032 if p < top => Some(p + 1),
            _ if p == top => Some(1),
            _ => None,
        }
    };
    let deep = Who::client_with(Some(1000), 42, &chain(1032), &|_| None, &|_| None);
    let Who::Process { lineage, .. } = &deep else {
        panic!("a process: {deep:?}");
    };
    assert_eq!(lineage.len(), LINEAGE_MAX);
    assert_eq!(lineage.last(), Some(&(1031, None)), "1032 is left unread");
    assert!(deep.lineage_cut());
    assert!(
        deep.parent_chain().lineage_cut(),
        "the parent chain keeps the cut"
    );

    let whole = Who::client_with(Some(1000), 42, &chain(1031), &|_| None, &|_| None);
    assert!(!whole.lineage_cut(), "CONTROL: 32 deep, ending at launchd");
    let unreadable = Who::client_with(
        Some(9002),
        42,
        &|p| (p == 9002).then_some(4242),
        &|_| None,
        &|_| None,
    );
    assert!(
        !unreadable.lineage_cut(),
        "CONTROL: a parent aterm cannot read"
    );
    assert!(!Who::client_with(None, 42, &parent, &group, &comm).lineage_cut());
}

/// A DRIVER'S TYPING OUTLIVES aterm's OWN AFTER IT ([`TypedStamp::driven_ms`]): a lab's `/exit`
/// followed, before the exit is decided, by mail the message bus delivers and a line the
/// harness types is still the lab's, though `typed_by=` names the last typist alone.
#[test]
fn a_drivers_typing_is_kept_past_aterms_own() {
    let stamp = TypedStamp::default();
    assert_eq!(stamp.driven_ms(1_000), None, "nobody typed");
    let _ = stamp.stamp(Caller::new(Who::Aterm), 500);
    assert_eq!(
        stamp.driven_ms(1_000),
        None,
        "aterm's own line drives nothing"
    );
    let _ = stamp.stamp(Caller::new(client(900)), 10_000);
    let _ = stamp.stamp(Caller::new(Who::Bridge), 10_400);
    let _ = stamp.stamp(Caller::new(Who::Aterm), 10_600);
    assert_eq!(
        stamp.last().map(|t| t.by.words()).as_deref(),
        Some("aterm"),
        "the last typist"
    );
    assert_eq!(
        stamp.driven_ms(11_000),
        Some(1_000),
        "the lab's, 1 s before"
    );
    let unread = Who::client_with(None, 42, &parent, &group, &comm);
    let _ = stamp.stamp(Caller::new(unread), 20_000);
    assert_eq!(
        stamp.driven_ms(20_250),
        Some(250),
        "a script with no peer is no typing of aterm's"
    );
}

/// An edge names who presented its token, on the edge's tab, and that
/// tab alone only when its own shell is the client.
#[test]
fn an_edge_is_who_presented_it_on_its_tab() {
    let edge = |sid: &str, client: Who| Who::Edge {
        src: SessionId::parse(sid).expect("a tab id"),
        client: Box::new(client),
    };
    let here = "tqn5r6e0148";
    let own = named(&edge(here, client(900)), &[(3, 700)]);
    assert_eq!(own.words, "tab-3", "the edge's own tab typed");
    assert_eq!(own.facts.as_deref(), Some("s-3"));
    let other = named(&edge(here, client(900)), &[(5, 700)]);
    assert_eq!(other.words, "tab-5 on tab-3's edge");
    assert_eq!(other.facts, None);
    assert_eq!(
        named(&edge(here, client(900)), &[]).words,
        "python3 (pid 900) on tab-3's edge"
    );
    // An edge no tab names is named by its tab ID as it is shown (LT-20): its first five
    // characters, longer while another ID this process knows shares them. The registry is the
    // test binary's, which other tests' tabs (minted, adopted, restored) fill as they run, so
    // the shown form is read from the one phrase itself, never from a second read of the
    // registry another test can change between the two, and held to that rule.
    let on_edge = |words: &str, who: &str, sid: &str| {
        let shown = words
            .strip_prefix(who)
            .and_then(|rest| rest.strip_prefix(" on "))
            .and_then(|rest| rest.strip_suffix("'s edge"))
            .unwrap_or_else(|| panic!("{words:?} is not \"{who} on <tab>'s edge\""));
        assert!(
            shown.len() >= 5 && sid.starts_with(shown),
            "{words:?}: {shown:?} is not {sid} as shown"
        );
    };
    let away = "tfedcba9876";
    on_edge(
        &named(&edge(away, client(900)), &[]).words,
        "python3 (pid 900)",
        away,
    );
    let unread = Who::client_with(None, 42, &parent, &group, &comm);
    on_edge(&Caller::new(edge(here, unread)).words(), "a script", here);
    assert_eq!(named(&Who::Bridge, &[(5, 700)]).words, "the message bus");
    let fleet = Who::Word("mini over the fleet".to_string());
    assert_eq!(named(&fleet, &[]).words, "mini over the fleet");
}

#[test]
fn no_tab_takes_a_word_aterm_credits_by() {
    for word in [
        "aterm",
        "Aterm",
        "the message bus",
        "a script",
        "human",
        "harness",
        "mac-03 over the fleet",
        "MAC-03  Over the\tFLEET ",
        fleet_credit("mac-03.local (node 1a2b3c4d)").as_str(),
    ] {
        assert!(is_credit_word(word), "{word}");
    }
    for word in [
        "lab-sweep",
        "aterm-dev",
        "bus",
        "fleet",
        "over-the-fleet-bot",
    ] {
        assert!(!is_credit_word(word), "{word}");
    }
}

/// AN `invoke Paste` TYPES FOR ITS CALLER where its paste lands while the
/// invoke runs (`App::invoke_typist`).
#[test]
#[cfg(unix)]
fn an_invoked_paste_is_its_callers_typing() {
    use std::os::fd::IntoRawFd;
    let pipe = aterm_uds::pipe()
        .map(|(r, w)| [r.into_raw_fd(), w.into_raw_fd()])
        .expect("a pipe");
    let sink = Arc::new(aterm_session::sink::SinkWriter::new(pipe[1]));
    let mut app = crate::App::headless_for_test_with_sink(sink);
    let wid = crate::WindowId(0);
    let owner = app.focused_session_id(wid);
    let ctx = Arc::clone(
        &app.pool
            .get(owner.expect("a front session"))
            .expect("pooled")
            .ctx,
    );
    app.deliver_paste(wid, owner, "ls".to_string(), crate::input::Source::Human);
    assert!(ctx.typed.last().is_none(), "the person's paste");

    app.invoke_typist = Some(Caller::new(Who::Word("lab-sweep".to_string())));
    app.deliver_paste(wid, owner, "ls".to_string(), crate::input::Source::Human);
    let typed = ctx.typed.last().expect("the caller typed");
    assert_eq!(typed.by.words(), "lab-sweep");
    assert!(app.invoke_typist.is_none(), "taken by the paste it credits");
    // SAFETY: the read end is this test's, closed once.
    unsafe { libc::close(pipe[0]) };
}

#[test]
fn a_stamp_owes_the_hover_only_a_new_caller_or_a_new_minute() {
    let stamp = TypedStamp::default();
    assert!(stamp.last().is_none());
    assert_eq!(wire(None, 0), "typed_by=- typed_ms=-");
    let first = Caller::named(client(900), named(&client(900), &[(5, 700)]));
    assert!(stamp.stamp(Arc::clone(&first), 120_000), "a first stamp");
    assert!(
        !stamp.stamp(Caller::new(client(900)), 150_000),
        "the same caller in the same minute"
    );
    let last = stamp.last().expect("stamped");
    assert!(Arc::ptr_eq(&last.by, &first), "it keeps its name");
    assert_eq!(last.at_unix_ms, 150_000);
    assert_eq!(wire(Some(&last), 154_000), "typed_by=tab-5 typed_ms=4000");
    assert!(
        !stamp.stamp(Caller::new(client(901)), 155_000),
        "`aterm ctl` again from the shell whose tab named it"
    );
    assert!(Arc::ptr_eq(&stamp.last().expect("stamped").by, &first));
    assert!(
        stamp.stamp(Caller::new(client(900)), 180_000),
        "a new minute"
    );
    assert!(
        stamp.stamp(Caller::new(Who::Bridge), 181_000),
        "a new caller"
    );
    let by_bus = stamp.last().expect("stamped");
    assert_eq!(
        wire(Some(&by_bus), 181_000),
        "typed_by=the%20message%20bus typed_ms=0"
    );

    let alone = TypedStamp::default();
    let by_pid = Caller::named(client(900), named(&client(900), &[]));
    assert!(alone.stamp(by_pid, 120_000));
    assert!(alone.stamp(Caller::new(client(901)), 121_000));
    assert!(
        alone.stamp(Caller::new(client(900)), 122_000),
        "an unnamed caller covers no sibling"
    );
}
