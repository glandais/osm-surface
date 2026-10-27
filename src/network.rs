//! The street graph of a request: the `streets` lines of the tiles around the trace, cut
//! into straight segments between nodes, with a spatial index.
//!
//! The tiles carry no node ids: nodes are recovered from coordinates. Lines of one tile share
//! their quantised vertices at junctions; lines cut at a tile border are rejoined by merging
//! the dangling ends that lie within [`MERGE_UNITS`]; and a dangling end that lies on another
//! line where the simplification removed the junction vertex is spliced into it.

use crate::config::AccessRules;
use crate::geo::{self, Grid};
use crate::mvt::{StreetAttrs, StreetTile};
use crate::valhalla::{self, EdgeInfo, Mode};
use rstar::primitives::{GeomWithData, Line};
use rstar::RTree;
use std::collections::{HashMap, HashSet};
use std::sync::Arc;

/// Dangling ends closer than this (global units, ~0.4 m at 47°N) are one node.
const MERGE_UNITS: f64 = 2.0;
/// A dangling end this close to another line's interior is spliced into it.
const SPLICE_UNITS: f64 = 1.5;

/// A class of edges: the Shortbread attributes and what Valhalla would make of them.
#[derive(Debug, Clone)]
pub struct EdgeClass {
    pub attrs: StreetAttrs,
    pub info: EdgeInfo,
}

#[derive(Debug, Clone, Copy)]
pub struct Seg {
    pub a: u32,
    pub b: u32,
    pub len_m: f64,
    pub class: u32,
}

pub type SegIndex = RTree<GeomWithData<Line<[f64; 2]>, u32>>;

pub struct Network {
    pub grid: Grid,
    /// The travel mode the network was filtered for.
    pub mode: Mode,
    pub nodes: Vec<[f64; 2]>,
    pub segs: Vec<Seg>,
    pub classes: Vec<EdgeClass>,
    /// CSR adjacency: the (segment, other node) pairs of node `n` are
    /// `adj[adj_start[n]..adj_start[n + 1]]`.
    adj_start: Vec<u32>,
    adj: Vec<(u32, u32)>,
    pub index: SegIndex,
}

/// The cells (of [`Corridor::CELL_UNITS`]) within some distance of the trace: what is outside
/// is never needed to match it.
pub struct Corridor {
    cells: HashSet<(i64, i64)>,
}

impl Corridor {
    const CELL_UNITS: f64 = 128.0;

    /// `points` (a line) in global units, with their latitude (for the metre scale).
    pub fn new(grid: Grid, points: &[([f64; 2], f64)], radius_m: f64) -> Self {
        let mut cells = HashSet::new();
        let mut mark = |p: [f64; 2], lat: f64| {
            let r = ((radius_m / grid.metres_per_unit(lat)) / Self::CELL_UNITS).ceil() as i64;
            let (cx, cy) = Self::cell(p);
            for dx in -r..=r {
                for dy in -r..=r {
                    cells.insert((cx + dx, cy + dy));
                }
            }
        };
        for (i, &(p, lat)) in points.iter().enumerate() {
            mark(p, lat);
            // Between two points, every half cell: the squares around them overlap.
            if let Some(&(q, lat2)) = points.get(i + 1) {
                let steps = (geo::dist_units(p, q) / (Self::CELL_UNITS / 2.0)).ceil() as usize;
                for k in 1..steps {
                    let f = k as f64 / steps as f64;
                    mark(geo::lerp(p, q, f), lat + (lat2 - lat) * f);
                }
            }
        }
        Self { cells }
    }

    fn cell(p: [f64; 2]) -> (i64, i64) {
        (
            (p[0] / Self::CELL_UNITS).floor() as i64,
            (p[1] / Self::CELL_UNITS).floor() as i64,
        )
    }

    pub fn contains(&self, p: [f64; 2]) -> bool {
        self.cells.contains(&Self::cell(p))
    }

    fn touches(&self, a: [f64; 2], b: [f64; 2]) -> bool {
        let d = geo::dist_units(a, b);
        let steps = (d / (Self::CELL_UNITS / 2.0)).ceil().max(1.0) as usize;
        (0..=steps).any(|k| self.contains(geo::lerp(a, b, k as f64 / steps as f64)))
    }
}

