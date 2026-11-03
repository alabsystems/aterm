// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0
//! Explicit headless Gmail enrollment. Reading mail never starts this flow implicitly.

use super::*;
use aterm_mail::oauth::{self, Error, ErrorKind};
use std::path::PathBuf;

struct Settings {
    account: String,
    client_file: PathBuf,
    within: Duration,
}

fn parse(home: &Path, args: &[String]) -> Result<Settings, Failure> {
    let account = args
        .first()
        .filter(|account| valid_account(account))
        .ok_or("A Google mailbox email address is required")?
        .to_ascii_lowercase();
    let mut settings = Settings {
        account,
        client_file: home.join(".config/aterm/mail/google-client.json"),
        within: Duration::from_secs(180),
    };
    let mut seen = std::collections::BTreeSet::new();
    for pair in args[1..].chunks(2) {
        if pair.len() != 2 || !seen.insert(&pair[0]) {
            return Err("Missing or duplicate Gmail connection option".into());
        }
        match pair[0].as_str() {
            "--client-file" if !pair[1].is_empty() && !pair[1].chars().any(char::is_control) => {
                settings.client_file = PathBuf::from(&pair[1])
            }
            "--timeout" => {
                let raw = pair[1].as_str();
                settings.within = Duration::from_secs(
                    (!raw.is_empty()
                        && !raw.starts_with('0')
                        && raw.bytes().all(|b| b.is_ascii_digit()))
                    .then(|| raw.parse::<u64>().ok())
                    .flatten()
                    .filter(|value| (1..=300).contains(value))
                    .ok_or("Gmail connection timeout must be 1..300 seconds")?,
                )
            }
            _ => return Err("Unsupported Gmail connection option".into()),
        }
    }
    Ok(settings)
}

fn failure(error: Error) -> Failure {
    Failure {
        retry: error.retryable(),
        message: error.to_string(),
    }
}

#[cfg(unix)]
fn read_client(path: &Path) -> Result<oauth::enrollment::Client, Failure> {
    let raw = aterm_secrets::fsx::read_private(path, 65536)
        .map_err(|_| {
            Failure::from("Could not read aterm's private Google desktop client registration")
        })?
        .ok_or("Aterm's Google desktop client registration is unavailable")?;
    oauth::enrollment::Client::decode(raw.as_slice()).map_err(failure)
}

#[cfg(not(unix))]
fn read_client(_: &Path) -> Result<oauth::enrollment::Client, Failure> {
    Err("Headless Google mail enrollment is unavailable on this platform".into())
}

pub(super) fn run(home: &Path, args: &[String], output: &mut impl Write) -> Result<(), Failure> {
    let settings = parse(home, args)?;
    connect(home, &settings)?;
    emit(
        output,
        json!({"schema":1,"type":"connected","provider":"gmail","account":settings.account}),
    )
}

pub(super) fn connect_default(home: &Path, account: &str, within: Duration) -> Result<(), Failure> {
    let mut settings = parse(home, &[account.to_owned()])?;
    if within < Duration::from_secs(1) || within > Duration::from_secs(300) {
        return Err("Gmail connection timeout must be 1..300 seconds".into());
    }
    settings.within = within;
    connect(home, &settings)
}

fn connect(home: &Path, settings: &Settings) -> Result<(), Failure> {
    let end = Instant::now() + settings.within;
    let client = read_client(&settings.client_file)?;
    let enrollment =
        oauth::enrollment::Enrollment::bind(client, &settings.account).map_err(failure)?;
    let config = headless_oauth::discover_mailbox_config(Some(home), None)
        .filter(|config| configured_account(config, &settings.account));
    let store = home.join(".config/aterm/mail/google-browser");
    let mut auth_error = None;
    let result = enrollment.run(end, |url, end| {
        headless_oauth::authorize_google_mail(
            url,
            &store,
            config.as_ref(),
            &settings.account,
            end.saturating_duration_since(Instant::now()),
        )
        .map_err(|message| {
            auth_error = Some(message);
            Error {
                kind: ErrorKind::Rejected,
            }
        })
    });
    let (client, tokens) =
        result.map_err(|error| auth_error.map_or_else(|| failure(error), Failure::from))?;
    let refresh = tokens
        .refresh_token
        .ok_or("Google did not authorize offline mail access")?;
    remaining(end)?;
    let mailbox = aterm_mail::imap::Imap {
        endpoint: aterm_mail::Endpoint::parse("imap.gmail.com", 993)?,
        authentication: Authentication::OAuth {
            username: settings.account.clone(),
            access_token: tokens.access_token,
        },
    };
    let mut reader = mailbox
        .open_until("INBOX", end)
        .map_err(|error| match error {
            aterm_mail::imap::ConnectError::Authentication => {
                Failure::from("Google authorization does not permit this mailbox")
            }
            aterm_mail::imap::ConnectError::Other(_) => {
                transient("Could not verify the Google mailbox connection")
            }
        })?;
    reader
        .run_until(end, |reader| reader.messages(1))
        .map_err(|_| transient("Could not verify reading the Google mailbox"))?;
    remaining(end)?;
    oauth::session::register(home, &settings.account, &client.id, client.secret, refresh)?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn enrollment_options_cannot_accept_credentials_or_unbounded_attempts() {
        let args = |values: &[&str]| values.iter().map(|v| (*v).into()).collect::<Vec<String>>();
        let home = Path::new("/home/example");
        let parsed = parse(home, &args(&["Me@gmail.com", "--timeout", "45"]))
            .unwrap_or_else(|_| panic!("valid enrollment options"));
        assert_eq!(parsed.account, "me@gmail.com");
        assert_eq!(parsed.within, Duration::from_secs(45));
        assert_eq!(
            parsed.client_file,
            home.join(".config/aterm/mail/google-client.json")
        );
        for values in [
            vec!["a@b", "--password", "secret"],
            vec!["a@b", "--timeout", "301"],
            vec!["a@b", "--timeout", "0"],
            vec!["a@b", "--timeout", "+45"],
            vec!["a@b", "--timeout", "045"],
            vec!["a@b", "--client-file"],
            vec!["a@b", "--client-file", "bad\0path"],
            vec!["a@b", "--timeout", "1", "--timeout", "2"],
            vec!["a@b@c"],
        ] {
            assert!(parse(home, &args(&values)).is_err());
        }
    }
}
