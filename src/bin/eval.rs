//! Evaluation against Valhalla: for each route file (`*.route.json`), the exact request the
//! client would send, our answer, the client's projection of it into surface ranges, and the
//! comparison with the ranges the client computed from real Valhalla answers.
//!
//! ```text
//! eval [--data testdata] [--url http://localhost:8002] [--out eval/report.md] [--costing bicycle]
//! ```
//! Without `--url` the service runs in-process (same configuration from the environment).

use osm_surface::client::{self, CallResult, Range, TrackSurface};
use osm_surface::config::Config;
use osm_surface::mvt::StreetAttrs;
use osm_surface::trace::{Service, TraceOutput, TraceResponse};
use osm_surface::valhalla;
use std::collections::BTreeMap;
use std::fmt::Write as _;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Instant;

struct Route {
    name: String,
    lat: Vec<f64>,
    lon: Vec<f64>,
    dist: Vec<f64>,
    reference: Vec<Range<TrackSurface>>,
}

fn load_route(path: &Path) -> anyhow::Result<Route> {
    let v: serde_json::Value = serde_json::from_str(&std::fs::read_to_string(path)?)?;
    let track = &v["tracks"][0];
    let coords = track["line"]["coordinates"]
        .as_array()
        .ok_or_else(|| anyhow::anyhow!("no coordinates"))?;
    let (mut lat, mut lon, mut dist) = (vec![], vec![], vec![]);
    for c in coords {
        lon.push(c[0].as_f64().unwrap_or(0.0));
        lat.push(c[1].as_f64().unwrap_or(0.0));
        dist.push(c[3].as_f64().unwrap_or(0.0));
    }
    let reference = track["surfaces"]
        .as_array()
        .map(|a| {
            a.iter()
                .map(|s| Range {
                    from: s["from"].as_f64().unwrap_or(0.0),
                    to: s["to"].as_f64().unwrap_or(0.0),
                    value: TrackSurface::of_name(s["surface"].as_str().unwrap_or("")),
                })
                .collect()
        })
        .unwrap_or_default();
    let name = path
        .file_name()
        .and_then(|f| f.to_str())
        .unwrap_or("?")
        .trim_end_matches(".route.json")
        .to_string();
    Ok(Route {
        name,
        lat,
        lon,
        dist,
        reference,
    })
}

/// One answered slice: the answer, the Shortbread attributes of its edges, and timings.
struct Answer {
    edge_osm: Vec<Option<StreetAttrs>>,
    ms: f64,
    tiles: usize,
    segments: usize,
}

enum Backend {
    InProcess(Arc<Service>),
    Http(reqwest::Client, String),
}

