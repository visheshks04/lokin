use std::error::Error;
use std::fmt::{self, Display, Formatter};
use std::io;

#[derive(Debug)]
pub enum LokinError {
    InvalidInput(String),
    InvalidTransition(String),
    NoActiveSession,
    UnknownSession(String),
    CorruptLog(String),
    UnsupportedSchemaVersion(u16),
    DataDirectoryUnavailable,
    LlmConfiguration(String),
    LlmInference(String),
    InteractiveInput(String),
    Io(io::Error),
    Json(serde_json::Error),
}

impl Display for LokinError {
    fn fmt(&self, formatter: &mut Formatter<'_>) -> fmt::Result {
        match self {
            Self::InvalidInput(message) | Self::InvalidTransition(message) => {
                write!(formatter, "{message}")
            }
            Self::NoActiveSession => write!(formatter, "No active Lokin session."),
            Self::UnknownSession(reference) => {
                write!(formatter, "No session matches '{reference}'.")
            }
            Self::CorruptLog(message) => write!(formatter, "Event log is invalid: {message}"),
            Self::UnsupportedSchemaVersion(version) => {
                write!(formatter, "Unsupported event schema version {version}.")
            }
            Self::DataDirectoryUnavailable => {
                write!(
                    formatter,
                    "Could not determine the local application-data directory."
                )
            }
            Self::LlmConfiguration(message) => {
                write!(formatter, "LLM configuration error: {message}")
            }
            Self::LlmInference(message) => write!(formatter, "LLM inference failed: {message}"),
            Self::InteractiveInput(message) => {
                write!(formatter, "Interactive planning error: {message}")
            }
            Self::Io(error) => write!(formatter, "Storage error: {error}"),
            Self::Json(error) => write!(formatter, "JSON error: {error}"),
        }
    }
}

impl Error for LokinError {
    fn source(&self) -> Option<&(dyn Error + 'static)> {
        match self {
            Self::Io(error) => Some(error),
            Self::Json(error) => Some(error),
            _ => None,
        }
    }
}

impl From<io::Error> for LokinError {
    fn from(error: io::Error) -> Self {
        Self::Io(error)
    }
}

impl From<serde_json::Error> for LokinError {
    fn from(error: serde_json::Error) -> Self {
        Self::Json(error)
    }
}

pub type Result<T> = std::result::Result<T, LokinError>;
