// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! `login-notice [<code> [<label>]|disk [<cause>]|end|back]`: THE HARNESS'S
//! LOGIN NOTICES ON DEMAND, in a development build (`dev-seams`), so each
//! one's real rendering — the band's row, its expanded sentence, the digest's
//! line, a loss's one row in Settings ▸ Messages as it moves and once the
//! login is back — can be read and imaged in a private headless instance
//! (`targo --unverified lab instance --pre 'login-notice si' --image …`)
//! without a lost login to stand it up. Bare, the verb lists the notices
//! (`aterm_agent::supervise::policy::notice::ALL`). `disk` raises the same
//! story's disk-full alert ([`HostNote::DiskFull`]: 1.0 MiB left, 31.0 GiB
//! freed, no folder named) for `<cause>` (`spent`, the default; `inodes`;
//! `off`), and `end` ends it with the rest.
//!
//! The notice goes the way the harness's own does: an exception built by
//! [`Exception::of_notice_reset`] — its `{why}` the longest clause the policy
//! raises it with ([`NoticeText::whys`]), its label given only where it names
//! one, a usage limit's reset the fixed [`SAMPLE_RESET`] — and handed to the
//! host as the loop hands it
//! ([`HostNote::ResumeException`] for the first window's first tab), or, for the two
//! accounts lines, the roster's word ([`HostNote::Accounts`]); `back` is the
//! loop's word that the tab's lost login is back ([`HostNote::LoginBack`]).
//! Nothing here words a notice: the host's builders do.

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

/// What the verb asks: the list, a notice to raise (with its label), the
/// raised one's end, or the tab's login back.
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

/// The verb's arguments, refused where this build does not inject. Pure.
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
            "login-notice <code>|end|back needs a development build (dev-seams)".to_string(),
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

/// The verb's control-thread half: the list here, a raise or an end on the
/// main thread (`Wake::LoginNotice`).
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

/// The host notes `asked` is, for tab `sid`: an exception the loop would
/// tell (`None` ends it), the roster's word for the two accounts lines, or
/// the loop's word that the login is back.
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
    /// `login-notice`'s main-thread half: `asked` for the first window's
    /// first tab ([`Self::first_tab_sid`]), through
    /// the host's own path ([`crate::App::note_harness_host`]). Answers
    /// `sid=<tab> notice=<id|end|back>`.
    pub(crate) fn note_login_notice(&mut self, asked: &Asked) -> Result<String, String> {
        let sid = self
            .first_tab_sid()
            .ok_or_else(|| "no tab holds a session".to_string())?;
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
mod tests {
    use super::*;

    #[test]
    fn the_verb_lists_raises_and_ends_and_refuses_junk() {
        assert_eq!(parse("", false), Ok(Asked::List));
        assert!(list_line().contains(&format!(
            "{}:{}",
            notice::LOGIN_SIGN_IN.code,
            notice::LOGIN_SIGN_IN.id
        )));
        assert_eq!(
            parse("si", true),
            Ok(Asked::Raise {
                text: &notice::LOGIN_SIGN_IN,
                label: LABEL.to_string()
            })
        );
        assert_eq!(
            parse("login-sign-in-as pool-1", true),
            Ok(Asked::Raise {
                text: &notice::LOGIN_SIGN_IN_AS,
                label: "pool-1".to_string()
            })
        );
        assert_eq!(parse("end", true), Ok(Asked::End));
        assert_eq!(parse("back", true), Ok(Asked::Back));
        assert_eq!(parse("disk", true), Ok(Asked::Disk(DiskFullCause::Spent)));
        assert_eq!(
            parse("disk inodes", true),
            Ok(Asked::Disk(DiskFullCause::Inodes))
        );
        assert!(matches!(
            notes(&Asked::Disk(DiskFullCause::Spent), "s-1").as_slice(),
            [HostNote::DiskFull { .. }]
        ));
        // NEGATIVE CONTROLS: a shipped build injects nothing; junk.
        assert!(parse("si", false).is_err());
        assert!(parse("end", false).is_err());
        assert!(parse("back", false).is_err());
        assert!(parse("disk", false).is_err());
        assert!(parse("disk full", true).is_err());
        assert!(parse("zz", true).is_err());
        assert!(parse("si a b", true).is_err());
        assert!(parse("si bad/label", true).is_err());
    }

    #[test]
    fn each_notice_goes_the_way_the_harness_tells_it() {
        for text in notice::ALL {
            let asked = Asked::Raise {
                text,
                label: LABEL.to_string(),
            };
            let notes = notes(&asked, "s-1");
            assert_eq!(notes.len(), 1, "{}", text.id);
            let accounts = matches!(notes[0], HostNote::Accounts(_));
            assert_eq!(
                accounts,
                [
                    notice::ACCOUNT_SIGNED_OUT.id,
                    notice::RENEWAL_WRONG_ACCOUNT.id
                ]
                .contains(&text.id),
                "{}",
                text.id
            );
            if let HostNote::ResumeException {
                standing: Some(e), ..
            } = &notes[0]
            {
                assert_eq!(e.notice.as_ref().map(|n| n.id), Some(text.id));
                // Its `{why}` is one it is raised with (the review of the
                // login carry, 2026-10-20: every one read "a draft is in the
                // composer"), in what aterm did or in the step (a usage
                // limit aterm.toml holds names the line that lets it go on:
                // r3h-2). NEGATIVE CONTROL: no other notice's clause.
                let did = e.notice.as_ref().map(|n| n.did.clone()).unwrap_or_default();
                assert!(!did.contains('{'), "{}: {did}", text.id);
                assert!(!e.action.contains('{'), "{}: {}", text.id, e.action);
                let did = format!("{did} {}", e.action);
                assert_eq!(
                    text.whys.iter().any(|w| did.contains(w)),
                    !text.whys.is_empty(),
                    "{}: {did}",
                    text.id
                );
                for other in notice::ALL.iter().flat_map(|n| n.whys) {
                    if !text.whys.contains(other) {
                        assert!(!did.contains(other), "{}: {did}", text.id);
                    }
                }
                // A usage limit's notice names the sample's reset, as the
                // loop names the limit's own; NEGATIVE CONTROL: no other
                // notice says one.
                let expect = e
                    .notice
                    .as_ref()
                    .map(|n| n.expect.clone())
                    .unwrap_or_default();
                assert_eq!(
                    expect.contains("on Oct 23 at 9am"),
                    text.expect_reset.is_some(),
                    "{}: {expect}",
                    text.id
                );
            }
        }
    }
}