impl Network {
    /// The graph of the streets of `tiles` that `mode` may use, within the corridor.
    pub fn build(
        grid: Grid,
        tiles: &[Arc<StreetTile>],
        corridor: &Corridor,
        mode: Mode,
        rules: AccessRules,
    ) -> Self {
        let mut classes: Vec<EdgeClass> = Vec::new();
        let mut class_ids: HashMap<StreetAttrs, Option<u32>> = HashMap::new();
        let mut class_of = |attrs: &StreetAttrs, classes: &mut Vec<EdgeClass>| -> Option<u32> {
            *class_ids.entry(attrs.clone()).or_insert_with(|| {
                if attrs.rail || !valhalla::is_routable_kind(&attrs.kind) {
                    return None;
                }
                let info = valhalla::derive(attrs, rules);
                if !info.allows(mode) {
                    return None;
                }
                classes.push(EdgeClass {
                    attrs: attrs.clone(),
                    info,
                });
                Some((classes.len() - 1) as u32)
            })
        };
        let mut node_ids: HashMap<(i64, i64), u32> = HashMap::new();
        let mut nodes: Vec<[f64; 2]> = Vec::new();
        let mut raw: Vec<(u32, u32, u32)> = Vec::new();
        let mut node_of = |p: [f64; 2], nodes: &mut Vec<[f64; 2]>| -> u32 {
            let key = ((p[0] * 16.0).round() as i64, (p[1] * 16.0).round() as i64);
            *node_ids.entry(key).or_insert_with(|| {
                nodes.push(p);
                (nodes.len() - 1) as u32
            })
        };
        for tile in tiles {
            for line in &tile.lines {
                let Some(class) = class_of(&tile.attrs[line.attr as usize], &mut classes) else {
                    continue;
                };
                for w in line.points.windows(2) {
                    if !corridor.touches(w[0], w[1]) {
                        continue;
                    }
                    let a = node_of(w[0], &mut nodes);
                    let b = node_of(w[1], &mut nodes);
                    if a != b {
                        raw.push((a, b, class));
                    }
                }
            }
        }
        let layers: Vec<u8> = classes.iter().map(|c| layer(&c.attrs)).collect();
        let raw = repair(&mut nodes, raw, &layers);
        let segs: Vec<Seg> = raw
            .into_iter()
            .map(|(a, b, class)| {
                let (lon1, lat1) = grid.to_lonlat(nodes[a as usize]);
                let (lon2, lat2) = grid.to_lonlat(nodes[b as usize]);
                Seg {
                    a,
                    b,
                    len_m: geo::haversine_m(lat1, lon1, lat2, lon2),
                    class,
                }
            })
            .collect();
        let mut degree = vec![0u32; nodes.len() + 1];
        for s in &segs {
            degree[s.a as usize] += 1;
            degree[s.b as usize] += 1;
        }
        let mut adj_start = vec![0u32; nodes.len() + 1];
        for n in 0..nodes.len() {
            adj_start[n + 1] = adj_start[n] + degree[n];
        }
        let mut fill = adj_start.clone();
        let mut adj = vec![(0u32, 0u32); adj_start[nodes.len()] as usize];
        for (i, s) in segs.iter().enumerate() {
            adj[fill[s.a as usize] as usize] = (i as u32, s.b);
            fill[s.a as usize] += 1;
            adj[fill[s.b as usize] as usize] = (i as u32, s.a);
            fill[s.b as usize] += 1;
        }
        let index = RTree::bulk_load(
            segs.iter()
                .enumerate()
                .map(|(i, s)| {
                    GeomWithData::new(
                        Line::new(nodes[s.a as usize], nodes[s.b as usize]),
                        i as u32,
                    )
                })
                .collect(),
        );
        Network {
            grid,
            mode,
            nodes,
            segs,
            classes,
            adj_start,
            adj,
            index,
        }
    }

    pub fn neighbours(&self, node: u32) -> &[(u32, u32)] {
        &self.adj
            [self.adj_start[node as usize] as usize..self.adj_start[node as usize + 1] as usize]
    }

