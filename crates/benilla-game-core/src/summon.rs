//! The converted summon content pack: what a `SPELL_EFFECT_SUMMON` row summons, and the authored
//! stock-TBC grants that put the warlock demons in the spellbook.
//!
//! The `[[summon]]` rows are converted creature-template identity (name, display, scale, health)
//! from the pinned CMaNGOS TBC source. The pack carries no combat numbers: a pet fights with the
//! creature baseline like every creature in the simulation (`CREATURE_DAMAGE` per
//! `CREATURE_SWING_MS`), a project-owned rule recorded in `RULES.md`.
//!
//! The `[[grant]]` rows are AUTHORED, not converted: stock TBC teaches a warlock its demons (Imp
//! at 1, Voidwalker at 10, Succubus at 20, Felhunter at 30) and the pinned Vengeance-One DB
//! expresses none of those acquisitions in `playercreateinfo_spell`, `npc_trainer`,
//! `npc_trainer_template` or `spell_learn_spell` (verified against the pinned dump). The grants
//! restore the stock spellbook so the class plays as stock TBC.

use std::sync::OnceLock;

use serde::Deserialize;

const SUMMON_PACK: &str = include_str!("../content/tbc-summons.toml");

#[derive(Debug, Deserialize)]
struct SummonPack {
    #[serde(default)]
    summon: Vec<SummonTemplate>,
    #[serde(default)]
    grant: Vec<SummonGrant>,
}

/// A summoned creature's converted identity: what the client draws, what the unit frame shows.
#[derive(Debug, Clone, Deserialize)]
pub struct SummonTemplate {
    pub entry: u32,
    pub name: String,
    pub display_id: u32,
    pub faction: u32,
    pub scale: f32,
    pub creature_type: u32,
    /// `CreatureFamily.dbc` id (the pet tab's family word); 0 for anything without a row.
    pub family: u32,
    pub health: u32,
}

/// One authored spellbook grant (a stock-TBC warlock demon).
#[derive(Debug, Clone, Deserialize)]
pub struct SummonGrant {
    pub race: u8,
    pub class: u8,
    pub spell_id: u32,
    #[allow(dead_code)]
    pub note: String,
}

fn summon_pack() -> &'static SummonPack {
    static PACK: OnceLock<SummonPack> = OnceLock::new();
    PACK.get_or_init(|| toml::from_str(SUMMON_PACK).expect("checked-in summon content must parse"))
}

/// The summoned creature `entry`'s converted identity; `None` for an entry the pack does not
/// convert (an explicit unsupported blocker at cast time, never a silent nothing).
pub fn summon_template(entry: u32) -> Option<&'static SummonTemplate> {
    summon_pack()
        .summon
        .iter()
        .find(|summon| summon.entry == entry)
}

/// The authored summon grants for a race and class (the stock-TBC warlock demon spellbook).
pub fn summon_grants(race: u8, class: u8) -> impl Iterator<Item = u32> + 'static {
    summon_pack()
        .grant
        .iter()
        .filter(move |grant| grant.race == race && grant.class == class)
        .map(|grant| grant.spell_id)
}

/// Every converted summon template, for the presentation's creature-name cache.
pub fn summons() -> impl Iterator<Item = &'static SummonTemplate> {
    summon_pack().summon.iter()
}