impl Backend {
    async fn call(&self, encoded: &str, costing: &str) -> (CallResult, Option<Answer>) {
        let started = Instant::now();
        match self {
            Backend::InProcess(service) => {
                let req = client::request(encoded, costing);
                match service.trace_attributes(&req).await {
                    Ok(TraceOutput {
                        response,
                        edge_osm,
                        stats,
                    }) => {
                        let answer = Answer {
                            edge_osm: edge_osm.into_iter().map(Some).collect(),
                            ms: started.elapsed().as_secs_f64() * 1000.0,
                            tiles: stats.tiles,
                            segments: stats.segments,
                        };
                        (CallResult::Ok(Box::new(response)), Some(answer))
                    }
                    Err(e) if e.status == 400 && client::DEFINITIVE_CODES.contains(&e.code) => {
                        (CallResult::Definitive(e.code), None)
                    }
                    Err(e) => (CallResult::Transient(e.to_string()), None),
                }
            }
            Backend::Http(client, base) => {
                let mut body = client::request_json(encoded, costing);
                body["debug"] = serde_json::Value::Bool(true);
                let resp = client
                    .post(format!(
                        "{}/trace_attributes",
                        base.trim_end_matches('/')
                    ))
                    .header("Authorization", "eval")
                    .json(&body)
                    .send()
                    .await;
                let resp = match resp {
                    Ok(r) => r,
                    Err(e) => return (CallResult::Transient(e.to_string()), None),
                };
                let status = resp.status().as_u16();
                let text = resp.text().await.unwrap_or_default();
                let ms = started.elapsed().as_secs_f64() * 1000.0;
                if status == 200 {
                    match serde_json::from_str::<TraceResponse>(&text) {
                        Ok(r) => {
                            let edge_osm = r.edges.iter().map(|e| e.osm.clone()).collect();
                            let answer = Answer {
                                edge_osm,
                                ms,
                                tiles: 0,
                                segments: 0,
                            };
                            (CallResult::Ok(Box::new(r)), Some(answer))
                        }
                        Err(e) => (CallResult::Transient(format!("bad JSON: {e}")), None),
                    }
                } else {
                    let code = serde_json::from_str::<serde_json::Value>(&text)
                        .ok()
                        .and_then(|v| v["error_code"].as_i64())
                        .unwrap_or(-1) as i32;
                    if status == 400 && client::DEFINITIVE_CODES.contains(&code) {
                        (CallResult::Definitive(code), None)
                    } else {
                        (
                            CallResult::Transient(format!("HTTP {status} code {code}")),
                            None,
                        )
                    }
                }
            }
        }
    }
}

/// What the matched street is at each metre: (slice answer, edge index) ranges along the track.
struct Lookup {
    ranges: Vec<Range<(usize, usize)>>,
}

impl Lookup {
    fn at(&self, m: f64) -> Option<(usize, usize)> {
        let i = self.ranges.partition_point(|r| r.to <= m);
        self.ranges.get(i).filter(|r| r.from <= m).map(|r| r.value)
    }
}

#[derive(Default)]
struct RouteReport {
    name: String,
    length: f64,
    points: usize,
    latency_ms: f64,
    tiles: usize,
    segments: usize,
    ours: BTreeMap<TrackSurface, f64>,
    reference: BTreeMap<TrackSurface, f64>,
    /// confusion[(ref, ours)]
    confusion: BTreeMap<(TrackSurface, TrackSurface), f64>,
    agree: f64,
    /// The agreement on the metres where Valhalla knows the surface.
    agree_known: f64,
    paved_agree: f64,
    unmatched_points: usize,
    worst: Vec<(f64, f64, TrackSurface, TrackSurface, String)>,
    /// Our unknown stretches: from, to, what Valhalla says there, midpoint.
    unknown: Vec<(f64, f64, String, String)>,
}

fn is_paved(s: TrackSurface) -> Option<bool> {
    match s {
        TrackSurface::Paved | TrackSurface::PavedRough => Some(true),
        TrackSurface::Gravel | TrackSurface::Dirt | TrackSurface::Path => Some(false),
        _ => None,
    }
}

fn point_at(r: &Route, m: f64) -> (f64, f64) {
    let i = r
        .dist
        .partition_point(|&d| d <= m)
        .clamp(1, r.dist.len() - 1);
    let (d0, d1) = (r.dist[i - 1], r.dist[i]);
    let f = if d1 > d0 {
        ((m - d0) / (d1 - d0)).clamp(0.0, 1.0)
    } else {
        0.0
    };
    (
        r.lat[i - 1] + (r.lat[i] - r.lat[i - 1]) * f,
        r.lon[i - 1] + (r.lon[i] - r.lon[i - 1]) * f,
    )
}

fn describe_street(a: Option<&StreetAttrs>) -> String {
    match a {
        None => "-".into(),
        Some(a) => {
            let mut s = a.kind.clone();
            for (k, v) in [
                ("surface", &a.surface),
                ("tracktype", &a.tracktype),
                ("service", &a.service),
                ("bicycle", &a.bicycle),
            ] {
                if let Some(v) = v {
                    write!(s, " {k}={v}").ok();
                }
            }
            write!(
                s,
                " → {}",
                valhalla::derive(a, osm_surface::config::AccessRules::France)
                    .surface
                    .as_str()
            )
            .ok();
            s
        }
    }
}

