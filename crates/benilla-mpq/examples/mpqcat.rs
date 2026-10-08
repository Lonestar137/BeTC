//! `mpqcat`: write one file out of an MPQ to stdout, to read the client's own data (FrameXML,
//! GlobalStrings, a BLP header).
//!
//! ```text
//! cargo run -q -p benilla-mpq --example mpqcat -- <archive.MPQ> 'Interface\FrameXML\UIParent.lua' [locale-id]
//! ```
//!
//! Names are the client's backslash paths. A miss is an error, not empty output: the same file
//! often lives in several archives (patch over interface over base), and "not here" must read
//! apart from "here but empty".
use std::io::Write;

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let mut args = std::env::args().skip(1);
    let (Some(archive), Some(name)) = (args.next(), args.next()) else {
        eprintln!("usage: mpqcat <archive.MPQ> <file-in-archive> [locale-id]");
        std::process::exit(2);
    };
    let locale_id = args.next().map_or(Ok(0), |value| {
        value
            .strip_prefix("0x")
            .map_or_else(|| value.parse(), |hex| u16::from_str_radix(hex, 16))
    })?;
    let bytes = benilla_mpq::Archive::open_with_locale(&archive, locale_id)?.read_file(&name)?;
    std::io::stdout().write_all(&bytes)?;
    Ok(())
}
