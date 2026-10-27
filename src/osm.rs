//! The optional OSM index (`OSM_INDEX`): the highway ways of an OSM extract with the tags
//! Valhalla reads, so that the streets of the tiles get their exact tags back.
//!
//! The Shortbread tiles reduce `surface` to `paved` / `unpaved`, drop `smoothness`,
//! `mtb:scale`, `sac_scale`, `access`, `foot`, and have no bridleways; their data is also a
//! few months old. The `osm-index` tool writes the ways of a (daily, e.g. Geofabrik) OSM
//! extract into a compact file; at request time each tile segment is joined to the OSM way
//! it lies on (nearest, parallel, same or related highway type), and the ways the tiles lack
//! are added to the network. Without an index, the tiles alone are used.
//!
//! File format (little endian): `OSMSURF1`, u32 version, source and replication time
//! (u16-prefixed strings), a string table, the tag sets (string ids of the fields of
//! [`FIELDS`] and a flag byte), then the ways (i64 id, u32 tag set, u32 point count, and the
//! points as i32 latitude / longitude in 1e-7 degrees).

use crate::geo::{self, Grid};
use crate::mvt::StreetAttrs;
use crate::valhalla;
use rstar::primitives::{GeomWithData, Rectangle};
use rstar::{RTree, AABB};
use std::collections::HashMap;
use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::sync::{Arc, RwLock};
use std::time::SystemTime;

const MAGIC: &[u8; 8] = b"OSMSURF1";
const VERSION: u32 = 1;
const NONE: u32 = u32::MAX;

/// The kinds the VersaTiles `streets` layer does not have: their ways are added from the
/// index to the network.
pub const EXTRA_KINDS: &[&str] = &["bridleway"];

/// The string fields of a tag set, in file order.
const FIELDS: usize = 12;

/// A tile segment is joined to an OSM way within this distance.
pub const JOIN_RADIUS_M: f64 = 8.0;
/// ... whose direction is within this angle (degrees) of the segment's.
const JOIN_MAX_ANGLE: f64 = 40.0;
/// Segments of the ways per entry of the spatial index.
const CHUNK: usize = 8;

/// Where the index comes from.
#[derive(Debug, Clone, serde::Serialize)]
pub struct IndexInfo {
    pub path: PathBuf,
    pub source: String,
    /// The OSM replication timestamp of the extract (ISO 8601), when the file had one.
    pub replication_time: Option<String>,
    pub ways: usize,
    pub points: usize,
}

/// The OSM tags of a highway way, as Valhalla reads them, in the shape of the tile
/// attributes (`kind` is the Shortbread kind of the `highway` value). `None`: not a way of
/// the street network.
pub fn attrs_from_tags<'a>(get: impl Fn(&str) -> Option<&'a str>) -> Option<StreetAttrs> {
    let highway = get("highway")?;
    if get("area") == Some("yes") {
        return None;
    }
    let (base, link) = match highway.strip_suffix("_link") {
        Some(b) => (b, true),
        None => (highway, false),
    };
    let kind = match (base, get("footway")) {
        ("footway", Some("sidewalk")) => "sidewalk",
        ("footway", Some("crossing")) => "crossing",
        (k, _) => k,
    };
    if !valhalla::is_routable_kind(kind) {
        return None;
    }
    let s = |k: &str| get(k).map(str::to_string);
    let yes = |k: &str| get(k).is_some_and(|v| v != "no" && v != "false" && v != "0");
    let oneway = get("oneway");
    Some(StreetAttrs {
        kind: kind.to_string(),
        surface: s("surface"),
        tracktype: s("tracktype"),
        service: s("service"),
        bicycle: s("bicycle"),
        horse: s("horse"),
        link,
        rail: false,
        bridge: yes("bridge"),
        tunnel: yes("tunnel"),
        oneway: matches!(oneway, Some("yes" | "true" | "1")),
        oneway_reverse: oneway == Some("-1"),
        osm_id: None,
        smoothness: s("smoothness"),
        mtb_scale: s("mtb:scale"),
        sac_scale: s("sac_scale"),
        foot: s("foot"),
        access: s("access"),
        vehicle: s("vehicle"),
    })
}

