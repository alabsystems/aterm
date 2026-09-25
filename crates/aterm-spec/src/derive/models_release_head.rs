// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Who holds the public channel's `latest` pointer, and when.

use super::*;

/// THE CHANNEL HEAD: the writer/reader contract on GitHub's `latest` pointer of the
/// public update channel (owner ruling R4, 2026-09-23).
///
/// The READER is every credential-less updater and every evergreen download link: one
/// request, `releases/latest/download/aterm-appcast.toml`, names the channel head, and a
/// head that serves no appcast pair is a head nobody can install from. The WRITERS are
/// two: the publication engine, which creates this version's release first as a SOURCE
/// release, and the cut, which puts the app on that same release object and is the only
/// writer allowed to move `latest` — with one guarded PATCH (`draft=false,
/// prerelease=false, make_latest=true`) after the appcast signature and then the appcast
/// are up, and never over a NEWER app release.
///
/// `latest`: 0 the previous (older) app release, 1 this version's release, 2 a newer app
/// release. `phase`: 0 before `pub publish`, 1 the source release exists, 2 the cut holds
/// the release lease (`step_mirror`), 3 the cut made it the head, 4 the cut refused.
/// `bound`: the cut's journal names this release (`ChannelRelease::bind` — the adoption's
/// durable intent and its ID), which it may do only once the head ratchet passed: a cut
/// refused before it uploaded a byte then leaves nothing in its journal for
/// `--retire-unmirrored` to trip on. The model is one pass of the sequence; a refusal
/// AFTER the bind (the roster half of the second ratchet, or a resumed bound pass) is
/// retire's leave-the-adopted-release-alone case, `verify::retire_channel_disposition`. A newer release can only come from another LEASE HOLDER, so it lands
/// while this cut does not hold the lease (`NewerRelease` in phase 1) — the reading a
/// stale journal resumed after someone else's release meets. An out-of-band publish while
/// the lease is held is outside the protocol and outside this model: GitHub offers no
/// conditional PATCH, so nothing could close that race.
///
/// Tier-1: `crates/aterm-release/tests/channel_latest.rs` drives the real
/// `mirror::publish_on_channel` and `publish::prove_channel_head_is_older` against a fake
/// GitHub, projecting its state onto these variables after every call.
///
/// `Buggy=1` restores each defect as its own action: the engine creating the source
/// release as a FULL release (it takes `latest` with nothing on it — v0.79.0 .. v0.91.0),
/// the cut finishing without the head PATCH (the adopt path before 2026-09-23), the head
/// PATCH sent before the appcast pair, a head ratchet that does not look, and the bind
/// made before the ratchet (the adopt path before 2026-09-23 again: a refused cut's
/// journal named the source release, and retire refused it as LIVE).
#[must_use]
#[cfg_attr(trust_verify, trust::skip)]
pub fn release_channel_head_model() -> Model {
    crate::ty_model! {
        ReleaseChannelHead {
            const Buggy = 0;
            var phase = 0;
            var latest = 0;
            var prerelease = 0;
            var sig_up = 0;
            var toml_up = 0;
            // The head ratchet passed while this cut holds the lease.
            var checked = 0;
            var newer = 0;
            // History: the head PATCH took `latest` from a newer app release.
            var displaced = 0;
            var bound = 0;

            // `pub publish`: for a tree declaring `BINARY_CUT_FOLLOWS_DEFAULT="true"`
            // the engine creates a PRERELEASE, which GitHub never lets hold `latest`.
            action PublishSource when (phase == 0) {
                phase = 1;
                prerelease = 1;
            }
            action PublishSourceFull when (Buggy == 1 && phase == 0) {
                phase = 1;
                latest = 1;
            }
            // Another lease holder's newer app release, before this cut's lease.
            action NewerRelease when (phase == 1 && newer == 0) {
                newer = 1;
                latest = 2;
            }
            action AcquireLease when (phase == 1) {
                phase = 2;
            }
            // `prove_channel_head_is_older`: the head is this release or older.
            action Ratchet when (phase == 2 && latest <= 1) {
                checked = 1;
            }
            action RefuseNewer when (phase == 2 && latest == 2) {
                phase = 4;
            }
            action RatchetBlind when (Buggy == 1 && phase == 2) {
                checked = 1;
            }
            // `ChannelRelease::bind`, only once the floors passed.
            action Bind when (phase == 2 && checked == 1 && bound == 0) {
                bound = 1;
            }
            action BindBlind when (Buggy == 1 && phase == 2 && checked == 0 && bound == 0) {
                bound = 1;
            }
            // `channel_upload_rank`: the signature, then the appcast, last.
            action UploadSig when (phase == 2 && checked == 1 && bound == 1 && sig_up == 0) {
                sig_up = 1;
            }
            action UploadToml when (
                phase == 2 && checked == 1 && bound == 1 && sig_up == 1 && toml_up == 0
            ) {
                toml_up = 1;
            }
            // `ChannelRelease::make_head`: the one guarded PATCH.
            action MakeHead when (
                phase == 2 && checked == 1 && bound == 1 && sig_up == 1 && toml_up == 1
            ) {
                displaced = if latest == 2 { 1 } else { displaced };
                latest = 1;
                prerelease = 0;
                phase = 3;
            }
            action MakeHeadEarly when (
                Buggy == 1 && phase == 2 && checked == 1 && bound == 1 && toml_up == 0
            ) {
                latest = 1;
                prerelease = 0;
                phase = 3;
            }
            action FinishWithoutHead when (
                Buggy == 1 && phase == 2 && checked == 1 && sig_up == 1 && toml_up == 1
            ) {
                phase = 3;
            }

            invariant NoEmptyHead:
                latest <= 0 || latest > 1 || (sig_up == 1 && toml_up == 1);
            invariant NeverDisplacesNewer: displaced == 0;
            invariant PublishedMeansHead:
                phase <= 2 || phase > 3 || (latest == 1 && prerelease == 0);
            // A cut refused before its first upload has recorded no channel release.
            invariant RefusedUnuploadedBindsNothing:
                phase <= 3 || bound == 0 || sig_up == 1;
        }
    }
}
