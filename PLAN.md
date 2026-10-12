# Offline TBC Vertical Slice Plan

## Goal

Fork benilla into a single-process, single-player TBC 2.4.3 game. The first
playable slice is the Blood Elf start: Sunstrider Isle and the surrounding
Eversong Woods, with Silvermoon City reachable, and multiple persistent local
characters.

The player supplies a legally obtained TBC 2.4.3 build 8606 install at runtime,
named by `WOW_DATA` (or the dev-build `WoW` link). Keep machine-specific install
paths out of the repository. Dropbox can rewrite files in a synced folder, so
the install-safety check (see Testing) must tolerate nothing changing under it
during a run, and a local unsynced copy is preferred for gate runs.

The game must not open a world/auth socket, start a local server process, or
require accounts, realms, or an external database at runtime. Content-conversion tooling may use a
pinned TBC server-content database to build the local data pack. It still needs an in-process game
authority: gameplay rules, simulation, and persistence do not disappear
when networking does.

## Non-Goals For The First Slice

- Network compatibility with TBC servers.
- Auth, SRP, realm selection, packet crypto, or TBC packet/opcode support.
- Reproducing all of TBC before a playable local game exists.
- Bundling Blizzard data, FrameXML, sounds, maps, or models in this repository.
- Compatibility with stock TBC addons, and a project mod API.
- Flying, Outland, instances, groups, PvP, professions.
- Maintaining the inherited networked Vanilla client (see Fork Policy).

## Fork Policy

This is a fork, not a contribution to upstream benilla. Upstream's rules
(`AGENTS.md`, `docs/METHOD.md`, `docs/CONTRIBUTING.md`) define benilla as a
faithful 1.12.1 network client that "does not build a server or simulate the
game". Those documents bind every agent session, so they are rewritten in
Phase 0 before any other work; until then an agent following them will reject
this plan.

Decisions:

- **Target.** TBC 2.4.3 build 8606, offline only. The 2.4.3 client is the
  reference for data, formats, UI and presentation. Gameplay rules have no
  client reference and are project-owned, documented design.
- **Vanilla.** The networked 1.12 mode is frozen at the fork point and not
  maintained. Vanilla-only code is deleted when it blocks TBC work, not kept
  behind conditionals. Supporting two versions doubles every DBC schema, field
  layout, Lua dialect and catalogue; this fork does not pay that.
- **Upstream merges.** The engine crates (`benilla-world`, `benilla-assets`,
  `benilla-bytes`, `benilla-blp`, `benilla-wmo`, `benilla-adt`, `benilla-wdt`)
  stay structurally close to upstream so rendering and graphics work can still
  be merged. Divergence concentrates in `benilla-app`, `benilla-ui`,
  `benilla-formats`, `benilla-dbc`, `benilla-mpq` and the new crates.
- **Kept rules.** The install is read-only, nothing from the install is
  committed, local state lives under `benilla-config/`, our code is
  MIT OR Apache-2.0, changes land small with gates green, and measurement beats
  eyeballing.

## Current Starting Point

The inherited project is a build-5875 Vanilla network client, not a
version-neutral engine. It provides a renderer, terrain streaming, collision,
asset readers, a UI runtime, character-select presentation, and server-less
capture fixtures. Facts that shape this plan:

- **Protocol coupling.** 278 of 670 `benilla-app` source files reference
  `benilla_protocol`. `SessionEvent` (about 275 variants) lives in
  `benilla-protocol/src/events.rs`; `ClientCommand` (about 252 variants, wire
  concepts such as movement flags and force-speed acks) lives in
  `benilla-app/src/net.rs`.
- **Entity state is a 1.12 update-field array.** Each entity carries
  `ObjectStore(ObjectFields)`, a dense `Vec<u32>` indexed by 1.12 descriptor
  constants (`FIELD_UNIT_HEALTH = 22`, `UNIT_END = 188`, `PLAYER_END = 1282`),
  with change notifications per index. About 98 app files use it.
- **An injection seam exists.** `NetEvents(Receiver<SessionEvent>)` is drained
  by the normal apply path; tests already feed `SessionEvent::ObjectCreate`
  through it (`net/apply/seam_tests.rs`). Capture mode runs with
  `NetPlugin { connect: false }` and spawns fixture entities directly.
- **Movement needs a placement.** The local player is simulated client-side
  with capsule collision (`player/mover.rs`, `benilla_world::collision`), but
  control activates only after a server-reported self pose
  (`player/wire_in.rs`), and run/walk/swim speeds come from server packets.
- **Time comes from the server.** Game time is set by SMSG_LOGIN_SETTIMESPEED
  and falls back to noon (`net.rs`); quest timers use a server wall clock.
- **Character select and create wait on the IO thread** for SMSG_CHAR_ENUM and
  create/delete results (`net/io.rs`, `char_select/mod.rs`,
  `char_create/mod.rs`).