/// Elementary intervals where both cover lists are constant.
fn sweep<A: Copy, B: Copy>(a: &[Range<A>], b: &[Range<B>], mut f: impl FnMut(f64, f64, A, B)) {
    let (mut i, mut j) = (0, 0);
    while i < a.len() && j < b.len() {
        let from = a[i].from.max(b[j].from);
        let to = a[i].to.min(b[j].to);
        if to > from {
            f(from, to, a[i].value, b[j].value);
        }
        if a[i].to <= b[j].to {
            i += 1;
        } else {
            j += 1;
        }
    }
}

type Calibration = BTreeMap<String, BTreeMap<TrackSurface, f64>>;

async fn evaluate(
    route: &Route,
    backend: &Backend,
    costing: &str,
    calibration: &mut Calibration,
) -> anyhow::Result<RouteReport> {
    let mut answers: Vec<Option<Answer>> = Vec::new();
    let handle = tokio::runtime::Handle::current();
    let started = Instant::now();
    let (cover, slices) =
        client::compute(&route.lat, &route.lon, &route.dist, costing, |enc, c| {
            let (result, answer) =
                tokio::task::block_in_place(|| handle.block_on(backend.call(enc, c)));
            answers.push(answer);
            result
        })
        .map_err(|e| anyhow::anyhow!("{}: {e}", route.name))?;
    let latency_ms = answers
        .iter()
        .flatten()
        .map(|a| a.ms)
        .sum::<f64>()
        .max(started.elapsed().as_secs_f64() * 1000.0 * 0.0);

    // Street lookup along the track.
    let mut lookup = Lookup { ranges: vec![] };
    let mut unmatched_points = 0;
    for (k, s) in slices.iter().enumerate() {
        let Some(resp) = &s.response else { continue };
        unmatched_points += resp
            .matched_points
            .iter()
            .filter(|m| m.type_.as_deref() == Some("unmatched"))
            .count();
        let d: Vec<f64> = (s.i0..=s.i1)
            .map(|i| route.dist[i] - route.dist[s.i0])
            .collect();
        for r in client::project(&d, resp, |e, _| Some(e)) {
            let (a, b) = (
                s.start.max(r.from + route.dist[s.i0]),
                s.end.min(r.to + route.dist[s.i0]),
            );
            if let (Some(e), true) = (r.value, b > a) {
                lookup.ranges.push(Range {
                    from: a,
                    to: b,
                    value: (k, e),
                });
            }
        }
    }
    lookup.ranges.sort_by(|a, b| a.from.total_cmp(&b.from));
    let street_at = |m: f64| -> Option<StreetAttrs> {
        let (k, e) = lookup.at(m)?;
        answers.get(k)?.as_ref()?.edge_osm.get(e)?.clone()
    };

    let ours: Vec<Range<TrackSurface>> = cover
        .iter()
        .map(|r| Range {
            from: r.from,
            to: r.to,
            value: TrackSurface::of_code(Some(&r.value)),
        })
        .collect();
    let mut rep = RouteReport {
        name: route.name.clone(),
        length: *route.dist.last().unwrap_or(&0.0),
        points: route.dist.len(),
        latency_ms,
        unmatched_points,
        ..Default::default()
    };
    for a in answers.iter().flatten() {
        rep.tiles += a.tiles;
        rep.segments += a.segments;
    }
    for r in &ours {
        *rep.ours.entry(r.value).or_default() += r.len();
    }
    for r in &route.reference {
        *rep.reference.entry(r.value).or_default() += r.len();
    }
    let mut total = 0.0;
    let mut known_total = 0.0;
    let mut paved_total = 0.0;
    let mut stretches: Vec<(f64, f64, TrackSurface, TrackSurface)> = vec![];
    sweep(&route.reference, &ours, |from, to, rf, us| {
        let l = to - from;
        total += l;
        *rep.confusion.entry((rf, us)).or_default() += l;
        if rf != TrackSurface::Unknown {
            known_total += l;
            if rf == us {
                rep.agree_known += l;
            }
        }
        if rf == us {
            rep.agree += l;
        } else {
            match stretches.last_mut() {
                Some(last) if last.1 == from && last.2 == us && last.3 == rf => last.1 = to,
                _ => stretches.push((from, to, us, rf)),
            }
        }
        if let (Some(a), Some(b)) = (is_paved(rf), is_paved(us)) {
            paved_total += l;
            if a == b {
                rep.paved_agree += l;
            }
        }
    });
    rep.agree /= total.max(1e-9);
    rep.agree_known /= known_total.max(1e-9);
    rep.paved_agree /= paved_total.max(1e-9);
    if std::env::var("EVAL_VERBOSE").is_ok() {
        for &(from, to, us, rf) in stretches.iter().filter(|s| s.1 - s.0 >= 40.0) {
            let mid = (from + to) / 2.0;
            let (lat, lon) = point_at(route, mid);
            println!(
                "  {:>7.0}-{:<7.0} {:5.0} m  ours {:11} ref {:11} {lat:.6},{lon:.6} {}",
                from,
                to,
                to - from,
                us.name(),
                rf.name(),
                describe_street(street_at(mid).as_ref())
            );
        }
    }
    stretches.sort_by(|a, b| (b.1 - b.0).total_cmp(&(a.1 - a.0)));
    for (from, to, us, rf) in stretches.into_iter().take(10) {
        let mid = (from + to) / 2.0;
        let (lat, lon) = point_at(route, mid);
        let street = describe_street(street_at(mid).as_ref());
        rep.worst
            .push((from, to, us, rf, format!("{lat:.6}, {lon:.6} | {street}")));
    }

    for r in ours
        .iter()
        .filter(|r| r.value == TrackSurface::Unknown && r.len() >= 30.0)
    {
        let mut refs: BTreeMap<TrackSurface, f64> = BTreeMap::new();
        sweep(
            &route.reference,
            std::slice::from_ref(r),
            |from, to, rf, _| *refs.entry(rf).or_default() += to - from,
        );
        let refs = refs
            .iter()
            .map(|(k, v)| format!("{} {:.0}", k.name(), v))
            .collect::<Vec<_>>()
            .join(", ");
        let (lat, lon) = point_at(route, (r.from + r.to) / 2.0);
        rep.unknown
            .push((r.from, r.to, refs, format!("{lat:.6}, {lon:.6}")));
    }

    // Calibration: per Shortbread attribute set, what the reference says, every 5 m.
    let ref_at = |m: f64| -> TrackSurface {
        let i = route.reference.partition_point(|r| r.to <= m);
        route
            .reference
            .get(i)
            .map_or(TrackSurface::Unknown, |r| r.value)
    };
    let mut m = 2.5;
    while m < rep.length {
        let key = match street_at(m) {
            Some(a) => format!(
                "{} | {} | {} | {}",
                a.kind,
                a.surface.as_deref().unwrap_or("-"),
                a.tracktype.as_deref().unwrap_or("-"),
                a.bicycle.as_deref().unwrap_or("-")
            ),
            None => "(unmatched)".into(),
        };
        *calibration
            .entry(key)
            .or_default()
            .entry(ref_at(m))
            .or_default() += 5.0;
        m += 5.0;
    }
    Ok(rep)
}

