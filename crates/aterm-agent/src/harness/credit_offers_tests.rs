// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Tests for the credit offers' allowlist reader ([`super::parse`], [`super::read`]), the
//! claim worker's task ([`super::claim_task`]) and what its fence reads ([`super::is_claim_folder`],
//! [`super::claim_page`]).

use aterm_json::{Value, json};

use super::*;

const ACCOUNT: &str = "5b1d2c3e-0000-4000-8000-0000000000aa";

const ORG: &str = "0F6C2B8E-1D3A-4C5E-9B7F-2A4D6E8C0B1A";

const ORG_LOWER: &str = "0f6c2b8e-1d3a-4c5e-9b7f-2a4d6e8c0b1a";

const ENDPOINT: &str = "/v1/code/promo/cloud_credit";

const SENTINEL: &str = "S3NT1NEL";

/// A profile with both offers live, and the keys never to surface carrying [`SENTINEL`].
fn live() -> Value {
    json!({
        "numStartups": 12,
        "oauthAccount": {
            "accountUuid": ACCOUNT,
            "organizationUuid": ORG,
            "emailAddress": format!("{SENTINEL}@example.com"),
            "displayName": format!("{SENTINEL} display"),
            "fullName": format!("{SENTINEL} full"),
            "organizationName": format!("{SENTINEL} org"),
        },
        "cachedGrowthBookFeatures": {
            "tengu_swift_lynx": {
                "version": 3,
                "endpoint": ENDPOINT,
                "webPath": "/code/claim-credit",
                "desktopPath": "/claim",
                "claimLinkIncludesOrganization": true,
                "startup": {
                    "text": "Claim a $250 bonus credit for cloud sessions, on top of your plan limits",
                    "claimPageLine": "Claim your $250 cloud session credit",
                    "buttonLabel": "Claim",
                },
                "limitWall": {"text": "x"},
            },
            "tengu_snug_storm": {
                "text": "Your Max plan includes $200 a month in API credits to power your apps and agents. Claim them at https://clau.de/api-credits",
            },
            "tengu_lilac_loom": {"text": format!("{SENTINEL} feature of the week")},
        },
        "promoStartupStatusCache": {
            "accountUuid": ACCOUNT,
            "endpoint": ENDPOINT,
            "eligible": true,
            "claimed": false,
            "checkedAt": 1_792_022_400_000_i64,
        },
        "apiCreditLinkStatusCache": {
            ORG: {"eligibleSince": 1_792_022_400_000_i64, "nextAskAt": null, "asks": 1},
        },
        "fotwEligibilityCache": {"eligible": true},
    })
}

fn with(mut value: Value, path: &[&str], leaf: Value) -> Value {
    let mut at = &mut value;
    for key in path.get(..path.len() - 1).unwrap_or(&[]) {
        at = at
            .as_object_mut()
            .and_then(|o| o.get_mut(*key))
            .expect("a path into the fixture");
    }
    at.as_object_mut()
        .expect("an object")
        .insert((*path.last().expect("a key")).to_owned(), leaf);
    value
}

fn offers(value: &Value) -> Vec<Offer> {
    parse(aterm_json::to_string(value).expect("compose").as_bytes()).expect("parses")
}

fn ids(value: &Value) -> Vec<OfferId> {
    offers(value).iter().map(|o| o.id).collect()
}

#[test]
fn both_offers_live_read_with_their_links_and_lines() {
    let found = offers(&live());
    assert_eq!(found.len(), 2, "{found:?}");
    let cloud = &found[0];
    assert_eq!(cloud.id, OfferId::CloudCredit);
    assert_eq!(
        cloud.url,
        format!("https://claude.ai/code/claim-credit/6/{ORG_LOWER}")
    );
    assert_eq!(cloud.words, "Claim your $250 cloud session credit");
    let api = &found[1];
    assert_eq!(api.id, OfferId::ApiCredits);
    assert_eq!(api.url, "https://clau.de/api-credits");
    assert_eq!(
        api.words,
        "Your Max plan includes $200 a month in API credits to power your apps and agents."
    );
    assert_ne!(cloud.key, api.key);
    assert_eq!(OfferKey::parse(cloud.key.as_str()), Some(cloud.key.clone()));
}