- **The archive chain is Vanilla-only.** `VANILLA_BASE_ORDER` lists the split
  1.12 archives; `mount_order` adds `patch.MPQ`, single-character
  `patch-?.MPQ`, and `speech2.MPQ`, with no locale subfolder. The MPQ reader
  reads only the 32-byte original header, supports only zlib, refuses
  single-unit and encrypted files, and ignores the hash-table locale word.
- **DBC schemas are hand-written 1.12 layouts** with exact field-count checks,
  a 9-dword localized string (8 locales + flags), and build-5875 row counts
  asserted in tests.
- **M2** accepts versions 256–263 and reads embedded views (correct for TBC:
  `.skin` files arrive in Wrath). Particle emitters and ribbons are read at
  fixed v256 offsets and strides with no version check.
- **The UI VM mixes 5.1 grammar with a 5.0 surface.** The vendored Lua 5.1.5
  grammar already accepts `...`, `#` and `%` (`third_party/lua-src`,
  `lparser.c:764,796,808`), and handlers already receive positional
  `(self, event, ...)` (`script/event.rs:260`). Still 5.0: the compat hunks
  (table-`for` rewrite in `lvm.c:681-730`, `LUA_COMPAT_LSTR`, constructor `;`,
  `recfield`, `LUA_QL`, `LUA_COMPAT_VARARG`) and the Rust library layer
  `script/lua50.rs` + `script/lua50/table_size.rs`. The `this`/`event`/
  `arg1..N` globals are still set alongside the positional path. The addon gate
  is pinned to interface `11200` (`script/addon_gate.rs:14`). TBC 2.4.3 FrameXML
  is stock Lua 5.1 and depends on secure templates and protected functions.
- **Per-character local state is keyed `<realm>-<character name>`** in
  `local_state.rs` (macros, bindings, saved variables, camera, chat, layout
  cache), with a per-realm name cache.
- **The gates are 1.12-shaped.** Tests pin the UI layer manifest and check the
  UI surface against `reference/1.12-*.tsv`; `scripts/smoke.sh` requires a live
  server login.

The supplied TBC data layout is incompatible with the Vanilla mount order:

```text
Data/common.MPQ
Data/expansion.MPQ
Data/patch.MPQ
Data/patch-2.MPQ
Data/enGB/base-enGB.MPQ
Data/enGB/locale-enGB.MPQ
Data/enGB/expansion-locale-enGB.MPQ
Data/enGB/patch-enGB.MPQ
Data/enGB/patch-enGB-2.MPQ
```

This is a repack: it may contain non-stock archives or modified stock ones.

## Migration Audit: Vanilla Systems To Update And TBC Systems To Create

This audit is the concrete, code-derived inventory behind the phases below; task-level tracking
stays in `TASKBOARD.md` and the phase order stays the sequencing authority. It separates what
already branches on build 8606 from what must be updated, and what TBC adds that 1.12.1 never had.
Line references are current sites and will move.

### Already TBC-aware

Do not rebuild these; extend them.

- **Archive chain.** `benilla-formats/src/chain.rs` has `tbc_mount_order` (:68), `Chain::open_tbc`
  (:182) and `is_tbc_data_dir` (`install.rs:81`); the locale is discovered, not assumed.
- **DBC adapters with 8606 branches.** `Map` (125 fields), `Spell` (216), `Faction`/`FactionGroup`/
  `ChrRaces` (16 locales), `CreatureModelData`/`CreatureDisplayInfo`/`CreatureDisplayInfoExtra`,
  `ItemDisplayInfo`, `SkillLine`, `Talent`/`TalentTab`, `SpellShapeshiftForm`/`SpellDispelType`/
  `SpellRange`.
- **M2.** `benilla-m2` accepts v256–263 with versioned bone (v260 stride 112) and skin batch
  (≥260 stride 48) layouts.
- **Game authority.** `benilla-game-core` (no `benilla-protocol` dependency) owns roster, saves
  (schema 7), combat, the one-quest loop and converted TBC content packs;
  `crates/benilla-app/src/offline.rs` is the working Stage A adapter.
- **Offline compile-out.** `benilla-offline` builds `benilla-app` with
  `default-features = false, features = ["offline"]`; `scripts/offline-deps.sh` asserts no SRP or
  network feature resolves.
- **Some 2.4.3 UI globals** already exist (`IsPassiveSpell`, `GetCursorInfo`,
  `SetBagPortraitTexture`, `GetContainerNumFreeSlots`).
- **AddOn interface gate.** `benilla-ui/src/script/addon_gate.rs` accepts build 8606's `20400`
  interface and rejects older or missing versions when `checkAddonVersion` is enabled.

### Vanilla systems that must be updated

