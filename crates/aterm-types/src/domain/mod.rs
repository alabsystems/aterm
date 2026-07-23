// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0
// Author: Andrew Yates

//! Domain abstraction types for terminal connections.
//!
//! A Domain represents a context for spawning terminal panes. Different domains
//! provide different connection types (local PTY, SSH, WSL, serial, etc.).
//!
//! These types live in `aterm-types` (the shared leaf crate) so that both
//! `aterm-core` domain implementations and extracted crates (`aterm-agent`)
//! can share the same trait boundary without circular dependencies.
//!
//! ## Design
//!
//! Based on WezTerm's domain architecture, with adaptations for aterm's
//! async-agnostic core library design. See `designs/2026-02-14-terminal-trait-decomposition-gate2.md`.

mod registry;

pub use registry::DomainRegistry;

use std::collections::HashMap;
use std::fmt;
use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};

use crate::TerminalSize;

// =============================================================================
// Identity types
// =============================================================================

/// Unique identifier for a domain.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct DomainId(u64);

impl DomainId {
    /// Allocate a new unique domain ID.
    #[must_use]
    pub fn new() -> Self {
        static COUNTER: AtomicU64 = AtomicU64::new(0);
        Self(COUNTER.fetch_add(1, Ordering::Relaxed))
    }

    /// Get the raw ID value.
    #[must_use]
    pub fn raw(self) -> u64 {
        self.0
    }
}

impl Default for DomainId {
    fn default() -> Self {
        Self::new()
    }
}

impl fmt::Display for DomainId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        // Trust gate: `write_str` instead of `write!` — runtime-argument
        // `format_args!` cannot be lowered natively. Byte-identical: the
        // nested `{}` always formats with default options, like `to_string`.
        f.write_str("domain:")?;
        f.write_str(&self.0.to_string())
    }
}

/// Unique identifier for a pane.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct PaneId(u64);

impl PaneId {
    /// Allocate a new unique pane ID.
    #[must_use]
    pub fn new() -> Self {
        static COUNTER: AtomicU64 = AtomicU64::new(0);
        Self(COUNTER.fetch_add(1, Ordering::Relaxed))
    }

    /// Get the raw ID value.
    #[must_use]
    pub fn raw(self) -> u64 {
        self.0
    }
}

impl Default for PaneId {
    fn default() -> Self {
        Self::new()
    }
}

impl fmt::Display for PaneId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        // Trust gate: see `Display for DomainId`.
        f.write_str("pane:")?;
        f.write_str(&self.0.to_string())
    }
}

// =============================================================================
// State enums
// =============================================================================

/// Connection state of a domain.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
#[non_exhaustive]
pub enum DomainState {
    /// Domain is not connected.
    #[default]
    Detached,
    /// Domain is connected and ready to spawn panes.
    Attached,
    /// Domain is in the process of connecting.
    Connecting,
    /// Domain connection failed.
    Failed,
}

/// Type of domain connection.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub enum DomainType {
    /// Local PTY connection.
    Local,
    /// SSH remote connection.
    Ssh,
    /// Windows Subsystem for Linux.
    Wsl,
    /// Serial port connection.
    Serial,
    /// Remote multiplexer connection.
    Mux,
    /// SSH-backed multiplexer connection (one SSH connection, many panes).
    SshMultiplexer,
    /// Custom/plugin domain.
    Custom,
}

/// Capabilities advertised by a domain implementation.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct DomainCapabilities {
    /// Domain runs on a remote host.
    pub remote: bool,
    /// Domain can carry multiple panes over one underlying connection.
    pub multiplexed: bool,
    /// Domain can spawn panes.
    pub spawnable: bool,
    /// Domain preserves panes when detached.
    pub detachable: bool,
}

