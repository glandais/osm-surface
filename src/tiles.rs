//! Tile source: HTTP fetches (bounded, retried once), a disk cache of the raw tiles with a
//! TTL, and an in-memory LRU of the decoded `streets` layers.

use crate::config::Config;
use crate::geo::Grid;
use crate::mvt::{self, StreetTile};
use futures::stream::{self, StreamExt, TryStreamExt};
use moka::future::Cache;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::{Duration, SystemTime};
use tokio::sync::Semaphore;

#[derive(Debug, Clone, thiserror::Error)]
pub enum TileError {
    #[error("tile {0} unavailable: {1}")]
    Unavailable(String, String),
    #[error("tile {0} is not a valid vector tile: {1}")]
    Invalid(String, String),
}

type Key = (u32, u32);

pub struct TileStore {
    grid: Grid,
    url_template: String,
    cache_dir: Option<PathBuf>,
    ttl: Duration,
    offline: bool,
    client: reqwest::Client,
    fetches: Arc<Semaphore>,
    memory: Cache<Key, Arc<StreetTile>>,
}

impl TileStore {
    pub fn new(config: &Config) -> anyhow::Result<Self> {
        let client = reqwest::Client::builder()
            .user_agent(config.user_agent.clone())
            .connect_timeout(Duration::from_secs(4))
            .timeout(config.fetch_timeout)
            .pool_max_idle_per_host(config.fetch_concurrency)
            .build()?;
        Ok(Self {
            grid: Grid::new(config.tile_zoom, 4096),
            url_template: config.tiles_url.clone(),
            cache_dir: config
                .cache_dir
                .as_ref()
                .map(|d| d.join(cache_namespace(&config.tiles_url))),
            ttl: config.cache_ttl,
            offline: config.offline,
            client,
            fetches: Arc::new(Semaphore::new(config.fetch_concurrency)),
            memory: Cache::builder()
                // Weighed in bytes (16 per vertex, plus a fixed overhead per line and tile).
                .weigher(|_, t: &Arc<StreetTile>| {
                    let points: usize = t.lines.iter().map(|l| l.points.len()).sum();
                    (points * 16 + t.lines.len() * 32 + 1024).min(u32::MAX as usize) as u32
                })
                .max_capacity(config.memory_cache_mb * 1024 * 1024)
                .time_to_live(config.cache_ttl.max(Duration::from_secs(60)))
                .build(),
        })
    }

    pub fn grid(&self) -> Grid {
        self.grid
    }

    /// The decoded tiles, fetched concurrently (bounded by the store's semaphore). Fails as a
    /// whole when one of them is unavailable.
    pub async fn get_many(&self, keys: &[Key]) -> Result<Vec<Arc<StreetTile>>, TileError> {
        stream::iter(keys.iter().copied())
            .map(|k| self.get(k))
            .buffered(32)
            .try_collect()
            .await
    }

    /// One decoded tile; concurrent requests for the same tile share one load.
    pub async fn get(&self, key: Key) -> Result<Arc<StreetTile>, TileError> {
        self.memory
            .try_get_with(key, self.load(key))
            .await
            .map_err(|e| (*e).clone())
    }

    fn name(&self, (x, y): Key) -> String {
        format!("{}/{}/{}", self.grid.zoom, x, y)
    }

    fn path(&self, (x, y): Key) -> Option<PathBuf> {
        self.cache_dir.as_ref().map(|d| {
            d.join(self.grid.zoom.to_string())
                .join(x.to_string())
                .join(format!("{y}.mvt"))
        })
    }

