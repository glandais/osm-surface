//! The `trace_attributes` request and answer, and the service that computes it: decode the
//! trace, load the tiles around it, build the street graph, match, and describe the edges.

use crate::config::Config;
use crate::geo::{self, corridor_tiles};
use crate::matcher::{self, Matched, Params, Step};
use crate::mvt::StreetAttrs;
use crate::network::{Corridor, Network};
use crate::polyline;
use crate::tiles::{TileError, TileStore};
use crate::valhalla::Mode;
use serde::{Deserialize, Serialize};
use std::sync::Arc;
use std::time::Instant;

/// Valhalla's request limits as measured on Stadia Maps.
pub const MAX_POINTS: usize = 16_000;
pub const MAX_DISTANCE_M: f64 = 200_000.0;
/// What a request may wait in all before its matching starts (tribly reads for 15 s).
const MATCH_BUDGET: std::time::Duration = std::time::Duration::from_secs(13);

#[derive(Debug, Clone, Deserialize, Default)]
pub struct LatLon {
    pub lat: f64,
    #[serde(alias = "lng")]
    pub lon: f64,
}

#[derive(Debug, Clone, Deserialize, Default)]
pub struct Filters {
    #[serde(default)]
    pub attributes: Vec<String>,
    #[serde(default)]
    pub action: Option<String>,
}

#[derive(Debug, Clone, Deserialize, Default)]
pub struct TraceOptions {
    pub search_radius: Option<f64>,
    pub gps_accuracy: Option<f64>,
}

/// A Valhalla `trace_attributes` request (the fields this service reads; others are ignored).
#[derive(Debug, Clone, Deserialize, Default)]
pub struct TraceRequest {
    pub encoded_polyline: Option<String>,
    pub shape: Option<Vec<LatLon>>,
    pub costing: Option<String>,
    pub shape_match: Option<String>,
    pub filters: Option<Filters>,
    pub trace_options: Option<TraceOptions>,
    /// Not Valhalla: adds the Shortbread attributes of each edge under `osm`.
    #[serde(default)]
    pub debug: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default, PartialEq)]
pub struct Edge {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub surface: Option<String>,
    #[serde(rename = "use", skip_serializing_if = "Option::is_none")]
    pub use_: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub cycle_lane: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub road_class: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub unpaved: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub bridge: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tunnel: Option<bool>,
    /// Kilometres, as Valhalla.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub length: Option<f64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub begin_shape_index: Option<usize>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub end_shape_index: Option<usize>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub osm: Option<StreetAttrs>,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default, PartialEq)]
pub struct MatchedPoint {
    #[serde(rename = "type", skip_serializing_if = "Option::is_none")]
    pub type_: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub edge_index: Option<usize>,
    /// The fraction (0..1) of the edge before the point, as Valhalla.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub distance_along_edge: Option<f64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub lat: Option<f64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub lon: Option<f64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub distance_from_trace_point: Option<f64>,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct TraceResponse {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub units: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub shape: Option<String>,
    #[serde(default)]
    pub edges: Vec<Edge>,
    #[serde(default)]
    pub matched_points: Vec<MatchedPoint>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub osm_changeset: Option<i64>,
}

/// A Valhalla error: `error_code` and the HTTP status that carries it.
#[derive(Debug, Clone, thiserror::Error)]
#[error("{status} {code}: {message}")]
pub struct ApiError {
    pub status: u16,
    pub code: i32,
    pub message: String,
}

impl ApiError {
    pub fn bad_request(code: i32, message: impl Into<String>) -> Self {
        Self {
            status: 400,
            code,
            message: message.into(),
        }
    }

    pub fn unavailable(message: impl Into<String>) -> Self {
        // Not a Valhalla code: tribly retries anything else than its definitive 400s.
        Self {
            status: 503,
            code: 503,
            message: message.into(),
        }
    }

