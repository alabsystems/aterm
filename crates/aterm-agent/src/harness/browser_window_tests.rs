// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Tests for `accounts browser`'s window: its flags against the headless approver's, which
//! browser it opens in, how it reads and takes a profile's lock, how it ends a browser (this
//! test binary run in place as one, `headless_oauth::vendor_tests::browser_stand_in`), and the
//! pass after it that drops the identity providers' cookies (over a stand-in DevTools pipe).

use std::ffi::OsString;

use super::*;
#[cfg(unix)]
use crate::harness::headless_oauth::vendor_tests::stand_in_lead;
use crate::harness::headless_oauth::{HeadlessBrowser, headless_launch_args};

/// The window opens the headless approver's profile with the same profile flags, and drops only
/// what marks a browser automated or headless. CONTROL: the headless launch of the same
/// (non-shell) browser does carry them.
#[test]
fn the_window_shares_the_headless_profile_flags_and_drops_only_the_headless_ones() {
    let profile = Path::new("/state/accounts/browsers/kimi-one-at-example.com");
    let url = "https://www.kimi.ai/";
    let window = window_args(profile, url);
    let words: Vec<String> = window
        .iter()
        .map(|a| a.to_string_lossy().into_owned())
        .collect();
    for flag in [
        format!("--user-data-dir={}", profile.display()),
        "--use-mock-keychain".to_owned(),
        "--password-store=basic".to_owned(),
    ] {
        assert!(words.contains(&flag), "{flag} in {words:?}");
    }
    assert!(
        !words.iter().any(|w| w.starts_with("--headless")
            || w.starts_with("--remote-debugging")
            || w.starts_with("--disable-blink-features")
            || w == "about:blank"),
        "{words:?}"
    );
    assert_eq!(words.last().map(String::as_str), Some(url), "the URL last");

    let chrome = HeadlessBrowser {
        path: PathBuf::from("/Applications/Chromium.app/Contents/MacOS/Chromium"),
        is_headless_shell: false,
    };
    let headless: Vec<OsString> = headless_launch_args(&chrome, profile);
    assert!(headless.contains(&OsString::from("--headless=new")));
    assert!(headless.contains(&OsString::from("--remote-debugging-pipe")));
    let shared: Vec<&OsString> = window[..window.len() - 1].iter().collect();
    assert!(
        shared.iter().all(|flag| headless.contains(flag)),
        "every window flag but the URL is a headless one: {shared:?}"
    );
}

fn file(path: &Path) -> PathBuf {
    std::fs::create_dir_all(path.parent().expect("parent")).expect("mkdir");
    std::fs::write(path, "not a browser").expect("write");
    path.to_path_buf()
}

const CFT: &str = "Google Chrome for Testing.app/Contents/MacOS/Google Chrome for Testing";

