// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! `swarm mailbox-signin` (`docs/DESIGN-alab-swarm-2026-10-27.md` §6.2, §12.3 C0):
//! measures automatic mailbox sign-in on an isolated headless `lab instance`.
//!
//! 1. **Negative control (mailbox config withheld)**: starts the instance with an unenrolled
//!    Claude roster row and no `ai-mailbox.toml`, verifying window upkeep records
//!    `AttemptOutcome::NoRoute` (`"no-route"`) and leaves the row unenrolled.
//! 2. **Positive flow (mailbox config supplied)**: supplies `~/.secrets/ai-mailbox.toml` +
//!    incoming verification email, wakes upkeep, measures sign-in latency until the row is
//!    enrolled via `Route::Mailbox`, launches `claude` on the newly bound login directory, and
//!    serves a turn (`state.account(&label).ran_at.is_some()` and `logins[&label].served_at.is_some()`).
//! 3. **`--live` mode**: runs the same measured proof against the host's real `claude` binary and
//!    `~/.secrets/ai-mailbox.toml` (or `--mailbox PATH`), optionally writing `docs/measured/`.

use std::collections::BTreeSet;
use std::io::Write as _;
use std::os::unix::fs::PermissionsExt as _;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use aterm_agent::harness::accounts::{self, AttemptOutcome, HOME_ENV, LOGIN_ENV, Store};
use aterm_agent::harness::accounts_sync::UPKEEP_SEAM;
use aterm_agent::harness::headless_oauth::{self, MailboxCredential, SECRETS_MAILBOX_TOML_FILE};
use aterm_agent::harness::tap;
use aterm_codec::json::escape;

use super::{HANG, STANDIN_MARKER, SWARM_CONFIG, ctl_ok};
use crate::instance::{Instance, Launch, World, resolve_aterm, take_value};
use crate::pyjson::{COMPACT, J};
use crate::screen::field;
use crate::util::{atomic_write, now_ms, now_secs, scratch_text, shell_quote, utc_stamp};
use crate::{Fail, LAB, Res, sys};

/// Default roster label for hermetic `swarm mailbox-signin`.
pub const LABEL_MBX: &str = "swarm-mbx";
/// Default email address for hermetic `swarm mailbox-signin`.
pub const EMAIL_MBX: &str = "swarm-mbx@example.test";
/// Verification code delivered in the hermetic Maildir message.
pub const EXPECTED_OTP: &str = "482917";

/// Parsed CLI options for `lab swarm mailbox-signin`.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct MailboxSigninOpts {
    pub live: bool,
    pub email: Option<String>,
    pub label: Option<String>,
    pub mailbox: Option<String>,
    pub record: bool,
    pub aterm: Option<String>,
    pub keep: bool,
}

/// Parse `lab swarm mailbox-signin ...` arguments.
pub fn parse_mailbox_args(args: &[String]) -> Result<MailboxSigninOpts, Fail> {
    if args.iter().any(|a| a == "-h" || a == "--help") {
        return Err(Fail::usage(
            "swarm mailbox-signin [--live] [--email EMAIL] [--label LABEL] [--mailbox PATH] [--record] [--aterm PATH] [--keep]",
        ));
    }
    let mut rem = args.to_vec();
    let mut opts = MailboxSigninOpts {
        email: take_value(&mut rem, "--email")?,
        label: take_value(&mut rem, "--label")?,
        mailbox: take_value(&mut rem, "--mailbox")?,
        aterm: take_value(&mut rem, "--aterm")?,
        ..MailboxSigninOpts::default()
    };
    for arg in rem {
        match arg.as_str() {
            "--live" => opts.live = true,
            "--record" => opts.record = true,
            "--keep" => opts.keep = true,
            other => {
                return Err(Fail::usage(format!(
                    "swarm mailbox-signin: unexpected '{other}' ({LAB} swarm --help)"
                )));
            }
        }
    }
    Ok(opts)
}

/// Captured facts from a `lab swarm mailbox-signin` run, judged purely from data.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MailboxEvidence {
    pub withheld_outcome: Option<String>,
    pub enrolled_while_withheld: bool,
    pub signin_outcome: Option<String>,
    pub signin_route: Option<String>,
    pub enrolled_after: BTreeSet<String>,
    pub bound_dir: Option<String>,
    pub turns_served: usize,
    pub account_ran: bool,
    pub login_served: bool,
    pub withheld_ms: u128,
    pub signin_ms: u128,
    pub turn_ms: u128,
}

/// Pure judge over [`MailboxEvidence`]: returns every failed invariant (empty = PASS).
#[must_use]
pub fn judge_mailbox_evidence(ev: &MailboxEvidence, expected_label: &str) -> Vec<String> {
    let mut bad = Vec::new();
    if ev.withheld_outcome.as_deref() != Some("no-route") {
        bad.push(format!(
            "withheld mailbox negative control recorded {:?}, expected Some(\"no-route\")",
            ev.withheld_outcome
        ));
    }
    if ev.enrolled_while_withheld {
        bad.push(format!(
            "{expected_label} became enrolled while mailbox config was withheld"
        ));
    }
    if ev.signin_outcome.as_deref() != Some(AttemptOutcome::SignedIn.as_str()) {
        bad.push(format!(
            "sign-in outcome after supplying mailbox config is {:?}, expected Some({:?})",
            ev.signin_outcome,
            AttemptOutcome::SignedIn.as_str()
        ));
    }
    if ev.signin_route.as_deref() != Some("mailbox") {
        bad.push(format!(
            "sign-in route is {:?}, expected Some(\"mailbox\")",
            ev.signin_route
        ));
    }
    if !ev.enrolled_after.contains(expected_label) {
        bad.push(format!(
            "enrolled set {:?} does not contain {expected_label:?}",
            ev.enrolled_after
        ));
    }
    if ev.bound_dir.as_deref().is_none_or(str::is_empty) {
        bad.push(format!(
            "bound roster row for {expected_label} has no login dir"
        ));
    }
    if ev.turns_served == 0 {
        bad.push(format!(
            "{expected_label} served 0 turns after mailbox sign-in"
        ));
    }
    if !ev.account_ran {
        bad.push(format!(
            "accounts/state.json did not record an Auth::Ok run for {expected_label}"
        ));
    }
    if !ev.login_served {
        bad.push(format!(
            "accounts/logins.json did not record served_at for {expected_label}"
        ));
    }
    bad
}