    pub fn status_text(&self) -> &'static str {
        match self.status {
            400 => "Bad Request",
            500 => "Internal Server Error",
            503 => "Service Unavailable",
            _ => "Error",
        }
    }

    pub fn body(&self) -> serde_json::Value {
        serde_json::json!({
            "error_code": self.code,
            "error": self.message,
            "status_code": self.status,
            "status": self.status_text(),
        })
    }
}

/// Which attributes the answer carries (`filters`).
#[derive(Debug, Clone)]
struct Selection {
    include: Option<Vec<String>>,
    exclude: Vec<String>,
}

impl Selection {
    fn of(filters: Option<&Filters>) -> Self {
        match filters {
            Some(f)
                if f.action
                    .as_deref()
                    .unwrap_or("include")
                    .eq_ignore_ascii_case("exclude") =>
            {
                Self {
                    include: None,
                    exclude: f.attributes.clone(),
                }
            }
            Some(f) if !f.attributes.is_empty() => Self {
                include: Some(f.attributes.clone()),
                exclude: vec![],
            },
            _ => Self {
                include: None,
                exclude: vec![],
            },
        }
    }

    fn has(&self, key: &str) -> bool {
        let listed = |list: &[String]| list.iter().any(|a| a == key);
        match &self.include {
            Some(list) => listed(list),
            None => !listed(&self.exclude),
        }
    }
}

/// What a request produced, with what the eval tool wants to know beyond the answer.
pub struct TraceOutput {
    pub response: TraceResponse,
    /// The Shortbread attributes of each edge of the answer.
    pub edge_osm: Vec<StreetAttrs>,
    pub stats: Stats,
}

#[derive(Debug, Clone, Default, Serialize)]
pub struct Stats {
    pub points: usize,
    pub observations: usize,
    pub tiles: usize,
    pub segments: usize,
    pub tiles_ms: u64,
    pub match_ms: u64,
    pub total_ms: u64,
}

/// A validated request.
pub struct Trace {
    pub points: Vec<(f64, f64)>,
    pub mode: Mode,
    selection: Selection,
    params: Params,
    debug: bool,
}

impl Trace {
    pub fn parse(req: &TraceRequest, base: &Params) -> Result<Self, ApiError> {
        let points: Vec<(f64, f64)> =
            if let Some(enc) = req.encoded_polyline.as_deref().filter(|s| !s.is_empty()) {
                polyline::decode(enc)
                    .ok_or_else(|| ApiError::bad_request(126, "Invalid encoded_polyline"))?
            } else if let Some(shape) = &req.shape {
                shape.iter().map(|p| (p.lat, p.lon)).collect()
            } else {
                return Err(ApiError::bad_request(
                    126,
                    "Insufficiently specified required parameter 'shape' or 'encoded_polyline'",
                ));
            };
        let costing = req
            .costing
            .as_deref()
            .ok_or_else(|| ApiError::bad_request(124, "No edge/node costing provided"))?;
        let mode = Mode::of_costing(costing).ok_or_else(|| {
            ApiError::bad_request(125, format!("No costing method found for '{costing}'"))
        })?;
        if points.len() < 2 {
            return Err(ApiError::bad_request(123, "Insufficient shape provided"));
        }
        if points.len() > MAX_POINTS {
            return Err(ApiError::bad_request(
                153,
                format!(
                    "Too many shape points: ({}). The best paths shape is limited to {MAX_POINTS}",
                    points.len()
                ),
            ));
        }
        if points.iter().any(|&(lat, lon)| {
            !(-90.0..=90.0).contains(&lat)
                || !(-180.0..=180.0).contains(&lon)
                || !lat.is_finite()
                || !lon.is_finite()
        }) {
            // What Stadia answers for an invalid latitude.
            return Err(ApiError::bad_request(
                154,
                "Path distance exceeds the max distance limit",
            ));
        }
        if points.windows(2).any(|w| (w[1].1 - w[0].1).abs() > 180.0) {
            // The tile grid and the densification are not wrapped around the globe.
            return Err(ApiError::bad_request(
                444,
                "Map Match algorithm failed to snap the shape points to the correct shape: \
                 traces crossing the antimeridian are not supported",
            ));
        }
        let length: f64 = points
            .windows(2)
            .map(|w| geo::haversine_m(w[0].0, w[0].1, w[1].0, w[1].1))
            .sum();
        if length > MAX_DISTANCE_M {
            return Err(ApiError::bad_request(
                154,
                format!(
                    "Path distance exceeds the max distance limit: {} meters",
                    MAX_DISTANCE_M as u64
                ),
            ));
        }
        let mut params = base.clone();
        if let Some(r) = req.trace_options.as_ref().and_then(|o| o.search_radius) {
            if r.is_finite() && r > 0.0 {
                params.search_radius_m = r.min(200.0);
            }
        }
        Ok(Self {
            points,
            mode,
            selection: Selection::of(req.filters.as_ref()),
            params,
            debug: req.debug,
        })
    }
}

