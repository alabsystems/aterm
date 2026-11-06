// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Whether macOS already allowed this process's code on the local network, as macOS itself
//! records it: nehelper's rule for the code in its Local Network configuration
//! (`docs/DESIGN-lan-broker-2026-11-05.md` §5, `docs/measured/lan-update-2026-11-06.md`).
//!
//! nehelper keeps one `NEPathRule` per app it asked about, under the enabled `NEPathController` of
//! the configuration named `com.apple.preferences.networkprivacy-<uuid>`, in
//! [`NEHELPER_PLIST`] (an `NSKeyedArchiver` archive, root's, readable by every account and
//! protected by SIP). A rule names the app's signing identifier, and its path when the app has no
//! team to name it by. Measured on macOS 27.2:
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

/// The name every Local Network configuration of nehelper's starts with.
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
    objects: Vec<usize>,
}

impl<'a> Archive<'a> {
    fn open(bytes: &'a [u8]) -> Result<Self, String> {
        let plist = Plist::parse(bytes)?;
        let objects = match plist
            .value(plist.top(), "$objects")
            .map(|i| plist.object(i))
        {
            Some(Ok(Object::Array(objects))) => objects,
            _ => return Err("not an NSKeyedArchiver archive: no $objects".to_owned()),
        };
        Ok(Self { plist, objects })
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

    /// Every rule of every enabled Local Network configuration.
    fn rules(&self) -> Result<Vec<Rule>, String> {
        let mut found = false;
        let mut rules = Vec::new();
        for &object in &self.objects {
            let Some(controller) = self.field(object, "PathController") else {
                continue;
            };
            let named = self
                .text(object, "Name")
                .is_some_and(|name| name.starts_with(NETWORK_PRIVACY));
            if !named
                || self.class(controller).as_deref() != Some("NEPathController")
                || self.flag(controller, "Enabled") != Some(true)
            {
                continue;
            }
            found = true;
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
        if found {
            Ok(rules)
        } else {
            Err("nehelper holds no enabled Local Network configuration".to_owned())
        }
    }
}

/// Whether nehelper's rules (`bytes`, [`NEHELPER_PLIST`]'s) allow `code` on the local network:
/// the allowance's words, or why there is none.
///
/// # Errors
/// No allowance: the words say which (none of the code's rules, one unanswered, a refusal, or an
/// archive this reader cannot read).
pub fn allowed(bytes: &[u8], code: &Code) -> Result<String, String> {
    let archive = Archive::open(bytes)?;
    let rules = archive.rules()?;
    let mine: Vec<&Rule> = rules.iter().filter(|rule| rule.names(code)).collect();
    if let Some(rule) = mine.iter().find(|rule| rule.allows()) {
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
    if let Some(rule) = mine.first() {
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

/// This process's code: its signing identifier and team and its executable's canonical path.
///
/// # Errors
/// Code with no signing identifier, or what reading it said.
pub fn this_code() -> Result<Code, String> {
    let identifier = aterm_keeper::identity::own_identifier()?
        .ok_or_else(|| "this process's code has no signing identifier".to_owned())?;
    let team = aterm_keeper::identity::own_team()?;
    let executable = std::env::current_exe()
        .and_then(std::fs::canonicalize)
        .map_err(|e| format!("this process's executable: {e}"))?;
    Ok(Code {
        identifier,
        team,
        executable: executable.to_string_lossy().into_owned(),
    })
}

/// `path`'s bytes, at most [`crate::bplist::MAX_BYTES`] and one more (which [`allowed`] refuses).
///
/// # Errors
/// The file's open or read, in words.
pub(crate) fn read_bounded(path: &Path) -> Result<Vec<u8>, String> {
    use std::io::Read as _;
    let file = std::fs::File::open(path).map_err(|e| format!("{}: {e}", path.display()))?;
    let mut bytes = Vec::new();
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

    fn team_code() -> Code {
        Code {
            identifier: ID.into(),
            team: Some("A66A9P66Z7".into()),
            executable: EXE.into(),
        }
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
        let mut rule_uids = Vec::new();
        for (identifier, path, answered, deny) in rules {
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
            (key(&mut plist, "Enabled"), boolean(&mut plist, enabled)),
            (key(&mut plist, "Rules"), uid(&mut plist, array)),
        ];
        let controller = push(&mut plist, &mut objs, Object::Dict(pairs));
        let name = push(&mut plist, &mut objs, Object::String(name.into()));
        let pairs = vec![
            (key(&mut plist, "Name"), uid(&mut plist, name)),
            (
                key(&mut plist, "PathController"),
                uid(&mut plist, controller),
            ),
        ];
        push(&mut plist, &mut objs, Object::Dict(pairs));
        plist[1] = Object::Array(objs);
        let objects_key = key(&mut plist, "$objects");
        plist[0] = Object::Dict(vec![(objects_key, 1)]);
        list(&plist, 0)
    }

    const PRIVACY: &str = "com.apple.preferences.networkprivacy-E8D0C90E";

    #[test]
    fn only_an_answered_allow_for_this_code_is_an_allowance() {
        let default = ("PathRuleDefaultNonSystemIdentifier", None, false, true);
        let allowed_team = archive(PRIVACY, true, &[default, (ID, None, true, false)]);
        assert!(allowed(&allowed_team, &team_code()).is_ok());
        // NEGATIVE CONTROLS: the same allowance read for an ad-hoc image (nehelper's rule with no
        // path is a team-signed app's), and every state that is not an answered Allow.
        assert!(allowed(&allowed_team, &ad_hoc_code()).is_err());
        let unanswered = archive(PRIVACY, true, &[default, (ID, None, false, true)]);
        let why = allowed(&unanswered, &team_code()).unwrap_err();
        assert!(why.contains("heard no answer"), "{why}");
        let refused = archive(PRIVACY, true, &[default, (ID, None, true, true)]);
        let why = allowed(&refused, &team_code()).unwrap_err();
        assert!(why.contains("does not allow"), "{why}");
        let other_app = archive(PRIVACY, true, &[("com.tcltk.wish", None, true, false)]);
        let why = allowed(&other_app, &team_code()).unwrap_err();
        assert!(why.contains("no Local Network answer"), "{why}");
        let disabled = archive(PRIVACY, false, &[(ID, None, true, false)]);
        assert!(allowed(&disabled, &team_code()).is_err());
        let other_config = archive("com.example.vpn", true, &[(ID, None, true, false)]);
        assert!(allowed(&other_config, &team_code()).is_err());
        assert!(allowed(b"bplist00", &team_code()).is_err());
        assert!(allowed(&[], &team_code()).is_err());
    }

    /// A rule that names a path (nehelper's for code with no team) allows only the executable at
    /// that path, ad hoc or team-signed.
    #[test]
    fn a_path_rule_allows_only_that_executable() {
        let at_path = archive(PRIVACY, true, &[(ID, Some(EXE), true, false)]);
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
            PRIVACY,
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
    }
}
