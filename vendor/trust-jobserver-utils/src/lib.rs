//! Shared owned scheduling transport for Trust shims, Targo and custodians.
//!
//! The default `client` feature includes GNU token scheduling. Disable default
//! features to transfer existing CPU/GNU descriptors through the same checked
//! handoff protocol without linking or constructing a token client.

#[cfg(feature = "client")]
use std::io;
#[cfg(all(unix, feature = "client"))]
use std::os::fd::{AsRawFd, BorrowedFd, OwnedFd};
#[cfg(feature = "client")]
use std::process::Command;
#[cfg(all(unix, feature = "client"))]
use std::sync::Arc;

#[cfg(feature = "client")]
use jobserver::{Client, FromEnvErrorKind};

#[cfg(any(target_os = "linux", target_os = "macos"))]
mod handoff;

/// Scheduling-only advertisement installed by the owner of a chosen pool.
/// Its pathname and environment never grant proof or compiler authority.
pub const DESCENDANT_HANDOFF_ENV: &str = "CARGO_JOBSERVER_HANDOFF";

/// Inert advertisement for recovering an actual ancestor-owned implicit CPU lease.
pub const CPU_HANDOFF_ENV: &str = "TRUST_CPU_HANDOFF";

#[cfg(any(target_os = "linux", target_os = "macos"))]
pub use handoff::{
    CpuLeaseHandoff, ScopedHandoff, recover_cpu_lease, recover_cpu_resources,
    recover_named_fifo_resources,
};

/// The caller's checked scheduling authority, retained through the child wait.
///
/// Trust std closes unrelated descriptors when spawning. Imported jobserver
/// clients keep private copies of their pipe descriptors, but version 0.1.35
/// still advertises the original numbers when configuring a child. Own a fresh
/// pair and advertise exactly the descriptors our spawn hook hands over.
#[derive(Clone, Debug)]
#[must_use]
#[cfg(feature = "client")]
pub struct Jobserver {
    client: Client,
    #[cfg(unix)]
    pipe: Option<Arc<[OwnedFd; 2]>>,
    #[cfg(any(target_os = "linux", target_os = "macos"))]
    handoff: Option<Arc<handoff::Endpoint>>,
}

#[cfg(feature = "client")]
impl Jobserver {
    /// Create the build scheduler chosen by the caller, preserving its tokens
    /// and transport together. Importing never calls this constructor.
    pub fn new(limit: usize) -> io::Result<Self> {
        Self::with_client(Client::new(limit)?)
    }

    /// Adopt an owned named scheduling FIFO without creating any credits.
    /// The caller retains its private namespace through every child wait.
    #[cfg(unix)]
    pub fn from_fifo(path: &std::path::Path) -> io::Result<Self> {
        Self::with_client(Client::from_fifo(path)?)
    }

    /// Adopt a named pool only after the newly opened node matches retained
    /// descriptor witnesses. The namespace remains caller-owned.
    #[cfg(unix)]
    pub fn from_fifo_with_witnesses(
        path: &std::path::Path,
        read: std::os::fd::BorrowedFd<'_>,
        write: std::os::fd::BorrowedFd<'_>,
    ) -> io::Result<Self> {
        Self::with_client(Client::from_fifo_with_witnesses(path, read, write)?)
    }

    /// Authenticate a scoped named FIFO witness handoff before freshly opening
    /// the untrusted locator. No ambient import or private-token fallback.
    #[cfg(any(target_os = "linux", target_os = "macos"))]
    pub fn from_fifo_handoff(
        path: &std::path::Path,
        advertisement: &std::ffi::OsStr,
    ) -> io::Result<Self> {
        use std::os::fd::AsFd;
        let [read, write] = recover_named_fifo_resources(advertisement)?;
        Self::from_fifo_with_witnesses(path, read.as_fd(), write.as_fd())
    }

    /// Access this same server for in-process scheduling; do not re-import its
    /// advertised descriptor numbers after startup.
    pub fn client(&self) -> &Client {
        &self.client
    }

    /// Allow unaware build scripts to hand this same pipe to Bootstrap shims.
    /// The caller must retain this owner through every relevant child wait.
    /// No tokens are acquired, created, replenished or proxied by this service.
    pub fn enable_descendant_handoff(&mut self) -> io::Result<()> {
        #[cfg(any(target_os = "linux", target_os = "macos"))]
        self.enable_descendant_handoff_in(&std::env::temp_dir())?;
        // Named FIFOs and Windows semaphores already survive an unaware spawn.
        // Other Unix platforms retain their existing explicit-fd behavior;
        // this recovery protocol is implemented only on Linux and macOS.
        Ok(())
    }

    #[cfg(any(target_os = "linux", target_os = "macos"))]
    fn enable_descendant_handoff_in(&mut self, directory: &std::path::Path) -> io::Result<()> {
        if self.handoff.is_none() {
            if let Some(pipe) = &self.pipe {
                self.handoff = Some(Arc::new(handoff::Endpoint::new(Arc::clone(pipe), directory)?));
            }
        }
        Ok(())
    }

