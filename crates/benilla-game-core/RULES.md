# Initial Offline Gameplay Rules

Schema version: 2.

These are project-owned rules and imported TBC creature behavior. Fireball's name, icon, range, cast
time, and mana cost are read from the installed build's Spell.dbc; creature display models are
resolved from the installed client tables. Creature templates, spawn positions, waypoint paths and
idle addon state come from the pinned source recorded in the local content pack.

## Starting Placement

New characters use race-specific project-authored anchors in their TBC starting areas. Human
placement uses the Northshire position already established in this repository's build-8606 world
probes. The other race positions are authored area anchors and should be visually checked against
build 8606 before expanding their content. Saved character positions always take precedence.

## Sunstrider First Encounter

- Sunstrider Isle uses its converted source population at source spawn positions. Basic combat state
  is created for imported creatures whose build-8606 `FactionTemplate.dbc` row is not friendly
  toward the Blood Elf player template 1610. Hostile templates in the map-530 pack are 12, 14, 16,
  38, 91 and 114; neutral templates 7, 15, 188, 189 and 1703 are also attackable, matching the
  client's `CanAttack` behavior for neutral units. Friendly templates are not combat targets even
  though they share the same unit presentation. The legacy practice combat harness is used only
  outside map 530.
- Imported visuals use display IDs resolved against the installed build-8606 creature catalog.
- Each imported combat target has 60 health and respawns 30,000 simulation milliseconds after death;
  combat state and updates retain its converted `source_guid` identity.
- Hostile faction templates 12, 14, 16, 38, 91, and 114 acquire within 20 yards. Neutral but
  attackable templates do not acquire by proximity, but retaliate when attacked. Friendly templates
  never enter combat. Acquired creatures pursue directly at 7 yards per second, attack inside 2
  yards for 5 physical damage every 2,000 simulation milliseconds, and evade/return to their spawn
  when the player exceeds a 40-yard leash. This first pass has no collision or line-of-sight query.
- New characters start with 100 health. Mana-using classes have a project-sized 100-mana pool;
  class-stat-derived pool sizes are not implemented yet. Current health and the pools are saved
  with the character, including zero health, and restored on world entry. Creature damage updates
  the health descriptor and combat stops at zero. Choosing the stock "Release Spirit" action
  performs the simplified offline recovery: restore health and the class pools, move to the race
  starting anchor, and return living creatures to their spawn points. There is no ghost/corpse
  run, durability or XP penalty.

## Power Pools And Class Mechanics

Snapshot schema 9 adds rage, energy and focus beside mana. Which pool a class runs on follows TBC:
warriors rage, rogues energy, every other class mana (a hunter pet's focus comes with the summon,
not the player). Druid form-driven pools are not modeled yet. Costs come from the build-8606
`Spell.dbc` `powerType`/`manaCost` pair in the spell's own unit: rage is stored tenfold in the row
("150 on the wire is 15 Rage") and divided here, energy and mana spend as written, and a negative
`powerType` (a health cost, Life Tap and Health Funnel) is an explicit unsupported blocker until
health costs are modeled. `ManaCostPercentage` counts against base mana only for mana spells.

- Energy regenerates 10 points every 2,000 simulation ms whether or not the character is casting.
- Mana regenerates 5 points every 1,000 simulation ms toward the class pool, paused while a spell
  is casting (the five-second rule's cast half). Mana is charged when a cast starts and is not
  refunded on interruption.
- Rage is generated from melee damage at the classic-era conversion value
  `R = 0.0091107836·level² + 3.225598133·level + 4.2652911`: damage dealt is worth `7.5/R` rage,
  damage taken `2.5/R`, at least 1 point for any hit, truncated to whole points. The source is the
  community-documented classic formula (no client data exposes it), so this is a project-owned
  rule. Rage decays 1 point every 2,000 simulation ms while no creature is engaged and the player
  is not swinging; it holds during a fight. The pool caps at 100 whole points, and the descriptor
  carries tenths (the client divides on read).
- Level-ups restore health and the class's pools to maximum, matching the established level rule.

## Melee, Weapons, And Combo Points

- A swing deals the equipped main hand's converted weapon damage (`item_template` `dmg_min1`/
  `dmg_max1`, rounded to whole points) at its converted speed (`delay` in ms). The converted
  values come from the pinned CMaNGOS TBC source; the 2.4.3 client exposes no weapon damage
  (its `Item.dbc` is display data only). An empty hand or a statless item swings the project
  baseline, 5 damage every 2,000 ms.
- Weapon-damage abilities (Heroic Strike 78's `SPELL_EFFECT_WEAPON_DAMAGE_NOSCHOOL`, Sinister
  Strike 1752's `SPELL_EFFECT_NORMALIZED_WEAPON_DMG`, Backstab 53's percent arm) resolve inside
  melee reach whatever the row's range reads, dealing the weapon roll scaled by the percent arm
  (`EffectBasePoints + 1`, 100 without one) plus the flat roll (`EffectBasePoints + 1` bounds).
  Normalized weapon damage uses the same roll for now; the normalization speed rules are not
  modeled.