impl DomainCapabilities {
    /// Build the default capability set for a domain type.
    #[must_use]
    pub const fn for_type(domain_type: DomainType) -> Self {
        match domain_type {
            DomainType::Local => Self {
                remote: false,
                multiplexed: false,
                spawnable: true,
                detachable: false,
            },
            DomainType::Ssh => Self {
                remote: true,
                multiplexed: false,
                spawnable: true,
                detachable: false,
            },
            DomainType::Wsl | DomainType::Serial => Self {
                remote: false,
                multiplexed: false,
                spawnable: true,
                detachable: false,
            },
            DomainType::Mux => Self {
                remote: true,
                multiplexed: true,
                spawnable: true,
                detachable: true,
            },
            DomainType::SshMultiplexer => Self {
                remote: true,
                multiplexed: true,
                spawnable: true,
                detachable: true,
            },
            DomainType::Custom => Self {
                remote: false,
                multiplexed: false,
                spawnable: false,
                detachable: false,
            },
        }
    }

    /// Override spawnability based on a live domain state.
    #[must_use]
    pub const fn with_spawnable(mut self, spawnable: bool) -> Self {
        self.spawnable = spawnable;
        self
    }

    /// Override detachment support based on the concrete implementation.
    #[must_use]
    pub const fn with_detachable(mut self, detachable: bool) -> Self {
        self.detachable = detachable;
        self
    }
}

/// Errors while parsing domain connection metadata.
// Skip (propagates to the derive-generated impls): the derived `Clone` deep-clones
// heap-owning fields (String/PathBuf/Vec/Arc) — the idiomatic-alloc class.
#[cfg_attr(trust_verify, trust::skip)]
#[derive(Debug, Clone, PartialEq, Eq, aterm_error::Error)]
#[non_exhaustive]
pub enum DomainConfigError {
    /// SSH target string was empty.
    #[error("SSH target must not be empty")]
    EmptyTarget,
    /// SSH target did not include a host.
    #[error("SSH target must include a host")]
    MissingHost,
    /// SSH target had an invalid port.
    #[error("invalid SSH port: {0}")]
    InvalidPort(String),
    /// SSH target had malformed bracketed host syntax.
    #[error("invalid SSH target syntax: {0}")]
    InvalidTarget(String),
}

/// SSH endpoint metadata for remote domains.
// Skip (propagates to the derive-generated impls): the derived `Clone` deep-clones
// heap-owning fields (String/PathBuf/Vec/Arc) — the idiomatic-alloc class.
#[cfg_attr(trust_verify, trust::skip)]
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SshConnectionConfig {
    /// Optional username.
    pub user: Option<String>,
    /// Hostname or address.
    pub host: String,
    /// TCP port.
    pub port: u16,
}

impl SshConnectionConfig {
    /// Default SSH port.
    pub const DEFAULT_PORT: u16 = 22;

    /// Create an SSH target for a host on port 22.
    #[must_use]
    pub fn new(host: impl Into<String>) -> Self {
        Self {
            user: None,
            host: host.into(),
            port: Self::DEFAULT_PORT,
        }
    }

    /// Parse `[user@]host[:port]`.
    ///
    /// Bracketed IPv6 host syntax (`user@[::1]:2222`) is accepted.
    ///
    /// # Errors
    ///
    /// Returns [`DomainConfigError`] if the target is empty, missing a host,
    /// has malformed bracket syntax, or includes an invalid port.
    pub fn parse(target: &str) -> Result<Self, DomainConfigError> {
        let target = target.trim();
        if target.is_empty() {
            return Err(DomainConfigError::EmptyTarget);
        }

        let (user, host_port) = match target.split_once('@') {
            Some((user, rest)) => (Some(user.to_string()), rest),
            None => (None, target),
        };

        let (host, port) = parse_host_port(host_port)?;
        if host.is_empty() {
            return Err(DomainConfigError::MissingHost);
        }

        Ok(Self { user, host, port })
    }

    /// Set the username.
    #[must_use]
    pub fn with_user(mut self, user: impl Into<String>) -> Self {
        self.user = Some(user.into());
        self
    }

    /// Set the TCP port.
    #[must_use]
    pub const fn with_port(mut self, port: u16) -> Self {
        self.port = port;
        self
    }

