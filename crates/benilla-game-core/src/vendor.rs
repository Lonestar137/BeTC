//! Converted direct (`npc_vendor`) and reusable (`npc_vendor_template`) inventories for creatures
//! in the base-world spawn pack. The build-8606 `UNIT_NPC_FLAGS` values come from the creature
//! templates; vendor stock is joined through `VendorTemplateId` where present.

use std::sync::OnceLock;

use serde::Deserialize;

use crate::quest::QuestItemTemplate;

const VENDOR_PACK: &str = include_str!("../content/tbc-vendor.toml");
pub const VENDOR_SCHEMA_VERSION: u32 = 1;

/// The converted vendor pack: one row per stock line and the item templates those lines need.
#[derive(Debug, Deserialize)]
pub struct VendorPack {
    pub schema_version: u32,
    pub source: String,
    pub revision: String,
    pub license: String,
    pub extraction_command: String,
    pub vendor: Vec<VendorRow>,
    pub item_template: Vec<QuestItemTemplate>,
}

/// One `npc_vendor` line. `maxcount`/`incrtime` are carried for provenance; the offline slice
/// serves unlimited stock and never restocks (project decision), so only the item and
/// `buy_count` (units delivered per purchase) are consumed.
#[derive(Debug, Clone, Copy, Deserialize)]
pub struct VendorRow {
    pub entry: u32,
    pub item: u32,
    pub maxcount: u32,
    pub incrtime: u32,
    pub buy_count: u32,
}

pub fn vendor_pack() -> &'static VendorPack {
    static PACK: OnceLock<VendorPack> = OnceLock::new();
    PACK.get_or_init(|| {
        let pack: VendorPack =
            toml::from_str(VENDOR_PACK).expect("checked-in TBC vendor pack parses");
        assert_eq!(pack.schema_version, VENDOR_SCHEMA_VERSION);
        pack
    })
}

/// One vendor's stock lines in file order, which is the wire's 1-based display order.
pub fn vendor_items(entry: u32) -> impl Iterator<Item = &'static VendorRow> {
    vendor_pack()
        .vendor
        .iter()
        .filter(move |row| row.entry == entry)
}

pub fn item_template(entry: u32) -> Option<&'static QuestItemTemplate> {
    vendor_pack()
        .item_template
        .iter()
        .find(|item| item.entry == entry)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashSet;

    #[test]
    fn the_vendor_pack_has_provenance_and_resolvable_stock() {
        let pack = vendor_pack();
        assert_eq!(pack.source, "https://github.com/cmangos/tbc-db");
        assert_eq!(pack.license, "GPL-3.0");
        assert_eq!(pack.revision.len(), 40);
        assert!(pack.vendor.len() >= 100);
        assert!(!pack.item_template.is_empty());

        // Every stock line names an item template the pack carries.
        let entries: HashSet<u32> = pack.item_template.iter().map(|item| item.entry).collect();
        for row in &pack.vendor {
            assert!(
                entries.contains(&row.item),
                "vendor row {} sells {} with no converted template",
                row.entry,
                row.item
            );
            assert!(row.buy_count >= 1);
        }
    }

    /// Build-8606 zone vendors: direct Sunstrider stock and Shara's reusable general-goods stock.
    #[test]
    fn direct_and_vendor_template_stock_are_converted() {
        let rows: Vec<_> = vendor_items(15289).collect();
        assert_eq!(rows.len(), 6, "Raelis Dawnstar's six stock lines");
        assert!(rows.iter().all(|row| row.buy_count >= 1));

        let shara: Vec<_> = vendor_items(15287).collect();
        assert_eq!(shara.len(), 4, "Shara Sunwing's reusable stock lines");
        assert!(shara.iter().any(|row| row.item == 159));
        assert!(shara.iter().any(|row| row.item == 2512));
        assert!(shara.iter().any(|row| row.item == 20_857));
        assert!(shara.iter().any(|row| row.item == 25_861));

        let innkeeper: Vec<_> = vendor_items(15433).collect();
        assert_eq!(innkeeper.len(), 12, "Innkeeper Delaniel's food and drink");
        assert!(innkeeper.iter().any(|row| row.item == 159), "spring water");
        assert!(innkeeper.iter().any(|row| row.buy_count == 5), "drink by the five");

        assert_eq!(vendor_items(0).count(), 0, "no creature has entry 0");
    }
}
