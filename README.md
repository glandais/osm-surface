# osm-surface

A Rust microservice that replaces, identically from its client's point of view, Valhalla's
`trace_attributes` endpoint as served by Stadia Maps
(<https://docs.stadiamaps.com/routing/trace-attributes/>), for computing the surface of Pédalons
traces (`pedalons` backend, `StadiaTraceAttributesClient` / `TraceSurfaceComputer`).

The data comes from **VersaTiles Shortbread 1.0** vector tiles (OpenStreetMap,
`https://tiles.versatiles.org/tiles/osm/{z}/{x}/{y}`, zoom 14), downloaded on demand and cached.
No Valhalla graph to build, no OSM extract to download or keep up to date.

The tiles are the only source. Shortbread reduces `surface` to `paved` / `unpaved`, has neither
`smoothness`, `mtb:scale`, `sac_scale`, `access` nor `bridleway`s, and its data is a few months
old. On the evaluation routes, agreement with Stadia is **86 to 94 %** per surface class and
**98 to 99 %** on paved / unpaved (see *Evaluation*).

## What the service does

```
POST /trace_attributes/v1      (Stadia path; /trace_attributes accepted too)
GET  /health
```

1. **Request**: `encoded_polyline` (polyline6, lat/lon × 1e6) or `shape: [{lat, lon}]`,
   `costing` (`bicycle`, `pedestrian`, `auto` and their variants), `shape_match` (everything is
   handled as `map_snap`), `filters` (`include` / `exclude`), optional `trace_options.search_radius`.
   The `Authorization: Stadia-Auth <key>` header is accepted and ignored.
2. **Tiles**: those touching a 100 m corridor around the trace (≈ 40 tiles for 90 km), plus an
   in-memory LRU of decoded `streets` layers. At most 8 simultaneous downloads, one retry on a
   transient error, explicit User-Agent. The disk cache keeps raw tiles for 7 days and reuses them
   stale if the server is unreachable. It is organised by source
   (`CACHE_DIR/<host_path>-<hash of TILES_URL>/z/x/y.mvt`): changing `TILES_URL` never serves the
   old content. A tile is only written there once decoded. An unreadable file is deleted and
   downloaded again. A 404 or an error page from the server gives a 503, never an empty tile.
3. **Graph**: the `streets` lines usable by the mode (see *Access*), split into segments clipped to
   the exact square of their tile; nodes recovered from coordinates (no OSM id in the tiles); ends
   cut at tile borders stitched back together; vertices lying on another line of the same level
   (ground / bridge / tunnel) connected to it (junction whose shared vertex disappeared during
   simplification); R-tree spatial index.
4. **`map_snap` matching**: meili-style HMM / Viterbi. The trace is densified every 10 m (a
   simplified trace is straight between its points), candidates within 40 m at most (8 per
   observation), Gaussian emission (σ = 5 m), transition `|network distance − observed distance| / β`
   (β = 3 m, bounded Dijkstra), straight-line jump allowed at a fixed cost when the network does
   not connect two candidates (connectivity gap in the tiles), point without a candidate →
   `unmatched`. For bicycles, a `footway`, sidewalk, `steps` or `pedestrian` costs a little more at
   emission (`MATCH_FOOTWAY_COST` = 0.75): next to a roadway, the roadway wins, as with Valhalla's
   bicycle profile.
5. **Response**: `shape` (polyline6 of the matched path), `edges` (one edge per run of segments of
   the same street class: `surface`, `use`, `cycle_lane`, `road_class`, `unpaved`, `bridge`,
   `tunnel`, `length` in km, `begin_shape_index`, `end_shape_index`), `matched_points` (one per
   point sent: `type`, `edge_index`, `distance_along_edge` as a **fraction** 0..1 of the edge,
   `lat`, `lon`, `distance_from_trace_point`). `osm_changeset` is not provided (pedalons accepts it
   null). Gzip (or brotli) response if the client accepts it. Edges cover the shape without gaps,
   even over a jump between two unconnected streets: the first half of the jump goes to the street
   being left, the second to the street reached. Only the segment between two pieces separated by
   an `unmatched` point stays outside any edge, and pedalons counts it as unknown anyway.

Distances along `shape` are computed the way pedalons recomputes them (haversine on coordinates
rounded to 1e-6), and the shape is monotone along the trace: pedalons's projection
(`TraceSurfaceProjection`, safeguard 1.2 × distance + 30 m) lands on the right ranges.
`pedalons.rs` is a port of it (with `SurfaceRanges` and `TrackSurface`) used by the tests and by
the evaluation tool.

### Errors (Valhalla format)

`HTTP 400 {"error_code": N, "error": "...", "status_code": 400, "status": "Bad Request"}`

| Code | Case |
|---:|---|
| 100 | unreadable JSON |
| 123 | fewer than 2 points |
| 124 / 125 | `costing` missing / unknown |
| 126 | neither `encoded_polyline` nor `shape` (or invalid polyline) |
| 153 | more than 16,000 points |
| 154 | more than 200 km (or invalid coordinate, like Stadia) |
| 171 | no street near the trace (at sea…) |
| 444 | streets, but none of the points sent could be matched (or trace crossing the antimeridian, not supported) |

pedalons treats {123, 171, 443, 444, 154} as final (the chunk stays unknown). When the tiles
cannot be loaded, the response is a **503**: pedalons sees it as transient and will retry later.
That is the case for an unreachable server, a 404, a response that is not a tile, an exceeded
`TILES_DEADLINE_SECS` deadline, a tile missing from the cache with `TILES_OFFLINE`, or all matching
slots busy for more than 13 s. Two identical points: empty 200, like Stadia.

## Surface: Valhalla's rules

Valhalla (`src/mjolnir/pbfgraphparser.cc`) takes the value of the `surface` tag; without a
`surface` tag, the `tracktype`; without either, `smoothness`. Then `mtb:scale` replaces whatever
was found, even an asphalt surface: 0 → `dirt`, 1 → `gravel`, 2 and above → `path`. If nothing was
found, `sac_scale` gives `path`, otherwise a default value depending on the road class and use
(table below).

### From Shortbread to Valhalla

Shortbread only keeps `paved` / `unpaved` from the `surface` tag and keeps neither `smoothness`,
`mtb:scale` nor `sac_scale`. Hence:

| Shortbread `streets` | Valhalla surface returned | pedalons class |
|---|---|---|
| `surface=paved` | `paved_smooth` | PAVED |
| `surface=unpaved`, `bicycle=designated` (greenway), tracktype missing or grade1-3 | `compacted` | GRAVEL |
| `surface=unpaved`, tracktype grade4 / grade5 | `dirt` | DIRT |
| `surface=unpaved`, tracktype grade1-3 | `gravel` | GRAVEL |
| `surface=unpaved`, `kind=path` (no tracktype) | `dirt` | DIRT |
| `surface=unpaved`, others (track, footway, service…) | `gravel` (Valhalla's reading of `unpaved`) | GRAVEL |
| no surface, `tracktype=grade1` | `paved_rough` | PAVED_ROUGH |
| no surface, `grade2` / `grade3` / `grade4` / `grade5` | `compacted` / `dirt` / `gravel` / `path` | GRAVEL / DIRT / GRAVEL / PATH |
| no surface nor tracktype: motorway … residential | `paved_smooth` | PAVED |
| same: service, living_street | `paved_smooth` | PAVED |
| same: track | `dirt` | DIRT |
| same: path, footway, pedestrian, bridleway | `compacted` | GRAVEL |
| same: cycleway, steps | `paved` | PAVED |

The rules for `unpaved` are the only choices specific to this service: the exact value being lost,
the most likely one is taken (OSM usage of `surface` per way type, checked on the evaluation
routes). The calibration table of `eval/report.md` shows, for each combination of Shortbread
attributes, what Stadia sees there.

**`use`**: `road`, `ramp` (links), `track`, `driveway`, `alley`, `parking_aisle`,
`emergency_access`, `drive_through`, `living_street`, `service_road`, `cycleway`, `footway`,
`steps`, `path`, `pedestrian`, `bridleway`, `bus`.
**`cycle_lane`** (best effort, Shortbread having no `cycleway:*`): `separated` on a cycleway,
`shared` on a path or a footway open to bicycles, `none` elsewhere.

### Access (`costing` filter)

Valhalla's default values (`lua/graph.lua`), with the French rules of `adminconstants.h` by default
(`ACCESS_RULES=fr`): bicycles allowed on footways, pedestrians on cycleways, neither on trunks; the
`bicycle` tag wins (`no`, `designated`…). `access`, `vehicle`, `foot`, `smoothness` and
`mtb:scale` are not in the tiles and therefore do not count. `ACCESS_RULES=default` gives the
worldwide rules. Railways, tramways and airport runways are never part of the network.

## Running

```bash
cargo run --release                         # listens on 0.0.0.0:8002
curl -s localhost:8002/health
```

| Variable | Default | Purpose |
|---|---|---|
| `LISTEN_ADDR` | `0.0.0.0:8002` | listen address |
| `TILES_URL` | `https://tiles.versatiles.org/tiles/osm/{z}/{x}/{y}` | tile URL template |
| `TILE_ZOOM` | `14` | tile zoom (14 = VersaTiles max zoom) |
| `CACHE_DIR` | `./.cache/tiles` | disk cache of raw tiles (empty: no disk cache) |
| `CACHE_TTL_DAYS` | `7` | age beyond which a tile is downloaded again; at 2 × TTL it is deleted |
| `CACHE_MAX_GB` | (none) | maximum disk cache size: the oldest tiles beyond it are deleted |
| `MEMORY_CACHE_MB` | `256` | in-memory LRU of decoded tiles |
| `FETCH_CONCURRENCY` | `8` | simultaneous downloads, process-wide |
| `FETCH_TIMEOUT_SECS` | `8` | timeout of one download |
| `TILES_DEADLINE_SECS` | `11` | total time to load the tiles of a request (then 503) |
| `TILES_OFFLINE` | `false` | `true`: disk cache only, never any download (missing tile: 503) |
| `USER_AGENT` | `osm-surface/<version> (…)` | sent to the tile server |
| `ACCESS_RULES` | `fr` | `fr` or `default` |
| `MAX_CONCURRENT_MATCHES` | number of CPUs | simultaneous matches |
| `RUST_LOG` / `LOG_FORMAT` | `info` / text | `tracing` logs (`LOG_FORMAT=json`) |
| `MATCH_*` | | matcher tuning (`MATCH_RADIUS_M`, `MATCH_SIGMA_M`, `MATCH_BETA_M`, `MATCH_STEP_M`, `MATCH_MAX_CANDIDATES`, `MATCH_JUMP_COST`, `MATCH_MAX_JUMP_M`, `MATCH_FOOTWAY_COST`) |

The disk cache is swept at startup and then every 6 hours. The sweep deletes abandoned temporary
files, tiles older than 2 × TTL and, with `CACHE_MAX_GB`, the oldest ones beyond the set size.
Without `CACHE_MAX_GB`, the cache grows with the regions traversed: a z14 tile averages 100 to
300 kB in a city, a few tens of kB in the countryside.

### Docker

```bash
docker build -t osm-surface .
docker run -d --name osm-surface -p 8002:8002 -v osm-surface-data:/data osm-surface
curl -s localhost:8002/health
```

Distroless runtime image (`cc-debian12`, non-root user) with `osm-surface`. The tile cache goes in
the `/data` volume. Count a few MB of cache per hundred km of routes (≈ 11 MB for the three
evaluation routes around Nantes). The build image is pinned (`rust:1.99.1-slim-bookworm`).

The GitHub Action `.github/workflows/docker.yml` publishes the image to
`ghcr.io/glandais/osm-surface`: `develop` and `latest` on every push to `develop`, `1.2.3` and
`1.2` for a `v1.2.3` tag, and `sha-<commit>` in every case. The image is multi-platform,
`linux/amd64` and `linux/arm64` (Apple Silicon Macs, Graviton…), each built on a native runner. On
a pull request, the image is only built. Nothing is built if `cargo test` or `cargo clippy`
(warnings denied) fails.

## Plugging into pedalons

pedalons's `stadiamaps-trace` client has its own URL (by default the Stadia routing one). It is
enough to override it on the backend side:

```bash
QUARKUS_REST_CLIENT_STADIAMAPS_TRACE_URL=http://<host>:8002
```

- Keep `STADIAMAPS_API_KEY` **non-empty**: the surface worker is disabled when the key is empty,
  and the client always sends `Authorization: Stadia-Auth <key>` (ignored here). The router and
  the geocoder keep using Stadia.
- The client read timeout (15 s) is generous: 50 to 100 ms per request when the tiles are cached,
  ≈ 0.9 s for the 40 tiles of the 90 km route downloaded cold (a few seconds for a 190 km chunk in
  a region never seen before).
- Surfaces already stored (computed by Stadia) remain valid; a full recomputation is not needed,
  but new traces will get slightly different ranges (see *Evaluation*).
- Cost: no more Stadia credits for the surface (20 credits per 190 km chunk until now).

### Attribution (ODbL)

The data is OpenStreetMap's (ODbL licence), via the VersaTiles tiles. The UI must display
"© OpenStreetMap contributors" with a link to <https://www.openstreetmap.org/copyright> wherever the
surface is shown. This is already the case (`routes.surface.attribution` on the web,
`routes.surfaceDetail.attribution` on mobile, link to the copyright page); to do at switch-over
time:

- remove any mention of Stadia Maps as the source of the surface if there is one (none found in the
  labels), and add "(ODbL)" to the credit to be explicit;
- in the privacy policy, the surface no longer goes through Stadia: recorded traces no longer leave
  our servers for this computation;
- the stored surface ranges are derived from OSM: if they are ever redistributed as a database
  (export, public API), it is under ODbL, with the same attribution.

## Evaluation

```bash
cargo run --release --bin eval                         # in process, tiles through the cache
cargo run --release --bin eval -- --url http://localhost:8002   # against a running server
EVAL_VERBOSE=1 cargo run --release --bin eval          # list every disagreement ≥ 40 m
```

For each `testdata/*.route.json`, the tool builds pedalons's exact request from
`tracks[0].line.coordinates` (polyline6, `bicycle`, `map_snap`, same filters), projects it like
pedalons (`TraceSurfaceComputer` → `TraceSurfaceProjection` → `SurfaceRanges`, `MIN_SPAN_M` = 20 →
`TrackSurface`) and compares it metre by metre to the ranges pedalons computed from the real Stadia
answers. Full report: [`eval/report.md`](eval/report.md) (breakdown, confusion matrix, 10 largest
disagreements with the matched street, our unknown ranges, calibration table).

| Route | Length | Agreement (class) | Excluding Stadia unknown | Paved / unpaved | Latency |
|---|---:|---:|---:|---:|---:|
| graymond-ride-138 | 90.2 km | **86.0 %** | 86.5 % | 98.1 % | ≈ 100 ms |
| np-655-gravel | 52.2 km | **91.6 %** | 91.5 % | 99.0 % | ≈ 75 ms |
| np677-chill-gravel | 42.8 km | **93.5 %** | 93.9 % | 99.3 % | ≈ 60 ms |

OSM tiles from 2026-06-07. Before the review: 85.6 / 91.6 / 93.4 %.

The reference is Stadia (Valhalla on an OSM from around late September 2026), not ground truth.
For paved versus unpaved, the tiles are enough. The remaining disagreements are mostly about the
class of an unpaved way (GRAVEL, DIRT or PATH), and come from what the tiles do not say:

- the surface reduced to `paved` / `unpaved` by Shortbread (≈ 60 % of the disagreements);
- the age of the tiles (≈ 14 %);
- `mtb:scale`, `smoothness`, `sac_scale` and `bridleway`s, missing from the tiles (≈ 10 %);
- the rest: the reference itself (Stadia without a surface, or routed elsewhere since an OSM
  change) and the matching (parallel ways).

The details are in the report and in *Known limits*.

Load: 15,000 noisy points over 180 km → under 160 ms, tiles cached. The response is 1.4 MB,
250 kB gzipped.

## Tests

```bash
cargo test
cargo clippy --all-targets
```

Everything runs offline. The unit tests cover:

- polyline6, including rejection of forged deltas;
- geodesy and MVT decoding on a real tile;
- the graph: splitting at tile borders and stitching;
- the Valhalla derivation from Shortbread attributes (surface, `use`, `cycle_lane`, access);
- matching on a synthetic network, jump coverage by edges and the 444 when only densified points
  are matched;
- the cache sweep and the port of the pedalons projection.

The integration tests go through the axum router (`tests/api.rs`). They use 6 km of the np-655
route, with its 3 real tiles:

- the response contract;
- agreement with the Stadia ranges (over 80 %);
- the `shape`, the Valhalla error codes, including 171 on two real sea tiles;
- the 503 when the tile server is unreachable, or offline when the tile is missing;
- gzip.

`tests/tiles.rs` runs a local tile server. It checks that an error page received with a 200 is not
cached and that an unreadable cache file is replaced. It also checks that a 404 gives an
"unavailable" error without writing anything.

Developer tool: `cargo run --release --bin inspect -- near <lat> <lon> [radius]` (the streets
around a point and their attributes), `… trace <route.json> <from_m> <to_m>` (the matching of a
stretch, `CANDS=1` for the candidates), `… prefetch|stats <route.json>…`,
`… fixture <route.json> <from_m> <to_m> <out.json> <tiles_dir>`.

## Known limits

- **Approximate unpaved surface**: `unpaved` does not say whether it is gravel, compacted, earth
  or grass. The GRAVEL / DIRT class is guessed (rules above): it is the main source of
  disagreement with Stadia (59 % of the disagreements).
- **Lost tags**: `mtb:scale` (which, in Valhalla, replaces even an asphalt surface), `sac_scale`
  (way → `path`), `smoothness`, `access`, `cycleway:*`, `foot`, `segregated`,
  `footway=sidewalk` are not in Shortbread. The PATH class only appears through
  `tracktype=grade5`. PAVED_ROUGH only appears through `tracktype=grade1` without a surface: cobbles
  (`cobblestone`) and the values Shortbread sets to null (`wood`…) take the default value of the
  way type.
- **Missing ways**: `highway=bridleway` is not in the tiles. These stretches come out `unmatched`
  (unknown for pedalons).
- **Freshness**: VersaTiles tiles are rebuilt from time to time (OSM from 2026-06-07 for those used
  here, date given by `/health`). More recent OSM edits are not in them.
- **Rebuilt topology**: no node id; junctions are recovered from geometry (tolerance < 1 m). Two
  ways crossing without a common vertex in the tile are not connected (a fixed-cost jump makes up
  for it in the matching). One-ways and turn restrictions are ignored (a planned trace is already
  legal).
- **No `osm_changeset`**, nor `admins`, `confidence_score`, `names`, `way_id`, `speed`…
- **Antimeridian**: a trace crossing it is rejected (444).
- **A single tile provider**: if `tiles.versatiles.org` is unreachable and the tiles are not
  cached, 503 (pedalons retries). To get around it, VersaTiles also publishes the full
  `.versatiles` file, which can be self-hosted and pointed to by `TILES_URL`.