    /// Return `user@host:port` (or `host:port` when no user is set).
    #[must_use]
    pub fn destination(&self) -> String {
        // Trust gate: concatenation instead of `format!` — runtime-argument
        // `format_args!` cannot be lowered natively. Byte-identical output.
        let host = if self.host.contains(':') && !self.host.starts_with('[') {
            let mut bracketed = String::from("[");
            bracketed.push_str(&self.host);
            bracketed.push(']');
            bracketed
        } else {
            self.host.clone()
        };
        let mut dest = String::new();
        if let Some(user) = &self.user {
            dest.push_str(user);
            dest.push('@');
        }
        dest.push_str(&host);
        dest.push(':');
        dest.push_str(&self.port.to_string());
        dest
    }
}

// Skip: `str::parse`/split absent std bodies; fail-closed.
#[cfg_attr(trust_verify, trust::skip)]
fn parse_host_port(host_port: &str) -> Result<(String, u16), DomainConfigError> {
    if let Some(rest) = host_port.strip_prefix('[') {
        // `split_once(']')` is byte-for-byte equivalent to find-then-slice on
        // the first ']' (host = before, tail = after, delimiter excluded) and
        // avoids the manual `end + 1` index the Trust gate cannot bound.
        let Some((host, tail)) = rest.split_once(']') else {
            return Err(DomainConfigError::InvalidTarget(host_port.to_string()));
        };
        let host = host.to_string();
        let port = if tail.is_empty() {
            SshConnectionConfig::DEFAULT_PORT
        } else if let Some(stripped) = tail.strip_prefix(':') {
            parse_port(stripped)?
        } else {
            return Err(DomainConfigError::InvalidTarget(host_port.to_string()));
        };
        return Ok((host, port));
    }

    match host_port.rsplit_once(':') {
        Some((host, port)) if !host.contains(':') => Ok((host.to_string(), parse_port(port)?)),
        _ => Ok((host_port.to_string(), SshConnectionConfig::DEFAULT_PORT)),
    }
}

// Skip: `str::parse` absent std body; fail-closed.
#[cfg_attr(trust_verify, trust::skip)]
fn parse_port(port: &str) -> Result<u16, DomainConfigError> {
    port.parse::<u16>()
        .map_err(|_| DomainConfigError::InvalidPort(port.to_string()))
}

/// Wire protocol used by a multiplexer domain.
// Skip (propagates to the derive-generated impls): the derived `Clone` deep-clones
// heap-owning fields (String/PathBuf/Vec/Arc) — the idiomatic-alloc class.
#[cfg_attr(trust_verify, trust::skip)]
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub enum MuxProtocol {
    /// aterm-native mux protocol.
    Aterm,
    /// tmux control mode.
    TmuxControl,
    /// A named custom protocol.
    Custom(String),
}

/// Transport for a multiplexer connection.
// Skip (propagates to the derive-generated impls): the derived `Clone` deep-clones
// `PathBuf`/`SshConnectionConfig` — the idiomatic-alloc class. (The `PartialEq`
// below is already hand-written for the sibling reason documented there.)
#[cfg_attr(trust_verify, trust::skip)]
#[derive(Debug, Clone, Eq)]
#[non_exhaustive]
pub enum MuxTransport {
    /// Local Unix-domain socket path.
    LocalSocket(PathBuf),
    /// SSH transport to the mux host.
    Ssh(SshConnectionConfig),
}

// Trust gate: hand-written to match the derive exactly. The derived `eq`
// lowers to a generic `std::cmp::PartialEq::eq` call target that the native
// TrustIr lowering treats as a hard error (unlike the derived `clone`, which
// is waived as memory-safe).
impl PartialEq for MuxTransport {
    fn eq(&self, other: &Self) -> bool {
        match (self, other) {
            (Self::LocalSocket(a), Self::LocalSocket(b)) => a == b,
            (Self::Ssh(a), Self::Ssh(b)) => a == b,
            _ => false,
        }
    }
}

/// Multiplexer connection metadata.
// Skip (propagates to the derive-generated impls): the derived `Clone` deep-clones
// heap-owning fields (String/PathBuf/Vec/Arc) — the idiomatic-alloc class.
#[cfg_attr(trust_verify, trust::skip)]
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MuxConnectionConfig {
    /// User-visible mux domain name.
    pub name: String,
    /// Underlying transport.
    pub transport: MuxTransport,
    /// Protocol spoken over the transport.
    pub protocol: MuxProtocol,
    /// Optional pane cap advertised by the server.
    pub max_panes: Option<u32>,
}

