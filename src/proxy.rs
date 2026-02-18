use anyhow::Result;
use http_body_util::{combinators::BoxBody, BodyExt, Empty, Full};
use hyper::body::Bytes;
use hyper::header::{HeaderValue, UPGRADE};
use hyper::{Request, Response, StatusCode};
use hyper_util::rt::TokioIo;
use std::net::SocketAddr;
use std::path::PathBuf;
use std::sync::{Arc, RwLock};
use tokio::io::AsyncReadExt;
use tokio::net::{TcpListener, TcpStream};
use tokio::sync::watch;
use tokio::time::{Duration, Instant};

use crate::routes::RouteStore;
use crate::types::Route;
use crate::utils::escape_html;

/// After all routes disappear, wait this long before shutting down.
const IDLE_SHUTDOWN_DELAY: Duration = Duration::from_secs(5);
/// Grace period after startup before idle-shutdown is armed (lets the first app register).
const IDLE_GRACE: Duration = Duration::from_secs(10);
/// How often the route-reloader re-reads routes.json.
const ROUTE_RELOAD_INTERVAL: Duration = Duration::from_millis(100);

pub async fn run_proxy(port: u16, state_dir: PathBuf) -> Result<()> {
    let addr = SocketAddr::from(([0, 0, 0, 0], port));
    let listener = TcpListener::bind(addr).await?;

    // Write PID file so `portless proxy stop` and `shutdown_proxy_if_idle` can find us.
    let pid_path = state_dir.join("proxy.pid");
    let port_path = state_dir.join("proxy.port");
    let my_pid = std::process::id();
    let _ = std::fs::write(&pid_path, my_pid.to_string());
    let _ = std::fs::write(&port_path, port.to_string());

    eprintln!("portless proxy listening on port {}", port);

    // `has_routes` is true while at least one live-PID route exists.
    // The route-reloader writes to this channel; the idle-shutdown task reads from it.
    let (routes_tx, mut routes_rx) = watch::channel(true);

    let store = RouteStore::new(state_dir.clone())?;
    let cached_routes: Arc<RwLock<Vec<Route>>> = Arc::new(RwLock::new(
        store.load_raw().unwrap_or_default(),
    ));

    // Background route-reloader: re-reads routes.json every ROUTE_RELOAD_INTERVAL,
    // updates the in-memory cache, and notifies the idle-shutdown task via the watch channel.
    {
        let cached = cached_routes.clone();
        let sd = state_dir.clone();
        tokio::spawn(async move {
            loop {
                tokio::time::sleep(ROUTE_RELOAD_INTERVAL).await;
                if let Ok(s) = RouteStore::new(sd.clone()) {
                    // load_raw for display cache (no PID filtering — fast path)
                    if let Ok(routes) = s.load_raw() {
                        if let Ok(mut lock) = cached.write() {
                            *lock = routes;
                        }
                    }
                    // load with PID filtering for idle-shutdown signal
                    let alive = s.load(false).unwrap_or_default();
                    let _ = routes_tx.send(!alive.is_empty());
                }
            }
        });
    }

    // Idle-shutdown task: waits until `has_routes` is false, then arms a deadline.
    // If routes come back before the deadline, the timer is cancelled.
    tokio::spawn(async move {
        // Grace period: don't shut down before the first app has had time to register.
        tokio::time::sleep(IDLE_GRACE).await;

        loop {
            // Wait until routes disappear.
            if routes_rx.borrow().eq(&true) {
                if routes_rx.wait_for(|has| !has).await.is_err() {
                    return; // sender dropped — proxy is shutting down anyway
                }
            }

            // Routes are gone; arm the shutdown deadline.
            let deadline = Instant::now() + IDLE_SHUTDOWN_DELAY;
            tokio::select! {
                // Timer fires — still no routes, shut down.
                _ = tokio::time::sleep_until(deadline) => {
                    eprintln!("portless proxy: no active routes — shutting down.");
                    std::process::exit(0);
                }
                // A new route appeared before the deadline — disarm and loop.
                _ = routes_rx.wait_for(|has| *has) => {}
            }
        }
    });

    loop {
        let (stream, remote_addr) = listener.accept().await?;
        let cached_routes = cached_routes.clone();
        let proxy_port = port;
        tokio::spawn(async move {
            let io = TokioIo::new(stream);
            if let Err(e) = hyper::server::conn::http1::Builder::new()
                .serve_connection(
                    io,
                    hyper::service::service_fn(move |req| {
                        let routes = cached_routes
                            .read()
                            .map(|g| g.clone())
                            .unwrap_or_default();
                        handle_request(req, remote_addr, routes, proxy_port)
                    }),
                )
                .with_upgrades()
                .await
            {
                // Ignore connection reset errors
                let msg = e.to_string();
                if !msg.contains("connection reset") && !msg.contains("broken pipe") {
                    eprintln!("connection error: {}", e);
                }
            }
        });
    }
}

