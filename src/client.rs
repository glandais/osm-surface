//! A port of what the client of this service does with a `trace_attributes` answer:
//! `TraceSurfaceComputer` (slices), `TraceSurfaceProjection` (answer back onto the sent
//! points), `SurfaceRanges` (normalisation) and `TrackSurface` (classes). Used by the eval
//! tool and the tests to check that the answers of this service project as Valhalla's do.

use crate::geo;
use crate::polyline;
use crate::trace::{Edge, TraceRequest, TraceResponse};

pub const MAX_REQUEST_M: f64 = 190_000.0;
pub const OVERLAP_M: f64 = 500.0;
pub const CORE_M: f64 = MAX_REQUEST_M - 2.0 * OVERLAP_M - 1_000.0;
pub const MAX_POINTS: usize = 15_000;
pub const MIN_SPAN_M: f64 = 20.0;
pub const DEFINITIVE_CODES: [i32; 5] = [123, 171, 443, 444, 154];
pub const UNKNOWN: &str = "unknown";
const SLACK: f64 = 1.2;
const ABS_SLACK_M: f64 = 30.0;

/// The attributes the client asks for.
pub const ATTRIBUTES: [&str; 10] = [
    "edge.surface",
    "edge.use",
    "edge.cycle_lane",
    "edge.begin_shape_index",
    "edge.end_shape_index",
    "matched.type",
    "matched.edge_index",
    "matched.distance_along_edge",
    "shape",
    "osm_changeset",
];

/// `TraceAttributesRequest.of(polyline, costing)` as JSON.
pub fn request_json(encoded_polyline: &str, costing: &str) -> serde_json::Value {
    serde_json::json!({
        "encoded_polyline": encoded_polyline,
        "costing": costing,
        "shape_match": "map_snap",
        "filters": { "attributes": ATTRIBUTES, "action": "include" },
    })
}

pub fn request(encoded_polyline: &str, costing: &str) -> TraceRequest {
    serde_json::from_value(request_json(encoded_polyline, costing)).expect("valid request")
}

/// `TrackSurface`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum TrackSurface {
    Paved,
    PavedRough,
    Gravel,
    Dirt,
    Path,
    Unknown,
    OffNetwork,
}

impl TrackSurface {
    pub const ALL: [TrackSurface; 6] = [
        TrackSurface::Paved,
        TrackSurface::PavedRough,
        TrackSurface::Gravel,
        TrackSurface::Dirt,
        TrackSurface::Path,
        TrackSurface::Unknown,
    ];

    pub fn of_code(code: Option<&str>) -> Self {
        match code {
            Some("paved_smooth") | Some("paved") => TrackSurface::Paved,
            Some("paved_rough") => TrackSurface::PavedRough,
            Some("compacted") | Some("gravel") | Some("fine_gravel") => TrackSurface::Gravel,
            Some("dirt") => TrackSurface::Dirt,
            Some("path") | Some("impassable") => TrackSurface::Path,
            Some("off_network") => TrackSurface::OffNetwork,
            _ => TrackSurface::Unknown,
        }
    }

