//! Configuration from the environment.

use std::path::PathBuf;
use std::time::Duration;

#[derive(Debug, Clone)]
pub struct Config {
    /// `LISTEN_ADDR`, default `0.0.0.0:8002`.
    pub listen_addr: String,
    /// `TILES_URL`, a template with `{z}`, `{x}` and `{y}`.
    pub tiles_url: String,
    /// `TILE_ZOOM`, default 14 (the max zoom of the VersaTiles Shortbread tiles).
    pub tile_zoom: u8,
    /// `CACHE_DIR`, default `./.cache/tiles`; empty disables the disk cache.
    pub cache_dir: Option<PathBuf>,
    /// `CACHE_TTL_DAYS`, default 7: older tiles on disk are fetched again (and still used
    /// when the fetch fails). Tiles not refreshed for twice that are deleted.
    pub cache_ttl: Duration,
    /// `CACHE_MAX_GB`: when set, the oldest tiles beyond this size are deleted (checked at
    /// startup and every 6 hours).
    pub cache_max_bytes: Option<u64>,
    /// `MEMORY_CACHE_MB`, default 256: the in-memory LRU of decoded tiles, weighed by the
    /// size of their vertices.
    pub memory_cache_mb: u64,
    /// `FETCH_CONCURRENCY`, default 8 simultaneous tile downloads for the whole process.
    pub fetch_concurrency: usize,
    /// `FETCH_TIMEOUT_SECS`, default 8.
    pub fetch_timeout: Duration,
    /// `TILES_OFFLINE`: `1`/`true` never fetches, the disk cache is the only source.
    pub offline: bool,
    /// `USER_AGENT` sent to the tile server.
    pub user_agent: String,
    /// `ACCESS_RULES`: `fr` (default, Valhalla's French defaults: bicycles allowed on
    /// footways, not on trunks) or `default` (Valhalla's world defaults).
    pub access_rules: AccessRules,
    /// `MAX_CONCURRENT_MATCHES`, default: the number of CPUs.
    pub max_concurrent_matches: usize,
    /// `TILES_DEADLINE_SECS`, default 11: loading the tiles of one request takes no longer,
    /// or the request fails with a 503 (pedalons's read timeout is 15 s; it retries a 503).
    pub tiles_deadline: Duration,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AccessRules {
    France,
    World,
}

pub const DEFAULT_TILES_URL: &str = "https://tiles.versatiles.org/tiles/osm/{z}/{x}/{y}";

impl Default for Config {
    fn default() -> Self {
        Self {
            listen_addr: "0.0.0.0:8002".into(),
            tiles_url: DEFAULT_TILES_URL.into(),
            tile_zoom: 14,
            cache_dir: Some(PathBuf::from("./.cache/tiles")),
            cache_ttl: Duration::from_secs(7 * 24 * 3600),
            cache_max_bytes: None,
            memory_cache_mb: 256,
            fetch_concurrency: 8,
            fetch_timeout: Duration::from_secs(8),
            offline: false,
            user_agent: concat!(
                "osm-surface/",
                env!("CARGO_PKG_VERSION"),
                " (trace_attributes replacement; +https://github.com/glandais)"
            )
            .into(),
            access_rules: AccessRules::France,
            max_concurrent_matches: std::thread::available_parallelism()
                .map(|n| n.get())
                .unwrap_or(4),
            tiles_deadline: Duration::from_secs(11),
        }
    }
}

impl Config {
    /// The TileJSON next to the tiles (`.../{z}/{x}/{y}` → `.../tiles.json`), if the
    /// template has that shape.
    pub fn tiles_json_url(&self) -> Option<String> {
        let i = self.tiles_url.find("/{z}/{x}/{y}")?;
        Some(format!("{}/tiles.json", &self.tiles_url[..i]))
    }

    pub fn from_env() -> Self {
        let mut c = Self::default();
        let var = |k: &str| std::env::var(k).ok().filter(|v| !v.trim().is_empty());
        if let Some(v) = var("LISTEN_ADDR") {
            c.listen_addr = v;
        }
        if let Some(v) = var("TILES_URL") {
            c.tiles_url = v;
        }
        if let Some(v) = var("TILE_ZOOM").and_then(|v| v.parse().ok()) {
            c.tile_zoom = v;
        }
        match std::env::var("CACHE_DIR") {
            Ok(v) if v.trim().is_empty() => c.cache_dir = None,
            Ok(v) => c.cache_dir = Some(PathBuf::from(v)),
            Err(_) => {}
        }
        if let Some(v) = var("CACHE_TTL_DAYS").and_then(|v| v.parse::<f64>().ok()) {
            c.cache_ttl = Duration::from_secs_f64(v.max(0.0) * 86_400.0);
        }
        if let Some(v) = var("CACHE_MAX_GB").and_then(|v| v.parse::<f64>().ok()) {
            c.cache_max_bytes = Some((v.max(0.0) * 1e9) as u64);
        }
        if let Some(v) = var("MEMORY_CACHE_MB").and_then(|v| v.parse().ok()) {
            c.memory_cache_mb = v;
        }
        if let Some(v) = var("FETCH_CONCURRENCY").and_then(|v| v.parse::<usize>().ok()) {
            c.fetch_concurrency = v.max(1);
        }
        if let Some(v) = var("FETCH_TIMEOUT_SECS").and_then(|v| v.parse::<f64>().ok()) {
            c.fetch_timeout = Duration::from_secs_f64(v.max(0.5));
        }
        if let Some(v) = var("TILES_OFFLINE") {
            c.offline = matches!(v.as_str(), "1" | "true" | "yes");
        }
        if let Some(v) = var("USER_AGENT") {
            c.user_agent = v;
        }
        if let Some(v) = var("ACCESS_RULES") {
            c.access_rules = if v.eq_ignore_ascii_case("default") || v.eq_ignore_ascii_case("world")
            {
                AccessRules::World
            } else {
                AccessRules::France
            };
        }
        if let Some(v) = var("MAX_CONCURRENT_MATCHES").and_then(|v| v.parse::<usize>().ok()) {
            c.max_concurrent_matches = v.max(1);
        }
        if let Some(v) = var("TILES_DEADLINE_SECS").and_then(|v| v.parse::<f64>().ok()) {
            c.tiles_deadline = Duration::from_secs_f64(v.max(0.5));
        }
        c
    }
}