impl MuxConnectionConfig {
    /// Create a aterm-native mux over a local socket.
    #[must_use]
    pub fn local_socket(name: impl Into<String>, path: impl Into<PathBuf>) -> Self {
        Self {
            name: name.into(),
            transport: MuxTransport::LocalSocket(path.into()),
            protocol: MuxProtocol::Aterm,
            max_panes: None,
        }
    }

    /// Create a aterm-native mux over SSH.
    #[must_use]
    pub fn ssh(name: impl Into<String>, target: SshConnectionConfig) -> Self {
        Self {
            name: name.into(),
            transport: MuxTransport::Ssh(target),
            protocol: MuxProtocol::Aterm,
            max_panes: None,
        }
    }

    /// Set the advertised pane cap.
    #[must_use]
    pub const fn with_max_panes(mut self, max_panes: u32) -> Self {
        self.max_panes = Some(max_panes);
        self
    }

    /// Set the wire protocol.
    #[must_use]
    pub fn with_protocol(mut self, protocol: MuxProtocol) -> Self {
        self.protocol = protocol;
        self
    }
}

/// Connection metadata returned by a domain.
// Skip (propagates to the derive-generated impls): the derived `Clone` deep-clones
// heap-owning fields (String/PathBuf/Vec/Arc) — the idiomatic-alloc class.
#[cfg_attr(trust_verify, trust::skip)]
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub enum DomainConnectionInfo {
    /// Local PTY domain.
    Local,
    /// Plain SSH domain.
    Ssh(SshConnectionConfig),
    /// Generic multiplexer domain.
    Mux(MuxConnectionConfig),
    /// SSH-backed multiplexer domain (one SSH transport, many panes).
    SshMultiplexer(MuxConnectionConfig),
}

// =============================================================================
// Configuration
// =============================================================================

/// OS-level sandbox policy for spawned child processes.
///
/// Controls kernel-enforced containment applied post-fork/pre-exec.
/// The sandbox is permanent — the child cannot escape it.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
#[non_exhaustive]
pub enum SandboxPolicy {
    /// No sandbox restrictions (user's own interactive shell).
    #[default]
    None,
    /// Home directory read/write (except sensitive paths), system read-only,
    /// outbound network only. Default for trusted agent sessions.
    Standard,
    /// Project directory + /tmp read/write only. Home read-only. No network.
    /// Requires `project_dir` in [`SpawnConfig`].
    Restricted,
    /// Project directory read-only, /tmp read/write. No network, no fork.
    /// Requires `project_dir` in [`SpawnConfig`].
    Paranoid,
}

pub use crate::env_sanitize::{
    ENV_DENY_PREFIXES, ENV_DENY_VARS, ENV_EDGE_READ, ENV_EDGE_SIGNAL, ENV_EDGE_TOKENS,
    ENV_EDGE_WRITE, ENV_LAUNCH_NONCE, ENV_NET_CERT, ENV_NET_KEY, ENV_NET_LISTEN,
    ENV_PARENT_SESSION_ID, ENV_SESSION_ID, is_ai_env_var,
};

/// Configuration for spawning a new pane.
// Skip (propagates to the derive-generated impls): the derived `Clone` deep-clones
// heap-owning fields (String/PathBuf/Vec/Arc) — the idiomatic-alloc class.
#[cfg_attr(trust_verify, trust::skip)]
#[derive(Debug, Clone, Default)]
pub struct SpawnConfig {
    /// Command to run (None = default shell).
    pub command: Option<String>,
    /// Arguments to the command.
    pub args: Vec<String>,
    /// Working directory.
    pub cwd: Option<PathBuf>,
    /// Environment variables to set.
    pub env: HashMap<String, String>,
    /// Environment variables to clear.
    pub env_remove: Vec<String>,
    /// OS-level sandbox policy for the child process.
    pub sandbox: SandboxPolicy,
    /// Project directory for sandbox restriction (Restricted/Paranoid tiers).
    /// Falls back to `cwd` if not specified.
    pub project_dir: Option<PathBuf>,
}