async fn handle_request(
    req: Request<hyper::body::Incoming>,
    remote_addr: SocketAddr,
    routes: Vec<Route>,
    proxy_port: u16,
) -> Result<Response<BoxBody<Bytes, hyper::Error>>, hyper::Error> {
    let hostname = extract_hostname(req.headers());

    let Some(host) = hostname else {
        return Ok(bad_request_response("Missing Host header"));
    };

    let target_port = routes.iter().find(|r| r.hostname == host).map(|r| r.port);

    let Some(port) = target_port else {
        return Ok(not_found_response(&routes, &host, proxy_port));
    };

    let is_websocket = req
        .headers()
        .get(UPGRADE)
        .and_then(|v| v.to_str().ok())
        .map(|v| v.eq_ignore_ascii_case("websocket"))
        .unwrap_or(false);

    if is_websocket {
        return handle_websocket(req, port, remote_addr).await;
    }

    handle_http(req, port, remote_addr).await
}

async fn handle_http(
    req: Request<hyper::body::Incoming>,
    port: u16,
    remote_addr: SocketAddr,
) -> Result<Response<BoxBody<Bytes, hyper::Error>>, hyper::Error> {
    let stream = match connect_to_backend(port).await {
        Ok(s) => s,
        Err(_) => return Ok(bad_gateway_response()),
    };

    let io = TokioIo::new(stream);
    let (mut sender, conn) = hyper::client::conn::http1::handshake(io).await?;
    tokio::spawn(conn);

    let (mut parts, body) = req.into_parts();

    let client_ip = remote_addr.ip().to_string();
    let host_val = parts
        .headers
        .get("host")
        .and_then(|v| v.to_str().ok())
        .unwrap_or("")
        .to_string();

    // Rewrite Host to localhost:<port> so backends like Vite accept the request
    parts.headers.insert(
        "host",
        HeaderValue::from_str(&format!("localhost:{}", port))
            .unwrap_or_else(|_| HeaderValue::from_static("localhost")),
    );

    // X-Forwarded-For: append (chain) existing value
    let xff = parts
        .headers
        .get("x-forwarded-for")
        .and_then(|v| v.to_str().ok())
        .map(|existing| format!("{}, {}", existing, client_ip))
        .unwrap_or_else(|| client_ip.clone());
    parts.headers.insert(
        "x-forwarded-for",
        HeaderValue::from_str(&xff).unwrap_or_else(|_| HeaderValue::from_static("unknown")),
    );

    // X-Forwarded-Proto: preserve existing or default to "http"
    if !parts.headers.contains_key("x-forwarded-proto") {
        parts
            .headers
            .insert("x-forwarded-proto", HeaderValue::from_static("http"));
    }

    // X-Forwarded-Host: preserve existing or set to Host header
    if !parts.headers.contains_key("x-forwarded-host") {
        parts.headers.insert(
            "x-forwarded-host",
            HeaderValue::from_str(&host_val)
                .unwrap_or_else(|_| HeaderValue::from_static("unknown")),
        );
    }

    // X-Forwarded-Port: preserve existing or extract from Host header
    if !parts.headers.contains_key("x-forwarded-port") {
        let fwd_port = host_val
            .split(':')
            .nth(1)
            .unwrap_or("80")
            .to_string();
        parts.headers.insert(
            "x-forwarded-port",
            HeaderValue::from_str(&fwd_port)
                .unwrap_or_else(|_| HeaderValue::from_static("80")),
        );
    }

    let req = Request::from_parts(parts, body);
    let mut response = sender.send_request(req).await?;

    response
        .headers_mut()
        .insert("x-portless", HeaderValue::from_static("1"));

    let (parts, body) = response.into_parts();
    Ok(Response::from_parts(parts, body.boxed()))
}