/// What the disagreements left come from (stretches of 40 m or more, `EVAL_VERBOSE=1`; OSM tags
/// checked on openstreetmap.org; measured on 2026-11-02 with the tiles of OSM 2026-10-19).
const RESIDUAL_GAPS: &str = "
### Where the disagreements come from

Since VersaTiles keeps the raw `surface` value (planetiler-shortbread 218534d, 2026-08-23), the
tiles give 96 to 99 % agreement per class, 98 to 99 % on paved / unpaved (86 to 94 % when the
surface was reduced to `paved` / `unpaved`). The stretches of 40 m or more left (about 4.3 km over
the three routes):

- **27 %: Valhalla UNKNOWN** where we give a surface (the \"excluding Valhalla unknown\" figure
  removes these). np-655 44180-44690 is a `surface=compacted` path with an explicit `bicycle=no`,
  which Valhalla refuses: Shortbread 1.1 folds `no`, `private` and `dismount` into `no`, which is
  therefore not read as a ban (see README).
- **22 %: `mtb:scale`** (Valhalla: 0 → dirt, 1 → gravel, ≥ 2 → path, even on asphalt;
  graymond 18370-19320: a `surface=asphalt` + `mtb:scale=0` road that Valhalla gives as DIRT),
  missing from the tiles like `sac_scale` and `smoothness`.
