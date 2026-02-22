# CLAUDE.md

This file provides guidance to Claude Code (claude.ai/code) when working with code in this repository.

## Overview

portless is a Rust-based HTTP/WebSocket reverse proxy that replaces port numbers with stable `.localhost` URLs for local development. It's a Rust port of [vercel-labs/portless](https://github.com/vercel-labs/portless), designed to be lightweight (only 1MB).

**Core concept**: When you run `portless myapp npm run dev`, it:
1. Allocates a free port (4000-4999) and injects it as `$PORT`
2. Registers `myapp.localhost` → `localhost:<port>` in `routes.json`
3. A background proxy (port 1355 by default) forwards traffic to the appropriate backend

## Build and Development

### Building

```bash
# Debug build
cargo build

# Release build
cargo build --release

# Local build for testing (adds to PATH)
cargo build --release
export PATH="$(pwd)/target/release:$PATH"
```

### Testing

```bash
# Run the binary locally
./target/release/portless --help

# Test with a real app (Vite example in test01/)
cd test01
../target/release/portless test01 npm run dev
# Then visit http://test01.localhost:1355

# Test proxy commands
./target/release/portless proxy start
./target/release/portless list
./target/release/portless proxy stop
```

### Linting

```bash
# Check code
cargo check

# Run clippy
cargo clippy

# Format code
cargo fmt
```

## Architecture

### Module Structure

The codebase is split into five modules:

- **`main.rs`** (370 lines): CLI interface, app runner, daemon management, signal handling
  - Parses commands via `clap`
  - Handles `portless <name> <cmd>`, `portless proxy start/stop`, `portless list`
  - Auto-starts the proxy if not running
  - Detects framework from command basename, injects `--port`/`--host` flags for Vite, React Router, Astro, Angular
  - Registers routes, spawns child processes with `$PORT`, `HOST`, `__VITE_ADDITIONAL_SERVER_ALLOWED_HOSTS` set
  - Forwards SIGINT/SIGTERM to children and cleans up routes
  - Auto-stops proxy when last route exits (via `shutdown_proxy_if_idle`)

- **`proxy.rs`** (480 lines): HTTP/WebSocket reverse proxy server
  - Runs on port 1355 (or custom port via `-p` / `PORTLESS_PORT`)
  - Background route-reloader: re-reads `routes.json` every 100ms
  - Idle-shutdown: waits 5s after routes disappear, then exits (grace period: 10s)
  - HTTP: rewrites `Host` to `localhost:<port>`, adds `X-Forwarded-*` headers
  - WebSocket: raw TCP upgrade handling, forwards backend's 101 response, bidirectional tunnel
  - Backend connection: tries IPv4 (127.0.0.1) first, then IPv6 (::1) for Node.js 18+ on macOS

- **`routes.rs`** (145 lines): Route storage with file locking
  - Stores routes in `~/.portless/routes.json` (or `/tmp/portless/` for privileged ports)
  - File-based mutex using `routes.lock` directory (stale lock detection: 10s)
  - `load()`: filters out dead PIDs via `nix::sys::signal::kill(pid, None)`
  - `add()`: acquires lock, removes old entry for hostname, appends new route
  - `remove()`: acquires lock, filters out hostname, saves

- **`types.rs`** (9 lines): Core data structures
  - `Route { hostname: String, port: u16, pid: u32 }`

- **`utils.rs`** (230 lines): Utilities
  - `find_free_port()`: random selection in 4000-4999, falls back to sequential scan
  - `is_proxy_running(port)`: sends HEAD request, checks for `X-Portless: 1` header
  - `parse_hostname()`: normalizes input to `.localhost` format, validates DNS label rules
  - `discover_state()`: checks `~/.portless` then `/tmp/portless` for running proxy
  - `resolve_state_dir(port)`: picks state dir based on port (privileged → `/tmp/portless`)
  - `find_pid_on_port(port)`: uses `lsof -ti tcp:<port> -sTCP:LISTEN`

### State Files

Located in `~/.portless/` (user-level) or `/tmp/portless/` (privileged ports):

- `routes.json`: Active hostname → port mappings (array of `Route` objects)
- `routes.lock`: Directory-based lock for `routes.json` writes
- `proxy.pid`: PID of background proxy
- `proxy.port`: Port the proxy is listening on
- `proxy.log`: stdout/stderr from background proxy

