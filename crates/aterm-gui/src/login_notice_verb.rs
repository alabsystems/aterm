// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! `login-notice [<code> [<label>]|disk [<cause>]|end|back]`.

use aterm_agent::supervise::limit::ResetClock;
use aterm_agent::supervise::policy::exception::Exception;
use aterm_agent::supervise::policy::notice::{self, NoticeText};
use aterm_phase::Program;

use crate::harness_host::{AccountsSaid, HostNote};
use crate::message_reporters::DiskFullCause;

/// Whether this build injects ([`cmd`]): a development build only.
const INJECTION_ENABLED: bool = cfg!(any(debug_assertions, feature = "dev-seams"));

/// The usage line every junk form answers with.
const USAGE: &str = "usage: login-notice [<code> [<label>]|disk [spent|inodes|off]|end|back]";

/// The roster label a notice names when the verb names none.
const LABEL: &str = "main";

/// The reset a usage limit's notice names ([`NoticeText::expect_reset`]),
/// as a vendor's notice says it: read through the policy's own clock, so the
/// verb shows the words the loop would.
const SAMPLE_RESET: &str = "Oct 23 at 9am (America/Los_Angeles)";

/// What the verb asks: the list, a notice to raise, the disk-full alert, the raised ones' end, or the tab's login back.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum Asked {
    List,
    Raise {
        text: &'static NoticeText,
        label: String,
    },
    Disk(DiskFullCause),
    End,
    Back,
}

/// The verb's arguments, refused where this build does not inject.
pub(crate) fn parse(rest: &str, injection: bool) -> Result<Asked, String> {
    let words: Vec<&str> = rest.split_whitespace().collect();
    let asked = match words.as_slice() {
        [] => return Ok(Asked::List),
        ["end"] => Asked::End,
        ["back"] => Asked::Back,
        ["disk"] => Asked::Disk(DiskFullCause::Spent),
        ["disk", cause] => Asked::Disk(match *cause {
            "spent" => DiskFullCause::Spent,
            "inodes" => DiskFullCause::Inodes,
            "off" => DiskFullCause::CleanupOff { fault: false },
            _ => return Err(format!("{USAGE} (no disk cause `{cause}`)")),
        }),
        [code] | [code, _] => {
            let text = notice::ALL
                .iter()
                .find(|n| n.code == *code || n.id == *code)
                .ok_or_else(|| format!("{USAGE} (no notice `{code}`; the bare verb lists them)"))?;
            let label = words.get(1).copied().unwrap_or(LABEL);
            if !aterm_agent::harness::accounts::admissible_label(label) {
                return Err(format!("{USAGE} (`{label}` is no roster label)"));
            }
            Asked::Raise {
                text,
                label: label.to_string(),
            }
        }
        _ => return Err(USAGE.to_string()),
    };
    if !injection {
        return Err(
            "login-notice <code>|disk|end|back needs a development build (dev-seams)".to_string(),
        );
    }
    Ok(asked)
}

/// The bare verb's line: every notice's code and id.
pub(crate) fn list_line() -> String {
    let notices: Vec<String> = notice::ALL
        .iter()
        .map(|n| format!("{}:{}", n.code, n.id))
        .collect();
    format!("OK notices={}\n", notices.join(","))
}

/// The verb's control-thread half: the list here, every other form on the main thread.
pub(crate) fn cmd(
    proxy: &aterm_window::event_loop::EventLoopProxy<crate::Wake>,
    rest: &str,
) -> String {
    let asked = match parse(rest, INJECTION_ENABLED) {
        Ok(Asked::List) => return list_line(),
        Ok(asked) => asked,
        Err(refusal) => return format!("ERR {refusal}\n"),
    };
    match crate::control::control_media::call_main(proxy, |reply| crate::Wake::LoginNotice {
        asked,
        reply,
    }) {
        Ok(Ok(line)) => format!("OK {line}\n"),
        Ok(Err(e)) => format!("ERR {e}\n"),
        Err(e) => format!("ERR {e}\n"),
    }
}

/// The host notes `asked` is, for tab `sid`.
pub(crate) fn notes(asked: &Asked, sid: &str) -> Vec<HostNote> {
    let resume = |standing: Option<Exception>| HostNote::ResumeException {
        sid: sid.to_string(),
        agent: Program::Claude,
        standing,
    };
    let accounts = |signed_out: Vec<String>, wrong: Vec<String>| {
        HostNote::Accounts(AccountsSaid::Listed {
            signed_out,
            credits: Vec::new(),
            window_full: Vec::new(),
            due: Vec::new(),
            none_soon: false,
            wrong,
            no_sign_in: Vec::new(),
            clock: None,
        })
    };
    match asked {
        Asked::List => Vec::new(),
        Asked::End => vec![
            resume(None),
            HostNote::Accounts(AccountsSaid::Nothing),
            HostNote::DiskFullOver,
        ],
        Asked::Disk(cause) => vec![HostNote::DiskFull {
            free: Some(1 << 20),
            freed: 31 << 30,
            hogs: Vec::new(),
            cause: *cause,
            thresholds: aterm_agent::harness::disk::Config::default().thresholds(None),
            asked: None,
        }],
        Asked::Back => vec![HostNote::LoginBack {
            sid: sid.to_string(),
        }],
        Asked::Raise { text, label } if text.id == notice::ACCOUNT_SIGNED_OUT.id => {
            vec![accounts(vec![label.clone()], Vec::new())]
        }
        Asked::Raise { text, label } if text.id == notice::RENEWAL_WRONG_ACCOUNT.id => {
            vec![accounts(Vec::new(), vec![label.clone()])]
        }
        Asked::Raise { text, label } => {
            let why = text.whys.iter().copied().max_by_key(|w| w.chars().count());
            let reset = text
                .expect_reset
                .and_then(|_| ResetClock::of(Some(SAMPLE_RESET)))
                .map(|clock| clock.words());
            vec![resume(Some(Exception::of_notice_reset(
                text,
                text.takes_label().then_some(label.as_str()),
                why,
                reset.as_deref(),
                "the agent working",
            )))]
        }
    }
}

impl crate::App {
    /// `login-notice`'s main-thread half.
    pub(crate) fn note_login_notice(&mut self, asked: &Asked) -> Result<String, String> {
        let sid = self
            .first_tab_sid()
            .ok_or_else(|| "the first tab holds no session".to_string())?;
        for note in notes(asked, &sid) {
            self.note_harness_host(note);
        }
        let word = match asked {
            Asked::Raise { text, .. } => text.id,
            Asked::Disk(_) => "disk-full",
            Asked::Back => "back",
            Asked::End | Asked::List => "end",
        };
        Ok(format!("sid={sid} notice={word}"))
    }

    /// The session id of the first window's first tab.
    fn first_tab_sid(&self) -> Option<String> {
        let store = self.store.read().unwrap_or_else(|p| p.into_inner());
        let (_, ws) = self.windows.iter().next()?;
        let tab = ws.tab_set.tabs().first()?;
        let mut sid = None;
        tab.root.any_leaf(&mut |view| {
            sid = self
                .view_store
                .get(*view)
                .copied()
                .and_then(crate::tab_model::View::terminal_session)
                .and_then(|local| store.by_local(local))
                .map(|h| h.sid.as_str().to_string());
            sid.is_some()
        });
        sid
    }
}

#[cfg(test)]
#[path = "login_notice_verb_tests.rs"]
mod tests;