fn fields(a: &StreetAttrs) -> [Option<&str>; FIELDS] {
    [
        Some(a.kind.as_str()),
        a.surface.as_deref(),
        a.tracktype.as_deref(),
        a.service.as_deref(),
        a.bicycle.as_deref(),
        a.horse.as_deref(),
        a.smoothness.as_deref(),
        a.mtb_scale.as_deref(),
        a.sac_scale.as_deref(),
        a.foot.as_deref(),
        a.access.as_deref(),
        a.vehicle.as_deref(),
    ]
}

fn set_fields(a: &mut StreetAttrs, v: [Option<String>; FIELDS]) {
    let [kind, surface, tracktype, service, bicycle, horse, smoothness, mtb, sac, foot, access, vehicle] =
        v;
    a.kind = kind.unwrap_or_default();
    a.surface = surface;
    a.tracktype = tracktype;
    a.service = service;
    a.bicycle = bicycle;
    a.horse = horse;
    a.smoothness = smoothness;
    a.mtb_scale = mtb;
    a.sac_scale = sac;
    a.foot = foot;
    a.access = access;
    a.vehicle = vehicle;
}

/// A way to write: id, tag set, points (lat, lon in 1e-7 degrees).
type RawWay = (i64, u32, Vec<(i32, i32)>);

/// Builds an index file.
#[derive(Default)]
pub struct IndexWriter {
    strings: Vec<String>,
    string_ids: HashMap<String, u32>,
    tagsets: Vec<StreetAttrs>,
    tagset_ids: HashMap<StreetAttrs, u32>,
    ways: Vec<RawWay>,
}

impl IndexWriter {
    pub fn add_way(&mut self, id: i64, attrs: StreetAttrs, points: Vec<(i32, i32)>) {
        if points.len() < 2 {
            return;
        }
        let next = self.tagsets.len() as u32;
        let t = *self.tagset_ids.entry(attrs.clone()).or_insert(next);
        if t == next {
            self.tagsets.push(attrs);
        }
        self.ways.push((id, t, points));
    }

    pub fn way_count(&self) -> usize {
        self.ways.len()
    }

    fn string_id(&mut self, s: Option<&str>) -> u32 {
        let Some(s) = s else { return NONE };
        if let Some(&i) = self.string_ids.get(s) {
            return i;
        }
        self.strings.push(s.to_string());
        let i = (self.strings.len() - 1) as u32;
        self.string_ids.insert(s.to_string(), i);
        i
    }

    pub fn write(
        mut self,
        out: &mut impl Write,
        source: &str,
        replication_time: Option<&str>,
    ) -> std::io::Result<()> {
        let tagsets = std::mem::take(&mut self.tagsets);
        let encoded: Vec<([u32; FIELDS], u8)> = tagsets
            .iter()
            .map(|a| {
                let f = fields(a);
                let mut ids = [NONE; FIELDS];
                for (k, v) in f.iter().enumerate() {
                    ids[k] = self.string_id(*v);
                }
                let flags = a.link as u8
                    | (a.bridge as u8) << 1
                    | (a.tunnel as u8) << 2
                    | (a.oneway as u8) << 3
                    | (a.oneway_reverse as u8) << 4;
                (ids, flags)
            })
            .collect();
        out.write_all(MAGIC)?;
        out.write_all(&VERSION.to_le_bytes())?;
        write_str(out, source)?;
        write_str(out, replication_time.unwrap_or(""))?;
        out.write_all(&(self.strings.len() as u32).to_le_bytes())?;
        for s in &self.strings {
            write_str(out, s)?;
        }
        out.write_all(&(encoded.len() as u32).to_le_bytes())?;
        for (ids, flags) in &encoded {
            for id in ids {
                out.write_all(&id.to_le_bytes())?;
            }
            out.write_all(&[*flags])?;
        }
        out.write_all(&(self.ways.len() as u32).to_le_bytes())?;
        for (id, t, pts) in &self.ways {
            out.write_all(&id.to_le_bytes())?;
            out.write_all(&t.to_le_bytes())?;
            out.write_all(&(pts.len() as u32).to_le_bytes())?;
            for (lat, lon) in pts {
                out.write_all(&lat.to_le_bytes())?;
                out.write_all(&lon.to_le_bytes())?;
            }
        }
        Ok(())
    }
}

