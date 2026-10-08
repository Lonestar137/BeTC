# Contributing

benilla-everwood is a fork building an offline, single-player TBC 2.4.3 game. Keep work aligned
with `PLAN.md`: the client install is the reference for data, formats, UI, and presentation;
simulation rules and game content are project-owned. Do not add network/server requirements or
commit files from a WoW install.

## Scope

The current target is the Blood Elf start on map 530: Sunstrider Isle, the route through Eversong
Woods, and access to Silvermoon City. Follow the plan's phases and acceptance criteria. Prefer a
small, independently verifiable step over a mixed rewrite.

The inherited 1.12 client is not a compatibility target. Its networked mode is frozen and may be
deleted when it blocks offline TBC work. The inherited `reference/1.12-*.tsv` catalogues and
associated gates remain frozen until the plan replaces them with build-8606 references.

## What gets in

- TBC 2.4.3 data and presentation behavior established from the player's own build-8606 install.
- Offline game simulation, persistence, and original content that meet a documented plan
  decision and have deterministic tests.
- Fixes with a reproducible before/after behavior or a failing/passing test.
- Format support validated against actual build-8606 files without committing those files.

## What does not

- Auth, sockets, realms, server processes, external databases, or TBC server compatibility.
- Bundled Blizzard assets, client code, FrameXML/GlueXML, or copied server-emulator content.
- Unplanned features outside the vertical slice, such as flying, Outland gameplay, instances,
  multiplayer, PvP, or professions.
- A test that silently depends on a developer's install, a host clock, or nondeterministic RNG.

## How a change is judged

1. The change satisfies a task or acceptance criterion in `PLAN.md` and stays within the current
   phase unless it is a necessary prerequisite.
2. TBC client facts cite the observed client behavior or the relevant build-8606 data/UI source.
   Project-owned rules and content state their design and provenance.
3. Tests cover the behavior deterministically. Runtime claims are backed by a bounded run or
   numeric instrument, not only compilation or inspection.
4. Run `scripts/check.sh` for the round. Phase 0 replaces the inherited chain with offline
   `scripts/gates.sh`; during that migration, report inherited gates that are not yet relevant or
   cannot run, and do not substitute a server smoke test for offline startup verification.
5. Never write into the install. Phase 0 adds a gate that compares its file manifest before and
   after a run.

## Setting up

- **Toolchain.** Use the stable Rust toolchain from `rust-toolchain.toml`, including rustfmt and
  clippy, plus the platform's C build dependencies for vendored native libraries. `python3` is
  used by repository scripts.
- **TBC install.** Provide a legally obtained 2.4.3 build 8606 install through `WOW_DATA`, or the
  documented dev-build `WoW` link. Do not assume a locale; archive and locale discovery are part
  of Phase 1. `WOW_DATA=` explicitly means no install and is used for no-data tests.
- **Without the install.** Unit tests and gates must remain useful. Data-dependent tests declare
  and report their skip; where data exists, `BENILLA_REQUIRE_DATA=1` makes skips fail.
- **No server setup.** The offline game requires no account, server, realm, or network variables.
- **Development loop.** Use `scripts/check.sh` for a work round and `scripts/gates.sh` for the
  full chain. The target clean-run check boots the local roster and enters the world without any
  socket. Follow `PLAN.md` for the offline launcher transition.

Working with an AI agent is expected. It reads `AGENTS.md`, and these rules bind it too.
