//! `map_snap`: an HMM map matcher (Newson & Krumm, as Valhalla's meili) over the street
//! graph of a request.
//!
//! The trace is densified (a planned, simplified track is straight between its points), each
//! observation gets the nearest projections on the streets as candidates, the emission cost
//! is a Gaussian on the distance to the street and the transition cost compares the distance
//! along the network between two candidates with the distance between the observations.
//! When the network does not connect two candidates (a missing junction in the tiles), a
//! "jump" in a straight line is allowed at a fixed cost so that one hole does not break the
//! match.

use crate::geo;
use crate::network::Network;
use crate::valhalla::{Mode, Use};
use std::cmp::Ordering;
use std::collections::{BinaryHeap, HashSet};

#[derive(Debug, Clone)]
pub struct Params {
    /// Candidates are searched this far from each observation.
    pub search_radius_m: f64,
    /// Standard deviation of the distance between an observation and its street.
    pub sigma_z: f64,
    /// Scale of the transition cost (metres of route length that differ from the trace).
    pub beta: f64,
    /// Densification step of the trace.
    pub step_m: f64,
    pub max_candidates: usize,
    /// Cost of a straight jump between two candidates the network does not connect.
    pub jump_cost: f64,
    /// A jump is not allowed over more than this.
    pub max_jump_m: f64,
    /// For bicycles, the extra emission cost of a footway, sidewalk, crossing, steps or
    /// pedestrian street: where one runs beside a road, the road is the likelier match (as
    /// Valhalla's bicycle costing, which makes footways dearer).
    pub footway_cost: f64,
}

impl Default for Params {
    fn default() -> Self {
        Self {
            search_radius_m: 40.0,
            sigma_z: 5.0,
            beta: 3.0,
            step_m: 10.0,
            max_candidates: 8,
            jump_cost: 12.0,
            max_jump_m: 60.0,
            footway_cost: 0.75,
        }
    }
}

impl Params {
    /// The defaults, overridden by `MATCH_RADIUS_M`, `MATCH_SIGMA_M`, `MATCH_BETA_M`,
    /// `MATCH_STEP_M`, `MATCH_MAX_CANDIDATES`, `MATCH_JUMP_COST`, `MATCH_MAX_JUMP_M`
    /// (for tuning).
    pub fn from_env() -> Self {
        let mut p = Self::default();
        let f = |k: &str| {
            std::env::var(k)
                .ok()
                .and_then(|v| v.parse::<f64>().ok())
                .filter(|v| v.is_finite() && *v > 0.0)
        };
        if let Some(v) = f("MATCH_RADIUS_M") {
            p.search_radius_m = v;
        }
        if let Some(v) = f("MATCH_SIGMA_M") {
            p.sigma_z = v;
        }
        if let Some(v) = f("MATCH_BETA_M") {
            p.beta = v;
        }
        if let Some(v) = f("MATCH_STEP_M") {
            p.step_m = v.max(1.0);
        }
        if let Some(v) = f("MATCH_MAX_CANDIDATES") {
            p.max_candidates = (v as usize).clamp(1, 64);
        }
        if let Some(v) = f("MATCH_JUMP_COST") {
            p.jump_cost = v;
        }
        if let Some(v) = f("MATCH_MAX_JUMP_M") {
            p.max_jump_m = v;
        }
        if let Ok(v) = std::env::var("MATCH_FOOTWAY_COST") {
            if let Some(v) = v.parse::<f64>().ok().filter(|v| v.is_finite() && *v >= 0.0) {
                p.footway_cost = v;
            }
        }
        p
    }
}

/// One observation of the densified trace.
#[derive(Debug, Clone, Copy)]
pub struct Obs {
    /// Global units.
    pub p: [f64; 2],
    pub lat: f64,
    pub lon: f64,
    /// Index of the input point it is, if it is one (and not an interpolated observation).
    pub input: Option<usize>,
}

#[derive(Debug, Clone, Copy)]
pub struct Cand {
    pub seg: u32,
    /// Fraction along the segment, from its node `a` to its node `b`.
    pub t: f64,
    pub pos: [f64; 2],
    pub dist_m: f64,
}

/// A stretch of a segment travelled from fraction `t0` to fraction `t1`.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Piece {
    pub seg: u32,
    pub t0: f64,
    pub t1: f64,
}

/// How the match goes from one observation to the next.
#[derive(Debug, Clone)]
pub enum Step {
    /// Along the network.
    Route(Vec<Piece>),
    /// In a straight line between two streets the network does not connect.
    Jump,
}