    /// The tile from the disk cache when it is fresh, else from the server (stored on disk
    /// only once it decodes), else from the stale cache. A cached file that does not decode is
    /// deleted and fetched again, so that a bad answer of the server is not kept for a TTL.
    async fn load(&self, key: Key) -> Result<Arc<StreetTile>, TileError> {
        let name = self.name(key);
        let path = self.path(key);
        let mut stale: Option<Vec<u8>> = None;
        if let Some(p) = &path {
            if let Some((bytes, age)) = read_cached(p).await {
                if self.offline || age.is_some_and(|a| a <= self.ttl) {
                    match self.decode_cached(key, bytes).await {
                        Ok(t) => return Ok(t),
                        Err(e) => {
                            tracing::warn!(tile = %name, error = %e, "invalid cached tile, deleted");
                            let _ = tokio::fs::remove_file(p).await;
                        }
                    }
                } else {
                    stale = Some(bytes);
                }
            }
        }
        if self.offline {
            // Not a definitive "no street here" (400/171): a 503 that tribly retries later.
            return Err(TileError::Unavailable(
                name,
                "not cached (offline mode)".into(),
            ));
        }
        let fetched = match self.fetch(key).await {
            Ok(Fetched::NoContent) => {
                return Ok(Arc::new(StreetTile {
                    x: key.0,
                    y: key.1,
                    ..Default::default()
                }))
            }
            Ok(Fetched::Body(bytes)) => match self.decode(key, bytes.clone()).await {
                Ok(tile) => {
                    if let Some(p) = path.as_ref().filter(|_| !bytes.is_empty()) {
                        if let Err(e) = write_atomic(p, &bytes).await {
                            tracing::warn!(tile = %name, error = %e, "cannot write the tile cache");
                        }
                    }
                    return Ok(tile);
                }
                Err(e) => e,
            },
            Err(e) => e,
        };
        match stale {
            Some(bytes) => match self.decode_cached(key, bytes).await {
                Ok(t) => {
                    tracing::warn!(tile = %name, error = %fetched, "fetch failed, using the stale cached tile");
                    Ok(t)
                }
                Err(_) => {
                    if let Some(p) = &path {
                        let _ = tokio::fs::remove_file(p).await;
                    }
                    Err(fetched)
                }
            },
            None => Err(fetched),
        }
    }

    /// A cached file is never empty (empty bodies are not written): an empty one is a leftover
    /// of an older version, invalid.
    async fn decode_cached(&self, key: Key, bytes: Vec<u8>) -> Result<Arc<StreetTile>, TileError> {
        if bytes.is_empty() {
            return Err(TileError::Invalid(
                self.name(key),
                "empty cached file".into(),
            ));
        }
        self.decode(key, bytes).await
    }

    async fn decode(&self, key: Key, bytes: Vec<u8>) -> Result<Arc<StreetTile>, TileError> {
        let grid = self.grid;
        let name = self.name(key);
        tokio::task::spawn_blocking(move || mvt::decode_streets(&bytes, grid, key.0, key.1))
            .await
            .map_err(|e| TileError::Invalid(name.clone(), e.to_string()))?
            .map(Arc::new)
            .map_err(|e| TileError::Invalid(name, e.to_string()))
    }

    async fn fetch(&self, key: Key) -> Result<Fetched, TileError> {
        let url = self
            .url_template
            .replace("{z}", &self.grid.zoom.to_string())
            .replace("{x}", &key.0.to_string())
            .replace("{y}", &key.1.to_string());
        let name = self.name(key);
        let mut last_error = String::new();
        for attempt in 0..2 {
            if attempt > 0 {
                tokio::time::sleep(Duration::from_millis(300)).await;
            }
            let _permit = self
                .fetches
                .acquire()
                .await
                .expect("semaphore never closed");
            let started = std::time::Instant::now();
            match self.client.get(&url).send().await {
                Ok(resp) => {
                    let status = resp.status();
                    if status == reqwest::StatusCode::NO_CONTENT {
                        // No tile there: nothing to match on (VersaTiles answers a small
                        // valid tile at sea; a 404 is a wrong URL, never "no street").
                        return Ok(Fetched::NoContent);
                    }
                    if status.is_success() {
                        match resp.bytes().await {
                            Ok(b) => {
                                tracing::debug!(tile = %name, bytes = b.len(), ms = started.elapsed().as_millis() as u64, "tile fetched");
                                return Ok(Fetched::Body(b.to_vec()));
                            }
                            Err(e) => last_error = e.to_string(),
                        }
                    } else if status.is_server_error()
                        || status == reqwest::StatusCode::TOO_MANY_REQUESTS
                    {
                        last_error = format!("HTTP {status}");
                    } else {
                        return Err(TileError::Unavailable(name, format!("HTTP {status}")));
                    }
                }
                Err(e) => last_error = e.to_string(),
            }
        }
        Err(TileError::Unavailable(name, last_error))
    }
}

enum Fetched {
    Body(Vec<u8>),
    NoContent,
}

/// The cached bytes and their age.
async fn read_cached(path: &Path) -> Option<(Vec<u8>, Option<Duration>)> {
    let meta = tokio::fs::metadata(path).await.ok()?;
    let age = meta
        .modified()
        .ok()
        .and_then(|m| SystemTime::now().duration_since(m).ok());
    let bytes = tokio::fs::read(path).await.ok()?;
    Some((bytes, age))
}