- An on-next-swing ability (`Attributes & 0x404`, Heroic Strike and Cleave) queues on the swing
  timer and replaces exactly one swing's damage with its own resolved amount; the queue carries
  its pre-rolled outcome and its cost is spent on queue. A white swing follows the ability swing.
- Rogue builders (`SPELL_EFFECT_ADD_COMBO_POINTS`, Sinister Strike) add one combo point on impact
  to five, tracked per target and lost when the target dies. A finishing move (`AttributesEx`'s
  finishing bit `0x500000`, Eviscerate) refuses with no point on the target,
  spends every point at cast start, and adds `EffectPointsPerComboPoint` damage per spent point to
  its roll.

## Channels And Movement Effects

- A channeled row (`AttributesEx & 0x44`, Mind Flay, Drain Life) resolves its effect once per
  `EffectAmplitude` tick for the row's duration, beginning one tick after the cast completes. The
  tick amount is rolled once at cast start, so the channel's outcome is fixed when it begins.
  Moving, a control effect, or `SpellStopCasting`/`CancelChannelling` ends the channel early; the
  ticks already delivered stand. A channel whose target dies ends without further ticks.
- Control/movement auras resolve from their build-8606 aura id and `EffectBasePoints`: stun
  (`SPELL_AURA_MOD_STUN`, Gouge 1776) stops everything, root (`SPELL_AURA_MOD_ROOT`, Frost Nova
  122) stops movement while actions continue, snare (`SPELL_AURA_MOD_DECREASE_SPEED`, Frostbolt's
  -40%) slows chase movement, and confuse (`SPELL_AURA_MOD_CONFUSE`, Polymorph) behaves as a stun
  here (wander behavior is not modeled). On a creature these gate acquisition, chase and swings for
  their duration; on the player they gate movement through the descriptor flag the client reads.
  Charge-like position changes (`SPELL_EFFECT_CHARGE`) and knockbacks are not modeled yet and are
  explicit unsupported blockers.
- Friendly spell effects (heals, buffs, debuffs) may target the player or a friendly unit; the
  friendly unit's health and active effects live in the simulation for the session and are not
  saved (the source creatures respawn from content, so their state is not part of the character's
  world snapshot).

## Summons

Summons, guardians and totems are not modeled yet: no pet state exists in the simulation, and a
summon spell is an explicit unsupported blocker rather than a silent no-op. The action bar, pet
frames and combat-log naming in the presentation remain wired for the future pet model.

## Spell Effect Coverage

