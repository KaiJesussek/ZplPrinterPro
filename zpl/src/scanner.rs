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

/// Two-letter mnemonics the parser must recognize: every `^` and `~` command
/// pair from the ZPL II guide's table of contents. The scanner consumes a
/// second alphabetic byte only when the pair names a real command; otherwise
/// the byte stays in the params - so `^AAN` is font A with orientation N
/// (the `^A` mnemonic is one letter), not some `^AA`. FUTURE TABLE ROWS THAT
/// ADD A 2-LETTER COMMAND MUST EXTEND THIS LIST.
static TWO_LETTER: &[&[u8; 2]] = &[
    b"BA", b"BB", b"BC", b"BD", b"BE", b"BF", b"BI", b"BJ", b"BK", b"BL", b"BM", b"BO", b"BP", b"BQ", b"BR", b"BS", b"BT", b"BU", b"BX", b"BY", b"BZ",
    b"CC", b"CD", b"CF", b"CI", b"CM", b"CN", b"CO", b"CP", b"CT", b"CV", b"CW",
    b"DB", b"DE", b"DF", b"DG", b"DN", b"DS", b"DT", b"DU", b"DY",
    b"EG",
    b"FB", b"FC", b"FD", b"FE", b"FH", b"FL", b"FM", b"FN", b"FO", b"FP", b"FR", b"FS", b"FT", b"FV", b"FW", b"FX",
    b"GB", b"GC", b"GD", b"GE", b"GF", b"GS",
    b"HB", b"HD", b"HF", b"HG", b"HH", b"HI", b"HL", b"HM", b"HQ", b"HR", b"HS", b"HT", b"HU", b"HV", b"HW", b"HY", b"HZ",
    b"ID", b"IL", b"IM", b"IS",
    b"JA", b"JB", b"JC", b"JD", b"JE", b"JF", b"JG", b"JH", b"JI", b"JJ", b"JL", b"JM", b"JN", b"JO", b"JP", b"JQ", b"JR", b"JS", b"JT", b"JU", b"JW", b"JX", b"JZ",
    b"KB", b"KC", b"KD", b"KL", b"KN", b"KP", b"KV",
    b"LF", b"LH", b"LL", b"LR", b"LS", b"LT",
    b"MA", b"MC", b"MD", b"MF", b"MI", b"ML", b"MM", b"MN", b"MP", b"MT", b"MU", b"MW",
    b"NB", b"NC", b"ND", b"NI", b"NN", b"NP", b"NR", b"NS", b"NT", b"NW",
    b"PA", b"PF", b"PH", b"PL", b"PM", b"PN", b"PO", b"PP", b"PQ", b"PR", b"PS", b"PW",
    b"RB", b"RF", b"RO", b"RL", b"RS", b"RU", b"RW",
    b"SC", b"SD", b"SE", b"SF", b"SI", b"SL", b"SN", b"SO", b"SP", b"SQ", b"SR", b"SS", b"ST", b"SX", b"SZ",
    b"TA", b"TB", b"TO",
    b"WA", b"WC", b"WD", b"WE", b"WL", b"WP", b"WQ", b"WR", b"WS", b"WX",
    b"XA", b"XB", b"XF", b"XG", b"XS", b"XZ",
    b"ZZ",
];

/// Streaming scanner over a ZPL II byte stream.
///
/// A `^`/`~` (or configured chars) starts a command; the mnemonic is the
/// 1-2 following alphabetic bytes (a second byte is only consumed when it
/// completes a known 2-letter command; single-letter mnemonics pad the
/// second byte with a space); parameters run until the next command char;
/// `Data` is any run containing neither command char.
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

    /// Current scan offset within the input slice. pub(crate): the parse
    /// driver uses it to compute raw-label spans for the server archive
    /// (Task 19).
    pub(crate) fn pos(&self) -> usize {
        self.pos
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
            let params = if mnemonic == *b"CC" || mnemonic == *b"CT" {
                // ^CC/^CT change a prefix character: their parameter is
                // exactly the next byte received (guide p.152/p.166: "the
                // next character received is the new prefix character"),
                // never a parameter run - a run would swallow the rest of
                // the format, whose new prefix differs from the old set
                // (e.g. ^CC/ followed by /FO...).
                if self.pos < self.src.len() {
                    let end = self.pos + 1;
                    let p = &self.src[self.pos..end];
                    self.pos = end;
                    p
                } else {
                    &[]
                }
            } else {
                self.read_until_command(in_field_data)
            };
            Token::Cmd(Cmd {
                prefix,
                mnemonic,
                params,
            })
        } else {
            Token::Data(self.read_until_command(in_field_data))
        })
    }

    /// Whether `b` starts a command under the current prefix characters;
    /// the tilde character is literal inside field data (^FD may contain
    /// it). Single source for both predicates - kept in sync with
    /// [`Self::read_until_command`] by construction (T2 deferred minor,
    /// landed with the ^CC/^CT wiring in T17).
    fn is_command_char(&self, b: u8, in_field_data: bool) -> bool {
        b == self.cmd_char || (!in_field_data && b == self.tilde_char)
    }

    fn begins_command(&self, in_field_data: bool) -> bool {
        self.is_command_char(self.src[self.pos], in_field_data)
    }

    /// Read the mnemonic: the first alphabetic byte always belongs to it; a
    /// second alphabetic byte is consumed only when the pair names a known
    /// 2-letter command, so letter-font syntax (`^AAN`) keeps its extra
    /// letters in the parameters instead of inventing an `^AA` command.
    fn read_mnemonic(&mut self) -> [u8; 2] {
        let mut m = [b' '; 2];
        if self.pos < self.src.len() && self.src[self.pos].is_ascii_alphabetic() {
            m[0] = self.src[self.pos];
            self.pos += 1;
        } else {
            return m;
        }
        if self.pos < self.src.len() && self.src[self.pos].is_ascii_alphabetic()
            && TWO_LETTER.contains(&&[m[0], self.src[self.pos]])
        {
            m[1] = self.src[self.pos];
            self.pos += 1;
        }
        m
    }

    /// Read bytes up to (not including) the next command character.
    fn read_until_command(&mut self, in_field_data: bool) -> &'a [u8] {
        let start = self.pos;
        while self.pos < self.src.len() {
            if self.is_command_char(self.src[self.pos], in_field_data) {
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
    fn two_letter_pairs_require_a_known_command() {
        // ^AA is not a command: letter-font syntax keeps the second byte in
        // the params (font A, orientation N).
        let mut s = Scanner::new(b"^AAN,30,30");
        assert!(matches!(s.next(false), Some(Token::Cmd(c)) if c.mnemonic == [b'A', b' '] && c.params == b"AN,30,30"));
        // FO/FS are real 2-letter commands and are consumed whole.
        let mut s = Scanner::new(b"^FO10^FS");
        assert!(matches!(s.next(false), Some(Token::Cmd(c)) if c.mnemonic == [b'F', b'O'] && c.params == b"10"));
        assert!(matches!(s.next(false), Some(Token::Cmd(c)) if c.mnemonic == [b'F', b'S'] && c.params.is_empty()));
        assert_eq!(s.next(false), None);
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
