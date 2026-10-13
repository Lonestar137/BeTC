# TODO

Research complete. Here's the synthesis — all findings are code-anchored hypotheses, verified where marked ✓, and none require capability changes to fix (every one is "same behavior, less work").
Performance improvement research — offline build
Baseline facts
- Budget: the project's own target is a 60 fps floor / 16.67 ms frame (benilla-app/src/perf/mod.rs:63). Nothing currently enforces it.
- Measurement infrastructure already exists and is strong — a perf investigation can start without building anything new:
- Ships in the offline binary: WOW_FPS_JOURNAL=<csv> (per-second frame ms, CPU ms, per-pass GPU buckets), WOW_NOVSYNC=1, frame pacing knobs, trace_chrome/tracy cargo features (benilla-offline/Cargo.toml:16).
- Dev build only: WOW_FRAME_PHASES, WOW_GPU_MS, WOW_UI_COST, WOW_HOVER_LOG, WOW_ANIM_COST, WOW_RIG_COST, WOW_INTERIOR_COST, WOW_GX_PERF, censuses, live FPS probes.
- Documented recipe: LIGHTING.md:197 (release build, never dev, WOW_NOVSYNC=1 + journal + WOW_GPU_PASSES=1).
- The codebase has a history of measured fixes (executor MT→ST chosen by measurement at lib.rs:307; torch depth already byte-compare gated; rig palette dirty-ranged). So the cheap wins are mostly gaps in that discipline, not greenfield.
GPU-side findings (likely the biggest framerate levers)

