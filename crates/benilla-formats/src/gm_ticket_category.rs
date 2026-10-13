//! `GMTicketCategory.dbc`, the ten categories of the Help window's GM list, which
//! `GetGMTicketCategories()` pushes as `(id, name)` pairs in file order; the `TICKET_TYPE1..4`
//! strings feed the OpenTicket dropdown, which 1.12 never shows. The id is the wire value: the
//! clicked button stores it as `HelpFrameOpenTicket.ticketType` and `NewGMTicket` sends it as
//! `CMSG_GMTICKET_CREATE`'s category, so the file's ids are kept, never renumbered.

use anyhow::{Context, Result};
use benilla_dbc::{FieldType, Schema, SchemaField};

use crate::Chain;
use crate::dbc::{parse, str_at, u32_at};

const GM_TICKET_CATEGORY: &str = "DBFilesClient\\GMTicketCategory.dbc";

/// `GMTicketCategory.dbc` in file order, the order `GetGMTicketCategories()` pushes.
pub struct GmTicketCategoryCatalog {
    categories: Vec<GmTicketCategory>,
}

/// One row: the wire category id and its localized display name.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GmTicketCategory {
    /// The id `CMSG_GMTICKET_CREATE` carries.
    pub id: u32,
    /// The button label, from the enUS `Name_Lang` slot.
    pub name: String,
}

impl GmTicketCategoryCatalog {
    /// The rows, in file order.
    pub fn categories(&self) -> &[GmTicketCategory] {
        &self.categories
    }

    pub fn len(&self) -> usize {
        self.categories.len()
    }

    pub fn is_empty(&self) -> bool {
        self.categories.is_empty()
    }
}

fn gm_ticket_category_schema() -> Schema {
    let mut s = Schema::new("GMTicketCategory");
    s.add_field(SchemaField::new("ID", FieldType::UInt32));
    for i in 0..16 {
        s.add_field(SchemaField::new(format!("Name{i}"), FieldType::String));
    }
    s.add_field(SchemaField::new("NameFlags", FieldType::UInt32));
    s
}

/// Load `GMTicketCategory.dbc` from the patch chain. A row with an empty name is skipped, as it
/// would paint a dead button; 5875 has none.
pub fn load_gm_ticket_categories(chain: &mut Chain) -> Result<GmTicketCategoryCatalog> {
    let bytes = chain
        .read_file(GM_TICKET_CATEGORY)
        .with_context(|| format!("reading {GM_TICKET_CATEGORY}"))?;
    let rs = parse(&bytes, gm_ticket_category_schema(), "GMTicketCategory")?;
    let mut categories = Vec::with_capacity(rs.records().len());
    for r in rs.records() {
        let (Some(id), Some(name)) = (u32_at(r, 0), str_at(&rs, r, 1).filter(|n| !n.is_empty()))
        else {
            continue;
        };
        categories.push(GmTicketCategory { id, name });
    }
    Ok(GmTicketCategoryCatalog { categories })
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The 8606 ids (the wire values) in file order (the painted order).
    #[test]
    fn the_shipped_categories_are_the_help_window_rows_in_order() {
        let data = crate::wow_data_or_skip!();
        let mut chain = crate::open_chain(&data).expect("open chain");
        let cat = load_gm_ticket_categories(&mut chain).expect("GMTicketCategory.dbc");

        let rows: Vec<(u32, &str)> = cat
            .categories()
            .iter()
            .map(|c| (c.id, c.name.as_str()))
            .collect();
        assert_eq!(
            rows,
            vec![
                (0, "<not set>"),
                (1, "Stuck"),
                (2, "Behavior/Harassment"),
                (3, "Guild"),
                (4, "Item"),
                (5, "Environmental"),
                (6, "Non-Quest/Creep"),
                (7, "Quest/Quest NPC"),
                (8, "Technical"),
                (9, "Account/Billing"),
                (10, "Character"),
                (11, "Arena/Honor item issues"),
                (12, "Arena/Honor points issues"),
                (13, "Botting/Cheating/Hacking"),
                (14, "Bug report"),
                (15, "Compromised account issue"),
                (16, "Game suggestions"),
                (17, "Gameplay question"),
                (18, "Guild bank issue"),
                (19, "Guild master issue"),
                (20, "Harassment/Scam report"),
                (21, "Inappropriate name Guild/Arena/Character/Pet"),
                (22, "Known issue fix"),
                (23, "Latency/Lag report"),
                (24, "Looting issue/mistake"),
                (25, "Mail issue"),
                (26, "Non in game related inquiry"),
                (27, "Parental controls/CAIS"),
                (28, "PCNC"),
                (29, "PCT"),
                (30, "Restoration status/follow up"),
                (31, "Server/Instance issues"),
                (32, "Spam"),
                (33, "Suicide case"),
                (34, "Suspension questions"),
                (35, "Technical/Sound/Graphics issue"),
                (36, "UI issue"),
            ],
            "the wire ids and the painted order both matter — see the module doc"
        );
        assert_eq!(cat.len(), 37, "the whole shipped table");
    }
}