fn write_str(out: &mut impl Write, s: &str) -> std::io::Result<()> {
    let b = &s.as_bytes()[..s.len().min(u16::MAX as usize)];
    out.write_all(&(b.len() as u16).to_le_bytes())?;
    out.write_all(b)
}

struct Reader<R: Read>(R);

impl<R: Read> Reader<R> {
    fn bytes<const N: usize>(&mut self) -> std::io::Result<[u8; N]> {
        let mut b = [0u8; N];
        self.0.read_exact(&mut b)?;
        Ok(b)
    }
    fn u32(&mut self) -> std::io::Result<u32> {
        Ok(u32::from_le_bytes(self.bytes()?))
    }
    fn i32(&mut self) -> std::io::Result<i32> {
        Ok(i32::from_le_bytes(self.bytes()?))
    }
    fn i64(&mut self) -> std::io::Result<i64> {
        Ok(i64::from_le_bytes(self.bytes()?))
    }
    fn string(&mut self) -> std::io::Result<String> {
        let n = u16::from_le_bytes(self.bytes()?) as usize;
        let mut b = vec![0u8; n];
        self.0.read_exact(&mut b)?;
        String::from_utf8(b).map_err(|e| std::io::Error::new(std::io::ErrorKind::InvalidData, e))
    }
}

struct Way {
    id: i64,
    tags: u32,
    start: u32,
    len: u32,
}

type ChunkTree = RTree<GeomWithData<Rectangle<[f64; 2]>, u32>>;

/// The loaded index, in the global units of a grid.
pub struct OsmIndex {
    pub info: IndexInfo,
    tagsets: Vec<StreetAttrs>,
    ways: Vec<Way>,
    /// Points in global units times `scale`, as u32 (half the memory of f64 pairs).
    points: Vec<[u32; 2]>,
    scale: f64,
    /// (way, first point, points) per chunk of CHUNK segments.
    chunks: Vec<(u32, u32, u8)>,
    tree: ChunkTree,
    extra: ChunkTree,
}

impl OsmIndex {
    pub fn load(path: &Path, grid: Grid) -> anyhow::Result<Self> {
        let file = std::fs::File::open(path)
            .map_err(|e| anyhow::anyhow!("cannot open the OSM index {}: {e}", path.display()))?;
        Self::read(std::io::BufReader::with_capacity(1 << 20, file), path, grid)
    }