pub struct Service {
    pub config: Config,
    pub store: TileStore,
    pub params: Params,
    /// The build and OSM dates of the tiles (their TileJSON), once fetched.
    pub tiles_info: std::sync::RwLock<Option<serde_json::Value>>,
    matches: Arc<tokio::sync::Semaphore>,
}

impl Service {
    pub fn new(config: Config) -> anyhow::Result<Self> {
        let store = TileStore::new(&config)?;
        Ok(Self {
            matches: Arc::new(tokio::sync::Semaphore::new(config.max_concurrent_matches)),
            config,
            store,
            params: Params::from_env(),
            tiles_info: std::sync::RwLock::new(None),
        })
    }

    /// Fetches the TileJSON of the tiles and keeps (and logs) the dates of their data: the
    /// agreement with Stadia depends on how old the tiles' OSM data is.
    pub async fn fetch_tiles_info(&self) {
        let Some(url) = self.config.tiles_json_url() else {
            return;
        };
        if self.config.offline {
            return;
        }
        let client = reqwest::Client::builder()
            .user_agent(self.config.user_agent.clone())
            .timeout(std::time::Duration::from_secs(5))
            .build();
        let got = match client {
            Ok(c) => match c.get(&url).send().await {
                Ok(r) if r.status().is_success() => r.json::<serde_json::Value>().await.ok(),
                Ok(r) => {
                    tracing::warn!(%url, status = %r.status(), "cannot read the tiles' TileJSON");
                    None
                }
                Err(e) => {
                    tracing::warn!(%url, error = %e, "cannot read the tiles' TileJSON");
                    None
                }
            },
            Err(_) => None,
        };
        let Some(v) = got else { return };
        let info = serde_json::json!({
            "tile_schema": v.get("tile_schema"),
            "build_time": v.get("planetiler:buildtime"),
            "osm_replication_time": v.get("planetiler:osm:osmosisreplicationtime"),
        });
        tracing::info!(%url, tiles = %info, "tile source");
        if let Ok(mut w) = self.tiles_info.write() {
            *w = Some(info);
        }
    }

    /// What `/health` says about the data sources.
    pub fn sources(&self) -> serde_json::Value {
        serde_json::json!({
            "tiles_url": self.config.tiles_url,
            "tiles": self.tiles_info.read().ok().and_then(|t| t.clone()),
        })
    }