| System | 1.12.1 assumption | Current site | TBC 2.4.3 change |
| --- | --- | --- | --- |
| DBC localized tables | ~13 tables hardcode 8 locales with no `tbc` branch; `benilla-dbc` rejects any field-count mismatch | `benilla-formats/src/{auction_house,itembagfamily,itemclass,item_random_properties,item_visuals,exhaustion,gm_ticket_category,cfg_categories,creature_types,emote_text,languages,game_tips,server_messages,chat_channels}.rs`; `lib.rs:628` | Widen to 16 locales + flags; make `schema_for` (`lib.rs:676`) build-aware or mark it 1.12-only |
| ADT liquid | `MCLQ` only | `benilla-adt/src/lib.rs:115-546`; `benilla-formats/src/liquid.rs`; `terrain.rs:15-70` | Read `MH2O` (see creations) |
| WDT | `WowVersion` is `Classic` only; unknown chunks dropped | `benilla-wdt/src/lib.rs:10-19,193-195` | Add a TBC variant; read `MFBO` |
| M2 emitters and ribbons | Fixed v256 stride and offsets, unversioned | `benilla-formats/src/{particles,ribbons}.rs` | Read v260–263 records; fail loudly on an unknown version |
| MPQ reader | zlib-only codec; v0/v1 header; encrypted rejected; locale defaults to 0 | `benilla-mpq/src/lib.rs:144-171,266-268,692-708` | Implement only what the install corpus uses: bzip2/PKWARE/multi-codec, single-unit, encrypted, the locale word; report per-feature counts |
| Descriptor layout | 1.12 `ObjectFields` table: `FIELD_UNIT_HEALTH=22`, `UNIT_END=188`, `PLAYER_END=1282`, plus hardcoded indices | `benilla-protocol/src/messages/update_object/fields/mod.rs:346-371`; `benilla-app/src/offline.rs:495-511,966-972`; `net.rs` | Retarget to the 2.4.3 field table, or execute Phase 6 typed components |
| Wire event surface | `SessionEvent` (~275 variants) and `ClientCommand` (~252) are pure Vanilla concepts | `benilla-protocol/src/events.rs:112`; `benilla-app/src/net.rs` | Narrow to the UI-consumed subset in Stage A; delete translations in Stage B |
| Lua VM surface | 5.0-restoring compat hunks and the `lua50` library | `third_party/lua-src/BENILLA.md`; `benilla-ui/src/script/lua50.rs`, `script/lua50/table_size.rs` | Go to the stock 5.1 library (see creations) |
| Handler globals | `this`/`event`/`arg1..N` set beside the positional args | `benilla-ui/src/script/event.rs:218-276` | Keep positional `(self, event, ...)`; drop globals except measured 2.4.3 reads |
| Catalogues and surface gates | All `reference/1.12-*.tsv` plus six gates | `reference/`; `benilla-ui/src/script/tests/{reference_surface,widget_surface}.rs`; `benilla-app/src/ui_script/{surface,shape,event_shape,verb_event}_gate.rs`; `scripts/api-coverage.sh` | Generate 2.4.3 catalogues; repoint every gate |
| Glue presentation | Hand-built Bevy UI that parses `GlueStrings` in Rust | `benilla-app/src/glue/`, `glue_strings.rs` | Replace with stock GlueXML execution |
| Combat rules | Fireball-hardcoded fallback; auras stored but never apply; no world query | `benilla-game-core/src/lib.rs:26-38,1654-1807`; `offline.rs:1677-1696` | Generalize effects; implement `WorldQuery` (see creations) |
| Resources | Mana only; flat pool and regen | `benilla-game-core/src/lib.rs:48-55,2749-2753` | Model rage/energy/focus |

### TBC systems that must be created

These have no 1.12.1 counterpart.

**Formats and data**

- `MH2O` terrain-liquid reader (TBC replaces `MCLQ`).
- `MFBO` flight-bounds chunk handling in WDT.
- M2 v260–263 particle-emitter and ribbon records.
- The TBC descriptor field table and TBC-only unit/player fields.
- TBC DBC tables and content absent from 1.12.1: Blood Elf and Draenei races, map 530
  (`Expansion01`) assets, 2.0 talents, socketed gems, arenas, jewelcrafting. The slice scope above
  bounds which are required.
- 2.4.3 reference catalogues (globals, events, widget shapes, message strings).

**UI runtime**

- Stock Lua 5.1 library layer: `coroutine`, `_VERSION`, `print`, the 5.1 string metatable,
  `table.maxn`, `math.fmod`/`modf`/`huge`.
- Secure-template and protected-function model: `SecureActionButtonTemplate`, `SecureHandler*`,
  `InCombatLockdown`, and attribute-driven action semantics (today `issecure`/`securecall`/
  `hooksecurefunc` are no-ops at `script/stdlib.rs:95-97,353-385`).
- GlueXML `.toc` loader and executor for the stock 2.4.3 glue scenes.

**Gameplay**

- Player spell effect families: periodic healing, drain/leech, stat/resource/armor/resistance
  auras, shields/absorb, dispels, control/movement, summons/guardians/totems, and class mechanics.
