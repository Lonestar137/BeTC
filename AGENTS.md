# benilla-everwood

An offline, single-player TBC 2.4.3 (build 8606) game in Rust and Bevy. The player's own TBC
install supplies client data and presentation; this project supplies the local game simulation
and converted TBC gameplay content. This is a fork, not an upstream benilla contribution.

Read, in this order:

1. `docs/METHOD.md` — binding project rules and implementation method.
2. `PLAN.md` — product decisions, phases, and acceptance criteria.
3. `docs/MAP.md` — generated inventory of what the code currently builds; never edit by hand.
4. `docs/CONTRIBUTING.md` — scope, gates, and local setup.

The install is runtime input only: never commit its files or write into it. The shipped game is
fully offline: do not add runtime sockets, auth, realms, server processes, or external databases.
Conversion tooling may ingest a pinned TBC server-content database or export to build the local
content pack; the executable must ship and run without that source database. The existing networked
Vanilla path is frozen and may be removed when it blocks the TBC offline path.

@docs/METHOD.md
