// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Bounded requests to one authenticated instance, retaining its original token.

use super::*;

/// One concrete instance's socket and original token. A later connection presents that same
/// token, so replacement of the socket or token cannot silently switch the instance read.
pub struct PeerClient {
    path: String,
    token: String,
    first: Option<CtlStream>,
}

impl PeerClient {
    /// Pin `path`, connect within `within`, then read its no-follow, bounded token file.
    ///
    /// # Errors
    /// The shared bounded connector is occupied, the deadline expires, the connection fails,
    /// or the token file is absent, refused or malformed. Nothing is sent without a token.
    pub fn connect(path: &str, within: std::time::Duration) -> io::Result<Self> {
        let deadline = ProbeDeadline::after(within);
        let path = pin_alias(path);
        let stream = shared_probe_connector()?.connect(&path, deadline)?;
        let token = read_token_beside(&path)?;
        deadline.remaining()?;
        Ok(Self {
            path,
            token,
            first: Some(stream),
        })
    }

    fn stream(&mut self, deadline: ProbeDeadline) -> io::Result<CtlStream> {
        deadline.remaining()?;
        match self.first.take() {
            Some(stream) => Ok(stream),
            None => shared_probe_connector()?.connect(&self.path, deadline),
        }
    }

    /// Authenticate one new connection and read `verb`'s counted `OK <n>` reply. Connect,
    /// writes and all reply lines share one deadline and the control client's byte bounds.
    ///
    /// # Errors
    /// Invalid request text, a connection or authentication failure, expiry, malformed framing,
    /// an oversized reply, invalid UTF-8, or a hang-up before every counted line arrives.
    pub fn lines(&mut self, verb: &str, within: std::time::Duration) -> io::Result<Vec<String>> {
        request_line(verb)?;
        let deadline = ProbeDeadline::after(within);
        let stream = self.stream(deadline)?;
        exchange_lines_until(&stream, &self.token, verb, deadline)
    }

    /// Authenticate one new connection, write these requests, and read one bounded status line
    /// for each. Dims and cwd have this framing rather than an `OK <n>` body. The whole batch,
    /// including connect, shares `within`; each returned line retains its `OK` or `ERR` prefix.
    ///
    /// # Errors
    /// Invalid request text, too many requests, a connection/authentication failure, expiry,
    /// an oversized or malformed reply, or a hang-up before every status line arrives.
    pub fn statuses(
        &mut self,
        requests: &[String],
        within: std::time::Duration,
    ) -> io::Result<Vec<String>> {
        if requests.len() > MAX_STREAM_LINES {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "too many requests",
            ));
        }
        for request in requests {
            request_line(request)?;
        }
        if requests.is_empty() {
            return Ok(Vec::new());
        }
        let deadline = ProbeDeadline::after(within);
        let stream = self.stream(deadline)?;
        let mut io = ProbeIo {
            stream: &stream,
            deadline,
        };
        let wrote = (|| {
            io.write_all(format!("AUTH {}\n", self.token).as_bytes())?;
            for request in requests {
                io.write_all(request.as_bytes())?;
                io.write_all(b"\n")?;
            }
            io.flush()
        })();
        let mut unwritten = match wrote {
            Ok(()) => None,
            Err(error) if redial::is_hangup(&error) => Some(error),
            Err(error) => return Err(error),
        };
        let mut reader = BufReader::new(io);
        collect_reply_lines(requests.len(), MAX_BODY_BYTES, |line| {
            let n = match probe_read_bounded_line(&mut reader, line, deadline) {
                Ok(n) => n,
                Err(error) => return Err(unwritten.take().unwrap_or(error)),
            };
            let status = line.trim_end_matches(['\r', '\n']);
            if status == "ERR auth" || status.starts_with("ERR control server busy") {
                return Err(io::Error::new(
                    io::ErrorKind::PermissionDenied,
                    status.to_owned(),
                ));
            }
            if n > 0 && status != "OK" && !status.starts_with("OK ") && !status.starts_with("ERR ")
            {
                return Err(malformed_header_error(status));
            }
            if let Some(error) = unwritten.take() {
                return Err(error);
            }
            Ok(n)
        })
    }
}

fn request_line(request: &str) -> io::Result<()> {
    if request.is_empty()
        || request.len() > MAX_REQUEST_LINE
        || request.bytes().any(|b| matches!(b, b'\n' | b'\r' | 0))
    {
        Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "expected one bounded request line",
        ))
    } else {
        Ok(())
    }
}

#[cfg(all(test, unix))]
#[path = "peer_client_tests.rs"]
mod tests;
