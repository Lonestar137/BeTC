//! The vanilla patch chain: a priority-ordered set of MPQ archives, read through `benilla-mpq`. A
//! read resolves a name to the highest-priority archive holding it, so a patch wins; base archives
//! carry no `(listfile)`, so resolution is by name hash.

use std::collections::HashSet;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result, anyhow, bail};
use benilla_mpq::Archive;

use crate::TbcInstall;
use crate::VANILLA_BASE_ORDER;

/// One entry from a chain listing: an internal path and its uncompressed size.
pub struct ChainEntry {
    pub name: String,
    pub size: u64,
}

/// A priority-ordered patch chain of MPQ archives (`Send + Sync`; reads are `&self` and lock-free).
pub struct Chain {
    /// Ascending priority: later archives win.
    archives: Vec<Archive>,
    client_build: Option<u32>,
    locale: Option<String>,
    unmounted_archives: Vec<PathBuf>,
}

/// `patch-?.MPQ` as the reference's `FindFirstFileW` glob matches it (template `0x82edbc`, wrapper
/// `0x42ad10`): `?` is exactly one character, any case, so `patch-10.MPQ` never mounts.
fn is_patch_glob_match(name: &str) -> bool {
    let lower = name.to_ascii_lowercase();
    let Some(mid) = lower
        .strip_prefix("patch-")
        .and_then(|rest| rest.strip_suffix(".mpq"))
    else {
        return false;
    };
    mid.chars().count() == 1
}

/// The reference's mount order over a `Data` listing, ascending priority (`0x403740`): the ten
/// [`VANILLA_BASE_ORDER`] archives, `patch.MPQ`, every `patch-?.MPQ` by case-folded name (the
/// reference sorts descending with `strnicmp` and walks backwards), then `speech2.MPQ`. Names
/// match case-insensitively and come back as found on disk.
fn mount_order(dir_names: &[String]) -> Vec<String> {
    let find = |want: &str| {
        dir_names
            .iter()
            .find(|n| n.eq_ignore_ascii_case(want))
            .cloned()
    };
    let mut order: Vec<String> = VANILLA_BASE_ORDER.iter().filter_map(|b| find(b)).collect();
    order.extend(find("patch.MPQ"));
    let mut patches: Vec<String> = dir_names
        .iter()
        .filter(|n| is_patch_glob_match(n))
        .cloned()
        .collect();
    patches.sort_by_key(|n| n.to_ascii_lowercase());
    order.extend(patches);
    order.extend(find("speech2.MPQ"));
    order
}

/// TBC's paired patch layers: each newer global patch is followed by its locale patch, so the
/// locale-specific file wins when both provide the same path.
fn tbc_mount_order(
    data_names: &[String],
    locale_names: &[String],
    locale: &str,
) -> Vec<(bool, String)> {
    let mut order = Vec::new();
    let find = |names: &[String], wanted: &str| {
        names
            .iter()
            .find(|name| name.eq_ignore_ascii_case(wanted))
            .cloned()
    };
    let mut push = |is_locale: bool, names: &[String], wanted: String| {
        if let Some(found) = find(names, &wanted) {
            order.push((is_locale, found));
        }
    };

    push(false, data_names, "common.MPQ".into());
    push(false, data_names, "expansion.MPQ".into());
    for name in [
        format!("base-{locale}.MPQ"),
        format!("locale-{locale}.MPQ"),
        format!("expansion-locale-{locale}.MPQ"),
        format!("speech-{locale}.MPQ"),
        format!("expansion-speech-{locale}.MPQ"),
    ] {
        push(true, locale_names, name);
    }

    let mut versions = std::collections::BTreeSet::new();
    for name in data_names {
        if let Some(version) = patch_version(name, "patch") {
            versions.insert(version);
        }
    }
    for name in locale_names {
        if let Some(version) = patch_version(name, &format!("patch-{locale}")) {
            versions.insert(version);
        }
    }
    for version in versions {
        let root_name = if version == 1 {
            "patch.MPQ".to_string()
        } else {
            format!("patch-{version}.MPQ")
        };
        let locale_name = if version == 1 {
            format!("patch-{locale}.MPQ")
        } else {
            format!("patch-{locale}-{version}.MPQ")
        };
        push(false, data_names, root_name);
        push(true, locale_names, locale_name);
    }
    order
}

