//! End-to-end tests through the axum router, offline: the tiles come from
//! `tests/fixtures/tiles` (real VersaTiles z14 tiles: the np655 stretch and two sea tiles of
//! the Bay of Biscay). © OpenStreetMap contributors, ODbL.

use axum::body::Body;
use axum::http::{Request, StatusCode};
use http_body_util::BodyExt;
use osm_surface::api;
use osm_surface::client::{self, CallResult, Range, TrackSurface};
use osm_surface::config::Config;
use osm_surface::polyline;
use osm_surface::trace::{Service, TraceResponse};
use std::io::Read;
use std::path::PathBuf;
use std::sync::Arc;
use tower::ServiceExt;

fn fixtures() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures")
}

fn offline_config() -> Config {
    Config {
        cache_dir: Some(fixtures().join("tiles")),
        offline: true,
        ..Config::default()
    }
}

fn offline_router() -> axum::Router {
    api::router(Arc::new(Service::new(offline_config()).unwrap()))
}

async fn post(
    router: axum::Router,
    body: serde_json::Value,
    gzip: bool,
) -> (StatusCode, serde_json::Value) {
    let mut req = Request::post("/trace_attributes")
        .header("content-type", "application/json")
        .header("authorization", "any-key");
    if gzip {
        req = req.header("accept-encoding", "gzip");
    }
    let resp = router
        .oneshot(req.body(Body::from(body.to_string())).unwrap())
        .await
        .unwrap();
    let status = resp.status();
    let gzipped = resp
        .headers()
        .get("content-encoding")
        .is_some_and(|v| v == "gzip");
    let bytes = resp.into_body().collect().await.unwrap().to_bytes();
    let bytes = if gzipped {
        let mut out = Vec::new();
        flate2::read::GzDecoder::new(&bytes[..])
            .read_to_end(&mut out)
            .unwrap();
        out
    } else {
        assert!(
            !gzip || bytes.len() < 1000,
            "a large answer should be gzipped when asked"
        );
        bytes.to_vec()
    };
    (status, serde_json::from_slice(&bytes).unwrap())
}

struct Stretch {
    lat: Vec<f64>,
    lon: Vec<f64>,
    dist: Vec<f64>,
    reference: Vec<Range<TrackSurface>>,
}

/// 6 km of the np-655 test route (26-32 km), with the ranges the client computed from Valhalla.
fn stretch() -> Stretch {
    let v: serde_json::Value = serde_json::from_str(
        &std::fs::read_to_string(fixtures().join("np655-26-32km.json")).unwrap(),
    )
    .unwrap();
    let pts = v["points"].as_array().unwrap();
    let col = |k: usize| {
        pts.iter()
            .map(|p| p[k].as_f64().unwrap())
            .collect::<Vec<_>>()
    };
    let reference = v["surfaces"]
        .as_array()
        .unwrap()
        .iter()
        .map(|r| Range {
            from: r["from"].as_f64().unwrap(),
            to: r["to"].as_f64().unwrap(),
            value: TrackSurface::of_name(r["surface"].as_str().unwrap()),
        })
        .collect();
    Stretch {
        lat: col(0),
        lon: col(1),
        dist: col(2),
        reference,
    }
}

fn encoded(s: &Stretch) -> String {
    let pts: Vec<(f64, f64)> = s.lat.iter().zip(&s.lon).map(|(a, b)| (*a, *b)).collect();
    polyline::encode(&pts)
}

#[tokio::test]
async fn health() {
    let resp = offline_router()
        .oneshot(Request::get("/health").body(Body::empty()).unwrap())
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
}

#[tokio::test]
async fn a_client_request_projects_like_valhalla() {
    let s = stretch();
    let n = s.lat.len();
    let (status, body) = post(
        offline_router(),
        client::request_json(&encoded(&s), "bicycle"),
        true,
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body}");

    // The contract TraceSurfaceProjection relies on.
    let r: TraceResponse = serde_json::from_value(body.clone()).unwrap();
    assert_eq!(r.matched_points.len(), n);
    let shape = polyline::decode(r.shape.as_deref().unwrap()).unwrap();
    let mut last_end = 0;
    for e in &r.edges {
        let (b, en) = (e.begin_shape_index.unwrap(), e.end_shape_index.unwrap());
        assert!(b < en && en < shape.len() && b >= last_end);
        last_end = en;
        assert!(matches!(
            e.surface.as_deref(),
            Some(
                "paved_smooth"
                    | "paved"
                    | "paved_rough"
                    | "compacted"
                    | "dirt"
                    | "gravel"
                    | "path"
                    | "impassable"
            )
        ));
        assert!(e.use_.is_some() && e.cycle_lane.is_some());
        assert!(e.road_class.is_none(), "not asked for in the filter");
    }
    let mut last_edge = 0;
    for m in &r.matched_points {
        if m.type_.as_deref() == Some("matched") {
            let e = m.edge_index.unwrap();
            assert!(e < r.edges.len() && e >= last_edge);
            last_edge = e;
            let f = m.distance_along_edge.unwrap();
            assert!((0.0..=1.0).contains(&f));
        }
    }
    let unmatched = r
        .matched_points
        .iter()
        .filter(|m| m.type_.as_deref() == Some("unmatched"))
        .count();
    assert!(unmatched * 50 < n, "{unmatched} unmatched of {n}");

    let (agree, paved_agree) = agreement(&s, &r);
    assert!(agree > 0.8, "agreement {agree}");
    assert!(paved_agree > 0.95, "paved/unpaved agreement {paved_agree}");
}

