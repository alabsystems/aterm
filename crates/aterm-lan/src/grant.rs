// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Whether macOS already allowed this process's code on the local network, as macOS itself
//! records it: nehelper's rule for the code in its Local Network configuration
//! (`docs/DESIGN-lan-broker-2026-11-05.md` §5, `docs/measured/lan-update-2026-11-06.md`).
//!
//! nehelper keeps each account's answers apart: one configuration per account, named
//! `com.apple.preferences.networkprivacy-<the account's GeneratedUID>`, in [`NEHELPER_PLIST`] (an
//! `NSKeyedArchiver` archive, root's, readable by every account and protected by SIP), and under
//! its enabled `NEPathController` one `NEPathRule` per app it asked that account about. Only this
//! process's account's configuration is read: another account's Allow is that account's answer,
//! and macOS would ask this one. A rule names the app's signing identifier, and its path when the
//! app has no team to name it by. Measured on macOS 27.2:
//!
//! | The person | `MulticastPreferenceSet` | `DenyMulticast` |
//! | --- | --- | --- |
//! | was asked and has not answered | `false` | `true` |
//! | pressed Allow (this Mac's own rules for aterm builds a person allowed) | `true` | `false` |
//! | was asked and the question went away unanswered at a restart (recorded as a refusal) | `true` | `true` |
//!
//! Only the second is an allowance. macOS asks only code it holds no answer for, so a process
//! this module reads as allowed raises no Local Network alert by its own LAN use; every other
//! reading (no rule, one unanswered, a refusal, a list this reader cannot read)
//! is no allowance. Reading the file raises nothing: it is a plain read of a world-readable file
//! outside every privacy-protected folder.

use std::path::Path;

use crate::bplist::{Object, Plist};

/// nehelper's configuration store, its Local Network rules among them.
pub const NEHELPER_PLIST: &str = "/Library/Preferences/com.apple.networkextension.plist";

/// The name every Local Network configuration of nehelper's starts with; `-<GeneratedUID>`
/// names whose it is.
const NETWORK_PRIVACY: &str = "com.apple.preferences.networkprivacy";

/// The code a rule is matched against: this process's.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Code {
    /// Its signing identifier (`com.aterm.aterm`).
    pub identifier: String,
    /// Its signing team; `None` for an ad-hoc image.
    pub team: Option<String>,
    /// Its executable's canonical path.
    pub executable: String,
    /// The directory UUID (`GeneratedUID`) of the account it runs as, whose configuration macOS
    /// answers it from.
    pub user: String,
}

/// One Local Network rule of nehelper's, as read.
#[derive(Clone, Debug, PartialEq, Eq)]
struct Rule {
    identifier: String,
    path: Option<String>,
    answered: bool,
    deny_multicast: bool,
    deny_all: bool,
}

impl Rule {
    /// Whether the rule is for `code`: its identifier, and its path when it names one; a rule
    /// with no path is nehelper's for a team-signed app, and an ad-hoc image (no team) is never
    /// read as that app.
    fn names(&self, code: &Code) -> bool {
        self.identifier == code.identifier
            && match &self.path {
                Some(path) => *path == code.executable,
                None => code.team.is_some(),
            }
    }

    const fn allows(&self) -> bool {
        self.answered && !self.deny_multicast && !self.deny_all
    }
}

/// The archive's objects table and the reads its references need.
struct Archive<'a> {
    plist: Plist<'a>,
    /// The `$objects` array's own object index.
    #[cfg_attr(
        not(any(test, debug_assertions)),
        expect(dead_code, reason = "read by the debug-only allow")
    )]
    objects_at: usize,
    objects: Vec<usize>,
}

impl<'a> Archive<'a> {
    fn open(bytes: &'a [u8]) -> Result<Self, String> {
        let plist = Plist::parse(bytes)?;
        let objects_at = plist.value(plist.top(), "$objects");
        let objects = match objects_at.map(|i| plist.object(i)) {
            Some(Ok(Object::Array(objects))) => objects,
            _ => return Err("not an NSKeyedArchiver archive: no $objects".to_owned()),
        };
        Ok(Self {
            plist,
            objects_at: objects_at.unwrap_or_default(),
            objects,
        })
    }

