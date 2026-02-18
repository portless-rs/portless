use anyhow::{anyhow, Result};
use std::fs;
use std::path::{Path, PathBuf};
use std::thread;
use std::time::{Duration, SystemTime};

use crate::types::Route;

const LOCK_MAX_RETRIES: u32 = 20;
const LOCK_RETRY_DELAY_MS: u64 = 50;
const STALE_LOCK_THRESHOLD_MS: u64 = 10_000;

pub struct RouteStore {
    state_dir: PathBuf,
}

impl RouteStore {
    pub fn new(state_dir: PathBuf) -> Result<Self> {
        fs::create_dir_all(&state_dir)?;
        Ok(Self { state_dir })
    }

    fn routes_path(&self) -> PathBuf {
        self.state_dir.join("routes.json")
    }

    fn lock_path(&self) -> PathBuf {
        self.state_dir.join("routes.lock")
    }

    /// Load routes from disk, filtering out stale entries (dead PIDs).
    /// When `persist_cleanup` is true and stale entries were removed,
    /// the cleaned list is written back to disk (only safe while holding the lock).
    pub fn load(&self, persist_cleanup: bool) -> Result<Vec<Route>> {
        let path = self.routes_path();
        if !path.exists() {
            return Ok(vec![]);
        }
        let content = fs::read_to_string(&path)?;
        if content.trim().is_empty() {
            return Ok(vec![]);
        }
        let routes: Vec<Route> = match serde_json::from_str(&content) {
            Ok(r) => r,
            Err(_) => return Ok(vec![]),
        };
        let alive: Vec<Route> = routes
            .into_iter()
            .filter(|r| is_pid_alive(r.pid))
            .collect();

        if persist_cleanup {
            // Only persist when called from within a locked section
            let _ = self.save(&alive);
        }

        Ok(alive)
    }

    /// Load routes without filtering stale entries (used by proxy for display).
    pub fn load_raw(&self) -> Result<Vec<Route>> {
        let path = self.routes_path();
        if !path.exists() {
            return Ok(vec![]);
        }
        let content = fs::read_to_string(&path)?;
        if content.trim().is_empty() {
            return Ok(vec![]);
        }
        Ok(serde_json::from_str(&content).unwrap_or_default())
    }

    pub fn save(&self, routes: &[Route]) -> Result<()> {
        let content = serde_json::to_string_pretty(routes)?;
        fs::write(self.routes_path(), content)?;
        Ok(())
    }

    fn acquire_lock(&self) -> Result<()> {
        let lock_path = self.lock_path();
        for attempt in 0..LOCK_MAX_RETRIES {
            match fs::create_dir(&lock_path) {
                Ok(_) => return Ok(()),
                Err(_) => {
                    if lock_path.exists() && is_lock_stale(&lock_path) {
                        let _ = fs::remove_dir_all(&lock_path);
                        continue;
                    }
                    if attempt + 1 < LOCK_MAX_RETRIES {
                        thread::sleep(Duration::from_millis(LOCK_RETRY_DELAY_MS));
                    }
                }
            }
        }
        Err(anyhow!(
            "Failed to acquire route lock after {} retries",
            LOCK_MAX_RETRIES
        ))
    }

    fn release_lock(&self) {
        let _ = fs::remove_dir_all(self.lock_path());
    }

    pub fn add(&self, route: Route) -> Result<()> {
        self.acquire_lock()?;
        let result = (|| {
            let mut routes = self.load(true)?;
            routes.retain(|r| r.hostname != route.hostname);
            routes.push(route);
            self.save(&routes)
        })();
        self.release_lock();
        result
    }

    pub fn remove(&self, hostname: &str) -> Result<()> {
        self.acquire_lock()?;
        let result = (|| {
            let mut routes = self.load(true)?;
            routes.retain(|r| r.hostname != hostname);
            self.save(&routes)
        })();
        self.release_lock();
        result
    }
}

fn is_pid_alive(pid: u32) -> bool {
    use nix::sys::signal;
    use nix::unistd::Pid;
    signal::kill(Pid::from_raw(pid as i32), None).is_ok()
}

fn is_lock_stale(lock_path: &Path) -> bool {
    let Ok(metadata) = fs::metadata(lock_path) else {
        return false;
    };
    let Ok(modified) = metadata.modified() else {
        return false;
    };
    let Ok(elapsed) = SystemTime::now().duration_since(modified) else {
        return false;
    };
    elapsed.as_millis() > STALE_LOCK_THRESHOLD_MS as u128
}
