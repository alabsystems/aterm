// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! `agent ssh`: the fleet key's forced command (design §3.1, `crate::protocol`).
//!
//! sshd runs it as the admin for every connection made with the fleet key, whatever the
//! client asked for; the client's command arrives in `SSH_ORIGINAL_COMMAND`. It is split on
//! single spaces (never by a shell) and must be exactly one of:
//!
//! * `hello`
//! * `put <sha256>`
//! * `apply` (re-run as root through the one sudoers rule, standard input passed through)
//! * `attach <run>`
//! * `screen` (bytes relayed both ways to screensharingd on the host's loopback, only
//!   while launchd enables Screen Sharing, [`super::relay`])
//!
//! Anything else, or no command at all (an interactive login), is `refused: …`, status 2.

use super::Layout;
use crate::port::{Cmd, System};
use std::io::{Read, Write};
use std::process::ExitCode;

/// The `sudo` the `apply` request becomes: exactly the argument vector the sudoers rule
/// names, `-n` so it never prompts.
#[must_use]
pub fn sudo_apply() -> Cmd {
    Cmd::new("/usr/bin/sudo").args([
        "-n",
        crate::protocol::AGENT_EXE,
        "machines",
        "agent",
        "apply",
    ])
}

/// What the client asked for, split, or why it is refused.
///
/// # Errors
/// The refusal's one line.
pub fn parse(original: Option<&str>) -> Result<Vec<&str>, String> {
    let Some(original) = original else {
        return Err(
            "this key runs only `hello`, `put <sha256>`, `apply`, `attach <run>` and `screen`; \
             it opens no shell"
                .into(),
        );
    };
    if original.bytes().any(|b| b.is_ascii_control()) {
        return Err("the request holds a control character".into());
    }
    let words: Vec<&str> = original.split(' ').collect();
    if words.iter().any(|w| w.is_empty()) {
        return Err(format!(
            "{original:?}: words are separated by exactly one space"
        ));
    }
    Ok(words)
}

/// `agent ssh` with its input explicit.
pub fn ssh(
    sys: &dyn System,
    layout: &Layout,
    original: Option<&str>,
    stdin: &mut dyn Read,
    out: &mut dyn Write,
) -> ExitCode {
    let words = match parse(original) {
        Ok(w) => w,
        Err(why) => return super::refuse(out, &why),
    };
    match words.as_slice() {
        ["hello"] => super::hello::print(sys, layout, out),
        ["put", sha] => super::inbox::put(sys, sha, stdin, out),
        ["attach", run] => super::apply::attach(sys, layout, run, out),
        // The relay reads this process's own standard input, not `stdin`: its reader
        // outlives the request when screensharingd closes first.
        ["screen"] => match super::relay::own_stdin() {
            Ok(input) => super::relay::screen(sys, super::relay::SCREEN_SHARING_PORT, input, out),
            Err(e) => super::refuse(out, &format!("screen: standard input: {e}")),
        },
        ["apply"] => {
            let _ = out.flush();
            // Standard input, output and error are this process's own: the envelope flows
            // from the SSH channel to the root agent, and its stream flows back.
            match sys.run_tty(&sudo_apply()) {
                Ok(Some(code)) => ExitCode::from(u8::try_from(code).unwrap_or(1)),
                Ok(None) => ExitCode::FAILURE,
                Err(e) => super::refuse(out, &format!("apply: sudo did not start: {e}")),
            }
        }
        _ => super::refuse(
            out,
            &format!(
                "{:?} is not a request this key may make (hello, put <sha256>, apply, attach \
                 <run>, screen)",
                original.unwrap_or_default()
            ),
        ),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::port::{Fake, Ran};

    fn call(fake: &Fake, original: Option<&str>) -> (ExitCode, String) {
        let mut out = Vec::new();
        let code = ssh(
            fake,
            &Layout { root: "/A".into() },
            original,
            &mut std::io::empty(),
            &mut out,
        );
        (code, String::from_utf8_lossy(&out).into_owned())
    }

    #[test]
    fn only_the_five_requests_pass_and_everything_else_is_refused_with_status_two() {
        let mut fake = Fake::root();
        fake.euid = 501;
        for bad in [
            None,
            Some(""),
            Some("bash"),
            Some("hello extra"),
            Some("hello  "),
            Some("apply; rm -rf /"),
            Some("put"),
            Some("put x y"),
            Some("attach"),
            Some("install --admin admin"),
            Some("apply\nhello"),
            Some("/bin/sh -c id"),
            Some("scp -t /tmp"),
            Some("internal-sftp"),
            Some("screen 5901"),
            Some("screen "),
            Some("screen\t"),
        ] {
            let (code, out) = call(&fake, bad);
            assert_eq!(code, ExitCode::from(super::super::REFUSED), "{bad:?}");
            assert!(out.starts_with("refused: "), "{bad:?}: {out}");
        }
        // Nothing ran for any of them.
        assert!(fake.calls().is_empty(), "{:?}", fake.calls());
    }

    #[test]
    fn apply_becomes_exactly_the_sudo_the_rule_names() {
        let mut fake = Fake::root();
        fake.euid = 501;
        fake.on(
            "tty /usr/bin/sudo -n /Library/Management/aterm/bin/aterm machines agent apply",
            Ran::exit(1, ""),
        );
        let (code, _) = call(&fake, Some("apply"));
        assert_eq!(code, ExitCode::FAILURE);
        assert_eq!(
            fake.calls(),
            ["tty /usr/bin/sudo -n /Library/Management/aterm/bin/aterm machines agent apply"]
        );
    }

    #[test]
    fn a_bad_digest_or_run_id_is_refused_before_anything_touches_a_path() {
        let mut fake = Fake::root();
        fake.euid = 501;
        let (code, out) = call(&fake, Some("put ../../.ssh/authorized_keys"));
        assert_eq!(code, ExitCode::from(super::super::REFUSED));
        assert!(out.contains("SHA-256"), "{out}");
        let (code, _) = call(&fake, Some("attach ../../../etc"));
        assert_eq!(code, ExitCode::from(super::super::REFUSED));
        assert!(fake.calls().is_empty(), "{:?}", fake.calls());
    }
}
