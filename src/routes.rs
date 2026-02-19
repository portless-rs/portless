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

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use tempfile::TempDir;

    fn create_test_store() -> (RouteStore, TempDir) {
        let temp_dir = TempDir::new().unwrap();
        let store = RouteStore::new(temp_dir.path().to_path_buf()).unwrap();
        (store, temp_dir)
    }

    #[test]
    fn test_route_store_new() {
        let temp_dir = TempDir::new().unwrap();
        let result = RouteStore::new(temp_dir.path().to_path_buf());
        assert!(result.is_ok());
    }

    #[test]
    fn test_load_empty() {
        let (store, _temp) = create_test_store();
        let routes = store.load(false).unwrap();
        assert_eq!(routes.len(), 0);
    }

    #[test]
    fn test_load_raw_empty() {
        let (store, _temp) = create_test_store();
        let routes = store.load_raw().unwrap();
        assert_eq!(routes.len(), 0);
    }

    #[test]
    fn test_save_and_load() {
        let (store, _temp) = create_test_store();

        let routes = vec![
            Route {
                hostname: "test1.localhost".to_string(),
                port: 4000,
                pid: std::process::id(),
            },
            Route {
                hostname: "test2.localhost".to_string(),
                port: 4001,
                pid: std::process::id(),
            },
        ];

        store.save(&routes).unwrap();
        let loaded = store.load(false).unwrap();

        assert_eq!(loaded.len(), 2);
        assert_eq!(loaded[0].hostname, "test1.localhost");
        assert_eq!(loaded[1].hostname, "test2.localhost");
    }

    #[test]
    fn test_add_route() {
        let (store, _temp) = create_test_store();

        let route = Route {
            hostname: "newapp.localhost".to_string(),
            port: 4200,
            pid: std::process::id(),
        };

        store.add(route.clone()).unwrap();
        let routes = store.load(false).unwrap();

        assert_eq!(routes.len(), 1);
        assert_eq!(routes[0].hostname, "newapp.localhost");
        assert_eq!(routes[0].port, 4200);
    }

    #[test]
    fn test_add_replaces_existing_hostname() {
        let (store, _temp) = create_test_store();

        let route1 = Route {
            hostname: "test.localhost".to_string(),
            port: 4000,
            pid: std::process::id(),
        };

        let route2 = Route {
            hostname: "test.localhost".to_string(),
            port: 4100,
            pid: std::process::id(),
        };

        store.add(route1).unwrap();
        store.add(route2).unwrap();

        let routes = store.load(false).unwrap();
        assert_eq!(routes.len(), 1);
        assert_eq!(routes[0].port, 4100); // Second route should replace first
    }

    #[test]
    fn test_remove_route() {
        let (store, _temp) = create_test_store();

        let route1 = Route {
            hostname: "app1.localhost".to_string(),
            port: 4000,
            pid: std::process::id(),
        };

        let route2 = Route {
            hostname: "app2.localhost".to_string(),
            port: 4001,
            pid: std::process::id(),
        };

        store.add(route1).unwrap();
        store.add(route2).unwrap();

        store.remove("app1.localhost").unwrap();

        let routes = store.load(false).unwrap();
        assert_eq!(routes.len(), 1);
        assert_eq!(routes[0].hostname, "app2.localhost");
    }

    #[test]
    fn test_remove_nonexistent() {
        let (store, _temp) = create_test_store();

        let result = store.remove("nonexistent.localhost");
        assert!(result.is_ok());

        let routes = store.load(false).unwrap();
        assert_eq!(routes.len(), 0);
    }

    #[test]
    fn test_load_filters_dead_pids() {
        let (store, _temp) = create_test_store();

        // Create a route with a fake dead PID
        let routes = vec![
            Route {
                hostname: "alive.localhost".to_string(),
                port: 4000,
                pid: std::process::id(), // Current process (alive)
            },
            Route {
                hostname: "dead.localhost".to_string(),
                port: 4001,
                pid: 999999, // Non-existent PID
            },
        ];

        store.save(&routes).unwrap();
        let loaded = store.load(false).unwrap();

        // Only the alive route should be loaded
        assert_eq!(loaded.len(), 1);
        assert_eq!(loaded[0].hostname, "alive.localhost");
    }

    #[test]
    fn test_load_raw_does_not_filter() {
        let (store, _temp) = create_test_store();

        let routes = vec![
            Route {
                hostname: "alive.localhost".to_string(),
                port: 4000,
                pid: std::process::id(),
            },
            Route {
                hostname: "dead.localhost".to_string(),
                port: 4001,
                pid: 999999,
            },
        ];

        store.save(&routes).unwrap();
        let loaded = store.load_raw().unwrap();

        // load_raw should return all routes without filtering
        assert_eq!(loaded.len(), 2);
    }

    #[test]
    fn test_load_invalid_json() {
        let (store, temp) = create_test_store();

        // Write invalid JSON
        fs::write(temp.path().join("routes.json"), "invalid json").unwrap();

        let routes = store.load(false).unwrap();
        assert_eq!(routes.len(), 0); // Should return empty vec on parse error
    }

    #[test]
    fn test_is_pid_alive() {
        // Current process should be alive
        assert!(is_pid_alive(std::process::id()));

        // Very high PID unlikely to exist
        assert!(!is_pid_alive(999999));
    }

    #[test]
    fn test_lock_mechanism() {
        let (store, _temp) = create_test_store();

        // Manually acquire lock
        store.acquire_lock().unwrap();

        // Verify lock directory exists
        assert!(store.lock_path().exists());

        // Release lock
        store.release_lock();

        // Verify lock is removed
        assert!(!store.lock_path().exists());
    }

    #[test]
    fn test_concurrent_add() {
        use std::sync::Arc;
        use std::thread;

        let (store, _temp) = create_test_store();
        let store = Arc::new(store);

        let mut handles = vec![];

        // Spawn multiple threads trying to add routes concurrently
        for i in 0..5 {
            let store_clone = Arc::clone(&store);
            let handle = thread::spawn(move || {
                let route = Route {
                    hostname: format!("app{}.localhost", i),
                    port: 4000 + i as u16,
                    pid: std::process::id(),
                };
                store_clone.add(route).unwrap();
            });
            handles.push(handle);
        }

        // Wait for all threads to complete
        for handle in handles {
            handle.join().unwrap();
        }

        // Verify all routes were added
        let routes = store.load(false).unwrap();
        assert_eq!(routes.len(), 5);
    }
}
