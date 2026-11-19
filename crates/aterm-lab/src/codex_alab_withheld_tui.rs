// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Deliver the private provider challenge to the packaged terminal's own composer.

use std::path::Path;
use std::sync::Mutex;
use std::time::{Duration, Instant};

use aterm_agent::harness::native_act::Process;

use super::{ApiLog, HANG, inconclusive, published_target};
use crate::codex_rig::SourceResponses;
use crate::instance::{Instance, Launch, World};
use crate::{Fail, Res, stop};

#[path = "codex_alab_tui_turn.rs"]
mod turn;

fn denied_turn(
    observed: Option<turn::Turn>,
    receipts: &SourceResponses,
) -> Result<Option<(String, usize)>, Fail> {
    if receipts.accepted != 0 {
        return Err(Fail::die(
            "FAIL: the private provider accepted a request while interactive Source was withheld",
        ));
    }
    let Some(turn) = observed else {
        return Ok(None);
    };
    if turn.completed {
        return Err(Fail::die(
            "FAIL: the signed TUI answered its challenge while Source was withheld",
        ));
    }
    Ok(
        (turn.denied && receipts.refused > 0 && receipts.refused == receipts.written)
            .then_some((turn.thread, receipts.written)),
    )
}

type Wire = aterm_agent::RelayClient<aterm_uds::CtlStream>;

/// The private host reads the selected session's actual PTY master for this group.
fn foreground(
    wire: &mut Wire,
    sid: &str,
    payload: &published_target::Payload,
) -> Result<Option<Process>, Fail> {
    let (header, who) = wire.request_counted("who").map_err(inconclusive)?;
    if !header.starts_with("OK ") {
        return Err(inconclusive(
            "the private host refused foreground discovery",
        ));
    }
    Ok(crate::harness_proof_judge::fgpgid(&who, sid)
        .and_then(|pid| i32::try_from(pid).ok())
        .and_then(|pid| payload.observe_process(pid)))
}

fn wait<T>(why: &str, mut look: impl FnMut() -> Result<Option<T>, Fail>) -> Result<T, Fail> {
    let until = Instant::now() + HANG;
    loop {
        stop::check()?;
        if let Some(found) = look()? {
            return Ok(found);
        }
        if Instant::now() >= until {
            return Err(inconclusive(why));
        }
        stop::sleep(Duration::from_millis(100))?;
    }
}

struct Terminal<'a> {
    instance: &'a Instance,
    sid: &'a str,
    wire: Wire,
    payload: &'a published_target::Payload,
    process: Process,
}

impl Terminal<'_> {
    fn check(&mut self) -> Result<(), Fail> {
        if foreground(&mut self.wire, self.sid, self.payload)?.as_ref() == Some(&self.process) {
            return Ok(());
        }
        Err(inconclusive(
            "the original signed TUI left its foreground run",
        ))
    }

    fn input(&mut self, verb: &str, arg: &str) -> Result<(), Fail> {
        self.check()?;
        let reply = self.instance.ctl(&[&format!("@{}", self.sid), verb, arg]);
        if reply.code != 0 {
            return Err(inconclusive(format!("the private TUI refused {verb}")));
        }
        Ok(())
    }

    fn screen(&mut self) -> Result<String, Fail> {
        self.check()?;
        let (reply, text) = self
            .wire
            .request_counted(&format!("@{} text", self.sid))
            .map_err(inconclusive)?;
        if !reply.starts_with("OK ") {
            return Err(inconclusive("the private host refused its TUI screen"));
        }
        Ok(text)
    }

    /// Issue the challenge through the ordinary TUI, with no judgment about the provider's answer.
    fn submit(&mut self, prompt: &str, log: &Mutex<ApiLog>) -> Result<(), Fail> {
        wait(
            "the signed interactive terminal showed neither login nor a composer",
            || {
                let text = self.screen()?;
                if turn::login_screen(&text) {
                    let (requests, _) = log
                        .lock()
                        .unwrap_or_else(std::sync::PoisonError::into_inner)
                        .snapshot()
                        .map_err(inconclusive)?;
                    let source_requests = requests
                        .iter()
                        .filter(|r| r.get("source_auth").is_some())
                        .count();
                    if requests.iter().any(|r| {
                        r.get("source_auth").and_then(crate::pyjson::J::as_str) == Some("accepted")
                    }) {
                        return Err(Fail::die(
                            "FAIL: the private provider accepted a request before interactive input",
                        ));
                    }
                    self.check()?;
                    return Err(inconclusive(format!(
                        "interactive TUI reached sign-in before its composer: phase=login prompt_submitted=false thread=unobserved responses_requests={source_requests} correlated_401=unobserved"
                    )));
                }
                Ok(turn::composer_ready(&text).then_some(()))
            },
        )?;
        self.input("send", prompt)?;
        wait(
            "the private TUI did not display the exact challenge in its composer",
            || {
                Ok(
                    (crate::screen::composer_row("codex", &self.screen()?).as_deref()
                        == Some(prompt))
                    .then_some(()),
                )
            },
        )?;
        self.input("key", "enter")
    }

    fn withheld_result(
        &mut self,
        world: &World,
        prompt: &str,
        log: &Mutex<ApiLog>,
    ) -> Result<(String, usize), Fail> {
        let home = world.dir.join("codex-home");
        let (thread, writes) = wait(
            "no failed original TUI turn and completed provider 401 could be joined to the submitted prompt",
            || {
                self.check()?;
                let observed = turn::rollout(&home, prompt).map_err(inconclusive)?;
                let thread = observed.as_ref().map_or("", |t| t.thread.as_str());
                let receipts = log
                    .lock()
                    .unwrap_or_else(std::sync::PoisonError::into_inner)
                    .source_responses(thread, prompt)
                    .map_err(inconclusive)?;
                denied_turn(observed, &receipts)
            },
        )?;
        self.check()?;
        Ok((thread, writes))
    }
}

