//! Developer tool: prefetch the tiles of route files into the disk cache, print
//! statistics of the `streets` layer, or dump the streets near a point.
//!
//! ```text
//! inspect prefetch testdata/*.route.json
//! inspect stats testdata/*.route.json
//! inspect near <lat> <lon> [radius_m]
//! ```

use osm_surface::config::Config;
use osm_surface::geo::{self, corridor_tiles};
use osm_surface::tiles::TileStore;
use std::collections::BTreeMap;

fn route_points(path: &str) -> anyhow::Result<Vec<(f64, f64)>> {
    let v: serde_json::Value = serde_json::from_str(&std::fs::read_to_string(path)?)?;
    let coords = v["tracks"][0]["line"]["coordinates"]
        .as_array()
        .ok_or_else(|| anyhow::anyhow!("no coordinates"))?;
    Ok(coords
        .iter()
        .map(|c| (c[1].as_f64().unwrap_or(0.0), c[0].as_f64().unwrap_or(0.0)))
        .collect())
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(tracing_subscriber::EnvFilter::from_default_env())
        .init();
    let args: Vec<String> = std::env::args().skip(1).collect();
    let config = Config::from_env();
    let store = TileStore::new(&config)?;
    let grid = store.grid();
    match args.first().map(String::as_str) {
        Some("prefetch") | Some("stats") => {
            let mut all = std::collections::BTreeSet::new();
            for f in &args[1..] {
                let pts = route_points(f)?;
                let tiles = corridor_tiles(grid, &pts, 120.0);
                println!("{f}: {} points, {} tiles", pts.len(), tiles.len());
                all.extend(tiles);
            }
            let keys: Vec<_> = all.into_iter().collect();
            let t = std::time::Instant::now();
            let tiles = store.get_many(&keys).await?;
            println!("{} tiles loaded in {:?}", tiles.len(), t.elapsed());
            if args[0] == "stats" {
                let mut kinds: BTreeMap<String, usize> = BTreeMap::new();
                let mut combos: BTreeMap<(String, String, String), usize> = BTreeMap::new();
                for t in &tiles {
                    for l in &t.lines {
                        let a = &t.attrs[l.attr as usize];
                        *kinds
                            .entry(format!("{} rail={} link={}", a.kind, a.rail, a.link))
                            .or_default() += 1;
                        *combos
                            .entry((
                                a.kind.clone(),
                                a.surface.clone().unwrap_or("-".into()),
                                a.tracktype.clone().unwrap_or("-".into()),
                            ))
                            .or_default() += 1;
                    }
                }
                for (k, n) in kinds {
                    println!("{n:6} {k}");
                }
                for (k, n) in combos {
                    if n > 20 {
                        println!("{n:6} {k:?}");
                    }
                }
                let mut bikes: BTreeMap<String, usize> = BTreeMap::new();
                for t in &tiles {
                    for a in &t.attrs {
                        *bikes
                            .entry(format!(
                                "bicycle={:?} service={:?} horse={:?}",
                                a.bicycle, a.service, a.horse
                            ))
                            .or_default() += 1;
                    }
                }
                for (k, n) in bikes {
                    println!("{n:6} {k}");
                }
            }
        }
        Some("near") => {
            let lat: f64 = args[1].parse()?;
            let lon: f64 = args[2].parse()?;
            let radius: f64 = args.get(3).and_then(|r| r.parse().ok()).unwrap_or(30.0);
            let p = grid.to_units(lon, lat);
            let mpu = grid.metres_per_unit(lat);
            let keys = corridor_tiles(grid, &[(lat, lon)], radius);
            for t in store.get_many(&keys).await? {
                for l in &t.lines {
                    let d = l
                        .points
                        .windows(2)
                        .map(|w| geo::dist_units(p, geo::project_on_segment(p, w[0], w[1]).1) * mpu)
                        .fold(f64::INFINITY, f64::min);
                    if d <= radius {
                        println!(
                            "{d:6.1} m  tile {}/{}  {:?}",
                            t.x, t.y, t.attrs[l.attr as usize]
                        );
                    }
                }
            }
        }
        Some("trace") => {
            // inspect trace <route.json> <from_m> <to_m>: the matching around a stretch
            use osm_surface::matcher::{self, Params, Step};
            use osm_surface::network::{Corridor, Network};
            use osm_surface::valhalla::Mode;
            let v: serde_json::Value = serde_json::from_str(&std::fs::read_to_string(&args[1])?)?;
            let coords = v["tracks"][0]["line"]["coordinates"]
                .as_array()
                .unwrap()
                .clone();
            let from: f64 = args[2].parse()?;
            let to: f64 = args[3].parse()?;
            let pts: Vec<(f64, f64)> = coords
                .iter()
                .map(|c| (c[1].as_f64().unwrap(), c[0].as_f64().unwrap()))
                .collect();
            let dist: Vec<f64> = coords.iter().map(|c| c[3].as_f64().unwrap()).collect();
            let params = Params::from_env();
            let keys = corridor_tiles(grid, &pts, params.search_radius_m + 60.0);
            let tiles = store.get_many(&keys).await?;
            let with_lat: Vec<([f64; 2], f64)> = pts
                .iter()
                .map(|&(la, lo)| (grid.to_units(lo, la), la))
                .collect();
            let corridor = Corridor::new(grid, &with_lat, params.search_radius_m + 60.0);
            let net = Network::build(grid, &tiles, &corridor, Mode::Bicycle, config.access_rules);
            let m = matcher::match_trace(&net, &pts, &params);
            let mut input = 0usize;
            for t in 0..m.obs.len() {
                if let Some(i) = m.obs[t].input {
                    input = i;
                }
                let d = dist[input];
                if d < from || d > to {
                    continue;
                }
                let o = &m.obs[t];
                let c = m.chosen[t];
                let step = match &m.steps[t] {
                    None => "-".to_string(),
                    Some(Step::Jump) => "JUMP".to_string(),
                    Some(Step::Route(p)) => format!(
                        "{} pieces {:?}",
                        p.len(),
                        p.iter().map(|x| x.seg).collect::<Vec<_>>()
                    ),
                };
                if std::env::var("CANDS").is_ok() {
                    for k in matcher::candidates(&net, o, &params) {
                        let s = net.segs[k.seg as usize];
                        println!(
                            "         cand seg {} ({}-{}) t={:.2} dist={:.1} {} {:?}",
                            k.seg,
                            s.a,
                            s.b,
                            k.t,
                            k.dist_m,
                            net.class_of(k.seg).attrs.kind,
                            net.class_of(k.seg).attrs.surface
                        );
                    }
                }
                match c {
                    None => println!(
                        "{t:6} in={:?} d~{d:.0} {:.6},{:.6} UNMATCHED",
                        o.input, o.lat, o.lon
                    ),
                    Some(c) => {
                        let cl = net.class_of(c.seg);
                        println!(
                            "{t:6} in={:?} d~{d:.0} seg {} t={:.2} dist={:.1} {} {:?} {:?} -> {}",
                            o.input,
                            c.seg,
                            c.t,
                            c.dist_m,
                            cl.attrs.kind,
                            cl.attrs.surface,
                            cl.info.surface,
                            step
                        );
                    }
                }
            }
        }
        Some("fixture") => {
            // inspect fixture <route.json> <from_m> <to_m> <out.json> <tiles_dir>: a stretch of a
            // route (points, reference ranges from its start) and the tiles it needs, for tests.
            let v: serde_json::Value = serde_json::from_str(&std::fs::read_to_string(&args[1])?)?;
            let from: f64 = args[2].parse()?;
            let to: f64 = args[3].parse()?;
            let coords = v["tracks"][0]["line"]["coordinates"].as_array().unwrap();
            let pts: Vec<[f64; 3]> = coords
                .iter()
                .map(|c| {
                    [
                        c[1].as_f64().unwrap(),
                        c[0].as_f64().unwrap(),
                        c[3].as_f64().unwrap(),
                    ]
                })
                .filter(|p| p[2] >= from && p[2] <= to)
                .collect();
            let d0 = pts[0][2];
            let d1 = pts[pts.len() - 1][2];
            let points: Vec<serde_json::Value> = pts
                .iter()
                .map(|p| serde_json::json!([p[0], p[1], p[2] - d0]))
                .collect();
            let surfaces: Vec<serde_json::Value> = v["tracks"][0]["surfaces"]
                .as_array()
                .unwrap()
                .iter()
                .filter_map(|r| {
                    let a = r["from"].as_f64()?.max(d0);
                    let b = r["to"].as_f64()?.min(d1);
                    (b > a).then(|| serde_json::json!({"from": a - d0, "to": b - d0, "surface": r["surface"]}))
                })
                .collect();
            std::fs::write(
                &args[4],
                serde_json::to_string(
                    &serde_json::json!({"points": points, "surfaces": surfaces}),
                )?,
            )?;
            let latlon: Vec<(f64, f64)> = pts.iter().map(|p| (p[0], p[1])).collect();
            let keys = corridor_tiles(
                grid,
                &latlon,
                osm_surface::matcher::Params::default().search_radius_m + 60.0,
            );
            let src = config.cache_dir.clone().expect("a cache dir");
            for (x, y) in keys {
                store.get((x, y)).await?;
                let rel = format!("{}/{x}/{y}.mvt", grid.zoom);
                let dst = std::path::Path::new(&args[5]).join(&rel);
                std::fs::create_dir_all(dst.parent().unwrap())?;
                std::fs::copy(src.join(&rel), &dst)?;
                println!("{rel}");
            }
        }
        _ => {
            eprintln!("usage: inspect prefetch|stats <route.json>... | near <lat> <lon> [radius_m]")
        }
    }
    Ok(())
}
