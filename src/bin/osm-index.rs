//! Builds the OSM index (`OSM_INDEX`) from an OSM extract: the highway ways of the street
//! network with the tags Valhalla reads.
//!
//! ```text
//! osm-index <extract.osm.pbf> <out.bin> [--bbox min_lon,min_lat,max_lon,max_lat]
//! ```
//! The file is written next to `out.bin` then renamed over it, so a running service never
//! reads a partial file (it reloads the index when the file changes).

use osm_surface::osm::{attrs_from_tags, IndexWriter};
use osmpbf::{BlobDecode, BlobReader, Element, IndexedReader};
use std::io::BufWriter;
use std::path::PathBuf;
use std::time::Instant;

fn main() -> anyhow::Result<()> {
    let mut args = std::env::args().skip(1);
    let mut input: Option<PathBuf> = None;
    let mut output: Option<PathBuf> = None;
    let mut bbox: Option<[f64; 4]> = None;
    while let Some(a) = args.next() {
        match a.as_str() {
            "--bbox" => {
                let v: Vec<f64> = args
                    .next()
                    .unwrap_or_default()
                    .split(',')
                    .filter_map(|x| x.trim().parse().ok())
                    .collect();
                anyhow::ensure!(v.len() == 4, "--bbox min_lon,min_lat,max_lon,max_lat");
                bbox = Some([v[0], v[1], v[2], v[3]]);
            }
            _ if input.is_none() => input = Some(PathBuf::from(a)),
            _ if output.is_none() => output = Some(PathBuf::from(a)),
            _ => anyhow::bail!("usage: osm-index <extract.osm.pbf> <out.bin> [--bbox ...]"),
        }
    }
    let (Some(input), Some(output)) = (input, output) else {
        anyhow::bail!(
            "usage: osm-index <extract.osm.pbf> <out.bin> [--bbox min_lon,min_lat,max_lon,max_lat]"
        );
    };
    let started = Instant::now();

    // The header: the replication timestamp of the extract, if any.
    let mut replication_time: Option<String> = None;
    if let Some(Ok(blob)) = BlobReader::from_path(&input)?.next() {
        if let Ok(BlobDecode::OsmHeader(h)) = blob.decode() {
            replication_time = h.osmosis_replication_timestamp().and_then(iso8601);
        }
    }

    let mut ways: Vec<(i64, osm_surface::mvt::StreetAttrs, Vec<i64>)> = Vec::new();
    let mut nodes: Vec<(i64, i32, i32)> = Vec::new();
    let mut reader = IndexedReader::from_path(&input)?;
    reader.read_ways_and_deps(
        |way| {
            let tags: Vec<(&str, &str)> = way.tags().collect();
            attrs_from_tags(|k| tags.iter().find(|(a, _)| *a == k).map(|(_, v)| *v)).is_some()
        },
        |element| match element {
            Element::Way(way) => {
                let tags: Vec<(&str, &str)> = way.tags().collect();
                if let Some(a) =
                    attrs_from_tags(|k| tags.iter().find(|(x, _)| *x == k).map(|(_, v)| *v))
                {
                    ways.push((way.id(), a, way.refs().collect()));
                }
            }
            Element::Node(n) => nodes.push((n.id(), n.decimicro_lat(), n.decimicro_lon())),
            Element::DenseNode(n) => nodes.push((n.id(), n.decimicro_lat(), n.decimicro_lon())),
            Element::Relation(_) => {}
        },
    )?;
    nodes.sort_unstable_by_key(|n| n.0);
    eprintln!(
        "read {} ways, {} nodes in {:.1} s",
        ways.len(),
        nodes.len(),
        started.elapsed().as_secs_f64()
    );

    let inside = |&(lat, lon): &(i32, i32)| match bbox {
        None => true,
        Some([x0, y0, x1, y1]) => {
            let (lat, lon) = (lat as f64 * 1e-7, lon as f64 * 1e-7);
            lon >= x0 && lon <= x1 && lat >= y0 && lat <= y1
        }
    };
    let mut writer = IndexWriter::default();
    let mut missing = 0usize;
    for (id, attrs, refs) in ways {
        let pts: Vec<(i32, i32)> = refs
            .iter()
            .filter_map(|r| {
                let i = nodes.binary_search_by_key(r, |n| n.0).ok();
                if i.is_none() {
                    missing += 1;
                }
                i.map(|i| (nodes[i].1, nodes[i].2))
            })
            .collect();
        if pts.iter().any(inside) {
            writer.add_way(id, attrs, pts);
        }
    }
    let source = input
        .file_name()
        .map(|f| f.to_string_lossy().to_string())
        .unwrap_or_default();
    let tmp = output.with_extension("bin.tmp");
    {
        let mut out = BufWriter::new(std::fs::File::create(&tmp)?);
        let count = writer.way_count();
        writer.write(&mut out, &source, replication_time.as_deref())?;
        std::io::Write::flush(&mut out)?;
        eprintln!(
            "{count} ways written ({missing} node references missing), OSM data of {}",
            replication_time.as_deref().unwrap_or("unknown date")
        );
    }
    std::fs::rename(&tmp, &output)?;
    eprintln!(
        "{} in {:.1} s",
        output.display(),
        started.elapsed().as_secs_f64()
    );
    Ok(())
}

/// Seconds since the epoch as `YYYY-MM-DDTHH:MM:SSZ`.
fn iso8601(secs: i64) -> Option<String> {
    if secs <= 0 {
        return None;
    }
    let days = secs.div_euclid(86_400);
    let rem = secs.rem_euclid(86_400);
    // Civil from days (Howard Hinnant's algorithm).
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z.rem_euclid(146_097);
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    let y = yoe + era * 400 + if m <= 2 { 1 } else { 0 };
    Some(format!(
        "{y:04}-{m:02}-{d:02}T{:02}:{:02}:{:02}Z",
        rem / 3600,
        rem % 3600 / 60,
        rem % 60
    ))
}
