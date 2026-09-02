//! Byte-stream scanner for ZPL II.

/// One command token: the prefix character that started it, a two-byte
/// mnemonic (single-letter mnemonics pad the second byte with a space), and
/// the raw parameter bytes that follow until the next command character.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Cmd<'a> {
    pub prefix: u8,
    pub mnemonic: [u8; 2],
    pub params: &'a [u8],
}

/// A token from the input stream: either a command or a run of data bytes.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Token<'a> {
    Cmd(Cmd<'a>),
    Data(&'a [u8]),
}

/// Streaming scanner over a ZPL II byte stream.
///
/// A `^`/`~` (or configured chars) starts a command; the mnemonic is the
/// 1-2 following alphabetic bytes (single-letter mnemonics pad the second
/// byte with a space); parameters run until the next command char; `Data`
/// is any run containing neither command char.
#[derive(Debug)]
pub struct Scanner<'a> {
    src: &'a [u8],
    pos: usize,
    cmd_char: u8,
    tilde_char: u8,
}

impl<'a> Scanner<'a> {
    pub fn new(src: &'a [u8]) -> Self {
        Scanner {
            src,
            pos: 0,
            cmd_char: b'^',
            tilde_char: b'~',
        }
    }

    /// Set the command character (^CC) and tilde character (^CT/^CC variants).
    pub fn set_chars(&mut self, cmd_char: u8, tilde_char: u8) {
        self.cmd_char = cmd_char;
        self.tilde_char = tilde_char;
    }

    /// in_field_data: when true, '~' does NOT start a command (literal data).
    pub fn next(&mut self, in_field_data: bool) -> Option<Token<'a>> {
        if self.pos >= self.src.len() {
            return None;
        }
        Some(if self.begins_command(in_field_data) {
            let prefix = self.src[self.pos];
            self.pos += 1;
            let mnemonic = self.read_mnemonic();
            let params = self.read_until_command(in_field_data);
            Token::Cmd(Cmd {
                prefix,
                mnemonic,
                params,
            })
        } else {
            Token::Data(self.read_until_command(in_field_data))
        })
    }

    fn begins_command(&self, in_field_data: bool) -> bool {
        let b = self.src[self.pos];
        b == self.cmd_char || (!in_field_data && b == self.tilde_char)
    }

    /// Read 1-2 alphabetic mnemonic bytes, padding the second byte with space.
    fn read_mnemonic(&mut self) -> [u8; 2] {
        let mut m = [b' '; 2];
        for slot in m.iter_mut() {
            if self.pos < self.src.len() && self.src[self.pos].is_ascii_alphabetic() {
                *slot = self.src[self.pos];
                self.pos += 1;
            } else {
                break;
            }
        }
        m
    }

    /// Read bytes up to (not including) the next command character.
    fn read_until_command(&mut self, in_field_data: bool) -> &'a [u8] {
        let start = self.pos;
        while self.pos < self.src.len() {
            let b = self.src[self.pos];
            if b == self.cmd_char || (!in_field_data && b == self.tilde_char) {
                break;
            }
            self.pos += 1;
        }
        &self.src[start..self.pos]
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn splits_commands_and_data() {
        let mut s = Scanner::new(b"abc^FO10,20^FSdef");
        assert_eq!(s.next(false), Some(Token::Data(b"abc")));
        assert!(matches!(s.next(false), Some(Token::Cmd(c)) if c.prefix == b'^' && c.mnemonic == [b'F', b'O'] && c.params == b"10,20"));
        assert!(matches!(s.next(false), Some(Token::Cmd(c)) if c.prefix == b'^' && c.mnemonic == [b'F', b'S'] && c.params == b"def"));
        assert_eq!(s.next(false), None);
    }

    #[test]
    fn tilde_is_literal_in_field_data() {
        let mut s = Scanner::new(b"^FD~ABC^FS");
        assert!(matches!(s.next(false), Some(Token::Cmd(_))));
        // in_field_data=true: "~ABC" is data, not a command
        assert_eq!(s.next(true), Some(Token::Data(b"~ABC")));
        assert!(matches!(s.next(false), Some(Token::Cmd(_))));
        assert_eq!(s.next(false), None);
    }

    #[test]
    fn mnemonic_pads_single_letters() {
        let mut s = Scanner::new(b"^A0");
        assert!(matches!(s.next(false), Some(Token::Cmd(c)) if c.mnemonic == [b'A', b' '] && c.params == b"0"));
    }

    #[test]
    fn set_chars_and_empty_params() {
        // ^CC-style command-char change (Task 17) takes effect via set_chars.
        let mut s = Scanner::new(b"!FO10!FS");
        s.set_chars(b'!', b'~');
        assert!(matches!(s.next(false), Some(Token::Cmd(c)) if c.prefix == b'!' && c.mnemonic == [b'F', b'O'] && c.params == b"10"));
        assert!(matches!(s.next(false), Some(Token::Cmd(c)) if c.mnemonic == [b'F', b'S'] && c.params.is_empty()));
        assert_eq!(s.next(false), None);
    }
}
