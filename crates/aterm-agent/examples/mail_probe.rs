// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0
//! Read-only end-to-end Proton probe using aterm's existing mailbox credential routing.

use aterm_agent::harness::headless_oauth;
use aterm_mail::{Mailbox, Secret};
use std::path::PathBuf;

fn verify(challenge: &aterm_mail::human::Challenge) -> Result<aterm_mail::human::Verified, String> {
    let recovery = aterm_mail::human::Recovery::prepare(challenge)?;
    println!("Proton verification purpose: {:?}", recovery.purpose);
    if !recovery.is_email() {
        return Err("No configured unattended SMS recovery route".into());
    }
    let config = aterm_agent::harness::mail_recovery::configured()?;
    if aterm_agent::harness::mail_recovery::candidates(&config, &recovery.destination).is_empty() {
        return Err("No stored mailbox route matches Proton's recovery address".into());
    }
    let since = aterm_time::unix_secs().ok_or("System clock is invalid")?;
    recovery.request_code()?;
    let code = aterm_agent::harness::mail_recovery::code(&config, &recovery.destination, since)?;
    recovery.complete(&Secret::new(code))
}

fn run() -> Result<(), String> {
    aterm_uds::secret::no_core_dumps();
    let mut args = std::env::args().skip(1);
    let first = args.next().ok_or("Pass the account email to test")?;
    let inspect = first == "--inspect";
    let email = if inspect {
        args.next().ok_or("Pass the account email to inspect")?
    } else {
        first
    };
    let home = std::env::var_os("HOME")
        .map(PathBuf::from)
        .ok_or("Home directory unavailable")?;
    let config = headless_oauth::discover_mailbox_config(Some(&home), None)
        .ok_or("No mailbox credentials are configured")?;
    if email == "--inventory" {
        let accounts = aterm_agent::harness::mail_recovery::accounts(&config);
        let gmail = accounts
            .iter()
            .filter(|email| {
                email.rsplit_once('@').is_some_and(|(_, domain)| {
                    domain.eq_ignore_ascii_case("gmail.com")
                        || domain.eq_ignore_ascii_case("googlemail.com")
                })
            })
            .count();
        println!("Stored mail account identities: {}", accounts.len());
        println!("Stored Gmail identities: {gmail}");
        println!("Stored mailbox routes: {}", config.mailboxes.len());
        return Ok(());
    }
    let user = config
        .webmail_user_for(Some(&email))
        .ok_or("No login is configured for this mailbox")?;
    let password = Secret::new(
        config
            .password_for(Some(&email))
            .ok_or("No password is configured for this mailbox")?,
    );
    let from_files = headless_oauth::discover_mailbox_config_files(Some(&home), None, None);
    println!(
        "Selected login matches requested email or local part: {}",
        user.eq_ignore_ascii_case(&email)
            || email
                .split_once('@')
                .is_some_and(|(local, _)| user.eq_ignore_ascii_case(local))
    );
    println!(
        "Selected credentials match on-disk configuration: {}",
        from_files.as_ref().is_some_and(|files| {
            files.webmail_user_for(Some(&email)).as_deref() == Some(user.as_str())
                && files.password_for(Some(&email)).as_deref() == Some(password.expose())
        })
    );
    if inspect {
        return Ok(());
    }
    println!("Starting Proton password authentication.");
    let mut proton = aterm_mail::proton::Proton::sign_in_with_verifier(&user, &password, verify)?;
    println!("Password authentication accepted; checking mailbox access.");
    let outcome: Result<(), String> = proton.with_verification(|proton| {
        if proton.needs_two_factor {
            let secret = Secret::new(
                config
                    .totp_secret_for(Some(&email))
                    .ok_or("This account requires a verification code")?,
            );
            let now = aterm_time::unix_secs().ok_or("System clock is invalid")?;
            let code = Secret::new(
                headless_oauth::generate_totp_at(secret.expose(), now)
                    .ok_or("Invalid verification-code configuration")?,
            );
            proton.two_factor(&code)?;
        }
        println!("Checking account address ownership.");
        if !proton.has_address(&email)? {
            return Err("Signed-in account does not own the requested address".into());
        }
        let mailbox_password = config
            .mailbox_password_for(Some(&email))
            .map(Secret::new)
            .unwrap_or_else(|| password.clone());
        println!("Unlocking mailbox keys locally.");
        proton.unlock_mailbox(&mailbox_password)?;
        let folders = proton.folders()?;
        let messages = proton.messages("0", 5)?;
        if let Some(message) = messages.first() {
            let body = proton.read("0", &message.id)?;
            println!(
                "One message decrypted locally ({} bytes); body not printed.",
                body.text.len()
            );
        }
        println!(
            "Identity verified; {} folders; {} inbox messages fetched. No message content printed and no messages changed.",
            folders.len(),
            messages.len()
        );
        Ok(())
    }, verify);
    let sign_out = proton.sign_out();
    match (outcome, sign_out) {
        (outcome, Ok(())) => {
            println!("Temporary test session signed out.");
            outcome
        }
        (Err(error), Err(cleanup)) => Err(format!(
            "{error}; temporary session revocation also failed: {cleanup}"
        )),
        (Ok(()), Err(cleanup)) => Err(format!("Temporary session revocation failed: {cleanup}")),
    }
}

fn main() -> std::process::ExitCode {
    match run() {
        Ok(()) => std::process::ExitCode::SUCCESS,
        Err(reason) => {
            eprintln!("Mail probe: {reason}");
            std::process::ExitCode::FAILURE
        }
    }
}