- **16 %: `highway=bridleway`**, missing from the tiles (graymond 13850-14540: 690 m of
  `surface=compacted` bridleway that Valhalla sees as GRAVEL, unknown for us).
- **35 %: the rest**, short stretches: the matching (parallel ways: driveway versus track,
  footway versus roadway), ways without a surface where the defaults of the kind differ from what
  Valhalla returns, and OSM edits between Valhalla's data and the tiles'.
";

fn pct(a: f64, total: f64) -> String {
    format!("{:.1} %", 100.0 * a / total.max(1e-9))
}

fn summary_table(s: &mut String, reports: &[RouteReport]) {
    writeln!(s, "| Route | Length | Points | Agreement (class) | Agreement excluding Valhalla unknown | Paved/unpaved agreement | Unknown (ours) | Unmatched points | Latency |").ok();
    writeln!(s, "|---|---:|---:|---:|---:|---:|---:|---:|---:|").ok();
    for r in reports {
        let unknown = r.ours.get(&TrackSurface::Unknown).copied().unwrap_or(0.0);
        writeln!(
            s,
            "| {} | {:.1} km | {} | **{:.1} %** | {:.1} % | {:.1} % | {} | {} | {:.0} ms |",
            r.name,
            r.length / 1000.0,
            r.points,
            r.agree * 100.0,
            r.agree_known * 100.0,
            r.paved_agree * 100.0,
            pct(unknown, r.length),
            r.unmatched_points,
            r.latency_ms
        )
        .ok();
    }
}