pub(super) fn run(
    world: &World,
    aterm: &Path,
    exe: &Path,
    payload: &published_target::Payload,
    jail: String,
    prompt: &str,
    log: &Mutex<ApiLog>,
) -> Res {
    // -e owns the only terminal in this headless instance. Set CODEX_HOME in the
    // executed child because aterm strips parent-agent variables when opening a PTY.
    let mut instance = Instance::start(
        world,
        Launch {
            aterm: aterm.to_path_buf(),
            headless: true,
            args: vec![
                "--lines".into(),
                "40".into(),
                "--columns".into(),
                "140".into(),
                "-e".into(),
                "/usr/bin/env".into(),
                format!("CODEX_HOME={}", world.dir.join("codex-home").display()),
                exe.to_string_lossy().into_owned(),
            ],
            env: world.env(),
            cwd: world.dir.join("work"),
            sandbox: Some(jail),
            reroute: false,
        },
    )
    .map_err(inconclusive)?;
    let mut observed_run = None;
    let result = (|| {
        instance.await_socket(crate::instance::BOOT)?;
        let sessions = instance.ctl(&["sessions"]);
        let mut ids = sessions.stdout.lines().filter_map(|row| {
            let mut words = row.split_whitespace();
            words.next()?.parse::<u64>().ok()?;
            let sid = words.next()?;
            (crate::stub::first_sid(sid) == Some(sid)).then_some(sid)
        });
        let sid = ids
            .next()
            .filter(|_| sessions.code == 0 && ids.next().is_none())
            .ok_or_else(|| {
                inconclusive("the private host did not identify exactly one terminal")
            })?;
        let mut wire = instance.connect().map_err(inconclusive)?.bounded(HANG);
        let process = wait(
            "no private foreground process mapped the leased signed package",
            || foreground(&mut wire, sid, payload),
        )?;
        observed_run = Some((process.pid, process.birth));
        println!(
            "interactive signed TUI: build={} pid={} birth={} device={} inode={} headless=true",
            payload.build(),
            process.pid,
            process.birth,
            process.image.device,
            process.image.inode
        );
        let mut terminal = Terminal {
            instance: &instance,
            sid,
            wire,
            payload,
            process,
        };
        terminal.submit(prompt, log)?;
        let (thread, writes) = terminal.withheld_result(world, prompt, log)?;
        Ok((terminal.process, thread, writes))
    })();
    let cleanup = instance.stop().and_then(|()| {
        let gone = |(pid, birth)| {
            crate::wait_until(HANG, || {
                aterm_sysprobe::kproc::record_read(pid).is_ok_and(|record| {
                    record.is_none_or(|r| {
                        let started = aterm_uds::process::ProcessBirth {
                            seconds: r.start_s,
                            microseconds: u64::from(r.start_us),
                        };
                        r.zombie || aterm_uds::process::Birth::from(started).0 != birth
                    })
                })
            })
        };
        if observed_run.is_some_and(|run| !gone(run)) {
            return Err(
                "the private signed TUI still runs after its headless host stopped".to_string(),
            );
        }
        println!("interactive cleanup: host_stopped=true desktop_window_opened=false");
        Ok(())
    });
    let (process, thread, writes) = crate::instance::after_stop(result, cleanup)?;
    println!(
        "PASS withheld-Source interactive TUI: build={} pid={} birth={} thread={thread} prompt_submitted=true matching_401_writes={writes}",
        payload.build(),
        process.pid,
        process.birth
    );
    Ok(0)
}

#[cfg(test)]
#[path = "codex_alab_withheld_tui_tests.rs"]
mod tests;
