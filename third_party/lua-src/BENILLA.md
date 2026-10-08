# benilla's `lua-src` fork: what differs from upstream, and how to check

Upstream: [`mlua-rs/lua-src-rs`](https://github.com/mlua-rs/lua-src-rs), MIT, version `550.0.0`,
the version mlua 0.11 resolves to. Wired in through `[patch.crates-io]` in the workspace root.

## Why a fork exists at all

benilla's UI runtime hosts stock TBC 2.4.3 FrameXML and legacy UI libraries. It accepts TBC's Lua 5.1
grammar while retaining selected Lua 5.0 compatibility behavior required by older UI code.

The 1.12 addon corpus is Lua 5.0 code. It uses the iterator-less generic-for:

```lua
for k, v in someTable do ... end        -- no pairs(), no iterator function
```

183 of 218 corpus addons are reached by it (118 carry it, 65 inherit it through a declared
dependency), across 1,163 sites. On stock Lua 5.1 it raises `attempt to call a table value`, the
first session-start error for 60 of the 218.

Lua 5.1 removed it at the opcode level, which is why no layer above the VM can reach it: a `__call`
metamethod would need a per-type default metatable for tables, which 5.1 does not have; rewriting
the chunk source means parsing Lua with a regex; and `lua-src` ships 5.1 through 5.5, every one of
which removed it.

## Grammar compatibility

benilla stays on Lua 5.1.5. TBC vararg expressions and the length operator remain enabled for the
installed FrameXML. `LUA_COMPAT_VARARG` also exposes the implicit `arg` table used by older UI
libraries. The iterator-less generic-for remains an opcode-level Lua 5.0 compatibility rule. The
5.0-shaped standard library is a separate layer in `benilla-ui` and must be expanded where TBC
FrameXML requires it.

## The delta, and the command that proves it

Eight hunks in three files. `src/lib.rs` additionally differs by having Lua 5.2/5.3/5.4/5.5 stripped
from the `Version` enum (with their source trees deleted): benilla builds 5.1 and only 5.1, and a
fork that still offered the other four would answer a request for one with a missing directory at
build time instead of a compile error.

Five hunks restore what 5.1 changed or removed:

| file | hunk | why |
|---|---|---|
| `lvm.c` | 5.0's `OP_TFORPREP` table-to-`next` substitution, folded into the top of `OP_TFORLOOP` | `for k, v in someTable do` raised "attempt to call a table value" |
| `luaconf.h` | `LUA_COMPAT_LSTR` `1` to `2` | 5.1 kept 5.0's `[[...]]` nesting machinery and put an advisory error in front of it; two corpus addons died on "nesting of `[[...]]` is deprecated" |
| `luaconf.h` | `LUA_QL(x)` back to 5.0's `` `x' `` from 5.1's `'x'` | every Lua error message quotes a program element, and `WoW.exe`'s own `.rdata` carries all five formats in the 5.0 spelling |
| `lparser.c` | 5.0's compat-semicolon skip restored at the top of `constructor()`'s field loop, 5.0's own line verbatim | one extra `;` after a field separator inside a table constructor (`Back_Title = AL["Factions"];;`, which AtlasLoot writes) is accepted by the client and was a parse error here |
| `lparser.c` | `recfield`'s `cc->nh++` moved back inside its `TK_NAME` arm, 5.0's own placement | a `[expr] = value` constructor field credits neither `OP_NEWTABLE` size hint in 5.0, so the table is born on the dummy node |

Three parser features required by TBC FrameXML remain enabled:

| file | hunk | why |
|---|---|---|
| `lparser.c` | `simpleexp` accepts `TK_DOTS` as a vararg expression | TBC FrameXML forwards `...` from event and helper functions |
| `lparser.c` | `getunopr` accepts `#` as `OPR_LEN` | TBC FrameXML uses the Lua 5.1 length operator |
| `lparser.c` | `getbinopr` accepts `%` as `OPR_MOD` | TBC FrameXML uses Lua 5.1 modulo expressions |

The grammar intentionally accepts TBC vararg, length, and modulo syntax alongside the legacy iterator-less
generic-for. Library functions, event-handler calling conventions, and secure-frame APIs remain
separate compatibility requirements and must be validated against build 8606.

To verify the Lua sources against upstream at any time:

```sh
# 550.0.0 is the pinned version; adjust the path if cargo's registry hash differs.
diff -r third_party/lua-src/lua-5.1.5 \
  ~/.cargo/registry/src/*/lua-src-550.0.0/lua-5.1.5
```

That must print exactly the eight hunks in the tables above and nothing else.

## The generic-for, in detail

The `lvm.c` comment carries the three details that are the client's own, each byte-read there: the
substitution test is a bare type-tag equality that never consults a metatable (a table carrying
`__call` still gets `next`); the callee is the global `next`, read raw and fetched fresh at every loop
entry (an addon assigning `next = myfn` changes every later generic-for in the session); and userdata
is not substituted. The substitution happens in `OP_TFORPREP`, an opcode 5.1 deleted, so the top of
`OP_TFORLOOP` is where 5.1 can host it.
