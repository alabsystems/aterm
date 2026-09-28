// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The one publication a cut makes, and who holds the channel's `latest` pointer.

use super::*;

/// PUBLISH ONCE: the writer/reader contract of the release channel's ONE release per
/// version and its `latest` pointer (owner ruling R4, 2026-09-23; the private-origin
/// leg retired 2026-09-26).
///
/// The READER is every credential-less updater and every evergreen download link: one
/// request, `releases/latest/download/aterm-appcast.toml`, names the channel head, and a
/// head that serves no appcast pair is a head nobody can install from. The WRITERS are
/// two: the publication engine, which creates this version's release first as a SOURCE
/// prerelease, and the cut, whose `publish` step (`publish::step_publish`,
/// `channel::publish_on_channel`) puts the app on that same release object and is the
/// only writer allowed to move `latest` — with one guarded PATCH (`draft=false,
/// prerelease=false, make_latest=true`) after the appcast signature and then the appcast
/// are up, the release's exact asset set has been proved byte-identical to the artifacts
/// the self-check proved AFTER the last upload, and the fleet's floors have been read
/// after the uploads too; never over a NEWER app release, and never under a machine-roster
/// generation the fleet has moved past.
///
/// Until 2026-09-26 a cut published TWICE: a draft on the private origin, flipped,
/// archived and verified there, then a copy mirrored to the channel. The origin's live
/// verify was what stood between bytes nobody had re-read and the fleet. With one
/// publication, the channel's own proof (`proved`, `ChannelRelease::prove_assets`) is
/// that line, and the head PATCH may not cross it.
///
/// `latest`: 0 the previous (older) app release, 1 this version's release, 2 a newer app
/// release. `phase`: 0 before `pub publish`, 1 the source release exists, 2 the cut holds
/// the release lease (`publish`), 3 the cut's head PATCH landed and what a stranger sees
/// is not yet proved (the pointer gate's five-minute bound ran out, as on v0.93.0), 4
/// the cut refused, 5 the cut was abandoned, 6 the cut finished. `bound`: the cut's
/// journal names this release (the adoption's durable intent and its ID), which it may
/// do only once the floors passed: a cut the floors refuse on its first read leaves
/// nothing in its journal to withdraw. `source`: the engine's release object exists — an abandon
/// WITHDRAWS what the cut put on it and never deletes it (it is the engine's, and a recut
/// adopts it again). `proved`: the exact set was proved byte-identical since the last
/// upload — every upload voids it. `headed`: history, this cut's head PATCH landed.
///
/// THE FLOORS ARE READ TWICE, and the second read is what `joined` exists for. A machine
/// joining the roster is not lease-gated: `RosterJoin` lands in any phase from the source
/// release to the head, re-dresses the channel head with a generation this cut's signed
/// appcast does not carry, and makes every floor read before it stale (`checked = 0`). A
/// join before the first read refuses the cut before the bind; one during the uploads is
/// refused by the read after them (`RefuseFloor` from a proved release, which the cut
/// then withdraws); one after the PATCH meets a release that is already the head — and a
/// head the cut made is FINISHED, never refused. The resume
/// (`channel::ChannelRelease::head_made`) uploads nothing and reads no roster floor: it
/// re-sends the PATCH (`ReassertHead`) — another release published as a full release
/// meanwhile holds `latest` (`OtherTakesLatest`), and a read-only resume wedges there —
/// and proves the read side. The model reads the second floor read and the PATCH as
/// adjacent; a join landing between those two requests is the one window no read can
/// close (GitHub offers no conditional PATCH), exactly like an out-of-band release while
/// the lease is held. A newer release can only come from another LEASE HOLDER, so it
/// lands while this cut does not hold the lease (`NewerRelease` in phase 1) — the reading
/// a stale journal resumed after someone else's release meets — and the second read's
/// head half is defence in depth the model does not need.
///
/// Tier-1: `crates/aterm-release/tests/it/channel_latest.rs` drives the real
/// `channel::publish_on_channel`, `publish::prove_channel_head_is_older`,
/// `publish::roster_floor_covered` and `channel::withdrawable_assets` against a fake
/// GitHub, projecting its state onto these variables after every call — through a first
/// pass, a join during the uploads, and a resume after the PATCH with a join and another
/// release's `latest` since.
///
/// `Buggy=1` restores each defect as its own action, each caught by its own invariant:
/// the engine creating the source release as a FULL release (it takes `latest` with
/// nothing on it — v0.79.0 .. v0.91.0), the cut finishing without the head PATCH (the
/// adopt path before 2026-09-23), the head PATCH sent before the appcast pair, the
/// appcast uploaded before its signature, a proof taken before the last upload, the head
/// PATCH sent over bytes nothing re-proved (what the origin's verify guarded before the
/// mirror copied a release, gone with the origin leg), the head PATCH sent on the first
/// floor read alone, a head ratchet that does not look, the bind made before the
/// ratchet, an abandon that deletes the engine's release, and the resume that re-entered
/// the write half over a head it made and was refused by the join since — stranding the
/// lease, because `--abandon` refuses a published release.
#[must_use]
#[cfg_attr(trust_verify, trust::skip)]
pub fn release_publish_once_model() -> Model {
    crate::ty_model! {
        ReleasePublishOnce {
            const Buggy = 0;
            var phase = 0;
            var latest = 0;
            var prerelease = 0;
            var source = 0;
            var sig_up = 0;
            var toml_up = 0;
            var proved = 0;
            // The floors were read, and no join has landed since.
            var checked = 0;
            var newer = 0;
            // A roster join past this cut's generation has landed.
            var joined = 0;
            var bound = 0;
            var headed = 0;
            // History: the head PATCH took `latest` from a newer app release.
            var displaced = 0;
            // History: the head PATCH made a head under a roster generation the fleet had
            // already moved past.
            var behind = 0;

            // `pub publish`: for a tree declaring `BINARY_CUT_FOLLOWS_DEFAULT="true"`
            // the engine creates a PRERELEASE, which GitHub never lets hold `latest`.
            action PublishSource when (phase == 0) {
                phase = 1;
                prerelease = 1;
                source = 1;
            }
            action PublishSourceFull when (Buggy == 1 && phase == 0) {
                phase = 1;
                latest = 1;
                source = 1;
            }
            // Another lease holder's newer app release, before this cut's lease.
            action NewerRelease when (phase == 1 && newer == 0) {
                newer = 1;
                latest = 2;
            }
            // `atpkg-keys join` / `ship provision`: no lease, any time until the cut is
            // finished. Every floor read before it is stale.
            action RosterJoin when (phase > 0 && phase <= 3 && joined == 0) {
                joined = 1;
                checked = 0;
            }
            action AcquireLease when (phase == 1) {
                phase = 2;
            }
            // `ChannelRelease::ratchet`: the head is this release or older
            // (`prove_channel_head_is_older`) and the roster generation is covered
            // (`roster_floor_covered`).
            action Ratchet when (phase == 2 && latest <= 1 && joined == 0) {
                checked = 1;
            }
            action RatchetBlind when (Buggy == 1 && phase == 2) {
                checked = 1;
            }
            action RefuseNewer when (phase == 2 && latest == 2) {
                phase = 4;
            }
            // The floor refusal, at whichever read meets the join: the first (a resume's
            // first may meet a release its journal bound on an earlier pass), or the one
            // after the proof.
            action RefuseFloor when (phase == 2 && joined == 1) {
                phase = 4;
            }
            // `ChannelRelease::bind`, only once the floors passed.
            action Bind when (phase == 2 && checked == 1 && bound == 0) {
                bound = 1;
            }
            action BindBlind when (Buggy == 1 && phase == 2 && checked == 0 && bound == 0) {
                bound = 1;
            }
            // `channel_upload_rank`: the signature, then the appcast, last. An upload
            // voids any proof taken before it.
            action UploadSig when (phase == 2 && bound == 1 && sig_up == 0) {
                sig_up = 1;
                proved = 0;
            }
            action UploadToml when (phase == 2 && sig_up == 1 && toml_up == 0) {
                toml_up = 1;
                proved = 0;
            }
            action UploadTomlFirst when (
                Buggy == 1 && phase == 2 && bound == 1 && sig_up == 0 && toml_up == 0
            ) {
                toml_up = 1;
                proved = 0;
            }
            // `ChannelRelease::prove_assets`: from a fresh listing, the exact set, every
            // object byte-identical to its artifact — after the last upload.
            action ProveAssets when (phase == 2 && toml_up == 1 && proved == 0) {
                proved = 1;
            }
            action ProveBeforeLastUpload when (
                Buggy == 1 && phase == 2 && bound == 1 && toml_up == 0 && proved == 0
            ) {
                proved = 1;
            }
            // `ChannelRelease::make_head`: the one guarded PATCH, on a proof and a floor
            // read that both postdate the uploads.
            action MakeHead when (phase == 2 && checked == 1 && proved == 1) {
                displaced = if latest == 2 { 1 } else { displaced };
                behind = joined;
                latest = 1;
                prerelease = 0;
                headed = 1;
                phase = 3;
            }
            action MakeHeadEarly when (
                Buggy == 1 && phase == 2 && checked == 1 && bound == 1 && toml_up == 0
            ) {
                latest = 1;
                prerelease = 0;
                headed = 1;
                phase = 3;
            }
            action MakeHeadUnproved when (
                Buggy == 1 && phase == 2 && checked == 1 && toml_up == 1 && proved == 0
            ) {
                latest = 1;
                prerelease = 0;
                headed = 1;
                phase = 3;
            }
            // The sequence with one floor read: the PATCH on the read before the uploads.
            action MakeHeadStaleFloors when (Buggy == 1 && phase == 2 && proved == 1) {
                behind = joined;
                latest = 1;
                prerelease = 0;
                headed = 1;
                phase = 3;
            }
            action FinishWithoutHead when (Buggy == 1 && phase == 2 && checked == 1 && proved == 1) {
                phase = 6;
            }
            // Another release published as a FULL release after the PATCH takes `latest`
            // (GitHub's rule for every newly published release) — an `atpkg-index-<n>`
            // cut, whose tag no client installs from.
            action OtherTakesLatest when (phase == 3 && latest == 1) {
                latest = 0;
            }
            // The resume whose `head_made` found the PATCH already landed: the head half
            // of the ratchet (nothing newer lands under the lease) and the same PATCH,
            // re-sent — `latest` is this release again. No upload, no roster floor.
            action ReassertHead when (phase == 3) {
                latest = 1;
            }
            // `ChannelRelease::prove_head`: the pointer names this release, and a
            // stranger reads it.
            action ProveHead when (phase == 3 && latest == 1) {
                phase = 6;
            }
            // The resume before 2026-09-26: the write half re-entered over a head the cut
            // made, and the floors read again refuse it once a join has re-dressed it.
            action ResumeRefusesItsOwnHead when (Buggy == 1 && phase == 3 && joined == 1) {
                phase = 4;
            }
            // `--abandon` of a cut that never made its release the head (it refuses a
            // PUBLISHED release): exactly what it uploaded is withdrawn, and the engine's
            // release stays.
            action Abandon when ((phase == 2 || phase == 4) && headed == 0) {
                sig_up = 0;
                toml_up = 0;
                proved = 0;
                bound = 0;
                phase = 5;
            }
            action AbandonDeletesRelease when (Buggy == 1 && (phase == 2 || phase == 4) && headed == 0) {
                sig_up = 0;
                toml_up = 0;
                proved = 0;
                bound = 0;
                source = 0;
                phase = 5;
            }
            // Abandoned or finished: the lease is free, nothing is left to do (the stutter
            // that tells the deadlock check a terminal state from a wedge).
            action Done when (phase == 5 || phase == 6) {
                phase = phase;
            }

            invariant NoEmptyHead:
                latest <= 0 || latest > 1 || (sig_up == 1 && toml_up == 1);
            // No client can read an appcast whose signature is not there yet.
            invariant SignatureBeforeAppcast: toml_up == 0 || sig_up == 1;
            // A proof is of the whole release, appcast included.
            invariant ProofAfterTheLastUpload: proved == 0 || toml_up == 1;
            // The cut makes a head only of bytes it proved after its last upload.
            invariant NoUnprovedHead: headed == 0 || (proved == 1 && toml_up == 1);
            invariant NeverDisplacesNewer: displaced == 0;
            invariant NeverBehindTheFleet: behind == 0;
            invariant PublishedMeansHead:
                phase <= 5 || (latest == 1 && prerelease == 0 && headed == 1);
            // The journal names a channel release only on a floor read that nothing but a
            // join has since made stale: a cut the floors refuse on its first read has
            // recorded nothing (no draft, no adoption intent, no ID) for `--abandon` to
            // withdraw.
            invariant BoundOnAFloorRead: bound == 0 || checked == 1 || joined == 1;
            // An abandoned cut leaves the engine's release, and nothing of its own on it.
            invariant AbandonLeavesTheSourceRelease:
                phase <= 4 ||
                    phase > 5 ||
                    (source == 1 && sig_up == 0 && toml_up == 0 && (latest <= 0 || latest > 1));
            // A head the cut made is finished, never refused: nothing but the read side
            // stands between it and `unlock`.
            invariant MadeHeadFinishes: headed == 0 || phase == 3 || phase == 6;
        }
    }
}
