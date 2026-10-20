// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The outlook of a wall: a person is asked for only what only a person can
//! do, and a wait is never one.

use super::*;

fn cfg() -> SupervisorConfig {
    SupervisorConfig::default()
}

/// A bucket with no reset asks a person, one with a reset does not; a lost
/// login is the supervisor's to carry (a person's where API errors are left
/// to the owner); a switched-off key is nobody's.
#[test]
fn every_wall_kind_has_an_outlook() {
    let c = cfg();
    let bucket = WallKind::ModelBucket { consent: false };
    assert_eq!(
        outlook(WallKind::UsageWeekly, true, false, &c, true),
        Outlook::Resumes
    );
    assert_eq!(
        outlook(WallKind::UsageWeekly, true, true, &c, true),
        Outlook::VendorResumes
    );
    assert_eq!(outlook(bucket, true, false, &c, true), Outlook::Resumes);
    assert_eq!(
        outlook(bucket, false, false, &c, true),
        Outlook::ResumesAndAsks(ExceptionKind::Bucket)
    );
    assert_eq!(
        outlook(WallKind::Spend, false, false, &c, true),
        Outlook::ResumesAndAsks(ExceptionKind::Spend)
    );
    assert_eq!(
        outlook(WallKind::Spend, true, false, &c, true),
        Outlook::Resumes
    );
    assert_eq!(
        outlook(WallKind::Auth, false, false, &c, true),
        Outlook::Carries(ExceptionKind::Login)
    );
    let mut off = cfg();
    off.retry_api_errors = false;
    assert_eq!(
        outlook(WallKind::Auth, false, false, &off, true),
        Outlook::Person(ExceptionKind::Login),
        "a login left to the owner"
    );
    assert_eq!(answer(WallKind::Auth, false, &c, true), Answer::Resumes);
    assert_eq!(answer(WallKind::Auth, false, &off, true), Answer::Nobody);
    assert_eq!(
        outlook(WallKind::Overloaded, false, false, &c, true),
        Outlook::Resumes
    );
    assert_eq!(
        outlook(WallKind::Context, false, false, &c, true),
        Outlook::Resumes
    );
    assert_eq!(
        outlook(WallKind::Memory, false, false, &c, true),
        Outlook::Resumes
    );
    assert_eq!(
        outlook(WallKind::UsageWeekly, true, false, &c, false),
        Outlook::Unattended
    );
    // A refused request: resumed slowly, and a person can lift it; a 500 and
    // a reply past its maximum are resumed, nobody asked.
    let refused = WallKind::ApiError {
        code: Some(400),
        retryable: false,
        cause: aterm_phase::ApiCause::Request,
    };
    assert_eq!(
        outlook(refused, false, false, &c, true),
        Outlook::ResumesAndAsks(ExceptionKind::Refused)
    );
    for cause in [
        aterm_phase::ApiCause::Server,
        aterm_phase::ApiCause::Overlong,
    ] {
        let kind = WallKind::ApiError {
            code: None,
            retryable: true,
            cause,
        };
        assert_eq!(
            outlook(kind, false, false, &c, true),
            Outlook::Resumes,
            "{cause:?}"
        );
    }
    assert_eq!(
        answer(refused, false, &c, true),
        Answer::ResumesAsks("fix in the tab")
    );
    let off = SupervisorConfig {
        resume_limits: false,
        ..cfg()
    };
    assert_eq!(
        outlook(WallKind::UsageSession, true, false, &off, true),
        Outlook::Unattended
    );
    assert_eq!(
        outlook(WallKind::Spend, true, false, &off, true),
        Outlook::Person(ExceptionKind::Spend)
    );
    let no_retries = SupervisorConfig {
        retry_api_errors: false,
        ..cfg()
    };
    assert_eq!(
        outlook(refused, false, false, &no_retries, true),
        Outlook::Unattended
    );
}

/// Memory and login walls are nobody's whatever the switches say; a reset-less
/// bucket or spend limit resumes slowly and names the one action.
#[test]
fn a_wall_is_answered_by_the_harness_or_by_nobody() {
    let on = SupervisorConfig::default();
    let bucket = WallKind::ModelBucket { consent: false };
    for kind in [
        WallKind::UsageSession,
        WallKind::UsageWeekly,
        WallKind::Overloaded,
        WallKind::Context,
    ] {
        assert_eq!(answer(kind, true, &on, true), Answer::Resumes, "{kind:?}");
        assert_eq!(
            answer(kind, true, &on, false),
            Answer::Nobody,
            "{kind:?}: no supervisor here"
        );
    }
    assert_eq!(answer(bucket, true, &on, true), Answer::Resumes);
    assert_eq!(
        answer(bucket, false, &on, true),
        Answer::ResumesAsks("/model to switch")
    );
    assert_eq!(answer(WallKind::Spend, true, &on, true), Answer::Resumes);
    assert_eq!(
        answer(WallKind::Spend, false, &on, true),
        Answer::ResumesAsks("add credits")
    );
    // A lost login is the loop's to carry (its notice asks the owner's step).
    assert_eq!(answer(WallKind::Auth, false, &on, true), Answer::Resumes);
    // Nobody: the memory banner, the owner's switches off.
    assert_eq!(answer(WallKind::Memory, false, &on, true), Answer::Nobody);
    let off = SupervisorConfig {
        resume_limits: false,
        retry_api_errors: false,
        compact_on_context_wall: false,
        ..SupervisorConfig::default()
    };
    for kind in [
        WallKind::UsageWeekly,
        WallKind::Overloaded,
        WallKind::Context,
    ] {
        assert_eq!(answer(kind, true, &off, true), Answer::Nobody, "{kind:?}");
    }
    assert_eq!(answer(WallKind::Spend, true, &off, true), Answer::Nobody);
    assert_eq!(answer(WallKind::Auth, false, &off, true), Answer::Nobody);
}

/// A session usage credits carry, the key off (the default): it waits for
/// the subscription's reset where the window they carry past is named, and
/// is a person's where none is. NEGATIVE CONTROLS: the key on carries it as
/// work going on, and an unsupervised session is nobody's; neither says a
/// word on a surface.
#[test]
fn a_session_on_usage_credits_waits_for_the_subscriptions_reset() {
    let off = cfg();
    let on = SupervisorConfig {
        usage_credits: true,
        ..cfg()
    };
    assert_eq!(
        credits_outlook(true, &off, true),
        CreditsOutlook::WaitsForReset
    );
    assert_eq!(credits_outlook(false, &off, true), CreditsOutlook::Person);
    assert!(CreditsOutlook::WaitsForReset.phrase().is_some());
    assert_ne!(
        CreditsOutlook::WaitsForReset.phrase(),
        CreditsOutlook::Person.phrase()
    );
    for has_reset in [true, false] {
        assert_eq!(
            credits_outlook(has_reset, &on, true),
            CreditsOutlook::Carried
        );
        assert_eq!(
            credits_outlook(has_reset, &off, false),
            CreditsOutlook::Unattended
        );
    }
    assert_eq!(CreditsOutlook::Carried.phrase(), None);
    assert_eq!(CreditsOutlook::Unattended.phrase(), None);
}