fn extract_email_flag(args: &[String]) -> Option<String> {
    let mut it = args.iter();
    while let Some(arg) = it.next() {
        if arg == "--email" {
            return it.next().cloned();
        }
        if let Some(v) = arg.strip_prefix("--email=") {
            return Some(v.to_owned());
        }
    }
    None
}

/// Pure-Rust `claude auth ...` handler for the hermetic stand-in, taking explicit `home` and
/// `claude_config_dir` so unit tests run with zero process-environment mutation.
pub fn run_standin_auth(
    args: &[String],
    home: &Path,
    claude_config_dir: Option<&Path>,
) -> (i32, String) {
    let sub = args.first().map(String::as_str).unwrap_or("");
    let target_dir = claude_config_dir
        .map(Path::to_path_buf)
        .unwrap_or_else(|| home.join(".claude"));
    let marker = scratch_text(&home.join(STANDIN_MARKER));
    let marker_cfg = J::parse(&marker).ok();
    let store_dir = marker_cfg
        .as_ref()
        .and_then(|c| c.get("store_dir"))
        .and_then(J::as_str)
        .map(PathBuf::from);
    let expected_otp = marker_cfg
        .as_ref()
        .and_then(|c| c.get("expected_otp"))
        .and_then(J::as_str)
        .map(str::to_owned);
    let ctl_dir = marker_cfg
        .as_ref()
        .and_then(|c| c.get("control_dir"))
        .and_then(J::as_str)
        .map(PathBuf::from);

    match sub {
        "login" => {
            let Some(email) = extract_email_flag(args).filter(|e| !e.trim().is_empty()) else {
                return (1, String::new());
            };
            let Some(mut mbx_cfg) =
                headless_oauth::discover_mailbox_config(Some(home), store_dir.as_deref())
            else {
                return (1, String::new());
            };
            mbx_cfg.alias_label_to_email(None, Some(&email));
            if !mbx_cfg.is_configured_for(Some(&email)) {
                return (1, String::new());
            }
            let Ok(cred) = headless_oauth::poll_mailbox_config_for_credential(
                &mbx_cfg,
                Some(&email),
                Duration::from_secs(2),
            ) else {
                return (1, String::new());
            };
            let otp_ok = match (&cred, expected_otp.as_deref()) {
                (MailboxCredential::Otp(code), Some(want)) => code == want,
                (MailboxCredential::Otp(code), None) => {
                    code.len() == 6 && code.chars().all(|c| c.is_ascii_digit())
                }
                (MailboxCredential::MagicLink(url), _) => !url.is_empty(),
            };
            if !otp_ok {
                return (1, String::new());
            }
            if std::fs::create_dir_all(&target_dir).is_err() {
                return (1, String::new());
            }
            let _ = std::fs::set_permissions(&target_dir, std::fs::Permissions::from_mode(0o700));
            let expires_ms = (now_secs() + 86_400) * 1000;
            let creds = format!(
                "{{\"claudeAiOauth\":{{\"accessToken\":\"tok-mbx-{}\",\"refreshToken\":\"ref-mbx-{}\",\
                 \"expiresAt\":{expires_ms},\"scopes\":[\"user:inference\",\"user:profile\"],\
                 \"subscriptionType\":\"max\"}}}}\n",
                escape(&email),
                escape(&email),
            );
            if sys::put(
                &target_dir.join(".credentials.json"),
                0o600,
                &sys::PutSrc::Bytes(creds.as_bytes()),
            )
            .is_err()
            {
                return (1, String::new());
            }
            let profile = format!(
                "{{\"oauthAccount\":{{\"accountUuid\":\"uuid-mbx\",\"emailAddress\":\"{}\",\
                 \"organizationType\":\"claude_max\"}},\"hasCompletedOnboarding\":true}}\n",
                escape(&email),
            );
            if sys::put(
                &target_dir.join(".claude.json"),
                0o600,
                &sys::PutSrc::Bytes(profile.as_bytes()),
            )
            .is_err()
            {
                return (1, String::new());
            }
            if let Some(ctl) = ctl_dir {
                let _ = std::fs::create_dir_all(&ctl);
                if let Ok(mut f) = std::fs::OpenOptions::new()
                    .create(true)
                    .append(true)
                    .open(ctl.join("auth-logins.tsv"))
                {
                    let _ = writeln!(f, "{email}\t{}", target_dir.display());
                }
            }
            (0, format!("Logged in as {email}\n"))
        }
        "status" => {
            let creds_ok = std::fs::metadata(target_dir.join(".credentials.json"))
                .is_ok_and(|m| m.is_file() && m.len() > 2);
            let profile_raw = scratch_text(&target_dir.join(".claude.json"));
            let email = J::parse(&profile_raw)
                .ok()
                .and_then(|j| {
                    j.get("oauthAccount")
                        .and_then(|o| o.get("emailAddress"))
                        .and_then(J::as_str)
                        .map(str::to_owned)
                })
                .filter(|e| !e.is_empty());
            match (creds_ok, email) {
                (true, Some(email)) => (
                    0,
                    format!(
                        "{{\"loggedIn\":true,\"authMethod\":\"claude.ai\",\"email\":\"{}\",\"subscriptionType\":\"max\"}}\n",
                        escape(&email),
                    ),
                ),
                _ => (
                    1,
                    "{\"loggedIn\":false,\"authMethod\":null,\"email\":null,\"subscriptionType\":null}\n"
                        .to_owned(),
                ),
            }
        }
        _ => (1, String::new()),
    }
}