    pub fn read(r: impl Read, path: &Path, grid: Grid) -> anyhow::Result<Self> {
        let mut r = Reader(r);
        if &r.bytes::<8>()? != MAGIC {
            anyhow::bail!("{} is not an osm-surface index", path.display());
        }
        let version = r.u32()?;
        if version != VERSION {
            anyhow::bail!(
                "{}: index version {version}, expected {VERSION}",
                path.display()
            );
        }
        let source = r.string()?;
        let replication_time = Some(r.string()?).filter(|s| !s.is_empty());
        let n_strings = r.u32()? as usize;
        let mut strings = Vec::with_capacity(n_strings);
        for _ in 0..n_strings {
            strings.push(r.string()?);
        }
        let n_tagsets = r.u32()? as usize;
        let mut tagsets = Vec::with_capacity(n_tagsets);
        for _ in 0..n_tagsets {
            let mut v: [Option<String>; FIELDS] = Default::default();
            for f in v.iter_mut() {
                let id = r.u32()?;
                *f = (id != NONE)
                    .then(|| strings.get(id as usize).cloned())
                    .flatten();
            }
            let flags = r.bytes::<1>()?[0];
            let mut a = StreetAttrs {
                link: flags & 1 != 0,
                bridge: flags & 2 != 0,
                tunnel: flags & 4 != 0,
                oneway: flags & 8 != 0,
                oneway_reverse: flags & 16 != 0,
                ..Default::default()
            };
            set_fields(&mut a, v);
            tagsets.push(a);
        }
        let n_ways = r.u32()? as usize;
        let world = grid.world();
        let scale = (2f64.powi(31) / world).floor().max(1.0);
        let mut ways = Vec::with_capacity(n_ways);
        let mut points: Vec<[u32; 2]> = Vec::with_capacity(n_ways * 10);
        for _ in 0..n_ways {
            let id = r.i64()?;
            let tags = r.u32()?;
            let n = r.u32()?;
            if tags as usize >= tagsets.len() {
                anyhow::bail!("{}: corrupt index (tag set {tags})", path.display());
            }
            let start = points.len() as u32;
            for _ in 0..n {
                let lat = r.i32()? as f64 * 1e-7;
                let lon = r.i32()? as f64 * 1e-7;
                let p = grid.to_units(lon, lat);
                points.push([(p[0] * scale).round() as u32, (p[1] * scale).round() as u32]);
            }
            ways.push(Way {
                id,
                tags,
                start,
                len: n,
            });
        }
        let mut chunks = Vec::new();
        let mut entries = Vec::new();
        let mut extra_entries = Vec::new();
        for (wi, w) in ways.iter().enumerate() {
            let is_extra = EXTRA_KINDS.contains(&tagsets[w.tags as usize].kind.as_str());
            let mut first = 0usize;
            while first + 1 < w.len as usize {
                let n = (w.len as usize - first).min(CHUNK + 1);
                let (mut lo, mut hi) = ([f64::MAX; 2], [f64::MIN; 2]);
                for k in 0..n {
                    let q = points[w.start as usize + first + k];
                    for d in 0..2 {
                        let v = q[d] as f64 / scale;
                        lo[d] = lo[d].min(v);
                        hi[d] = hi[d].max(v);
                    }
                }
                let ci = chunks.len() as u32;
                chunks.push((wi as u32, w.start + first as u32, n as u8));
                let rect = Rectangle::from_corners(lo, hi);
                if is_extra {
                    extra_entries.push(GeomWithData::new(rect, ci));
                }
                entries.push(GeomWithData::new(rect, ci));
                first += n - 1;
            }
        }
        let info = IndexInfo {
            path: path.to_path_buf(),
            source,
            replication_time,
            ways: ways.len(),
            points: points.len(),
        };
        Ok(Self {
            info,
            tagsets,
            ways,
            points,
            scale,
            chunks,
            tree: RTree::bulk_load(entries),
            extra: RTree::bulk_load(extra_entries),
        })
    }

    fn pt(&self, i: u32) -> [f64; 2] {
        let q = self.points[i as usize];
        [q[0] as f64 / self.scale, q[1] as f64 / self.scale]
    }

    /// The tags of a way, with its id.
    pub fn attrs(&self, way: u32) -> StreetAttrs {
        let w = &self.ways[way as usize];
        let mut a = self.tagsets[w.tags as usize].clone();
        a.osm_id = Some(w.id);
        a
    }