impl SpawnConfig {
    /// Create a new spawn config with default shell.
    #[must_use]
    pub fn default_shell() -> Self {
        Self::default()
    }

    /// Create a spawn config for a specific command.
    #[must_use]
    pub fn command(cmd: impl Into<String>) -> Self {
        Self {
            command: Some(cmd.into()),
            ..Default::default()
        }
    }

    /// Set the working directory.
    #[must_use]
    pub fn with_cwd(mut self, cwd: impl Into<PathBuf>) -> Self {
        self.cwd = Some(cwd.into());
        self
    }

    /// Add an argument.
    #[must_use]
    pub fn with_arg(mut self, arg: impl Into<String>) -> Self {
        self.args.push(arg.into());
        self
    }

    /// Add multiple arguments.
    #[must_use]
    pub fn with_args<I, S>(mut self, args: I) -> Self
    where
        I: IntoIterator<Item = S>,
        S: Into<String>,
    {
        self.args.extend(args.into_iter().map(Into::into));
        self
    }

    /// Set an environment variable.
    #[must_use]
    pub fn with_env(mut self, key: impl Into<String>, value: impl Into<String>) -> Self {
        self.env.insert(key.into(), value.into());
        self
    }

    /// Set the sandbox policy.
    #[must_use]
    pub fn with_sandbox(mut self, policy: SandboxPolicy) -> Self {
        self.sandbox = policy;
        self
    }

    /// Set the project directory for sandbox restriction.
    #[must_use]
    pub fn with_project_dir(mut self, dir: impl Into<PathBuf>) -> Self {
        self.project_dir = Some(dir.into());
        self
    }
}

// =============================================================================
// Error types
// =============================================================================

/// Result type for domain operations.
pub type DomainResult<T> = Result<T, DomainError>;

/// Errors that can occur in domain operations.
#[derive(Debug, aterm_error::Error)]
#[non_exhaustive]
pub enum DomainError {
    /// Domain is not connected.
    #[error("domain is not attached")]
    NotAttached,
    /// Connection failed.
    #[error("connection failed: {0}")]
    ConnectionFailed(String),
    /// Spawn failed.
    #[error("spawn failed: {0}")]
    SpawnFailed(String),
    /// I/O error.
    #[error("I/O error: {0}")]
    Io(#[from] std::io::Error),
    /// Pane not found.
    #[error("pane not found: {}", .0.raw())]
    PaneNotFound(PaneId),
    /// Domain not found.
    #[error("domain not found: {}", .0.raw())]
    DomainNotFound(DomainId),
    /// Operation not supported.
    #[error("operation not supported: {0}")]
    NotSupported(String),
    /// Authentication failed.
    #[error("authentication failed: {0}")]
    AuthenticationFailed(String),
    /// Timeout.
    #[error("operation timed out")]
    Timeout,
    /// Other error.
    #[error("{0}")]
    Other(String),
}

// =============================================================================
// Traits
// =============================================================================

/// A pane represents an active terminal session within a domain.
///
/// Panes are the fundamental unit of terminal interaction. They handle:
/// - Input/output to the underlying process or connection
/// - Terminal size (rows/columns)
/// - Process lifecycle (running/exited)
pub trait Pane: Send + Sync {
    /// Get the unique pane ID.
    fn pane_id(&self) -> PaneId;

    /// Get the domain ID this pane belongs to.
    fn domain_id(&self) -> DomainId;

    /// Get the current terminal size.
    fn size(&self) -> TerminalSize;

    /// Resize the terminal.
    fn resize(&self, size: TerminalSize) -> DomainResult<()>;

    /// Write data to the pane (input to the process).
    fn write(&self, data: &[u8]) -> DomainResult<usize>;

    /// Read available data from the pane (output from the process).
    ///
    /// Returns the data read, or an empty slice if no data is available.
    /// This is non-blocking.
    fn read(&self, buf: &mut [u8]) -> DomainResult<usize>;

    /// Check if the pane process is still running.
    fn is_alive(&self) -> bool;

