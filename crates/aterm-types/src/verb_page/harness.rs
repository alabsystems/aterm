// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! `help harness`: what a self-supervising agent reads of aterm's harness: the `[harness]`
//! policy, the account menu's recovery observation, and a measured fact for an act.

use super::{Page, Section, para, row};

/// The `harness` entry.
pub(crate) const PAGE: Page = Page {
    usage: &[
        para(
            "The harness is aterm's supervisor of an agent's tab: under Autopilot it answers the \
             agent's boxes and continues it at each turn end, with no end (Cruise) or until its \
             work is landed (Land), by the `[harness]` policy. An agent that supervises \
             itself (codex-alab, `supervisor_proven=1`) reads that policy here. A fact is a \
             measured sentence (a stall's no-progress) aterm's own loop lets ride after one of \
             its acts.",
        ),
        row(
            "harness policy",
            "the `[harness]` policy and this session's verdict, what a self-supervising agent \
             reads (any owner connection gets it whole)",
        ),
        row(
            "harness recovery",
            "the account menu's cached recovery observation; probes nothing (owner)",
        ),
        row(
            "harness fact for=<rule> reader=<id> kinds=<kinds> wall=<word>",
            "the session's untold measured fact for an act under <rule> (owner; the standing \
             proven codex-alab self-claim alone)",
        ),
        row(
            "harness fact told episode=<016x>",
            "say the act that carried the fact started",
        ),
    ],
    options: &[],
    reply: &[
        row(
            "OK gen=<n> effective=<on|off> why=<…> contract=<…> …",
            "policy: the verdict, then every `[harness]` key and the composed prompts \
             (`help harness policy`); its `autopilot=` is the table's default, the session's own \
             word is `status autopilot=`; an edge token gets only `OK effective=<on|off>`",
        ),
        row(
            "OK recovery version=1 available=<0|1> …",
            "recovery: phase= account= target= route= … (`help harness recovery`)",
        ),
        row(
            "OK fact=- why=<…> | OK fact=<pct> kind=no-progress …",
            "fact: none and why, or the sentence and the rider to type after the act's words",
        ),
        row("OK | ERR stale fact", "fact told"),
    ],
    examples: &[
        row(
            "aterm ctl @self harness policy",
            "the policy this session's harness runs under",
        ),
        row(
            "aterm ctl @self harness recovery",
            "what the account menu last saw of recovery",
        ),
    ],
    sections: &[
        Section {
            name: "policy",
            about: "the verdict, the contracts served, every key and every composed prompt",
            rows: &[
                para(
                    "Read-only `[harness]` policy for a self-supervising agent (`meta` \
                     `supervisor_proven=1`). `gen=` is the policy generation (`EVENT <local> \
                     harness-policy gen=<n>`); `effective=` and `why=` give this session's \
                     verdict (`unhosted|disabled|headless|agent-off|suspended|hold|autopilot-off|\
                     enabled`), for the agent a rostered harness runs when one leads the session \
                     (Grok Build under grok-alab); `contract=` is the set of harness contracts \
                     served (2: codex-alab's self-claim, 3: a rostered harness's scoped `meta set \
                     supervisor`); ` codex_rotation=1` follows while the native policy's quota \
                     rotation is on.",
                ),
                para(
                    "Followed by every `[harness]` key (pct-encoded, `-` when empty, \
                     `continue_text=` composed with `rules_file`; `autopilot=` is the table \
                     default while the session's word is `status autopilot=`), \
                     `questions=<ask|recommended|long-term|short-term>`, and the composed \
                     Cruise/Land prompts: `done_check_text=` (the first angle's whole check), \
                     `land_text=`, `land_nudge_text=`, `land_answer_text=`, `needs_hint_text=`, \
                     `needs_ack_text=`, then `done_check_angles=` (the done check's six angles in \
                     turn, one per line), `done_check_tail=` (what follows each), \
                     `land_check_text=` (the land check, `land-check@v1`, typed once at a \
                     `Nothing left:` defense over a clean repository: a reply of only `LANDED` \
                     lands, a `NEEDS YOU:` line is a need, anything else is the agent's list of \
                     what is left), `land_question_text=` (the policy text Land writes into a \
                     question dialog's free-text row, `answer-land@v1`), then \
                     `land_checks=git,left`: the checks Land makes before a landing, `git` the \
                     session repository and `left` the processes the session's agent started \
                     that still run (`sys left`; each ended with `end <token>` or kept by a `keep \
                     <token>` line under `Nothing left:`), which a harness that supervises itself \
                     makes the same.",
                ),
                para("Edge tokens receive only `OK effective=<on|off>`."),
            ],
        },
        Section {
            name: "recovery",
            about: "the recovery observation's fields, and what it grants (nothing)",
            rows: &[
                para(
                    "Owner-only `recovery` reads the account menu's cached, current-foreground \
                     observation without probing or retrying: `OK recovery version=1 \
                     available=0` when absent; otherwise `available=1 phase= account= target= \
                     route= known= eligible= wall= observed_at= next_at= forecasts_paused= \
                     detail_count= detail_0= ... login_diagnostic_count= \
                     login_diagnostic_0_account= login_diagnostic_0_completed_at= \
                     login_diagnostic_0_words= ... independent_work_count= \
                     independent_work_0_phase= independent_work_0_account= \
                     independent_work_0_route= independent_work_0_observed_at= ...`.",
                ),
                para(
                    "Indexed independent work is currently observed local pool sign-in, separate \
                     from the primary decision's phase, inventory and deadline. It carries no \
                     eligibility, claim, retry or credential authority; zero means no separate \
                     work observed, not proof that no work exists. Indexed login diagnostics \
                     retain the captured account label, original completion time and validated \
                     outcome words, never raw provider output. They are history and grant no \
                     retry or sign-in authority, including while paused or busy. \
                     `forecasts_paused=1` means the session policy withdrew future recovery \
                     promises independently of current work; 0 grants no authority when the \
                     observation is unknown or busy.",
                ),
                para(
                    "Text is percent-encoded; `-` means unknown or absent, counts may be zero, \
                     times are Unix seconds from the observation (not read time), and a retained \
                     next_at may be past. Phases: unknown, paused, watching, disabled, limited, \
                     waiting, probing, ready, switching, verifying, signing-in. This read never \
                     refreshes credentials.",
                ),
            ],
        },
        Section {
            name: "fact",
            about: "harness fact and fact told: the lease, the why words, the rider",
            rows: &[
                para(
                    "`fact for=<rule> reader=<id> kinds=<kinds> wall=<word>` (Owner; only the \
                     session's standing proven codex-alab self-claim, else `ERR denied`): the \
                     session's untold measured fact under `[harness] facts`, for an act under \
                     `<rule>` that aterm's own loop would let a fact ride (its continuation, done \
                     check, Land instruction or nudge, outside a limit's place), once per stall, \
                     leased to `<id>` for 30 s: `OK fact=- \
                     why=<rule|limit|kinds|stale|none|told|offered|busy|off|facts-off>` or `OK \
                     fact=<pct sentence> kind=no-progress episode=<016x> as_of_ms=<unix ms> \
                     rider=<pct ' (aterm: <sentence>)'>`, the rider typed verbatim after the act's \
                     words and standing rules; `ERR the session ledger is unreadable: <why>` \
                     where the session ledger stands but cannot be read whole (no fact this time; \
                     the next read asks it again).",
                ),
                para(
                    "`fact told episode=<016x>` (Write) says the act that carried it started: \
                     `OK`, or `ERR stale fact` (no offer of that episode to this claim), and \
                     nothing is told.",
                ),
            ],
        },
    ],
};
