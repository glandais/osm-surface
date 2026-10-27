//! Mapbox Vector Tile decoding (prost messages written from `vector_tile.proto` v2.1), limited
//! to what the matcher needs: the LineStrings of the Shortbread `streets` layer, in global
//! units, clipped to the tile's own square so that neighbouring tiles do not overlap.

use crate::geo::Grid;
use prost::Message;
use std::collections::HashMap;

#[derive(Clone, PartialEq, Message)]
struct RawTile {
    /// Layers kept as raw bytes: only the one named `streets` is decoded in full.
    #[prost(bytes = "vec", repeated, tag = "3")]
    layers: Vec<Vec<u8>>,
}

#[derive(Clone, PartialEq, Message)]
struct LayerName {
    #[prost(string, tag = "1")]
    name: String,
}

#[derive(Clone, PartialEq, Message)]
struct Layer {
    #[prost(uint32, tag = "15")]
    version: u32,
    #[prost(string, tag = "1")]
    name: String,
    #[prost(message, repeated, tag = "2")]
    features: Vec<Feature>,
    #[prost(string, repeated, tag = "3")]
    keys: Vec<String>,
    #[prost(message, repeated, tag = "4")]
    values: Vec<Value>,
    #[prost(uint32, optional, tag = "5")]
    extent: Option<u32>,
}

#[derive(Clone, PartialEq, Message)]
struct Feature {
    #[prost(uint64, optional, tag = "1")]
    id: Option<u64>,
    #[prost(uint32, repeated, tag = "2")]
    tags: Vec<u32>,
    #[prost(int32, optional, tag = "3")]
    geom_type: Option<i32>,
    #[prost(uint32, repeated, tag = "4")]
    geometry: Vec<u32>,
}

#[derive(Clone, PartialEq, Message)]
struct Value {
    #[prost(string, optional, tag = "1")]
    string_value: Option<String>,
    #[prost(float, optional, tag = "2")]
    float_value: Option<f32>,
    #[prost(double, optional, tag = "3")]
    double_value: Option<f64>,
    #[prost(int64, optional, tag = "4")]
    int_value: Option<i64>,
    #[prost(uint64, optional, tag = "5")]
    uint_value: Option<u64>,
    #[prost(sint64, optional, tag = "6")]
    sint_value: Option<i64>,
    #[prost(bool, optional, tag = "7")]
    bool_value: Option<bool>,
}

impl Value {
    fn as_string(&self) -> Option<String> {
        if let Some(s) = &self.string_value {
            return Some(s.clone());
        }
        if let Some(b) = self.bool_value {
            return Some(b.to_string());
        }
        self.int_value
            .or(self.sint_value)
            .map(|v| v.to_string())
            .or(self.uint_value.map(|v| v.to_string()))
            .or(self.double_value.map(|v| v.to_string()))
            .or(self.float_value.map(|v| v.to_string()))
    }

    fn as_bool(&self) -> bool {
        match (&self.bool_value, &self.string_value) {
            (Some(b), _) => *b,
            (None, Some(s)) => s == "true" || s == "yes" || s == "1",
            _ => {
                self.int_value.or(self.sint_value).unwrap_or(0) != 0
                    || self.uint_value.unwrap_or(0) != 0
            }
        }
    }
}

const GEOM_LINESTRING: i32 = 2;
pub const STREETS_LAYER: &str = "streets";

/// The attributes of a Shortbread 1.0 `streets` feature (the OSM tags that survived the
/// schema; there is no OSM way id in the schema).
#[derive(Debug, Clone, Default, PartialEq, Eq, Hash, serde::Serialize, serde::Deserialize)]
pub struct StreetAttrs {
    pub kind: String,
    pub surface: Option<String>,
    pub tracktype: Option<String>,
    pub service: Option<String>,
    pub bicycle: Option<String>,
    pub horse: Option<String>,
    pub link: bool,
    pub rail: bool,
    pub bridge: bool,
    pub tunnel: bool,
    pub oneway: bool,
    pub oneway_reverse: bool,
}

