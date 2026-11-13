// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Private-instance custody fixture. It submits no installer command and opens no prompt.

use super::{Install, Terminal, receipt};
use crate::App;

impl App {
    pub(crate) fn seed_lab_installer_custody(&mut self) {
        if !aterm_types::dirs::runs_under_private_state_root()
            || aterm_types::dev_seam!("ATERM_DEBUG_INSTALLER_CUSTODY").as_deref()
                != Some(std::ffi::OsStr::new("1"))
        {
            return;
        }
        if let Err(error) = self.lab_installer_custody() {
            crate::logging::stderr_line!("aterm-lab-installer error={error}");
        }
    }

    fn lab_installer_custody(&mut self) -> Result<(), String> {
        let existing = self
            .workstation_setup
            .developer_tools_session
            .iter()
            .find(|terminal| !terminal.finished)
            .map(|terminal| (terminal.session, terminal.recovered));
        let (session, stage) = if let Some((session, recovered)) = existing {
            (session, if recovered { "adopted" } else { "seeded" })
        } else {
            if self.workstation_setup.developer_tools_mark.held().is_some() {
                return Ok(());
            }
            let child = self.pool.get(0).ok_or("missing initial fixture terminal")?;
            if child.handoff_local_id.is_some() {
                return Err("successor did not recover installer custody".into());
            }
            let sid = child.ctx.self_id.to_string();
            let pid = child.pid;
            let birth = child.shell_identity.birth;
            let mark = self
                .workstation_setup
                .developer_tools_mark
                .path
                .clone()
                .ok_or("fixture has no private installer mark")?;
            if !self.workstation_setup.developer_tools_mark.claim() {
                return Err("fixture installer mark was already claimed or unavailable".into());
            }
            let claim = self.workstation_setup.developer_tools_mark.hand(pid);
            let record = receipt::Receipt::for_child(&mark, &sid, pid, birth, claim.as_ref())
                .ok_or("fixture child has no exact native identity")?;
            record
                .write_exclusive()
                .map_err(|error| error.to_string())?;
            let mut terminal = Terminal::running(0, false, Some(record));
            terminal.claim = claim;
            self.workstation_setup
                .developer_tools_session
                .push(terminal);
            self.workstation_setup.developer_tools_attempt = Some(0);
            self.workstation_setup.developer_tools = Install::Opened;
            self.recover_access_dialog();
            (0, "seeded")
        };
        let child = self
            .pool
            .get(session)
            .ok_or("fixture child left the pool")?;
        let birth = child
            .shell_identity
            .birth
            .ok_or("fixture child has no birth")?;
        if self.developer_tools_install_holds_handoff() {
            return Err("fixture receipt does not permit the handoff".into());
        }
        crate::logging::stderr_line!(
            "aterm-lab-installer stage={stage} gui={} sid={} child={} birth={birth} active=true",
            std::process::id(),
            child.ctx.self_id,
            child.pid,
        );
        Ok(())
    }
}
