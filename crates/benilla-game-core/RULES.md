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
  is created only for imported creatures whose build-8606 `FactionTemplate.dbc` row reacts hostile
  toward the Blood Elf player template 1610. In the checked-in map-530 pack those rows are faction
  templates 12, 14, 16, 38, 91 and 114. Neutral and friendly imported units are not combat targets,
  even though they share the same client unit presentation. The legacy practice combat harness is
  used only outside map 530.
- Imported visuals use display IDs resolved against the installed build-8606 creature catalog.
- Each imported hostile has 60 health and respawns 30,000 simulation milliseconds after death;
  combat state and updates retain its converted `source_guid` identity.
- Every character knows the common Attack action (client spell ID 6603), placed in action-bar slot
  1. Mage characters also know Fireball (client spell ID 133), placed in slot 2. Other classes do
  not receive a fabricated class spell.
- Auto-attack requires a living hostile within 5 yards. It deals a fixed 5 physical damage every
  2,000 simulation milliseconds, beginning one swing interval after Attack starts. It stops when
  toggled off, the target dies, or the target leaves range.
- Fireball is limited to a living hostile within 35 yards, uses the client's 1,500 ms cast time and
  30-mana cost, and deals a fixed 30 damage. The new Mage has 100 mana and no regen yet; two
  successful casts defeat one 60-health hostile. Mana is charged when the cast starts and is not
  refunded on interruption.
- Cooldown lockout is 1,500 simulation milliseconds, beginning when the cast starts. Combat uses
  injected elapsed time; no wall clock or random roll affects the outcome.

## Imported Creature Movement

- Waypoint movement type 2 follows the converted `creature_movement` points in ascending point
  order, then loops back to the first point. The source `waittime` is honored after reaching each
  point. These local spline legs currently use a project-owned walk speed of 2.5 yards per second.
- Imported creature addon stand state, NPC emote state and sheath state are applied when the unit
  enters the world.
- Waypoint movement type 2 follows its source path. Other source movement types, including type 4
  present in this pack, currently stay at their spawn position. Random-radius movement type 1,
  non-waypoint movement types, waypoint ScriptId actions, addon auras/mounts, and combat AI are not
  implemented yet; they are not substituted with invented behaviors.

The cast damage, hostile health, encounter placement, and respawn time are project decisions. The
quest contact establishes the next incremental slice; quest state, loot, XP, and rewards are not
yet implemented.

## Class Progression

- Class starter spells and later spell availability follow the converted TBC 2.4.3 server-content
  records.
- Later class spells are bought from trainers when the character meets the recorded level and
  prerequisite requirements. They are not automatically learned on level-up.
- The runtime consumes only the checked-in, versioned local content pack; conversion may read the
  pinned source database but the game never connects to it.
