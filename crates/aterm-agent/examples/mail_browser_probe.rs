// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0
#[cfg(unix)]
use aterm_mail::Mailbox;

#[cfg(unix)]
fn main() -> Result<(), String> {
    aterm_uds::secret::no_core_dumps();
    let email = std::env::args().nth(1).ok_or("Pass the mailbox email")?;
    let home = std::env::var_os("HOME")
        .map(std::path::PathBuf::from)
        .ok_or("No home directory")?;
    if std::env::args().nth(2).as_deref() == Some("--seed-home") {
        let target = std::env::args()
            .nth(3)
            .ok_or("Pass the private instance home")?;
        return seed_home(&home, std::path::Path::new(&target), &email);
    }
    if std::env::args().any(|arg| arg == "--auth-poll") {
        let mut config =
            aterm_agent::harness::headless_oauth::discover_mailbox_config(Some(&home), None)
                .ok_or("No mailbox configuration")?;
        let login = config
            .webmail_user_for(Some(&email))
            .ok_or("No mailbox login")?;
        let password = config
            .password_for(Some(&email))
            .ok_or("No mailbox password")?;
        let mailbox_password = config.mailbox_password_for(Some(&email));
        let totp_secret = config.totp_secret_for(Some(&email));
        let shared = aterm_agent::harness::mail_browser::shared_mailbox(
            &home,
            aterm_agent::harness::mail_browser::Credentials {
                email: &email,
                login: &login,
                password: &password,
                mailbox_password: mailbox_password.as_deref(),
                totp_secret: totp_secret.as_deref(),
            },
        )
        .map_err(|error| error.to_string())?;
        let messages = shared
            .lock()
            .map_err(|_| "Mail lock failed")?
            .messages("0", 1)?;
        if let Some(message) = messages.first() {
            let (source, _) = shared
                .lock()
                .map_err(|_| "Mail lock failed")?
                .read_source(&message.id)?;
            if source.text.is_empty() {
                return Err("No message source".into());
            }
        }
        config.senders = vec!["aterm-native-probe.invalid".into()];
        let result = aterm_agent::harness::headless_oauth::poll_mailbox_config_for_credential(
            &config,
            Some(&email),
            std::time::Duration::from_secs(3),
        );
        match result {
            Err("timed out waiting for verification email in mailbox") => {
                println!("Native authentication reader connected and polled successfully");
                return Ok(());
            }
            Err(error) => return Err(error.into()),
            Ok(_) => return Err("Unexpected credential from excluded sender".into()),
        }
    }
    let started = std::time::Instant::now();
    let mailbox = aterm_agent::harness::mail_browser::HeadlessMail::connect_with_progress(
        &home,
        &email,
        |stage| println!("{stage}"),
    )?;
    println!("Connect: {} ms", started.elapsed().as_millis());
    let started = std::time::Instant::now();
    let mut api = mailbox.into_mailbox()?;
    println!(
        "Native API mailbox unlocked: {} ms",
        started.elapsed().as_millis()
    );
    let started = std::time::Instant::now();
    let list = api.messages("0", 20)?;
    println!(
        "Native API inbox: {} messages, {} ms",
        list.len(),
        started.elapsed().as_millis()
    );
    let mut reads = Vec::new();
    let mut cached_reads = Vec::new();
    for message in &list {
        let started = std::time::Instant::now();
        let read = api.read("0", &message.id)?;
        reads.push(started.elapsed().as_micros());
        let cached = std::time::Instant::now();
        let again = api.read("0", &message.id)?;
        assert_eq!(again.text, read.text);
        cached_reads.push(cached.elapsed().as_micros());
    }
    reads.sort_unstable();
    cached_reads.sort_unstable();
    if !reads.is_empty() {
        let percentile = |values: &[u128], percent: usize| {
            values[((values.len() * percent).div_ceil(100)).saturating_sub(1)]
        };
        println!(
            "Read samples: n={}, p50={} us, p95={} us; cached p50={} us, p95={} us",
            reads.len(),
            percentile(&reads, 50),
            percentile(&reads, 95),
            percentile(&cached_reads, 50),
            percentile(&cached_reads, 95)
        );
    }
    if std::env::args().any(|argument| argument == "--draft-roundtrip") {
        let draft = aterm_mail::Draft {
            from: email.clone(),
            to: vec![email.clone()],
            subject: "aterm encrypted draft validation".into(),
            text: "Local mail-client validation. This draft is not sent and will be removed."
                .into(),
            ..Default::default()
        };
        let prepared = api.prepare_send(&draft)?;
        drop(prepared);
        println!("Outbound recipient encryption prepared");
        let id = api.save_draft(&draft)?;
        let read = api.read("1", &id);
        let cleanup = api.discard_draft(&id);
        cleanup.map_err(|error| format!("Test draft {id} needs cleanup: {error}"))?;
        if read?.text != draft.text {
            return Err("Encrypted draft did not roundtrip".into());
        }
        if api
            .messages("1", 100)?
            .iter()
            .any(|message| message.id == id)
        {
            return Err("Deleted test draft still appears in Drafts".into());
        }
        println!("Encrypted draft roundtrip succeeded; removal verified in Drafts");
    }
    Ok(())
}