/// A window opens in Chrome for Testing or Chromium (named, unpacked in the cache newest first,
/// in an applications folder, or on `PATH`), never in a headless shell or branded Google Chrome.
#[test]
fn a_window_browser_is_cft_or_chromium_never_a_headless_shell_or_branded_chrome() {
    let tmp = aterm_tempfile::tempdir().expect("tmp");
    let none = |_: &str| None::<OsString>;

    let apps = tmp.path().join("apps");
    let app = file(&apps.join(CFT));
    assert_eq!(
        find_window_browser_in(&none, &[], std::slice::from_ref(&apps), &[]),
        Some(app.clone())
    );

    let cache = tmp.path().join("cache");
    let old = file(
        &cache
            .join("chrome-99.0.4844.51-mac-arm64/chrome-mac-arm64")
            .join(CFT),
    );
    let new = file(
        &cache
            .join("chrome-155.0.8059.84-mac-arm64/chrome-mac-arm64")
            .join(CFT),
    );
    assert_ne!(old, new);
    assert_eq!(
        find_window_browser_in(&none, std::slice::from_ref(&cache), &[], &[]),
        Some(new),
        "the newest by version, not by name"
    );

    let bin = tmp.path().join("bin");
    let chromium = file(&bin.join("chromium"));
    let path = std::env::join_paths([&bin]).expect("PATH");
    let on_path = |name: &str| (name == "PATH").then(|| path.clone());
    assert_eq!(
        find_window_browser_in(&on_path, &[], &[], &["chromium"]),
        Some(chromium)
    );

    // Negative: a headless shell, a `$ATERM_BROWSER` naming one, and branded Google Chrome are
    // never a window browser.
    let shells = tmp.path().join("shells");
    let shell = file(
        &shells
            .join("chrome-headless-shell-155.0.8059.84-mac-arm64/chrome-headless-shell-mac-arm64")
            .join("chrome-headless-shell"),
    );
    assert_eq!(
        find_window_browser_in(&none, std::slice::from_ref(&shells), &[], &[]),
        None
    );
    let names_shell = |name: &str| (name == "ATERM_BROWSER").then(|| shell.clone().into());
    assert_eq!(
        find_window_browser_in(&names_shell, std::slice::from_ref(&shells), &[], &[]),
        None
    );
    let branded_apps = tmp.path().join("branded");
    let branded = file(&branded_apps.join("Google Chrome.app/Contents/MacOS/Google Chrome"));
    assert_eq!(
        find_window_browser_in(&none, &[], std::slice::from_ref(&branded_apps), &[]),
        None
    );
    let names_branded = |name: &str| (name == "ATERM_BROWSER").then(|| branded.clone().into());
    assert_eq!(find_window_browser_in(&names_branded, &[], &[], &[]), None);
    let names_app = |name: &str| (name == "ATERM_BROWSER").then(|| app.clone().into());
    assert_eq!(
        find_window_browser_in(&names_app, &[], &[], &[]),
        Some(app.clone()),
        "CONTROL: `$ATERM_BROWSER` naming Chrome for Testing is taken"
    );
    assert_eq!(browser_name(&app), "Google Chrome for Testing");
}

#[cfg(unix)]
fn lock(profile: &Path, target: &str) {
    std::fs::create_dir_all(profile).expect("profile");
    let at = profile.join("SingletonLock");
    let _ = std::fs::remove_file(&at);
    std::os::unix::fs::symlink(target, at).expect("lock");
}

/// A profile a live browser of this machine holds, or any browser of another machine, is named
/// held, by its pid; a lock whose process is gone, or none, is not.
#[cfg(unix)]
#[test]
fn a_profile_a_live_browser_holds_is_named_and_a_stale_lock_is_not() {
    let tmp = aterm_tempfile::tempdir().expect("tmp");
    let profile = tmp.path().join("profile");
    assert_eq!(profile_holder(&profile), None, "no profile, no lock");
    std::fs::create_dir_all(&profile).expect("profile");
    assert_eq!(profile_holder(&profile), None, "no lock");
    let here = host_name().expect("this machine's name");
    let own = std::process::id();
    lock(&profile, &format!("{here}-{own}"));
    let own = i32::try_from(own).expect("pid");
    assert_eq!(profile_holder(&profile), Some(Holder::Browser(own)));
    lock(&profile, "other-desk.example-1");
    assert_eq!(
        profile_holder(&profile),
        Some(Holder::Elsewhere {
            pid: 1,
            host: "other-desk.example".to_owned()
        })
    );
    lock(&profile, &format!("{here}-2147483647"));
    assert_eq!(profile_holder(&profile), None, "a stale lock");
}

#[cfg(unix)]
fn window_with(
    mode: &[&str],
    hang: Duration,
    grace: Duration,
    ended: &dyn Fn() -> Option<WindowEnd>,
) -> WindowEnd {
    let exe = std::env::current_exe().expect("test exe");
    let profile = aterm_tempfile::tempdir().expect("profile");
    run_window_with(
        &exe,
        &stand_in_lead(mode),
        profile.path(),
        "https://www.example.com/",
        hang,
        grace,
        ended,
    )
    .expect("the window ran")
}