    pub async fn trace_attributes(
        self: &Arc<Self>,
        req: &TraceRequest,
    ) -> Result<TraceOutput, ApiError> {
        let started = Instant::now();
        let trace = Trace::parse(req, &self.params)?;
        let grid = self.store.grid();
        let margin = trace.params.search_radius_m + 60.0;
        let keys = corridor_tiles(grid, &trace.points, margin);
        let tiles = tokio::time::timeout(self.config.tiles_deadline, self.store.get_many(&keys))
            .await
            .map_err(|_| {
                tracing::warn!(tiles = keys.len(), "tile loading timed out");
                ApiError::unavailable(format!("loading {} tiles timed out", keys.len()))
            })?
            .map_err(|e: TileError| {
                tracing::warn!(error = %e, "tiles unavailable");
                ApiError::unavailable(e.to_string())
            })?;
        let tiles_ms = started.elapsed().as_millis() as u64;
        // The permit moves into the blocking task: a client that gives up does not free it
        // while the computation still runs. Waiting for one is bounded by tribly's patience.
        let wait = MATCH_BUDGET.saturating_sub(started.elapsed());
        let permit = tokio::time::timeout(wait, self.matches.clone().acquire_owned())
            .await
            .map_err(|_| {
                tracing::warn!("no matching slot free in time");
                ApiError::unavailable("server busy, retry later")
            })?
            .map_err(|_| ApiError::unavailable("shutting down"))?;
        let rules = self.config.access_rules;
        let match_started = Instant::now();
        let mut out = tokio::task::spawn_blocking(move || -> Result<TraceOutput, ApiError> {
            let _permit = permit;
            let with_lat: Vec<([f64; 2], f64)> = trace
                .points
                .iter()
                .map(|&(lat, lon)| (grid.to_units(lon, lat), lat))
                .collect();
            let corridor = Corridor::new(grid, &with_lat, margin);
            let net = Network::build(grid, &tiles, &corridor, trace.mode, rules);
            let matched = matcher::match_trace(&net, &trace.points, &trace.params);
            let mut out = describe(&net, &matched, &trace)?;
            out.stats.tiles = tiles.len();
            out.stats.segments = net.segs.len();
            Ok(out)
        })
        .await
        .map_err(|e| ApiError {
            status: 500,
            code: 500,
            message: format!("matching failed: {e}"),
        })??;
        out.stats.tiles_ms = tiles_ms;
        out.stats.match_ms = match_started.elapsed().as_millis() as u64;
        out.stats.total_ms = started.elapsed().as_millis() as u64;
        Ok(out)
    }
}

