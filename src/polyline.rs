//! Google encoded polylines with 6 digits of precision (Valhalla's `polyline6`), latitude
//! first in each pair.

const PRECISION: f64 = 1e6;

/// Decodes a polyline6 into `(lat, lon)` pairs. `None` when the string is malformed.
pub fn decode(encoded: &str) -> Option<Vec<(f64, f64)>> {
    decode_with(encoded, PRECISION)
}

fn decode_with(encoded: &str, precision: f64) -> Option<Vec<(f64, f64)>> {
    let bytes = encoded.as_bytes();
    let mut out = Vec::with_capacity(bytes.len() / 8);
    let mut i = 0;
    let (mut lat, mut lon) = (0i64, 0i64);
    while i < bytes.len() {
        let dlat = read(bytes, &mut i)?;
        let dlon = read(bytes, &mut i)?;
        // A valid delta fits in 32 bits; the checked sums reject forged input anyway.
        lat = lat.checked_add(dlat)?;
        lon = lon.checked_add(dlon)?;
        out.push((lat as f64 / precision, lon as f64 / precision));
    }
    Some(out)
}

fn read(bytes: &[u8], i: &mut usize) -> Option<i64> {
    let mut result: i64 = 0;
    let mut shift = 0u32;
    loop {
        let b = *bytes.get(*i)? as i64 - 63;
        *i += 1;
        // 7 chunks (35 bits) hold any valid polyline6 delta (|delta| < 2^29 · 2).
        if !(0..64).contains(&b) || shift > 30 {
            return None;
        }
        result |= (b & 0x1f) << shift;
        shift += 5;
        if b < 0x20 {
            break;
        }
    }
    Some(if result & 1 != 0 {
        !(result >> 1)
    } else {
        result >> 1
    })
}

/// Encodes `(lat, lon)` pairs as a polyline6.
pub fn encode(points: &[(f64, f64)]) -> String {
    encode_with(points, PRECISION)
}

fn encode_with(points: &[(f64, f64)], precision: f64) -> String {
    let mut out = String::with_capacity(points.len() * 10);
    let (mut plat, mut plon) = (0i64, 0i64);
    for &(lat, lon) in points {
        let la = (lat * precision).round() as i64;
        let lo = (lon * precision).round() as i64;
        write(&mut out, la - plat);
        write(&mut out, lo - plon);
        plat = la;
        plon = lo;
    }
    out
}

fn write(out: &mut String, delta: i64) {
    let mut v: u64 = if delta < 0 {
        !((delta as u64) << 1)
    } else {
        (delta as u64) << 1
    };
    while v >= 0x20 {
        out.push((((0x20 | (v & 0x1f)) + 63) as u8) as char);
        v >>= 5;
    }
    out.push(((v + 63) as u8) as char);
}

/// Rounds a coordinate to the 1e-6 degree grid of the format, as a decoder would see it.
pub fn round6(v: f64) -> f64 {
    (v * PRECISION).round() / PRECISION
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn round_trip() {
        let pts = vec![
            (47.20572, -1.545701),
            (47.2060422, -1.5457661),
            (-33.9, 151.2),
            (0.0, 0.0),
        ];
        let enc = encode(&pts);
        let dec = decode(&enc).unwrap();
        assert_eq!(dec.len(), pts.len());
        for (a, b) in pts.iter().zip(&dec) {
            assert!((a.0 - b.0).abs() < 1e-6 && (a.1 - b.1).abs() < 1e-6);
        }
    }

    #[test]
    fn known_google_vector() {
        // The reference example of Google's format (precision 1e5); polyline6 is the same
        // algorithm with another factor.
        let pts = [(38.5, -120.2), (40.7, -120.95), (43.252, -126.453)];
        assert_eq!(encode_with(&pts, 1e5), "_p~iF~ps|U_ulLnnqC_mqNvxq`@");
        let dec = decode_with("_p~iF~ps|U_ulLnnqC_mqNvxq`@", 1e5).unwrap();
        assert!((dec[2].0 - 43.252).abs() < 1e-9 && (dec[2].1 + 126.453).abs() < 1e-9);
        // and 1e6 decodes what 1e6 encodes, at 1e-6
        let dec6 = decode(&encode(&pts)).unwrap();
        assert!((dec6[1].1 + 120.95).abs() < 1e-9);
    }

    #[test]
    fn malformed_is_rejected() {
        assert!(decode("_izlhA~rlgdF_").is_none());
        assert!(decode("\u{1}\u{2}").is_none());
        assert_eq!(decode("").unwrap().len(), 0);
    }

    #[test]
    fn forged_huge_deltas_are_rejected_not_wrapped() {
        // 12 continuation chunks then a terminator: a ~2^60 delta.
        let huge = format!("{}?", "~".repeat(12));
        assert!(decode(&format!("{huge}{huge}{huge}{huge}")).is_none());
        // the largest valid deltas still decode
        let pts = [(-90.0, -180.0), (90.0, 180.0), (-90.0, -180.0)];
        let dec = decode(&encode(&pts)).unwrap();
        assert!((dec[1].0 - 90.0).abs() < 1e-9 && (dec[2].1 + 180.0).abs() < 1e-9);
    }
}