    /// Capture an inherited server without acquiring or creating any tokens.
    /// An advertised but invalid server is an error, never a private budget.
    ///
    /// # Safety
    ///
    /// Call at process startup, before opening argument files, sidecars or other
    /// descriptors. Checking for pipes cannot identify an unrelated pipe that
    /// has already reused an advertised descriptor number.
    pub unsafe fn from_env() -> io::Result<Option<Self>> {
        // SAFETY: the caller guarantees the early-import precondition above.
        let inherited = unsafe { Client::from_env_ext(true) };
        let client = match inherited.client {
            Ok(client) => client,
            Err(error)
                if matches!(
                    error.kind(),
                    FromEnvErrorKind::NoEnvVar
                        | FromEnvErrorKind::NoJobserver
                        | FromEnvErrorKind::NegativeFd
                        | FromEnvErrorKind::Unsupported
                ) =>
            {
                return Ok(None);
            }
            Err(error) => {
                return Err(io::Error::new(
                    io::ErrorKind::InvalidInput,
                    format!("invalid advertised jobserver: {error}"),
                ));
            }
        };
        Self::with_client(client).map(Some)
    }

    /// Import a scheduler for a native build driver, recovering only an
    /// authenticated ancestral pipe after an unaware descendant closed both
    /// advertised descriptors. No ambient environment bytes are changed.
    ///
    /// # Safety
    ///
    /// The early-startup requirement of [`Jobserver::from_env`] applies.
    pub unsafe fn from_env_with_handoff() -> io::Result<Option<Self>> {
        // SAFETY: the caller guarantees capture before descriptor reuse.
        match unsafe { Self::from_env() } {
            Ok(server) => Ok(server),
            Err(original) => {
                #[cfg(any(target_os = "linux", target_os = "macos"))]
                if let Some(advertisement) = std::env::var_os(DESCENDANT_HANDOFF_ENV) {
                    use std::os::fd::AsFd;

                    let flags = std::env::var_os("CARGO_MAKEFLAGS").unwrap_or_default();
                    let recovered = handoff::recover(&advertisement, &flags).and_then(|pipe| {
                        let client = Client::from_pipe(pipe[0].as_fd(), pipe[1].as_fd())?;
                        Self::with_client(client)
                    });
                    return recovered.map(Some).map_err(|error| {
                        io::Error::new(
                            error.kind(),
                            format!("{original}; cannot recover inherited jobserver: {error}"),
                        )
                    });
                }
                Err(original)
            }
        }
    }

    fn with_client(client: Client) -> io::Result<Self> {
        #[cfg(unix)]
        let pipe = Self::own_pipe(&client)?;
        Ok(Self {
            client,
            #[cfg(unix)]
            pipe,
            #[cfg(any(target_os = "linux", target_os = "macos"))]
            handoff: None,
        })
    }

    #[cfg(unix)]
    fn own_pipe(client: &Client) -> io::Result<Option<Arc<[OwnedFd; 2]>>> {
        // Read the library's canonical metadata, not ambient MAKEFLAGS: this
        // keeps its variable precedence and FIFO parsing authoritative.
        let mut metadata = Command::new("__jobserver_metadata_only__");
        client.configure(&mut metadata);
        let flags = metadata
            .get_envs()
            .find_map(|(key, value)| (key == "CARGO_MAKEFLAGS").then_some(value).flatten())
            .and_then(|flags| flags.to_str())
            .expect("jobserver configured non-UTF-8 or absent flags");
        let auth = flags
            .split_whitespace()
            .find_map(|flag| flag.strip_prefix("--jobserver-auth="))
            .expect("jobserver configured no authentication metadata");
        if auth.starts_with("fifo:") {
            use std::os::unix::fs::FileTypeExt;

            // jobserver 0.1.35 checks numeric descriptors but opens FIFO paths
            // without checking their file type. Reject a stable non-FIFO path
            // rather than treating a regular file as scheduling authority.
            // This check does not authenticate mutable FIFO pathnames.
            if !std::fs::metadata(&auth[5..])?.file_type().is_fifo() {
                return Err(io::Error::new(
                    io::ErrorKind::InvalidInput,
                    "advertised jobserver FIFO path is not a FIFO",
                ));
            }
            return Ok(None);
        }
        let invalid =
            || io::Error::new(io::ErrorKind::InvalidInput, "invalid jobserver pipe metadata");
        let (read, write) = auth.split_once(',').ok_or_else(invalid)?;
        let read = read.parse::<libc::c_int>().map_err(|_| invalid())?;
        let write = write.parse::<libc::c_int>().map_err(|_| invalid())?;
        // SAFETY: checked import just validated these inherited pipe numbers;
        // no intervening operation closes them. Duplicate immediately so later
        // file opens cannot affect the descriptors passed to the compiler.
        let pipe = unsafe {
            [
                BorrowedFd::borrow_raw(read).try_clone_to_owned()?,
                BorrowedFd::borrow_raw(write).try_clone_to_owned()?,
            ]
        };
        Ok(Some(Arc::new(pipe)))
    }

