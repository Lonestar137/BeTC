//! `Cfg_Categories.dbc`, the realm list's category tabs. The reference builds its category set
//! once per process (`0x46e300`, from `CGlueMgr::Initialize`) out of every row whose column 1
//! equals the client Region ([`crate::client_region`]), in file order: column 0 is the id a
//! realm's wire category byte is matched against, and the row's localized name is the tab's
//! label (`GetRealmCategories`, `0x46f238`).

use anyhow::{Context, Result};
use benilla_dbc::{FieldType, Schema, SchemaField};

use crate::Chain;
use crate::dbc::{parse, str_at, u32_at};

const CFG_CATEGORIES: &str = "DBFilesClient\\Cfg_Categories.dbc";

/// One realm category of the client's Region.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RealmCategory {
    /// Column 0, compared for equality with a realm's category byte (`0x46e60b`).
    pub id: u32,
    /// The enUS `Name_Lang` slot, the column the reference reads on an enUS or enGB client.
    pub name: String,
}

/// The 21 columns × 0x54 bytes of build-8606's `Cfg_Categories.dbc`: `ID`, the region/locale mask,
/// two scalars, the sixteen-locale `Name` block and its flags (cmangos-tbc `DBCStructure.h`'s
/// `Cfg_CategoriesEntry`). The name's enUS slot is column 4.
fn cfg_categories_schema() -> Schema {
    let mut s = Schema::new("Cfg_Categories");
    s.add_field(SchemaField::new("ID", FieldType::UInt32));
    s.add_field(SchemaField::new("Region", FieldType::UInt32));
    s.add_field(SchemaField::new("CharsetMask", FieldType::UInt32));
    s.add_field(SchemaField::new("Flags", FieldType::UInt32));
    for i in 0..16 {
        s.add_field(SchemaField::new(format!("Name{i}"), FieldType::String));
    }
    s.add_field(SchemaField::new("NameFlags", FieldType::UInt32));
    s
}

/// The enUS `Name_Lang` slot of build-8606's `Cfg_Categories.dbc`.
const CATEGORY_NAME_FIELD: usize = 4;

/// The categories of `region`, in file order. Build-8606's column 1 is a bitmask: a row joins when
/// bit `region - 1` is set (`0x46e3b1`'s equality test is build-5875's). A `region` of 0, or one
/// past the word, names no category.
pub fn load_realm_categories(chain: &mut Chain, region: u32) -> Result<Vec<RealmCategory>> {
    let Some(bit) = region.checked_sub(1).filter(|b| *b < 32).map(|b| 1u32 << b) else {
        return Ok(Vec::new());
    };
    let bytes = chain
        .read_file(CFG_CATEGORIES)
        .with_context(|| format!("reading {CFG_CATEGORIES}"))?;
    let rs = parse(&bytes, cfg_categories_schema(), "Cfg_Categories")?;
    Ok(rs
        .records()
        .iter()
        .filter(|r| u32_at(r, 1).is_some_and(|mask| mask & bit != 0))
        .filter_map(|r| {
            Some(RealmCategory {
                id: u32_at(r, 0)?,
                // An empty name is pushed as the empty string; only a missing row reads "UNKNOWN".
                name: str_at(&rs, r, CATEGORY_NAME_FIELD).unwrap_or_default(),
            })
        })
        .collect())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn rows(region: u32) -> Option<Vec<(u32, String)>> {
        let data = crate::wow_data_or_skip!(None);
        let mut chain = crate::open_chain(&data).expect("open chain");
        let cats = load_realm_categories(&mut chain, region).expect("Cfg_Categories.dbc");
        Some(cats.into_iter().map(|c| (c.id, c.name)).collect())
    }

    /// The installed `Wow.ini` names Region 3 (enGB); build-8606's column 1 is a bitmask, so the
    /// rows are the five language tabs carrying bit 2.
    #[test]
    fn the_installed_region_is_the_language_tabs() {
        let Some(region) = rows(3) else { return };
        let ids: Vec<u32> = region.iter().map(|(id, _)| *id).collect();
        assert_eq!(ids, [8, 9, 10, 11, 13], "the ids are not contiguous");
        assert_eq!(region[0].1, "English");
        assert_eq!(region[3].1, "Spanish");
        assert_eq!(region[4].1, "Tournament");
    }

    /// A category is matched by its own id, never by position in the list.
    #[test]
    fn a_category_keeps_its_own_id() {
        let Some(region) = rows(3) else { return };
        assert!(region.iter().any(|(id, name)| *id == 8 && name == "English"));
    }

    /// No row carries Region 0, the value a client without a `Wow.ini` Region reads.
    #[test]
    fn region_zero_has_no_categories() {
        let Some(none) = rows(0) else { return };
        assert!(none.is_empty(), "{none:?}");
    }
}