#[test]
fn nothing_but_the_allowlist_surfaces() {
    let found = offers(&live());
    let shown = format!("{found:?}");
    assert!(!shown.contains(SENTINEL), "{shown}");
    assert!(
        !shown.contains(ACCOUNT),
        "the account id is kept only as a digest: {shown}"
    );
    for offer in &found {
        assert!(!claim_task(offer).contains(SENTINEL));
    }
}

#[test]
fn the_cloud_credit_is_live_only_under_each_of_its_conditions() {
    let cases: [(&[&str], Value); 7] = [
        (&["promoStartupStatusCache", "claimed"], json!(true)),
        (&["promoStartupStatusCache", "claimed"], json!(null)),
        (&["promoStartupStatusCache", "eligible"], json!(false)),
        (&["promoStartupStatusCache", "eligible"], json!("true")),
        (
            &["promoStartupStatusCache", "accountUuid"],
            json!("another-account"),
        ),
        (
            &["promoStartupStatusCache", "endpoint"],
            json!("/v1/code/promo/other"),
        ),
        (
            &["cachedGrowthBookFeatures", "tengu_swift_lynx", "endpoint"],
            json!(null),
        ),
    ];
    for (path, leaf) in cases {
        let value = with(live(), path, leaf.clone());
        assert_eq!(ids(&value), vec![OfferId::ApiCredits], "{path:?} = {leaf}");
    }
    let mut gone = live();
    gone.as_object_mut()
        .unwrap()
        .remove("promoStartupStatusCache");
    assert_eq!(ids(&gone), vec![OfferId::ApiCredits], "no status");
}

#[test]
fn a_web_path_claude_code_would_refuse_is_no_offer() {
    for path in [
        "code/claim-credit",
        "/code/Claim-Credit",
        "/code//claim",
        "/code/claim-credit/",
        "/code/claim credit",
        "//evil.example/x",
        "/code/claim-credit?x=1",
        "",
    ] {
        let value = with(
            live(),
            &["cachedGrowthBookFeatures", "tengu_swift_lynx", "webPath"],
            json!(path),
        );
        assert_eq!(ids(&value), vec![OfferId::ApiCredits], "{path:?}");
    }
    let value = with(
        live(),
        &["cachedGrowthBookFeatures", "tengu_swift_lynx", "webPath"],
        json!("/code/claim_credit-2"),
    );
    assert_eq!(
        offers(&value)[0].url,
        format!("https://claude.ai/code/claim_credit-2/6/{ORG_LOWER}")
    );
}

#[test]
fn the_cloud_link_names_the_organization_only_where_the_flag_asks() {
    let value = with(
        live(),
        &[
            "cachedGrowthBookFeatures",
            "tengu_swift_lynx",
            "claimLinkIncludesOrganization",
        ],
        json!(false),
    );
    assert_eq!(
        offers(&value)[0].url,
        "https://claude.ai/code/claim-credit/6"
    );
    let no_org = with(live(), &["oauthAccount", "organizationUuid"], json!(null));
    assert_eq!(
        ids(&no_org),
        Vec::<OfferId>::new(),
        "the flag asks for an organization and none is named; no API status matches either"
    );
    let bad_org = with(
        live(),
        &["oauthAccount", "organizationUuid"],
        json!("not-a-uuid"),
    );
    assert!(!ids(&bad_org).contains(&OfferId::CloudCredit));
}

