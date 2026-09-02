//! Parse errors produced by the ZPL scanner/parser.

/// Fatal errors encountered while parsing a ZPL II stream.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ParseError {
    /// `^XA` was seen but the format was never closed with `^XZ`.
    TruncatedFormat,
    /// A command parameter is malformed or out of range.
    InvalidParam { cmd: String, detail: String },
    /// A `^FH` escape sequence is malformed.
    BadEscapes { cmd: String },
}

impl std::fmt::Display for ParseError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            ParseError::TruncatedFormat => write!(f, "format started with ^XA but never closed with ^XZ"),
            ParseError::InvalidParam { cmd, detail } => {
                write!(f, "invalid parameter for {cmd}: {detail}")
            }
            ParseError::BadEscapes { cmd } => write!(f, "malformed ^FH escape in {cmd}"),
        }
    }
}

impl std::error::Error for ParseError {}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn error_displays() {
        let e = ParseError::TruncatedFormat;
        assert!(!e.to_string().is_empty());
    }
}
