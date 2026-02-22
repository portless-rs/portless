# portless

> A Rust port of [vercel-labs/portless](https://github.com/vercel-labs/portless) — built with Rust, only 1MB.

Replace port numbers with stable `.localhost` URLs for local development.

Instead of juggling `localhost:3000`, `localhost:8080`, etc., **portless** gives each app a clean, memorable URL:

```
http://myapp.localhost:1355
http://api.localhost:1355
http://dashboard.localhost:1355
```

No `/etc/hosts` edits. No browser config. No conflicts when port numbers change.

## How it works

portless runs a lightweight HTTP/WebSocket reverse proxy in the background. When you start an app with `portless <name> <command>`, it:

1. Picks a free port in the range 4000–4999 and injects it as `$PORT` into your app.
2. Registers a route: `<name>.localhost` → `localhost:<port>`.
3. The background proxy forwards traffic for that hostname to the app's actual port.

Routes are stored in a JSON file (`~/.portless/routes.json`) and cleaned up automatically when the app exits.

## Installation

**Homebrew (macOS):**

```bash
brew tap portless-rs/portless https://github.com/portless-rs/portless
brew install portless
```

**npm:**

```bash
npm install -g portless-rs
```

**Local build (for testing):**

```bash
cargo build --release
export PATH="/Users/lusons/Documents/workspace/portless-rs/target/release:$PATH"
```

## Usage

### Start an app

```bash
portless <name> <command...>
```

`<name>` becomes the subdomain. The command is run with `$PORT` set to a free port.

**Examples:**

```bash
portless myapp npm run dev
portless backend python manage.py runserver $PORT
```

Your app is then available at `http://myapp.localhost:1355` (or `http://myapp.localhost` if the proxy runs on port 80).

The proxy is started automatically in the background if it isn't already running.

### List active routes

```bash
portless list
```

Output:

```
Active routes:

  http://myapp.localhost:1355  ->  localhost:4213  (pid 12345)
  http://api.localhost:1355    ->  localhost:4872  (pid 12346)
```

### Manage the proxy

> **Note:** In most cases you don't need to manage the proxy manually — it starts automatically when you run `portless` and stops automatically when all tunnels are closed.

```bash
# Start the proxy in the background (daemon)
portless proxy start

# Stop the proxy
portless proxy stop

# Run in the foreground (useful for debugging)
portless proxy start --foreground
```

## Framework support

No configuration changes are needed — just wrap your existing dev command with `portless <name>`.

portless automatically injects the right flags for frameworks that don't read `$PORT`, and sets `HOST=127.0.0.1` for all processes so the proxy can always reach them.

| Framework | How it works |
|---|---|
| **Vite** (incl. SvelteKit) | `--port`, `--strictPort`, `--host 127.0.0.1` injected automatically |
| **React Router** | `--port`, `--strictPort`, `--host 127.0.0.1` injected automatically |
| **Astro** | `--port`, `--host 127.0.0.1` injected automatically |
| **Angular** (`ng`) | `--port`, `--host 127.0.0.1` injected automatically |
| **Next.js** | reads `$PORT` natively — no flags needed |
| **Nuxt** | reads `$PORT` natively — no flags needed |
| **Express / Node.js** | reads `$PORT` natively — no flags needed |

**Examples:**

```diff
- "dev": "vite"                        # http://localhost:5173
+ "dev": "portless myapp vite"         # http://myapp.localhost:1355

- "dev": "next dev"                    # http://localhost:3000
+ "dev": "portless myapp next dev"     # http://myapp.localhost:1355

- "dev": "astro dev"                   # http://localhost:4321
+ "dev": "portless myapp astro dev"    # http://myapp.localhost:1355
```

## Proxy port

The default proxy port is **1355** (no `sudo` required). You can change it via:

- The `-p` / `--port` flag: `portless proxy start -p 80`
- The `PORTLESS_PORT` environment variable: `PORTLESS_PORT=8080 portless ...`

> **Port 80** requires `sudo` on most systems:
> ```bash
> sudo portless proxy start -p 80
> sudo portless proxy stop -p 80
> ```

## Skipping portless

Set `PORTLESS=0` (or `PORTLESS=skip`) to bypass portless and run the command directly. This is useful in CI or when you want to opt out without modifying your scripts:

```bash
PORTLESS=0 portless myapp npm run dev
# equivalent to: npm run dev
```

## Environment variables

| Variable                               | Description                                         | Default         |
|----------------------------------------|-----------------------------------------------------|-----------------|
| `PORTLESS_PORT`                        | Proxy port                                          | `1355`          |
| `PORTLESS_STATE_DIR`                   | Directory for PID file, route list, and proxy log   | `~/.portless`   |
| `PORTLESS`                             | Set to `0` or `skip` to bypass portless             | —               |
| `PORT`                                 | Injected into child processes — the assigned port   | auto-assigned   |
| `HOST`                                 | Injected into child processes — always `127.0.0.1`  | `127.0.0.1`     |
| `__VITE_ADDITIONAL_SERVER_ALLOWED_HOSTS` | Injected so Vite accepts `.localhost` requests    | `.localhost`    |

## State files

portless keeps its state in `~/.portless/` (or `/tmp/portless/` when the proxy runs on a privileged port):

| File            | Description                                      |
|-----------------|--------------------------------------------------|
| `routes.json`   | Active hostname → port mappings                  |
| `proxy.pid`     | PID of the background proxy process              |
| `proxy.port`    | Port the proxy is listening on                   |
| `proxy.log`     | stdout/stderr from the background proxy          |

## WebSocket support

portless transparently proxies WebSocket connections, forwarding the backend's `101 Switching Protocols` response (including `Sec-WebSocket-Accept`) and then tunneling traffic bidirectionally.

## Forwarded headers

The proxy adds standard forwarding headers to every upstream request:

| Header               | Value                                      |
|----------------------|--------------------------------------------|
| `X-Forwarded-For`    | Client IP (appended to any existing value) |
| `X-Forwarded-Proto`  | `http`                                     |
| `X-Forwarded-Host`   | Original `Host` header                     |
| `X-Forwarded-Port`   | Port from the `Host` header                |
| `X-Portless`         | `1` (on all responses)                     |

## Requirements

- Rust 2024 edition (rustc 1.85+)
- macOS or Linux (uses Unix signals and `lsof`)
- No system-level DNS changes required — `.localhost` subdomains resolve to `127.0.0.1` natively in modern browsers and operating systems

## License

MIT