pub struct Matched {
    pub obs: Vec<Obs>,
    /// The candidate chosen for each observation; `None`: unmatched.
    pub chosen: Vec<Option<Cand>>,
    /// `steps[t]`: from observation `t` to `t + 1` when both are matched in the same chain.
    pub steps: Vec<Option<Step>>,
    /// Whether at least one observation had a candidate.
    pub any_candidate: bool,
}

/// The input points (lat, lon) and the points inserted every `step_m` between them.
pub fn densify(net_grid: geo::Grid, points: &[(f64, f64)], step_m: f64) -> Vec<Obs> {
    let mut out = Vec::with_capacity(points.len() * 4);
    for (i, &(lat, lon)) in points.iter().enumerate() {
        out.push(Obs {
            p: net_grid.to_units(lon, lat),
            lat,
            lon,
            input: Some(i),
        });
        if let Some(&(lat2, lon2)) = points.get(i + 1) {
            let d = geo::haversine_m(lat, lon, lat2, lon2);
            let n = (d / step_m).ceil() as usize;
            for k in 1..n {
                let f = k as f64 / n as f64;
                let (la, lo) = (lat + (lat2 - lat) * f, lon + (lon2 - lon) * f);
                out.push(Obs {
                    p: net_grid.to_units(lo, la),
                    lat: la,
                    lon: lo,
                    input: None,
                });
            }
        }
    }
    out
}

#[derive(Clone, Copy, PartialEq)]
struct HeapItem(f64, u32);
impl Eq for HeapItem {}
impl Ord for HeapItem {
    fn cmp(&self, other: &Self) -> Ordering {
        other
            .0
            .total_cmp(&self.0)
            .then_with(|| other.1.cmp(&self.1))
    }
}
impl PartialOrd for HeapItem {
    fn partial_cmp(&self, other: &Self) -> Option<Ordering> {
        Some(self.cmp(other))
    }
}

const NO_PRED: u32 = u32::MAX;

/// A bounded Dijkstra reusing its arrays between runs.
struct Dijkstra {
    dist: Vec<f64>,
    pred: Vec<u32>,
    stamp: Vec<u32>,
    settled: Vec<u32>,
    run: u32,
    heap: BinaryHeap<HeapItem>,
}

impl Dijkstra {
    fn new(n: usize) -> Self {
        Self {
            dist: vec![0.0; n],
            pred: vec![NO_PRED; n],
            stamp: vec![0; n],
            settled: vec![0; n],
            run: 0,
            heap: BinaryHeap::new(),
        }
    }

    /// From the two ends of the candidate's segment, until every target is settled or the
    /// distance exceeds `limit`.
    fn run_from(&mut self, net: &Network, c: &Cand, targets: &HashSet<u32>, limit: f64) {
        self.run += 1;
        self.heap.clear();
        let s = net.segs[c.seg as usize];
        for (node, d) in [(s.a, c.t * s.len_m), (s.b, (1.0 - c.t) * s.len_m)] {
            self.relax(node, d, NO_PRED);
        }
        let mut left = targets.len();
        while let Some(HeapItem(d, n)) = self.heap.pop() {
            if self.settled[n as usize] == self.run || d > self.dist[n as usize] {
                continue;
            }
            self.settled[n as usize] = self.run;
            if targets.contains(&n) {
                left -= 1;
                if left == 0 {
                    break;
                }
            }
            if d > limit {
                break;
            }
            for &(seg, other) in net.neighbours(n) {
                let nd = d + net.segs[seg as usize].len_m;
                if nd <= limit * 1.5 {
                    self.relax(other, nd, seg);
                }
            }
        }
    }

    fn relax(&mut self, node: u32, d: f64, pred: u32) {
        let i = node as usize;
        if self.stamp[i] != self.run || d < self.dist[i] {
            self.stamp[i] = self.run;
            self.dist[i] = d;
            self.pred[i] = pred;
            self.heap.push(HeapItem(d, node));
        }
    }

    fn dist_to(&self, node: u32) -> Option<f64> {
        let i = node as usize;
        (self.stamp[i] == self.run).then_some(self.dist[i])
    }

    /// Route length from the source candidate to `c`, and through which end of `c`'s segment
    /// (`true`: node `a`).
    fn route_to(&self, net: &Network, from: &Cand, c: &Cand) -> Option<(f64, Option<bool>)> {
        let s = net.segs[c.seg as usize];
        let mut best: Option<(f64, Option<bool>)> = None;
        if from.seg == c.seg {
            best = Some(((c.t - from.t).abs() * s.len_m, None));
        }
        for (node, via_a, off) in [
            (s.a, true, c.t * s.len_m),
            (s.b, false, (1.0 - c.t) * s.len_m),
        ] {
            if let Some(d) = self.dist_to(node) {
                let total = d + off;
                if best.is_none_or(|(b, _)| total < b - 1e-9) {
                    best = Some((total, Some(via_a)));
                }
            }
        }
        best
    }

