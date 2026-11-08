use serde::Serialize;
use sshos_shared::ErrorPayload;

/// Error returned by every Tauri command. Serialized as [`ErrorPayload`].
#[derive(Debug)]
pub struct AppError(pub ErrorPayload);

pub type AppResult<T> = Result<T, AppError>;

impl AppError {
    pub fn msg(m: impl Into<String>) -> Self {
        AppError(ErrorPayload::Message { message: m.into() })
    }
    pub fn needs_terminal(command: impl Into<String>) -> Self {
        AppError(ErrorPayload::NeedsTerminal { command: command.into() })
    }
    pub fn npm_auth(m: impl Into<String>) -> Self {
        AppError(ErrorPayload::NpmAuthRequired { message: m.into() })
    }
}

impl std::fmt::Display for AppError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0.message())
    }
}

impl std::error::Error for AppError {}

impl Serialize for AppError {
    fn serialize<S: serde::Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
        self.0.serialize(s)
    }
}

macro_rules! from_err {
    ($($t:ty),*) => {$(
        impl From<$t> for AppError {
            fn from(e: $t) -> Self { AppError::msg(e.to_string()) }
        }
    )*};
}

from_err!(
    std::io::Error,
    russh::Error,
    russh::keys::Error,
    russh_sftp::client::error::Error,
    serde_json::Error,
    reqwest::Error,
    keyring::Error,
    tauri::Error
);
