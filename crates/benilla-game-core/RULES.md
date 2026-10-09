# Initial Offline Gameplay Rules

Schema version: 1.

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
  rage, energy, focus, and class-stat-derived pool sizes are not implemented yet. Current health and
  mana are saved with the character, including zero health, and restored on world entry. Creature
  damage updates the health descriptor and combat stops at zero. Choosing the stock "Release Spirit"
  action performs the simplified offline recovery: restore health and mana, move to the race
  starting anchor, and return living creatures to their spawn points. There is no ghost/corpse run,
  durability or XP penalty.
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
  toggled off, the target dies, or the target leaves range.
- The current direct-cast resolver handles build-8606 `SPELL_EFFECT_SCHOOL_DAMAGE` against a living
  attackable creature and `SPELL_EFFECT_HEAL` on self, for learned mana-cost spells. It reads effect
  bounds, range, cast time, recovery/GCD, power cost, damage school, and linked periodic-damage
  amplitude/duration from `Spell.dbc` and related tables. Other power types and effect families are
  explicit unsupported blockers; the runtime must not treat a spellbook entry as proof its effect
  works. The character's deterministic damage/heal RNG state is saved with the snapshot.
- Fireball (133) is limited to a living attackable creature within its build-8606 range of 35 yards,
  uses its 1,500 ms cast time and 30-mana cost, and rolls rank-1 direct damage 14-22 at level 1. Its
  build-8606 periodic tail remains 1 damage every 2,000 ms for 4,000 ms on surviving targets. Mana
  regenerates 5 per simulation second when not casting. Mana is charged when a cast starts and is
  not refunded on interruption.

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

Combat-target health, encounter placement, respawn time, and quest-8325 XP award are project
decisions. Spell effect base points and dice come from build-8606 `Spell.dbc`; the seeded cast roll
is deterministic. Quest 8325, “Reclaiming Sunstrider Isle,” is offered and turned in at Magistrix
Erona (creature 15278); killing eight Mana Wyrms (creature 15274) completes it. The reward is 30
  copper, 50 project XP, and a choice of one item (20997 or 20998). The source quest template has no
  XP amount, so the value is recorded in the checked-in quest content. The new Mage needs 400 current-
  level XP to level; quest 8325 does not level a new character. CMaNGOS's pinned XP thresholds are
  converted through level 70. Level-ups update level, remaining XP, and talent points; class health,
  mana, and primary-stat growth are still project-sized rather than the source's full stat curves.

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
  proportions. Older snapshots migrate explicitly, seeding RNG from the stable character ID. Client
  Spell.dbc supplies spell metadata, including effect base points and dice, to the offline
  simulation adapter.
- The runtime consumes only the checked-in, versioned local content pack; conversion may read the
  pinned source database but the game never connects to it.