/// The cache sub-directory of a tile URL template: a readable slug of it and a hash of the
/// whole template, so that changing `TILES_URL` never serves the tiles of the old source.
pub fn cache_namespace(url_template: &str) -> String {
    let base = url_template
        .split_once("://")
        .map_or(url_template, |(_, rest)| rest);
    let base = base.split('{').next().unwrap_or(base);
    let slug: String = base
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || c == '.' || c == '-' {
                c
            } else {
                '_'
            }
        })
        .collect();
    let slug: String = slug.trim_matches('_').chars().take(60).collect();
    // FNV-1a: stable across builds and platforms (unlike std's hasher).
    let mut h: u64 = 0xcbf2_9ce4_8422_2325;
    for b in url_template.bytes() {
        h ^= b as u64;
        h = h.wrapping_mul(0x0100_0000_01b3);
    }
    format!("{slug}-{:08x}", h as u32)
}

/// Removes what the disk cache no longer needs: temporary files of interrupted writes,
/// tiles not refreshed for `max_age`, and the oldest tiles beyond `max_bytes` (if set).
/// Returns the number of files removed and the bytes left. Blocking: run it off the runtime.
pub fn sweep_cache(root: &Path, max_age: Duration, max_bytes: Option<u64>) -> (usize, u64) {
    let mut files: Vec<(SystemTime, u64, PathBuf)> = Vec::new();
    let mut removed = 0;
    let now = SystemTime::now();
    let mut stack = vec![root.to_path_buf()];
    while let Some(dir) = stack.pop() {
        let Ok(entries) = std::fs::read_dir(&dir) else {
            continue;
        };
        for entry in entries.flatten() {
            let path = entry.path();
            let Ok(meta) = entry.metadata() else { continue };
            if meta.is_dir() {
                stack.push(path);
                continue;
            }
            let mtime = meta.modified().unwrap_or(now);
            let age = now.duration_since(mtime).unwrap_or_default();
            let ext = path.extension().and_then(|e| e.to_str()).unwrap_or("");
            let is_tmp = ext.starts_with("tmp");
            if (is_tmp && age > Duration::from_secs(3600)) || (ext == "mvt" && age > max_age) {
                if std::fs::remove_file(&path).is_ok() {
                    removed += 1;
                }
            } else if ext == "mvt" {
                files.push((mtime, meta.len(), path));
            }
        }
    }
    let mut total: u64 = files.iter().map(|f| f.1).sum();
    if let Some(max) = max_bytes {
        files.sort();
        for (_, len, path) in &files {
            if total <= max {
                break;
            }
            if std::fs::remove_file(path).is_ok() {
                removed += 1;
                total -= len;
            }
        }
    }
    (removed, total)
}

async fn write_atomic(path: &Path, bytes: &[u8]) -> std::io::Result<()> {
    if let Some(dir) = path.parent() {
        tokio::fs::create_dir_all(dir).await?;
    }
    let tmp = path.with_extension(format!("tmp{}", std::process::id()));
    tokio::fs::write(&tmp, bytes).await?;
    tokio::fs::rename(&tmp, path).await
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_cache_namespace_depends_on_the_url() {
        // tests/fixtures/tiles is laid out with this name: keep it stable.
        assert_eq!(
            cache_namespace(crate::config::DEFAULT_TILES_URL),
            "tiles.versatiles.org_tiles_osm-9602913b"
        );
        assert_ne!(
            cache_namespace("https://tiles.versatiles.org/tiles/osmX/{z}/{x}/{y}"),
            cache_namespace(crate::config::DEFAULT_TILES_URL)
        );
    }

    #[test]
    fn the_sweep_removes_old_and_temporary_files_and_enforces_the_size() {
        let root = std::env::temp_dir().join(format!("osm-surface-sweep-{}", std::process::id()));
        let dir = root.join("ns/14/1");
        std::fs::create_dir_all(&dir).unwrap();
        let old = SystemTime::now() - Duration::from_secs(30 * 86_400);
        let write = |name: &str, len: usize, mtime: Option<SystemTime>| {
            let p = dir.join(name);
            std::fs::write(&p, vec![1u8; len]).unwrap();
            if let Some(t) = mtime {
                std::fs::File::options()
                    .write(true)
                    .open(&p)
                    .unwrap()
                    .set_modified(t)
                    .unwrap();
            }
            p
        };
        let stale = write("1.mvt", 10, Some(old));
        let tmp = write("2.tmp123", 10, Some(old));
        let a = write(
            "3.mvt",
            100,
            Some(SystemTime::now() - Duration::from_secs(60)),
        );
        let b = write("4.mvt", 100, None);
        let (removed, left) = sweep_cache(&root, Duration::from_secs(14 * 86_400), Some(150));
        assert_eq!((removed, left), (3, 100));
        assert!(!stale.exists() && !tmp.exists() && !a.exists() && b.exists());
        std::fs::remove_dir_all(&root).unwrap();
    }
}
