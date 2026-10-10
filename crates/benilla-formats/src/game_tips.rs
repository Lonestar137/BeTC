//! `GameTips.dbc`: the loading-screen tips, each with its own `|cffffd100Tip:|r` prefix and a
//! trailing `\r\n` in the data; FrameXML never reads them. The reference loads them in file order
//! into one array (`[0xc0dcd0]`, count `[0xc0dcd4]`, loader `0x545f90`), and
//! `CGlueMgr::EnterWorld` (`0x46b500`) stores the chosen record index, never an id, in the
//! `gameTip` CVar. Record order is not id order: the file opens on id 396.

use anyhow::{Context, Result};
use benilla_dbc::{FieldType, Schema, SchemaField};

use crate::chain::Chain;
use crate::dbc::{parse, str_at};

const GAME_TIPS: &str = "DBFilesClient\\GameTips.dbc";

/// The tips in file record order, the order the persisted `gameTip` index counts through.
#[derive(Clone, Debug, Default)]
pub struct GameTipsCatalog {
    tips: Vec<String>,
}

impl GameTipsCatalog {
    /// A catalog over given tips, for a test without an install.
    pub fn from_tips(tips: Vec<String>) -> Self {
        GameTipsCatalog { tips }
    }

    /// The tip at a record index; an index saved under another chain can fall past the table.
    pub fn get(&self, index: usize) -> Option<&str> {
        self.tips.get(index).map(String::as_str)
    }

    pub fn len(&self) -> usize {
        self.tips.len()
    }

    pub fn is_empty(&self) -> bool {
        self.tips.is_empty()
    }
}

/// `GameTips.dbc`: 10 fields in 5875 and 18 in build 8606 (16 localized strings + flags).
fn schema(tbc: bool) -> Schema {
    let mut s = Schema::new("GameTips");
    s.add_field(SchemaField::new("ID", FieldType::UInt32));
    s.add_field(SchemaField::new_array(
        "Text",
        FieldType::String,
        if tbc { 16 } else { 8 },
    ));
    s.add_field(SchemaField::new("TextMask", FieldType::UInt32));
    s.set_key_field("ID");
    s
}

pub fn load_game_tips(chain: &mut Chain) -> Result<GameTipsCatalog> {
    let tbc = chain.client_build() == Some(8606);
    let bytes = chain.read_file(GAME_TIPS).context("reading GameTips.dbc")?;
    let rs = parse(&bytes, schema(tbc), "GameTips")?;
    let mut tips = Vec::with_capacity(rs.records().len());
    for r in rs.records() {
        // A row with an empty text slot is dropped, not shown blank; the shipped file has none.
        match str_at(&rs, r, 1) {
            Some(text) if !text.trim().is_empty() => tips.push(text),
            _ => {}
        }
    }
    Ok(GameTipsCatalog { tips })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn empty_dbc(field_count: u32) -> Vec<u8> {
        let mut bytes = b"WDBC".to_vec();
        for value in [0, field_count, field_count * 4, 1] {
            bytes.extend_from_slice(&value.to_le_bytes());
        }
        bytes.push(0);
        bytes
    }

    #[test]
    fn schema_tracks_classic_and_tbc_locale_widths() {
        for (tbc, fields) in [(false, 10), (true, 18)] {
            let bytes = empty_dbc(fields);
            parse(&bytes, schema(tbc), "GameTips fixture").unwrap_or_else(|error| {
                panic!("build TBC={tbc} schema should accept {fields} fields: {error}")
            });
        }
    }

    #[test]
    fn the_real_table_loads_tips_in_file_order() {
        let data = crate::wow_data_or_skip!();
        let mut chain = crate::open_chain(&data).expect("open chain");
        let tbc = chain.client_build() == Some(8606);
        let cat = load_game_tips(&mut chain).expect("load GameTips");
        assert!(!cat.is_empty(), "the install has loading-screen tips");
        assert!(cat.get(0).is_some(), "the first record is readable");
        assert!(cat.get(cat.len()).is_none(), "one past the end is None");
        if !tbc {
            assert_eq!(cat.len(), 74, "the 5875 enGB chain ships 74 tips");
            assert!(
                cat.get(0).unwrap().starts_with("|cffffd100"),
                "the colour prefix is authored INTO the data, not added by the renderer: {:?}",
                cat.get(0)
            );
            assert!(
                cat.get(0).unwrap().contains("mini-map"),
                "record order, not id order — record 0 is id 396: {:?}",
                cat.get(0)
            );
        }
    }
}
