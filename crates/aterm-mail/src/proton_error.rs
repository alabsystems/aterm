// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0
//! Credential-free Proton failures and conservative recovery decisions.

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Kind {
    Network,
    Timeout,
    AccessDenied,
    Configuration,
    Protocol,
    Credentials,
    SessionExpired,
    Verification,
    DeviceVerification,
    Scope,
    AccountRestricted,
    RateLimited,
    Service,
    ClientRejected,
    Refused,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Recovery {
    /// A caller may retry a read after a delay; never replay a send automatically.
    RetryRead,
    Reauthenticate,
    Verify,
    Stop,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Operation {
    AuthInfo,
    SignIn,
    Refresh,
    TwoFactor,
    Addresses,
    AccountKeys,
    KeySalts,
    RecipientKeys,
    Messages,
    Other,
}

impl std::fmt::Display for Operation {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(match self {
            Self::AuthInfo => "authentication parameters",
            Self::SignIn => "authentication",
            Self::Refresh => "session refresh",
            Self::TwoFactor => "two-factor authentication",
            Self::Addresses => "account addresses",
            Self::AccountKeys => "account keys",
            Self::KeySalts => "key salts",
            Self::RecipientKeys => "recipient keys",
            Self::Messages => "mail messages",
            Self::Other => "mail request",
        })
    }
}

/// Only allowlisted operation names are retained, never URLs, bodies or tokens.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Failure {
    pub operation: Operation,
    pub kind: Kind,
    pub http_status: Option<u16>,
    pub api_code: Option<u64>,
    pub retry_after_seconds: Option<u64>,
    pub retry_after_unparsed: bool,
    pub phase: Option<aterm_http::Phase>,
}

impl Failure {
    pub(crate) fn new(path: &str, kind: Kind) -> Self {
        Self {
            operation: match path {
                "/auth/v4/info" => Operation::AuthInfo,
                "/auth/v4" => Operation::SignIn,
                "/auth/v4/refresh" => Operation::Refresh,
                "/auth/v4/2fa" => Operation::TwoFactor,
                "/core/v4/addresses" => Operation::Addresses,
                "/core/v4/users" => Operation::AccountKeys,
                "/core/v4/keys/salts" => Operation::KeySalts,
                p if p.starts_with("/core/v4/keys?") => Operation::RecipientKeys,
                p if p.starts_with("/mail/v4/messages") => Operation::Messages,
                _ => Operation::Other,
            },
            kind,
            http_status: None,
            api_code: None,
            retry_after_seconds: None,
            retry_after_unparsed: false,
            phase: None,
        }
    }

    pub(crate) fn response(path: &str, status: u16, code: u64, retry: Option<&str>) -> Self {
        // Explicit API errors take precedence over a generic HTTP status.
        let kind = match code {
            8002 => Kind::Credentials,
            9001 => Kind::Verification,
            9002 => Kind::DeviceVerification,
            9100 => Kind::Scope,
            10003 | 12100 | 10403 | 10404 => Kind::AccountRestricted,
            2064 | 5003 => Kind::ClientRejected,
            401 | 10013 => Kind::SessionExpired,
            _ if status == 429 => Kind::RateLimited,
            _ if status == 401 => Kind::SessionExpired,
            _ if status >= 500 => Kind::Service,
            0 if (200..300).contains(&status) => Kind::Protocol,
            _ => Kind::Refused,
        };
        let mut failure = Self::new(path, kind);
        failure.http_status = Some(status);
        failure.phase = Some(aterm_http::Phase::Answered);
        failure.api_code = Some(code);
        let retry = retry.map(str::trim).filter(|value| !value.is_empty());
        failure.retry_after_seconds = retry
            .filter(|value| value.bytes().all(|b| b.is_ascii_digit()))
            .and_then(|value| value.parse().ok());
        failure.retry_after_unparsed = retry.is_some() && failure.retry_after_seconds.is_none();
        failure
    }

    pub(crate) fn transport(path: &str, error: &aterm_http::SendError) -> Self {
        let kind = match &error.error {
            aterm_http::Error::Invalid(_) => Kind::Configuration,
            aterm_http::Error::Protocol(_) | aterm_http::Error::TooLarge { .. } => Kind::Protocol,
            aterm_http::Error::Io(error) => match error.kind() {
                std::io::ErrorKind::TimedOut => Kind::Timeout,
                std::io::ErrorKind::PermissionDenied => Kind::AccessDenied,
                std::io::ErrorKind::InvalidData | std::io::ErrorKind::InvalidInput => {
                    Kind::Protocol
                }
                _ => Kind::Network,
            },
        };
        let mut failure = Self::new(path, kind);
        failure.phase = Some(error.phase);
        failure
    }

    pub fn recovery(&self) -> Recovery {
        // An unparsed server deadline is never permission to retry early.
        if self.retry_after_unparsed {
            return Recovery::Stop;
        }
        match self.kind {
            Kind::Network | Kind::Timeout | Kind::RateLimited | Kind::Service => {
                Recovery::RetryRead
            }
            Kind::SessionExpired => Recovery::Reauthenticate,
            Kind::Verification | Kind::DeviceVerification => Recovery::Verify,
            _ => Recovery::Stop,
        }
    }

    pub fn rejects_refresh_token(&self) -> bool {
        self.kind == Kind::SessionExpired && self.operation == Operation::Refresh
    }
}

impl std::fmt::Display for Failure {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let reason = match self.kind {
            Kind::Network => "Could not reach Proton Mail",
            Kind::Timeout => "Proton request timed out",
            Kind::AccessDenied => "Mail network access was denied locally",
            Kind::Configuration => "Mail transport configuration is invalid",
            Kind::Protocol => "Proton returned an invalid response",
            Kind::Credentials => "Proton rejected the account credentials",
            Kind::SessionExpired => "Proton session expired or was revoked",
            Kind::Verification => "Proton requires verification for this session",
            Kind::DeviceVerification => "Proton requires device verification",
            Kind::Scope => "Proton session lacks permission for this operation",
            Kind::AccountRestricted => "Proton restricted this account or application",
            Kind::RateLimited => "Proton asked the client to wait",
            Kind::Service => "Proton service is unavailable",
            Kind::ClientRejected => "Proton rejected this client's application or version",
            Kind::Refused => "Proton refused the request",
        };
        write!(f, "{reason} during {}", self.operation)?;
        if let Some(status) = self.http_status {
            write!(f, " (HTTP {status}")?;
            if let Some(code) = self.api_code {
                write!(f, ", code {code}")?;
            }
            write!(f, ")")?;
        }
        if let Some(seconds) = self.retry_after_seconds {
            write!(f, "; retry after {seconds} seconds")?;
        }
        Ok(())
    }
}

impl std::error::Error for Failure {}

#[cfg(test)]
#[path = "proton_error_tests.rs"]
mod tests;