/// WebSocket proxy: connect to backend using http upgrade, forward the backend's
/// actual 101 headers (including Sec-WebSocket-Accept), then tunnel bidirectionally.
async fn handle_websocket(
    req: Request<hyper::body::Incoming>,
    port: u16,
    remote_addr: SocketAddr,
) -> Result<Response<BoxBody<Bytes, hyper::Error>>, hyper::Error> {
    use tokio::io::AsyncWriteExt;

    let client_ip = remote_addr.ip().to_string();
    let method = req.method().clone();
    let uri_path = req
        .uri()
        .path_and_query()
        .map(|p| p.as_str().to_string())
        .unwrap_or_else(|| "/".to_string());
    let host = req
        .headers()
        .get("host")
        .and_then(|v| v.to_str().ok())
        .unwrap_or("localhost")
        .to_string();

    // Build raw HTTP request to send to backend
    let mut req_str = format!("{} {} HTTP/1.1\r\n", method, uri_path);
    // Rewrite Host to localhost:<port> so backends like Vite accept the request;
    // skip the original Host header from the client.
    req_str.push_str(&format!("host: localhost:{}\r\n", port));
    for (name, value) in req.headers() {
        if name == "host" {
            continue;
        }
        if let Ok(v) = value.to_str() {
            req_str.push_str(&format!("{}: {}\r\n", name, v));
        }
    }
    // X-Forwarded-For: append
    let xff = req
        .headers()
        .get("x-forwarded-for")
        .and_then(|v| v.to_str().ok())
        .map(|existing| format!("{}, {}", existing, client_ip))
        .unwrap_or_else(|| client_ip.clone());
    req_str.push_str(&format!("x-forwarded-for: {}\r\n", xff));
    if req.headers().get("x-forwarded-host").is_none() {
        req_str.push_str(&format!("x-forwarded-host: {}\r\n", host));
    }
    if req.headers().get("x-forwarded-proto").is_none() {
        req_str.push_str("x-forwarded-proto: http\r\n");
    }
    req_str.push_str("\r\n");

    let mut backend = match connect_to_backend(port).await {
        Ok(s) => s,
        Err(_) => return Ok(bad_gateway_response()),
    };

    if backend.write_all(req_str.as_bytes()).await.is_err() {
        return Ok(bad_gateway_response());
    }

    // Read backend's HTTP response headers to get the real Sec-WebSocket-Accept etc.
    let (status_code, backend_headers) = match read_http_headers(&mut backend).await {
        Ok(r) => r,
        Err(_) => return Ok(bad_gateway_response()),
    };

    if status_code != 101 {
        return Ok(bad_gateway_response());
    }

    // Schedule the tunnel once hyper upgrades the client connection.
    let upgrade_fut = hyper::upgrade::on(req);
    tokio::spawn(async move {
        let upgraded_client = match upgrade_fut.await {
            Ok(u) => u,
            Err(e) => {
                eprintln!("WebSocket client upgrade error: {}", e);
                return;
            }
        };
        let mut client_io = TokioIo::new(upgraded_client);
        let _ = tokio::io::copy_bidirectional(&mut client_io, &mut backend).await;
    });

    // Build 101 response forwarding all headers received from backend.
    let mut resp_builder = Response::builder()
        .status(StatusCode::SWITCHING_PROTOCOLS)
        .header("x-portless", "1");
    for (name, value) in &backend_headers {
        resp_builder = resp_builder.header(name.as_str(), value.as_str());
    }

    Ok(resp_builder.body(empty_body()).unwrap())
}

/// Read HTTP response headers from a raw TCP stream byte-by-byte until \r\n\r\n.
async fn read_http_headers(
    stream: &mut TcpStream,
) -> anyhow::Result<(u16, Vec<(String, String)>)> {
    let mut buf: Vec<u8> = Vec::with_capacity(1024);
    let mut byte = [0u8; 1];
    loop {
        stream.read_exact(&mut byte).await?;
        buf.push(byte[0]);
        if buf.ends_with(b"\r\n\r\n") {
            break;
        }
        if buf.len() > 65536 {
            return Err(anyhow::anyhow!("Response headers too large"));
        }
    }
    let text = String::from_utf8_lossy(&buf);
    let mut lines = text.lines();
    let status_line = lines.next().unwrap_or("");
    let status: u16 = status_line
        .split_whitespace()
        .nth(1)
        .and_then(|s| s.parse().ok())
        .unwrap_or(200);
    let mut headers = Vec::new();
    for line in lines {
        if line.is_empty() {
            break;
        }
        if let Some((name, value)) = line.split_once(": ") {
            headers.push((name.to_lowercase(), value.to_string()));
        }
    }
    Ok((status, headers))
}

