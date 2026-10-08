//! Audit every path in one archive's `(listfile)` without extracting or modifying client data.
//!
//! ```text
//! cargo run -p benilla-mpq --example mpqaudit -- /path/to/Data/common.MPQ [locale-id]
//! ```

use std::collections::{BTreeMap, HashSet};
use std::path::Path;

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let mut args = std::env::args().skip(1);
    let path = args
        .next()
        .ok_or("usage: mpqaudit <archive.MPQ> [locale-id]")?;
    let locale_id = args.next().map_or(Ok(0), |value| {
        value
            .strip_prefix("0x")
            .map_or_else(|| value.parse(), |hex| u16::from_str_radix(hex, 16))
    })?;
    let archive = benilla_mpq::Archive::open_with_locale(&path, locale_id)?;
    let listfile = archive.read_file("(listfile)")?;
    let names = String::from_utf8_lossy(&listfile);
    let files = names
        .lines()
        .map(str::trim)
        .filter(|name| !name.is_empty())
        .map(str::to_owned)
        .collect::<HashSet<_>>();
    let mut failures = BTreeMap::<&'static str, (usize, String)>::new();
    let mut tombstones = 0usize;

    for name in &files {
        if let Err(error) = archive.read_file(name) {
            if archive.is_delete_marker(name) {
                tombstones += 1;
                continue;
            }
            let category = match &error {
                benilla_mpq::Error::Io(_) => "io",
                benilla_mpq::Error::NotMpq => "not-mpq",
                benilla_mpq::Error::NotFound(_) => "not-found",
                benilla_mpq::Error::Unsupported(_) => "unsupported",
                benilla_mpq::Error::Decompress(_) => "decompress",
                benilla_mpq::Error::Corrupt(_) => "corrupt",
            };
            let row = failures
                .entry(category)
                .or_insert((0, format!("{name}: {error}")));
            row.0 += 1;
        }
    }

    println!("archive: {}", Path::new(&path).display());
    println!("locale: {locale_id:#06x}");
    println!("listfile entries: {}", files.len());
    println!("delete markers: {tombstones}");
    println!(
        "readable: {}",
        files.len() - failures.values().map(|(n, _)| n).sum::<usize>()
    );
    for (category, (count, example)) in failures {
        println!("{category}: {count} (example: {example})");
    }
    Ok(())
}