/// The answer for a match: the matched line, its edges (runs of one street class) and where
/// each input point landed.
pub fn describe(net: &Network, m: &Matched, trace: &Trace) -> Result<TraceOutput, ApiError> {
    let n_input = trace.points.len();
    // Only the input points count: a densified observation matched between unmatched input
    // points would answer a one-point shape that tribly cannot use.
    let any_input = m
        .obs
        .iter()
        .zip(&m.chosen)
        .any(|(o, c)| o.input.is_some() && c.is_some());
    if !any_input {
        let length: f64 = trace
            .points
            .windows(2)
            .map(|w| geo::haversine_m(w[0].0, w[0].1, w[1].0, w[1].1))
            .sum();
        if length < 0.01 {
            // Stadia answers an empty 200 for two identical points.
            return Ok(TraceOutput {
                response: TraceResponse {
                    units: Some("kilometers".into()),
                    shape: Some(String::new()),
                    ..Default::default()
                },
                edge_osm: vec![],
                stats: Stats {
                    points: n_input,
                    observations: m.obs.len(),
                    ..Default::default()
                },
            });
        }
        return Err(if net.is_empty() || !m.any_candidate {
            ApiError::bad_request(171, "No suitable edges near location")
        } else {
            ApiError::bad_request(
                444,
                "Map Match algorithm failed to snap the shape points to the correct shape.",
            )
        });
    }

    let n = m.obs.len();
    let mut shape: Vec<[f64; 2]> = Vec::new();
    // label[k]: the class of the shape segment k..k+1 (None: a jump or a gap between chains).
    let mut label: Vec<Option<u32>> = Vec::new();
    let mut obs_vertex: Vec<Option<usize>> = vec![None; n];
    let pos = |seg: u32, t: f64| {
        let (a, b) = net.seg_points(seg);
        geo::lerp(a, b, t)
    };
    let push =
        |shape: &mut Vec<[f64; 2]>, label: &mut Vec<Option<u32>>, p: [f64; 2], l: Option<u32>| {
            if let Some(last) = shape.last() {
                if geo::dist_units(*last, p) < 1e-9 {
                    return;
                }
                label.push(l);
            }
            shape.push(p);
        };
    #[allow(clippy::needless_range_loop)]
    for t in 0..n {
        let Some(c) = m.chosen[t] else { continue };
        let starts_chain = t == 0 || m.steps[t - 1].is_none();
        if starts_chain {
            push(&mut shape, &mut label, c.pos, None);
        }
        obs_vertex[t] = Some(shape.len() - 1);
        match &m.steps[t] {
            Some(Step::Route(pieces)) => {
                for p in pieces {
                    push(
                        &mut shape,
                        &mut label,
                        pos(p.seg, p.t1),
                        Some(net.segs[p.seg as usize].class),
                    );
                }
            }
            Some(Step::Jump) => {
                // Between two streets the network does not join: the first half of the jump
                // belongs to the street left, the second to the street reached, so that the
                // edges cover the whole shape (as Valhalla's do) and tribly leaves no gap.
                if let Some(next) = m.chosen.get(t + 1).copied().flatten() {
                    push(
                        &mut shape,
                        &mut label,
                        geo::lerp(c.pos, next.pos, 0.5),
                        Some(net.segs[c.seg as usize].class),
                    );
                    push(
                        &mut shape,
                        &mut label,
                        next.pos,
                        Some(net.segs[next.seg as usize].class),
                    );
                }
            }
            None if starts_chain => {
                // A lone matched point: the street it is on, from it to the farther end.
                let end = if c.t < 0.5 { 1.0 } else { 0.0 };
                push(
                    &mut shape,
                    &mut label,
                    pos(c.seg, end),
                    Some(net.segs[c.seg as usize].class),
                );
            }
            None => {}
        }
    }

    // Coordinates as the client decodes them, and its cumulative haversine distance.
    let lat_lon: Vec<(f64, f64)> = shape
        .iter()
        .map(|&p| {
            let (lon, lat) = net.grid.to_lonlat(p);
            (polyline::round6(lat), polyline::round6(lon))
        })
        .collect();
    let mut cum = vec![0.0; lat_lon.len()];
    for k in 1..lat_lon.len() {
        let (a, b) = (lat_lon[k - 1], lat_lon[k]);
        cum[k] = cum[k - 1] + geo::haversine_m(a.0, a.1, b.0, b.1);
    }

    // Edges: maximal runs of one class.
    struct Run {
        class: u32,
        begin: usize,
        end: usize,
    }
    let mut runs: Vec<Run> = Vec::new();
    let mut seg_edge: Vec<Option<usize>> = vec![None; label.len()];
    for (k, l) in label.iter().enumerate() {
        let Some(class) = *l else { continue };
        match runs.last_mut() {
            Some(r) if r.class == class && r.end == k => r.end = k + 1,
            _ => runs.push(Run {
                class,
                begin: k,
                end: k + 1,
            }),
        }
        seg_edge[k] = Some(runs.len() - 1);
    }

    let sel = &trace.selection;
    let edges: Vec<Edge> = runs
        .iter()
        .map(|r| {
            let class = &net.classes[r.class as usize];
            let info = class.info;
            Edge {
                surface: sel
                    .has("edge.surface")
                    .then(|| info.surface.as_str().to_string()),
                use_: sel.has("edge.use").then(|| info.use_.as_str().to_string()),
                cycle_lane: sel
                    .has("edge.cycle_lane")
                    .then(|| info.cycle_lane.as_str().to_string()),
                road_class: sel
                    .has("edge.road_class")
                    .then(|| info.road_class.as_str().to_string()),
                unpaved: sel.has("edge.unpaved").then(|| info.surface.unpaved()),
                bridge: sel.has("edge.bridge").then_some(class.attrs.bridge),
                tunnel: sel.has("edge.tunnel").then_some(class.attrs.tunnel),
                length: sel
                    .has("edge.length")
                    .then(|| ((cum[r.end] - cum[r.begin]) / 1000.0 * 1e3).round() / 1e3),
                begin_shape_index: sel.has("edge.begin_shape_index").then_some(r.begin),
                end_shape_index: sel.has("edge.end_shape_index").then_some(r.end),
                osm: trace.debug.then(|| class.attrs.clone()),
            }
        })
        .collect();
    let edge_osm: Vec<StreetAttrs> = runs
        .iter()
        .map(|r| net.classes[r.class as usize].attrs.clone())
        .collect();

    let mut matched_points = Vec::with_capacity(n_input);
    for (t, o) in m.obs.iter().enumerate() {
        if o.input.is_none() {
            continue;
        }
        let (Some(c), Some(v)) = (m.chosen[t], obs_vertex[t]) else {
            matched_points.push(MatchedPoint {
                type_: sel.has("matched.type").then(|| "unmatched".to_string()),
                lat: sel.has("matched.point").then_some(o.lat),
                lon: sel.has("matched.point").then_some(o.lon),
                ..Default::default()
            });
            continue;
        };
        let out_edge = seg_edge.get(v).copied().flatten();
        let in_edge = if v > 0 {
            seg_edge.get(v - 1).copied().flatten()
        } else {
            None
        };
        let edge = out_edge.or(in_edge).or_else(|| {
            // Next to a jump: the nearest edge along the line.
            seg_edge[v.min(seg_edge.len())..]
                .iter()
                .flatten()
                .next()
                .copied()
                .or_else(|| {
                    seg_edge[..v.min(seg_edge.len())]
                        .iter()
                        .rev()
                        .flatten()
                        .next()
                        .copied()
                })
        });
        let Some(e) = edge else {
            matched_points.push(MatchedPoint {
                type_: sel.has("matched.type").then(|| "unmatched".to_string()),
                ..Default::default()
            });
            continue;
        };
        let r = &runs[e];
        let (a, b) = (cum[r.begin], cum[r.end]);
        let frac = if b > a {
            ((cum[v] - a) / (b - a)).clamp(0.0, 1.0)
        } else {
            0.0
        };
        let (lon, lat) = net.grid.to_lonlat(c.pos);
        matched_points.push(MatchedPoint {
            type_: sel.has("matched.type").then(|| "matched".to_string()),
            edge_index: sel.has("matched.edge_index").then_some(e),
            distance_along_edge: sel.has("matched.distance_along_edge").then_some(frac),
            lat: sel.has("matched.point").then_some(polyline::round6(lat)),
            lon: sel.has("matched.point").then_some(polyline::round6(lon)),
            distance_from_trace_point: sel
                .has("matched.distance_from_trace_point")
                .then(|| (c.dist_m * 1000.0).round() / 1000.0),
        });
    }
    debug_assert_eq!(matched_points.len(), n_input);

    Ok(TraceOutput {
        response: TraceResponse {
            units: Some("kilometers".into()),
            shape: sel.has("shape").then(|| polyline::encode(&lat_lon)),
            edges,
            matched_points,
            osm_changeset: None,
        },
        edge_osm,
        stats: Stats {
            points: n_input,
            observations: n,
            ..Default::default()
        },
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::AccessRules;
    use crate::mvt::{StreetLine, StreetTile};

    const LAT: f64 = 47.2;
    const LON: f64 = -1.5;

    /// Metres east / north of the origin, as (lat, lon).
    fn at(east: f64, north: f64) -> (f64, f64) {
        let lat = LAT + (north / geo::EARTH_RADIUS_M).to_degrees();
        let lon = LON + (east / (geo::EARTH_RADIUS_M * LAT.to_radians().cos())).to_degrees();
        (lat, lon)
    }

    fn network(lines: Vec<(&str, Vec<(f64, f64)>)>, trace: &[(f64, f64)]) -> Network {
        let grid = geo::Grid::new(14, 4096);
        let u = |(e, n): (f64, f64)| {
            let (lat, lon) = at(e, n);
            grid.to_units(lon, lat)
        };
        let mut tile = StreetTile::default();
        for (kind, pts) in lines {
            tile.attrs.push(StreetAttrs {
                kind: kind.into(),
                ..Default::default()
            });
            tile.lines.push(StreetLine {
                attr: (tile.attrs.len() - 1) as u32,
                points: pts.into_iter().map(u).collect(),
            });
        }
        let line: Vec<([f64; 2], f64)> = trace
            .iter()
            .map(|&(lat, lon)| (grid.to_units(lon, lat), lat))
            .collect();
        Network::build(
            grid,
            &[Arc::new(tile)],
            &Corridor::new(grid, &line, 200.0),
            Mode::Bicycle,
            AccessRules::France,
        )
    }

    fn trace(points: &[(f64, f64)]) -> Trace {
        let req = TraceRequest {
            encoded_polyline: Some(polyline::encode(points)),
            costing: Some("bicycle".into()),
            ..Default::default()
        };
        Trace::parse(&req, &Params::default()).unwrap()
    }

    #[test]
    fn a_jump_between_unjoined_streets_is_covered_by_edges() {
        // A residential street to x = 200 m, then 30 m of nothing, then a track.
        let pts: Vec<(f64, f64)> = (0..=10).map(|k| at(k as f64 * 50.0, 0.0)).collect();
        let net = network(
            vec![
                ("residential", vec![(0.0, 0.0), (200.0, 0.0)]),
                ("track", vec![(230.0, 0.0), (500.0, 0.0)]),
            ],
            &pts,
        );
        let t = trace(&pts);
        let m = matcher::match_trace(&net, &t.points, &t.params);
        assert!(m.steps.iter().flatten().any(|s| matches!(s, Step::Jump)));
        let out = describe(&net, &m, &t).unwrap();
        let r = &out.response;
        let shape = polyline::decode(r.shape.as_deref().unwrap()).unwrap();
        // The edges cover the shape from end to end without a hole.
        assert_eq!(r.edges.first().unwrap().begin_shape_index, Some(0));
        assert_eq!(
            r.edges.last().unwrap().end_shape_index,
            Some(shape.len() - 1)
        );
        for w in r.edges.windows(2) {
            assert_eq!(w[0].end_shape_index, w[1].begin_shape_index);
        }
        let surfaces: Vec<&str> = r
            .edges
            .iter()
            .map(|e| e.surface.as_deref().unwrap())
            .collect();
        assert_eq!(surfaces, vec!["paved_smooth", "dirt"]);
        assert!(r
            .matched_points
            .iter()
            .all(|p| p.type_.as_deref() == Some("matched")));
    }

    #[test]
    fn only_densified_points_matched_is_a_failed_match() {
        // Two input points 50 m on either side of a street they cross: beyond the search
        // radius, only the observations inserted between them reach the street.
        let pts = [at(-50.0, 0.0), at(50.0, 0.0)];
        let net = network(
            vec![("residential", vec![(0.0, -300.0), (0.0, 300.0)])],
            &pts,
        );
        let t = trace(&pts);
        let m = matcher::match_trace(&net, &t.points, &t.params);
        assert!(m.chosen.iter().any(Option::is_some));
        let err = describe(&net, &m, &t).err().unwrap();
        assert_eq!((err.status, err.code), (400, 444));
    }

    #[test]
    fn traces_across_the_antimeridian_are_refused() {
        let req = TraceRequest {
            encoded_polyline: Some(polyline::encode(&[(65.0, 179.9995), (65.0, -179.9995)])),
            costing: Some("bicycle".into()),
            ..Default::default()
        };
        let err = Trace::parse(&req, &Params::default()).err().unwrap();
        assert_eq!((err.status, err.code), (400, 444));
    }
}