    /// The OSM way the tile segment `a`-`b` (global units) of a street of kind `tile` lies
    /// on: the way most of the points along the segment are nearest to, among those within
    /// [`JOIN_RADIUS_M`] and roughly parallel; a different highway type costs a few metres
    /// (it is how the data changed since the tiles were built: a path became a track...).
    /// `extra_cost` adds metres to a way (the network builder penalises the ways the travel
    /// mode cannot use, when the tile street could: a way split since into a cycleway and a
    /// footway closed to bicycles is the cycleway for a bicycle).
    pub fn join(
        &self,
        a: [f64; 2],
        b: [f64; 2],
        tile: &StreetAttrs,
        mpu: f64,
        extra_cost: &mut dyn FnMut(u32) -> f64,
    ) -> Option<u32> {
        let len_m = geo::dist_units(a, b) * mpu;
        let samples = ((len_m / 8.0).ceil() as usize).clamp(1, 12);
        let r = JOIN_RADIUS_M / mpu;
        let dir = [b[0] - a[0], b[1] - a[1]];
        let check_angle = len_m >= 2.0;
        let mut votes: Vec<(u32, u32, f64)> = Vec::new();
        for k in 0..samples {
            let p = geo::lerp(a, b, (k as f64 + 0.5) / samples as f64);
            let env = AABB::from_corners([p[0] - r, p[1] - r], [p[0] + r, p[1] + r]);
            let mut best: Option<(f64, u32)> = None;
            for g in self.tree.locate_in_envelope_intersecting(env) {
                let (wi, first, n) = self.chunks[g.data as usize];
                let w = &self.ways[wi as usize];
                let attrs = &self.tagsets[w.tags as usize];
                let kind_cost = kind_cost(&tile.kind, &attrs.kind)
                    + extra_cost(wi)
                    + if (tile.bridge, tile.tunnel) != (attrs.bridge, attrs.tunnel) {
                        3.0
                    } else {
                        0.0
                    };
                for s in 0..(n as u32 - 1) {
                    let (q0, q1) = (self.pt(first + s), self.pt(first + s + 1));
                    let (_, q) = geo::project_on_segment(p, q0, q1);
                    let d = geo::dist_units(p, q) * mpu;
                    if d > JOIN_RADIUS_M {
                        continue;
                    }
                    let mut angle_cost = 0.0;
                    if check_angle {
                        let angle = undirected_angle(dir, [q1[0] - q0[0], q1[1] - q0[1]]);
                        if angle > JOIN_MAX_ANGLE {
                            continue;
                        }
                        angle_cost = 2.0 * angle / JOIN_MAX_ANGLE;
                    }
                    let score = d + kind_cost + angle_cost;
                    if score <= JOIN_RADIUS_M && best.is_none_or(|(bs, _)| score < bs) {
                        best = Some((score, wi));
                    }
                }
            }
            if let Some((score, wi)) = best {
                match votes.iter_mut().find(|v| v.0 == wi) {
                    Some(v) => {
                        v.1 += 1;
                        v.2 += score;
                    }
                    None => votes.push((wi, 1, score)),
                }
            }
        }
        let (wi, count, _) = votes
            .into_iter()
            .max_by(|x, y| x.1.cmp(&y.1).then(y.2.total_cmp(&x.2)))?;
        // Most of the segment must lie along it: not a way that only crosses it.
        (count as usize * 2 >= samples).then_some(wi)
    }

    /// The ways within `radius` (global units) of `p`, nearest first, with their distance.
    pub fn ways_near(&self, p: [f64; 2], radius: f64) -> Vec<(u32, f64)> {
        let env = AABB::from_corners(
            [p[0] - radius, p[1] - radius],
            [p[0] + radius, p[1] + radius],
        );
        let mut best: HashMap<u32, f64> = HashMap::new();
        for g in self.tree.locate_in_envelope_intersecting(env) {
            let (wi, first, n) = self.chunks[g.data as usize];
            for s in 0..(n as u32 - 1) {
                let (_, q) = geo::project_on_segment(p, self.pt(first + s), self.pt(first + s + 1));
                let d = geo::dist_units(p, q);
                if d <= radius {
                    let e = best.entry(wi).or_insert(d);
                    *e = e.min(d);
                }
            }
        }
        let mut out: Vec<(u32, f64)> = best.into_iter().collect();
        out.sort_by(|a, b| a.1.total_cmp(&b.1));
        out
    }

    /// The ways of the kinds the tiles lack ([`EXTRA_KINDS`]) that cross the box.
    pub fn extra_ways(&self, lo: [f64; 2], hi: [f64; 2]) -> Vec<u32> {
        let mut out: Vec<u32> = self
            .extra
            .locate_in_envelope_intersecting(AABB::from_corners(lo, hi))
            .map(|g| self.chunks[g.data as usize].0)
            .collect();
        out.sort_unstable();
        out.dedup();
        out
    }

    /// The points of a way in global units, rounded to whole units as tile vertices are, so
    /// that a junction node of the tiles and of the index falls on the same point.
    pub fn way_points(&self, way: u32) -> Vec<[f64; 2]> {
        let w = &self.ways[way as usize];
        (w.start..w.start + w.len)
            .map(|i| {
                let p = self.pt(i);
                [p[0].round(), p[1].round()]
            })
            .collect()
    }
}

