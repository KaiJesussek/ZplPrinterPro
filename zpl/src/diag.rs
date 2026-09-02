//! Non-fatal diagnostics collected while parsing a format.

/// How severe a diagnostic is.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Severity {
    Warning,
    UnknownCommand,
    NotSupported,
}

/// A single diagnostic, e.g. an unknown command encountered mid-format.
#[derive(Debug, Clone)]
pub struct Diagnostic {
    pub severity: Severity,
    /// Command mnemonic, e.g. `^ZZ`.
    pub command: String,
    pub message: String,
    /// Byte offset in the input stream where the diagnostic was raised.
    pub offset: usize,
}

impl Diagnostic {
    /// Build a diagnostic for an unrecognized command, e.g. `^ZZ`.
    pub fn unknown_command(prefix: u8, mnemonic: &[u8], offset: usize) -> Self {
        let mut command = Vec::with_capacity(1 + mnemonic.len());
        command.push(prefix);
        command.extend_from_slice(mnemonic);
        let command = String::from_utf8_lossy(&command).into_owned();
        let message = format!("unknown command {command}");
        Diagnostic {
            severity: Severity::UnknownCommand,
            command,
            message,
            offset,
        }
    }
}

/// A collection of diagnostics from parsing a format.
#[derive(Debug, Default)]
pub struct Diagnostics {
    pub items: Vec<Diagnostic>,
}

impl Diagnostics {
    pub fn push(&mut self, d: Diagnostic) {
        self.items.push(d);
    }

    pub fn is_empty(&self) -> bool {
        self.items.is_empty()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn diagnostics_append_and_iterate() {
        let mut d = Diagnostics::default();
        d.push(Diagnostic::unknown_command(b'^', &[b'Z', b'Z'], 12));
        assert_eq!(d.items.len(), 1);
        assert!(matches!(d.items[0].severity, Severity::UnknownCommand));
        assert_eq!(d.items[0].offset, 12);
    }
}
