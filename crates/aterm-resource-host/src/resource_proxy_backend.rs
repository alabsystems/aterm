// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The cached helper forwards original descriptions once to its actual pinned
//! ancestor host. Unknown attachment retains private lifecycle observation.
use crate::resource_proxy::Backend;
use aterm_ctl::{
    resource_admission_transport as transport,
    resource_subgroup_client::{Delegations, RemoteChild, Stdio, StdioConfig, SubmitError},
};
use aterm_types::{resource_admission::Binding, resource_subgroup as subgroup};
use aterm_uds::{CtlStream, ready};
use std::{
    io,
    os::fd::{AsFd, OwnedFd},
    time::{Duration, Instant},
};
use trust_resource_controller::{GnuTransport, Request};
use trust_resource_provenance::Store;

pub(crate) struct NativeBackend {
    #[cfg(test)]
    store: Option<Store>,
}
impl NativeBackend {
    pub(crate) const fn account() -> Self {
        Self {
            #[cfg(test)]
            store: None,
        }
    }
    #[cfg(all(test, target_os = "macos"))]
    pub(crate) fn for_store(store: Store) -> Self {
        Self { store: Some(store) }
    }
    fn connect(&self, deadline: Instant) -> io::Result<trust_resource_provenance::Host> {
        #[cfg(test)]
        if let Some(store) = &self.store {
            return store.connect_ancestor(deadline);
        }
        Store::open_account()?.connect_ancestor(deadline)
    }
}
fn invalid() -> io::Error {
    io::ErrorKind::InvalidData.into()
}
fn fd(value: Stdio) -> io::Result<OwnedFd> {
    match value {
        Stdio::Fd(fd) => Ok(fd),
        _ => Err(invalid()),
    }
}
struct Rights {
    stdin: OwnedFd,
    stdout: OwnedFd,
    stderr: OwnedFd,
    events: OwnedFd,
    producer: OwnedFd,
    delegated: Delegations,
}
impl Rights {
    fn borrowed(&self) -> Vec<std::os::fd::BorrowedFd<'_>> {
        let mut result = vec![
            self.stdin.as_fd(),
            self.stdout.as_fd(),
            self.stderr.as_fd(),
            self.events.as_fd(),
            self.producer.as_fd(),
        ];
        result.extend(self.delegated.cpu.iter().map(AsFd::as_fd));
        result.extend(self.delegated.gnu.iter().map(AsFd::as_fd));
        result
    }
}
impl Backend for NativeBackend {
    fn submit(
        &mut self,
        request: Request,
        producer: OwnedFd,
        stdio: StdioConfig,
        delegated: Delegations,
        accounted: bool,
    ) -> Result<RemoteChild, SubmitError> {
        let deadline = Instant::now() + Duration::from_secs(8);
        let (events, host_events) =
            aterm_uds::kept_out(CtlStream::pair).map_err(SubmitError::NotSubmitted)?;
        let binding = Binding {
            session: request.binding.session.clone(),
            attempt: request.member_attempt,
        };
        let child = (if accounted {
            RemoteChild::from_private_events_accounted(binding, events.into())
        } else {
            RemoteChild::from_private_events(binding, events.into())
        })
        .map_err(SubmitError::NotSubmitted)?;
        let mut offered = false;
        let prepare = || -> io::Result<_> {
            let bytes = request.encode()?;
            let gnu = if matches!(request.delegations.gnu, GnuTransport::None) {
                0
            } else {
                2
            };
            if !request.delegations.fences.is_empty()
                || !delegated.fences.is_empty()
                || delegated.cpu.is_some() != request.delegations.cpu.is_some()
                || delegated.gnu.len() != gnu
            {
                return Err(invalid());
            }
            let host = self.connect(deadline)?;
            let stream = host.stream();
            let mut hello = aterm_json::json!({
                "version": 3, "role": if accounted { trust_resource_controller::accounting::PROXY_ROLE } else { "proxy-member" }, "session": request.binding.session,
                "attempt": aterm_codec::hex::encode_lower(&request.binding.root_attempt),
            });
            if accounted {
                hello.as_object_mut().ok_or_else(invalid)?.insert(
                    "accounting".into(),
                    trust_resource_controller::accounting::CAPABILITY.into(),
                );
            }
            let hello = aterm_json::to_vec(&hello).map_err(io::Error::other)?;
            transport::write_frame(stream, &hello, deadline)?;
            if transport::read_frame(stream, deadline)? != subgroup::PREPARE {
                return Err(invalid());
            }
            offered = true;
            transport::write_frame(stream, &bytes, deadline)?;
            if transport::read_frame(stream, deadline)? != subgroup::READY {
                return Err(io::ErrorKind::PermissionDenied.into());
            }
            host.revalidate(deadline)?;
            Ok((
                host,
                Rights {
                    stdin: fd(stdio.stdin)?,
                    stdout: fd(stdio.stdout)?,
                    stderr: fd(stdio.stderr)?,
                    events: host_events.into(),
                    producer,
                    delegated,
                },
            ))
        };
        let (host, rights) = match prepare() {
            Ok(prepared) => prepared,
            Err(error) => {
                return Err(if offered {
                    // A reused attempt may already exist. A missing READY is not
                    // proof that this binding never executed, even before our FDs.
                    SubmitError::Unknown {
                        error,
                        child: Box::new(child),
                    }
                } else {
                    SubmitError::NotSubmitted(error)
                });
            }
        };
        // Any attempted transfer can deliver all rights before an ACK is lost.
        // Keep the same private observer; never manufacture NoChild or replay.
        let result = (|| {
            let stream = host.stream();
            loop {
                if Instant::now() >= deadline {
                    return Err(io::ErrorKind::TimedOut.into());
                }
                match aterm_uds::fdpass::send_with_fds(
                    stream,
                    &[subgroup::FD_MARKER],
                    &rights.borrowed(),
                ) {
                    Ok(1) => break,
                    Ok(_) => return Err(io::ErrorKind::WriteZero.into()),
                    Err(error) if error.kind() == io::ErrorKind::Interrupted => {}
                    Err(error) if error.kind() == io::ErrorKind::WouldBlock => {
                        ready::until(stream.as_fd(), ready::Interest::Write, deadline)?;
                    }
                    Err(error) => return Err(error),
                }
            }
            if transport::read_frame(stream, deadline)? != subgroup::ATTACHED {
                return Err(invalid());
            }
            Ok(())
        })();
        drop(rights);
        match result {
            Ok(()) => Ok(child),
            Err(error) => Err(SubmitError::Unknown {
                error,
                child: Box::new(child),
            }),
        }
    }
}