/// Stand-in entry point invoked from [`super::claude_main`] when `argv[1] == "auth"`.
pub fn claude_auth_main(args: &[String]) -> i32 {
    let Some(home) = std::env::var_os("HOME").map(PathBuf::from) else {
        return 1;
    };
    let cfg_dir = std::env::var_os(HOME_ENV)
        .filter(|v| !v.is_empty())
        .map(PathBuf::from);
    let (code, stdout) = run_standin_auth(args, &home, cfg_dir.as_deref());
    if !stdout.is_empty() {
        print!("{stdout}");
    }
    code
}

/// Write an RFC 5322 verification message containing `otp` addressed to `email` inside `maildir`.
pub fn write_verification_email(maildir: &Path, email: &str, otp: &str) -> Result<PathBuf, String> {
    std::fs::create_dir_all(maildir).map_err(|e| format!("create {}: {e}", maildir.display()))?;
    let path = maildir.join("verify-001.eml");
    let msg = format!(
        "From: Anthropic <noreply@mail.claude.ai>\r\n\
         To: <{email}>\r\n\
         Subject: Your Claude verification code\r\n\
         MIME-Version: 1.0\r\n\
         Content-Type: text/plain; charset=utf-8\r\n\r\n\
         Your verification code for Claude is {otp}.\r\n"
    );
    atomic_write(&path, msg.as_bytes()).map_err(|e| format!("write {}: {e}", path.display()))?;
    Ok(path)
}

/// Write `~/.secrets/ai-mailbox.toml` (`0600`) pointing at `maildir` with a password for `email`.
pub fn write_hermetic_ai_mailbox_toml(
    home: &Path,
    maildir: &Path,
    email: &str,
) -> Result<PathBuf, String> {
    let secrets = home.join(".secrets");
    std::fs::create_dir_all(&secrets).map_err(|e| format!("create {}: {e}", secrets.display()))?;
    std::fs::set_permissions(&secrets, std::fs::Permissions::from_mode(0o700))
        .map_err(|e| format!("chmod {}: {e}", secrets.display()))?;
    let path = secrets.join(SECRETS_MAILBOX_TOML_FILE);
    let body = format!(
        "dir = \"{}\"\n\n[passwords]\n\"{}\" = \"hermetic-mailbox-pass\"\n",
        escape(&maildir.to_string_lossy()),
        escape(&email.to_ascii_lowercase()),
    );
    sys::put(&path, 0o600, &sys::PutSrc::Bytes(body.as_bytes()))
        .map_err(|e| format!("write {}: {e}", path.display()))?;
    Ok(path)
}

fn resolve_host_claude() -> Option<PathBuf> {
    if let Some(real_home) = std::env::var_os("HOME").map(PathBuf::from) {
        let candidates = [
            atpkg::store::default_prefix(&real_home).join("agents/claude"),
            real_home.join(".local/bin/claude"),
        ];
        for cand in candidates {
            if cand.is_file() {
                return Some(cand);
            }
        }
    }
    std::env::var_os("PATH").and_then(|paths| {
        std::env::split_paths(&paths)
            .map(|dir| dir.join("claude"))
            .find(|p| p.is_file())
    })
}

fn pick_live_email(cfg: &headless_oauth::MailboxConfig, explicit: Option<&str>) -> Option<String> {
    if let Some(e) = explicit.filter(|s| !s.trim().is_empty()) {
        return Some(e.trim().to_owned());
    }
    cfg.passwords
        .keys()
        .find(|k| k.contains('@'))
        .cloned()
        .or_else(|| cfg.user.clone().filter(|u| u.contains('@')))
}

fn write_measured_report(mode: &str, label: &str, ev: &MailboxEvidence) -> Result<PathBuf, String> {
    let stamp = utc_stamp(now_secs());
    let day = stamp.get(..10).unwrap_or("2026-10-28");
    let path = PathBuf::from(format!("docs/measured/claude-mailbox-signin-{day}.md"));
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).map_err(|e| e.to_string())?;
    }
    let text = format!(
        "# Measured Claude Mailbox Sign-In (`C0`)\n\n\
         - **Date:** `{stamp}`\n\
         - **Mode:** `{mode}`\n\
         - **Row label:** `{label}`\n\
         - **Negative control (`ai-mailbox.toml` withheld):** `{:?}` in `{} ms` (`enrolled_while_withheld = {}`)\n\
         - **Mailbox sign-in outcome:** `{:?}` via route `{:?}` in `{} ms`\n\
         - **Turn served on enrolled login:** `turns = {}`, `account_ran = {}`, `login_served = {}` in `{} ms`\n",
        ev.withheld_outcome,
        ev.withheld_ms,
        ev.enrolled_while_withheld,
        ev.signin_outcome,
        ev.signin_route,
        ev.signin_ms,
        ev.turns_served,
        ev.account_ran,
        ev.login_served,
        ev.turn_ms,
    );
    atomic_write(&path, text.as_bytes()).map_err(|e| e.to_string())?;
    Ok(path)
}