/// Enter ends the window with a hangup, which the browser exits on by itself: `Done`, its
/// marker written, no kill. CONTROL: a browser that ignores the hangup is killed after the
/// grace.
#[cfg(unix)]
#[test]
fn the_window_ends_on_enter_with_a_hangup_and_waits_before_any_kill() {
    let dir = aterm_tempfile::tempdir().expect("tmp");
    let at = dir.path().to_string_lossy().into_owned();
    let ready = dir.path().join("ready");
    let entered = || ready.is_file().then_some(WindowEnd::Done);
    let minute = Duration::from_secs(60);
    assert_eq!(
        window_with(&["hup", &at], minute, minute, &entered),
        WindowEnd::Done
    );
    assert!(dir.path().join("hup").is_file(), "it saw the hangup");

    let deaf = aterm_tempfile::tempdir().expect("tmp");
    let at = deaf.path().to_string_lossy().into_owned();
    let ready = deaf.path().join("ready");
    let entered = || ready.is_file().then_some(WindowEnd::Done);
    assert_eq!(
        window_with(
            &["ignore-hup", &at],
            minute,
            Duration::from_millis(300),
            &entered
        ),
        WindowEnd::Killed
    );
}

/// A browser the person quits ends the window with no signal sent: `Quit`, no hangup marker.
/// CONTROL: one that exits 21 by itself is `Exited(21)`.
#[cfg(unix)]
#[test]
fn a_browser_that_quits_by_itself_ends_the_window_without_a_signal() {
    let minute = Duration::from_secs(60);
    let dir = aterm_tempfile::tempdir().expect("tmp");
    let at = dir.path().to_string_lossy().into_owned();
    assert_eq!(
        window_with(&["hup", &at, "quit", "0", "100"], minute, minute, &|| None),
        WindowEnd::Quit
    );
    assert!(!dir.path().join("hup").exists(), "no hangup was sent");
    assert_eq!(
        window_with(&["hup", &at, "quit", "21", "100"], minute, minute, &|| None),
        WindowEnd::Exited(21)
    );
    assert_eq!(WindowEnd::Exited(21).word(), "exited-21");
}

/// Google's other channels are branded Google Chrome as well: Beta, Dev and Canary, named in
/// `$ATERM_BROWSER` or found in an applications folder, and `google-chrome-*` anywhere, are
/// never a window browser. CONTROL: Chrome for Testing and Chromium named so are taken.
#[test]
fn every_google_chrome_channel_is_branded_and_never_a_window_browser() {
    let tmp = aterm_tempfile::tempdir().expect("tmp");
    let none = |_: &str| None::<OsString>;
    for branded in [
        "Google Chrome Beta.app/Contents/MacOS/Google Chrome Beta",
        "Google Chrome Dev.app/Contents/MacOS/Google Chrome Dev",
        "Google Chrome Canary.app/Contents/MacOS/Google Chrome Canary",
        "opt/google/chrome-beta/google-chrome-beta",
        "usr/bin/google-chrome-unstable",
    ] {
        let at = file(&tmp.path().join(branded));
        let named = |name: &str| (name == "ATERM_BROWSER").then(|| at.clone().into());
        assert_eq!(
            find_window_browser_in(&named, &[], &[], &[]),
            None,
            "{branded}"
        );
        assert!(branded_chrome(&at), "{branded}");
    }
    let apps = tmp.path().join("beta-apps");
    file(&apps.join("Google Chrome Beta.app/Contents/MacOS/Google Chrome Beta"));
    assert_eq!(
        find_window_browser_in(&none, &[], std::slice::from_ref(&apps), &[]),
        None
    );
    assert!(branded_chrome(Path::new("/opt/google/chrome/chrome")));
    for taken in [
        CFT,
        "Chromium.app/Contents/MacOS/Chromium",
        "chrome-linux64/chrome",
    ] {
        let at = file(&tmp.path().join("taken").join(taken));
        let named = |name: &str| (name == "ATERM_BROWSER").then(|| at.clone().into());
        assert_eq!(
            find_window_browser_in(&named, &[], &[], &[]),
            Some(at.clone()),
            "CONTROL: {taken}"
        );
    }
}