- Acquisition sources and gating: skill, profession, quest/item learn, racial,
  mutually-exclusive talents; persisted rank chains; and an explicit effect-coverage audit.
- Non-mana resources (rage, energy, focus) and their generation.
- `WorldQuery` (ground height, ray/segment collision, liquid at point) over
  `benilla_world::collision`, with a stub for tests.
- The gameplay rules and formulas for the expanded effects (per `RULES.md`, currently one slice).

### Corrections to the inherited picture

- Protocol coupling has grown: 282 of 693 `benilla-app` files reference `benilla_protocol`, 212
  touch `ObjectStore`/`ObjectFields`, and 134 touch `ClientCommand`.
- The Stage A adapter exists in substance as `crates/benilla-app/src/offline.rs`
  (`OfflineGamePlugin`, feature-gated `offline`) and translates `GameCore` directly to
  `SessionEvent`/`ClientCommand`. There is no `GameEvent`/`GameCommand` type or `adapter` module;
  the plan names the target shape, the code currently uses a direct form.
- `benilla-game-core` exists and does not depend on `benilla-protocol`.
- `WorldQuery` is described in this plan and in `RULES.md` but is implemented nowhere.
- The UI VM line under Starting Point is corrected above: `#`, `%` and `...` are already enabled;
  the 5.0 work is the library layer and the compat hunks.

## Product Decisions

### Process Model

The shipped game is one executable and one process. `game-core` owns the
authoritative state; the renderer, UI, audio, and input consume its events and
submit commands to it.

```text
input + UI -> game command -> game-core -> game event -> presentation adapter -> ECS/UI
                                  |              ^
                                  |              +-- WorldQuery (heights, collision, LoS)
                                  +-> save snapshot
```

- **Tick.** `game-core` advances on a fixed simulation tick driven from Bevy's
  `FixedUpdate`, independent of frame rate. Its clock is an injected game
  clock, never the host clock, so tests run deterministically.
- **No transport.** There is no socket and no serialized packet on this path.
  The network IO thread and Login/Realm Select are compiled out of the offline
  binary, not merely disabled at runtime.
- **World queries.** AI chase, evade, line of sight, spawn ground placement
  and falling need world geometry. `game-core` depends on a `WorldQuery` trait
  (ground height, ray/segment collision, liquid at point). The game implements
  it over `benilla_world::collision`; tests use a stub. Pathing for the slice
  is direct steering with ground snapping and obstacle give-up (evade); a
  navmesh is a later decision, not a slice requirement.
- **Determinism.** All randomness flows from a seeded RNG owned by `game-core`
  whose state is saved, so loot, combat rolls and respawns reproduce across
  save/load.

### Presentation Adapter Strategy

Rewriting the 278 protocol-coupled app files before anything is playable is
the largest risk in the plan. The chosen path is staged:

1. **Stage A (slice).** `game-core` emits version-neutral `GameEvent`s. An
   adapter translates them into the existing `SessionEvent` stream and injects
   it through `NetEvents`, and translates the existing `ClientCommand`s back
   into `GameCommand`s. The 1.12 `ObjectFields` layout becomes a private,
   in-memory presentation format: no bytes are serialized and nothing claims
   wire compatibility. `benilla-protocol` remains a compile-time dependency for
   its types; its network, auth and SRP code is not compiled into the offline
   binary.
2. **Stage B (after the slice).** Replace `ObjectStore(ObjectFields)` with
   typed components system by system, where the 1.12 layout costs real work
   (TBC-only fields, version-specific semantics). Each replaced system deletes
   its `SessionEvent` translation.

`game-core` itself never depends on `benilla-protocol`; the translation lives in
the adapter, in `benilla-app`.

### Content And Assets

- The TBC install remains read-only and external to the repository.
- TBC client files provide visual assets, DBC data, FrameXML/GlueXML, and
  localized UI strings.
- The client data does **not** contain server content. In 2.4.3 the following
  come from the server and must be converted from the selected content source: creature and item templates
  (names, stats, levels, factions), quest text and objectives, NPC gossip text,
  vendor/trainer lists, loot tables, spawn placements, player create info
  (start position, starting spells and action bar), per-level player stats,
  and all gameplay formulas.
- The client data does contain, and content references by ID: creature and
  item display info, spells and spell visuals, `CharStartOutfit`, factions,
  areas and maps.
- TBC server-content datasets may be converted into project content for this offline port. Every
  imported content set pins its source, revision, license, extraction command, and conversion
  version; every generated content file states that provenance.
- Every content record has a stable identifier, and every content file a schema
  version, so saves can be migrated or rejected safely.

### Gameplay Rules

Combat, regeneration, XP and leveling formulas are server-side in the real game
and have no client reference. They are project-owned, written down in one
rules document with a version, and unit-tested. Where the client exposes a
number (spell base points, cast times, cooldowns, ranges in `Spell.dbc`), the
rules use it rather than inventing one.