/// Entry point for `targo --unverified lab swarm mailbox-signin`.
pub fn cmd_mailbox_signin(args: &[String]) -> Res {
    let opts = parse_mailbox_args(args)?;
    let aterm = resolve_aterm(opts.aterm.clone())?;

    let (live_mailbox_bytes, label, email, host_claude) = if opts.live {
        let mbx_src = opts.mailbox.as_deref().map_or_else(
            || {
                std::env::var_os("HOME")
                    .map(PathBuf::from)
                    .unwrap_or_else(|| PathBuf::from("/var/empty"))
                    .join(".secrets")
                    .join(SECRETS_MAILBOX_TOML_FILE)
            },
            PathBuf::from,
        );
        if !mbx_src.is_file() {
            return Err(Fail::die(format!(
                "swarm mailbox-signin --live: {} does not exist; publish it first with \
                 `aterm secrets set ai-mailbox.toml --file <path>`",
                mbx_src.display()
            )));
        }
        let raw =
            std::fs::read(&mbx_src).map_err(|e| format!("read {}: {e}", mbx_src.display()))?;
        let tmp_check = aterm_tempfile::tempdir().map_err(|e| e.to_string())?;
        let tmp_secrets = tmp_check.path().join(".secrets");
        std::fs::create_dir_all(&tmp_secrets).map_err(|e| e.to_string())?;
        sys::put(
            &tmp_secrets.join(SECRETS_MAILBOX_TOML_FILE),
            0o600,
            &sys::PutSrc::Bytes(&raw),
        )
        .map_err(|e| e.to_string())?;
        let parsed = headless_oauth::discover_mailbox_config(Some(tmp_check.path()), None)
            .ok_or_else(|| {
                Fail::die(format!(
                    "{} parsed no mailbox configuration",
                    mbx_src.display()
                ))
            })?;
        let email = pick_live_email(&parsed, opts.email.as_deref()).ok_or_else(|| {
            Fail::die(
                "swarm mailbox-signin --live: pass `--email <addr>` or list an email under [passwords] in ai-mailbox.toml",
            )
        })?;
        let claude = resolve_host_claude().ok_or_else(|| {
            Fail::die("swarm mailbox-signin --live: could not locate installed `claude` binary")
        })?;
        let label = opts.label.clone().unwrap_or_else(|| "c0-live".to_owned());
        (Some(raw), label, email, Some(claude))
    } else {
        let label = opts.label.clone().unwrap_or_else(|| LABEL_MBX.to_owned());
        let email = opts.email.clone().unwrap_or_else(|| EMAIL_MBX.to_owned());
        (None, label, email, None)
    };

    let world_cfg = format!("{SWARM_CONFIG}[fleet]\nmachine = \"m15\"\n");
    let mut world = World::new(&world_cfg)?;
    if opts.keep {
        world.keep();
    }

    let secrets_dir = world.home.join(".secrets");
    std::fs::create_dir_all(&secrets_dir).map_err(|e| e.to_string())?;
    std::fs::set_permissions(&secrets_dir, std::fs::Permissions::from_mode(0o700))
        .map_err(|e| e.to_string())?;

    let harness = world.state().join("harness/claude-harness");
    let store = Store::at(&harness);
    std::fs::create_dir_all(store.logins_dir()).map_err(|e| e.to_string())?;

    let claude_home = world.home.join(".claude");
    std::fs::create_dir_all(&claude_home).map_err(|e| e.to_string())?;
    std::fs::set_permissions(&claude_home, std::fs::Permissions::from_mode(0o700))
        .map_err(|e| e.to_string())?;
    let base_expires_ms = (now_secs() + 86_400) * 1000;
    let base_creds = format!(
        "{{\"claudeAiOauth\":{{\"accessToken\":\"tok-swarm-base\",\"refreshToken\":\"ref-swarm-base\",\
         \"expiresAt\":{base_expires_ms},\"scopes\":[\"user:inference\",\"user:profile\"],\
         \"subscriptionType\":\"max\"}}}}\n"
    );
    sys::put(
        &claude_home.join(".credentials.json"),
        0o600,
        &sys::PutSrc::Bytes(base_creds.as_bytes()),
    )
    .map_err(|e| e.to_string())?;
    let base_profile = "{\"oauthAccount\":{\"accountUuid\":\"uuid-swarm-base\",\
                        \"emailAddress\":\"swarm-base@example.test\",\
                        \"organizationType\":\"claude_max\"},\"hasCompletedOnboarding\":true}\n";
    sys::put(
        &claude_home.join(".claude.json"),
        0o600,
        &sys::PutSrc::Bytes(base_profile.as_bytes()),
    )
    .map_err(|e| e.to_string())?;
    atomic_write(&world.home.join(".claude.json"), base_profile.as_bytes())
        .map_err(|e| e.to_string())?;
    atomic_write(
        &claude_home.join("settings.json"),
        b"{\"env\":{\"CLAUDE_CODE_DISABLE_NONESSENTIAL_TRAFFIC\":\"1\"}}\n",
    )
    .map_err(|e| e.to_string())?;

    // Unenrolled dedicated row: `swarm-base` holds the machine's default `$HOME/.claude` login so
    // `fleet-accounts.toml` (`opt_in = "named"`) keeps `<label>` on its own dedicated `dir` (not
    // created on disk yet, so `login_made` is false) until mailbox sign-in enrolls it.
    let initial_login_dir = store
        .login_dir(&label)
        .ok_or_else(|| Fail::die(format!("{label} has no login_dir")))?;
    let fleet_toml = format!(
        "opt_in = \"named\"\n\n\
         [[account]]\nlabel = \"swarm-base\"\nmachines = [\"m15\"]\nclass = \"dedicated\"\n\
         email = \"swarm-base@example.test\"\n\n\
         [[account]]\nlabel = \"{}\"\nmachines = [\"m15\"]\nclass = \"dedicated\"\n\
         email = \"{}\"\n",
        escape(&label),
        escape(&email),
    );
    for fleet_path in [
        secrets_dir.join("fleet-accounts.toml"),
        world.cfg.join("aterm/fleet-accounts.toml"),
    ] {
        sys::put(
            &fleet_path,
            0o600,
            &sys::PutSrc::Bytes(fleet_toml.as_bytes()),
        )
        .map_err(|e| e.to_string())?;
    }
    let roster_toml = format!(
        "[[account]]\nlabel = \"swarm-base\"\nclass = \"dedicated\"\n\
         email = \"swarm-base@example.test\"\n\n\
         [[account]]\nlabel = \"{}\"\nclass = \"dedicated\"\ndir = \"{}\"\nemail = \"{}\"\n",
        escape(&label),
        escape(&initial_login_dir.to_string_lossy()),
        escape(&email),
    );
    let roster_path = world.cfg.join("aterm/accounts.toml");
    atomic_write(&roster_path, roster_toml.as_bytes()).map_err(|e| e.to_string())?;

    let tap_exe = if tap::exe_admissible(&aterm) {
        aterm.clone()
    } else {
        let link = world.dir.join("aterm-tap");
        std::os::unix::fs::symlink(&aterm, &link).map_err(|e| e.to_string())?;
        link
    };
    let tap_settings = tap::settings_value(&tap_exe)
        .ok_or_else(|| Fail::die("could not build tap --settings value"))?;

    let work_dir = world.dir.join("work");
    std::fs::create_dir_all(&work_dir).map_err(|e| e.to_string())?;
    let ctl_dir = world.dir.join("swarm-mailbox");
    std::fs::create_dir_all(&ctl_dir).map_err(|e| e.to_string())?;

    let local_bin = world.home.join(".local/bin");
    std::fs::create_dir_all(&local_bin).map_err(|e| e.to_string())?;
    let agents_dir = atpkg::store::default_prefix(&world.home).join("agents");
    std::fs::create_dir_all(&agents_dir).map_err(|e| e.to_string())?;
    let claude_bin = if let Some(real_claude) = host_claude {
        let link_local = local_bin.join("claude");
        std::os::unix::fs::symlink(&real_claude, &link_local).map_err(|e| e.to_string())?;
        let link_agents = agents_dir.join("claude");
        std::os::unix::fs::symlink(&real_claude, &link_agents).map_err(|e| e.to_string())?;
        link_agents
    } else {
        let lab_exe = std::env::current_exe().map_err(|e| e.to_string())?;
        let standin_bin = world.dir.join("standin-bin");
        std::fs::create_dir_all(&standin_bin).map_err(|e| e.to_string())?;
        let standin_dash = standin_bin.join("dash");
        std::os::unix::fs::symlink(&lab_exe, &standin_dash).map_err(|e| e.to_string())?;
        let shebang = format!("#!{}\n", standin_dash.display());
        let local_claude = local_bin.join("claude");
        sys::put(
            &local_claude,
            0o755,
            &sys::PutSrc::Bytes(shebang.as_bytes()),
        )
        .map_err(|e| format!("write {}: {e}", local_claude.display()))?;
        let agents_claude = agents_dir.join("claude");
        sys::put(
            &agents_claude,
            0o755,
            &sys::PutSrc::Bytes(shebang.as_bytes()),
        )
        .map_err(|e| format!("write {}: {e}", agents_claude.display()))?;
        let marker = J::obj([
            ("control_dir", J::s(ctl_dir.to_string_lossy().into_owned())),
            ("store_dir", J::s(store.dir.to_string_lossy().into_owned())),
            ("expected_otp", J::s(EXPECTED_OTP)),
            ("login_a", J::s("__none__")),
        ]);
        atomic_write(
            &world.home.join(STANDIN_MARKER),
            marker.dump(COMPACT).as_bytes(),
        )
        .map_err(|e| e.to_string())?;
        agents_claude
    };

    let mut env = world.env();
    env.push(("ATERM_LOG".into(), "info".into()));
    env.push((aterm_types::domain::ENV_DEV_FAST_TIMERS.into(), "1".into()));
    env.push((UPKEEP_SEAM.into(), "1".into()));
    let mut inst = Instance::start(
        &world,
        Launch {
            aterm: aterm.clone(),
            headless: true,
            args: vec![
                "--lines".into(),
                "36".into(),
                "--columns".into(),
                "160".into(),
                "-e".into(),
                "/bin/sh".into(),
            ],
            env,
            cwd: work_dir.clone(),
            sandbox: None,
            reroute: false,
        },
    )?;
    inst.await_socket(HANG)?;

    // Phase 1 (Negative control): mailbox config withheld -> upkeep records `no-route` and leaves
    // the row unenrolled.
    let t_withheld = Instant::now();
    let _ = atomic_write(&store.wake_path(), format!("{}\n", now_ms()).as_bytes());
    let saw_no_route = crate::wait_until(HANG, || {
        store
            .load_logins(now_secs())
            .ok()
            .is_some_and(|(logins, _)| {
                logins
                    .get(&label)
                    .and_then(|b| b.sign_in_attempt.as_ref())
                    .is_some_and(|a| a.outcome == AttemptOutcome::NoRoute)
            })
    });
    let withheld_ms = t_withheld.elapsed().as_millis();
    let withheld_outcome = store.load_logins(now_secs()).ok().and_then(|(logins, _)| {
        logins
            .get(&label)
            .and_then(|b| b.sign_in_attempt.as_ref())
            .map(|a| a.outcome.as_str().to_owned())
    });
    let enrolled_while_withheld = store
        .read_bound_roster(&roster_path, now_secs())
        .ok()
        .is_some_and(|r| accounts::enrolled(&r).contains(&label));

    // Phase 2 (Positive proof): supply `ai-mailbox.toml`, wake upkeep, and measure sign-in time
    // until the row is enrolled.
    if let Some(raw) = &live_mailbox_bytes {
        let secrets = world.home.join(".secrets");
        std::fs::create_dir_all(&secrets).map_err(|e| e.to_string())?;
        std::fs::set_permissions(&secrets, std::fs::Permissions::from_mode(0o700))
            .map_err(|e| e.to_string())?;
        sys::put(
            &secrets.join(SECRETS_MAILBOX_TOML_FILE),
            0o600,
            &sys::PutSrc::Bytes(raw),
        )
        .map_err(|e| e.to_string())?;
    } else {
        let maildir = world.dir.join("maildir");
        write_verification_email(&maildir, &email, EXPECTED_OTP)?;
        write_hermetic_ai_mailbox_toml(&world.home, &maildir, &email)?;
    }

    let signin_wait = if opts.live {
        Duration::from_secs(300)
    } else {
        HANG
    };
    let t_signin = Instant::now();
    let _ = atomic_write(&store.wake_path(), format!("{}\n", now_ms()).as_bytes());
    let enrolled_ok = crate::wait_until(signin_wait, || {
        store
            .read_bound_roster(&roster_path, now_secs())
            .ok()
            .is_some_and(|r| accounts::enrolled(&r).contains(&label))
    });
    let signin_ms = t_signin.elapsed().as_millis();

    let (logins_after, _) = store
        .load_logins(now_secs())
        .map_err(|e| format!("load logins after sign-in: {e}"))?;
    let signin_outcome = logins_after
        .get(&label)
        .and_then(|b| b.sign_in_attempt.as_ref())
        .map(|a| a.outcome.as_str().to_owned());
    let signin_route = logins_after
        .get(&label)
        .and_then(|b| b.sign_in_attempt.as_ref())
        .map(|a| a.routes.clone());
    let bound = store
        .read_bound_roster(&roster_path, now_secs())
        .map_err(|e| format!("read bound roster: {e:?}"))?;
    let enrolled_after = accounts::enrolled(&bound);
    let bound_dir = bound.row(&label).and_then(|r| r.dir.clone());

    // Phase 3: launch Claude on the newly bound login directory, serve a turn, and verify both
    // `accounts/state.json` (`auth == Ok`, `ran_at.is_some()`) and `accounts/logins.json`
    // (`served_at.is_some()`) record the served turn on `<label>`.
    let t_turn = Instant::now();
    let mut turns_served = 0usize;
    if enrolled_ok && let Some(dir_str) = bound_dir.as_deref() {
        let sessions_out = ctl_ok(&inst, &["sessions"])?;
        let sid = sessions_out
            .lines()
            .find(|l| l.starts_with(|c: char| c.is_ascii_digit()))
            .and_then(|l| l.split_whitespace().nth(1))
            .ok_or_else(|| Fail::die(format!("no initial session in: {sessions_out}")))?
            .to_owned();
        let sel = format!("@{sid}");
        ctl_ok(&inst, &[&sel, "send", "/bin/bash --norc --noprofile -i"])?;
        ctl_ok(&inst, &[&sel, "key", "enter"])?;
        if !crate::wait_until(HANG, || {
            inst.ctl(&[&sel, "status"]).stdout.contains("program=bash")
        }) {
            inst.stop();
            world.keep();
            return Err(Fail::die("tab did not enter interactive bash"));
        }
        ctl_ok(&inst, &[&sel, "meta", "set", "autopilot", "cruise"])?;
        ctl_ok(&inst, &[&sel, "task", "mark", "working"])?;

        let launch_cmd = format!(
            "cd {} && {LOGIN_ENV}={} {} --settings {}",
            shell_quote(&work_dir.to_string_lossy()),
            shell_quote(dir_str),
            shell_quote(&claude_bin.to_string_lossy()),
            shell_quote(&tap_settings),
        );
        ctl_ok(&inst, &[&sel, "send", &launch_cmd])?;
        ctl_ok(&inst, &[&sel, "key", "enter"])?;

        if crate::wait_until(HANG, || {
            let s = inst.ctl(&[&sel, "status"]).stdout;
            field(&s, "program").as_deref() == Some("claude")
                && field(&s, "supervisor")
                    .as_deref()
                    .is_some_and(|v| v.starts_with("aterm-harness"))
        }) {
            if !opts.live {
                let _ = crate::wait_until(Duration::from_secs(5), || {
                    store
                        .load(now_secs())
                        .ok()
                        .is_some_and(|(st, _)| st.accounts.contains_key(&label))
                });
            }
            let mut last_send: Option<Instant> = None;
            let mut sent = 0usize;
            let _ = crate::wait_until(signin_wait, || {
                let due =
                    last_send.is_none_or(|t| sent < 2 && t.elapsed() >= Duration::from_secs(3));
                if due {
                    let _ = inst.ctl(&[
                        &sel,
                        "send",
                        "Write one sentence about lighthouses, then stop.",
                    ]);
                    let _ = inst.ctl(&[&sel, "key", "enter"]);
                    sent += 1;
                    last_send = Some(Instant::now());
                }
                let turn_done = if opts.live {
                    sent >= 1
                        && scratch_text(&world.state().join("logs/aterm.log"))
                            .lines()
                            .filter(|l| l.contains(&sid) && l.contains("EVENT idle"))
                            .count()
                            >= 2
                } else {
                    scratch_text(&ctl_dir.join("turns-b.log"))
                        .lines()
                        .next()
                        .is_some()
                };
                let ran = store.load(now_secs()).ok().is_some_and(|(st, _)| {
                    st.account(&label).auth == accounts::Auth::Ok
                        && st.account(&label).ran_at.is_some()
                });
                let served = store
                    .load_logins(now_secs())
                    .ok()
                    .is_some_and(|(lg, _)| lg.get(&label).and_then(|b| b.served_at).is_some());
                turn_done && ran && served
            });
        }
        turns_served = if opts.live {
            scratch_text(&world.state().join("logs/aterm.log"))
                .lines()
                .filter(|l| l.contains(&sid) && l.contains("EVENT idle"))
                .count()
                .saturating_sub(1)
        } else {
            scratch_text(&ctl_dir.join("turns-b.log")).lines().count()
        };
    }
    let turn_ms = t_turn.elapsed().as_millis();

    let (final_state, _) = store
        .load(now_secs())
        .map_err(|e| format!("load final accounts state: {e}"))?;
    let (final_logins, _) = store
        .load_logins(now_secs())
        .map_err(|e| format!("load final logins state: {e}"))?;
    let account_ran = final_state.account(&label).auth == accounts::Auth::Ok
        && final_state.account(&label).ran_at.is_some();
    let login_served = final_logins.get(&label).and_then(|b| b.served_at).is_some();
    let ev = MailboxEvidence {
        withheld_outcome,
        enrolled_while_withheld,
        signin_outcome,
        signin_route,
        enrolled_after,
        bound_dir,
        turns_served,
        account_ran,
        login_served,
        withheld_ms,
        signin_ms,
        turn_ms,
    };

    // Verify negative control on the judge itself: withheld-only state must fail.
    let withheld_only = MailboxEvidence {
        signin_outcome: ev.withheld_outcome.clone(),
        signin_route: None,
        enrolled_after: BTreeSet::new(),
        bound_dir: None,
        turns_served: 0,
        account_ran: false,
        login_served: false,
        ..ev.clone()
    };
    if judge_mailbox_evidence(&withheld_only, &label).is_empty() {
        inst.stop();
        return Err(Fail::die(
            "negative control broken: judge accepted withheld-only evidence",
        ));
    }

    let failures = judge_mailbox_evidence(&ev, &label);
    inst.stop();

    let mode_word = if opts.live { "live" } else { "hermetic" };
    println!("swarm mailbox-signin ({mode_word}):");
    println!(
        "  withheld_control: {:?} ({} ms, enrolled_while_withheld={})",
        ev.withheld_outcome, ev.withheld_ms, ev.enrolled_while_withheld
    );
    println!(
        "  signin: {:?} route={:?} ({} ms)  enrolled={:?}",
        ev.signin_outcome, ev.signin_route, ev.signin_ms, ev.enrolled_after
    );
    println!(
        "  turn: turns_served={} account_ran={} login_served={} ({} ms)",
        ev.turns_served, ev.account_ran, ev.login_served, ev.turn_ms
    );

    if !saw_no_route || !enrolled_ok || !failures.is_empty() {
        world.keep();
        let host_log = scratch_text(&world.state().join("logs/aterm.log"));
        let tail: Vec<&str> = host_log
            .lines()
            .filter(|l| l.contains("harness"))
            .rev()
            .take(25)
            .collect();
        for f in &failures {
            println!("  FAIL {f}");
        }
        eprintln!(
            "aterm-lab: swarm mailbox-signin harness log tail:\n{}",
            tail.into_iter().rev().collect::<Vec<_>>().join("\n")
        );
        return Ok(1);
    }

    if opts.record {
        let report = write_measured_report(mode_word, &label, &ev)?;
        println!("  recorded: {}", report.display());
    }

    println!("swarm mailbox-signin: PASS");
    Ok(0)
}