#[test]
fn the_api_credits_are_live_only_with_a_number_for_this_organization() {
    for status in [
        json!({"eligibleSince": null, "nextAskAt": 1, "asks": 2}),
        json!({"eligibleSince": "1792022400000"}),
        json!({}),
        json!(7),
    ] {
        let value = with(live(), &["apiCreditLinkStatusCache", ORG], status.clone());
        assert_eq!(ids(&value), vec![OfferId::CloudCredit], "{status}");
    }
    let other = with(
        live(),
        &["apiCreditLinkStatusCache"],
        json!({ ORG_LOWER: {"eligibleSince": 1} }),
    );
    assert_eq!(
        ids(&other),
        vec![OfferId::CloudCredit],
        "the full organization id, as Claude Code keys it"
    );
    let missing = with(live(), &["oauthAccount", "organizationUuid"], json!(null));
    assert!(
        !ids(&missing).contains(&OfferId::ApiCredits),
        "no organization"
    );
    let scalar = with(live(), &["apiCreditLinkStatusCache"], json!([1, 2]));
    assert_eq!(ids(&scalar), vec![OfferId::CloudCredit]);
}

#[test]
fn an_api_link_off_the_vendor_or_not_https_is_no_offer() {
    for text in [
        "Claim them at http://clau.de/api-credits",
        "Claim them at https://clau.de.evil.example/api-credits",
        "Claim them at https://evil.example/https://clau.de",
        "Claim them at https://user@clau.de/api-credits",
        "Claim them at https://clau.de:8443/api-credits",
        "Claim them at https://CLAU.DE/api-credits",
        "Claim them at https://clau.de/api\u{7}credits",
        "No link at all.",
        "",
    ] {
        let value = with(
            live(),
            &["cachedGrowthBookFeatures", "tengu_snug_storm", "text"],
            json!(text),
        );
        assert_eq!(ids(&value), vec![OfferId::CloudCredit], "{text:?}");
    }
    let long = format!("{} https://clau.de/api-credits", "x".repeat(API_TEXT_MAX));
    let value = with(
        live(),
        &["cachedGrowthBookFeatures", "tengu_snug_storm", "text"],
        json!(long),
    );
    assert_eq!(
        ids(&value),
        vec![OfferId::CloudCredit],
        "over 240 characters"
    );
    let value = with(
        live(),
        &["cachedGrowthBookFeatures", "tengu_snug_storm", "text"],
        json!("Claim your API credits at https://claude.ai/settings/api-credits."),
    );
    let api = &offers(&value)[1];
    assert_eq!(
        api.url, "https://claude.ai/settings/api-credits",
        "the stop is dropped"
    );
    assert_eq!(
        api.words, "Claim your API credits at",
        "no stop before the link: all of it"
    );
}

#[test]
fn an_account_other_than_the_signed_in_one_claims_nothing() {
    let value = with(
        live(),
        &["oauthAccount", "accountUuid"],
        json!("someone-else"),
    );
    assert_eq!(
        ids(&value),
        vec![OfferId::ApiCredits],
        "the cloud status names another"
    );
    let signed_out = with(live(), &["oauthAccount"], json!(null));
    assert!(ids(&signed_out).is_empty(), "no account: no offer");
    let keys = |v: &Value| offers(v).into_iter().map(|o| o.key).collect::<Vec<_>>();
    let other_account = with(
        live(),
        &["oauthAccount", "accountUuid"],
        json!("someone-else"),
    );
    assert_ne!(
        keys(&live())[1],
        keys(&other_account)[0],
        "the key names the account"
    );
}

#[test]
fn a_vendor_line_that_does_not_read_is_named_instead() {
    let value = with(
        live(),
        &["cachedGrowthBookFeatures", "tengu_swift_lynx", "startup"],
        json!({"claimPageLine": "bad\u{202e}line", "text": "y".repeat(WORDS_MAX + 1)}),
    );
    assert_eq!(offers(&value)[0].words, OfferId::CloudCredit.name());
    let value = with(
        live(),
        &["cachedGrowthBookFeatures", "tengu_swift_lynx", "startup"],
        json!({"text": "Claim a $250 bonus credit"}),
    );
    assert_eq!(offers(&value)[0].words, "Claim a $250 bonus credit");
}

