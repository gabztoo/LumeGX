# VIctory · Technical reference

Runs Grand Theft Auto VI (Xbox Series X|S) natively on Windows, from files
the user supplies.

The title's x86-64 code runs unchanged inside our process; VIctory supplies
the platform underneath it and converts the title's data, ahead of time where
it can and on first use where it must (shaders). See
[architecture.md](architecture.md).

## Status (2026-11-23)

**Not playable yet.** The title boots, runs its menus at its own 30 fps and
loads the world, where rendering fails.

| Stage | State |
|-------|-------|
| Boot, first-boot screens (accessibility, language, settings, legal) | works, 30 fps |
| Intro video with sound | works |
| Start screen, profile and saves | works |
| Loading into the world (about 50 s) | works |
| The world (first mission's intro) | reached and runs with indirect draws; its title card renders, the scene after it is still dark; 0.5-1.8 s per frame |

Tested on one machine: Windows 11, AMD RX 6700 XT.

> **Warning.** Entering the world on a real GPU can currently hang it, and
> Windows then resets the display driver (every monitor goes black for a
> moment). To look further, use a diagnostics build with
> `VICTORY_D3D12_WARP=1`, which renders on the software adapter.

What each part does today and what it still needs:

| Subsystem | Today | Needs |
|-----------|-------|-------|
| Loader | Runs the title's executable and its DLLs in our process; crash reports name the faulting code and are kept in `crashes.log` | – |
| Runtime services | One signed-in offline user, licences, packages, saves that survive a kill at any step, text entry dialog | Speech is silent; the dialog and relaunch are untested live |
| Network | Offline by design: the title cannot reach the network | – |
| Files and streaming | Archives and streamed data read, zlib and BCPack decoded in parallel | Loading is limited by writes into title memory |
| Input | XInput pads, keyboard, keyboard stand-in pad | Trigger rumble |
| Audio | Menu, video and loading sound; XMA voices and Opus streams decode | Reverb is silent; Opus results unverified against the title |
| GPU: menus | Draws, compute and presentation on host D3D12; only changed title memory is copied | Frame pacing drifts on one screen in release builds |
| GPU: world | Mesh pipelines, shader-written buffers, descriptor lifetimes, indirect execution (arguments clamped), pipelines made by worker threads and kept on disk, the depth bounds test where the GPU has it | Most of the scene is still missing (untranslated shaders drop draws); frames take seconds; real queries, HDR and scaling are missing |
| Shaders | Console GPU code recompiled to DXIL at first use, cached on disk. Menus: all. World: mesh 8,191 of 8,506, vertex/pixel 10,617 of 12,346, compute 1,643 of 2,539 | 64-bit image atomics, merged geometry stages, the ray-tracing builder's shaders; some compute programs need a GPU with 64-lane waves |
| Textures | Console tiled layouts and detiling for the modes the title uses | Render-target metadata planes |
| Series S mode | Runs through the menus to the loading screen | The world, as above |

## Getting started

Requirements: Windows 10/11 x64, a CPU with AVX2, a Direct3D 12 GPU with
shader model 6.6 (and mesh shaders for the world), Rust (pinned in
`rust-toolchain.toml`).

The title also needs several GiB of available Windows commit at startup.
Check **Committed** under **Task Manager → Performance → Memory**. If
`crashes.log` reports Windows error 1455, close other applications or increase
the Windows paging file size, then retry.

```powershell
cargo xtask install-hooks                # asset-policy check on every commit
copy victory.example.toml victory.toml   # then set your paths (outside the repo)
cargo test --workspace                   # needs no game data
cargo run -p victory -- config           # check your paths
```

Run the title:

```powershell
cargo build --release -p victory
target\release\victory.exe run <game dir>\<the title's exe> --execute
```

The window caption shows the framerate. Saves, settings and `crashes.log`
are kept under the data folder (default `%LOCALAPPDATA%\victory\data`), derived
data under the cache folder.

Input: XInput pads. With none connected the keyboard is a pad while the
title's window has focus:

| Keys | Pad |
|------|-----|
| W A S D / I J K L | left / right stick |
| arrows | D-pad |
| Enter, Backspace, X, Y | A, B, X, Y |
| Q / E, Z / C | shoulders, triggers |
| F / H | stick clicks |
| Esc, Tab | Menu, View |

## Commands

```
victory inspect <exe|dll> [--imports] [--exports]
victory census [paths...] [--missing] [--strict]
victory lib-exports <import.lib> [--list]
victory shader-info [paths...] [--verbose] [--disasm]
victory translate [paths...] [--validate] [--pipelines] [--list] [--jobs N]
victory run <exe> [--execute] [--unimplemented trap|zero] [--strict]
            [--console series-x|series-s] [-- <title arguments>]
victory config
```

Logging: `VICTORY_LOG=victory_loader=trace,info` (a release build logs
info and above).

## Debugging

A release build has no debugging aids. Build them in with

```powershell
cargo build --profile diag -p victory --features diagnostics   # target\diag\victory.exe
```

| Variable | Effect |
|----------|--------|
| `VICTORY_LOG=<target>=debug,...` | debug and trace output |
| `VICTORY_STACKS=20,60` | every thread's stack at those seconds |
| `VICTORY_WATCH=<rva>,...` | logs a title u32 when it changes |
| `VICTORY_KEYS=30:Enter,35:X` | presses keys at those seconds, focus or not |
| `VICTORY_CRASH_SAVE=<rva>:<len>,...` | saves title memory to the cache folder on a crash |
| `VICTORY_CAPTURE=<dir>` | saves presented frames (powers of two, or `VICTORY_CAPTURE_FRAMES=<list>`; `w+K` in the list is the K-th frame after the first with over 400 draws, the world's, logged as `first frame with over 400 draws`) |
| `VICTORY_DUMP_TARGETS=1` | with the two above, saves every render target of those frames |
| `VICTORY_PRESENT_COMPARE=<w>x<h>-f<format>` | probes matching targets in the same list as each present, against the output ([present-comparison.md](present-comparison.md), which also shows how to find the scene's texture from the traces of a listed frame) |
| `VICTORY_D3D12_DEBUG=1` | host debug layer |
| `VICTORY_D3D12_GBV=1` | with the above, GPU-based validation (bad descriptor use reported, not faulted; slow) |
| `VICTORY_D3D12_DRED=1` | on a GPU failure, the allocation hit and the commands in flight |
| `VICTORY_SKIP_PIPELINES=<hash>,...` | omit draws and dispatches, including indirect ones, whose pipeline contains a listed 16-digit shader hash |
| `VICTORY_SHADER_DUMP=1` | writes the title's shaders that do not translate to `untranslated` in the cache folder, for `victory translate` and `victory shader-info --disasm` (outside the repository; never commit them) |
| `VICTORY_DUMP_SHADERS=<hash>,...` | writes these shaders (16-digit hashes as the logs print them) to the same folder even when they translate, e.g. the ones a device-removed report names |
| `VICTORY_GPU_TIMING=1` | with the above, each list of a submission run and timed on its own; slow lists logged with their slowest commands |
| `VICTORY_NULL_VIEW_STRIDES=<stride>,...` | (diagnostics builds) buffer views of these strides, decimal or `0x` hex, are left null, to confirm in the game which ones a pass needs |
| `VICTORY_HEAP_USAGE=1` | translated pixel and compute shaders mark the heap slots of the texture and buffer views they read from the heap itself (off by default; upstream #1962 has it on). A marked placed texture is made after the list that read it and uploaded by the next one that marks; a marked buffer view is made and brought up to date once per frame for programs reading the heap itself (`heap buffers:` line). Without the marks they stay null views. Changes the translation: the shader cache keys on it |
| `VICTORY_VIEW_STRIDES=1` | (experimental, off) buffers read by element with no stride the shader names are read raw at element × the view's stride, from a host view-stride table (`t255`). The device hung during loading with it on the RTX 5050. Changes the translation: the shader cache keys on it |
| `VICTORY_PIPELINE_STATS=1` | (diagnostics builds) a pipeline-statistics query around every graphics draw; every 5 s, per pipeline (shaders and target count, `+d` with depth): draws, vertex or mesh invocations, primitives made, primitives into and out of the clipper, pixel shader invocations, and for indirect draws the first record as the GPU read it (records with no work, mesh records with groups in x but none in y or z, work asked): `pipeline stats: ...` lines, the first summing all |
| `VICTORY_PRODUCERS=1` | (diagnostics builds) logs once per pair a draw or dispatch that runs reading what only dropped work would have written: `reads what only dropped work writes reader=... dropped=... what=... frame=N` (the frame being built is N+1); work that runs and writes the range clears it. The dropped writer carries why it was dropped, as the frame lines' `dropped N/frame: ...` count it: `pixel <hash> (being made)`, `(a rectangle list drawn indirectly)`, `(pipeline made, root UAV not on 16 bytes of its host copy)`... |
| `VICTORY_D3D12_WARP=1` | host device on the software adapter (slow; cannot reset the driver) |
| `VICTORY_DESCRIPTOR_POOL=legacy` | dynamic descriptor tables are searched and sized as before (no free run across the search cursor, a fresh table from entry 0), for A/B runs with one binary; the line `dynamic descriptor slots: ... tables refused`, every 5 s, counts the refusals since the previous one (fewer free slots than the table needs: exhaustion; enough but no run that long: fragmentation), the draws dropped for them, the dynamic slots taken (now and at most) and the tables made afresh for reaching past a capped heap's mirrored slots |
| `VICTORY_ROOT_SRV_DIRECT=1` | root SRVs are bound as root descriptors (no bound), as before they were tables of one raw view ending at the buffer's host copy's end, for A/B runs with one binary; the line `root SRVs: ...` counts, every 5 s, the bounded views made, those reading zeros, slots refused and draws dropped with no view heap |
| `VICTORY_CAPPED_DYNAMIC=<n>` | a view heap the host caps at 1,000,000 slots (NVIDIA) keeps `n` dynamic slots instead of 262,144 (65536 is the split before: 934,464 mirrored title slots); tables reaching title slots past the mirrored part are made afresh |
| `VICTORY_DEPTH_BOUNDS=0` | the title's depth bounds test is left out (pipelines without it, the bounds lists set ignored), as before it was supported; the log line `depth bounds: ... draws/frame with the test` says how many draws use it |
| `VICTORY_SHADER_LOOP_LIMIT=<n>` | translated shader loops stop after `n` iterations per lane instead of 2^20 (own shader cache entries); if a lower `n` makes world frames much faster, loops hitting the limit are what makes them slow |
| `VICTORY_AUDIO_CAPTURE=<file.wav>` | records what the title plays (our process only) |
| `VICTORY_XMA_DUMP=<dir>` | saves the first 32 XMA voices' context and input |

Every build appends crashes and host GPU failures to `crashes.log` in the
title's data folder.

## License

GPL-3.0-or-later. See [LICENSE](../LICENSE).