/// What it costs (metres) to join a tile street of one kind to an OSM way of another.
fn kind_cost(tile: &str, osm: &str) -> f64 {
    if tile == osm {
        return 0.0;
    }
    const PATHS: &[&str] = &[
        "path",
        "track",
        "footway",
        "sidewalk",
        "crossing",
        "cycleway",
        "bridleway",
        "steps",
        "pedestrian",
    ];
    const MINOR: &[&str] = &[
        "track",
        "service",
        "unclassified",
        "residential",
        "living_street",
        "road",
        "pedestrian",
    ];
    const MAJOR: &[&str] = &[
        "motorway",
        "trunk",
        "primary",
        "secondary",
        "tertiary",
        "unclassified",
        "residential",
    ];
    let related = [PATHS, MINOR, MAJOR]
        .iter()
        .any(|g| g.contains(&tile) && g.contains(&osm));
    if related {
        2.0
    } else {
        6.0
    }
}

/// The angle (degrees, 0..90) between two lines, whatever their direction.
fn undirected_angle(u: [f64; 2], v: [f64; 2]) -> f64 {
    let (nu, nv) = (u[0].hypot(u[1]), v[0].hypot(v[1]));
    if nu <= 0.0 || nv <= 0.0 {
        return 0.0;
    }
    let c = ((u[0] * v[0] + u[1] * v[1]) / (nu * nv)).abs().min(1.0);
    c.acos().to_degrees()
}

/// The extra join cost of a way the travel mode cannot use.
pub const UNUSABLE_JOIN_COST: f64 = 3.0;

/// The index of a running service: loaded at startup, reloaded when its file changes.
pub struct OsmHandle {
    path: Option<PathBuf>,
    grid: Grid,
    current: RwLock<Option<(Arc<OsmIndex>, Option<SystemTime>)>>,
}

impl OsmHandle {
    /// Loads the index if a path is configured; a configured index that cannot be read is
    /// an error (better not to start than to answer with the tiles alone unnoticed).
    pub fn new(path: Option<PathBuf>, grid: Grid) -> anyhow::Result<Self> {
        let current = match &path {
            Some(p) => {
                let started = std::time::Instant::now();
                let index = OsmIndex::load(p, grid)?;
                tracing::info!(
                    path = %p.display(),
                    ways = index.info.ways,
                    points = index.info.points,
                    replication_time = index.info.replication_time.as_deref().unwrap_or("?"),
                    ms = started.elapsed().as_millis() as u64,
                    "OSM index loaded"
                );
                Some((Arc::new(index), mtime(p)))
            }
            None => None,
        };
        Ok(Self {
            path,
            grid,
            current: RwLock::new(current),
        })
    }

    pub fn none(grid: Grid) -> Self {
        Self {
            path: None,
            grid,
            current: RwLock::new(None),
        }
    }

    pub fn current(&self) -> Option<Arc<OsmIndex>> {
        self.current
            .read()
            .ok()?
            .as_ref()
            .map(|(i, _)| Arc::clone(i))
    }

    pub fn info(&self) -> Option<IndexInfo> {
        self.current().map(|i| i.info.clone())
    }

    /// Reloads the index if its file changed (a new extract was indexed). Blocking.
    pub fn reload_if_changed(&self) {
        let Some(p) = &self.path else { return };
        let now = mtime(p);
        let known = self
            .current
            .read()
            .ok()
            .and_then(|c| c.as_ref().map(|(_, m)| *m));
        if now.is_none() || known == Some(now) {
            return;
        }
        match OsmIndex::load(p, self.grid) {
            Ok(index) => {
                tracing::info!(
                    ways = index.info.ways,
                    replication_time = index.info.replication_time.as_deref().unwrap_or("?"),
                    "OSM index reloaded"
                );
                if let Ok(mut c) = self.current.write() {
                    *c = Some((Arc::new(index), now));
                }
            }
            Err(e) => {
                tracing::warn!(error = %e, "cannot reload the OSM index, keeping the old one")
            }
        }
    }
}