    /// The plist object a `CF$UID` object names.
    fn target(&self, uid_object: usize) -> Option<usize> {
        match self.plist.object(uid_object) {
            Ok(Object::Uid(n)) => usize::try_from(n)
                .ok()
                .and_then(|n| self.objects.get(n).copied()),
            _ => None,
        }
    }

    /// Dictionary `dict`'s `key`, followed through its `CF$UID`.
    fn field(&self, dict: usize, key: &str) -> Option<usize> {
        self.target(self.plist.value(dict, key)?)
    }

    /// `dict`'s `key` as a string through its `CF$UID`; `$null` (the archive's nil) is `None`.
    fn text(&self, dict: usize, key: &str) -> Option<String> {
        self.plist
            .string(self.field(dict, key)?)
            .filter(|text| text != "$null")
    }

    fn flag(&self, dict: usize, key: &str) -> Option<bool> {
        match self.plist.object(self.plist.value(dict, key)?) {
            Ok(Object::Bool(b)) => Some(b),
            _ => None,
        }
    }

    /// The `$classname` of `dict`'s class.
    fn class(&self, dict: usize) -> Option<String> {
        let class = self.field(dict, "$class")?;
        self.plist.string(self.plist.value(class, "$classname")?)
    }

    /// The objects an archived `NSArray` (`NS.objects`) holds.
    fn array(&self, dict: usize) -> Vec<usize> {
        match self
            .plist
            .value(dict, "NS.objects")
            .map(|i| self.plist.object(i))
        {
            Some(Ok(Object::Array(refs))) => {
                refs.into_iter().filter_map(|r| self.target(r)).collect()
            }
            _ => Vec::new(),
        }
    }

    /// The enabled `NEPathController` of each Local Network configuration of `user`'s (a
    /// GeneratedUID).
    fn controllers(&self, user: &str) -> Vec<usize> {
        self.objects
            .iter()
            .filter_map(|&object| {
                let controller = self.field(object, "PathController")?;
                let named = self.text(object, "Name").is_some_and(|name| {
                    name.strip_prefix(NETWORK_PRIVACY)
                        .and_then(|rest| rest.strip_prefix('-'))
                        .is_some_and(|whose| whose.eq_ignore_ascii_case(user))
                });
                (named
                    && self.class(controller).as_deref() == Some("NEPathController")
                    && self.flag(controller, "Enabled") == Some(true))
                .then_some(controller)
            })
            .collect()
    }

    /// Every rule of `user`'s enabled Local Network configuration (`user` a GeneratedUID).
    fn rules(&self, user: &str) -> Result<Vec<Rule>, String> {
        let controllers = self.controllers(user);
        if controllers.is_empty() {
            return Err(no_configuration(user));
        }
        let mut rules = Vec::new();
        for controller in controllers {
            let Some(list) = self.field(controller, "Rules") else {
                continue;
            };
            for rule in self.array(list) {
                if self.class(rule).as_deref() != Some("NEPathRule") {
                    continue;
                }
                let (Some(identifier), Some(answered), Some(deny_multicast)) = (
                    self.text(rule, "SigningIdentifier"),
                    self.flag(rule, "MulticastPreferenceSet"),
                    self.flag(rule, "DenyMulticast"),
                ) else {
                    continue;
                };
                rules.push(Rule {
                    identifier,
                    path: self.text(rule, "Path"),
                    answered,
                    deny_multicast,
                    deny_all: self.flag(rule, "DenyAll").unwrap_or(true),
                });
            }
        }
        Ok(rules)
    }
}

fn no_configuration(user: &str) -> String {
    format!("nehelper holds no enabled Local Network configuration for this account ({user})")
}