    /// Get the exit status if the process has exited.
    fn exit_status(&self) -> Option<i32>;

    /// Kill the pane process.
    fn kill(&self) -> DomainResult<()>;

    /// Get the process ID (if applicable).
    fn pid(&self) -> Option<u32> {
        None
    }

    /// Get the pane title.
    fn title(&self) -> String {
        String::new()
    }

    /// Get the current working directory (if known).
    fn cwd(&self) -> Option<PathBuf> {
        None
    }

    /// Get the foreground process name (if known).
    fn foreground_process_name(&self) -> Option<String> {
        None
    }
}

/// A domain represents a context for spawning terminal panes.
///
/// Domains abstract over different connection types (local, SSH, WSL, etc.)
/// providing a uniform interface for creating and managing terminal sessions.
pub trait Domain: Send + Sync {
    /// Get the unique domain ID.
    fn domain_id(&self) -> DomainId;

    /// Get the domain name (short identifier).
    fn domain_name(&self) -> &str;

    /// Get a human-readable label for the domain.
    // Skip: a trait DEFAULT method calling caller-implemented `Self` methods —
    // `Domain` is a public, downstream-implemented trait (the irreducible
    // open-world dispatch class), and the default bodies allocate Strings.
    #[cfg_attr(trust_verify, trust::skip)]
    fn domain_label(&self) -> String {
        self.domain_name().to_string()
    }

    /// Get the domain type.
    fn domain_type(&self) -> DomainType;

    /// Get the domain's advertised connection metadata, if known.
    // Skip: a trait DEFAULT method calling caller-implemented `Self` methods —
    // `Domain` is a public, downstream-implemented trait (the irreducible
    // open-world dispatch class), and the default bodies allocate Strings.
    #[cfg_attr(trust_verify, trust::skip)]
    fn connection_info(&self) -> Option<DomainConnectionInfo> {
        match self.domain_type() {
            DomainType::Local => Some(DomainConnectionInfo::Local),
            _ => None,
        }
    }

    /// Get the domain's advertised capabilities.
    // Skip: a trait DEFAULT method calling caller-implemented `Self` methods —
    // `Domain` is a public, downstream-implemented trait (the irreducible
    // open-world dispatch class), and the default bodies allocate Strings.
    #[cfg_attr(trust_verify, trust::skip)]
    fn capabilities(&self) -> DomainCapabilities {
        DomainCapabilities::for_type(self.domain_type())
            .with_spawnable(self.spawnable())
            .with_detachable(self.detachable())
    }

    /// Get the current connection state.
    fn state(&self) -> DomainState;

    /// Check if this domain can spawn new panes.
    ///
    /// Returns false for placeholder or disconnected domains.
    // Skip: a trait DEFAULT method calling caller-implemented `Self` methods —
    // `Domain` is a public, downstream-implemented trait (the irreducible
    // open-world dispatch class), and the default bodies allocate Strings.
    #[cfg_attr(trust_verify, trust::skip)]
    fn spawnable(&self) -> bool {
        self.state() == DomainState::Attached
    }

    /// Check if this domain supports detachment.
    ///
    /// Detachable domains preserve panes when disconnected.
    fn detachable(&self) -> bool;

    /// Attach to the domain (establish connection).
    ///
    /// For local domains, this is typically a no-op.
    /// For remote domains, this establishes the connection.
    fn attach(&self) -> DomainResult<()>;

    /// Detach from the domain.
    ///
    /// For detachable domains, panes continue running.
    /// For non-detachable domains, panes are terminated.
    fn detach(&self) -> DomainResult<()>;

    /// Spawn a new pane in this domain.
    fn spawn_pane(&self, size: TerminalSize, config: SpawnConfig) -> DomainResult<Arc<dyn Pane>>;

    /// Get a pane by ID.
    fn get_pane(&self, id: PaneId) -> Option<Arc<dyn Pane>>;

    /// List all panes in this domain.
    fn list_panes(&self) -> Vec<Arc<dyn Pane>>;

    /// Remove a pane from the domain.
    fn remove_pane(&self, id: PaneId) -> Option<Arc<dyn Pane>>;
}