/// Chrome for Testing's zip unzipped straight into the browser cache (its own
/// `chrome-mac-arm64/` folder there, as the refusal tells a person to) is found, after any
/// versioned build. CONTROL: a folder that is neither is not.
#[test]
fn chrome_for_testing_unzipped_straight_into_the_cache_is_found_after_a_versioned_build() {
    let tmp = aterm_tempfile::tempdir().expect("tmp");
    let none = |_: &str| None::<OsString>;
    let cache = tmp.path().join("browser.noindex");
    let elsewhere = file(&cache.join("downloads/chrome-mac-arm64").join(CFT));
    assert_eq!(
        find_window_browser_in(&none, std::slice::from_ref(&cache), &[], &[]),
        None,
        "CONTROL: {elsewhere:?} is no cache build"
    );
    let bare = file(&cache.join("chrome-mac-arm64").join(CFT));
    assert_eq!(
        find_window_browser_in(&none, std::slice::from_ref(&cache), &[], &[]),
        Some(bare)
    );
    let versioned = file(
        &cache
            .join("chrome-155.0.8059.84-mac-arm64/chrome-mac-arm64")
            .join(CFT),
    );
    assert_eq!(
        find_window_browser_in(&none, std::slice::from_ref(&cache), &[], &[]),
        Some(versioned),
        "a versioned build first"
    );
}

/// One of aterm's launches holds a profile by its own lock, which a headless shell (no
/// `SingletonLock`) needs: while it is held, the profile is named held by aterm, with the pid,
/// and a second claim is refused; dropped, it is free again. CONTROL: a live browser's
/// `SingletonLock` refuses a claim as that browser's.
#[cfg(unix)]
#[test]
fn aterms_own_lock_holds_a_profile_a_headless_shell_has_open() {
    let tmp = aterm_tempfile::tempdir().expect("tmp");
    let profile = tmp.path().join("profile");
    std::fs::create_dir_all(&profile).expect("profile");
    let own = i32::try_from(std::process::id()).expect("pid");
    let held = ProfileLock::claim(&profile).expect("free");
    assert_eq!(profile_holder(&profile), Some(Holder::Aterm(Some(own))));
    match ProfileLock::claim(&profile) {
        Err(Refusal::Held(Holder::Aterm(Some(pid)))) => assert_eq!(pid, own),
        other => panic!("{other:?}"),
    }
    drop(held);
    assert_eq!(profile_holder(&profile), None, "released on drop");
    let again = ProfileLock::claim(&profile).expect("free again");
    drop(again);

    let here = host_name().expect("this machine's name");
    lock(&profile, &format!("{here}-{own}"));
    match ProfileLock::claim(&profile) {
        Err(Refusal::Held(Holder::Browser(pid))) => assert_eq!(pid, own),
        other => panic!("CONTROL: {other:?}"),
    }
}

/// A STALE LOCK WHOSE PID WAS TAKEN BY ANOTHER PROCESS HOLDS NOTHING: a lock naming a live
/// process that started after the lock was written (its pid reused, as after a crash or a
/// reboot) or another user's process (launchd's pid 1) is stale, so the profile is free and
/// claimed. CONTROL: a lock naming a live process of this user that started before it was
/// written is that browser's, and the profile is refused.
#[cfg(target_os = "macos")]
#[test]
fn a_lock_whose_pid_now_names_a_later_or_another_users_process_is_stale() {
    let tmp = aterm_tempfile::tempdir().expect("tmp");
    let profile = tmp.path().join("profile");
    let here = host_name().expect("this machine's name");
    let own = std::process::id();
    let own_pid = i32::try_from(own).expect("pid");
    lock(&profile, &format!("{here}-{own}"));
    assert_eq!(
        profile_holder(&profile),
        Some(Holder::Browser(own_pid)),
        "CONTROL: written after this process started"
    );
    assert!(ProfileLock::claim(&profile).is_err(), "CONTROL: refused");

    let at = std::ffi::CString::new(profile.join("SingletonLock").as_os_str().as_encoded_bytes())
        .expect("path");
    let long_ago = [
        libc::timeval {
            tv_sec: 1_000_000,
            tv_usec: 0,
        },
        libc::timeval {
            tv_sec: 1_000_000,
            tv_usec: 0,
        },
    ];
    // SAFETY: `at` is a NUL-terminated path and `long_ago` two timevals, both outliving the
    // call, which sets the link's own times and follows nothing.
    assert_eq!(unsafe { libc::lutimes(at.as_ptr(), long_ago.as_ptr()) }, 0);
    assert_eq!(
        profile_holder(&profile),
        None,
        "this process started after that lock was written, so it is not its browser"
    );
    drop(ProfileLock::claim(&profile).expect("a stale lock leaves the profile free"));

    lock(&profile, &format!("{here}-1"));
    assert_eq!(
        profile_holder(&profile),
        None,
        "launchd is no browser of this user's"
    );
}