    /// The pieces from the source candidate to `c` entering through `via_a`.
    fn pieces_to(&self, net: &Network, from: &Cand, c: &Cand, via_a: Option<bool>) -> Vec<Piece> {
        let Some(via_a) = via_a else {
            return vec![Piece {
                seg: c.seg,
                t0: from.t,
                t1: c.t,
            }];
        };
        let s = net.segs[c.seg as usize];
        let mut node = if via_a { s.a } else { s.b };
        let mut middle: Vec<Piece> = Vec::new();
        loop {
            let p = self.pred[node as usize];
            if p == NO_PRED {
                break;
            }
            let ps = net.segs[p as usize];
            let (t0, t1, prev) = if ps.b == node {
                (0.0, 1.0, ps.a)
            } else {
                (1.0, 0.0, ps.b)
            };
            middle.push(Piece { seg: p, t0, t1 });
            node = prev;
        }
        middle.reverse();
        let fs = net.segs[from.seg as usize];
        let exit_t = if node == fs.a { 0.0 } else { 1.0 };
        let mut out = Vec::with_capacity(middle.len() + 2);
        out.push(Piece {
            seg: from.seg,
            t0: from.t,
            t1: exit_t,
        });
        out.extend(middle);
        out.push(Piece {
            seg: c.seg,
            t0: if via_a { 0.0 } else { 1.0 },
            t1: c.t,
        });
        out.retain(|p| p.t0 != p.t1);
        out
    }
}

/// The candidates of an observation: the nearest projections, one per position.
pub fn candidates(net: &Network, o: &Obs, params: &Params) -> Vec<Cand> {
    let mpu = net.grid.metres_per_unit(o.lat);
    let r = params.search_radius_m / mpu;
    let mut out: Vec<Cand> = net
        .index
        .locate_within_distance(o.p, r * r)
        .map(|g| {
            let (a, b) = net.seg_points(g.data);
            let (t, pos) = geo::project_on_segment(o.p, a, b);
            Cand {
                seg: g.data,
                t,
                pos,
                dist_m: geo::dist_units(o.p, pos) * mpu,
            }
        })
        .filter(|c| c.dist_m <= params.search_radius_m)
        .collect();
    out.sort_by(|a, b| a.dist_m.total_cmp(&b.dist_m));
    let mut seen: HashSet<(i64, i64)> = HashSet::new();
    out.retain(|c| {
        seen.insert((
            (c.pos[0] * 4.0).round() as i64,
            (c.pos[1] * 4.0).round() as i64,
        ))
    });
    out.truncate(params.max_candidates);
    out
}