#	Finding	Anchor	Default state
G1	Sun shadow lane ≈5 ms/frame (their own measurement): 2048² map re-rendered every frame regardless of change, 9-tap PCF per lit fragment in 3 world shaders, CPU re-skin + full mesh re-upload at 30 Hz for shadow proxies despite an existing GPU skin palette (rig_palette.rs)	shadow_core.rs:83-131, shadow_hook.wgsl:44	ON
G2	Volumetric fog full-res raymarch: 24 steps/pixel × shadow-map compares, runs over sky pixels too (opacity 0 but the march still executes)	volumetric_fog.rs:595-778	ON (tier 1)
G3	Water: 2 full-res copy passes + heavy fragment shader (up to 256-light insertion sort per fragment, ~15 noise taps) — the colour copy exists for refraction/SSR which is off at this tier	liquid/scene_depth.rs:344, enhanced_water.wgsl:821	ON
G4	Post chain = ~5 full-res read+write passes (fog, shafts, bloom, grading, ffx-combine) that could be 2–3; grading and ffx-combine are both "sample scene, tiny math, write scene"	post/*, ffx_glow.rs:829	ON
G5	Torch cube shadows: 144 MiB depth array, 6 texture copies + 6 render passes per dynamic fixture per frame (24 MiB/frame copies)	torch_depth.rs:60-921	ON
G6	static_gx.wgsl samples all 3 wrap-mode textures unconditionally per fragment (3× fetches), though wrap mode is already a per-batch property → 3 pipeline variants would sample once	static_gx.wgsl:1384-1409 ✓	ON
Note the asymmetry: SSAO and volumetric light ship off with kill switches; the three costliest (fog, water, shadows) ship on. A quality-tier preset is a pure-capability-preserving lever (stock TBC had exactly these tiers).
CPU-side findings — world (benilla-world)

#	Finding	Anchor
W1	compute_wmo_pvs: full portal-graph flood per WMO instance every frame, incl. per-instance matrix inverses and Vec allocs — comment admits "a parked camera's flood repeats its answer" but only the write is gated, not the recompute ✓	wmo_portal/mod.rs:291-488
W2	Interior classifier: 0.01 yd resample threshold (RESAMPLE_DIST_SQ = 1e-4) means every moving anchor re-resolves every frame, scanning all resident WMO instances with ray work. The sibling room claim uses 0.25 yd², showing the coarser grain suffices ✓	interior.rs:30,430-545
W3	3 more independent per-frame O(all-instances) sweeps (track_area_interior, track_current_interior, sync_interior_volumes) re-deriving eye-column state W1 already computed; sync_interior_volumes builds 2 HashSets per frame over every registered placement	wmo_portal/interior.rs:101-212, terrain_stream.rs:1199
W4	3 full-population submesh walks/frame (~100k parts in city content): apply_model_visibility, apply_exterior_cull (no still-gate at all + double frustum build), park_hidden_parts — each cheap per part but not hierarchical/incremental	model_render/visibility.rs:21, exterior_cull.rs:271, park.rs:32
W5	static_gx::cull_cells: full retained-scene walk/frame + dozens of fresh Vec<bool> allocs + sorts, even for a still camera	static_gx/cull.rs:138-559
W6	ExteriorGate + frustum rebuilt ~6×/frame across 6 systems (each = matrix inverse + frustum construct)	exterior_cull.rs:145
W7	build_light_data: all-lights scan + collect/sort/8.5 KB copy per frame	lighting/global_light.rs:1316
W8	billboard_joint_palette: 4 heap allocs + 2 HashSet builds per rig per frame, ungated	billboard.rs:292-390
Root causes: (a) eye-column/portal state recomputed independently by 3+ systems per frame; (b) verdicts recomputed for every entity every frame whenever the camera moves (most frames) instead of hierarchically or on change.
CPU-side findings — game/adapter (offline-specific)

#	Finding	Anchor
O1	Hover picking CPU-skins + triangle-ray-tests every visible unit's mesh every frame, and repeats the whole pass inflated when the cursor hits nothing (common: pointing at ground). O(visible units × verts) per frame — verified ✓	target/hover.rs:179-437 ✓
O2	Full-roster TOML encode on the frame thread at combat-change frequency: save_after_command → queue() synchronously encode_snapshots every character + profile (double toml::to_string + SHA-1 each); only the file write is off-thread. Every CombatUpdate sets changed ✓	offline/persistence.rs:29-61 ✓, offline.rs:2680 ✓
O3	sync_corpse_respawns builds a BTreeMap over all hostiles every frame; advance_combat also loops all hostiles per frame	game-core/lib.rs:4677
O4	advance_offline_patrols: O(patrols × hostiles) linear scans per frame, twice	offline.rs:1851-1904
O5	Net dispatch: world.run_system_with per handler per event + SessionEvent clones + remove/insert resource per frame	net/handlers.rs:60-106
O6	Re-emitted unchanged ObjectValues still mark ObjectStore changed (Bevy tick), waking every Changed<ObjectStore> consumer → UI gate reopens	net/objects.rs:1059
O7	update_active runs twice per frame (String clones + quest-journal scans); publish_body_proportions rebuilds a BTreeMap of all snapshots per frame	offline.rs:2051,4141,167
O8	drive_animations: full state machine over every animated unit/frame; the code itself notes "resting rows … no skip is taken"; linear clips.iter().find	creature_anim/driver.rs:411
O9	Nameplates + vplates + combat_text: 3 all-unit passes/frame each recomputing overhead_anchor + faction reaction	nameplates.rs:354, vplates.rs:300
CPU-side findings — UI/Lua

#	Finding	Anchor
U1	Fading chat lines defeat the extract gate → full UI re-conversion (all text re-laid out) every frame for the whole fade window; Text entries are explicitly not spliceable ✓	ui_script/extract/mod.rs:290 ✓, messageframe.rs:253
U2	extract() rebuilds the render list per frame and clones RegionData (Strings + anchor Vec) per visible region even on settled frames	benilla-ui/src/script/extract.rs:242
U3	Unnamed layout touches force a full graph derive (~30× a solve) — code says "expected zero", but live touch sites are reachable per frame	model.rs:1518, ui_script/mod.rs:86
U4	feed_action_state recomputes every action slot's state (bag walks, usability, range) every frame unconditionally — unlike its sibling feeds which are gated	ui_action/state.rs:73-343
U5	Lua dispatch fixed overhead: ~15–20 C-API string-key ops + 2–3 allocs per handler fire; OnUpdate walk rebuilt + sorted per tick	script/event.rs:194-276, tick.rs:85
U6	UI mesh rebuild copies the whole vertex stream several times; hit-test re-traverses and rebuilds the scroll-clip map per frame	ui_pass.rs:730, pointer.rs:154
Cross-cutting themes
1. "Write-gated but not recompute-gated" — most expensive verdicts are pure functions of (camera, static scene) and recomputed fully each frame (W1–W5, G1).
2. Repeated per-frame full-population walks that could share one pass (W3, W4, O9, U6).
3. Allocation churn in per-frame loops (W5, W8, U2, O3) — cheap individually, visible in aggregate.
4. One bad actor defeats a good gate (U1 chat fade, O6 unchanged-field emissions, O2 combat saves).
5. Missing quality tiers for the three heaviest GPU lanes (G1–G3) — stock TBC shipped such tiers, so adding one is capability-compatible and arguably fidelity-positive.
Suggested next steps (measurement first, per docs/METHOD.md)
1. Baseline: release/play build per LIGHTING.md:197, WOW_NOVSYNC=1 + WOW_FPS_JOURNAL + WOW_GPU_PASSES=1 in a representative scene (Eversong + combat + chat + city), with the existing cvar A/Bs (volumetricFog 0/1/2, waterQuality, shadowFilter 0/1, characterShadows 0) to attribute GPU shares — no code changes needed.
2. CPU attribution: trace_chrome build (works without dev), or a dev build for WOW_FRAME_PHASES + WOW_UI_COST + WOW_HOVER_LOG + WOW_ANIM_COST + WOW_INTERIOR_COST + WOW_GX_PERF.
3. Then implement in slices, biggest measured win first, gated by the existing FPS journal as a regression check. My predicted ordering of measured wins: G1/G2/G3 (GPU) ≈ O1/O2 (CPU spikes) > U1/U2 (UI) > W1–W5 (world).
Nothing has been changed in the tree. If you want, I can next run the baseline measurement pass (step 1) and turn this into a ranked, numbers-backed implementation plan.
