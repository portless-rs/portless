# portless

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
brew tap lusons/portless
brew install portless
```

**npm:**

```bash
npm install -g portless-rs
```

**cargo:**

```bash
cargo install portless
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
portless api cargo run
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

```bash
# Start the proxy in the background (daemon)
portless proxy start

# Stop the proxy
portless proxy stop

# Run in the foreground (useful for debugging)
portless proxy start --foreground
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

| Variable             | Description                                         | Default         |
|----------------------|-----------------------------------------------------|-----------------|
| `PORTLESS_PORT`      | Proxy port                                          | `1355`          |
| `PORTLESS_STATE_DIR` | Directory for PID file, route list, and proxy log   | `~/.portless`   |
| `PORTLESS`           | Set to `0` or `skip` to bypass portless             | —               |
| `PORT`               | Injected into child processes by portless           | auto-assigned   |

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