/// Matches the trace `points` (lat, lon) on the network.
pub fn match_trace(net: &Network, points: &[(f64, f64)], params: &Params) -> Matched {
    let obs = densify(net.grid, points, params.step_m);
    let n = obs.len();
    let cands: Vec<Vec<Cand>> = obs.iter().map(|o| candidates(net, o, params)).collect();
    let any_candidate = cands.iter().any(|c| !c.is_empty());
    let mut dij = Dijkstra::new(net.nodes.len());

    // Viterbi, in costs (negative log probabilities). back[t][j]: the best predecessor of
    // candidate j of t; u16::MAX starts a chain.
    let footway_cost = if net.mode == Mode::Bicycle {
        params.footway_cost
    } else {
        0.0
    };
    let emission = |c: &Cand| {
        let side = match net.class_of(c.seg).info.use_ {
            Use::Footway | Use::Steps | Use::Pedestrian => footway_cost,
            _ => 0.0,
        };
        0.5 * (c.dist_m / params.sigma_z).powi(2) + side
    };
    let mut back: Vec<Vec<u16>> = Vec::with_capacity(n);
    let mut chain_start = vec![false; n];
    let mut prev_cost: Vec<f64> = Vec::new();
    let mut last_costs: Vec<Vec<f64>> = vec![Vec::new(); n];
    for t in 0..n {
        let cur = &cands[t];
        if cur.is_empty() {
            back.push(Vec::new());
            prev_cost.clear();
            continue;
        }
        let mut cost = vec![f64::INFINITY; cur.len()];
        let mut bp = vec![u16::MAX; cur.len()];
        if t > 0 && !prev_cost.is_empty() {
            let gc = geo::haversine_m(obs[t - 1].lat, obs[t - 1].lon, obs[t].lat, obs[t].lon);
            let limit = 2.0 * gc + 60.0;
            let targets: HashSet<u32> = cur
                .iter()
                .flat_map(|c| [net.segs[c.seg as usize].a, net.segs[c.seg as usize].b])
                .collect();
            let mpu = net.grid.metres_per_unit(obs[t].lat);
            for (i, ci) in cands[t - 1].iter().enumerate() {
                if !prev_cost[i].is_finite() {
                    continue;
                }
                dij.run_from(net, ci, &targets, limit);
                for (j, cj) in cur.iter().enumerate() {
                    let tr = match dij.route_to(net, ci, cj) {
                        Some((route, _)) if route <= limit => (route - gc).abs() / params.beta,
                        _ => {
                            let straight = geo::dist_units(ci.pos, cj.pos) * mpu;
                            if straight > params.max_jump_m {
                                continue;
                            }
                            params.jump_cost + (straight - gc).abs() / params.beta
                        }
                    };
                    let c = prev_cost[i] + tr;
                    if c < cost[j] {
                        cost[j] = c;
                        bp[j] = i as u16;
                    }
                }
            }
        }
        if cost.iter().all(|c| !c.is_finite()) {
            chain_start[t] = true;
            cost.iter_mut().for_each(|c| *c = 0.0);
            bp.iter_mut().for_each(|b| *b = u16::MAX);
        }
        for (j, c) in cur.iter().enumerate() {
            cost[j] += emission(c);
        }
        // Keep the numbers small on long traces.
        let min = cost.iter().copied().fold(f64::INFINITY, f64::min);
        cost.iter_mut().for_each(|c| *c -= min);
        last_costs[t] = cost.clone();
        prev_cost = cost;
        back.push(bp);
    }

    // Backtrack every chain from its end.
    let mut chosen_idx: Vec<Option<usize>> = vec![None; n];
    let mut t = n;
    while t > 0 {
        t -= 1;
        if cands[t].is_empty() || chosen_idx[t].is_some() {
            continue;
        }
        // t is the end of a chain: the best final candidate.
        let costs = &last_costs[t];
        let mut j = (0..costs.len())
            .min_by(|&a, &b| costs[a].total_cmp(&costs[b]))
            .unwrap_or(0);
        let mut k = t;
        loop {
            chosen_idx[k] = Some(j);
            let b = back[k][j];
            if b == u16::MAX || k == 0 {
                break;
            }
            j = b as usize;
            k -= 1;
        }
        t = k;
    }
    let chosen: Vec<Option<Cand>> = (0..n).map(|t| chosen_idx[t].map(|j| cands[t][j])).collect();

    // The steps between consecutive matched observations of one chain.
    let mut steps: Vec<Option<Step>> = vec![None; n];
    for t in 0..n.saturating_sub(1) {
        let (Some(ji), Some(jj)) = (chosen_idx[t], chosen_idx[t + 1]) else {
            continue;
        };
        if back[t + 1][jj] != ji as u16 {
            continue; // a new chain starts at t + 1
        }
        let (ci, cj) = (cands[t][ji], cands[t + 1][jj]);
        let gc = geo::haversine_m(obs[t].lat, obs[t].lon, obs[t + 1].lat, obs[t + 1].lon);
        let limit = 2.0 * gc + 60.0;
        let targets: HashSet<u32> = [net.segs[cj.seg as usize].a, net.segs[cj.seg as usize].b]
            .into_iter()
            .collect();
        dij.run_from(net, &ci, &targets, limit);
        steps[t] = Some(match dij.route_to(net, &ci, &cj) {
            Some((route, via)) if route <= limit => Step::Route(dij.pieces_to(net, &ci, &cj, via)),
            _ => Step::Jump,
        });
    }
    Matched {
        obs,
        chosen,
        steps,
        any_candidate,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::AccessRules;
    use crate::mvt::{StreetAttrs, StreetLine, StreetTile};
    use crate::network::Corridor;
    use crate::valhalla::Mode;
    use std::sync::Arc;

    const LAT: f64 = 47.2;
    const LON: f64 = -1.5;

    fn street(kind: &str, surface: Option<&str>) -> StreetAttrs {
        StreetAttrs {
            kind: kind.into(),
            surface: surface.map(Into::into),
            ..Default::default()
        }
    }

    /// Metres east / north of the origin, as (lat, lon).
    fn at(east: f64, north: f64) -> (f64, f64) {
        let lat = LAT + (north / geo::EARTH_RADIUS_M).to_degrees();
        let lon = LON + (east / (geo::EARTH_RADIUS_M * LAT.to_radians().cos())).to_degrees();
        (lat, lon)
    }

    /// A paved road along y = 0 from x = 0 to 600 m, a gravel track parallel to it 20 m north
    /// from x = 100 to 500, joined to the road at both ends, and a motorway 10 m south.
    fn network(mode: Mode) -> Network {
        let grid = geo::Grid::new(14, 4096);
        let u = |e: f64, n: f64| {
            let (lat, lon) = at(e, n);
            grid.to_units(lon, lat)
        };
        let mut tile = StreetTile::default();
        let mut add = |a: StreetAttrs, pts: Vec<[f64; 2]>| {
            tile.attrs.push(a);
            tile.lines.push(StreetLine {
                attr: (tile.attrs.len() - 1) as u32,
                points: pts,
            });
        };
        add(
            street("residential", Some("paved")),
            (0..=12).map(|k| u(k as f64 * 50.0, 0.0)).collect(),
        );
        add(
            StreetAttrs {
                tracktype: Some("grade2".into()),
                ..street("track", None)
            },
            vec![
                u(100.0, 0.0),
                u(100.0, 20.0),
                u(300.0, 20.0),
                u(500.0, 20.0),
                u(500.0, 0.0),
            ],
        );
        add(
            street("motorway", Some("paved")),
            vec![u(0.0, -10.0), u(600.0, -10.0)],
        );
        let tiles = vec![Arc::new(tile)];
        let line: Vec<([f64; 2], f64)> = (0..=12).map(|k| (u(k as f64 * 50.0, 0.0), LAT)).collect();
        Network::build(
            grid,
            &tiles,
            &Corridor::new(grid, &line, 200.0),
            mode,
            AccessRules::France,
            None,
        )
    }

    fn surfaces_along(net: &Network, m: &Matched) -> Vec<&'static str> {
        m.chosen
            .iter()
            .flatten()
            .map(|c| net.class_of(c.seg).info.surface.as_str())
            .collect()
    }

    #[test]
    fn follows_the_parallel_track_it_is_on() {
        let net = network(Mode::Bicycle);
        // On the road to x = 100, then on the track (18 m north: noisy), back on the road.
        let trace = vec![
            at(0.0, 1.0),
            at(100.0, 1.0),
            at(100.0, 19.0),
            at(300.0, 18.0),
            at(500.0, 19.0),
            at(500.0, 1.0),
            at(600.0, 0.0),
        ];
        let m = match_trace(&net, &trace, &Params::default());
        assert!(m.chosen.iter().all(Option::is_some));
        let s = surfaces_along(&net, &m);
        assert_eq!(s.first(), Some(&"paved_smooth"));
        assert_eq!(s.last(), Some(&"paved_smooth"));
        let compacted = s.iter().filter(|v| **v == "compacted").count();
        assert!(
            compacted > 40,
            "{compacted} observations on the track: {s:?}"
        );
        // The motorway is not for bicycles: never a candidate.
        assert!(m
            .chosen
            .iter()
            .flatten()
            .all(|c| net.class_of(c.seg).attrs.kind != "motorway"));
        // Every step between two matched observations is a route on the network.
        assert!(m
            .steps
            .iter()
            .flatten()
            .all(|s| matches!(s, Step::Route(_))));
    }

    #[test]
    fn far_points_are_unmatched_and_cut_the_chain() {
        let net = network(Mode::Bicycle);
        let trace = vec![
            at(0.0, 0.0),
            at(200.0, 0.0),
            at(300.0, 300.0),
            at(400.0, 0.0),
            at(600.0, 0.0),
        ];
        let m = match_trace(&net, &trace, &Params::default());
        let far = m.obs.iter().position(|o| o.input == Some(2)).unwrap();
        assert!(m.chosen[far].is_none());
        assert!(m.chosen[0].is_some() && m.chosen.last().unwrap().is_some());
    }

    #[test]
    fn a_car_takes_the_motorway_when_it_is_on_it() {
        let net = network(Mode::Auto);
        let trace = vec![at(0.0, -10.5), at(300.0, -9.5), at(600.0, -10.0)];
        let m = match_trace(&net, &trace, &Params::default());
        assert!(m
            .chosen
            .iter()
            .flatten()
            .all(|c| net.class_of(c.seg).attrs.kind == "motorway"));
    }
}