/// A holder's words say what to do: quit a browser of this machine, wait for aterm's own, and,
/// for a lock naming another machine (perhaps stale there), remove the profile's
/// `SingletonLock` rather than quit a browser that is not here. Each has its own static brief.
#[test]
fn a_holders_words_say_what_to_do_and_another_machines_lock_names_its_file() {
    let profile = Path::new("/state/accounts/browsers/kimi-one-at-example.com");
    let here = Holder::Browser(42).words(profile);
    assert!(
        here.contains("pid 42") && here.contains("quit it"),
        "{here}"
    );
    let far = Holder::Elsewhere {
        pid: 7,
        host: "other-desk.example".to_owned(),
    }
    .words(profile);
    assert!(
        far.contains("pid 7 on other-desk.example")
            && far.contains(&format!("remove {}/SingletonLock", profile.display()))
            && !far.contains("quit it"),
        "{far}"
    );
    let ours = Holder::Aterm(Some(9)).words(profile);
    assert!(
        ours.contains("pid 9") && ours.contains("wait for it"),
        "{ours}"
    );
    let briefs = [
        Holder::Browser(1).brief(),
        Holder::Elsewhere {
            pid: 1,
            host: "h".to_owned(),
        }
        .brief(),
        Holder::Aterm(None).brief(),
    ];
    assert!(briefs[1].contains("SingletonLock") && !briefs[1].contains("quit it"));
    assert!(briefs[0] != briefs[1] && briefs[1] != briefs[2] && briefs[0] != briefs[2]);
}

/// The identity providers' cookies are Google's under any top-level domain, YouTube's, and those
/// of Apple, Microsoft, GitHub, X and Facebook with their subdomains. CONTROL: the vendors' own
/// (kimi.ai, claude.ai, x.ai, openai.com) and look-alikes are not.
#[test]
fn identity_providers_cookies_are_named_by_their_domains_and_the_vendors_are_not() {
    for idp in [
        ".google.com",
        "accounts.google.com",
        "google.co.uk",
        ".google.com.hk",
        "accounts.youtube.com",
        ".youtube.com",
        "appleid.apple.com",
        ".icloud.com",
        "login.microsoftonline.com",
        "login.live.com",
        "github.com",
        ".x.com",
        "twitter.com",
        "www.facebook.com",
    ] {
        assert!(idp_cookie(idp), "{idp}");
    }
    for own in [
        ".kimi.ai",
        "www.kimi.ai",
        "www.kimi.com",
        "claude.ai",
        ".x.ai",
        "accounts.x.ai",
        "auth.openai.com",
        "googleapis.com",
        "notgoogle.com",
        "google.example.org",
        "google.foo.org",
        "google.abc.com",
        "youtube.bar.net",
        "google.foo.uk",
        "google.a.b.c.com",
        "google.127.0.0.1",
        "mygithub.com",
        "box.com",
        "127.0.0.1",
    ] {
        assert!(!idp_cookie(own), "CONTROL: {own}");
    }
}