fn mtime(p: &Path) -> Option<SystemTime> {
    std::fs::metadata(p).ok()?.modified().ok()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tags(kv: &[(&'static str, &'static str)]) -> Option<StreetAttrs> {
        attrs_from_tags(|k| kv.iter().find(|(a, _)| *a == k).map(|(_, v)| *v))
    }

    #[test]
    fn osm_tags_become_street_attributes() {
        let a = tags(&[
            ("highway", "footway"),
            ("footway", "sidewalk"),
            ("surface", "asphalt"),
        ])
        .unwrap();
        assert_eq!(
            (a.kind.as_str(), a.surface.as_deref()),
            ("sidewalk", Some("asphalt"))
        );
        let l = tags(&[("highway", "secondary_link"), ("bridge", "yes")]).unwrap();
        assert!(l.link && l.bridge && l.kind == "secondary");
        assert!(tags(&[("highway", "pedestrian"), ("area", "yes")]).is_none());
        assert!(tags(&[("highway", "proposed")]).is_none());
        assert!(tags(&[("building", "yes")]).is_none());
        let m = tags(&[
            ("highway", "path"),
            ("mtb:scale", "2"),
            ("sac_scale", "hiking"),
        ])
        .unwrap();
        assert_eq!(
            (m.mtb_scale.as_deref(), m.sac_scale.as_deref()),
            (Some("2"), Some("hiking"))
        );
    }

    #[test]
    fn round_trip_and_join() {
        let grid = Grid::new(14, 4096);
        let e7 = |lat: f64, lon: f64| ((lat * 1e7).round() as i32, (lon * 1e7).round() as i32);
        let mut w = IndexWriter::default();
        // a track going east, a parallel road 20 m north, a bridleway going north
        w.add_way(
            1,
            tags(&[("highway", "track"), ("surface", "compacted")]).unwrap(),
            vec![e7(47.0, -1.5), e7(47.0, -1.49)],
        );
        w.add_way(
            2,
            tags(&[("highway", "residential")]).unwrap(),
            vec![e7(47.00018, -1.5), e7(47.00018, -1.49)],
        );
        w.add_way(
            3,
            tags(&[("highway", "bridleway"), ("bicycle", "yes")]).unwrap(),
            vec![e7(47.0, -1.495), e7(47.01, -1.495)],
        );
        let mut buf = Vec::new();
        w.write(&mut buf, "test", Some("2026-10-25T21:00:00Z"))
            .unwrap();
        let idx = OsmIndex::read(&buf[..], Path::new("mem"), grid).unwrap();
        assert_eq!(idx.info.ways, 3);
        assert_eq!(
            idx.info.replication_time.as_deref(),
            Some("2026-10-25T21:00:00Z")
        );
        let mpu = grid.metres_per_unit(47.0);
        // a tile "path" 2 m south of the track: joined to the track (data changed since)
        let off = 2.0 / mpu;
        let a = grid.to_units(-1.499, 47.0);
        let b = grid.to_units(-1.497, 47.0);
        let path = StreetAttrs {
            kind: "path".into(),
            surface: Some("unpaved".into()),
            ..Default::default()
        };
        let j = idx
            .join(
                [a[0], a[1] + off],
                [b[0], b[1] + off],
                &path,
                mpu,
                &mut |_| 0.0,
            )
            .unwrap();
        let attrs = idx.attrs(j);
        assert_eq!(
            (attrs.osm_id, attrs.surface.as_deref()),
            (Some(1), Some("compacted"))
        );
        // a tile segment going north is not joined to the east-west ways
        let c = grid.to_units(-1.4985, 47.0001);
        let d = grid.to_units(-1.4985, 47.0003);
        assert!(idx.join(c, d, &path, mpu, &mut |_| 0.0).is_none());
        // 30 m away from anything: nothing
        let far = 30.0 / mpu;
        assert!(idx
            .join(
                [a[0], a[1] + far],
                [b[0], b[1] + far],
                &path,
                mpu,
                &mut |_| 0.0
            )
            .is_none());
        // the extra cost of a way counts against it
        assert!(idx
            .join(
                [a[0], a[1] + off],
                [b[0], b[1] + off],
                &path,
                mpu,
                &mut |w| {
                    if w == j {
                        10.0
                    } else {
                        0.0
                    }
                }
            )
            .is_none());
        // the bridleway is an extra way
        let extra = idx.extra_ways(grid.to_units(-1.5, 47.01), grid.to_units(-1.49, 47.0));
        assert_eq!(extra.len(), 1);
        assert_eq!(idx.attrs(extra[0]).kind, "bridleway");
        let pts = idx.way_points(extra[0]);
        assert!(pts.len() == 2 && pts[0][0].fract() == 0.0);
    }
}