#[test]
fn the_file_is_read_bounded() {
    let tmp = aterm_tempfile::Builder::new()
        .prefix("aterm-credit-offers-")
        .tempdir()
        .expect("temp dir");
    let path = tmp.path().join(".claude.json");
    assert_eq!(read(&path), Err(NoProfile::Missing));
    assert_eq!(stamp(&path), None);
    let text = aterm_json::to_string(&live()).expect("compose");
    std::fs::write(&path, &text).expect("write");
    assert_eq!(read(&path).expect("reads").len(), 2);
    assert_eq!(stamp(&path).map(|s| s.len), Some(text.len() as u64));
    assert_eq!(read(tmp.path()), Err(NoProfile::NotAFile));
    std::fs::write(&path, b"[1, 2]").expect("write");
    assert_eq!(read(&path), Err(NoProfile::Unreadable));
    let large = tmp.path().join("large.json");
    let file = std::fs::File::create(&large).expect("create");
    file.set_len(super::super::profile::PROFILE_MAX + 1)
        .expect("a sparse file");
    assert_eq!(read(&large), Err(NoProfile::TooLarge));
}

#[test]
fn the_profile_is_the_one_the_process_environment_names() {
    let home = Path::new("/Users//someone");
    assert_eq!(
        path_for(&[], home).as_deref(),
        Some(Path::new("/Users//someone/.claude.json"))
    );
    assert_eq!(
        path_for(&["CLAUDE_CONFIG_DIR=/elsewhere/claude".to_string()], home).as_deref(),
        Some(Path::new("/elsewhere/claude/.claude.json"))
    );
}

#[test]
fn the_claim_task_is_one_guarded_string() {
    let offer = &offers(&live())[0];
    let task = claim_task(offer);
    assert!(
        task.starts_with(super::super::upgrade::HARNESS_MARK),
        "{task}"
    );
    assert!(task.contains(&format!("open {} in the person's own Chrome", offer.url)));
    assert!(task.contains("(Claim your $250 cloud session credit)"));
    for guard in [
        "Act only on the claim page and the pages its claim leads to",
        "never open billing, plan, usage or payment settings",
        "Never enter, add or change payment details",
        "never buy anything",
        "never turn on extra usage or change the plan, billing or spend limits",
        "never start a paid trial",
        "never sign in with or create new credentials",
        "Never ask a question.",
        "or the Chrome tools are not available, stop",
    ] {
        assert!(task.contains(guard), "{guard}: {task}");
    }
    assert!(
        task.ends_with(
            "End with one reply starting `Nothing left:` that says CLAIMED: <what was claimed> \
             or NOT CLAIMED: <why>, and no NEEDS YOU line: the person reads your reply in this \
             tab."
        ),
        "{task}"
    );
    assert!(!task.contains('\n'), "one line");
}

/// THE CLAIM TASK'S LAST REPLY LANDS AUTOPILOT LAND: each reply the task asks for, claimed or
/// not and whatever the common reasons, is a clean defense to Land's own reader
/// ([`crate::supervise::policy::turn_end::clean_defense`]), so Land types nothing more. CONTROL:
/// the task's old last line (no `Nothing left:`) and a defense with a NEEDS YOU line hold Land.
#[test]
fn the_claim_tasks_last_reply_lands_autopilot_land() {
    use crate::supervise::policy::turn_end::clean_defense;
    for reply in [
        "Nothing left: CLAIMED: the $250 cloud session credit for this account.",
        "Nothing left: CLAIMED: Claim your $250 cloud session credit. The page confirmed it.",
        "Nothing left: CLAIMED: the Max plan API credits.",
        "Nothing left: NOT CLAIMED: the page asks for payment details.",
        "Nothing left: NOT CLAIMED: the Chrome tools are not available.",
        "Nothing left: NOT CLAIMED: the page says this credit was already claimed.",
        "Nothing left: NOT CLAIMED: the page asks to start a paid trial.",
        "Nothing left: NOT CLAIMED: aterm declined the action on the page.",
    ] {
        assert!(clean_defense(Some(reply)), "{reply}");
    }
    for held in [
        "NOT CLAIMED: the page asks for payment details.",
        "CLAIMED: the $250 cloud session credit.",
        "Nothing left: NOT CLAIMED: the page asks for payment details.\nNEEDS YOU: add a payment \
         method on claude.ai.",
    ] {
        assert!(!clean_defense(Some(held)), "{held}");
    }
}