/// The window's profile has its password manager's offers turned off, its other settings kept;
/// a profile with no settings yet, or settings that are no JSON object, gets them alone.
#[test]
fn the_window_turns_off_the_password_managers_offers_and_keeps_other_settings() {
    let tmp = aterm_tempfile::tempdir().expect("tmp");
    let read = |profile: &Path| -> Value {
        aterm_json::from_str(
            &std::fs::read_to_string(profile.join("Default/Preferences")).expect("prefs"),
        )
        .expect("json")
    };
    let used = tmp.path().join("used");
    std::fs::create_dir_all(used.join("Default")).expect("profile");
    std::fs::write(
        used.join("Default/Preferences"),
        r#"{"intl":{"accept_languages":"en"},"profile":{"name":"Person 1","password_manager_enabled":true},"credentials_enable_service":true}"#,
    )
    .expect("prefs");
    quiet_password_manager(&used).expect("quieted");
    let prefs = read(&used);
    assert_eq!(
        prefs.pointer("/credentials_enable_service"),
        Some(&Value::Bool(false))
    );
    assert_eq!(
        prefs.pointer("/profile/password_manager_enabled"),
        Some(&Value::Bool(false))
    );
    assert_eq!(
        prefs.pointer("/profile/name").and_then(Value::as_str),
        Some("Person 1")
    );
    assert_eq!(
        prefs
            .pointer("/intl/accept_languages")
            .and_then(Value::as_str),
        Some("en")
    );
    for (name, text) in [("fresh", None), ("broken", Some("[1, 2"))] {
        let profile = tmp.path().join(name);
        if let Some(text) = text {
            std::fs::create_dir_all(profile.join("Default")).expect("profile");
            std::fs::write(profile.join("Default/Preferences"), text).expect("prefs");
        }
        quiet_password_manager(&profile).expect("quieted");
        let prefs = read(&profile);
        assert_eq!(
            prefs.pointer("/credentials_enable_service"),
            Some(&Value::Bool(false))
        );
        assert_eq!(
            prefs.pointer("/profile/password_manager_enabled"),
            Some(&Value::Bool(false))
        );
    }
}

/// What a stand-in browser over the DevTools pipes saw: each method called, in order, and the
/// cookies it was asked to delete.
#[derive(Debug, Default)]
struct Seen {
    methods: Vec<String>,
    deleted: Vec<Value>,
    fulfilled: Vec<String>,
    failed: Vec<String>,
}

/// A stand-in browser for [`tidy_over_cdp`]: its cookie store `cookies` (a delete removes one
/// by name, domain and path, unless `stuck`); a navigation to `origin` pauses a favicon's request,
/// then its page's; the page asked after `localStorage` answers `kept` once it was served.
fn tidy_browser(
    cookies: Vec<Value>,
    stuck: bool,
    kept: bool,
) -> (
    super::super::headless_oauth::CdpClient,
    std::thread::JoinHandle<Seen>,
) {
    use std::io::{Read as _, Write as _};
    let (to_browser_r, to_browser_w) = std::io::pipe().expect("pipe");
    let (from_browser_r, mut from_browser_w) = std::io::pipe().expect("pipe");
    let browser = std::thread::spawn(move || {
        let mut seen = Seen::default();
        let mut store = cookies;
        let mut reader = to_browser_r;
        let mut pending = Vec::new();
        let mut buf = [0u8; 8192];
        let send = |w: &mut std::io::PipeWriter, v: Value| {
            let mut bytes = aterm_json::to_vec(&v).expect("json");
            bytes.push(0);
            w.write_all(&bytes).is_ok()
        };
        while let Ok(n) = reader.read(&mut buf) {
            if n == 0 {
                break;
            }
            pending.extend_from_slice(&buf[..n]);
            for msg in super::super::headless_oauth::split_nul_messages(&mut pending) {
                let id = msg.get("id").and_then(Value::as_u64).unwrap_or(0);
                let method = msg
                    .get("method")
                    .and_then(Value::as_str)
                    .unwrap_or("")
                    .to_owned();
                let params = msg.get("params").cloned().unwrap_or(Value::Null);
                seen.methods.push(method.clone());
                let mut then = None;
                let result = match method.as_str() {
                    "Storage.getCookies" => json!({"cookies": store.clone()}),
                    "Target.createTarget" => json!({"targetId": "t1"}),
                    "Target.attachToTarget" => json!({"sessionId": "s1"}),
                    "Network.deleteCookies" => {
                        seen.deleted.push(params.clone());
                        if !stuck {
                            store.retain(|c| {
                                ["name", "domain", "path"]
                                    .iter()
                                    .any(|k| c.get(k) != params.get(k))
                            });
                        }
                        json!({})
                    }
                    "Fetch.fulfillRequest" => {
                        seen.fulfilled.push(
                            params
                                .get("requestId")
                                .and_then(Value::as_str)
                                .unwrap_or("")
                                .to_owned(),
                        );
                        json!({})
                    }
                    "Fetch.failRequest" => {
                        seen.failed.push(
                            params
                                .get("requestId")
                                .and_then(Value::as_str)
                                .unwrap_or("")
                                .to_owned(),
                        );
                        json!({})
                    }
                    "Runtime.evaluate" => {
                        let expr = params
                            .get("expression")
                            .and_then(Value::as_str)
                            .unwrap_or("");
                        if expr.starts_with("location.href = ") {
                            then =
                                Some(["https://www.kimi.ai/favicon.ico", "https://www.kimi.ai/"]);
                            json!({"result": {"type": "boolean", "value": true}})
                        } else if expr.contains("localStorage.getItem(\"access_token\")") {
                            let at_origin = seen.fulfilled.iter().any(|r| r == "r1");
                            json!({"result": if at_origin {
                                json!({"type": "boolean", "value": kept})
                            } else {
                                json!({"type": "object", "value": null})
                            }})
                        } else {
                            json!({"result": {"type": "undefined"}})
                        }
                    }
                    _ => json!({}),
                };
                if !send(&mut from_browser_w, json!({"id": id, "result": result})) {
                    return seen;
                }
                for (at, url) in then.into_iter().flatten().enumerate() {
                    let paused = json!({
                        "method": "Fetch.requestPaused",
                        "sessionId": "s1",
                        "params": {"requestId": format!("r{at}"), "request": {"url": url}}
                    });
                    let _ = send(&mut from_browser_w, paused);
                }
                if method == "Browser.close" {
                    return seen;
                }
            }
        }
        seen
    });
    (
        super::super::headless_oauth::CdpClient::new(to_browser_w, from_browser_r),
        browser,
    )
}