### Persistence

`benilla-config/` remains the one local-state root.

```text
benilla-config/
  offline/
    profile.toml
    characters/<character-id>/save.toml
    characters/<character-id>/save.toml.bak
    characters/<character-id>/ui/
```

- `character-id` is a generated stable identifier, never the display name.
  Names are user-facing and may be changed or collide after normalization.
- **One snapshot per character.** Character state (appearance, position,
  stats, inventory, quests) and that character's world state (respawn timers,
  world flags, RNG state, game clock) live in a single `save.toml`, so a quest
  turn-in and its world effects commit together.
- **Atomic commit.** Write to a temporary file in the same directory, fsync,
  rename over the old file, fsync the directory. The previous snapshot is kept
  as `.bak`. A checksum detects corruption; load falls back to `.bak` and says
  so.
- Every root document carries a save format version. Schemas grow per phase
  with explicit migrations; older saves are migrated or rejected with a clear
  error, never silently reinterpreted.
- `game-core` is the only writer of `save.toml`. The roster writer in Phase 4
  becomes the first `game-core` command handler, not a separate writer that is
  later replaced.
- Per-character UI state (macros, bindings, saved variables, camera, chat,
  layout cache) is keyed by `character-id` instead of `<realm>-<name>`.
  Account-wide UI state moves under `offline/`. There is no realm name; nothing
  synthesizes one as a key.

### Time

Game time (day/night, respawns, buff durations) advances only while that
character is being played, and is saved in its snapshot. Respawn and
expiry timers are in game time, so closing the game does not respawn or expire
anything. The initial time of day for a new character is a project-owned
constant.

### Character Roster

The character-select screen stays a normal, multi-character roster:

- The app boots to the local roster, not Login or Realm Select.
- Create, delete, select, and last-selected character work without a server.
- Character creation supports Blood Elves only. Class choice is limited to the
  class(es) the slice implements; the others are shown unavailable rather than
  hidden.
- The roster shows saved appearance, level, zone, play time, and last-played
  timestamp. Play time is accumulated by `game-core`.
- Name validation uses the client's own name rules and filter lists where the
  2.4.3 data provides them, plus uniqueness across the local roster.
- The roster holds the stock per-realm maximum (10 in the stock UI); the slice
  does not add paging.
- A selected character enters its saved position, or the Blood Elf start
  position for a new character.

### Optional Player-Model Proportions

- Proportion controls are an offline-only, explicit opt-in on character creation. With the feature
  off, the stock TBC character model, dimensions, and appearance values remain unchanged.
- A profile is saved per character. Height is bounded from -10% to +35%; shoulder, arm, waist, hip,
  and bust controls are bounded to +/-20%, and the seat control to -20% to +40%. Female models
  additionally expose a bust control. Head size is not adjustable, and the supported ranges must
  not permit collapsed or extreme body regions.
- Height uses the model's normal instance scale. Shoulder, arm, waist, hip, bust, and seat changes
  are cosmetic skinned-model deformations based on installed M2 bounds, with race-specific vertical
  anchors for the pelvis, chest, and seat; they do not change combat stats or edit client assets.
- Existing characters and profiles without the opt-in settings migrate to the stock, unmodified
  proportions.

## Target Slice

### Geography

- Map 530 (`Expansion01`), which also contains Outland and the Draenei zones;
  only the Blood Elf area is in scope.
- Sunstrider Isle (the Blood Elf new-character start), its route into Eversong
  Woods, and Silvermoon City.
- The Blood Elf character-select and character-create glue scenes.
- Map streaming correctly resolves the TBC map and all referenced terrain, WMO,
  model, texture, sky/light, liquid and minimap assets.

### First Playable Loop

1. Create or select a Blood Elf character.
2. Enter the world at the Sunstrider Isle start.
3. Move, turn, jump, collide with terrain and WMOs, target entities, and use the
   camera.
4. Accept one quest from an NPC through gossip.
5. Kill a small set of hostile creatures using basic combat.
6. Loot an item, complete the objective, return, and receive an XP and item
   reward.
7. Save automatically at meaningful transitions and restore the same character
   after restart.

### Required Game Features

- Player placement (initial pose, speeds, teleport) emitted by `game-core`.
- Local entity identity: 64-bit IDs from `game-core`, stable across save/load
  for entities that persist.
- Static creature, NPC, and game-object spawns.
- Targeting, interaction range, health, mana/energy, regeneration, damage,
  death, respawn, and loot.
- One starting class: auto-attack and one or two abilities, with cast time,
  cooldown, resource cost, range, target validation and spell visuals from
  `Spell.dbc`.
- XP from kills and quests, and leveling within the slice's level range.
- Player death and recovery (a simple respawn rule; the ghost/corpse run is
  out of scope unless it is cheap once death works).
