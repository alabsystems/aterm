// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates
// Author: Andrew Yates

//! `AghEnsureOnce` (docs/DESIGN-agh-2026-10-19.md §17.2): one release-draft create under lost
//! answers. A one-shot POST needs a durable intent first; an intent is posted at most once
//! and leaves at most one draft; an answered 4xx (Refused) closes the intent with nothing
//! created, so a later run may post again; a lost answer leaves the intent open (Unknown),
//! and only a WriterRead discovery closes it, never a reader's listing and never "none seen",
//! because listing lag is not absence. A crash is a run that ends with its answer
//! unrecorded: the refusal it may have read is process-local, so the intent stays open, and
//! the POST it sent may still land after it, so discovery meets a draft that appears late.
//! `agh intent retract` (a person's step) is outside the model.

use aterm_spec::derive::Model;

/// The ensure-once model.
#[must_use]
#[cfg_attr(trust_verify, trust::skip)]
pub fn agh_ensure_once_model() -> Model {
    aterm_spec::ty_model! {
        AghEnsureOnce {
            const Buggy = 0;
            var intent = 0;   // 0 none, 1 open, 2 closed
            var sent = 0;     // POSTs sent for the open intent
            var drafts = 0;   // drafts GitHub holds for the tag
            var answer = 0;   // 0 none, 1 created (2xx), 2 refused (4xx), 3 lost (no answer, or 5xx)
            var settled = 0;  // 0 not yet, 1 NeverSent, 2 Refused, 3 Created, 4 Unknown
            var visible = 0;  // 1: a WriterRead listing shows the draft
            var found = 0;    // discovery: 0 not run, 1 found, 2 none seen
            var asw = 1;      // 1: discovery ran as the writer (WriterRead)
            var runs = 0;     // crashes so far
            var begun = 0;    // intents begun so far
            action Begin when (visible == 0 && begun <= 1 && (intent == 0 || (intent == 2 && settled <= 2))) {
                intent = 1; sent = 0; answer = 0; settled = 0; found = 0; begun = begun + 1;
            }
            action NotSent when (intent == 1 && sent == 0 && settled == 0) { settled = 1; intent = 2; }
            action Send when (intent == 1 && sent == 0 && settled == 0) { sent = 1; }
            action AnswerCreated when (sent == 1 && answer == 0 && settled == 0) {
                drafts = if drafts == 2 { 2 } else { drafts + 1 }; answer = 1;
            }
            action AnswerRefused when (sent == 1 && answer == 0 && settled == 0) { answer = 2; }
            action LandedLost when (sent == 1 && answer == 0 && settled == 0) {
                drafts = if drafts == 2 { 2 } else { drafts + 1 }; answer = 3;
            }
            action FailedLost when (sent == 1 && answer == 0 && settled == 0) { answer = 3; }
            action Reveal when (drafts > 0 && visible == 0) { visible = 1; }
            action Settle when (answer > 0 && settled == 0) {
                settled = if answer == 1 { 3 } else if answer == 2 { 2 } else { 4 };
                intent = if answer <= 2 { 2 } else { 1 };
            }
            action Discover when (intent == 1 && settled == 4 && found == 0) {
                found = if visible == 1 { 1 } else { 2 };
                intent = if visible == 1 { 2 } else { 1 };
                settled = if visible == 1 { 3 } else { 4 };
            }
            action Rediscover when (intent == 1 && found == 2) { found = 0; }
            action Crash when (intent == 1 && runs <= 1) {
                runs = runs + 1; answer = 0; settled = if settled == 0 { 4 } else { settled };
            }
            action LandedAfterCrash when (sent == 1 && runs > 0 && drafts == 0) { drafts = 1; }
            action BuggySendWithoutIntent when (Buggy == 1 && intent == 0 && sent == 0) { sent = 1; }
            action BuggyRepostOnUnknown when (Buggy == 1 && intent == 1 && settled == 4 && sent == 1) {
                sent = 2; drafts = if drafts <= 1 { drafts + 1 } else { drafts };
            }
            action BuggyRefusedStaysOpen when (Buggy == 1 && answer == 2 && settled == 0) { settled = 4; intent = 1; }
            action BuggyLostIsCreated when (Buggy == 1 && answer == 3 && settled == 0 && drafts == 0) { settled = 3; intent = 2; }
            action BuggyDiscoverAsReader when (Buggy == 1 && intent == 1 && settled == 4 && found == 0) { asw = 0; found = 2; }
            action BuggyNoneCloses when (Buggy == 1 && intent == 1 && settled == 4 && found == 0 && visible == 0) {
                found = 2; intent = 2; settled = 1;
            }
            invariant SendNeedsIntent: sent <= 0 || intent > 0;
            invariant OnePostPerIntent: sent <= 1;
            invariant AtMostOneDraft: drafts <= 1;
            invariant RefusedCloses: answer <= 1 || answer > 2 || settled <= 0 || (settled == 2 && intent > 1);
            invariant CreatedNeedsObject: settled <= 2 || settled > 3 || drafts > 0;
            invariant DiscoveryIsWriterRead: found <= 0 || asw > 0;
            invariant NoneFoundStaysOpen: found <= 1 || intent == 1;
            bound IntentRange: intent <= 2;   bound SentRange: sent <= 2;       bound DraftsRange: drafts <= 2;
            bound AnswerRange: answer <= 3;   bound SettledRange: settled <= 4; bound VisibleRange: visible <= 1;
            bound FoundRange: found <= 2;     bound AswRange: asw <= 1;         bound RunsRange: runs <= 2;
            bound BegunRange: begun <= 2;
        }
    }
}
