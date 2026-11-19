//! Diagnostic sinks (`MessageHandler`).
//!
//! Log levels plus null and recording handlers. Servants take an
//! optional handler (`None` silences, as callers passing `NULL` do
//! during merges); tests assert on the mock's transcript.

/// Severity (`MessageHandler::MessageType`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MessageLevel {
    /// Informational (`kInfo`).
    Info,
    /// Warning (`kWarning`).
    Warning,
    /// Error (`kError`).
    Error,
    /// Fatal (`kFatal`).
    Fatal,
}

/// Diagnostic sink (`MessageHandler`).
pub trait MessageHandler {
    /// Records a preformatted message (`Message`).
    fn message(&mut self, level: MessageLevel, text: String);
}

/// Discarding sink (`NullMessageHandler`).
#[derive(Debug, Default)]
pub struct NullMessageHandler;

impl MessageHandler for NullMessageHandler {
    fn message(&mut self, _level: MessageLevel, _text: String) {}
}

/// Recording sink (`MockMessageHandler`).
#[derive(Debug, Default)]
pub struct MockMessageHandler {
    messages: Vec<(MessageLevel, String)>,
}

impl MockMessageHandler {
    /// Creates an empty transcript.
    #[must_use]
    pub fn new() -> Self {
        MockMessageHandler::default()
    }

    /// Recorded messages in order.
    #[must_use]
    pub fn messages(&self) -> &[(MessageLevel, String)] {
        &self.messages
    }

    /// Messages above info severity (`SeriousMessages`).
    #[must_use]
    pub fn serious_messages(&self) -> usize {
        self.messages
            .iter()
            .filter(|(level, _)| *level != MessageLevel::Info)
            .count()
    }
}

impl MessageHandler for MockMessageHandler {
    fn message(&mut self, level: MessageLevel, text: String) {
        self.messages.push((level, text));
    }
}