- Hostile AI: idle, acquire, chase, attack, evade, death, respawn.
- Inventory, equipment presentation, starting outfit, currency, and quest log.
- NPC gossip plus quest accept/progress/turn-in UI.
- Game clock driving sky/light, with deterministic respawn timers.
- Pause/menu, save-on-exit, and recovery from an interrupted save.
- Zone music and ambience from the TBC sound DBCs.

## Implementation Phases

### Phase 0: Fork Governance, Gates, And Offline Binary

Rewrite the rules first, then the build.

Tasks:

- Rewrite `AGENTS.md` and `docs/METHOD.md` for the fork: target, Fork Policy,
  what is reference-driven (data, formats, UI) and what is project-owned
  (rules, content). Update `docs/CONTRIBUTING.md` or replace it.
- Remove or rescope gates that cannot apply: `scripts/smoke.sh` (live server
  login) is replaced by an offline boot-to-world smoke run. The 1.12 catalogue
  checks (`reference/1.12-*.tsv`, shape/event/verb/surface gates) are marked
  frozen and later replaced with 2.4.3 catalogues in Phase 3. The UI layer
  manifest test stays, retargeted to the fork's layer.
- Update `scripts/genmap.sh` and crate descriptions so `docs/MAP.md` stops
  describing a 1.12 client.
- Add the offline binary: a launcher crate that builds `benilla-app` with the
  network plugin, IO thread, Login and Realm Select compiled out (cargo
  feature), and starts at the local roster screen.
- A `GameMode`/startup configuration is chosen before plugins are installed.
- Install a no-op `game-core` behind the adapter so later phases have a home.

Acceptance:

- An agent session reading `AGENTS.md` gets rules consistent with this plan.
- `scripts/gates.sh` is green on the fork with the 1.12-only gates
  rescoped, and no gate needs a server.
- The offline binary contains no auth, SRP or socket code; a run makes no
  outbound connection attempt, checked by an instrument, not inspection.
- `WOW_HOST`, `WOW_USER`, `WOW_PASS` and `WOW_CHAR` are not read by the offline
  binary.

### Phase 1: TBC Install And Archive Chain

Add a TBC install resolver and archive chain, replacing the Vanilla one rather
than adding conditionals to it.

Tasks:

- Detect a TBC Data directory by `common.MPQ`, `expansion.MPQ`, and a locale
  subfolder (`Data/<locale>/`); the locale is discovered, not assumed to be
  enGB.
