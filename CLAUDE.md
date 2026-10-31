# CLAUDE.md

This file provides guidance to Claude Code (claude.ai/code) when working with code in this repository.

## What this is

A Rust (axum) drop-in replacement for the Valhalla / Stadia Maps `trace_attributes` endpoint, as
consumed by Pédalons' `pedalons` backend (`StadiaTraceAttributesClient` / `TraceSurfaceComputer`).
Instead of a Valhalla graph, it reads VersaTiles **Shortbread 1.0** vector tiles (z14, `streets`
layer), fetched on demand and cached. The response must stay byte-for-byte compatible with what
pedalons expects from Stadia (fields, Valhalla error codes, 503 for transient failures).

The README is the detailed spec (attribute derivation tables, error codes, env
vars, known limits, eval results). Code and doc comments are in English. Keep the README in sync
when behaviour, env vars, error codes or eval figures change.

## Commands

```bash
cargo build --release
cargo test                                   # all offline (fixture tiles, local tile server)
cargo test --test api                        # integration tests through the axum router
cargo test --lib matcher                     # unit tests of one module
cargo test <name_substring>                  # single test
cargo clippy --all-targets

cargo run --release                          # serves on 0.0.0.0:8002 (POST /trace_attributes/v1, GET /health)
cargo run --release --bin eval               # agreement with Stadia on testdata/*.route.json → eval/report.md
EVAL_VERBOSE=1 cargo run --release --bin eval        # list every disagreement ≥ 40 m
cargo run --release --bin eval -- --url http://localhost:8002
cargo run --release --bin inspect -- near <lat> <lon> [radius]       # streets + attributes around a point
cargo run --release --bin inspect -- trace <route.json> <from_m> <to_m>   # matching of a stretch (CANDS=1 for candidates)
cargo run --release --bin inspect -- fixture <route.json> <from_m> <to_m> <out.json> <tiles_dir>
```

`eval` and `inspect` hit the network unless the tiles are in the disk cache (`CACHE_DIR`, default
`./.cache/tiles`). All configuration is env-driven (`src/config.rs`, table in the README), including
the matcher tuning knobs `MATCH_*`.

## Architecture

Request pipeline, orchestrated by `trace::Service` (`src/trace.rs`):

1. **`api.rs`** — axum router, compression, body limits; maps `ApiError` to Valhalla-style JSON
   errors (`error_code` 100/123/124/125/126/153/154/171/444) or 503.
2. **`trace.rs`** — request types (`encoded_polyline` via `polyline.rs` or `shape`), validation,
   concurrency limit on matches, then `describe()` turns the match into `edges` / `matched_points`
   / `shape`. Edges must cover the whole shape without gaps (even over jumps) and the shape must be
   monotone along the trace, because pedalons re-projects it (`TraceSurfaceProjection`).
3. **`tiles.rs`** — tile corridor around the trace, bounded concurrent fetch with retry, disk
   cache keyed by source (`<host_path>-<hash of TILES_URL>/z/x/y.mvt`), stale-on-error, in-memory
   moka LRU of decoded layers, cache sweep. Any failure to get a real tile is a 503, never an empty
   tile; a file is only cached after it decodes.
4. **`mvt.rs`** — hand-written prost MVT decoder, limited to the `streets` layer → `StreetAttrs`.
5. **`valhalla.rs`** — port of Valhalla's tag → edge derivation (surface, `use`, `road_class`,
   `cycle_lane`, access per costing mode, `ACCESS_RULES` fr/default) applied to the reduced
   Shortbread attributes. The `unpaved` → gravel/dirt/compacted choices are the service's own
   heuristics, calibrated with `eval`.
6. **`network.rs`** — builds the street graph: lines clipped to their tile, nodes recovered from
   coordinates (tiles have no OSM ids), dangling ends at tile borders merged, junction vertices lost
   to simplification re-spliced; R-tree spatial index.
7. **`matcher.rs`** — meili-style HMM/Viterbi map matching (trace densified every 10 m, Gaussian
   emission, network-vs-straight-distance transitions via bounded Dijkstra, fixed-cost straight
   jump when the network is disconnected, footway penalty for bicycle).

Supporting modules: `geo.rs` (geodesy/projection), `pedalons.rs` (port of pedalons's projection into
surface ranges — `SurfaceRanges`, `TrackSurface` — used by the integration tests and `eval` to
score the output the way the real client would).

## Test data

- `tests/fixtures/tiles/…` — real z14 tiles (np655 stretch + two sea tiles for error 171), used
  with `offline: true` so tests never touch the network. New fixtures are produced with
  `inspect fixture`.
- `testdata/*.route.json` — Pédalons routes with the reference surface ranges pedalons computed from
  real Stadia answers; input of `eval`. Stadia is the reference, not ground truth.
- `tests/tiles.rs` spins up a local tile server to test cache/error behaviour.