The direct-cast resolver reads build-8606 `Spell.dbc` effect rows and resolves these families:
school damage, heal, the four weapon-damage arms, aura-only buffs and debuffs, and one linked
periodic-damage aura (DoT). Secondary mechanics beside the direct arm are named and carried:
builder combo points, the interrupt's school lockout (Shield Bash, Pummel; no caster to interrupt
yet, so the lockout is presentation), and a trigger (`EffectTriggerSpell`). Periodic damage ticks
as the linked DoT. Effect families that nothing models yet — mana burn (`SPELL_EFFECT_MANA_BURN`),
resource restore (`SPELL_EFFECT_ENERGIZE`), dummy-armed damage (Execute), health-cost casts, and
triggered spells whose trigger is the whole mechanic (Charge's stun) — are explicit unsupported
blockers: the cast refuses with a named reason
and the coverage audit counts them. The runtime must not treat a spellbook entry as proof its
effect works.
- Active buffs and debuffs persist by aura slot, spell id, flags, caster level, stacks, and remaining
  duration. Finite aura time advances only while the character is played; `None` duration is
  permanent. Expired character auras are removed from the descriptor and snapshot. Periodic damage
  currently emits a transient debuff descriptor on creatures, but creature aura state is not saved;
  other aura effects and stat/resource modifiers are not yet simulated.
- Every character knows the common Attack action (client spell ID 6603), placed in action-bar slot
  1. Mage characters also know Fireball (client spell ID 133), placed in slot 2. Other classes do
  not receive a fabricated class spell.
- Auto-attack requires a living attackable creature within 5 yards. It deals a fixed 5 physical damage every
  2,000 simulation milliseconds, beginning one swing interval after Attack starts. It stops when
  toggled off, the target dies, or the target leaves range. Swing damage and speed come from the
  equipped weapon (see "Melee, Weapons, And Combo Points"); the fixed baseline is unarmed only.
- The direct-cast resolver handles the effect families listed under "Spell Effect Coverage" for
  learned spells of any class, reading effect bounds, range, cast time, recovery/GCD, power cost,
  damage school, and linked periodic-damage amplitude/duration from `Spell.dbc` and related tables.
  The character's deterministic damage/heal RNG state is saved with the snapshot.
- Fireball (133) is limited to a living attackable creature within its build-8606 range of 35 yards,
  uses its 1,500 ms cast time and 30-mana cost, and rolls rank-1 direct damage 14-22 at level 1. Its
  build-8606 periodic tail remains 1 damage every 2,000 ms for 4,000 ms on surviving targets.

## Imported Creature Movement

- Waypoint movement type 2 follows the converted `creature_movement` points in ascending point
  order, then loops back to the first point. The source `waittime` is honored after reaching each
  point. These local spline legs currently use a project-owned walk speed of 2.5 yards per second.
- A movement type 2 spawn with no converted points stays at its spawn position. With one point, the
  adapter emits only the initial leg from the spawn to that point; it does not create a repeating
  patrol. Repeating movement requires at least two points.
- Patrol legs and interpolated poses advance from Bevy frame delta. The adapter synchronizes each
  active patrol's current pose and movement id into combat state before hostile AI advances, so
  acquisition and chase begin at the creature's visible location. Patrol progress is not saved.
- Imported creature addon stand state, NPC emote state and sheath state are applied when the unit
  enters the world.
- Waypoint movement type 2 follows its source path. Other source movement types, including type 4
  present in this pack, currently stay at their spawn position. Random-radius movement type 1,
  non-waypoint movement types, waypoint ScriptId actions, and addon auras/mounts are not implemented
  yet; they are not substituted with invented behaviors.

Combat-target health, encounter placement, respawn time, creature kill XP, and quest-8325 XP award
are project decisions. Solo kill XP uses `(creature level * 5 + 45)` at equal or lower levels, scaled
linearly toward zero at the player's gray-level threshold; creatures above the player grant an
additional 5% per level above the player. Fractions truncate. Group and rested bonuses are not
modeled. Creature levels and the current-level XP thresholds come from the pinned CMaNGOS TBC
content source. XP is awarded once on the killing blow, including a periodic-damage killing blow,
and stops at level 70. Spell effect base points and dice come from build-8606 `Spell.dbc`; the seeded
cast roll is deterministic. Quest 8325, “Reclaiming Sunstrider Isle,” is offered and turned in at
Magistrix Erona (creature 15278); killing eight Mana Wyrms (creature 15274) completes it. The reward
is 30 copper, 50 project XP, and a choice of one item (20997 or 20998). The source quest template has
no XP amount, so the value is recorded in the checked-in quest content. A level-1 Mana Wyrm awards
50 XP; its eight kills plus the quest reward reach level 2 with 50 XP remaining. CMaNGOS's pinned XP
thresholds are converted through level 70. Level-ups update level, remaining XP, and talent points;
each gained level restores current health and the class's modeled mana pool to maximum. The current
maximum pools remain project-sized constants; class health, mana, and primary-stat growth are not
yet the source's full stat curves.

## Creature Loot Data

- Starting-zone creature loot tables, nested reference tables, gold ranges, and referenced item
  templates are converted from the same pinned CMaNGOS TBC database revision into
  `content/tbc-creature-loot.toml`; runtime never reads SQL.
- Independent chances, exclusive loot groups, nested reference tables, and item-count ranges are
  preserved by the deterministic loot roller. Rolls consume the character's persisted RNG stream.
- Rows with source conditions are preserved but not rolled until those conditions are modeled.
  Negative quest-item chances are preserved and require an explicit quest-need predicate; the
  runtime does not silently treat them as ordinary drops. The current quest pack has no item
  objectives, so those rows remain unavailable.

## Class Progression

- New characters' known spells and initial action bar follow the race/class rows in the converted
  TBC 2.4.3 progression pack. Trainer purchases require the source-listed trainer entry and class,
  recorded level and spell prerequisites, and enough saved copper.
- Trainer offers use the converted class, level, trainer, prerequisite, copper, faction/race,
  boolean-condition, and learned-spell requirements. Unknown source condition types and nonzero
  skill requirements remain explicit unsupported acquisition blockers; the runtime does not guess
  their meaning. A learn-spell trainer service records and announces the taught spell, not its
  wrapper service ID. Other converted class spells are learned only from a successful trainer
  purchase, never automatically on level-up.
- Talent purchases use the build-8606 `Talent`/`TalentTab` records: one point per level beginning at
  level 10, five spent points per unlocked tier, class/race masks, required spells, talent-rank
  prerequisites, and the row's rank chain. A purchase learns the rank spell set through the chosen
  rank; there is no talent reset path yet.
- Known spell IDs and copper are part of snapshot schema 2; schema-1 snapshots migrate with the
  converted starter spell list and zero copper. Quest journal, current-level XP, and backpack reward
  items extend snapshot schema 3; schema-1 and schema-2 snapshots migrate with empty quest/inventory
  state and zero XP. Snapshot schema 4 adds health, mana, and active aura state; schema 5 adds worn
  equipment; schema 6 adds persisted deterministic RNG state; schema 7 adds project-only body
  proportions; schema 8 adds pending creature corpse loot and respawn timers; schema 9 adds the
  rage, energy and focus pools. Older snapshots
  migrate explicitly, seeding RNG from the stable character ID. Client Spell.dbc supplies spell
  metadata, including effect base points and dice, to the offline simulation adapter.
- The runtime consumes only the checked-in, versioned local content pack; conversion may read the
  pinned source database but the game never connects to it.