fn report_markdown(
    reports: &[RouteReport],
    calibration: &Calibration,
    mode: &str,
    sources: &str,
) -> String {
    let mut s = String::new();
    writeln!(s, "# osm-surface vs Valhalla evaluation\n").ok();
    writeln!(
        s,
        "Generated by `cargo run --release --bin eval` ({mode}). Reference: surface ranges computed by the client \
         from real Valhalla answers; our ranges go through the same port of \
         `TraceSurfaceProjection` + `SurfaceRanges` (MIN_SPAN_M = 20) + `TrackSurface`.\n"
    )
    .ok();
    writeln!(s, "Sources: {sources}\n").ok();
    writeln!(s, "## Summary\n").ok();
    summary_table(&mut s, reports);
    writeln!(s, "{}", RESIDUAL_GAPS).ok();
    for r in reports {
        writeln!(s, "\n## {}\n", r.name).ok();
        // Over HTTP the tile and segment counts are not known (0): not printed
        let counts = if r.tiles == 0 && r.segments == 0 {
            String::new()
        } else {
            format!(
                "{} tiles, {} street segments kept, ",
                r.tiles, r.segments
            )
        };
        writeln!(
            s,
            "{:.1} km, {} points, {counts}latency {:.0} ms.\n",
            r.length / 1000.0,
            r.points,
            r.latency_ms
        )
        .ok();
        writeln!(
            s,
            "| Class | Ours (m) | Ours (%) | Valhalla (m) | Valhalla (%) |"
        )
        .ok();
        writeln!(s, "|---|---:|---:|---:|---:|").ok();
        for c in TrackSurface::ALL {
            let (o, f) = (
                r.ours.get(&c).copied().unwrap_or(0.0),
                r.reference.get(&c).copied().unwrap_or(0.0),
            );
            if o > 0.0 || f > 0.0 {
                writeln!(
                    s,
                    "| {} | {:.0} | {} | {:.0} | {} |",
                    c.name(),
                    o,
                    pct(o, r.length),
                    f,
                    pct(f, r.length)
                )
                .ok();
            }
        }
        writeln!(
            s,
            "\nConfusion matrix (metres; rows: Valhalla, columns: ours)\n"
        )
        .ok();
        write!(s, "| Valhalla \\ ours |").ok();
        for c in TrackSurface::ALL {
            write!(s, " {} |", c.name()).ok();
        }
        writeln!(s).ok();
        writeln!(s, "|---|{}", "---:|".repeat(TrackSurface::ALL.len())).ok();
        for rf in TrackSurface::ALL {
            write!(s, "| {} |", rf.name()).ok();
            for us in TrackSurface::ALL {
                let v = r.confusion.get(&(rf, us)).copied().unwrap_or(0.0);
                if rf == us {
                    write!(s, " **{v:.0}** |").ok();
                } else {
                    write!(s, " {v:.0} |").ok();
                }
            }
            writeln!(s).ok();
        }
        writeln!(s, "\nThe 10 largest disagreements\n").ok();
        writeln!(s, "| From (m) | To (m) | Length | Ours | Valhalla | Midpoint (lat, lon) \\| matched street (tags → Valhalla surface) |").ok();
        writeln!(s, "|---:|---:|---:|---|---|---|").ok();
        for (from, to, us, rf, info) in &r.worst {
            writeln!(
                s,
                "| {:.0} | {:.0} | {:.0} m | {} | {} | {} |",
                from,
                to,
                to - from,
                us.name(),
                rf.name(),
                info
            )
            .ok();
        }
    }
    writeln!(s, "\n## Our unknown ranges (≥ 30 m)\n").ok();
    writeln!(
        s,
        "| Route | From (m) | To (m) | Length | Valhalla sees | Midpoint (lat, lon) |"
    )
    .ok();
    writeln!(s, "|---|---:|---:|---:|---|---|").ok();
    for r in reports {
        for (from, to, refs, mid) in &r.unknown {
            writeln!(
                s,
                "| {} | {:.0} | {:.0} | {:.0} m | {} | {} |",
                r.name,
                from,
                to,
                to - from,
                refs,
                mid
            )
            .ok();
        }
    }
    writeln!(
        s,
        "\n## Calibration: what Valhalla says depending on the matched street's attributes\n"
    )
    .ok();
    writeln!(s, "All routes combined, sampled every 5 m. `kind | surface | tracktype | bicycle`.\n").ok();
    write!(s, "| Attributes | Total (m) |").ok();
    for c in TrackSurface::ALL {
        write!(s, " {} |", c.name()).ok();
    }
    writeln!(s).ok();
    writeln!(s, "|---|---:|{}", "---:|".repeat(TrackSurface::ALL.len())).ok();
    let mut rows: Vec<(&String, &BTreeMap<TrackSurface, f64>)> = calibration.iter().collect();
    rows.sort_by(|a, b| {
        b.1.values()
            .sum::<f64>()
            .total_cmp(&a.1.values().sum::<f64>())
    });
    for (k, v) in rows {
        let total: f64 = v.values().sum();
        if total < 100.0 {
            continue;
        }
        write!(s, "| {k} | {total:.0} |").ok();
        for c in TrackSurface::ALL {
            let x = v.get(&c).copied().unwrap_or(0.0);
            write!(
                s,
                " {} |",
                if x > 0.0 {
                    format!("{:.0} %", 100.0 * x / total)
                } else {
                    String::new()
                }
            )
            .ok();
        }
        writeln!(s).ok();
    }
    s
}

