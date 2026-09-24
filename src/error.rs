//! Errors returned to the server as tool-level errors.
//!
//! Every code here is part of the wire protocol (see PROTOCOL.md).

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ErrorCode {
    /// Arguments are missing, malformed, or out of range.
    InvalidArgument,
    /// The path is not of the form `root_id:relative/path`, or contains an
    /// absolute path, `..`, a NUL byte, a backslash, or a drive letter.
    InvalidPath,
    /// No root with that ID is shared.
    UnknownRoot,
    /// Resolution would leave the root (through a symlink or otherwise).
    OutsideRoot,
    /// The path matches the deny list, is a symlink that is not followed, is a
    /// hard link, or is not a regular file or directory.
    Denied,
    NotFound,
    NotAFile,
    NotADirectory,
    /// The file is larger than the configured limit.
    TooLarge,
    /// The file type can't be turned into text.
    Unsupported,
    /// A rate or volume limit was hit. Retrying later may work.
    RateLimited,
    /// The user paused the agent.
    Paused,
    Internal,
}

impl ErrorCode {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::InvalidArgument => "invalid_argument",
            Self::InvalidPath => "invalid_path",
            Self::UnknownRoot => "unknown_root",
            Self::OutsideRoot => "outside_root",
            Self::Denied => "denied",
            Self::NotFound => "not_found",
            Self::NotAFile => "not_a_file",
            Self::NotADirectory => "not_a_directory",
            Self::TooLarge => "too_large",
            Self::Unsupported => "unsupported",
            Self::RateLimited => "rate_limited",
            Self::Paused => "paused",
            Self::Internal => "internal",
        }
    }

    /// Whether the audit log records this as a policy denial rather than an
    /// ordinary error.
    pub fn is_denial(self) -> bool {
        matches!(
            self,
            Self::InvalidPath
                | Self::UnknownRoot
                | Self::OutsideRoot
                | Self::Denied
                | Self::RateLimited
                | Self::Paused
        )
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, thiserror::Error)]
#[error("{code:?}: {message}")]
pub struct ToolError {
    pub code: ErrorCode,
    pub message: String,
}

impl ToolError {
    pub fn new(code: ErrorCode, message: impl Into<String>) -> Self {
        Self {
            code,
            message: message.into(),
        }
    }

    pub fn invalid_argument(message: impl Into<String>) -> Self {
        Self::new(ErrorCode::InvalidArgument, message)
    }

    pub fn invalid_path(message: impl Into<String>) -> Self {
        Self::new(ErrorCode::InvalidPath, message)
    }

    pub fn denied(message: impl Into<String>) -> Self {
        Self::new(ErrorCode::Denied, message)
    }

    pub fn internal(message: impl Into<String>) -> Self {
        Self::new(ErrorCode::Internal, message)
    }
}