### Key Algorithms

**Auto-start flow** (main.rs:382-444):
- Check `is_proxy_running(proxy_port)`
- If privileged port: prompt user or fail if non-interactive
- Spawn `portless proxy start` (with `sudo` if needed)
- Poll for 5s until proxy is ready

**Idle shutdown** (proxy.rs:71-97):
- Wait 10s grace period after startup
- Watch `routes_tx` channel (updated every 100ms by route-reloader)
- When routes disappear, arm 5s deadline
- If routes come back before deadline, cancel and loop
- Otherwise, `std::process::exit(0)`

**WebSocket proxying** (proxy.rs:247-339):
- Build raw HTTP upgrade request, rewrite `Host` to `localhost:<port>`
- Send to backend, read response headers byte-by-byte until `\r\n\r\n`
- Forward backend's 101 response (including `Sec-WebSocket-Accept`)
- Spawn bidirectional tunnel with `tokio::io::copy_bidirectional`

### Signal Handling

The app runner (main.rs:485-518) uses `tokio::select!` to wait for:
- Child process exit → remove route, shutdown proxy if idle, exit with child's status
- SIGINT → forward to child, cleanup, exit with 130 (128 + 2)
- SIGTERM → forward to child, cleanup, exit with 143 (128 + 15)

Exit codes follow standard Unix convention: 128 + signal number (see `signal_exit_code` in utils.rs:22).

## Release Process

Releases are automated via `.github/workflows/release.yml`:

1. **Trigger**: Push a tag matching `v*` (e.g., `v0.1.12`)
2. **Build**: Cross-compile for macOS (arm64/x64) and Linux (x64/arm64)
3. **GitHub Release**: Upload `.tar.gz` archives and SHA256 checksums
4. **Homebrew**: Auto-update `Formula/portless.rb` with new version/URLs/hashes
5. **npm**: Publish to npm registry as `portless-rs`

The workflow syncs the version from the git tag into `Cargo.toml` at build time, so the crate version doesn't need to be manually updated before tagging.

### Version Management

- **Git tag** is the source of truth (e.g., `v0.1.12`)
- **Cargo.toml**: Version is overwritten from tag during GitHub Actions build
- **npm/package.json**: Updated by workflow after successful release
- **Formula/portless.rb**: Updated by workflow with new URLs and SHA256 hashes

To release a new version:
```bash
git tag v0.1.12
git push origin v0.1.12
# Wait for GitHub Actions to build, publish, and update files
```

## Environment Variables

- `PORTLESS_PORT`: Override default proxy port (1355)
- `PORTLESS_STATE_DIR`: Override state directory (`~/.portless`)
- `PORTLESS`: Set to `0` or `skip` to bypass portless (runs command directly)
- `PORT`: Injected into child processes by portless (auto-assigned from 4000-4999)
- `HOST`: Injected as `127.0.0.1` so frameworks bind IPv4 and the proxy can reach them
- `__VITE_ADDITIONAL_SERVER_ALLOWED_HOSTS`: Injected as `.localhost` so Vite's dev server accepts `.localhost` requests

## Security Notes

- **npx/pnpm dlx blocking**: main.rs:81-93 prevents running via `npx` or `pnpm dlx` (security risk with `sudo`)
- **Privileged ports** (< 1024): Require `sudo` to start proxy, state stored in `/tmp/portless/`
- **PID validation**: Routes with dead PIDs are filtered out on every load
- **Stale lock cleanup**: Locks older than 10s are considered stale and removed

## Dependencies

Key dependencies (see Cargo.toml):
- **tokio**: Async runtime (features: full)
- **hyper** + **hyper-util**: HTTP server/client (v1)
- **clap**: CLI argument parsing (derive feature)
- **serde** + **serde_json**: Route serialization
- **nix**: Unix signals, process management (signal, process features)
- **dirs**: Home directory resolution
- **rand**: Random port selection
- **colored**: Terminal color output

## Requirements

- Rust 2024 edition (rustc 1.85+)
- macOS or Linux (uses Unix signals and `lsof`)
- No system-level DNS changes required (`.localhost` resolves to 127.0.0.1 natively)
