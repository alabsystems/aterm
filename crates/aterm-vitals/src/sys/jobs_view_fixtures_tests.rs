// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use crate::jobs::{
    Class, Consumer, JobKind, JobRow, MuFrom, Standing, Tick, Verdict, Wait, WaitName,
};

pub(crate) fn row(id: &str, tab: u64, class: Class, verdict: Verdict) -> JobRow {
    JobRow {
        id: id.into(),
        tab,
        kind: JobKind::AgentCall,
        orphan_of: None,
        consumer: Consumer::AgentBg,
        promoted_ms: None,
        task: Some("bq7f2k1za".into()),
        pgid: 40977,
        leader: (40977, None),
        exe: "curl".into(),
        members: 2,
        class,
        wait: None,
        named: None,
        stop: None,
        input: None,
        cpu_mc: None,
        zombies: None,
        standing: None,
        quiet_ms: None,
        phi_milli: None,
        mu_ms: None,
        mu_from: None,
        verdict,
        no_progress: false,
        working_descendant: None,
        episode: 1,
        exit: None,
        tick: Tick::default(),
    }
}

pub(crate) fn stuck(id: &str, tab: u64) -> JobRow {
    let mut r = row(id, tab, Class::Waiting, Verdict::Stuck);
    let mut w = Wait::bare(WaitName::Tcp, Standing::Capable, 40978, "curl");
    w.tcp = Some((
        Some("127.0.0.1:47811".parse().unwrap()),
        crate::jobs::TcpState::Established,
        0,
    ));
    r.wait = Some(w);
    r.quiet_ms = Some(1_152_000);
    r.phi_milli = Some(8_338);
    r.mu_ms = Some(60_000);
    r.mu_from = Some(MuFrom::Prior);
    r
}
