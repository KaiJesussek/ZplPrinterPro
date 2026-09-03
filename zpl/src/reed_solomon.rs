//! Reed-Solomon error-correction codecs shared by the 2D barcode encoders
//! (Task 16). Implementation follows the classic Andrews & Arnold encoder
//! (`reedsol.c`, BSD-3-Clause, as bundled with libzint), the reference
//! behind the ISO-compliant encoders of QR, DataMatrix, Aztec and PDF417:
//! a log/alog multiplication table over the field, the generator polynomial
//! prod of(x - a^i) for i = index..index+nsym-1, and the standard LFSR division
//! whose remainder is the transmitted error-correction codewords.
//!
//! Field conventions per standard (verified, see the Task 16 report):
//! - QR (ISO/IEC 18004): GF(2^8), primitive polynomial `0x11d`, index 0.
//! - DataMatrix ECC 200 (ISO/IEC 16022): GF(2^8), `0x12d`, index 1.
//! - Aztec payload (ISO/IEC 24778): GF(2^6) `0x43`, GF(2^8) `0x12d`,
//!   GF(2^1^0) `0x409`, GF(2^1^2) `0x1069` - by codeword size - index 1;
//!   the mode-message descriptor uses GF(2^4) `0x13`, index 1.
//! - PDF417 (ISO/IEC 15438): the prime field GF(929), a = 3, index 1,
//!   with the transmitted codewords **negated** (characteristic != 2 - in
//!   every GF(2^m) above, addition is subtraction so the plain remainder
//!   is already "negative").

use std::fmt;

/// The Galois field of a [`Rs`] codec.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Field {
    /// GF(2^m) given by its primitive polynomial, e.g. `0x11d` for GF(256).
    Binary(u32),
    /// The prime field GF(p) with p prime (PDF417's GF(929), a = 3).
    Prime(u32),
}

impl Field {
    fn order(self) -> usize {
        match self {
            // Degree of the primitive polynomial: 0x11d/0x12d are x^8+...
            // (9 binary digits), 0x409 is x^10+..., 0x1069 x^12+...
            Field::Binary(poly) => (1usize << poly.ilog2()) - 1,
            // Size of the multiplicative group, not the field size: the
            // cyclic group has p-1 elements (PDF417: a = 3 has order 928
            // in GF(929)).
            Field::Prime(p) => p as usize - 1,
        }
    }
}

/// A Reed-Solomon systematic encoder over [`Field`]: `nsym` error
/// correction codewords per block, generator roots a^index..a^(index+nsym-1).
pub struct Rs {
    field: Field,
    nsym: usize,
    log: Vec<u16>,
    /// Doubled table: `alog[i + order] = alog[i]` so a sum of two logs
    /// needs no modulo.
    alog: Vec<u16>,
    /// Generator coefficients (constant term .. x^(nsym-1)); the leading
    /// monic term is implicit. Logs of the coefficients for the LFSR.
    gen: Vec<u16>,
}

impl fmt::Debug for Rs {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "Rs {{ field: {:?}, nsym: {} }}", self.field, self.nsym)
    }
}

impl Rs {
    /// Build the codec. `index` is the exponent of the generator's first
    /// root (QR: 0; DataMatrix/Aztec/PDF417: 1). Errors only on a field
    /// whose codewords cannot hold the values (nsym > order - the
    /// standards never reach that).
    pub fn new(field: Field, nsym: usize, index: usize) -> Result<Rs, String> {
        if nsym == 0 {
            return Err("Reed-Solomon: nsym must be >= 1".into());
        }
        let order = field.order();
        if nsym > order {
            return Err(format!(
                "Reed-Solomon: {nsym} error codewords exceed field order {order}"
            ));
        }
        let alog = make_alog(field, order);
        let mut log = vec![0u16; order + 1];
        for i in 0..order {
            log[alog[i] as usize] = i as u16;
        }
        // Generator polynomial: coefficients low..high of
        // prod of (x - a^i) - in GF(2^m) minus is plus, in GF(p) subtract.
        let mut gen = vec![1u16];
        for i in index..index + nsym {
            let root = alog[i];
            let mut next = vec![0u16; gen.len() + 1];
            for (k, &c) in gen.iter().enumerate() {
                // (x +/- root) * old: c*x^(k+1) +/- c*root*x^k
                next[k + 1] = add(field, next[k + 1], c);
                let t = mul_alog(field, &alog, &log, c, root);
                next[k] = add(field, next[k], if field_is_binary(field) { t } else { neg(field, t) });
            }
            gen = next;
        }
        // Logs of the coefficients, dropping the monic leading term
        // (x^nsym is implicit). A zero coefficient is unreachable
        // (nsym <= order keeps the roots distinct), kept defensive.
        let mut gen_log = vec![0u16; nsym];
        for (k, &c) in gen.iter().take(nsym).enumerate() {
            gen_log[k] = if c == 0 { 0 } else { log[c as usize] };
        }
        Ok(Rs { field, nsym, log, alog, gen: gen_log })
    }