    pub fn seg_points(&self, seg: u32) -> ([f64; 2], [f64; 2]) {
        let s = &self.segs[seg as usize];
        (self.nodes[s.a as usize], self.nodes[s.b as usize])
    }

    pub fn class_of(&self, seg: u32) -> &EdgeClass {
        &self.classes[self.segs[seg as usize].class as usize]
    }

    pub fn is_empty(&self) -> bool {
        self.segs.is_empty()
    }
}

/// 0 on the ground, 1 on a bridge, 2 in a tunnel: lines on two levels never connect.
fn layer(a: &StreetAttrs) -> u8 {
    if a.bridge {
        1
    } else if a.tunnel {
        2
    } else {
        0
    }
}

/// Merges the dangling ends that are one point cut in two (tile borders), and splices the
/// vertices that lie on another line of the same level into it (a junction whose shared
/// vertex the tile simplification removed). Returns the segments, renumbered.
fn repair(
    nodes: &mut [[f64; 2]],
    raw: Vec<(u32, u32, u32)>,
    layers: &[u8],
) -> Vec<(u32, u32, u32)> {
    let mut degree = vec![0u32; nodes.len()];
    for &(a, b, _) in &raw {
        degree[a as usize] += 1;
        degree[b as usize] += 1;
    }
    // 1. Merge dangling ends with any node within MERGE_UNITS.
    let mut bucket: HashMap<(i64, i64), Vec<u32>> = HashMap::new();
    let cell = |p: [f64; 2]| {
        (
            (p[0] / MERGE_UNITS).floor() as i64,
            (p[1] / MERGE_UNITS).floor() as i64,
        )
    };
    for (i, p) in nodes.iter().enumerate() {
        if degree[i] > 0 {
            bucket.entry(cell(*p)).or_default().push(i as u32);
        }
    }
    let mut parent: Vec<u32> = (0..nodes.len() as u32).collect();
    fn find(parent: &mut [u32], mut x: u32) -> u32 {
        while parent[x as usize] != x {
            parent[x as usize] = parent[parent[x as usize] as usize];
            x = parent[x as usize];
        }
        x
    }
    for i in 0..nodes.len() {
        if degree[i] != 1 {
            continue;
        }
        let p = nodes[i];
        let (cx, cy) = cell(p);
        let mut best: Option<(f64, u32)> = None;
        for dx in -1..=1 {
            for dy in -1..=1 {
                if let Some(list) = bucket.get(&(cx + dx, cy + dy)) {
                    for &j in list {
                        if j as usize == i {
                            continue;
                        }
                        let d = geo::dist_units(p, nodes[j as usize]);
                        if d <= MERGE_UNITS && best.is_none_or(|(bd, _)| d < bd) {
                            best = Some((d, j));
                        }
                    }
                }
            }
        }
        if let Some((_, j)) = best {
            let (ri, rj) = (find(&mut parent, i as u32), find(&mut parent, j));
            if ri != rj {
                // Keep the node that has more lines (a real vertex) as the representative.
                if degree[ri as usize] >= degree[rj as usize] {
                    parent[rj as usize] = ri;
                } else {
                    parent[ri as usize] = rj;
                }
            }
        }
    }
    let mut segs: Vec<(u32, u32, u32)> = raw
        .into_iter()
        .filter_map(|(a, b, c)| {
            let (ra, rb) = (find(&mut parent, a), find(&mut parent, b));
            (ra != rb).then_some((ra, rb, c))
        })
        .collect();
    segs.sort_unstable();
    segs.dedup();

    // 2. Splice the vertices into the segment interior of the same level they lie on.
    let mut node_layers = vec![0u8; nodes.len()];
    for &(a, b, c) in &segs {
        let bit = 1u8 << layers[c as usize];
        node_layers[a as usize] |= bit;
        node_layers[b as usize] |= bit;
    }
    let tree: RTree<GeomWithData<Line<[f64; 2]>, u32>> = RTree::bulk_load(
        segs.iter()
            .enumerate()
            .map(|(i, &(a, b, _))| {
                GeomWithData::new(Line::new(nodes[a as usize], nodes[b as usize]), i as u32)
            })
            .collect(),
    );
    let mut splits: HashMap<u32, Vec<(f64, u32)>> = HashMap::new();
    for n in 0..nodes.len() {
        if node_layers[n] == 0 {
            continue;
        }
        let p = nodes[n];
        let mut best: Option<(f64, u32, f64)> = None;
        for g in tree.locate_within_distance(p, SPLICE_UNITS * SPLICE_UNITS) {
            let si = g.data;
            let (a, b, c) = segs[si as usize];
            if a as usize == n
                || b as usize == n
                || node_layers[n] & (1u8 << layers[c as usize]) == 0
            {
                continue;
            }
            let (t, q) = geo::project_on_segment(p, nodes[a as usize], nodes[b as usize]);
            let d = geo::dist_units(p, q);
            if t > 0.0 && t < 1.0 && d <= SPLICE_UNITS && best.is_none_or(|(bd, _, _)| d < bd) {
                best = Some((d, si, t));
            }
        }
        if let Some((_, si, t)) = best {
            splits.entry(si).or_default().push((t, n as u32));
        }
    }
    if splits.is_empty() {
        return segs;
    }
    let mut out = Vec::with_capacity(segs.len() + splits.len() * 2);
    for (i, &(a, b, c)) in segs.iter().enumerate() {
        match splits.get_mut(&(i as u32)) {
            None => out.push((a, b, c)),
            Some(list) => {
                list.sort_by(|x, y| x.0.total_cmp(&y.0));
                let mut prev = a;
                for &(_, n) in list.iter() {
                    if n != prev {
                        out.push((prev, n, c));
                        prev = n;
                    }
                }
                if prev != b {
                    out.push((prev, b, c));
                }
            }
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::mvt::StreetLine;

    fn tile(lines: Vec<(StreetAttrs, Vec<[f64; 2]>)>) -> Arc<StreetTile> {
        let mut t = StreetTile::default();
        for (a, pts) in lines {
            t.attrs.push(a);
            t.lines.push(StreetLine {
                attr: (t.attrs.len() - 1) as u32,
                points: pts,
            });
        }
        Arc::new(t)
    }

    fn road() -> StreetAttrs {
        StreetAttrs {
            kind: "residential".into(),
            ..Default::default()
        }
    }

    #[test]
    fn border_cuts_and_t_junctions_are_rejoined() {
        let grid = Grid::new(14, 4096);
        let base = grid.to_units(-1.5, 47.2);
        let at = |dx: f64, dy: f64| [base[0] + dx, base[1] + dy];
        let tiles = vec![
            // one street cut in two with a 0.3-unit mismatch at the cut
            tile(vec![(road(), vec![at(0.0, 0.0), at(100.0, 0.0)])]),
            tile(vec![(road(), vec![at(100.0, 0.3), at(200.0, 0.0)])]),
            // a street ending on the middle of the first one, with no shared vertex
            tile(vec![(road(), vec![at(50.0, 0.5), at(50.0, 100.0)])]),
            // a motorway, not for bicycles
            tile(vec![(
                StreetAttrs {
                    kind: "motorway".into(),
                    ..Default::default()
                },
                vec![at(0.0, 10.0), at(200.0, 10.0)],
            )]),
        ];
        let points: Vec<([f64; 2], f64)> =
            (0..=20).map(|k| (at(k as f64 * 10.0, 0.0), 47.2)).collect();
        let corridor = Corridor::new(grid, &points, 200.0);
        let net = Network::build(grid, &tiles, &corridor, Mode::Bicycle, AccessRules::France);
        // 0-50, 50-100, 100-200, and the side street
        assert_eq!(net.segs.len(), 4, "{:?}", net.segs);
        assert_eq!(net.classes.len(), 1);
        let junction = net
            .segs
            .iter()
            .filter(|s| s.a == net.segs[3].a || s.b == net.segs[3].a)
            .count();
        assert!(junction >= 2);
        let total: f64 = net.segs.iter().map(|s| s.len_m).sum();
        assert!(
            (total - 300.0 * grid.metres_per_unit(47.2)).abs() < 1.0,
            "{total}"
        );
    }
}