#[cfg(test)]
mod tests {
    use super::*;
    use aterm_agent::harness::align::Captured;

    #[test]
    fn standin_auth_login_enrolls_via_mailbox_and_refuses_when_mailbox_withheld() {
        let tmp = aterm_tempfile::tempdir().expect("tempdir");
        let root = std::fs::canonicalize(tmp.path()).expect("canonical tempdir");
        let home = root.join("home");
        let staging = root.join("staging");
        let ctl = root.join("ctl");
        let store_dir = root.join("store");
        std::fs::create_dir_all(home.join(".claude")).expect("claude dir");
        let marker = J::obj([
            ("control_dir", J::s(ctl.to_string_lossy().into_owned())),
            ("store_dir", J::s(store_dir.to_string_lossy().into_owned())),
            ("expected_otp", J::s(EXPECTED_OTP)),
        ]);
        atomic_write(&home.join(STANDIN_MARKER), marker.dump(COMPACT).as_bytes()).expect("marker");

        let login_args = vec![
            "login".to_owned(),
            "--claudeai".to_owned(),
            "--email".to_owned(),
            EMAIL_MBX.to_owned(),
        ];
        let status_args = vec!["status".to_owned(), "--json".to_owned()];

        // Negative control 1: mailbox config withheld -> login exits 1 and status exits 1.
        let (code, _) = run_standin_auth(&login_args, &home, Some(&staging));
        assert_eq!(
            code, 1,
            "login must refuse when ai-mailbox.toml is withheld"
        );
        assert!(!staging.join(".credentials.json").exists());
        let (st_code, st_out) = run_standin_auth(&status_args, &home, Some(&staging));
        assert_eq!(st_code, 1);
        assert!(st_out.contains("\"loggedIn\":false"), "{st_out}");

        // Negative control 2: ai-mailbox.toml configured, but email addressed to a different user.
        let maildir = root.join("maildir");
        write_verification_email(&maildir, "someone-else@example.test", EXPECTED_OTP)
            .expect("wrong recipient mail");
        write_hermetic_ai_mailbox_toml(&home, &maildir, EMAIL_MBX).expect("mailbox toml");
        let (wrong_code, _) = run_standin_auth(&login_args, &home, Some(&staging));
        assert_eq!(
            wrong_code, 1,
            "login must refuse when maildir holds no OTP for target email"
        );

        // Positive: verification email addressed to EMAIL_MBX with EXPECTED_OTP -> enrolls and
        // `claude auth status --json` satisfies `cli::status_facts(...).renews()`.
        write_verification_email(&maildir, EMAIL_MBX, EXPECTED_OTP).expect("target mail");
        let (ok_code, _) = run_standin_auth(&login_args, &home, Some(&staging));
        assert_eq!(
            ok_code, 0,
            "login must succeed once mailbox holds target OTP"
        );
        let (ok_st_code, ok_st_out) = run_standin_auth(&status_args, &home, Some(&staging));
        assert_eq!(ok_st_code, 0);
        let captured = Captured {
            code: Some(ok_st_code),
            stdout: ok_st_out.into_bytes(),
        };
        let facts = aterm_agent::harness::cli::status_facts(&captured, EMAIL_MBX)
            .expect("status JSON must parse");
        assert!(
            facts.renews(),
            "status facts must qualify row renewal: {facts:?}"
        );
    }

