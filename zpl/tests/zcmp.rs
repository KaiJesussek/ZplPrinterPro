// Eyeball cross-check harness: prints one of our 2D barcode rasters
// (QR/DataMatrix/PDF417/Aztec) as rows of 0/1 for comparing against an
// external reference engine by eye. Prints OUR encoder's output only -
// nothing reference-derived lives here. Usage (env):
// SYM=q|d|p|a DATA=hello STRIP=n cargo test -p zpl --test zcmp -- --nocapture
// Prints SYM w h + rows of 0/1, quiet zone stripped by STRIP on all sides.
use zpl::bc2d;
use zpl::ir::BarcodeField;

fn field() -> BarcodeField {
    // Mirrors the parser defaults; override via env.
    let mut f = BarcodeField {
        x: 0,
        y: 0,
        symbology: b'Q',
        data: String::new(),
        module: 2,
        ratio: 30,
        height: 10,
        line: b'Y',
        interpret: false,
        ucc: false,
        model: b'N',
        check: 0,
        start: b'A',
        stop: b'A',
        qr_ec: b'Q',
        qr_mask: 0, // ^BQ e absent -> mask 0 (the reference's fixed pick, measured corpus 11/16)
        dm_quality: 0,
        dm_format: 6,
        dm_aspect: 1,
        az_level: 0,
        az_menu: false,
        az_append: 1,
        pdf_security: 0,
        pdf_cols: 0,
        pdf_rows: 0,
        pdf_truncate: false,
        font: *b"0N",
        font_height: 0,
        font_width: 0,
        font_name: None,
        rotation: zpl::ir::Rotation::R0,
    };
    if let Ok(v) = std::env::var("EC") {
        f.qr_ec = v.as_bytes()[0];
    }
    if let Ok(v) = std::env::var("MASK") {
        f.qr_mask = v.parse().unwrap();
    }
    if let Ok(v) = std::env::var("SEC") {
        f.pdf_security = v.parse().unwrap();
    }
    if let Ok(v) = std::env::var("COLS") {
        f.pdf_cols = v.parse().unwrap();
    }
    if let Ok(v) = std::env::var("ROWS") {
        f.pdf_rows = v.parse().unwrap();
    }
    if let Ok(v) = std::env::var("AZL") {
        f.az_level = v.parse().unwrap();
    }
    if let Ok(v) = std::env::var("Q") {
        f.dm_quality = v.parse().unwrap();
    }
    if let Ok(v) = std::env::var("ASP") {
        f.dm_aspect = v.parse().unwrap();
    }
    f
}

#[test]
fn zcmp() {
    let sym = std::env::var("SYM").unwrap_or_else(|_| "q".into());
    let hex = std::env::var("HEX").is_ok();
    let data_raw = std::env::var("DATA").unwrap_or_else(|_| "hello".into());
    let data: Vec<u8> = if hex {
        (0..data_raw.len())
            .step_by(2)
            .map(|i| u8::from_str_radix(&data_raw[i..i + 2], 16).unwrap())
            .collect()
    } else {
        data_raw.into_bytes()
    };
    let strip: usize = std::env::var("STRIP")
        .unwrap_or_else(|_| "4".into())
        .parse()
        .unwrap();
    let f = field();
    let res = match sym.as_str() {
        "q" => bc2d::qr(&f, &data),
        "d" => bc2d::datamatrix(&f, &data),
        "p" => bc2d::pdf417(&f, &data),
        "a" => bc2d::aztec(&f, &data),
        _ => panic!("bad sym"),
    };
    let m = res.unwrap_or_else(|e| panic!("encode error: {e}"));
    if let zpl::bc::Pattern::Modules {
        width,
        height,
        data: d,
    } = m
    {
        let w = width as usize;
        let h = height as usize;
        println!("SYM {} {}", w - 2 * strip, h - 2 * strip);
        for y in strip..h - strip {
            let mut row = String::with_capacity(w);
            for x in strip..w - strip {
                let i = y * w + x;
                row.push(if d[i >> 3] & (0x80 >> (i & 7)) != 0 {
                    '1'
                } else {
                    '0'
                });
            }
            println!("{row}");
        }
    }
}