/// The claim worker's folder is read by its last two names, as the disk-free worker's is.
/// CONTROL: the disk-free worker's folder, the ledger's folder, the same name under another
/// parent, and a folder under the claim folder are no claim worker's.
#[test]
fn a_claim_folder_is_named_by_the_harness_state_and_its_own_name() {
    for yes in [
        "/Users//a/Library/Application Support/aterm/harness/claude-harness/credit-claim",
        "/private/tmp/scratch/state/harness/claude-harness/credit-claim/",
        "claude-harness/credit-claim",
    ] {
        assert!(is_claim_folder(Path::new(yes)), "{yes}");
    }
    for no in [
        "/Users//a/Library/Application Support/aterm/harness/claude-harness/disk-worker",
        "/Users//a/Library/Application Support/aterm/harness/claude-harness/credit-claims",
        "/Users//a/Library/Application Support/aterm/credit-claim",
        "/Users//a/Developer/credit-claim",
        "/Users//a/state/harness/claude-harness/credit-claim/sub",
        "/Users//a/state/harness/claude-harness/credit-claim/..",
        "credit-claim",
        "",
    ] {
        assert!(!is_claim_folder(Path::new(no)), "{no}");
    }
    assert!(!super::super::disk::fence::is_worker_folder(Path::new(
        "/Users//a/state/harness/claude-harness/credit-claim"
    )));
}

/// A page a claim worker may act on: `https://` on a vendor host or a name under one, with no
/// user, port or money word after the host. CONTROL: a host that only begins or ends with the
/// vendor's letters, a user before the host, a port, `http://`, upper case, an empty label, a
/// space, and each money word in the path, the query or percent-encoded, are refused.
#[test]
fn a_claim_page_is_https_on_the_vendor_with_no_money_word() {
    for yes in [
        "https://claude.ai/code/claim-credit/6/0f6c2b8e-1d3a-4c5e-9b7f-2a4d6e8c0b1a",
        "https://claude.ai",
        "https://www.claude.ai/claim",
        "https://platform.claude.com/claim?code=abc#top",
        "https://console.anthropic.com/claim",
        "https://clau.de/x",
    ] {
        assert_eq!(claim_page(yes), Ok(()), "{yes}");
    }
    for (no, why) in [
        ("", "names no page"),
        ("https://claude.ai.evil.com/claim", "not the vendor's host"),
        ("https://evilclaude.ai/claim", "not the vendor's host"),
        ("https://user@claude.ai.evil.com/claim", "a user"),
        ("https://claude.ai@evil.com/claim", "a user"),
        ("https://claude.ai:443/claim", "a port"),
        ("http://claude.ai/claim", "https://"),
        ("HTTPS://claude.ai/claim", "https://"),
        ("https://CLAUDE.AI/claim", "not the vendor's host"),
        ("https://claude..ai/claim", "not the vendor's host"),
        ("https://.claude.ai/claim", "not the vendor's host"),
        ("https://claude.ai/claim credit", "one plain line"),
        ("https://claude.ai/settings/billing", "`billing`"),
        (
            "https://claude.ai/claim?next=%2Fsettings%2Fbilling",
            "`billing`",
        ),
        ("https://claude.ai/upgrade", "`upgrade`"),
        ("https://claude.ai/Settings/Usage", "`usage`"),
        ("https://claude.ai/max-plan", "`plan`"),
        ("https://console.anthropic.com/settings/limits", "`limit`"),
        ("https://claude.ai/claim#checkout", "`checkout`"),
    ] {
        let refused = claim_page(no).expect_err(no);
        assert!(refused.contains(why), "{no}: {refused}");
    }
    for host in VENDOR_HOSTS {
        assert!(vendor_host(host), "{host}");
        assert!(vendor_host(&format!("www.{host}")), "{host}");
        assert!(!vendor_host(&format!("{host}.evil.com")), "{host}");
        if host.matches('.').count() == 1 {
            assert!(!vendor_host(&format!("x{host}")), "{host}");
        }
    }
}