    /// Error-correction codewords for `data`, in transmission order.
    /// For GF(2^m) this is the remainder, highest-degree coefficient
    /// first; for GF(p) (PDF417) the negated remainder, highest-degree
    /// first.
    pub fn encode(&self, data: &[u16]) -> Result<Vec<u16>, String> {
        if data.is_empty() {
            return Err("Reed-Solomon: no data to encode".into());
        }
        let field = self.field;
        let nsym = self.nsym;
        let prime = matches!(field, Field::Prime(_));
        let mut res = vec![0u16; nsym];
        for &d in data {
            let m = add(field, res[nsym - 1], d);
            if m != 0 {
                let lm = self.log[m as usize];
                for k in (1..nsym).rev() {
                    // res[k] = res[k-1] - m*gen[k]; in GF(2^m) minus is
                    // the same as XOR, in GF(p) the product is negated.
                    let t = self.alog[lm as usize + self.gen[k] as usize];
                    let t = if prime { neg(field, t) } else { t };
                    res[k] = add(field, res[k - 1], t);
                }
                let t = self.alog[lm as usize + self.gen[0] as usize];
                res[0] = if prime { neg(field, t) } else { t };
            } else {
                for k in (1..nsym).rev() {
                    res[k] = res[k - 1];
                }
                res[0] = 0;
            }
        }
        res.reverse();
        if let Field::Prime(p) = field {
            for r in &mut res {
                if *r != 0 {
                    *r = p as u16 - *r;
                }
            }
        }
        Ok(res)
    }
}

fn field_is_binary(field: Field) -> bool {
    matches!(field, Field::Binary(_))
}

fn add(field: Field, a: u16, b: u16) -> u16 {
    match field {
        Field::Binary(_) => a ^ b,
        Field::Prime(p) => ((a as u32 + b as u32) % p) as u16,
    }
}

/// a^b when b may be 0 (a zero element): GF(2^m) alog[0] = alog[order] = 1
/// is wrong for 0, so the tables must not be consulted with a zero operand.
fn neg(field: Field, a: u16) -> u16 {
    match field {
        Field::Binary(_) => a, // a XOR a = 0; a is its own inverse
        Field::Prime(p) => (p as u16 - a) % p as u16,
    }
}

/// Multiply two non-assumed-zero field elements via their logs.
fn mul_alog(field: Field, alog: &[u16], log: &[u16], a: u16, b: u16) -> u16 {
    if a == 0 || b == 0 {
        return 0;
    }
    match field {
        Field::Binary(_) => alog[log[a as usize] as usize + log[b as usize] as usize],
        Field::Prime(_) => alog[log[a as usize] as usize + log[b as usize] as usize],
    }
}

/// a^0..a^(order-1), doubled for the no-modulo multiply.
fn make_alog(field: Field, order: usize) -> Vec<u16> {
    let mut alog = vec![0u16; 2 * order + 1];
    if let Field::Prime(p) = field {
        // a = 3 is a primitive root of the PDF417 field GF(929).
        let mut x = 1u32;
        for i in 0..order {
            alog[i] = x as u16;
            alog[order + i] = x as u16;
            x = (x * 3) % p as u32;
        }
        return alog;
    }
    // GF(2^m): x doubles, overflowing bit m wraps by XORing the primitive
    // polynomial (which has its degree-m bit set by construction).
    let m = (order + 1).trailing_zeros();
    let poly = match field {
        Field::Binary(p) => p,
        _ => unreachable!(),
    };
    let mut x = 1u32;
    for i in 0..order {
        alog[i] = x as u16;
        alog[order + i] = x as u16;
        x <<= 1;
        if x & (1u32 << m) != 0 {
            x ^= poly;
        }
    }
    alog
}

#[cfg(test)]
mod tests {
    use super::*;