/// the client's projection of an answer, against the ranges it computed from Valhalla's answer:
/// the class agreement and the paved / unpaved agreement.
fn agreement(s: &Stretch, r: &TraceResponse) -> (f64, f64) {
    let (cover, _) = client::compute(&s.lat, &s.lon, &s.dist, "bicycle", |_, _| {
        CallResult::Ok(Box::new(r.clone()))
    })
    .unwrap();
    let (mut agree, mut paved_agree, mut total) = (0.0, 0.0, 0.0);
    for rf in &s.reference {
        for us in &cover {
            let l = rf.to.min(us.to) - rf.from.max(us.from);
            if l <= 0.0 {
                continue;
            }
            total += l;
            let ours = TrackSurface::of_code(Some(&us.value));
            if ours == rf.value {
                agree += l;
            }
            let paved =
                |c: TrackSurface| matches!(c, TrackSurface::Paved | TrackSurface::PavedRough);
            if paved(ours) == paved(rf.value) {
                paved_agree += l;
            }
        }
    }
    (agree / total, paved_agree / total)
}

#[tokio::test]
async fn offline_a_trace_outside_the_cache_is_a_503_not_a_definitive_171() {
    let (status, err) = post(
        offline_router(),
        client::request_json(
            &polyline::encode(&[(48.85, 2.35), (48.851, 2.35)]),
            "bicycle",
        ),
        false,
    )
    .await;
    assert_eq!(status, StatusCode::SERVICE_UNAVAILABLE, "{err}");
}

#[tokio::test]
async fn the_valhalla_shape_form_and_no_filter() {
    let s = stretch();
    let shape: Vec<serde_json::Value> = s
        .lat
        .iter()
        .zip(&s.lon)
        .take(30)
        .map(|(a, b)| serde_json::json!({"lat": a, "lon": b}))
        .collect();
    let (status, body) = post(
        offline_router(),
        serde_json::json!({"shape": shape, "costing": "pedestrian"}),
        false,
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(body["matched_points"].as_array().unwrap().len(), 30);
    let e = &body["edges"][0];
    assert!(e["road_class"].is_string() && e["length"].is_number() && e["surface"].is_string());
    assert!(body["matched_points"][0]["lat"].is_number());
}

#[tokio::test]
async fn valhalla_errors() {
    let s = stretch();
    let one = polyline::encode(&[(s.lat[0], s.lon[0])]);
    let cases = vec![
        (client::request_json(&one, "bicycle"), 123),
        (client::request_json(&encoded(&s), "spaceship"), 125),
        // in the Bay of Biscay: no tile, no street
        (
            client::request_json(&polyline::encode(&[(46.0, -4.0), (46.01, -4.0)]), "bicycle"),
            171,
        ),
        // 2.2 degrees of latitude: more than 200 km
        (
            client::request_json(&polyline::encode(&[(45.0, -1.0), (47.2, -1.0)]), "bicycle"),
            154,
        ),
        (
            client::request_json(&polyline::encode(&vec![(47.2, -1.5); 16_001]), "bicycle"),
            153,
        ),
        (serde_json::json!({"costing": "bicycle"}), 126),
    ];
    for (body, code) in cases {
        let (status, err) = post(offline_router(), body, false).await;
        assert_eq!(status, StatusCode::BAD_REQUEST, "{err}");
        assert_eq!(err["error_code"], code, "{err}");
        assert_eq!(err["status_code"], 400);
        assert_eq!(err["status"], "Bad Request");
        assert!(err["error"].is_string());
    }
    let resp = offline_router()
        .oneshot(
            Request::post("/trace_attributes/v1")
                .body(Body::from("{not json"))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::BAD_REQUEST);
}

#[tokio::test]
async fn an_unreachable_tile_server_is_a_503_for_the_client_to_retry() {
    let config = Config {
        cache_dir: None,
        tiles_url: "http://127.0.0.1:9/{z}/{x}/{y}".into(),
        fetch_timeout: std::time::Duration::from_secs(2),
        ..Config::default()
    };
    let router = api::router(Arc::new(Service::new(config).unwrap()));
    let s = stretch();
    let (status, err) = post(router, client::request_json(&encoded(&s), "bicycle"), false).await;
    assert_eq!(status, StatusCode::SERVICE_UNAVAILABLE, "{err}");
    assert!(!client::DEFINITIVE_CODES.contains(&(err["error_code"].as_i64().unwrap() as i32)));
}

#[tokio::test]
async fn two_identical_points_answer_an_empty_200() {
    let s = stretch();
    let p = (s.lat[3], s.lon[3]);
    let (status, body) = post(
        offline_router(),
        client::request_json(&polyline::encode(&[p, p]), "bicycle"),
        false,
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(body["matched_points"].as_array().unwrap().len(), 2);
}