/// nehelper's record (`bytes`) with the answer an Allow press writes for `code`, for `code`'s
/// account: its rule for the code (a team-signed code's has no path, an ad-hoc one's names its
/// executable) answered, multicast allowed and not deny-all, added in the shape of the
/// configuration's own rules when there is none. Every other object stays as nehelper wrote
/// it. The lab's guest base writes it with SIP off (`lab mac nux lan-allowed`), so a person's
/// press, which the lab never fakes, is not needed to measure what follows it; [`allowed`]
/// reads the result as the allowance, or this refuses.
///
/// # Errors
/// No configuration for the account, no rule to take the shape of, a rule missing a key the
/// answer sets, or a record the rewrite cannot write.
#[cfg(any(test, debug_assertions))]
pub fn allow(bytes: &[u8], code: &Code) -> Result<Vec<u8>, String> {
    use std::collections::BTreeMap;
    const SETS: [&str; 7] = [
        "SigningIdentifier",
        "Path",
        "DesignatedRequirement",
        "AllowEmptyDesignatedRequirement",
        "MulticastPreferenceSet",
        "DenyMulticast",
        "DenyAll",
    ];
    let archive = Archive::open(bytes)?;
    let plist = &archive.plist;
    if archive
        .objects
        .first()
        .and_then(|&null| plist.string(null))
        .as_deref()
        != Some("$null")
    {
        return Err("an archive whose first object is not $null".to_owned());
    }
    let controller = *archive
        .controllers(&code.user)
        .first()
        .ok_or_else(|| no_configuration(&code.user))?;
    let list = archive
        .field(controller, "Rules")
        .ok_or_else(|| "the configuration has no Rules".to_owned())?;
    let ns = plist
        .value(list, "NS.objects")
        .ok_or_else(|| "the Rules array has no NS.objects".to_owned())?;
    let Ok(Object::Array(rule_refs)) = plist.object(ns) else {
        return Err("the Rules array's NS.objects is not an array".to_owned());
    };
    let rules: Vec<usize> = rule_refs
        .iter()
        .filter_map(|&r| archive.target(r))
        .filter(|&rule| archive.class(rule).as_deref() == Some("NEPathRule"))
        .collect();
    // The objects this appends: the answer's values first, then what a new rule needs.
    let mut next = plist.len();
    let mut appended = Vec::new();
    let mut push = |object: Object| {
        appended.push(object);
        next += 1;
        next - 1
    };
    let yes = push(Object::Bool(true));
    let no = push(Object::Bool(false));
    let null = push(Object::Uid(0));
    let answer = |key: &str| match key {
        "MulticastPreferenceSet" | "AllowEmptyDesignatedRequirement" => Some(yes),
        "DenyMulticast" | "DenyAll" => Some(no),
        _ => None,
    };
    let mine = rules.iter().copied().find(|&rule| {
        archive.text(rule, "SigningIdentifier").as_deref() == Some(code.identifier.as_str())
            && match archive.text(rule, "Path") {
                Some(path) => path == code.executable,
                None => code.team.is_some(),
            }
    });
    let mut replaced = BTreeMap::new();
    if let Some(rule) = mine {
        let Ok(Object::Dict(pairs)) = plist.object(rule) else {
            return Err("this code's rule is not a dictionary".to_owned());
        };
        let pairs = pairs
            .into_iter()
            .map(|(k, v)| {
                let set = plist.string(k).and_then(|key| answer(&key));
                (k, set.unwrap_or(v))
            })
            .collect();
        replaced.insert(rule, Object::Dict(pairs));
    } else {
        let shape = rules
            .iter()
            .copied()
            .find(|&rule| {
                archive.text(rule, "SigningIdentifier").as_deref()
                    != Some("PathRuleDefaultNonSystemIdentifier")
            })
            .or_else(|| rules.first().copied())
            .ok_or_else(|| "the configuration holds no rule to take the shape of".to_owned())?;
        let Ok(Object::Dict(pairs)) = plist.object(shape) else {
            return Err("a rule that is not a dictionary".to_owned());
        };
        let keys: Vec<String> = pairs.iter().filter_map(|(k, _)| plist.string(*k)).collect();
        if let Some(missing) = SETS.iter().find(|key| !keys.iter().any(|k| k == *key)) {
            return Err(format!(
                "nehelper's rules have no {missing}: a shape this does not know"
            ));
        }
        let archived = archive.objects.len();
        let identifier = push(Object::String(code.identifier.clone()));
        let identifier_uid = push(Object::Uid(archived as u64));
        let mut added = vec![identifier];
        let path_uid = if code.team.is_some() {
            null
        } else {
            added.push(push(Object::String(code.executable.clone())));
            push(Object::Uid((archived + 1) as u64))
        };
        let pairs = pairs
            .into_iter()
            .map(|(k, v)| {
                let key = plist.string(k).unwrap_or_default();
                let set = match key.as_str() {
                    "SigningIdentifier" => Some(identifier_uid),
                    "Path" => Some(path_uid),
                    "DesignatedRequirement" => Some(null),
                    other => answer(other),
                };
                (k, set.unwrap_or(v))
            })
            .collect();
        let rule = push(Object::Dict(pairs));
        let rule_uid = push(Object::Uid((archived + added.len()) as u64));
        added.push(rule);
        let mut refs = rule_refs.clone();
        refs.push(rule_uid);
        replaced.insert(ns, Object::Array(refs));
        let mut objects = archive.objects.clone();
        objects.extend(added);
        replaced.insert(archive.objects_at, Object::Array(objects));
    }
    let out = plist.rewrite(&replaced, &appended)?;
    allowed(&out, code)
        .map(|_| out)
        .map_err(|why| format!("the rewritten record does not read as allowed: {why}"))
}

