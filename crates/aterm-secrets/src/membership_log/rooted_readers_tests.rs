// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Readers of a rooted log's dropped records outside its evaluators (tracker FL-148E3): a
//! settle of a Claude Code token pair whose carry stands for a dropped op, and the search for
//! the log a Mac applied whose head a root dropped.

use super::*;
use crate::claude_token::{Key, Plan};
use crate::membership_log::entries::OUTER_ONLY;

/// The token values: setup-tokens by their grammar, and inert.
const TOKEN: &[u8] = concat!("sk-ant-", "oat01-inert_fixture-token-0123456789").as_bytes();
const NEXT: &[u8] = concat!("sk-ant-", "oat01-inert_fixture-token-replaced").as_bytes();

/// A meta naming when its token was minted (`minted_at`), which a settle ranks metas by.
fn meta(minted_at: u64) -> Vec<u8> {
    format!(r#"{{"minted_at":{minted_at}}}"#).into_bytes()
}

/// What a settle of `key`'s pair plans on `log`, as `mac` reads it at `now` (unix seconds):
/// what it would write (each name, value and the ops it settles), or why it writes nothing.
fn planned(log: &Log, mac: Mac, key: &Key, now: u64) -> String {
    let home = aterm_tempfile::tempdir().expect("a home");
    let paths = crate::client::Paths::for_home(home.path());
    let by = std::time::Instant::now() + aterm_spec::wait::MUST_HAPPEN;
    match crate::claude_token::plan_of(&paths, log, &mac.identity(), key, now, by).expect("a plan")
    {
        Plan::Done(settled) => format!("{settled:?}"),
        Plan::Write(entries) => entries
            .iter()
            .map(|entry| {
                format!(
                    "{} = {} settling {:?}",
                    entry.name,
                    String::from_utf8_lossy(entry.value.as_slice()),
                    entry.settles
                )
            })
            .collect::<Vec<_>>()
            .join("\n"),
    }
}

/// A Claude Code token pair (the token and its meta, each a register) whose meta stands for a
/// draft a final checkpoint's prune dropped, as both logs hold it ([`split_pair`]).
struct SplitPair {
    key: Key,
    /// The whole log, and the log rooted at the checkpoint.
    log: Log,
    rooted: Log,
    /// A's first draft, which the prune dropped.
    first_token: Record,
    first_meta: Record,
    /// The token register's two ops, B's and C's, and the meta register's one, C's carry.
    b_token: Record,
    c_token: Record,
    c_meta: Record,
}

/// A writes the pair (draft `first`: `TOKEN` beside a meta minted at 200); B, on the log that
/// holds it alone, puts `TOKEN` again while A writes the pair again (`NEXT` beside a meta minted
/// at 100) superseding it; A and B share the checkpoint whose prune drops A's first draft. B then
/// removes A at that draft, so A's second stops counting and the first stays dropped
/// (`vault_checkpoint_a_remove_behind_a_final_cut_revives_nothing`), and C, waking on the log that
/// holds the first draft alone, puts `TOKEN` again and reseals the meta. The token register holds
/// B's and C's `TOKEN`, the meta register C's carry, which stands for A's dropped first meta.
/// `carries` says whether B's and C's token `Put`s are carries of A's first (a reseal) or fresh
/// writes of the same value.
fn split_pair(carries: bool) -> SplitPair {
    let key = Key::parse("ac-0123456789abcdef").expect("an account key");
    let (token, meta_name) = (key.entry(), key.meta_entry());
    let mut log = found(&[A, B, C]);
    let t0 = T - 5 * DAY_MS;
    let (e, epoch_key) = epoch(&mut log, A, t0, EpochAudience::Everyone, 1, &[A, B, C]);
    let put = |log: &Log, by: Mac, at: u64, name: &str, value: &[u8]| {
        log.draft_put(
            &by.id(),
            at,
            e,
            &epoch_key,
            &entry(&EpochAudience::Everyone, name, value),
        )
        .expect("a put")
    };
    let none = Statement::default();
    let first_token = held!(
        log,
        two(put(&log, A, t0 + HOUR, &token, TOKEN), A, Some(&none))
    );
    let first_meta = held!(
        log,
        two(
            put(&log, A, t0 + HOUR, &meta_name, &meta(200)),
            A,
            Some(&none)
        )
    );
    let token_carry = carries.then_some(&first_token);
    let mut stale = log.clone();
    // B puts the token on the log that holds the first draft alone; A, not yet holding it,
    // writes the pair again.
    let b_token = put(&stale, B, t0 + HOUR + HOUR / 2, &token, TOKEN);
    held!(
        log,
        two(
            put(&log, A, t0 + 2 * HOUR, &token, NEXT),
            A,
            Some(&supersedes(&[&first_token], None))
        )
    );
    held!(
        log,
        two(
            put(&log, A, t0 + 2 * HOUR, &meta_name, &meta(100)),
            A,
            Some(&supersedes(&[&first_meta], None))
        )
    );
    let b_token = held!(
        log,
        two(b_token, B, Some(&supersedes(&[&first_token], token_carry)))
    );
    held!(log, one(draft(&log, B, t0 + 3 * HOUR, name("b1")), B));
    let plan = propose(&log, NOW).expect("a proposal");
    let mut dropped = vec![*first_token.id(), *first_meta.id()];
    dropped.sort_unstable();
    assert_eq!(plan.dropped, dropped, "the prune drops A's first draft");
    share_at(&mut log, A, &plan.body, T + 2 * DAY_MS);
    share_at(&mut log, B, &plan.body, T + 2 * DAY_MS + HOUR);
    let mut rooted = log.adopt(&plan.body).expect("a final checkpoint");

    // B removes A at its first draft, so A's second stops counting; C, waking on the log that
    // holds the first draft alone, puts the token and reseals the meta.
    let remove = one(
        draft(
            &log,
            B,
            T + 3 * DAY_MS,
            Body::Remove {
                member: A.id(),
                cutoff: *first_meta.id(),
            },
        ),
        B,
    );
    let at = T + 4 * DAY_MS;
    let c_token = held!(
        stale,
        two(
            put(&stale, C, at, &token, TOKEN),
            C,
            Some(&supersedes(&[&first_token], token_carry))
        )
    );
    let c_meta = held!(
        stale,
        two(
            put(&stale, C, at, &meta_name, &meta(200)),
            C,
            Some(&supersedes(&[&first_meta], Some(&first_meta)))
        )
    );
    for record in [&remove, &c_token, &c_meta] {
        hold(&mut log, record.clone());
        assert_eq!(rooted.insert(record.clone()).expect("held"), Inserted::Held);
    }
    assert!(rooted.is_dropped(first_token.id()) && rooted.is_dropped(first_meta.id()));

    // The meta's carry stands for A's dropped first meta in both logs, and the token is split.
    for opened in [&log, &rooted].map(|log| {
        log.open_audience_entries(&B.identity())
            .expect("opened")
            .expect("B reads the vault")
    }) {
        for put in [&b_token, &c_token] {
            let stands = if carries { &first_token } else { put };
            assert_eq!(opened.stands_for(put.id()), *stands.id());
        }
        assert_eq!(opened.stands_for(c_meta.id()), *first_meta.id());
        assert_eq!(opened.everyone_ops(&token).len(), 2, "the token is split");
    }
    SplitPair {
        key,
        log,
        rooted,
        first_token,
        first_meta,
        b_token,
        c_token,
        c_meta,
    }
}

impl SplitPair {
    /// The settle that keeps `TOKEN` beside the meta minted at 200, settling both registers.
    fn keeps_token(&self) -> String {
        format!(
            "{} = {} settling {:?}\n{} = {} settling {:?}",
            self.key.entry(),
            String::from_utf8_lossy(TOKEN),
            Some(
                [*self.b_token.id(), *self.c_token.id()]
                    .into_iter()
                    .collect::<BTreeSet<_>>()
            ),
            self.key.meta_entry(),
            String::from_utf8_lossy(&meta(200)),
            Some(BTreeSet::from([*self.c_meta.id()])),
        )
    }

    /// What B and C each plan on `log`.
    fn planned(&self, log: &Log) -> Vec<String> {
        let now = (T + 5 * DAY_MS) / 1_000;
        [B, C].map(|mac| planned(log, mac, &self.key, now)).into()
    }
}

/// A Claude Code token pair split between two carries of a draft a final checkpoint's prune
/// dropped ([`split_pair`] with carries): B's and C's token `Put`s each reseal A's dropped first
/// token, and C's meta its first meta, kept as stubs.
///
/// A settle reads which draft each meta stands for (a carry stands for its rep) and keeps the
/// token that draft wrote beside it. In the rooted log the rep is a stub, and the stub decides it
/// as the record did: its signer, clock and epoch name the draft ([`Log::drafted`]), and the
/// token that draft put is the one the token register's carry of it holds, which no stub holds.
/// Both logs plan the same, for B and for C: one Mac's turn writes `TOKEN` beside the meta
/// minted at 200, settling the carries.
///
/// NEGATIVE CONTROLS, each made in `claude_token::plan_of`, run against this test and seen to
/// fail with ``assertion `left == right` failed: the rooted log settles as the whole log``:
/// - `draft_of` reading `log.get(id)` alone, as before: `left` is `Held("a write of the pair is
///   a removal, or does not open here")` for both Macs.
/// - no `carried_token` (no op of the token register is read as the dropped token `Put`):
///   `left` is `Held("a write a meta of the pair stands for was dropped by a checkpoint")` for
///   both Macs.
#[test]
fn vault_checkpoint_a_dropped_reps_stub_still_decides_an_entrys_winning_op() {
    // The pair's values as a build before the inner seal holds them: no epoch's inner key is
    // needed to read them, and what the settle decides is the same.
    OUTER_ONLY.with(|outer| outer.set(true));
    let pair = split_pair(true);
    let whole = pair.planned(&pair.log);
    assert!(
        whole.contains(&pair.keeps_token()),
        "the whole log keeps the first draft's token beside its meta: {whole:?}"
    );
    assert_eq!(
        pair.planned(&pair.rooted),
        whole,
        "the rooted log settles as the whole log"
    );
    OUTER_ONLY.with(|outer| outer.set(false));
}

/// A Claude Code token pair whose meta stands for a draft whose token `Put` a prune dropped, with
/// no carry of that `Put` left ([`split_pair`] without carries): B and C each put `TOKEN` afresh,
/// and C's meta reseals A's dropped first meta.
///
/// The whole log reads the token A's first draft put, `TOKEN`, which the token register holds,
/// and keeps it beside the meta minted at 200. The rooted log holds a stub of that `Put`, which
/// keeps no value, and no op of the token register carries it: it cannot tell which token the
/// meta stands beside, so it settles nothing, and says why, rather than read the meta as beside
/// no token, which a log with a second meta beside another token would settle to that token.
///
/// NEGATIVE CONTROL: `claude_token::plan_of` skipping such a meta (`None => continue`, as
/// before), run against this test and seen to fail with ``assertion `left == right` failed: a
/// rooted log holds a pair whose dropped token it cannot read``, `left` being `Held("no meta
/// written beside a token the pair holds")` for both Macs.
#[test]
fn vault_checkpoint_a_rooted_log_holds_a_pair_whose_dropped_token_no_carry_keeps() {
    OUTER_ONLY.with(|outer| outer.set(true));
    let pair = split_pair(false);
    let whole = pair.planned(&pair.log);
    assert!(
        whole.contains(&pair.keeps_token()),
        "the whole log keeps the token A's first draft put beside its meta: {whole:?}"
    );
    let dropped = pair.rooted.rooting().expect("rooted");
    assert!(
        dropped.stubs.contains_key(pair.first_token.id())
            && dropped.stubs.contains_key(pair.first_meta.id())
    );
    let hold_it = format!(
        "{:?}",
        crate::claude_token::Settled::Held(
            "a write a meta of the pair stands for was dropped by a checkpoint".to_owned()
        )
    );
    assert_eq!(
        pair.planned(&pair.rooted),
        vec![hold_it; 2],
        "a rooted log holds a pair whose dropped token it cannot read"
    );
    OUTER_ONLY.with(|outer| outer.set(false));
}

/// A rooted log finds the log a Mac applied where a root of it dropped that log's head. A held
/// the vault at A's last share of the older checkpoint, its newest record and its log's one
/// head ([`dropped_head`]), and applied it there; the prune drops that share, and a record
/// lands after it. The rooted log's search names the applied heads by the digest the Mac kept,
/// as the whole log's does, whether the envelope's stamp names them (the hint) or not (a
/// dropped record alone): so a writer enrolls the rooted log as holding what it applied
/// (`client::log_entries`' `legacy_applied_held`). A search of a digest no down-set has is
/// complete over the whole log and never over the rooted one, whose down-sets with a dropped
/// head the walks do not take: a writer reads such a miss as `Unsearched`, never `Absent`.
///
/// NEGATIVE CONTROL: `Search::candidates` taking the hint only where the log holds each record
/// it names and with no `Search::dropped` (as before), run against this test and seen to fail
/// with ``assertion `left == right` failed: the rooted log finds the applied log (hint [..])``,
/// `left: None`; and `down_set_search` reading a rooted log's walk as complete, with ``a rooted
/// log's miss is no proof``.
#[test]
fn vault_checkpoint_a_rooted_log_finds_an_applied_log_whose_head_it_dropped() {
    let (mut whole, body, last) = dropped_head();
    let applied = whole.heads_digest();
    assert_eq!(whole.heads(), vec![last]);
    let mut rooted = whole.rooted_at(&body).expect("rooted");
    assert!(rooted.is_dropped(&last));
    let next = one(draft(&rooted, A, D + 41 * DAY_MS, name("a3")), A);
    hold(&mut rooted, next.clone());
    hold(&mut whole, next);
    assert_ne!(rooted.heads_digest(), applied);
    let budget = 1 << 22;
    for hint in [vec![last], Vec::new()] {
        assert_eq!(
            whole
                .down_set_search(&applied, &hint, budget)
                .named
                .map(|(heads, _)| heads),
            Some(vec![last]),
            "the whole log finds the applied log (hint {hint:?})"
        );
        assert_eq!(
            rooted
                .down_set_search(&applied, &hint, budget)
                .named
                .map(|(heads, _)| heads),
            Some(vec![last]),
            "the rooted log finds the applied log (hint {hint:?})"
        );
    }
    let none = [0; 32];
    let searched = whole.down_set_search(&none, &[], budget);
    assert!(searched.named.is_none() && searched.complete);
    let searched = rooted.down_set_search(&none, &[], budget);
    assert!(searched.named.is_none());
    assert!(!searched.complete, "a rooted log's miss is no proof");
}