    #[test]
    fn mailbox_judge_accepts_enrolled_served_run_and_rejects_controls() {
        let good = MailboxEvidence {
            withheld_outcome: Some(AttemptOutcome::NoRoute.as_str().into()),
            enrolled_while_withheld: false,
            signin_outcome: Some(AttemptOutcome::SignedIn.as_str().into()),
            signin_route: Some("mailbox".into()),
            enrolled_after: BTreeSet::from([LABEL_MBX.to_owned()]),
            bound_dir: Some("/tmp/logins/swarm-mbx+1".into()),
            turns_served: 1,
            account_ran: true,
            login_served: true,
            withheld_ms: 45,
            signin_ms: 120,
            turn_ms: 1500,
        };
        assert_eq!(
            judge_mailbox_evidence(&good, LABEL_MBX),
            Vec::<String>::new()
        );

        let missing_no_route = MailboxEvidence {
            withheld_outcome: None,
            ..good.clone()
        };
        assert!(
            judge_mailbox_evidence(&missing_no_route, LABEL_MBX)
                .iter()
                .any(|m| m.contains("withheld mailbox negative control"))
        );

        let not_served = MailboxEvidence {
            account_ran: false,
            login_served: false,
            turns_served: 0,
            ..good
        };
        let bad = judge_mailbox_evidence(&not_served, LABEL_MBX);
        assert!(bad.iter().any(|m| m.contains("served 0 turns")));
        assert!(bad.iter().any(|m| m.contains("accounts/state.json")));
        assert!(bad.iter().any(|m| m.contains("accounts/logins.json")));
    }

    #[test]
    fn mailbox_cli_parser_handles_flags_and_rejects_unknown_args() {
        let s = |v: &[&str]| v.iter().map(|x| (*x).to_owned()).collect::<Vec<_>>();
        assert_eq!(
            parse_mailbox_args(&s(&[])).unwrap(),
            MailboxSigninOpts::default()
        );
        assert_eq!(
            parse_mailbox_args(&s(&[
                "--live",
                "--email",
                "a@example.com",
                "--label",
                "m22-1",
                "--mailbox",
                "/tmp/ai-mailbox.toml",
                "--record",
                "--aterm",
                "/tmp/aterm",
                "--keep"
            ]))
            .unwrap(),
            MailboxSigninOpts {
                live: true,
                email: Some("a@example.com".into()),
                label: Some("m22-1".into()),
                mailbox: Some("/tmp/ai-mailbox.toml".into()),
                record: true,
                aterm: Some("/tmp/aterm".into()),
                keep: true,
            }
        );
        assert!(parse_mailbox_args(&s(&["--help"])).is_err());
        assert!(parse_mailbox_args(&s(&["--bogus"])).is_err());
    }
}
