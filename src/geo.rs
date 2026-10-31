//! Geodesy helpers: haversine distances and the Web Mercator "global unit" grid
//! the vector tiles are drawn on.

/// Mean Earth radius used by the client's `GeoDistance` (and most haversine code).
pub const EARTH_RADIUS_M: f64 = 6_371_000.0;

/// Equatorial circumference of the Web Mercator sphere (EPSG:3857).
const MERCATOR_CIRCUMFERENCE_M: f64 = 40_075_016.685_578_5;

/// Haversine distance in metres between two positions in degrees.
pub fn haversine_m(lat1: f64, lon1: f64, lat2: f64, lon2: f64) -> f64 {
    let d_lat = (lat2 - lat1).to_radians();
    let d_lon = (lon2 - lon1).to_radians();
    let a = (d_lat / 2.0).sin().powi(2)
        + lat1.to_radians().cos() * lat2.to_radians().cos() * (d_lon / 2.0).sin().powi(2);
    2.0 * EARTH_RADIUS_M * a.sqrt().min(1.0).asin()
}

/// A grid of "global units": the pixel grid of every tile of one zoom level laid side by
/// side, `extent` units per tile. Two tiles of the same zoom share it, so a vertex
/// quantised at the same OSM node lands on the same global coordinate in both.
#[derive(Debug, Clone, Copy)]
pub struct Grid {
    pub zoom: u8,
    pub extent: u32,
}

impl Grid {
    pub fn new(zoom: u8, extent: u32) -> Self {
        Self { zoom, extent }
    }

    /// Size of the whole world in global units.
    pub fn world(&self) -> f64 {
        (1u64 << self.zoom) as f64 * self.extent as f64
    }

    /// Longitude/latitude (degrees) to global units (x to the east, y to the south).
    pub fn to_units(&self, lon: f64, lat: f64) -> [f64; 2] {
        let w = self.world();
        let lat = lat.clamp(-85.051_128_78, 85.051_128_78).to_radians();
        let x = (lon + 180.0) / 360.0 * w;
        let y = (1.0 - lat.tan().asinh() / std::f64::consts::PI) / 2.0 * w;
        [x, y]
    }

    /// Global units to longitude/latitude (degrees).
    pub fn to_lonlat(&self, p: [f64; 2]) -> (f64, f64) {
        let w = self.world();
        let lon = p[0] / w * 360.0 - 180.0;
        let n = std::f64::consts::PI * (1.0 - 2.0 * p[1] / w);
        let lat = n.sinh().atan().to_degrees();
        (lon, lat)
    }

    /// Metres per global unit at a latitude (Mercator is conformal: the same in x and y).
    pub fn metres_per_unit(&self, lat: f64) -> f64 {
        MERCATOR_CIRCUMFERENCE_M * lat.to_radians().cos() / self.world()
    }

    /// The tile (x, y) holding a position in global units.
    pub fn tile_of(&self, p: [f64; 2]) -> (u32, u32) {
        let n = 1u32 << self.zoom;
        let e = self.extent as f64;
        let tx = ((p[0] / e).floor().max(0.0) as u32).min(n - 1);
        let ty = ((p[1] / e).floor().max(0.0) as u32).min(n - 1);
        (tx, ty)
    }
}

/// Projection of `p` on the segment `a`-`b`: the fraction along it (0..1) and the point.
pub fn project_on_segment(p: [f64; 2], a: [f64; 2], b: [f64; 2]) -> (f64, [f64; 2]) {
    let dx = b[0] - a[0];
    let dy = b[1] - a[1];
    let len2 = dx * dx + dy * dy;
    if len2 <= 0.0 {
        return (0.0, a);
    }
    let t = (((p[0] - a[0]) * dx + (p[1] - a[1]) * dy) / len2).clamp(0.0, 1.0);
    (t, [a[0] + t * dx, a[1] + t * dy])
}

pub fn dist_units(a: [f64; 2], b: [f64; 2]) -> f64 {
    ((a[0] - b[0]).powi(2) + (a[1] - b[1]).powi(2)).sqrt()
}

pub fn lerp(a: [f64; 2], b: [f64; 2], t: f64) -> [f64; 2] {
    [a[0] + (b[0] - a[0]) * t, a[1] + (b[1] - a[1]) * t]
}

/// The tiles within `radius_m` of a line through `points` (lat, lon), densified so that no
/// tile the line crosses is missed.
pub fn corridor_tiles(grid: Grid, points: &[(f64, f64)], radius_m: f64) -> Vec<(u32, u32)> {
    let mut set = std::collections::BTreeSet::new();
    let mut add = |lat: f64, lon: f64| {
        let p = grid.to_units(lon, lat);
        let r = radius_m / grid.metres_per_unit(lat);
        for dx in [-r, 0.0, r] {
            for dy in [-r, 0.0, r] {
                set.insert(grid.tile_of([p[0] + dx, p[1] + dy]));
            }
        }
    };
    for (i, &(lat, lon)) in points.iter().enumerate() {
        add(lat, lon);
        if let Some(&(lat2, lon2)) = points.get(i + 1) {
            let d = haversine_m(lat, lon, lat2, lon2);
            let steps = (d / 100.0).ceil() as usize;
            for k in 1..steps {
                let t = k as f64 / steps as f64;
                add(lat + (lat2 - lat) * t, lon + (lon2 - lon) * t);
            }
        }
    }
    set.into_iter().collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn haversine_matches_a_known_distance() {
        // One degree of latitude on the 6371 km sphere.
        let d = haversine_m(48.0, 2.0, 49.0, 2.0);
        assert!((d - 111_194.9).abs() < 1.0, "{d}");
    }

    #[test]
    fn units_round_trip() {
        let g = Grid::new(14, 4096);
        let p = g.to_units(-1.5457, 47.2057);
        let (lon, lat) = g.to_lonlat(p);
        assert!((lon + 1.5457).abs() < 1e-9 && (lat - 47.2057).abs() < 1e-9);
        assert_eq!(g.tile_of(p), (8121, 5748));
        // ~0.406 m per unit at 47°N
        assert!((g.metres_per_unit(47.2) - 0.4057).abs() < 0.001);
    }
}