    /// Configure the final child after all other environment/argv processing.
    /// Explicitly hand over only the checked jobserver's endpoints.
    pub fn configure(&self, command: &mut Command) {
        // An ambient endpoint must never become a different pool's handoff.
        command.env_remove(DESCENDANT_HANDOFF_ENV);
        #[cfg(unix)]
        if let Some(pipe) = &self.pipe {
            configure_pipe(pipe, command);
            #[cfg(any(target_os = "linux", target_os = "macos"))]
            if let Some(handoff) = &self.handoff {
                handoff.configure(command);
            }
            return;
        }
        // FIFO paths and Windows semaphores use the library's transport.
        self.client.configure(command);
    }

    /// Configure GNU make with the same owned transport as other children.
    /// Derive all three variables from the final descriptor pair, never from
    /// the imported flags whose original descriptors may already be closed.
    pub fn configure_make(&self, command: &mut Command) {
        self.configure(command);
        let flags = command
            .get_envs()
            .find_map(|(key, value)| (key == "CARGO_MAKEFLAGS").then_some(value).flatten())
            .expect("jobserver configured no make flags")
            .to_owned();
        command.env("MAKEFLAGS", &flags).env("MFLAGS", flags);
    }
}

#[cfg(all(unix, feature = "client"))]
fn configure_pipe(pipe: &Arc<[OwnedFd; 2]>, command: &mut Command) {
    use std::os::unix::process::CommandExt;

    let pipe = Arc::clone(pipe);
    let auth = format!("{},{}", pipe[0].as_raw_fd(), pipe[1].as_raw_fd());
    command.env("CARGO_MAKEFLAGS", format!("-j --jobserver-fds={auth} --jobserver-auth={auth}"));
    // SAFETY: only async-signal-safe fcntl calls run after fork. The captured
    // owners keep the exact advertised descriptors live through spawn; Trust
    // std strips descriptors before running this explicit handoff hook.
    unsafe {
        command.pre_exec(move || {
            for fd in pipe.iter() {
                let flags = libc::fcntl(fd.as_raw_fd(), libc::F_GETFD);
                if flags < 0
                    || libc::fcntl(fd.as_raw_fd(), libc::F_SETFD, flags & !libc::FD_CLOEXEC) < 0
                {
                    return Err(io::Error::last_os_error());
                }
            }
            Ok(())
        });
    }
}

#[derive(Debug)]
#[cfg(feature = "client")]
enum InheritedTransport {
    Client(Jobserver),
    #[cfg(any(target_os = "linux", target_os = "macos"))]
    Recovered(Arc<[OwnedFd; 2]>),
}

/// An optional inherited scheduler, used by compiler/doc shims that do not
/// create a scheduler when none was passed by their caller.
#[derive(Debug)]
#[must_use]
#[cfg(feature = "client")]
pub struct InheritedJobserver(Option<InheritedTransport>);

#[cfg(feature = "client")]
impl InheritedJobserver {
    /// Capture the inherited scheduler before opening sidecars/argument files.
    ///
    /// # Safety
    ///
    /// The early-startup requirement of [`Jobserver::from_env`] applies.
    pub unsafe fn from_env() -> io::Result<Self> {
        // SAFETY: the caller guarantees the documented early capture.
        match unsafe { Jobserver::from_env() } {
            Ok(jobserver) => Ok(Self(jobserver.map(InheritedTransport::Client))),
            Err(original) => {
                #[cfg(any(target_os = "linux", target_os = "macos"))]
                if let Some(advertisement) = std::env::var_os(DESCENDANT_HANDOFF_ENV) {
                    let flags = std::env::var_os("CARGO_MAKEFLAGS").unwrap_or_default();
                    return handoff::recover(&advertisement, &flags)
                        .map(|pipe| Self(Some(InheritedTransport::Recovered(pipe))))
                        .map_err(|error| {
                            io::Error::new(
                                error.kind(),
                                format!("{original}; cannot recover inherited jobserver: {error}"),
                            )
                        });
                }
                Err(original)
            }
        }
    }

    /// Whether startup validated an actual inherited scheduler transport.
    /// This observation grants no additional tokens or scheduling authority.
    pub fn is_present(&self) -> bool {
        self.0.is_some()
    }

    /// Hand the same scheduler to this child, without creating a private one.
    pub fn configure(&self, command: &mut Command) {
        match &self.0 {
            Some(InheritedTransport::Client(jobserver)) => jobserver.configure(command),
            #[cfg(any(target_os = "linux", target_os = "macos"))]
            Some(InheritedTransport::Recovered(pipe)) => configure_pipe(pipe, command),
            None => {}
        }
        // Leaf shim boundary: only the exact valid numeric descriptors pass
        // to the real driver. Do not re-export a root binding after renumbering.
        command.env_remove(DESCENDANT_HANDOFF_ENV);
    }
}