    /// DataMatrix ECC 200: GF(256) with primitive polynomial 0x12d,
    /// generator index 1 - the ISO/IEC 16022 convention. Data "hello" in
    /// ASCII encodation (105, 102, 109, 109, 112) with 7 EC codewords
    /// (the 12x12 symbol) produces the reference remainder below
    /// (independent Python implementation of the same convention).
    #[test]
    fn dm_rs_known_vector() {
        let rs = Rs::new(Field::Binary(0x12d), 7, 1).unwrap();
        let ec = rs.encode(&[105, 102, 109, 109, 112]).unwrap();
        assert_eq!(
            ec,
            vec![0x7F, 0x65, 0xA3, 0x16, 0x45, 0x7B, 0x54]
        );
    }

    /// QR: GF(256) 0x11d, generator index 0 (ISO/IEC 18004). Version 1-M
    /// "hello": 16 data codewords (0x40 0x05 + bytes + 9 pad codewords)
    /// with 10 EC codewords.
    #[test]
    fn qr_rs_known_vector() {
        let rs = Rs::new(Field::Binary(0x11d), 10, 0).unwrap();
        let data = [
            0x40, 0x05, b'h', b'e', b'l', b'l', b'o', 0xEC, 0x11, 0xEC,
            0x11, 0xEC, 0x11, 0xEC, 0x11, 0xEC,
        ];
        let ec = rs.encode(&data.map(|b| b as u16)).unwrap();
        assert_eq!(
            ec,
            vec![0x23, 0x14, 0x93, 0x88, 0xA3, 0x32, 0x4C, 0x59, 0x58, 0x3B]
        );
    }

    /// PDF417: the prime field GF(929) (a = 3), index 1, transmitted
    /// codewords negated. Verify the transmission is a valid systematic
    /// codeword: the received polynomial must vanish at every generator
    /// root 3^1..3^k.
    #[test]
    fn pdf417_syndromes_vanish() {
        let data = [4u16, 901, 65, 66]; // SD + byte-latch stream
        for (nsym, expect) in [
            (2usize, vec![290, 864]),
            (8, vec![923, 706, 614, 784, 479, 707, 69, 795]),
        ] {
            let rs = Rs::new(Field::Prime(929), nsym, 1).unwrap();
            let ec = rs.encode(&data).unwrap();
            assert_eq!(ec, expect, "nsym {nsym}");
            for i in 1..=nsym {
                // root = 3^i mod 929; evaluate C(root) = 0.
                let mut root = 1u64;
                for _ in 0..i {
                    root = root * 3 % 929;
                }
                let mut acc = 0u64; // Horner, highest degree first
                for &c in data.iter().chain(ec.iter()) {
                    acc = (acc * root + c as u64) % 929;
                }
                assert_eq!(acc, 0, "syndrome at root 3^{i}");
            }
        }
    }
}

#[cfg(test)]
mod oracle_tests {
    //! Cross-checked against the zint reference encoder's output for
    //! byte-mode PDF417 payloads (level 0: 2 EC codewords).
    use super::*;

    #[test]
    fn pdf417_ec_matches_zint() {
        let rs = Rs::new(Field::Prime(929), 2, 1).unwrap();
        // zint "Complete CW string":        SD   latch bytes
        // vector 1 (payload 0xC3 0xB6):   [4, 901, 195, 182]  -> EC [410, 595]
        assert_eq!(rs.encode(&[4, 901, 195, 182]).unwrap(), vec![410, 595]);
        // vector 2 (zint's stream for 0xC3 0xB6 0x41 0x42, with its
        // 913-split): [6, 913, 195, 913, 182, 1] -> EC [594, 559]
        assert_eq!(
            rs.encode(&[6, 913, 195, 913, 182, 1]).unwrap(),
            vec![594, 559]
        );
        // The Annex F tables are reproduced by the generator
        // construction: prod of(x - 3^i) for i = 1..k.
        let rs4 = Rs::new(Field::Prime(929), 4, 1).unwrap();
        // constant..x^3 coefficients of the k = 4 generator:
        // expected [522, 568, 723, 809] (Table F.2) - decoded back from
        // the stored logs:
        let mut got = Vec::new();
        for &lg in &rs4.gen {
            got.push(rs4.alog[lg as usize] as u32);
        }
        assert_eq!(got, vec![522, 568, 723, 809]);
    }
}

