// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Shared vendor-file readers and matchers for deterministic fixtures and the
//! explicit vendor-drift bench. No vendor executable is started.

use crate::anchors::*;

/// Whether `needle` occurs in `hay` (Boyer-Moore-Horspool: a 225 MB binary
/// is scanned in a fraction of a second per needle, where a window-by-
/// window compare took most of a minute for the table).
pub(crate) fn contains_bytes(hay: &[u8], needle: &[u8]) -> bool {
    let n = needle.len();
    if n == 0 {
        return true;
    }
    if hay.len() < n {
        return false;
    }
    let mut skip = [n; 256];
    for (i, &b) in needle[..n - 1].iter().enumerate() {
        skip[usize::from(b)] = n - 1 - i;
    }
    let mut at = 0;
    while at + n <= hay.len() {
        if &hay[at..at + n] == needle {
            return true;
        }
        at += skip[usize::from(hay[at + n - 1])];
    }
    false
}

/// The version a `--version` line names: a dotted one (`2.1.286 (Claude
/// Code)`, `codex-cli 0.130.0`, agy's bare `1.4.0`), or Muse Code's with
/// its revision, `x.y.z-R<n>.<s>` — `Muse Code 1.4.9 (1.4.9-R6297.1)`
/// names both, and the revision is the build its anchors were read on.
pub(crate) fn version_in(text: &str) -> Option<String> {
    let dotted = |v: &str| {
        v.split('.').count() >= 2
            && v.split('.')
                .all(|p| !p.is_empty() && p.bytes().all(|b| b.is_ascii_digit()))
    };
    let words: Vec<&str> = text
        .split_whitespace()
        .map(|w| w.trim_matches(|c: char| matches!(c, '(' | ')' | ',')))
        .collect();
    words
        .iter()
        .find(|w| {
            w.split_once("-R")
                .is_some_and(|(v, rev)| dotted(v) && (dotted(rev) || rev.parse::<u64>().is_ok()))
        })
        .or_else(|| words.iter().find(|w| dotted(w)))
        .map(|w| (*w).to_string())
}

pub(crate) fn contains(bytes: &[u8], needle: &str) -> bool {
    contains_bytes(bytes, needle.as_bytes())
}

/// One of the limit reset's templates, whole, as the bundle spells it
/// ([`reset_templates`]): the row it stands for, and the builds that
/// spell it so — from `since` through `last`, as an anchor's are
/// ([`Anchor::drawn_by`]).
pub(crate) struct Template {
    pub(crate) id: &'static str,
    pub(crate) since: &'static str,
    pub(crate) last: Option<&'static str>,
    pub(crate) text: String,
}

impl Template {
    pub(crate) fn spelled_by(&self, version: Option<&str>) -> bool {
        Anchor {
            last: self.last,
            id: self.id,
            text: "",
            since: self.since,
            kind: AnchorKind::Literal,
        }
        .drawn_by(version)
    }
}

/// THE LIMIT RESET'S TEMPLATES, WHOLE, as the bundle spells them — a `·`
/// as its `\xB7` escape, a value as its `{placeholder}` — each built
/// from the rows the readers match by (`crate::reset`) and named by the
/// row it stands for: the grant row, the confirmation's body and its
/// detail, and the opening of each answer a reader tells apart; and from
/// 2.1.304 on, which names the kind of reset, the confirmation's title,
/// its yes and its early lines' tail too, each with its `{kind}`.
///
/// The literal-anchor matcher looks for each side of a `·` on its own, and the sides of
/// the confirmation's two joins are `now`, `left` and `use by`: words a
/// build that rewords the confirmation still carries elsewhere. A
/// template is ONE literal of the vendor's table, so a reworded one is
/// missing — and what a rewording costs is every confirmation read
/// inexact and Escaped, the wall's one attempt with it.
pub(crate) fn reset_templates() -> Vec<Template> {
    let escaped = |id: &str| anchor(id).replace('·', "\\xB7");
    let led = |id: &str| format!("{} \\xB7 ", anchor(id));
    let stays = anchor("reset.confirm.stays");
    let (now, week) = stays.split_once(" · ").expect("the week's join");
    let use_by = escaped("reset.confirm.use_by");
    let old = |id, text| Template {
        id,
        since: "2.1.287",
        last: Some("2.1.303"),
        text,
    };
    let any = |id, text| Template {
        id,
        since: "2.1.287",
        last: None,
        text,
    };
    let kinds = |id, text| Template {
        id,
        since: "2.1.304",
        last: None,
        text,
    };
    vec![
        old(
            "reset.row.until",
            format!(
                "{} ({{left}} {} {{date}})",
                anchor("reset.row"),
                anchor("reset.row.until")
            ),
        ),
        old(
            "reset.confirm.stays",
            format!(
                "{} {{limits}} {} {{week}}",
                anchor("reset.confirm.refills"),
                escaped("reset.confirm.stays")
            ),
        ),
        old(
            "reset.confirm.use_by",
            format!("{{resets}} {use_by} {{deadline}}"),
        ),
        old("reset.done", led("reset.done")),
        old("reset.kept", led("reset.kept")),
        any("reset.lifted", escaped("reset.lifted")),
        any("reset.login", led("reset.login")),
        kinds(
            "reset.kind.row",
            format!(
                "{} {{kind}} ({{left}} {} {{date}})",
                anchor("reset.kind.row"),
                anchor("reset.row.until")
            ),
        ),
        kinds(
            "reset.kind.title",
            format!("{} {{kind}}?", anchor("reset.kind.row")),
        ),
        kinds(
            "reset.kind.yes",
            format!("{} {{kind}}", anchor("reset.kind.yes")),
        ),
        // The body: 2.1.304 put a `{returns}` segment between the two,
        // which only a Fable reset fills (`Ot()`); a segment with no
        // value is dropped, so every other kind's body is drawn as
        // before.
        kinds(
            "reset.confirm.stays",
            format!(
                "{} {{limits}} {now} \\xB7 {{returns}} \\xB7 {week} {{week}}",
                anchor("reset.confirm.refills"),
            ),
        ),
        kinds(
            "reset.confirm.use_by",
            format!("{{kindResets}} {use_by} {{deadline}}"),
        ),
        kinds(
            "reset.kind.early",
            format!(
                "{} {{kind}} {}",
                anchor("reset.kind.early"),
                anchor("reset.kind.anyway")
            ),
        ),
        kinds(
            "reset.done",
            format!("{{done}} \\xB7 {week} {{week}} \\xB7 {{kindResets}} left"),
        ),
        kinds(
            "reset.kind.kept",
            format!("{{Kind}} {} \\xB7 ", anchor("reset.kind.kept")),
        ),
        kinds("reset.kept.choice", led("reset.kept.choice")),
    ]
}

/// The rows whose template a build of `version` spells and `bytes` do
/// not hold whole.
pub(crate) fn missing_templates(bytes: &[u8], version: Option<&str>) -> Vec<&'static str> {
    reset_templates()
        .iter()
        .filter(|t| t.spelled_by(version) && !contains(bytes, &t.text))
        .map(|t| t.id)
        .collect()
}

/// A rollout key no Codex build or rollout holds: the key tests'
/// negative control.
pub(crate) const PLANTED_ROLLOUT_KEY: &[Anchor] = &[Anchor {
    id: "rollout.planted",
    text: "turn_context_planted_by_no_build",
    since: "0.163.0",
    last: None,
    kind: AnchorKind::Literal,
}];