    pub fn name(self) -> &'static str {
        match self {
            TrackSurface::Paved => "PAVED",
            TrackSurface::PavedRough => "PAVED_ROUGH",
            TrackSurface::Gravel => "GRAVEL",
            TrackSurface::Dirt => "DIRT",
            TrackSurface::Path => "PATH",
            TrackSurface::Unknown => "UNKNOWN",
            TrackSurface::OffNetwork => "OFF_NETWORK",
        }
    }

    pub fn of_name(name: &str) -> Self {
        match name {
            "PAVED" => TrackSurface::Paved,
            "PAVED_ROUGH" => TrackSurface::PavedRough,
            "GRAVEL" => TrackSurface::Gravel,
            "DIRT" => TrackSurface::Dirt,
            "PATH" => TrackSurface::Path,
            "OFF_NETWORK" => TrackSurface::OffNetwork,
            _ => TrackSurface::Unknown,
        }
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct Range<V> {
    pub from: f64,
    pub to: f64,
    pub value: V,
}

impl<V> Range<V> {
    pub fn len(&self) -> f64 {
        self.to - self.from
    }

    pub fn is_empty(&self) -> bool {
        self.to <= self.from
    }
}

fn shape_distances(r: &TraceResponse) -> Option<Vec<f64>> {
    let shape = polyline::decode(r.shape.as_deref()?)?;
    if shape.len() < 2 {
        return None;
    }
    let mut cum = vec![0.0; shape.len()];
    for i in 1..shape.len() {
        let (a, b) = (shape[i - 1], shape[i]);
        cum[i] = cum[i - 1] + geo::haversine_m(a.0, a.1, b.0, b.1);
    }
    for e in &r.edges {
        let (b, en) = (e.begin_shape_index?, e.end_shape_index?);
        if en < b || en >= cum.len() {
            return None;
        }
    }
    Some(cum)
}

fn emit<V: PartialEq + Clone>(
    out: &mut Vec<Range<Option<V>>>,
    from: f64,
    to: f64,
    value: Option<V>,
) {
    if to <= from {
        return;
    }
    if let Some(last) = out.last_mut() {
        if last.value == value && (last.to - from).abs() < 1e-6 {
            last.to = to;
            return;
        }
    }
    out.push(Range { from, to, value });
}

/// `TraceSurfaceProjection.project`: `d` are the cumulative distances of the sent points.
pub fn project<V: PartialEq + Clone>(
    d: &[f64],
    r: &TraceResponse,
    key: impl Fn(usize, &Edge) -> Option<V>,
) -> Vec<Range<Option<V>>> {
    let unusable = vec![Range {
        from: d[0],
        to: d[d.len() - 1],
        value: None,
    }];
    let Some(sd) = shape_distances(r) else {
        return unusable;
    };
    let edges = &r.edges;
    let matched = &r.matched_points;
    if matched.len() != d.len() {
        return unusable;
    }
    let on_edge = |i: usize| -> Option<usize> {
        let m = &matched[i];
        let e = m.edge_index?;
        (m.type_.as_deref() != Some("unmatched") && e < edges.len()).then_some(e)
    };
    let position = |i: usize, e: usize| -> f64 {
        let edge = &edges[e];
        let a = sd[edge.begin_shape_index.unwrap_or(0)];
        let b = sd[edge.end_shape_index.unwrap_or(0)];
        a + matched[i].distance_along_edge.unwrap_or(0.0) * (b - a)
    };
    let mut out = Vec::new();
    for i in 0..d.len() - 1 {
        let (Some(e0), Some(e1)) = (on_edge(i), on_edge(i + 1)) else {
            emit(&mut out, d[i], d[i + 1], None);
            continue;
        };
        if e0 == e1 {
            emit(&mut out, d[i], d[i + 1], key(e0, &edges[e0]));
            continue;
        }
        let s0 = position(i, e0);
        let s1 = position(i + 1, e1);
        let dd = d[i + 1] - d[i];
        if e1 < e0 || s1 <= s0 || (s1 - s0) > SLACK * dd + ABS_SLACK_M {
            let mid = (d[i] + d[i + 1]) / 2.0;
            emit(&mut out, d[i], mid, key(e0, &edges[e0]));
            emit(&mut out, mid, d[i + 1], key(e1, &edges[e1]));
            continue;
        }
        let at = |s: f64| {
            if s1 > s0 {
                d[i] + (s - s0) / (s1 - s0) * dd
            } else {
                d[i]
            }
        };
        for (k, e) in edges.iter().enumerate().take(e1 + 1).skip(e0) {
            let a = sd[e.begin_shape_index.unwrap_or(0)].max(s0);
            let b = sd[e.end_shape_index.unwrap_or(0)].min(s1);
            if b <= a && k != e0 && k != e1 {
                continue;
            }
            emit(&mut out, at(a.max(s0)), at(b.min(s1)), key(k, e));
        }
    }
    out
}

fn merge<V: PartialEq + Clone>(ranges: Vec<Range<V>>) -> Vec<Range<V>> {
    let mut out: Vec<Range<V>> = Vec::with_capacity(ranges.len());
    for r in ranges {
        match out.last_mut() {
            Some(last) if last.value == r.value => last.to = r.to,
            _ => out.push(r),
        }
    }
    out
}

fn cover<V: PartialEq + Clone>(ranges: &[Range<V>], length: f64, unknown: &V) -> Vec<Range<V>> {
    let mut sorted = ranges.to_vec();
    sorted.sort_by(|a, b| a.from.total_cmp(&b.from));
    let mut out: Vec<Range<V>> = Vec::with_capacity(sorted.len() + 2);
    for r in sorted {
        let from = r.from.max(0.0);
        let to = r.to.min(length);
        if to <= from {
            continue;
        }
        let mut after: Vec<Range<V>> = Vec::new();
        while out.last().is_some_and(|l| l.to > from) {
            let previous = out.pop().expect("non-empty");
            if previous.to > to {
                after.insert(
                    0,
                    Range {
                        from: previous.from.max(to),
                        to: previous.to,
                        value: previous.value.clone(),
                    },
                );
            }
            if previous.from < from {
                out.push(Range {
                    from: previous.from,
                    to: from,
                    value: previous.value,
                });
                break;
            }
        }
        let end = out.last().map_or(0.0, |l| l.to);
        if from > end {
            out.push(Range {
                from: end,
                to: from,
                value: unknown.clone(),
            });
        }
        out.push(Range {
            from,
            to,
            value: r.value,
        });
        out.extend(after);
    }
    let end = out.last().map_or(0.0, |l| l.to);
    if end < length {
        out.push(Range {
            from: end,
            to: length,
            value: unknown.clone(),
        });
    }
    out
}

fn absorb_short<V: PartialEq + Clone>(cover: &mut Vec<Range<V>>) {
    while cover.len() > 1 {
        let mut shortest: Option<usize> = None;
        for i in 0..cover.len() {
            let l = cover[i].len();
            if l < MIN_SPAN_M && shortest.is_none_or(|s| l < cover[s].len()) {
                shortest = Some(i);
            }
        }
        let Some(s) = shortest else { return };
        let gone = cover.remove(s);
        // the longer neighbour, the left one on a tie (or the only one at an end)
        let into = if s == 0 {
            0
        } else if s == cover.len() || cover[s - 1].len() >= cover[s].len() {
            s - 1
        } else {
            s
        };
        let keeper = &mut cover[into];
        keeper.from = keeper.from.min(gone.from);
        keeper.to = keeper.to.max(gone.to);
        let merged = merge(std::mem::take(cover));
        *cover = merged;
    }
}

fn decimetre(m: f64) -> f64 {
    // Java Math.round: floor(x + 0.5)
    (m * 10.0 + 0.5).floor() / 10.0
}

/// `SurfaceRanges.normalize`.
pub fn normalize<V: PartialEq + Clone>(
    ranges: &[Range<V>],
    length: f64,
    unknown: &V,
) -> Vec<Range<V>> {
    let mut c = merge(cover(ranges, length, unknown));
    absorb_short(&mut c);
    let mut out = Vec::with_capacity(c.len());
    let mut from = 0.0;
    let n = c.len();
    for (i, r) in c.into_iter().enumerate() {
        let to = if i == n - 1 {
            decimetre(length)
        } else {
            decimetre(r.to)
        };
        if to > from {
            out.push(Range {
                from,
                to,
                value: r.value,
            });
            from = to;
        }
    }
    merge(out)
}

/// How a call to the provider went, as the client sees it.
pub enum CallResult {
    Ok(Box<TraceResponse>),
    /// A 400 with one of the definitive codes: the slice is unknown.
    Definitive(i32),
    /// Anything else: the client fails the whole track and retries later.
    Transient(String),
}

/// One slice of a track as sent, with its answer.
pub struct Slice {
    pub i0: usize,
    pub i1: usize,
    pub start: f64,
    pub end: f64,
    pub response: Option<TraceResponse>,
}

/// `TraceSurfaceComputer.compute` for the surfaces: the normalised surface codes along the
/// track, and the slices (for the caller to look further into the answers).
pub fn compute(
    lat: &[f64],
    lon: &[f64],
    dist: &[f64],
    costing: &str,
    mut call: impl FnMut(&str, &str) -> CallResult,
) -> Result<(Vec<Range<String>>, Vec<Slice>), String> {
    let n = dist.len();
    let length = if n == 0 { 0.0 } else { dist[n - 1] };
    if n < 2 || length < 1.0 {
        return Err("too-short".into());
    }
    let mut surfaces: Vec<Range<String>> = Vec::new();
    let mut slices = Vec::new();
    let mut definitive = 0;
    let mut start = 0.0;
    while start < length {
        let mut end = length.min(start + CORE_M);
        let i0 = last_at_or_before(dist, (start - OVERLAP_M).max(0.0));
        let mut i1 = first_at_or_after(dist, length.min(end + OVERLAP_M));
        if i1 - i0 + 1 > MAX_POINTS {
            i1 = i0 + MAX_POINTS - 1;
            end = (start + (dist[i1] - start) / 2.0).max(dist[i1] - OVERLAP_M);
            end = end.max(length.min(start + 1.0));
        }
        let pts: Vec<(f64, f64)> = (i0..=i1).map(|i| (lat[i], lon[i])).collect();
        let encoded = polyline::encode(&pts);
        let response = match call(&encoded, costing) {
            CallResult::Ok(r) => Some(*r),
            CallResult::Definitive(_) => {
                definitive += 1;
                None
            }
            CallResult::Transient(e) => return Err(e),
        };
        match &response {
            None => surfaces.push(Range {
                from: start,
                to: end,
                value: UNKNOWN.into(),
            }),
            Some(r) => {
                let d: Vec<f64> = (i0..=i1).map(|k| dist[k] - dist[i0]).collect();
                for rg in project(&d, r, |_, e| e.surface.clone()) {
                    let a = start.max(rg.from + dist[i0]);
                    let b = end.min(rg.to + dist[i0]);
                    if b > a {
                        surfaces.push(Range {
                            from: a,
                            to: b,
                            value: rg.value.unwrap_or_else(|| UNKNOWN.into()),
                        });
                    }
                }
            }
        }
        slices.push(Slice {
            i0,
            i1,
            start,
            end,
            response,
        });
        start = end;
    }
    if definitive == slices.len() {
        return Err("all slices definitive errors".into());
    }
    Ok((normalize(&surfaces, length, &UNKNOWN.to_string()), slices))
}

fn last_at_or_before(dist: &[f64], d: f64) -> usize {
    dist.partition_point(|&x| x <= d).saturating_sub(1)
}

fn first_at_or_after(dist: &[f64], d: f64) -> usize {
    dist.partition_point(|&x| x < d).min(dist.len() - 1)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::trace::MatchedPoint;

    fn line(n: usize, step: f64) -> (Vec<f64>, Vec<f64>, Vec<f64>) {
        let d_lat = (step / geo::EARTH_RADIUS_M).to_degrees();
        let lat: Vec<f64> = (0..n).map(|i| 48.0 + i as f64 * d_lat).collect();
        let lon = vec![2.0; n];
        let mut dist = vec![0.0; n];
        for i in 1..n {
            dist[i] = dist[i - 1] + geo::haversine_m(lat[i - 1], 2.0, lat[i], 2.0);
        }
        (lat, lon, dist)
    }

    fn edge(surface: &str, b: usize, e: usize) -> Edge {
        Edge {
            surface: Some(surface.into()),
            begin_shape_index: Some(b),
            end_shape_index: Some(e),
            ..Default::default()
        }
    }

    fn at(e: usize, f: f64) -> MatchedPoint {
        MatchedPoint {
            type_: Some("matched".into()),
            edge_index: Some(e),
            distance_along_edge: Some(f),
            ..Default::default()
        }
    }

    fn response(lat: &[f64], lon: &[f64], matched: Vec<MatchedPoint>) -> TraceResponse {
        let pts: Vec<(f64, f64)> = lat.iter().zip(lon).map(|(a, b)| (*a, *b)).collect();
        TraceResponse {
            shape: Some(polyline::encode(&pts)),
            edges: vec![edge("paved", 0, 1), edge("gravel", 1, 2)],
            matched_points: matched,
            ..Default::default()
        }
    }

    /// The cases of the client's TraceSurfaceProjectionTest.
    #[test]
    fn projection_cases_of_the_client() {
        let (lat, lon, _) = line(3, 100.0);
        let r = response(&lat, &lon, vec![at(0, 0.0), at(0, 0.5), at(0, 1.0)]);
        let out = project(&[0.0, 50.0, 100.0], &r, |_, e| e.surface.clone());
        assert_eq!(out.len(), 1);
        assert!((out[0].to - 100.0).abs() < 0.5);

        let r = response(&lat, &lon, vec![at(0, 0.0), at(1, 1.0)]);
        let out = project(&[0.0, 200.0], &r, |_, e| e.surface.clone());
        assert_eq!(out.len(), 2);
        assert!((out[0].to - 100.0).abs() < 0.5 && out[1].value.as_deref() == Some("gravel"));

        let out = project(&[1000.0, 1100.0], &r, |_, e| e.surface.clone());
        assert!((out[0].to - 1050.0).abs() < 0.5);

        // detour: 200 m of snapped line for 50 m
        let out = project(&[0.0, 50.0], &r, |_, e| e.surface.clone());
        assert!((out[0].to - 25.0).abs() < 0.5);

        // way back
        let r = response(&lat, &lon, vec![at(1, 1.0), at(0, 0.0)]);
        let out = project(&[0.0, 100.0], &r, |_, e| e.surface.clone());
        assert_eq!(out[0].value.as_deref(), Some("gravel"));
        assert!((out[0].to - 50.0).abs() < 0.5);

        // wrong number of matched points: one unknown range
        let r = response(&lat, &lon, vec![at(0, 0.0)]);
        let out = project(&[0.0, 50.0, 100.0], &r, |_, e| e.surface.clone());
        assert_eq!(
            out,
            vec![Range {
                from: 0.0,
                to: 100.0,
                value: None
            }]
        );
    }

    #[test]
    fn normalization_absorbs_short_ranges_and_fills_gaps() {
        let r = |from: f64, to: f64, v: &str| Range {
            from,
            to,
            value: v.to_string(),
        };
        let out = normalize(
            &[
                r(0.0, 100.0, "a"),
                r(100.0, 110.0, "b"),
                r(110.0, 300.0, "a"),
                r(320.0, 400.0, "c"),
            ],
            400.0,
            &"u".into(),
        );
        assert_eq!(
            out,
            vec![
                r(0.0, 300.0, "a"),
                r(300.0, 320.0, "u"),
                r(320.0, 400.0, "c")
            ]
        );
    }

    #[test]
    fn slices_cover_a_long_track() {
        let (lat, lon, dist) = line(2_100, 100.0);
        let mut calls = 0;
        let (cover, slices) = compute(&lat, &lon, &dist, "bicycle", |enc, _| {
            calls += 1;
            let pts = polyline::decode(enc).unwrap();
            let n = pts.len();
            CallResult::Ok(Box::new(TraceResponse {
                shape: Some(enc.to_string()),
                edges: vec![edge("paved", 0, n - 1)],
                matched_points: (0..n).map(|i| at(0, i as f64 / (n - 1) as f64)).collect(),
                ..Default::default()
            }))
        })
        .unwrap();
        assert_eq!(calls, 2);
        assert_eq!(slices.len(), 2);
        assert_eq!(cover.len(), 1);
        assert_eq!(cover[0].value, "paved");
        assert!((cover[0].to - dist[dist.len() - 1]).abs() < 0.1);
    }
}
