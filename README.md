# Genetic Cars

A local experiment workspace for evolving neural driving controllers. Rust owns the service and a native Metal engine; a static Next.js / React application plays completed generations in Canvas2D.

## Run

Requires Apple Silicon, macOS 15 or later, Rust via rustup, Bun 1.3.2, and Node 20.9+ for the Next.js build. Open the application in Chrome.

```sh
bun run start
```

Open **http://127.0.0.1:8501**. The first start downloads dependencies and builds the application. To launch an existing build directly:

```sh
./target/release/genetic-cars
```

Optional server arguments: `--port 8501`, `--data-dir PATH`, `--frontend-dir PATH`. Experiments default to `~/Library/Application Support/Genetic Cars`; `GENETIC_CARS_DATA_DIR` overrides that location.

Set cars, laps (1–100, default 5), and generations per circuit (default 50), then **Start driving**. Each run trains one population through all six circuits in a seeded random order. At each transition the previous population arrives unchanged: compare its arrival finish rate with its rate after learning the new circuit. Advanced vehicle and genetic settings remain under **Advanced settings**; leave the seed blank for a fresh run. Training continues when you close the browser. Stop discards uncommitted work; resume and server restart retain the tour and last checkpoint.

Playback follows completed generations independently of training. Auto-advance visits each circuit’s arrival and final population; the tour cards open either replay directly. Pan, zoom, follow or inspect cars in the same viewport. Progress, survival, generation time and fastest-lap statistics remain available.

Valid laps compete against the all-time champion for the same circuit geometry and vehicle settings. Finishers are selected by completion time; other cars by progress. Lap times use 30 simulation steps per second and fractional timing-line crossings, independently of playback speed. A cyan ghost follows the reference champion, aligned to the viewed car’s lap. Records and their single-lap ghosts are permanent and survive replay-cache eviction; champions never enter the breeding population.

Tours and champions are stored in the stable `race-lab.sqlite3` database. Earlier `experiments-v3.sqlite3` and `experiments.sqlite3` files are preserved untouched and are not imported.

## Engine and storage

- Metal computes sensors, the 5–3–1 tanh controller, swept point-car movement, collisions, fitness, ranking and mutation. Rust prepares validated road geometry and its BVH once, then schedules bounded GPU blocks. There is no CPU simulation backend. Native Metal kernels fuse geometry and inference without a separate MPS tensor pipeline.
- A run records its seed, immutable parameters, circuit order and roads, engine identity and parameter-major f32 population checkpoints. Philox randomness depends on explicit seed/generation/car counters. This is an f32 engine contract; checkpoint reuse requires the same engine/shader/device identity.
- SQLite commits each completed generation and its next population atomically. Summaries and status changes reach the browser through resumable SSE. Preview messages contain at most 128 terminal cars.
- Replays and selected-car readings use the same GPU kernel. While training and replay both need work, the owner schedules three training blocks per replay/inspection block. Replay generation therefore shares GPU time with training; downloading stored chunks does not.
- Replay chunks contain up to 32 frames. A 16-byte little-endian u32 header (`version=2, firstFrame, frameCount, carCount`) precedes frame-major f32 `[x, y, heading]` values. Replay and inspection data retain at most 10,000 samples, including both endpoints; the manifest carries the sampling stride and final simulation step. The browser decodes only the chunks around the playhead and relies on the HTTP cache for revisits; the server bounds derived replay/inspection data at 2 GiB. Experiment history and checkpoints are retained.

## Development

```sh
bun run build       # Rust release binary, generated TS contracts, static frontend
bun run check       # Rust formatting/lints and frontend checks
bun run smoke       # One local GPU workflow; build first
```

`backend/src/model.rs` defines the wire types; `bun run types` regenerates the TypeScript declarations. Backend code lives in `backend/src`; the Canvas player lives in `frontend/src/lib/race-player.ts`. Bun manages packages and commands; Next's build CLI uses Node because Bun 1.3.2 cannot run this Next release's build internals. The server serves `frontend/out` and `/api` from the same local origin. No Node production server is required.