#[cfg(unix)]
fn seed_home(
    source: &std::path::Path,
    target: &std::path::Path,
    email: &str,
) -> Result<(), String> {
    use std::io::Write;
    use std::os::unix::fs::OpenOptionsExt;
    let config = aterm_agent::harness::headless_oauth::discover_mailbox_config(Some(source), None)
        .ok_or("No stored account")?;
    let quote = |value: &str| {
        aterm_json::to_string(value).map_err(|_| "Invalid configuration value".to_owned())
    };
    let login = config.webmail_user_for(Some(email)).ok_or("No login")?;
    let password = config.password_for(Some(email)).ok_or("No password")?;
    let mut body = format!("user = {}\n", quote(&login)?);
    for (key, value) in [
        ("mailbox_password", config.mailbox_password_for(Some(email))),
        ("totp_secret", config.totp_secret_for(Some(email))),
    ] {
        if let Some(value) = value {
            body.push_str(&format!("{key} = {}\n", quote(&value)?));
        }
    }
    body.push_str(&format!(
        "[passwords]\n{} = {}\n",
        quote(email)?,
        quote(&password)?
    ));
    let body = aterm_mail::Secret::new(body);
    let directory = target.join(".config/aterm");
    aterm_types::fs_restricted::ensure_private_dir(&directory).map_err(|e| e.to_string())?;
    let mut file = std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(0o600)
        .open(directory.join("mailbox.toml"))
        .map_err(|e| e.to_string())?;
    file.write_all(body.expose().as_bytes())
        .map_err(|e| e.to_string())?;
    let mut digest = aterm_digest::Sha256::new();
    digest.update(email.trim().to_ascii_lowercase().as_bytes());
    let key: String = digest
        .finalize()
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect();
    let profile_parent = directory.join("mail/browser");
    aterm_types::fs_restricted::ensure_private_dir(&profile_parent).map_err(|e| e.to_string())?;
    let profile = source.join(".config/aterm/mail/browser").join(&key);
    let _lease = aterm_agent::harness::browser_window::ProfileLock::claim(&profile)
        .map_err(|_| "The source mail profile is in use")?;
    copy_profile(&profile, &profile_parent.join(key)).map_err(|e| e.to_string())?;
    if let Some(cache) = aterm_agent::harness::headless_fetch::cache_root(Some(source)) {
        let parent = target.join(".cache/aterm");
        aterm_types::fs_restricted::ensure_private_dir(&parent).map_err(|e| e.to_string())?;
        std::os::unix::fs::symlink(cache, parent.join("browser.noindex"))
            .map_err(|e| e.to_string())?;
    }
    println!("Private mail UI fixture prepared");
    Ok(())
}

#[cfg(unix)]
fn copy_profile(source: &std::path::Path, target: &std::path::Path) -> std::io::Result<()> {
    aterm_types::fs_restricted::ensure_private_dir(target)?;
    for entry in std::fs::read_dir(source)? {
        let entry = entry?;
        let kind = entry.file_type()?;
        let to = target.join(entry.file_name());
        if kind.is_dir() {
            copy_profile(&entry.path(), &to)?;
        } else if kind.is_file() {
            std::fs::copy(entry.path(), to)?;
        }
    }
    Ok(())
}

#[cfg(not(unix))]
fn main() {
    eprintln!("Headless mail is unavailable on this platform");
}