fn patch_version(name: &str, stem: &str) -> Option<u32> {
    let lower = name.to_ascii_lowercase();
    let stem = stem.to_ascii_lowercase();
    let rest = lower.strip_prefix(&stem)?.strip_suffix(".mpq")?;
    if rest.is_empty() {
        return Some(1);
    }
    rest.strip_prefix('-')?.parse().ok()
}

impl Chain {
    /// Open a `Data` directory's archives in [`mount_order`], or a single `.MPQ` file.
    ///
    /// Deviation: an archive that fails to open is an error, where the reference logs
    /// `"Failed to open archive"` and goes on, because a skipped corrupt archive surfaces only as
    /// missing files far downstream.
    pub fn open(path: &Path) -> Result<Self> {
        if crate::is_tbc_data_dir(path) {
            let install = crate::tbc_install(path)?;
            return Self::open_tbc(&install);
        }
        let mut archives = Vec::new();
        if path.is_dir() {
            let mut names: Vec<String> = std::fs::read_dir(path)
                .with_context(|| format!("listing {}", path.display()))?
                .filter_map(|entry| {
                    let entry = entry.ok()?;
                    // `path().is_file()` follows symlinks (`read_dir`'s file_type doesn't).
                    entry.path().is_file().then(|| entry.file_name())
                })
                .filter_map(|name| name.into_string().ok())
                .collect();
            // read_dir order is arbitrary; sort so case-variant ties resolve deterministically.
            names.sort();
            for name in mount_order(&names) {
                let mpq = path.join(&name);
                archives.push(
                    Archive::open(&mpq).with_context(|| format!("opening {}", mpq.display()))?,
                );
            }
            if archives.is_empty() {
                bail!("no known vanilla MPQs found in {}", path.display());
            }
        } else {
            archives.push(
                Archive::open(path).with_context(|| format!("opening MPQ {}", path.display()))?,
            );
        }
        Ok(Self {
            archives,
            client_build: None,
            locale: None,
            unmounted_archives: Vec::new(),
        })
    }

    /// Open the recognized build-8606 archives in client precedence order. Unknown MPQs, including
    /// repack backups, are reported but never mounted.
    pub fn open_tbc(install: &TbcInstall) -> Result<Self> {
        let data_names = archive_names(&install.data_dir)?;
        let locale_names = archive_names(&install.locale_dir)?;
        let selected = tbc_mount_order(&data_names, &locale_names, &install.locale);
        let mut archives = Vec::with_capacity(selected.len());
        let mut mounted = HashSet::new();
        for (is_locale, name) in selected {
            let directory = if is_locale {
                &install.locale_dir
            } else {
                &install.data_dir
            };
            let path = directory.join(&name);
            mounted.insert((is_locale, name.to_ascii_lowercase()));
            archives.push(
                Archive::open_with_locale(&path, install.locale_id)
                    .with_context(|| format!("opening TBC archive {}", path.display()))?,
            );
        }
        if archives.is_empty() {
            bail!(
                "no supported TBC MPQs found in {}",
                install.data_dir.display()
            );
        }
        let unmounted_archives = data_names
            .iter()
            .map(|name| (false, name))
            .chain(locale_names.iter().map(|name| (true, name)))
            .filter(|(is_locale, name)| !mounted.contains(&(*is_locale, name.to_ascii_lowercase())))
            .map(|(is_locale, name)| {
                (if is_locale {
                    &install.locale_dir
                } else {
                    &install.data_dir
                })
                .join(name)
            })
            .collect();
        Ok(Self {
            archives,
            client_build: Some(8606),
            locale: Some(install.locale.clone()),
            unmounted_archives,
        })
    }

    pub fn client_build(&self) -> Option<u32> {
        self.client_build
    }

    pub fn locale(&self) -> Option<&str> {
        self.locale.as_deref()
    }

    pub fn mounted_archives(&self) -> impl Iterator<Item = &Path> {
        self.archives.iter().map(Archive::path)
    }

