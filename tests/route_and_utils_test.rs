use std::path::PathBuf;
use std::sync::Arc;
use std::thread;
use tempfile::TempDir;

#[test]
fn test_route_store_basic_operations() {
    use portless::types::Route;
    use portless::routes::RouteStore;

    let temp_dir = TempDir::new().unwrap();
    let store = RouteStore::new(temp_dir.path().to_path_buf()).unwrap();

    // Test add
    let route = Route {
        hostname: "test.localhost".to_string(),
        port: 4000,
        pid: std::process::id(),
    };
    store.add(route.clone()).unwrap();

    // Test load
    let routes = store.load(false).unwrap();
    assert_eq!(routes.len(), 1);
    assert_eq!(routes[0].hostname, "test.localhost");

    // Test remove
    store.remove("test.localhost").unwrap();
    let routes = store.load(false).unwrap();
    assert_eq!(routes.len(), 0);
}

#[test]
fn test_hostname_parsing() {
    use portless::utils::parse_hostname;

    // Valid cases
    assert_eq!(parse_hostname("test").unwrap(), "test.localhost");
    assert_eq!(parse_hostname("my-app").unwrap(), "my-app.localhost");
    assert_eq!(parse_hostname("http://test").unwrap(), "test.localhost");

    // Invalid cases
    assert!(parse_hostname("").is_err());
    assert!(parse_hostname("test_app").is_err());
    assert!(parse_hostname("-test").is_err());
}

#[test]
fn test_port_utilities() {
    use portless::utils::{find_free_port, MIN_APP_PORT, MAX_APP_PORT};

    let port = find_free_port().unwrap();
    assert!(port >= MIN_APP_PORT);
    assert!(port <= MAX_APP_PORT);
}

#[test]
fn test_format_url() {
    use portless::utils::format_url;

    assert_eq!(format_url("test.localhost", 1355), "http://test.localhost:1355");
    assert_eq!(format_url("test.localhost", 80), "http://test.localhost");
}

#[test]
fn test_html_escaping() {
    use portless::utils::escape_html;

    assert_eq!(escape_html("<script>alert('xss')</script>"),
               "&lt;script&gt;alert(&#39;xss&#39;)&lt;/script&gt;");
    assert_eq!(escape_html("a & b"), "a &amp; b");
}

#[test]
fn test_signal_exit_codes() {
    use portless::utils::signal_exit_code;
    use nix::sys::signal::Signal;

    assert_eq!(signal_exit_code(Signal::SIGINT), 130);
    assert_eq!(signal_exit_code(Signal::SIGTERM), 143);
}

#[test]
fn test_route_serialization() {
    use portless::types::Route;

    let route = Route {
        hostname: "app.localhost".to_string(),
        port: 4500,
        pid: 12345,
    };

    let json = serde_json::to_string(&route).unwrap();
    let parsed: Route = serde_json::from_str(&json).unwrap();

    assert_eq!(route, parsed);
}

#[test]
fn test_state_dir_resolution() {
    use portless::utils::resolve_state_dir;

    // Privileged port
    let dir = resolve_state_dir(80);
    assert_eq!(dir, PathBuf::from("/tmp/portless"));

    // User-level port
    let dir = resolve_state_dir(1355);
    assert!(dir.to_str().unwrap().contains(".portless"));
}

#[test]
fn test_route_replacement() {
    use portless::types::Route;
    use portless::routes::RouteStore;

    let temp_dir = TempDir::new().unwrap();
    let store = RouteStore::new(temp_dir.path().to_path_buf()).unwrap();

    // Add first route
    let route1 = Route {
        hostname: "test.localhost".to_string(),
        port: 4000,
        pid: std::process::id(),
    };
    store.add(route1).unwrap();

    // Add second route with same hostname
    let route2 = Route {
        hostname: "test.localhost".to_string(),
        port: 4100,
        pid: std::process::id(),
    };
    store.add(route2).unwrap();

    // Should only have one route with updated port
    let routes = store.load(false).unwrap();
    assert_eq!(routes.len(), 1);
    assert_eq!(routes[0].port, 4100);
}

#[test]
fn test_concurrent_route_operations() {
    use portless::types::Route;
    use portless::routes::RouteStore;

    let temp_dir = TempDir::new().unwrap();
    let store = Arc::new(RouteStore::new(temp_dir.path().to_path_buf()).unwrap());

    let mut handles = vec![];

    // Spawn multiple threads adding routes
    for i in 0..10 {
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

    // Wait for all threads
    for handle in handles {
        handle.join().unwrap();
    }

    // All routes should be present
    let routes = store.load(false).unwrap();
    assert_eq!(routes.len(), 10);
}

#[test]
fn test_dead_pid_filtering() {
    use portless::types::Route;
    use portless::routes::RouteStore;

    let temp_dir = TempDir::new().unwrap();
    let store = RouteStore::new(temp_dir.path().to_path_buf()).unwrap();

    // Create routes with live and dead PIDs
    let routes = vec![
        Route {
            hostname: "alive.localhost".to_string(),
            port: 4000,
            pid: std::process::id(), // Current process
        },
        Route {
            hostname: "dead.localhost".to_string(),
            port: 4001,
            pid: 999999, // Non-existent PID
        },
    ];

    store.save(&routes).unwrap();

    // load() should filter out dead PIDs
    let loaded = store.load(false).unwrap();
    assert_eq!(loaded.len(), 1);
    assert_eq!(loaded[0].hostname, "alive.localhost");

    // load_raw() should keep all routes
    let raw = store.load_raw().unwrap();
    assert_eq!(raw.len(), 2);
}

