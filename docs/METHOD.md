# METHOD — how benilla-everwood is built

benilla-everwood is an offline, single-player game targeting World of Warcraft: The Burning
Crusade 2.4.3, build 8606. The player's own install is the reference for client data formats,
assets, UI, and presentation. The project owns its simulation rules and converted world content;
there is no server and no network protocol on the game path.

## The rule for every change

Make one small, tested step toward the vertical slice in `PLAN.md`. Use the TBC client as evidence
for client behavior and data. State project-owned gameplay decisions explicitly in the rules or
content schema. Keep the install read-only and external, keep the executable offline, and run the
appropriate gates. The plan's phase acceptance criteria determine when a phase is complete.

### Fidelity default

Unless this project explicitly records a different offline gameplay rule, reproduce TBC 2.4.3
behavior and content semantics. This applies to UI, presentation, class progression, trainers,
spells, quests, items, and world interactions. Multiplayer, runtime transport, server processes,
accounts, realms, and runtime external databases are permanent exceptions: their gameplay authority
is local and their source content is converted into a versioned local pack.

## The loop

1. **Establish the fact.** For data, formats, UI, or presentation, use build 8606 files or observe
   the TBC client. For game rules and content, record a project-owned decision and its provenance.
2. **Implement one slice.** Keep the game simulation independent of Bevy presentation and client
   protocol types. Use the adapter boundary in `PLAN.md` while the inherited presentation is being
   replaced incrementally.
3. **Measure behavior.** Use deterministic tests, traces, and numeric probes. Visual captures are
   useful for appearance, not timing or ordering. Do not treat a successful build as evidence of
   runtime behavior.
4. **Verify the changed surface.** Run `scripts/check.sh` for a round and the full offline
   `scripts/gates.sh` before a land. A live run must prove the intended startup and gameplay path.
5. **Keep changes small.** Work on a branch, preserve unrelated worktree changes, and report the
   verification performed with each completed piece.

## Compile-efficient rounds

Treat a round as one cohesive, testable behavior slice, not one compile per edit. Before the first
build, trace the behavior through its relevant layers, establish client facts from build 8606,
identify the production and test files, and decide which command will verify the whole slice. Then
make the connected implementation and regression-test edits before compiling. Do not bundle
unrelated behavior just to amortize a build.

- On a Nix system, run every build, test, clippy, and check/gate command inside the repository's
  `nix-shell` so native dependencies and runtime libraries come from `shell.nix`.

- Prefer one test invocation naming all affected packages and the intended feature set. When
  packages share dependencies, this lets Cargo build their common graph once. Keep profiles and
  feature flags stable between runs so the artifacts are reusable.
- Avoid running `cargo check` and then the same tests solely as successive preflights; choose the
  check that answers the question for the round. A test build already type-checks its target.
- If a build or test fails, use its output to inspect and fix the whole failure set before the next
  build, unless a compile error or new client evidence requires narrowing the implementation first.
- For repeated interactive presentation checks, prefer the workspace's incremental `play` profile
  where suitable (for example, `cargo run --profile play -p benilla-offline`) rather than rebuilding
  the test profile for each visual iteration. Keep deterministic tests as the behavioral gate.
- Run `scripts/check.sh` once the cohesive round is ready, and the full land gates at the land step.
  Avoid cleaning build artifacts or changing profiles and feature combinations without a measured
  reason; either can turn a warm iteration into a cold build. If test compilation dominates, measure
  any faster test-profile proposal separately, including its effect on test runtime.

## Hard rules

- **Offline means no runtime transport or database.** The shipped executable must not open auth or
  world sockets, launch a server process, or require accounts, realms, or an external database.
  Offline conversion tooling may read a pinned TBC server-content database and emit the content
  pack the game consumes. Compile network,
  auth, SRP, Login, and Realm Select out of the offline binary rather than relying on a runtime
  switch.
- **The install is read-only runtime input.** Resolve the player's TBC install through `WOW_DATA`
  (or the documented dev-build link). Never write into it and never commit its assets, interface
  files, or data. Phase 0 adds a gate check that the install is unchanged across each run.
- **Imported TBC world content is permitted.** Creature templates, spawn placements, quests, text,
  and vendor data may be converted from a compatible TBC server-content dataset for this offline
  port. Record the source, revision, license, extraction command, and conversion version with every
  imported content set. Never write to or commit assets from the player's client install.
- **Local state has one root.** Saves and settings live under `benilla-config/`; paths resolve
  through `crate::local_state`. Per-character saves use stable generated IDs, not display names.
- **Simulation is deterministic.** `game-core` owns game time and seeded randomness. Tests inject
  time and RNG state; gameplay does not depend on host wall-clock time.
- **Presentation follows the client.** Load the stock 2.4.3 GlueXML/FrameXML from the player's
  install. Do not bundle a replacement UI or add project-only addon APIs for the slice.
- **License reflects provenance.** Our code is MIT OR Apache-2.0; no original client code or
  bundled client assets.

## Where things live

- `PLAN.md`: product decisions, phase tasks, and acceptance criteria.
- `docs/METHOD.md`: binding engineering and provenance rules; never a work log.
- `docs/MAP.md`: generated inventory of the current code, produced by `scripts/genmap.sh`; never
  edit by hand.
- `crates/`: Rust workspace; `reference/`: inherited Vanilla catalogues, frozen until replaced
  by build-8606 catalogues; `scripts/`: gates and instruments; `third_party/`: vendored code.
- Git history records changes. Do not add progress logs or status narratives to the repository.

## Gates

Phase 0 replaces the inherited gate chain with an offline chain that does not require a server.
The target chain is: format, clippy with warnings denied, workspace tests with and without client
data, documentation and render-pass lints, player/offline build and tests, and offline engine boot
checks. Data-gated tests may skip without an install; when TBC data is present,
`BENILLA_REQUIRE_DATA=1` makes a skip a failure. The completed chain compares the install before
and after each run.

Use `scripts/check.sh` for each implementation round. Until Phase 0 has replaced inherited
network-dependent or Vanilla-only gates, state any gate that could not meaningfully verify this
fork; do not treat a legacy live-server smoke test as an offline-game check.

The fourth gate is a clean offline run: boot the local roster, enter the world, and exit without a
socket or server. Platform-specific code and dependencies must be built on their target platform
before claiming that platform is supported.