/// One line of the `streets` layer, clipped to its tile, in global units.
#[derive(Debug, Clone)]
pub struct StreetLine {
    /// Index into [`StreetTile::attrs`].
    pub attr: u32,
    pub points: Vec<[f64; 2]>,
}

/// The decoded `streets` layer of a tile.
#[derive(Debug, Clone, Default)]
pub struct StreetTile {
    pub x: u32,
    pub y: u32,
    pub attrs: Vec<StreetAttrs>,
    pub lines: Vec<StreetLine>,
}

impl StreetTile {
    pub fn segment_count(&self) -> usize {
        self.lines
            .iter()
            .map(|l| l.points.len().saturating_sub(1))
            .sum()
    }
}

#[derive(Debug, thiserror::Error)]
pub enum MvtError {
    #[error("invalid vector tile: {0}")]
    Decode(#[from] prost::DecodeError),
    #[error("invalid gzip vector tile: {0}")]
    Gzip(#[from] std::io::Error),
}

/// Decodes the `streets` layer of tile (x, y) of the grid's zoom. A gzip-wrapped tile is
/// accepted too; an empty body is an empty tile.
pub fn decode_streets(bytes: &[u8], grid: Grid, x: u32, y: u32) -> Result<StreetTile, MvtError> {
    let mut tile = StreetTile {
        x,
        y,
        ..Default::default()
    };
    if bytes.is_empty() {
        return Ok(tile);
    }
    let owned;
    let bytes = if bytes.len() > 2 && bytes[0] == 0x1f && bytes[1] == 0x8b {
        let mut out = Vec::new();
        std::io::Read::read_to_end(&mut flate2::read::GzDecoder::new(bytes), &mut out)?;
        owned = out;
        &owned[..]
    } else {
        bytes
    };
    let raw = RawTile::decode(bytes)?;
    for layer_bytes in &raw.layers {
        if LayerName::decode(&layer_bytes[..])?.name != STREETS_LAYER {
            continue;
        }
        let layer = Layer::decode(&layer_bytes[..])?;
        decode_layer(&layer, grid, &mut tile);
    }
    Ok(tile)
}

fn decode_layer(layer: &Layer, grid: Grid, tile: &mut StreetTile) {
    let extent = layer.extent.unwrap_or(4096).max(1) as f64;
    let scale = grid.extent as f64 / extent;
    let x0 = tile.x as f64 * grid.extent as f64;
    let y0 = tile.y as f64 * grid.extent as f64;
    let bbox = [x0, y0, x0 + grid.extent as f64, y0 + grid.extent as f64];
    let mut interned: HashMap<StreetAttrs, u32> = HashMap::new();
    for f in &layer.features {
        if f.geom_type != Some(GEOM_LINESTRING) {
            continue;
        }
        let attrs = feature_attrs(layer, f);
        let attr = *interned.entry(attrs.clone()).or_insert_with(|| {
            tile.attrs.push(attrs);
            (tile.attrs.len() - 1) as u32
        });
        for line in decode_lines(&f.geometry) {
            let global: Vec<[f64; 2]> = line
                .iter()
                .map(|&(px, py)| [x0 + px as f64 * scale, y0 + py as f64 * scale])
                .collect();
            for clipped in clip_polyline(&global, bbox) {
                if clipped.len() >= 2 {
                    tile.lines.push(StreetLine {
                        attr,
                        points: clipped,
                    });
                }
            }
        }
    }
}

fn feature_attrs(layer: &Layer, f: &Feature) -> StreetAttrs {
    let mut a = StreetAttrs::default();
    for pair in f.tags.as_chunks::<2>().0 {
        let (Some(key), Some(value)) = (
            layer.keys.get(pair[0] as usize),
            layer.values.get(pair[1] as usize),
        ) else {
            continue;
        };
        match key.as_str() {
            "kind" => a.kind = value.as_string().unwrap_or_default(),
            "surface" => a.surface = value.as_string(),
            "tracktype" => a.tracktype = value.as_string(),
            "service" => a.service = value.as_string(),
            "bicycle" => a.bicycle = value.as_string(),
            "horse" => a.horse = value.as_string(),
            "link" => a.link = value.as_bool(),
            "rail" => a.rail = value.as_bool(),
            "bridge" => a.bridge = value.as_bool(),
            "tunnel" => a.tunnel = value.as_bool(),
            "oneway" => a.oneway = value.as_bool(),
            "oneway_reverse" => a.oneway_reverse = value.as_bool(),
            _ => {}
        }
    }
    a
}

fn zigzag(v: u32) -> i32 {
    ((v >> 1) as i32) ^ -((v & 1) as i32)
}

/// The linestrings of a feature's command stream, in tile pixels.
fn decode_lines(geometry: &[u32]) -> Vec<Vec<(i32, i32)>> {
    let mut lines: Vec<Vec<(i32, i32)>> = Vec::new();
    let (mut cx, mut cy) = (0i32, 0i32);
    let mut i = 0;
    while i < geometry.len() {
        let cmd = geometry[i] & 0x7;
        let count = (geometry[i] >> 3) as usize;
        i += 1;
        match cmd {
            1 | 2 => {
                for _ in 0..count {
                    let (Some(&dx), Some(&dy)) = (geometry.get(i), geometry.get(i + 1)) else {
                        return lines;
                    };
                    i += 2;
                    cx = cx.wrapping_add(zigzag(dx));
                    cy = cy.wrapping_add(zigzag(dy));
                    if cmd == 1 {
                        lines.push(vec![(cx, cy)]);
                    } else if let Some(last) = lines.last_mut() {
                        if last.last() != Some(&(cx, cy)) {
                            last.push((cx, cy));
                        }
                    }
                }
            }
            7 => {
                // ClosePath: a closed street (roundabout drawn as a line) gets its first point back.
                if let Some(last) = lines.last_mut() {
                    if let Some(&first) = last.first() {
                        last.push(first);
                    }
                }
            }
            _ => return lines,
        }
    }
    lines
}

/// Clips a polyline to an axis-aligned box: the pieces inside, in order. The cut points are
/// computed from the segment's endpoints taken in a canonical order, so the two tiles of a
/// border compute (nearly) the same point.
pub fn clip_polyline(points: &[[f64; 2]], bbox: [f64; 4]) -> Vec<Vec<[f64; 2]>> {
    let mut out: Vec<Vec<[f64; 2]>> = Vec::new();
    let mut current: Vec<[f64; 2]> = Vec::new();
    for w in points.windows(2) {
        match clip_segment(w[0], w[1], bbox) {
            Some((a, b)) => {
                if current.last().is_none_or(|l| *l != a) {
                    if current.len() >= 2 {
                        out.push(std::mem::take(&mut current));
                    } else {
                        current.clear();
                    }
                    current.push(a);
                }
                if a != b {
                    current.push(b);
                }
            }
            None => {
                if current.len() >= 2 {
                    out.push(std::mem::take(&mut current));
                } else {
                    current.clear();
                }
            }
        }
    }
    if current.len() >= 2 {
        out.push(current);
    }
    out
}

fn clip_segment(p: [f64; 2], q: [f64; 2], bbox: [f64; 4]) -> Option<([f64; 2], [f64; 2])> {
    let inside =
        |r: [f64; 2]| r[0] >= bbox[0] && r[0] <= bbox[2] && r[1] >= bbox[1] && r[1] <= bbox[3];
    if inside(p) && inside(q) {
        return Some((p, q));
    }
    // Canonical order: the cut depends only on the unordered pair of endpoints.
    let swap = (q[0], q[1]) < (p[0], p[1]);
    let (a, b) = if swap { (q, p) } else { (p, q) };
    let (dx, dy) = (b[0] - a[0], b[1] - a[1]);
    let (mut t0, mut t1) = (0.0f64, 1.0f64);
    for (pk, qk) in [
        (-dx, a[0] - bbox[0]),
        (dx, bbox[2] - a[0]),
        (-dy, a[1] - bbox[1]),
        (dy, bbox[3] - a[1]),
    ] {
        if pk == 0.0 {
            if qk < 0.0 {
                return None;
            }
        } else {
            let r = qk / pk;
            if pk < 0.0 {
                t0 = t0.max(r);
            } else {
                t1 = t1.min(r);
            }
        }
    }
    if t0 > t1 {
        return None;
    }
    let at = |t: f64| -> [f64; 2] {
        if t <= 0.0 {
            a
        } else if t >= 1.0 {
            b
        } else {
            let mut r = [a[0] + t * dx, a[1] + t * dy];
            // Snap the coordinate that was cut onto the border exactly.
            for (k, edge) in [(0, bbox[0]), (0, bbox[2]), (1, bbox[1]), (1, bbox[3])] {
                if (r[k] - edge).abs() < 1e-6 {
                    r[k] = edge;
                }
            }
            r
        }
    };
    let (ca, cb) = (at(t0), at(t1));
    if ca == cb {
        return None;
    }
    Some(if swap { (cb, ca) } else { (ca, cb) })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_real_tile_decodes() {
        // VersaTiles Shortbread z14 tile 8118/5742 (near Nantes), © OpenStreetMap contributors
        let bytes = include_bytes!(
            "../tests/fixtures/tiles/tiles.versatiles.org_tiles_osm-9602913b/14/8118/5742.mvt"
        );
        let grid = Grid::new(14, 4096);
        let tile = decode_streets(bytes, grid, 8118, 5742).unwrap();
        assert!(tile.lines.len() > 100, "{} lines", tile.lines.len());
        assert!(tile
            .attrs
            .iter()
            .any(|a| a.kind == "track" && a.tracktype.is_some()));
        assert!(tile
            .attrs
            .iter()
            .any(|a| a.surface.as_deref() == Some("paved")));
        assert!(tile.attrs.iter().all(|a| !a.kind.is_empty()));
        // every point inside the tile's square, in global units
        let (x0, y0) = (8118.0 * 4096.0, 5742.0 * 4096.0);
        for l in &tile.lines {
            for p in &l.points {
                assert!(p[0] >= x0 && p[0] <= x0 + 4096.0 && p[1] >= y0 && p[1] <= y0 + 4096.0);
            }
        }
        // the same bytes gzip-wrapped
        let mut gz = flate2::write::GzEncoder::new(Vec::new(), flate2::Compression::fast());
        std::io::Write::write_all(&mut gz, bytes).unwrap();
        let again = decode_streets(&gz.finish().unwrap(), grid, 8118, 5742).unwrap();
        assert_eq!(again.lines.len(), tile.lines.len());
        assert_eq!(decode_streets(&[], grid, 1, 1).unwrap().lines.len(), 0);
    }

    #[test]
    fn zigzag_decodes() {
        assert_eq!(zigzag(0), 0);
        assert_eq!(zigzag(1), -1);
        assert_eq!(zigzag(2), 1);
        assert_eq!(zigzag(3), -2);
    }

    #[test]
    fn commands_decode_into_lines() {
        // MoveTo(2,2) LineTo(+0,+8) (+8,+0) ; MoveTo(+1,+1) LineTo(+2,+0)
        let g = [9, 4, 4, 18, 0, 16, 16, 0, 9, 2, 2, 10, 4, 0];
        let lines = decode_lines(&g);
        assert_eq!(
            lines,
            vec![vec![(2, 2), (2, 10), (10, 10)], vec![(11, 11), (13, 11)]]
        );
    }

    #[test]
    fn clipping_cuts_at_the_border_symmetrically() {
        let a = [10.0, 10.0];
        let b = [30.0, 20.0];
        let left = clip_polyline(&[a, b], [0.0, 0.0, 20.0, 40.0]);
        let right = clip_polyline(&[b, a], [20.0, 0.0, 40.0, 40.0]);
        assert_eq!(left, vec![vec![a, [20.0, 15.0]]]);
        assert_eq!(right, vec![vec![b, [20.0, 15.0]]]);
        // fully outside
        assert!(clip_polyline(&[[50.0, 50.0], [60.0, 60.0]], [0.0, 0.0, 20.0, 20.0]).is_empty());
        // in, out, in again: two pieces
        let pieces = clip_polyline(
            &[[5.0, 5.0], [30.0, 5.0], [30.0, 10.0], [5.0, 10.0]],
            [0.0, 0.0, 20.0, 20.0],
        );
        assert_eq!(pieces.len(), 2);
    }
}
