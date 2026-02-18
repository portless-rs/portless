use anyhow::{anyhow, Result};
use rand::Rng;
use std::net::TcpListener;
use std::path::PathBuf;
use std::time::Duration;

pub const DEFAULT_PROXY_PORT: u16 = 1355;
pub const PRIVILEGED_PORT_THRESHOLD: u16 = 1024;
pub const MIN_APP_PORT: u16 = 4000;
pub const MAX_APP_PORT: u16 = 4999;
pub const RANDOM_PORT_ATTEMPTS: usize = 50;

/// TCP connect timeout when checking if the proxy is listening.
const SOCKET_TIMEOUT_MS: u64 = 500;

/// Maximum poll attempts waiting for proxy to start.
pub const WAIT_FOR_PROXY_MAX_ATTEMPTS: u32 = 20;
/// Interval between proxy readiness polls (ms).
pub const WAIT_FOR_PROXY_INTERVAL_MS: u64 = 250;

/// Signal number table (mirrors cli-utils.ts SIGNAL_CODES).
pub fn signal_exit_code(sig: nix::sys::signal::Signal) -> i32 {
    use nix::sys::signal::Signal::*;
    128 + match sig {
        SIGHUP => 1,
        SIGINT => 2,
        SIGQUIT => 3,
        SIGABRT => 6,
        SIGKILL => 9,
        SIGTERM => 15,
        _ => 15,
    }
}

pub fn resolve_state_dir(proxy_port: u16) -> PathBuf {
    if let Ok(dir) = std::env::var("PORTLESS_STATE_DIR") {
        return PathBuf::from(dir);
    }
    if proxy_port < PRIVILEGED_PORT_THRESHOLD {
        PathBuf::from("/tmp/portless")
    } else {
        dirs::home_dir()
            .unwrap_or_else(|| PathBuf::from("/tmp"))
            .join(".portless")
    }
}

fn read_port_from_dir(dir: &std::path::Path) -> Option<u16> {
    let content = std::fs::read_to_string(dir.join("proxy.port")).ok()?;
    content.trim().parse().ok()
}

/// Discover the active proxy's state dir and port.
/// Checks user-level dir first (~/.portless), then system-level (/tmp/portless).
/// Falls back to defaults if nothing is running.
pub fn discover_state() -> (PathBuf, u16) {
    if let Ok(dir_str) = std::env::var("PORTLESS_STATE_DIR") {
        let dir = PathBuf::from(dir_str);
        let port = read_port_from_dir(&dir)
            .unwrap_or_else(get_default_port);
        return (dir, port);
    }

    // Check user-level state
    let user_dir = dirs::home_dir()
        .unwrap_or_else(|| PathBuf::from("/tmp"))
        .join(".portless");
    if let Some(port) = read_port_from_dir(&user_dir)
        && is_proxy_running(port) {
            return (user_dir, port);
        }

    // Check system-level state
    let sys_dir = PathBuf::from("/tmp/portless");
    if let Some(port) = read_port_from_dir(&sys_dir)
        && is_proxy_running(port) {
            return (sys_dir, port);
        }

    // Fall back to defaults
    let default_port = get_default_port();
    (resolve_state_dir(default_port), default_port)
}

pub fn get_default_port() -> u16 {
    std::env::var("PORTLESS_PORT")
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(DEFAULT_PROXY_PORT)
}

/// Format a .localhost URL; omit the port when it is 80 (standard HTTP).
pub fn format_url(hostname: &str, proxy_port: u16) -> String {
    if proxy_port == 80 {
        format!("http://{}", hostname)
    } else {
        format!("http://{}:{}", hostname, proxy_port)
    }
}