#[tokio::main(flavor = "multi_thread")]
async fn main() -> anyhow::Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env().unwrap_or_else(|_| "warn".into()),
        )
        .init();
    let mut data = PathBuf::from("testdata");
    let mut url: Option<String> = None;
    let mut out = PathBuf::from("eval/report.md");
    let mut costing = "bicycle".to_string();
    let mut args = std::env::args().skip(1);
    while let Some(a) = args.next() {
        match a.as_str() {
            "--data" => data = PathBuf::from(args.next().unwrap_or_default()),
            "--url" => url = args.next(),
            "--out" => out = PathBuf::from(args.next().unwrap_or_default()),
            "--costing" => costing = args.next().unwrap_or_else(|| "bicycle".into()),
            _ => anyhow::bail!(
                "usage: eval [--data dir] [--url base] [--out report.md] [--costing bicycle]"
            ),
        }
    }
    let (backend, mode, sources) = match &url {
        Some(u) => {
            let client = reqwest::Client::new();
            let sources = http_sources(&client, u)
                .await
                .unwrap_or_else(|| "the queried service (unreadable `/health`)".to_string());
            (
                Backend::Http(client, u.clone()),
                format!("HTTP {u}"),
                sources,
            )
        }
        None => {
            let service = Arc::new(Service::new(Config::from_env())?);
            service.fetch_tiles_info().await;
            let sources = tiles_sources(service.tiles_info.read().ok().and_then(|t| t.clone()));
            (
                Backend::InProcess(service),
                "in process".to_string(),
                sources,
            )
        }
    };
    let mut files: Vec<PathBuf> = std::fs::read_dir(&data)?
        .filter_map(|e| e.ok().map(|e| e.path()))
        .filter(|p| p.to_string_lossy().ends_with(".route.json"))
        .collect();
    files.sort();
    let mut reports = Vec::new();
    let mut calibration = Calibration::new();
    for f in &files {
        let route = load_route(f)?;
        let rep = evaluate(&route, &backend, &costing, &mut calibration).await?;
        println!(
            "{:24} {:6.1} km  agreement {:5.1} % (known {:5.1} %)  paved/unpaved {:5.1} %  unmatched pts {:3}  {:6.0} ms",
            rep.name,
            rep.length / 1000.0,
            rep.agree * 100.0,
            rep.agree_known * 100.0,
            rep.paved_agree * 100.0,
            rep.unmatched_points,
            rep.latency_ms
        );
        let line = |m: &BTreeMap<TrackSurface, f64>| {
            TrackSurface::ALL
                .iter()
                .filter_map(|c| m.get(c).map(|v| format!("{} {:.0}", c.name(), v)))
                .collect::<Vec<_>>()
                .join(" / ")
        };
        println!(
            "    ours: {}\n    ref:  {}",
            line(&rep.ours),
            line(&rep.reference)
        );
        reports.push(rep);
    }
    let md = report_markdown(&reports, &calibration, &mode, &sources);
    if let Some(dir) = out.parent() {
        std::fs::create_dir_all(dir)?;
    }
    std::fs::write(&out, md)?;
    println!("report written to {}", out.display());
    Ok(())
}

/// The tile source as the report states it, from the TileJSON dates (`Service::sources`).
fn tiles_sources(tiles: Option<serde_json::Value>) -> String {
    match tiles
        .as_ref()
        .and_then(|t| t["osm_replication_time"].as_str())
    {
        Some(d) => format!("VersaTiles tiles (OSM from {d})"),
        None => "VersaTiles tiles".to_string(),
    }
}

/// The data sources of a running service, from its `/health` (same wording as in-process).
async fn http_sources(client: &reqwest::Client, base: &str) -> Option<String> {
    let url = format!("{}/health", base.trim_end_matches('/'));
    let v: serde_json::Value = client.get(url).send().await.ok()?.json().await.ok()?;
    Some(tiles_sources(Some(v["sources"]["tiles"].clone())))
}