fn extract_hostname(headers: &hyper::HeaderMap) -> Option<String> {
    headers
        .get("host")
        .and_then(|v| v.to_str().ok())
        .map(|host| host.split(':').next().unwrap_or(host).to_ascii_lowercase())
        .filter(|h| !h.is_empty())
}

fn not_found_response(
    routes: &[Route],
    hostname: &str,
    proxy_port: u16,
) -> Response<BoxBody<Bytes, hyper::Error>> {
    let safe_host = escape_html(hostname);
    let routes_html = if routes.is_empty() {
        "<p><em>No apps running.</em></p>".to_string()
    } else {
        let items: String = routes
            .iter()
            .map(|r| {
                let safe_h = escape_html(&r.hostname);
                let url = crate::utils::format_url(&r.hostname, proxy_port);
                let safe_url = escape_html(&url);
                format!(
                    "<li><a href=\"{}\">{}</a> - localhost:{}</li>",
                    safe_url, safe_h, r.port
                )
            })
            .collect();
        format!("<ul>{}</ul>", items)
    };

    let name_hint = hostname.replace(".localhost", "");
    let body = format!(
        r#"<html>
  <head><title>portless - Not Found</title></head>
  <body style="font-family: system-ui; padding: 40px; max-width: 600px; margin: 0 auto;">
    <h1>Not Found</h1>
    <p>No app registered for <strong>{safe_host}</strong></p>
    <h2>Active apps:</h2>
    {routes_html}
    <p>Start an app with: <code>portless {name} your-command</code></p>
  </body>
</html>"#,
        safe_host = safe_host,
        routes_html = routes_html,
        name = escape_html(&name_hint),
    );

    let mut resp = Response::new(
        Full::new(Bytes::from(body))
            .map_err(|e| match e {})
            .boxed(),
    );
    *resp.status_mut() = StatusCode::NOT_FOUND;
    resp.headers_mut().insert(
        "content-type",
        HeaderValue::from_static("text/html; charset=utf-8"),
    );
    resp.headers_mut()
        .insert("x-portless", HeaderValue::from_static("1"));
    resp
}

fn bad_request_response(msg: &'static str) -> Response<BoxBody<Bytes, hyper::Error>> {
    let mut resp = Response::new(
        Full::new(Bytes::from(msg))
            .map_err(|e| match e {})
            .boxed(),
    );
    *resp.status_mut() = StatusCode::BAD_REQUEST;
    resp.headers_mut()
        .insert("content-type", HeaderValue::from_static("text/plain"));
    resp.headers_mut()
        .insert("x-portless", HeaderValue::from_static("1"));
    resp
}

fn bad_gateway_response() -> Response<BoxBody<Bytes, hyper::Error>> {
    let body = "Bad Gateway: the target app may not be running.";
    let mut resp = Response::new(
        Full::new(Bytes::from(body))
            .map_err(|e| match e {})
            .boxed(),
    );
    *resp.status_mut() = StatusCode::BAD_GATEWAY;
    resp.headers_mut()
        .insert("content-type", HeaderValue::from_static("text/plain"));
    resp.headers_mut()
        .insert("x-portless", HeaderValue::from_static("1"));
    resp
}

fn empty_body() -> BoxBody<Bytes, hyper::Error> {
    Empty::<Bytes>::new().map_err(|e| match e {}).boxed()
}

/// Connect to a local backend port, trying IPv4 first then IPv6.
/// Node.js 18+ on macOS resolves `localhost` to ::1 (IPv6) by default,
/// so hardcoding 127.0.0.1 fails when the backend only binds to ::1.
async fn connect_to_backend(port: u16) -> std::io::Result<TcpStream> {
    if let Ok(s) = TcpStream::connect(("127.0.0.1", port)).await {
        return Ok(s);
    }
    TcpStream::connect(("::1", port)).await
}