/// Whether nehelper's rules (`bytes`, [`NEHELPER_PLIST`]'s) for `code`'s account allow `code` on
/// the local network: the allowance's words, or why there is none.
///
/// # Errors
/// No allowance: the words say which (no configuration for the account, none of the code's
/// rules, one unanswered, a refusal, or an archive this reader cannot read).
pub fn allowed(bytes: &[u8], code: &Code) -> Result<String, String> {
    let archive = Archive::open(bytes)?;
    let rules = archive.rules(&code.user)?;
    if let Some(rule) = rules.iter().find(|rule| rule.names(code) && rule.allows()) {
        return Ok(match &rule.path {
            Some(path) => format!(
                "macOS allowed {} at {path} on the local network",
                rule.identifier
            ),
            None => format!(
                "macOS allowed {} (team {}) on the local network",
                rule.identifier,
                code.team.as_deref().unwrap_or("-")
            ),
        });
    }
    if let Some(rule) = rules.iter().find(|rule| rule.names(code)) {
        return Err(if rule.answered {
            format!(
                "macOS does not allow {} on the local network",
                code.identifier
            )
        } else {
            format!(
                "macOS asked about {} on the local network and heard no answer",
                code.identifier
            )
        });
    }
    if rules.iter().any(|rule| rule.identifier == code.identifier) {
        return Err(format!(
            "macOS's Local Network rules for {} name another copy of it, not {} ({})",
            code.identifier,
            code.executable,
            code.team
                .as_deref()
                .map_or_else(|| "ad hoc".to_owned(), |team| format!("team {team}"))
        ));
    }
    Err(format!(
        "macOS holds no Local Network answer for {}",
        code.identifier
    ))
}

/// This process's code: its signing identifier and team, its executable's canonical path, and
/// its account's directory UUID.
///
/// # Errors
/// Code with no signing identifier, an account the directory names no UUID for, or what reading
/// either said.
pub fn this_code() -> Result<Code, String> {
    let identifier = aterm_keeper::identity::own_identifier()?
        .ok_or_else(|| "this process's code has no signing identifier".to_owned())?;
    let team = aterm_keeper::identity::own_team()?;
    let executable = std::env::current_exe()
        .and_then(std::fs::canonicalize)
        .map_err(|e| format!("this process's executable: {e}"))?;
    let user = aterm_containment::consent::user_uuid()
        .ok_or_else(|| "macOS's directory names no UUID for this account".to_owned())?;
    Ok(Code {
        identifier,
        team,
        executable: executable.to_string_lossy().into_owned(),
        user,
    })
}

/// `path`'s bytes, at most [`crate::bplist::MAX_BYTES`] and one more (which [`allowed`] refuses).
///
/// # Errors
/// The file's open or read, in words.
pub(crate) fn read_bounded(path: &Path) -> Result<Vec<u8>, String> {
    use std::io::Read as _;
    let file = std::fs::File::open(path).map_err(|e| format!("{}: {e}", path.display()))?;
    let cap = file
        .metadata()
        .ok()
        .and_then(|m| usize::try_from(m.len()).ok())
        .map_or(0, |len| len.min(crate::bplist::MAX_BYTES).saturating_add(1));
    let mut bytes = Vec::with_capacity(cap);
    file.take(u64::try_from(crate::bplist::MAX_BYTES).unwrap_or(u64::MAX) + 1)
        .read_to_end(&mut bytes)
        .map_err(|e| format!("{}: {e}", path.display()))?;
    Ok(bytes)
}

#[cfg(test)]
mod tests {
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
}
