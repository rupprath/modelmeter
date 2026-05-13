#![forbid(unsafe_code)]

use modelmeter_core::logging::redact_if_key;

/// Error type returned by all Tauri commands. Must implement `serde::Serialize`
/// so Tauri can send it to the frontend as a structured error payload.
#[derive(Debug, serde::Serialize)]
pub struct CommandError {
    pub message: String,
}

impl CommandError {
    pub fn new(msg: impl std::fmt::Display) -> Self {
        let raw = msg.to_string();
        // Last-resort safety net: strip any sk-* string that accidentally
        // ended up in an error message before it reaches the frontend.
        let message = redact_if_key(&raw).to_string();
        Self { message }
    }
}

impl From<anyhow::Error> for CommandError {
    fn from(e: anyhow::Error) -> Self {
        // Log the full chain internally; send a redacted copy to the frontend.
        tracing::warn!(error = %e, "command error");
        let message = redact_if_key(&e.to_string()).to_string();
        Self { message }
    }
}

impl From<modelmeter_core::providers::ProviderError> for CommandError {
    fn from(e: modelmeter_core::providers::ProviderError) -> Self {
        // ProviderError::Display is already sanitised (no key material).
        Self { message: e.to_string() }
    }
}

pub type CommandResult<T> = Result<T, CommandError>;
