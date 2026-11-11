// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;
use crate::bplist::write::list;

const ID: &str = "com.aterm.aterm";
const EXE: &str = "/Applications/aterm.app/Contents/MacOS/aterm";
/// The guest account whose configuration the fixtures hold.
const USER: &str = "E8D0C90E-068E-4C80-AC44-6815AE2AC56B";
/// Another account on the same Mac.
const OTHER_USER: &str = "5D773287-9015-4E7D-A8F3-7CE6088E62D5";

fn team_code() -> Code {
    Code {
        identifier: ID.into(),
        team: Some("A66A9P66Z7".into()),
        executable: EXE.into(),
        user: USER.into(),
    }
}

fn privacy(user: &str) -> String {
    format!("{NETWORK_PRIVACY}-{user}")
}

fn ad_hoc_code() -> Code {
    Code {
        team: None,
        ..team_code()
    }
}

/// One rule as an archive would hold it: (identifier, path, answered, deny multicast).
type R<'a> = (&'a str, Option<&'a str>, bool, bool);

/// An NSKeyedArchiver-shaped list: one configuration named `name`, its controller enabled or
/// not, holding `rules`.
fn archive(name: &str, enabled: bool, rules: &[R<'_>]) -> Vec<u8> {
    configurations(&[(name, enabled, rules)])
}

/// An NSKeyedArchiver-shaped list of several configurations, each (name, enabled, rules).
fn configurations(configs: &[(&str, bool, &[R<'_>])]) -> Vec<u8> {
    // Plist objects; `$objects` (archive order) is the list of plist indexes `objs`.
    let mut plist: Vec<Object> = Vec::new();
    let mut objs: Vec<usize> = Vec::new();
    let push = |plist: &mut Vec<Object>, objs: &mut Vec<usize>, o: Object| {
        plist.push(o);
        objs.push(plist.len() - 1);
        objs.len() - 1
    };
    // The top dict and `$objects` array are written last; reserve their slots.
    plist.push(Object::Null); // 0: top
    plist.push(Object::Null); // 1: $objects array
    let key = |plist: &mut Vec<Object>, text: &str| {
        plist.push(Object::String(text.into()));
        plist.len() - 1
    };
    let uid = |plist: &mut Vec<Object>, n: usize| {
        plist.push(Object::Uid(n as u64));
        plist.len() - 1
    };
    let boolean = |plist: &mut Vec<Object>, b: bool| {
        plist.push(Object::Bool(b));
        plist.len() - 1
    };
    let null = push(&mut plist, &mut objs, Object::String("$null".into()));
    let class = |plist: &mut Vec<Object>, objs: &mut Vec<usize>, name: &str| {
        let k = key(plist, "$classname");
        let v = key(plist, name);
        plist.push(Object::Dict(vec![(k, v)]));
        objs.push(plist.len() - 1);
        objs.len() - 1
    };
    let rule_class = class(&mut plist, &mut objs, "NEPathRule");
    let controller_class = class(&mut plist, &mut objs, "NEPathController");
    let array_class = class(&mut plist, &mut objs, "NSArray");
    for (name, enabled, rules) in configs {
        let mut rule_uids = Vec::new();
        for (identifier, path, answered, deny) in *rules {
            let id = push(&mut plist, &mut objs, Object::String((*identifier).into()));
            let path = match path {
                Some(p) => push(&mut plist, &mut objs, Object::String((*p).into())),
                None => null,
            };
            let mut pairs = Vec::new();
            for (k, v) in [
                ("$class", uid(&mut plist, rule_class)),
                ("SigningIdentifier", uid(&mut plist, id)),
                ("Path", uid(&mut plist, path)),
                ("MulticastPreferenceSet", boolean(&mut plist, *answered)),
                ("DenyMulticast", boolean(&mut plist, *deny)),
                ("DenyAll", boolean(&mut plist, false)),
            ] {
                pairs.push((key(&mut plist, k), v));
            }
            rule_uids.push(push(&mut plist, &mut objs, Object::Dict(pairs)));
        }
        let refs: Vec<usize> = rule_uids.iter().map(|r| uid(&mut plist, *r)).collect();
        plist.push(Object::Array(refs));
        let array_at = plist.len() - 1;
        let pairs = vec![
            (key(&mut plist, "$class"), uid(&mut plist, array_class)),
            (key(&mut plist, "NS.objects"), array_at),
        ];
        let array = push(&mut plist, &mut objs, Object::Dict(pairs));
        let pairs = vec![
            (key(&mut plist, "$class"), uid(&mut plist, controller_class)),
            (key(&mut plist, "Enabled"), boolean(&mut plist, *enabled)),
            (key(&mut plist, "Rules"), uid(&mut plist, array)),
        ];
        let controller = push(&mut plist, &mut objs, Object::Dict(pairs));
        let name = push(&mut plist, &mut objs, Object::String((*name).into()));
        let pairs = vec![
            (key(&mut plist, "Name"), uid(&mut plist, name)),
            (
                key(&mut plist, "PathController"),
                uid(&mut plist, controller),
            ),
        ];
        push(&mut plist, &mut objs, Object::Dict(pairs));
    }
    plist[1] = Object::Array(objs);
    let objects_key = key(&mut plist, "$objects");
    plist[0] = Object::Dict(vec![(objects_key, 1)]);
    list(&plist, 0)
}

#[test]
fn only_an_answered_allow_for_this_code_is_an_allowance() {
    let privacy = privacy(USER);
    let privacy = privacy.as_str();
    let default = ("PathRuleDefaultNonSystemIdentifier", None, false, true);
    let allowed_team = archive(privacy, true, &[default, (ID, None, true, false)]);
    assert!(allowed(&allowed_team, &team_code()).is_ok());
    // NEGATIVE CONTROLS: the same allowance read for an ad-hoc image (nehelper's rule with no
    // path is a team-signed app's), and every state that is not an answered Allow.
    assert!(allowed(&allowed_team, &ad_hoc_code()).is_err());
    let unanswered = archive(privacy, true, &[default, (ID, None, false, true)]);
    let why = allowed(&unanswered, &team_code()).unwrap_err();
    assert!(why.contains("heard no answer"), "{why}");
    let refused = archive(privacy, true, &[default, (ID, None, true, true)]);
    let why = allowed(&refused, &team_code()).unwrap_err();
    assert!(why.contains("does not allow"), "{why}");
    let other_app = archive(privacy, true, &[("com.tcltk.wish", None, true, false)]);
    let why = allowed(&other_app, &team_code()).unwrap_err();
    assert!(why.contains("no Local Network answer"), "{why}");
    let disabled = archive(privacy, false, &[(ID, None, true, false)]);
    assert!(allowed(&disabled, &team_code()).is_err());
    let other_config = archive("com.example.vpn", true, &[(ID, None, true, false)]);
    assert!(allowed(&other_config, &team_code()).is_err());
    assert!(allowed(b"bplist00", &team_code()).is_err());
    assert!(allowed(&[], &team_code()).is_err());
}

/// macOS answers each account from that account's configuration, so only this process's
/// account's answer counts. NEGATIVE CONTROLS: another account's Allow beside this account's
/// unanswered rule, or with no configuration for this account at all, is no allowance; a
/// name that only starts with this account's UUID is another configuration.
#[test]
fn only_this_accounts_answer_counts() {
    let allow: &[R<'_>] = &[(ID, None, true, false)];
    let unanswered: &[R<'_>] = &[(ID, None, false, true)];
    let (mine, theirs) = (privacy(USER), privacy(OTHER_USER));
    let both = configurations(&[(&theirs, true, allow), (&mine, true, unanswered)]);
    let why = allowed(&both, &team_code()).unwrap_err();
    assert!(why.contains("heard no answer"), "{why}");
    let theirs_alone = configurations(&[(&theirs, true, allow)]);
    let why = allowed(&theirs_alone, &team_code()).unwrap_err();
    assert!(why.contains(USER), "{why}");
    let longer = format!("{mine}0");
    assert!(allowed(&configurations(&[(&longer, true, allow)]), &team_code()).is_err());
    // The same archives read as the other account: its own Allow.
    let other = Code {
        user: OTHER_USER.into(),
        ..team_code()
    };
    assert!(allowed(&both, &other).is_ok());
    assert!(allowed(&theirs_alone, &other).is_ok());
    let lower = Code {
        user: OTHER_USER.to_ascii_lowercase(),
        ..team_code()
    };
    assert!(allowed(&theirs_alone, &lower).is_ok());
}

/// A rule that names a path (nehelper's for code with no team) allows only the executable at
/// that path, ad hoc or team-signed.
#[test]
fn a_path_rule_allows_only_that_executable() {
    let privacy = privacy(USER);
    let privacy = privacy.as_str();
    let at_path = archive(privacy, true, &[(ID, Some(EXE), true, false)]);
    assert!(allowed(&at_path, &ad_hoc_code()).is_ok());
    assert!(allowed(&at_path, &team_code()).is_ok());
    let elsewhere = Code {
        executable: "/Users//a/dist/aterm.app/Contents/MacOS/aterm".into(),
        ..ad_hoc_code()
    };
    let why = allowed(&at_path, &elsewhere).unwrap_err();
    assert!(why.contains("another copy"), "{why}");
    // A refusal beside an allowance for the same code: the allowance is macOS's answer for
    // this copy, and an answered refusal could raise no alert either.
    let both = archive(
        privacy,
        true,
        &[(ID, Some(EXE), true, true), (ID, None, true, false)],
    );
    assert!(allowed(&both, &team_code()).is_ok());
    assert!(allowed(&both, &ad_hoc_code()).is_err());
}

/// nehelper's own files from a macOS 27.2 guest (`docs/measured/lan-update-2026-11-06.md`):
/// released 0.158.0 (team A66A9P66Z7) asked and unanswered; the same rule after the question
/// went away unanswered at a restart; and after the lab wrote the allowance an Allow press
/// writes (the host's own aterm rule: answered, multicast allowed).
#[test]
fn nehelpers_own_files_read_as_measured() {
    let unanswered = include_bytes!("fixtures/nehelper-unanswered.plist");
    let dismissed = include_bytes!("fixtures/nehelper-dismissed.plist");
    let granted = include_bytes!("fixtures/nehelper-granted.plist");
    let why = allowed(unanswered, &team_code()).unwrap_err();
    assert!(why.contains("heard no answer"), "{why}");
    let why = allowed(dismissed, &team_code()).unwrap_err();
    assert!(why.contains("does not allow"), "{why}");
    assert_eq!(
        allowed(granted, &team_code()).unwrap(),
        "macOS allowed com.aterm.aterm (team A66A9P66Z7) on the local network"
    );
    assert!(allowed(granted, &ad_hoc_code()).is_err());
    let wish = Code {
        identifier: "com.tcltk.wish".into(),
        ..team_code()
    };
    assert!(
        allowed(granted, &wish).is_err(),
        "the image's unanswered wish rule"
    );
    // The guest's one account allowed aterm; any other account of that Mac did not.
    let other = Code {
        user: OTHER_USER.into(),
        ..team_code()
    };
    let why = allowed(granted, &other).unwrap_err();
    assert!(why.contains("for this account"), "{why}");
}

/// The lab's allowance ([`allow`]) on nehelper's own files reads back as macOS's answer for
/// exactly that code and account: an ad-hoc build at its path is added in the shape of the
/// configuration's rules; released 0.158.0's dismissed rule is answered in place. NEGATIVE
/// CONTROLS: the same record does not allow another copy, another account, or the image's
/// unanswered wish rule; and a record with no configuration for the account is refused.
#[test]
fn the_labs_allowance_reads_back_as_macos_answer_for_that_code_alone() {
    let unanswered = include_bytes!("fixtures/nehelper-unanswered.plist");
    let dismissed = include_bytes!("fixtures/nehelper-dismissed.plist");
    let ad_hoc = ad_hoc_code();
    let added = allow(unanswered, &ad_hoc).unwrap();
    assert_eq!(
        allowed(&added, &ad_hoc).unwrap(),
        format!("macOS allowed {ID} at {EXE} on the local network")
    );
    let elsewhere = Code {
        executable: "/Users//admin/aterm.app/Contents/MacOS/aterm".into(),
        ..ad_hoc.clone()
    };
    assert!(
        allowed(&added, &elsewhere)
            .unwrap_err()
            .contains("another copy")
    );
    let other = Code {
        user: OTHER_USER.into(),
        ..ad_hoc.clone()
    };
    assert!(
        allowed(&added, &other)
            .unwrap_err()
            .contains("for this account")
    );
    let wish = Code {
        identifier: "com.tcltk.wish".into(),
        ..team_code()
    };
    assert!(
        allowed(&added, &wish)
            .unwrap_err()
            .contains("heard no answer")
    );
    // The team-signed rule 0.158.0's question left unanswered stays as it was: a
    // team-signed copy elsewhere, which the new path rule does not name, reads it.
    let released_elsewhere = Code {
        executable: "/Users//admin/Applications/aterm.app/Contents/MacOS/aterm".into(),
        ..team_code()
    };
    assert!(
        allowed(&added, &released_elsewhere)
            .unwrap_err()
            .contains("heard no answer")
    );
    assert_eq!(
        Plist::parse(&allow(&added, &ad_hoc).unwrap())
            .unwrap()
            .len(),
        Plist::parse(&added).unwrap().len() + 3,
        "allowed again: the rule answered in place, no second rule"
    );

    let answered = allow(dismissed, &team_code()).unwrap();
    assert_eq!(
        allowed(&answered, &team_code()).unwrap(),
        format!("macOS allowed {ID} (team A66A9P66Z7) on the local network")
    );
    assert_eq!(
        Plist::parse(&answered).unwrap().len(),
        Plist::parse(dismissed).unwrap().len() + 3,
        "answered in place: only the answer's three values are added"
    );
    assert!(
        allow(dismissed, &other)
            .unwrap_err()
            .contains("for this account")
    );
}