pub fn parse_hostname(input: &str) -> Result<String> {
    let s = input.trim();
    // Strip protocol prefix
    let s = s
        .strip_prefix("https://")
        .or_else(|| s.strip_prefix("http://"))
        .unwrap_or(s);
    // Strip path and port
    let s = s.split('/').next().unwrap_or(s);
    let mut hostname = s.to_ascii_lowercase();

    if hostname.is_empty() || hostname == ".localhost" {
        return Err(anyhow!("Hostname cannot be empty"));
    }

    // Append .localhost if not already present
    if !hostname.ends_with(".localhost") {
        hostname = format!("{}.localhost", hostname);
    }

    validate_hostname(&hostname)?;
    Ok(hostname)
}

fn validate_hostname(hostname: &str) -> Result<()> {
    let label = hostname.strip_suffix(".localhost").unwrap_or(hostname);

    if label.is_empty() {
        return Err(anyhow!("Hostname label cannot be empty"));
    }

    if label.contains("..") {
        return Err(anyhow!(
            "Invalid hostname \"{}\": consecutive dots are not allowed",
            label
        ));
    }

    // Must match: [a-z0-9]([a-z0-9.-]*[a-z0-9])?
    // Single character must be alphanumeric; multi-character must start and end with alphanumeric.
    let chars: Vec<char> = label.chars().collect();
    let valid = if chars.len() == 1 {
        chars[0].is_ascii_alphanumeric()
    } else {
        chars[0].is_ascii_alphanumeric()
            && chars[chars.len() - 1].is_ascii_alphanumeric()
            && chars[1..chars.len() - 1]
                .iter()
                .all(|c| c.is_ascii_alphanumeric() || *c == '-' || *c == '.')
    };

    if !valid {
        return Err(anyhow!(
            "Invalid hostname \"{}\": must contain only lowercase letters, digits, hyphens, and dots",
            label
        ));
    }

    Ok(())
}

pub fn escape_html(s: &str) -> String {
    s.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
        .replace('\'', "&#39;")
}

pub fn find_free_port() -> Result<u16> {
    let mut rng = rand::rng();

    for _ in 0..RANDOM_PORT_ATTEMPTS {
        let port = rng.random_range(MIN_APP_PORT..=MAX_APP_PORT);
        if is_port_free(port) {
            return Ok(port);
        }
    }

    for port in MIN_APP_PORT..=MAX_APP_PORT {
        if is_port_free(port) {
            return Ok(port);
        }
    }

    Err(anyhow!(
        "No free port found in range {}-{}",
        MIN_APP_PORT,
        MAX_APP_PORT
    ))
}

fn is_port_free(port: u16) -> bool {
    TcpListener::bind(("127.0.0.1", port)).is_ok()
}

/// Check if a portless proxy is running on the given port.
/// Makes a HEAD HTTP request with a short timeout and checks for X-Portless: 1.
pub fn is_proxy_running(port: u16) -> bool {
    use std::io::{Read, Write};

    let Ok(mut stream) = std::net::TcpStream::connect(format!("127.0.0.1:{}", port)) else {
        return false;
    };

    let _ = stream.set_read_timeout(Some(Duration::from_millis(SOCKET_TIMEOUT_MS)));
    let _ = stream.set_write_timeout(Some(Duration::from_millis(SOCKET_TIMEOUT_MS)));

    let request = format!(
        "HEAD / HTTP/1.0\r\nHost: 127.0.0.1:{}\r\nConnection: close\r\n\r\n",
        port
    );

    if stream.write_all(request.as_bytes()).is_err() {
        return false;
    }

    let mut response = String::new();
    let _ = stream.read_to_string(&mut response);

    response.to_ascii_lowercase().contains("x-portless: 1")
}

/// Try to find the PID of a process listening on a given TCP port using `lsof`.
pub fn find_pid_on_port(port: u16) -> Option<u32> {
    let output = std::process::Command::new("lsof")
        .args(["-ti", &format!("tcp:{}", port), "-sTCP:LISTEN"])
        .output()
        .ok()?;
    let s = String::from_utf8_lossy(&output.stdout);
    s.trim().lines().next()?.trim().parse().ok()
}
