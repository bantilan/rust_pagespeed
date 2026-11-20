//! Bounded message buffer (`MockMessageHandler` storage side).
//!
//! Records `(severity, text)` entries and dumps them in the mock
//! handler line format, which the admin readers parse back.

use ps_config::message_handler::MessageLevel;

/// Severity letters (`MessageTypeToString` first letters).
fn severity_letter(level: MessageLevel) -> &'static str {
    match level {
        MessageLevel::Info => "I",
        MessageLevel::Warning => "W",
        MessageLevel::Error => "E",
        MessageLevel::Fatal => "F",
    }
}

/// Severity names (`MessageTypeToString`).
fn severity_name(level: MessageLevel) -> &'static str {
    match level {
        MessageLevel::Info => "Info",
        MessageLevel::Warning => "Warning",
        MessageLevel::Error => "Error",
        MessageLevel::Fatal => "Fatal",
    }
}

/// Bounded in-memory message log.
#[derive(Debug, Default)]
pub struct LogBuffer {
    lines: Vec<String>,
    max_bytes: usize,
    used_bytes: usize,
    enabled: bool,
}

impl LogBuffer {
    /// Creates a buffer with a byte budget (0 = unbounded).
    #[must_use]
    pub fn new(max_bytes: usize) -> Self {
        LogBuffer {
            lines: Vec::new(),
            max_bytes,
            used_bytes: 0,
            enabled: true,
        }
    }

    /// Creates a disabled buffer (`MessageBufferSize 0`): dumps fail.
    #[must_use]
    pub fn disabled() -> Self {
        LogBuffer {
            lines: Vec::new(),
            max_bytes: 0,
            used_bytes: 0,
            enabled: false,
        }
    }

    /// Resizes the budget (`MessageBufferSize` set): zero
    /// disables, trimming oldest-first when shrinking.
    pub fn set_max_bytes(&mut self, max_bytes: usize) {
        self.max_bytes = max_bytes;
        self.enabled = max_bytes > 0;
        if max_bytes > 0 {
            while !self.lines.is_empty() && self.used_bytes > max_bytes {
                if let Some(old) = self.lines.first() {
                    self.used_bytes -= old.len();
                }
                self.lines.remove(0);
            }
        }
    }

    /// Records a message (`MessageSImpl`).
    pub fn message(&mut self, level: MessageLevel, text: String) {
        let line = Self::format_line(level, &text);
        if self.max_bytes > 0 {
            while !self.lines.is_empty() && self.used_bytes + line.len() > self.max_bytes {
                if let Some(old) = self.lines.first() {
                    self.used_bytes -= old.len();
                }
                self.lines.remove(0);
            }
            if line.len() > self.max_bytes {
                return;
            }
            self.used_bytes += line.len();
        }
        self.lines.push(line);
    }

    /// Dumps the buffer (`Dump`).
    #[must_use]
    pub fn dump(&self) -> String {
        self.lines.concat()
    }

    /// Dumps the buffer, failing when disabled (`Dump`).
    #[must_use]
    pub fn dump_checked(&self) -> Option<String> {
        self.enabled.then(|| self.dump())
    }

    /// Formats one dump line.
    fn format_line(level: MessageLevel, text: &str) -> String {
        format!(
            "{}[Wed Jan 01 00:00:00 2014] [{}] [00000] {text}\n",
            severity_letter(level),
            severity_name(level),
        )
    }

    /// Parses a dump into messages (`ParseMessageDumpIntoMessages`):
    /// drops the first (possibly partial) line, splits the rest.
    #[must_use]
    pub fn parse_dump(dump: &str) -> Vec<&str> {
        let rest = match dump.find('\n') {
            Some(pos) => &dump[pos + 1..],
            None => dump,
        };
        if rest.is_empty() {
            Vec::new()
        } else {
            rest.split('\n').filter(|line| !line.is_empty()).collect()
        }
    }

    /// Classifies a message by its leading letter
    /// (`GetMessageType`).
    #[must_use]
    pub fn message_type(message: &str) -> MessageLevel {
        match message.as_bytes().first() {
            Some(b'E') => MessageLevel::Error,
            Some(b'W') => MessageLevel::Warning,
            Some(b'F') => MessageLevel::Fatal,
            _ => MessageLevel::Info,
        }
    }

    /// Strips the type indicator (`ReformatMessage`).
    #[must_use]
    pub fn reformat(message: &str) -> &str {
        message.get(1..).unwrap_or("")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn resize_toggles_and_trims() {
        let mut buffer = LogBuffer::disabled();
        assert_eq!(None, buffer.dump_checked());
        buffer.message(MessageLevel::Info, "aaa".to_string());
        buffer.message(MessageLevel::Info, "bbb".to_string());
        buffer.set_max_bytes(1000);
        assert!(buffer.dump_checked().is_some());
        // Shrinking below the used bytes drops oldest-first.
        buffer.set_max_bytes(1);
        assert!(buffer.dump_checked().is_some());
        buffer.set_max_bytes(0);
        assert_eq!(None, buffer.dump_checked());
    }
}
