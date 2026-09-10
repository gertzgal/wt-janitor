use std::fmt;

pub const EXIT_OK: i32 = 0;
pub const EXIT_OPERATIONAL: i32 = 1;
pub const EXIT_USAGE: i32 = 2;
pub const EXIT_DEPENDENCY: i32 = 3;
pub const EXIT_SAFETY: i32 = 4;

/// Base error carrying a process exit code.
#[derive(Debug)]
pub enum Error {
    Usage(String),
    Config(String),
    Dependency(String),
    Safety(String),
    Operational(String),
}

impl Error {
    pub fn exit_code(&self) -> i32 {
        match self {
            Self::Usage(_) | Self::Config(_) => EXIT_USAGE,
            Self::Dependency(_) => EXIT_DEPENDENCY,
            Self::Safety(_) => EXIT_SAFETY,
            Self::Operational(_) => EXIT_OPERATIONAL,
        }
    }

    pub fn usage(msg: impl Into<String>) -> Self {
        Self::Usage(msg.into())
    }

    pub fn config(msg: impl Into<String>) -> Self {
        Self::Config(msg.into())
    }

    pub fn dependency(msg: impl Into<String>) -> Self {
        Self::Dependency(msg.into())
    }

    pub fn safety(msg: impl Into<String>) -> Self {
        Self::Safety(msg.into())
    }

    pub fn operational(msg: impl Into<String>) -> Self {
        Self::Operational(msg.into())
    }
}

impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Usage(m)
            | Self::Config(m)
            | Self::Dependency(m)
            | Self::Safety(m)
            | Self::Operational(m) => f.write_str(m),
        }
    }
}

impl std::error::Error for Error {}

impl From<std::io::Error> for Error {
    fn from(err: std::io::Error) -> Self {
        Self::Operational(err.to_string())
    }
}

pub type Result<T> = std::result::Result<T, Error>;
