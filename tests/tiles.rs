//! The tile store against a local tile server: what is cached, and what becomes a 503.

use axum::extract::{Path as UrlPath, State};
use axum::http::StatusCode;
use axum::routing::get;
use axum::Router;
use osm_surface::config::Config;
use osm_surface::tiles::{cache_namespace, TileError, TileStore};
use std::path::PathBuf;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;

const TILE: (u32, u32) = (8118, 5742);

fn fixture_tile() -> Vec<u8> {
    std::fs::read(
        PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("tests/fixtures/tiles")
            .join(cache_namespace(osm_surface::config::DEFAULT_TILES_URL))
            .join("14/8118/5742.mvt"),
    )
    .unwrap()
}

/// What the server answers to its n-th request (0-based).
type Script = Arc<dyn Fn(usize) -> (StatusCode, Vec<u8>) + Send + Sync>;

#[derive(Clone)]
struct Server {
    script: Script,
    hits: Arc<AtomicUsize>,
}

async fn tile(
    State(s): State<Server>,
    UrlPath(_): UrlPath<(u32, u32, String)>,
) -> (StatusCode, Vec<u8>) {
    let n = s.hits.fetch_add(1, Ordering::SeqCst);
    (s.script)(n)
}

/// A tile server on a free port; returns its URL template and its request counter.
async fn serve(script: Script) -> (String, Arc<AtomicUsize>) {
    let hits = Arc::new(AtomicUsize::new(0));
    let app = Router::new()
        .route("/t/{z}/{x}/{y}", get(tile))
        .with_state(Server {
            script,
            hits: hits.clone(),
        });
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
    (format!("http://{addr}/t/{{z}}/{{x}}/{{y}}"), hits)
}

fn cache_dir(name: &str) -> PathBuf {
    let d =
        PathBuf::from(env!("CARGO_TARGET_TMPDIR")).join(format!("{name}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&d);
    d
}

fn store(url: &str, dir: &std::path::Path, offline: bool) -> TileStore {
    TileStore::new(&Config {
        tiles_url: url.into(),
        cache_dir: Some(dir.to_path_buf()),
        offline,
        ..Config::default()
    })
    .unwrap()
}

fn cached(url: &str, dir: &std::path::Path) -> PathBuf {
    dir.join(cache_namespace(url))
        .join(format!("14/{}/{}.mvt", TILE.0, TILE.1))
}

#[tokio::test]
async fn a_garbage_answer_is_not_cached_and_the_next_fetch_succeeds() {
    let good = fixture_tile();
    let (url, hits) = serve(Arc::new(move |n| {
        if n == 0 {
            (StatusCode::OK, b"<html>maintenance</html>".to_vec())
        } else {
            (StatusCode::OK, good.clone())
        }
    }))
    .await;
    let dir = cache_dir("garbage");
    let s = store(&url, &dir, false);
    assert!(matches!(s.get(TILE).await, Err(TileError::Invalid(..))));
    assert!(
        !cached(&url, &dir).exists(),
        "an invalid tile must not be cached"
    );
    // A new store (an empty memory cache): the disk has nothing, so it fetches again.
    let s = store(&url, &dir, false);
    let t = s.get(TILE).await.unwrap();
    assert!(!t.lines.is_empty());
    assert_eq!(hits.load(Ordering::SeqCst), 2);
    assert!(cached(&url, &dir).exists());
}

#[tokio::test]
async fn a_poisoned_cache_file_is_deleted_and_fetched_again() {
    let good = fixture_tile();
    let (url, hits) = serve(Arc::new(move |_| (StatusCode::OK, good.clone()))).await;
    let dir = cache_dir("poisoned");
    let p = cached(&url, &dir);
    std::fs::create_dir_all(p.parent().unwrap()).unwrap();
    std::fs::write(&p, b"<html>error</html>").unwrap();
    let s = store(&url, &dir, false);
    assert!(!s.get(TILE).await.unwrap().lines.is_empty());
    assert_eq!(hits.load(Ordering::SeqCst), 1);
    assert_eq!(std::fs::read(&p).unwrap(), fixture_tile());
}

#[tokio::test]
async fn a_404_is_unavailable_and_nothing_is_written() {
    let (url, _) = serve(Arc::new(|_| (StatusCode::NOT_FOUND, b"not found".to_vec()))).await;
    let dir = cache_dir("notfound");
    let s = store(&url, &dir, false);
    assert!(matches!(s.get(TILE).await, Err(TileError::Unavailable(..))));
    assert!(!cached(&url, &dir).exists());
}

#[tokio::test]
async fn offline_a_tile_missing_from_the_cache_is_unavailable() {
    let dir = cache_dir("offline");
    let s = store(osm_surface::config::DEFAULT_TILES_URL, &dir, true);
    assert!(matches!(s.get(TILE).await, Err(TileError::Unavailable(..))));
}