fn cookie(name: &str, domain: &str) -> Value {
    json!({"name": name, "domain": domain, "path": "/", "value": "never-read", "secure": true})
}

/// A HEADLESS LAUNCH MAKES UP A WINDOW'S PASS THAT NEVER RAN: on a profile carrying the untidied
/// mark it deletes every identity provider's cookie in a page of its own, leaves the browser open
/// for the launch, and removes the mark. CONTROLS: an unmarked profile is not touched (not one
/// call); a store that keeps a deleted cookie fails and keeps the mark for the next launch.
#[test]
fn a_launch_makes_up_a_windows_pass_that_never_ran_and_only_then() {
    let deadline = || Instant::now() + Duration::from_secs(60);
    let tmp = aterm_tempfile::tempdir().expect("tmp");
    let profile = tmp.path().join("profile");
    std::fs::create_dir_all(&profile).expect("profile");
    let store = vec![
        cookie("SID", ".google.com"),
        cookie("MSPAuth", ".login.live.com"),
        cookie("kimi-auth", ".kimi.ai"),
    ];

    let (mut cdp, browser) = tidy_browser(store.clone(), false, true);
    assert_eq!(make_up_tidy(&mut cdp, &profile, deadline()), None);
    drop(cdp);
    let seen = browser.join().expect("stand-in");
    assert!(seen.methods.is_empty(), "CONTROL: {:?}", seen.methods);

    mark_untidied(&profile).expect("mark");
    let (mut cdp, browser) = tidy_browser(store.clone(), false, true);
    assert_eq!(make_up_tidy(&mut cdp, &profile, deadline()), Some(Ok(2)));
    drop(cdp);
    let seen = browser.join().expect("stand-in");
    assert_eq!(
        seen.deleted,
        [
            json!({"name": "SID", "domain": ".google.com", "path": "/"}),
            json!({"name": "MSPAuth", "domain": ".login.live.com", "path": "/"}),
        ]
    );
    assert!(
        !seen.methods.iter().any(|m| m == "Browser.close"),
        "the launch keeps its browser: {:?}",
        seen.methods
    );
    assert!(!profile.join(UNTIDIED_MARK).exists(), "made up");

    mark_untidied(&profile).expect("mark");
    let (mut cdp, browser) = tidy_browser(store, true, true);
    let stuck = make_up_tidy(&mut cdp, &profile, deadline());
    drop(cdp);
    browser.join().expect("stand-in");
    assert!(
        stuck.is_some_and(|r| r.is_err_and(|why| why.contains("stayed"))),
        "CONTROL"
    );
    assert!(profile.join(UNTIDIED_MARK).is_file(), "CONTROL: kept");
    clear_untidied(&profile).expect("cleared");
    clear_untidied(&profile).expect("clearing none is no error");
}

