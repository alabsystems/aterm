// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Current observed process charges, kept apart from the coalition's independent total.

use super::{Jv, exe_word, rec};
use crate::explain::MemoryMembers;

pub(super) fn text(members: &MemoryMembers) -> String {
    let top = if members.top.is_empty() {
        "-".to_owned()
    } else {
        members
            .top
            .iter()
            .map(|member| {
                format!(
                    "{}:{}:{}",
                    member.pid,
                    exe_word(&member.name),
                    member.footprint
                )
            })
            .collect::<Vec<_>>()
            .join(",")
    };
    let mut text = format!(
        " members={top} members_coverage={} members_readable={} members_unread={} members_missing={}",
        if members.complete {
            "complete"
        } else {
            "partial"
        },
        members.readable,
        members.unread,
        rec(members.missing, "not_sampled")
    );
    if members.ambiguous > 0 {
        text.push_str(&format!(" members_ambiguous={}", members.ambiguous));
    }
    if members.excluded > 0 {
        text.push_str(&format!(" members_excluded={}", members.excluded));
    }
    if members.truncated {
        text.push_str(" members_truncated=1");
    }
    text
}

pub(super) fn json(members: &MemoryMembers) -> Jv {
    Jv::obj([
        ("basis", Jv::s("observed_readable")),
        (
            "coverage",
            Jv::s(if members.complete {
                "complete"
            } else {
                "partial"
            }),
        ),
        ("observed", Jv::u(u64::from(members.observed))),
        ("readable", Jv::u(u64::from(members.readable))),
        ("unread", Jv::u(u64::from(members.unread))),
        ("excluded", Jv::u(u64::from(members.excluded))),
        ("ambiguous", Jv::u(u64::from(members.ambiguous))),
        ("missing", Jv::known_or(members.missing, "not_sampled")),
        ("truncated", Jv::Bool(members.truncated)),
        (
            "top",
            Jv::Arr(
                members
                    .top
                    .iter()
                    .map(|member| {
                        Jv::obj([
                            ("pid", Jv::u(u64::from(member.pid))),
                            ("start", Jv::known_or(member.start, "not_sampled")),
                            (
                                "version",
                                member.version.map_or_else(
                                    || Jv::why("not_sampled"),
                                    |version| Jv::Int(i64::from(version)),
                                ),
                            ),
                            (
                                "birth_ms",
                                member
                                    .birth_ms
                                    .map_or_else(|| Jv::why("not_sampled"), Jv::Int),
                            ),
                            ("at_ns", Jv::known_or(member.at_ns, "not_sampled")),
                            (
                                "exe",
                                if member.name.is_empty() {
                                    Jv::why("not_sampled")
                                } else {
                                    Jv::s(exe_word(&member.name))
                                },
                            ),
                            ("fp_bytes", Jv::u(member.footprint)),
                        ])
                    })
                    .collect(),
            ),
        ),
    ])
}