- Establish the 2.4.3 mount order and precedence from a named reference (the
  client's own load order as observed or documented), including which numbered
  `patch-N`/`patch-<locale>-N` archives the stock client loads, and record the
  source in a code comment.
- Define a policy for non-stock archives in a repack: list them in diagnostics,
  and load only what the stock client would load.
- Check the MPQ reader against every archive in the install before extending
  it: the extended (Burning Crusade) header and hi-block table, compression
  codecs other than zlib (bzip2, PKWARE implode, multi-codec), single-unit
  files, encrypted files, and the hash-table locale word. Implement what the
  corpus actually uses; test each against the install.
- Add diagnostics that name the selected game version, locale, archive chain,
  and the winning archive for an asset path.
- Make asset and derived-data cache keys include game build and locale.

Acceptance:

- The resolver rejects a Vanilla install.
- It loads known Sunstrider, Eversong and Silvermoon terrain, WMO, model,
  texture, and DBC paths from the supplied install.
- Every file listed in every archive's listfile opens or fails with a named,
  unsupported-feature error; the count of each is reported.
- Tests cover archive priority and locale fallback without committing install
  data.

### Phase 2: TBC Formats And World Viewer

Port the format paths needed to render the target area. Every layout is
validated against build 8606 data; no 1.12 layout is assumed compatible because
it happens to parse.

Tasks:

- Replace the 1.12 DBC schemas with 2.4.3 schemas for the tables used by map
  loading, lighting, liquids, character displays, creatures, items, spells,
  areas, sound and minimaps. Confirm the 2.4.3 localized-string width
  (believed to be 16 locales + flags, versus 8 + flags in 1.12) from the files
  and centralize it.
- Remove build-5875 row counts and fixed IDs from tests and code; replace them
  with 8606 values measured from the install.
- Validate ADT, WDT, WMO, M2 and BLP behavior against target-area assets,
  including TBC-specific ADT chunks (such as `MFBO`) that must be skipped or
  read.
- Version-check M2 particle emitter and ribbon layouts: read v260–263 records
  at their actual offsets and strides instead of the fixed v256 layout, and
  fail loudly on an unknown version.
- Verify the TBC character-model path: Blood Elf M2s, `CharSections`, hair and
  facial geosets, and `CharStartOutfit`.

Acceptance:

- `benilla-worldview` opens Sunstrider Isle, Eversong and Silvermoon without
  format errors, and a numeric probe (particle and ribbon census, missing
  texture count, missing doodad count) is clean in each.
- Deterministic captures of the three areas exist as visual baselines.
- Automated data tests exercise selected known assets from all target areas.

### Phase 3: TBC UI Runtime

The roster, character creation, HUD, gossip, quest, loot and bag UI are needed
by Phases 4 and 5, so the UI runtime moves before gameplay. The UI is the stock
2.4.3 GlueXML/FrameXML loaded from the install; no throwaway project UI is
built.

Tasks:

- Switch the vendored Lua from the 5.0-restoring patch set to stock Lua 5.1
  semantics: `...`, `#`, `%`, the 5.1 standard library, and no `for k,v in t`
  table-for or `arg` varargs.
- Change script handler dispatch to the 2.x convention (`self, event, ...` as
  parameters), keeping the `this`/`argN` globals only where 2.4.3 FrameXML
  still reads them.
- Set the interface version gate to 2.4.3's (`20400`).
- Implement the secure-template and protected-function model to the extent the
  stock action bar, unit frames and buttons need to function (an offline game
  has no taint threat, but the stock files call the API).
- Generate 2.4.3 reference catalogues (globals, events, widget shapes, message
  strings) from the 2.4.3 FrameXML and API, and retarget the catalogue gates to
  them.
- Boot GlueXML to the character-select screen against an empty local roster,
  then FrameXML to an in-world HUD against a fixture player.

Acceptance:

- GlueXML and FrameXML load from the install with zero Lua errors on the files
  the slice uses, measured by the error count, not by looking.
- The catalogue gates pass against the 2.4.3 catalogues.

### Phase 4: Local Roster, Saves, And World Entry

Implement the save contract and the first `game-core` commands. This is the
first executable milestone: a saved player standing in Eversong.

Tasks:

- Define the save schema for profile, roster entry, appearance, position,
  play time and game clock only. Later phases extend it with migrations.
- Implement atomic snapshot commit, `.bak` fallback, checksums and clear
  incompatible-save errors.
- Implement roster commands in `game-core` (create, delete, select, list) and
  translate them through the adapter into the existing `CharListMessage` /
  `CharActionResultMessage` paths that character select and create consume.
- Implement world entry: `game-core` emits the player object, initial pose,
  movement speeds and game time; the adapter turns these into the self-pose,
  speed and time events that activate the player controller and sky.
- Feed movement commands into `game-core`, which accepts the client-simulated
  position (single-player: no anti-cheat validation) and persists it.
- Rekey per-character UI state in `local_state.rs` by `character-id`.

Acceptance:

- At least ten local characters can be created, selected, deleted, and
  restored across process restarts.
- A character's appearance, last position and play time survive restart.
- A process killed during a save loads the previous valid snapshot.
- Unit tests run roster and world-entry command/event sequences with no
  renderer, window, install, socket, or host clock.

### Phase 5: Eversong Gameplay Slice

Implement the first complete local gameplay loop with project-owned content
and rules.

Tasks:

- Define the content format and loader: creature templates, item templates,
  spawn points, NPC gossip, quests, loot tables, player create info and simple
  AI parameters. Content references client data only by ID.
- Author the slice content: a small population around the start, one
  quest-giver, one quest, one loot item and reward.
- Write the rules document: melee and spell damage, hit/miss, regeneration,
  XP curve and per-level stats for the slice's class and level range.
- Implement `WorldQuery` over `benilla_world::collision` and use it for spawn
  ground placement, chase, evade and line of sight.
- Implement AI, combat, death/respawn, loot, inventory/equipment, XP and the
  quest from acceptance through objective updates to reward, each as
  `game-core` commands and events translated by the adapter.
- Extend the save schema (stats, inventory, quests, world flags, respawn
  timers, RNG state) with a migration from the Phase 4 schema.

Acceptance:

- A newly created character can finish the quest loop with the stock UI,
  without a console, test fixture, or debug command.
- Save/reload preserves quest progress, dead/respawning entities where
  intended, inventory, currency, XP and location.
- A scripted bounded run (unattended, silent) completes the loop and reports
  each step from an instrument trace.
- The game runs with only the TBC install and the local save directory.

### Phase 6: Typed Presentation (After The Slice)

Stage B of the adapter strategy, done incrementally once the slice is
playable.

Tasks:

- Replace `ObjectStore(ObjectFields)` with typed components one system at a
  time, starting where TBC-only fields or semantics make the 1.12 layout costly.
- Delete each system's `SessionEvent` translation as it moves to direct
  `GameEvent` consumption.
- Drop `benilla-protocol` from the offline binary once nothing references it.

Acceptance:

- Each migrated system has the same instrumented behavior before and after.
- The slice stays playable at every step.

### Phase 7: Complete TBC Player Spell Coverage (Post-Slice Expansion)

The first playable slice only needs one starting class and a small number of abilities. The product
scope now also requires complete build-8606 player-spell acquisition and behavior across classes;
this is a post-slice expansion, not a claim that the initial quest loop already supports every
spell.

Tasks:

- Inventory every player-acquisition source represented by TBC 2.4.3: race/class starting spells,
  class and profession trainers, talents, racial abilities, and other learn/quest/item sources that
  the game models. Exclude NPC-only and internal spells unless a player can acquire them.
- Validate rank chains and source gates (race, class, level, prerequisites, skill, condition,
  trainer, talent choice, and mutually exclusive selections) against build-8606-compatible source
  data. An unavailable source condition must be modeled or reported as an explicit unsupported
  blocker, never silently opened or silently omitted.
- Implement player resource pools, cast validation and timing, cooldowns, target rules, and effect
  resolution from the build-8606 spell tables. Cover direct and periodic damage/healing, auras and
  dispels, resource/stat/armor/resistance changes, control/movement effects, summons, and
  class-specific mechanics used by player-acquirable spells. Document any intentionally unsupported
  effect before marking an individual spell unavailable.
- Add a generated/data-gated coverage audit joining every player-acquirable spell to its rank,
  source, required client data, and runtime effect handler. No player-facing spell may fall through
  to a fixed Fireball implementation or appear usable without its effect being simulated.
- Keep simulation deterministic and save/load all persistent spell state, including active effects
  and any RNG state needed to reproduce combat outcomes.

Acceptance:

- Coverage tests enumerate player-acquirable spells by class/source and find no missing rank,
  availability rule, or runtime effect handler for the supported TBC source set.
- Tests verify representative spells from every class and acquisition source for actual damage,
  healing, buffs/debuffs, resource cost, cast/cooldown/range/target rules, aura expiry, interruption,
  and save/re-entry behavior against build-8606 data.
- Trainer, talent, and other stock UI paths expose only choices permitted by the character's class,
  race, level, prerequisites, skill, and source conditions.
- A bounded offline run exercises each class and effect family. Unsupported player-acquirable spells
  are reported explicitly and prevent phase acceptance.

## Testing Strategy

- Unit tests: save encoding, migration and crash recovery; `game-core`
  command/event sequences; combat and XP arithmetic; quest conditions; loot
  rolls with a fixed seed; AI state changes against a stub `WorldQuery`.
- Content tests: every content reference to client data (display IDs, spell
  IDs, map/area IDs, faction IDs) resolves against the 2.4.3 DBCs; every
  content-to-content reference (quest to creature, loot to item) resolves
  within the project content.
- Integration tests: create two characters, play/save/reload each
  independently, delete one, and verify the other's snapshot is byte-identical.
- Engine checks: offline startup, roster entry, world entry, and a bounded
  automated gameplay loop.
- Visual checks: deterministic captures of Sunstrider/Eversong/Silvermoon,
  character select and create, HUD, gossip, quest dialog, combat, and loot.
- Install safety: a size and mtime manifest of every file under the install is
  taken before and after each gate run and must match. Saves, logs and caches
  are written only under `benilla-config/`.
- Data-gated tests skip without the install and fail on a skip where the
  install is present, as upstream's gates do, now keyed to a TBC install.

## Sequencing And Risks

The first executable milestone is not combat. It is:

```text
fork rules + gates -> TBC archive chain -> Eversong in worldview
  -> TBC UI runtime -> local roster -> saved player standing in Eversong
```

This settles governance, TBC data compatibility, the UI runtime, the no-network
boot path, and the save contract before game rules multiply the cost of
changes.

Primary risks:

- **Presentation coupling.** The 1.12 field layout and `SessionEvent` stream
  run through about 100 to 280 app files. Mitigation: Stage A adapter now,
  typed components later.
- **UI runtime.** Moving the VM from 5.0 to 5.1 semantics and the 2.x handler
  convention touches every UI verb and test; secure templates are new.
  Mitigation: a dedicated phase before gameplay, measured by Lua error counts
  and 2.4.3 catalogue gates.
- **TBC format differences** in DBC schemas, M2 particles/ribbons and MPQ
  features that silently misparse. Mitigation: version checks that fail loudly
  and full-archive open counts.
- **Content conversion cost.** The selected source database's schema and semantics must be
  converted into the local content pack. Mitigation: start with one zone and validate each source
  table used by the slice.
- **Save-format churn.** Mitigation: minimal schema first, migrations per phase.
- **Upstream drift.** Mitigation: keep engine crates close to upstream; merge
  graphics work regularly rather than in large batches.
- **Provenance.** Imported server content needs a pinned source revision, license, extraction
  command, and conversion version. Client-install assets remain runtime-only and are never
  committed.

## Definition Of Done For The Vertical Slice

The vertical slice is complete when a player with the supplied TBC install can:

1. Launch one executable with no server configuration.
2. Create, select, and delete multiple Blood Elf characters.
3. Enter the Blood Elf starting area with any selected character.
4. Complete the defined NPC -> combat -> loot -> quest reward loop with the
   stock UI.
5. Quit, relaunch, and resume each character independently.
6. Play without any socket, auth service, realm list, source database, or install modification.