/// The pass after the window deletes every identity provider's cookie by its name, domain and
/// path and keeps the vendor's, then serves a blank page at the vendor's origin itself (its own
/// request answered here, any other failed, none reaching the network) and asks whether its
/// `localStorage` keeps the session's key. CONTROLS: a store that keeps a deleted cookie is an
/// error; with no key to ask after, no request is intercepted and the session is unknown; a
/// profile that keeps no session says so.
#[test]
fn the_pass_after_the_window_drops_identity_provider_cookies_and_asks_after_the_session() {
    let deadline = || Instant::now() + Duration::from_secs(60);
    let store = vec![
        cookie("SID", ".google.com"),
        cookie("__Host-GAPS", "accounts.google.com"),
        cookie("kimi-auth", ".kimi.ai"),
    ];
    let (mut cdp, browser) = tidy_browser(store.clone(), false, true);
    let tidied = tidy_over_cdp(
        &mut cdp,
        Some(("https://www.kimi.ai", "access_token")),
        deadline(),
    );
    drop(cdp);
    let seen = browser.join().expect("stand-in");
    assert_eq!(
        tidied,
        Ok(Tidied {
            dropped: 2,
            session: Some(true)
        })
    );
    assert_eq!(
        seen.deleted,
        [
            json!({"name": "SID", "domain": ".google.com", "path": "/"}),
            json!({"name": "__Host-GAPS", "domain": "accounts.google.com", "path": "/"}),
        ],
        "deleted by name, domain and path, and never by value"
    );
    assert_eq!(seen.fulfilled, ["r1"], "its own page served here");
    assert_eq!(seen.failed, ["r0"], "anything else failed, never sent");
    let fetch = seen
        .methods
        .iter()
        .position(|m| m == "Fetch.enable")
        .expect("requests intercepted");
    let bypass = seen
        .methods
        .iter()
        .position(|m| m == "Network.setBypassServiceWorker")
        .expect("service worker bypassed");
    let sent = seen
        .methods
        .iter()
        .position(|m| m == "Runtime.evaluate")
        .expect("navigated");
    assert!(bypass < sent && fetch < sent, "{:?}", seen.methods);
    assert_eq!(
        seen.methods.last().map(String::as_str),
        Some("Browser.close")
    );

    let (mut cdp, browser) = tidy_browser(store.clone(), true, true);
    let stuck = tidy_over_cdp(&mut cdp, None, deadline());
    drop(cdp);
    browser.join().expect("stand-in");
    assert!(
        stuck
            .as_ref()
            .is_err_and(|why| why.contains("2 identity-provider cookie(s) stayed")),
        "CONTROL: {stuck:?}"
    );

    let (mut cdp, browser) = tidy_browser(vec![cookie("kimi-auth", ".kimi.ai")], false, true);
    let unasked = tidy_over_cdp(&mut cdp, None, deadline());
    drop(cdp);
    let seen = browser.join().expect("stand-in");
    assert_eq!(
        unasked,
        Ok(Tidied {
            dropped: 0,
            session: None
        })
    );
    assert!(
        !seen.methods.iter().any(|m| m.starts_with("Fetch.")),
        "CONTROL: {:?}",
        seen.methods
    );

    let (mut cdp, browser) = tidy_browser(store, false, false);
    let signed_out = tidy_over_cdp(
        &mut cdp,
        Some(("https://www.kimi.ai", "access_token")),
        deadline(),
    );
    drop(cdp);
    browser.join().expect("stand-in");
    assert_eq!(
        signed_out,
        Ok(Tidied {
            dropped: 2,
            session: Some(false)
        })
    );
}