    pub fn unmounted_archives(&self) -> &[PathBuf] {
        &self.unmounted_archives
    }

    /// The highest-priority archive with an entry for `name`, a delete marker included, as a
    /// tombstone shadows every lower copy: check [`Archive::is_delete_marker`] for a readable file.
    fn resolve(&self, name: &str) -> Option<&Archive> {
        self.archives.iter().rev().find(|a| a.contains(name))
    }

    /// Whether `name` (`/` or `\`, any case) is a readable file, not a delete marker.
    pub fn contains(&self, name: &str) -> bool {
        self.resolve(name)
            .is_some_and(|a| !a.is_delete_marker(name))
    }

    /// The path of the archive `name` resolves to, for debugging and extraction.
    pub fn find_file_archive(&self, name: &str) -> Option<&Path> {
        self.resolve(name).map(|a| a.path())
    }

    /// Read a file by internal path (`/` or `\`) from its winning archive.
    pub fn read(&self, name: &str) -> Result<Vec<u8>> {
        let archive = self.archive_for(name)?;
        archive
            .read_file(name)
            .with_context(|| format!("reading {name} from {}", archive.path().display()))
    }

    /// The archive `name` reads from, as a cheap handle: a caller holding the chain behind a lock
    /// looks the file up under it and reads (`Archive::open_file`) after releasing it. No I/O.
    pub fn archive_for(&self, name: &str) -> Result<Archive> {
        let archive = self
            .resolve(name)
            .ok_or_else(|| anyhow!("file not in patch chain: {name}"))?;
        // A tombstone deletes the path from the composite: not found, never a stale lower copy.
        if archive.is_delete_marker(name) {
            bail!(
                "file deleted from patch chain: {name} (tombstoned by {})",
                archive.path().display()
            );
        }
        Ok(archive.clone())
    }

    /// `&mut` alias of [`Chain::read`] for call sites that thread a `&mut Chain`.
    pub fn read_file(&mut self, name: &str) -> Result<Vec<u8>> {
        self.read(name)
    }

    /// The chain's named files with sizes, for development and extraction; a file in no listfile
    /// (most of `texture.MPQ`) is readable by name but not listed. Unions every archive's
    /// `(listfile)`, as each names only its own files; sizes come from the winning archive.
    pub fn list(&self) -> Result<Vec<ChainEntry>> {
        let mut seen = HashSet::new();
        let mut out = Vec::new();
        for archive in &self.archives {
            let Ok(listfile) = archive.read_file("(listfile)") else {
                continue;
            };
            for raw in String::from_utf8_lossy(&listfile).split([';', '\r', '\n']) {
                let name = raw.trim();
                // Dedupe the way MPQ hashing compares names: any case, `/` and `\` alike.
                if name.is_empty() || !seen.insert(name.replace('/', "\\").to_ascii_lowercase()) {
                    continue;
                }
                if let Some(a) = self.resolve(name) {
                    // A tombstoned path is not a file in the composite.
                    if a.is_delete_marker(name) {
                        continue;
                    }
                    out.push(ChainEntry {
                        name: name.to_string(),
                        size: a.file_size(name).unwrap_or(0) as u64,
                    });
                }
            }
        }
        Ok(out)
    }
}

fn archive_names(directory: &Path) -> Result<Vec<String>> {
    let mut names = std::fs::read_dir(directory)
        .with_context(|| format!("listing {}", directory.display()))?
        .filter_map(|entry| {
            let entry = entry.ok()?;
            entry
                .path()
                .is_file()
                .then(|| entry.file_name().into_string().ok())?
        })
        .filter(|name| name.to_ascii_lowercase().ends_with(".mpq"))
        .collect::<Vec<_>>();
    names.sort_by_key(|name| name.to_ascii_lowercase());
    Ok(names)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn owned(names: &[&str]) -> Vec<String> {
        names.iter().map(|n| n.to_string()).collect()
    }

    #[test]
    fn patch_glob_matches_exactly_one_character_case_insensitively() {
        assert!(is_patch_glob_match("patch-2.MPQ"));
        assert!(is_patch_glob_match("patch-3.MPQ"));
        assert!(is_patch_glob_match("PATCH-A.mpq"));
        assert!(!is_patch_glob_match("patch-.MPQ"));
        assert!(!is_patch_glob_match("patch-10.MPQ"));
        assert!(!is_patch_glob_match("patch-33.MPQ"));
        assert!(!is_patch_glob_match("patch.MPQ"));
        assert!(!is_patch_glob_match("patch-2.MPQ.bak"));
        assert!(!is_patch_glob_match("mypatch-2.MPQ"));
    }

    #[test]
    fn mount_order_is_the_carved_law() {
        // base.MPQ is telemetry-only in the reference and never mounts.
        let dir = owned(&[
            "patch-2.MPQ",
            "backup.MPQ",
            "model.MPQ",
            "base.MPQ",
            "dbc.MPQ",
            "patch.MPQ",
            "eula.html",
            "patch-3.MPQ",
            "speech2.MPQ",
            "texture.MPQ",
        ]);
        assert_eq!(
            mount_order(&dir),
            owned(&[
                "dbc.MPQ",
                "texture.MPQ",
                "model.MPQ",
                "patch.MPQ",
                "patch-2.MPQ",
                "patch-3.MPQ",
                "speech2.MPQ",
            ])
        );
    }

    #[test]
    fn patch_sort_is_ascending_and_case_folded() {
        let dir = owned(&["patch-B.MPQ", "patch-3.MPQ", "patch-a.MPQ", "patch-2.MPQ"]);
        assert_eq!(
            mount_order(&dir),
            owned(&["patch-2.MPQ", "patch-3.MPQ", "patch-a.MPQ", "patch-B.MPQ"])
        );
    }

    #[test]
    fn base_archives_are_found_case_insensitively() {
        let dir = owned(&["DBC.mpq", "Model.MPQ", "PATCH.mpq"]);
        assert_eq!(
            mount_order(&dir),
            owned(&["DBC.mpq", "Model.MPQ", "PATCH.mpq"])
        );
    }

    #[test]
    fn tbc_patch_archives_mount_in_numeric_pairs_and_ignore_repack_backups() {
        let data = owned(&[
            "patch-2.MPQ",
            "backup.MPQ",
            "expansion.MPQ",
            "common.MPQ",
            "patch.MPQ",
        ]);
        let locale = owned(&[
            "backup-enGB.MPQ",
            "patch-enGB-2.MPQ",
            "speech-enGB.MPQ",
            "base-enGB.MPQ",
            "locale-enGB.MPQ",
            "patch-enGB.MPQ",
            "expansion-locale-enGB.MPQ",
            "expansion-speech-enGB.MPQ",
        ]);
        assert_eq!(
            tbc_mount_order(&data, &locale, "enGB"),
            vec![
                (false, "common.MPQ".into()),
                (false, "expansion.MPQ".into()),
                (true, "base-enGB.MPQ".into()),
                (true, "locale-enGB.MPQ".into()),
                (true, "expansion-locale-enGB.MPQ".into()),
                (true, "speech-enGB.MPQ".into()),
                (true, "expansion-speech-enGB.MPQ".into()),
                (false, "patch.MPQ".into()),
                (true, "patch-enGB.MPQ".into()),
                (false, "patch-2.MPQ".into()),
                (true, "patch-enGB-2.MPQ".into()),
            ]
        );
    }

    #[test]
    fn supplied_tbc_install_resolves_the_expansion_world_and_localized_dbc() {
        let data = crate::wow_data_or_skip!();
        if !crate::is_tbc_data_dir(&data) {
            crate::skipped("resolved install is not TBC", &[data]);
            return;
        }
        let chain = Chain::open(&data).expect("open the selected build-8606 archives");
        assert_eq!(chain.client_build(), Some(8606));
        assert_eq!(chain.locale(), Some("enGB"));
        assert!(chain.contains("World\\Maps\\Expansion01\\Expansion01.wdt"));
        assert!(chain.contains("DBFilesClient\\Map.dbc"));
        assert!(
            chain
                .read("World\\Maps\\Expansion01\\Expansion01.wdt")
                .is_ok()
        );
        assert!(chain.read("DBFilesClient\\Map.dbc").is_ok());
    }
}
