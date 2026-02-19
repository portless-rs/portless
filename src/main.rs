mod proxy;
mod routes;
mod types;
mod utils;

use anyhow::{anyhow, Result};
use clap::{Parser, Subcommand};
use colored::Colorize;
use std::env;
use std::fs;
use std::io::{BufRead, Write as IoWrite};
use std::path::PathBuf;
use std::process::Stdio;
use std::thread;
use std::time::Duration;
use tokio::process::Command as TokioCommand;
use tokio::signal::unix::{signal, SignalKind};

use routes::RouteStore;
use types::Route;
use utils::{
    discover_state, find_free_port, find_pid_on_port, format_url, is_proxy_running, parse_hostname,
    resolve_state_dir, signal_exit_code, DEFAULT_PROXY_PORT, PRIVILEGED_PORT_THRESHOLD,
};

#[derive(Parser)]
#[command(
    name = "portless",
    version = env!("CARGO_PKG_VERSION"),
    author = env!("CARGO_PKG_AUTHORS"),
    about = "Replace port numbers with stable .localhost URLs for local development — built with Rust, only 1MB",
    long_about = None,
    help_template = "{name} {version}\n{about}\n\n{usage-heading} {usage}\n\n{all-args}"
)]
struct Cli {
    #[command(subcommand)]
    command: Option<Commands>,

    /// App name (used when running an app directly: portless <name> <cmd...>)
    #[arg(index = 1, required = false)]
    name: Option<String>,

    /// Command and arguments to run
    #[arg(
        index = 2,
        trailing_var_arg = true,
        required = false,
        allow_hyphen_values = true
    )]
    cmd: Vec<String>,

    /// Proxy port (default: 1355, or $PORTLESS_PORT)
    #[arg(short = 'p', long, global = true)]
    port: Option<u16>,
}

#[derive(Subcommand)]
enum Commands {
    /// Manage the proxy server
    Proxy {
        #[command(subcommand)]
        action: ProxyAction,
    },
    /// List active routes
    List,
}

#[derive(Subcommand)]
enum ProxyAction {
    /// Start the proxy server
    Start {
        /// Run in foreground instead of as a daemon
        #[arg(long)]
        foreground: bool,
    },
    /// Stop the running proxy server
    Stop,
}

#[tokio::main]
async fn main() -> Result<()> {
    let cli = Cli::parse();

    // Security: block npx / pnpm dlx (unsafe with sudo)
    let is_npx = env::var("npm_command").as_deref() == Ok("exec")
        && env::var("npm_lifecycle_event").is_err();
    let is_pnpm_dlx = env::var("PNPM_SCRIPT_SRC_DIR").is_ok()
        && env::var("npm_lifecycle_event").is_err();
    if is_npx || is_pnpm_dlx {
        eprintln!(
            "{}",
            "Error: portless should not be run via npx or pnpm dlx.".red()
        );
        eprintln!("{}", "Install globally instead:".blue());
        eprintln!("{}", "  cargo install portless".cyan());
        std::process::exit(1);
    }

    match cli.command {
        Some(Commands::Proxy { action }) => {
            let proxy_port = cli
                .port
                .or_else(|| {
                    env::var("PORTLESS_PORT")
                        .ok()
                        .and_then(|v| v.parse().ok())
                })
                .unwrap_or(DEFAULT_PROXY_PORT);
            let state_dir = resolve_state_dir(proxy_port);

            match action {
                ProxyAction::Start { foreground } => {
                    cmd_proxy_start(proxy_port, state_dir, foreground).await
                }
                ProxyAction::Stop => cmd_proxy_stop(proxy_port, state_dir).await,
            }
        }
        Some(Commands::List) => {
            let (state_dir, proxy_port) = discover_state();
            cmd_list(state_dir, proxy_port)
        }
        None => {
            // Skip portless if PORTLESS=0 or PORTLESS=skip
            let portless_env = env::var("PORTLESS").unwrap_or_default();
            let name = cli.name.unwrap_or_default();
            let cmd = cli.cmd;

            if (portless_env == "0" || portless_env.eq_ignore_ascii_case("skip"))
                && !name.is_empty()
                && name != "proxy"
            {
                return run_passthrough(&cmd);
            }

            if name.is_empty() {
                eprintln!("{}", "Usage: portless <name> <command...>".yellow());
                eprintln!("       portless proxy start|stop");
                eprintln!("       portless list");
                std::process::exit(1);
            }
            if cmd.is_empty() {
                eprintln!(
                    "{}",
                    format!("Usage: portless {} <command...>", name).yellow()
                );
                std::process::exit(1);
            }

            let (state_dir, proxy_port) = discover_state();
            cmd_run(name, cmd, proxy_port, state_dir).await
        }
    }
}

async fn cmd_proxy_start(port: u16, state_dir: PathBuf, foreground: bool) -> Result<()> {
    if is_proxy_running(port) {
        if foreground {
            // Foreground mode used internally by daemon fork; exit silently if already running
            return Ok(());
        }
        let sudo_prefix = if port < PRIVILEGED_PORT_THRESHOLD { "sudo " } else { "" };
        println!("{}", format!("Proxy is already running on port {}.", port).yellow());
        println!(
            "{}",
            format!(
                "To restart: portless proxy stop && {}portless proxy start",
                sudo_prefix
            )
            .blue()
        );
        return Ok(());
    }

    // Privileged port requires root
    #[cfg(unix)]
    if port < PRIVILEGED_PORT_THRESHOLD {
        let uid = unsafe { nix::libc::getuid() };
        if uid != 0 {
            eprintln!("{}", format!("Error: Port {} requires sudo.", port).red());
            eprintln!("{}", "Either run with sudo:".blue());
            eprintln!("{}", format!("  sudo portless proxy start -p {}", port).cyan());
            eprintln!("{}", "Or use the default port (no sudo needed):".blue());
            eprintln!("{}", "  portless proxy start".cyan());
            std::process::exit(1);
        }
    }

    if foreground {
        println!(
            "{}",
            format!("\nportless proxy v{}\n", env!("CARGO_PKG_VERSION")).bold().blue()
        );
        proxy::run_proxy(port, state_dir).await
    } else {
        daemonize_proxy(port, state_dir)
    }
}

fn daemonize_proxy(port: u16, state_dir: PathBuf) -> Result<()> {
    fs::create_dir_all(&state_dir)?;
    let log_path = state_dir.join("proxy.log");

    let log_file = fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(&log_path)?;
    let log_file2 = log_file.try_clone()?;

    let exe = env::current_exe()?;
    let mut cmd = std::process::Command::new(&exe);
    cmd.args(["proxy", "start", "--foreground"]);
    cmd.args(["-p", &port.to_string()]);
    cmd.stdin(Stdio::null());
    cmd.stdout(Stdio::from(log_file));
    cmd.stderr(Stdio::from(log_file2));

    #[cfg(unix)]
    {
        use std::os::unix::process::CommandExt;
        unsafe {
            cmd.pre_exec(|| {
                nix::libc::setsid();
                Ok(())
            });
        }
    }

    let child = cmd.spawn()?;
    drop(child); // detach

    // Wait for proxy to be ready (20 × 250ms = 5s)
    for _ in 0..utils::WAIT_FOR_PROXY_MAX_ATTEMPTS {
        thread::sleep(Duration::from_millis(utils::WAIT_FOR_PROXY_INTERVAL_MS));
        if is_proxy_running(port) {
            println!("{}", format!("Proxy started on port {}", port).green());
            return Ok(());
        }
    }

    eprintln!("{}", "Proxy failed to start (timed out waiting for it to listen).".red());
    eprintln!("{}", "Try starting the proxy in the foreground to see the error:".blue());
    let sudo_prefix = if port < PRIVILEGED_PORT_THRESHOLD { "sudo " } else { "" };
    eprintln!(
        "{}",
        format!("  {}portless proxy start --foreground", sudo_prefix).cyan()
    );
    if log_path.exists() {
        eprintln!("{}", format!("Logs: {}", log_path.display()).dimmed());
    }
    std::process::exit(1);
}

async fn cmd_proxy_stop(port: u16, state_dir: PathBuf) -> Result<()> {
    let pid_path = state_dir.join("proxy.pid");
    let port_path = state_dir.join("proxy.port");
    let needs_sudo = port < PRIVILEGED_PORT_THRESHOLD;
    let sudo_hint = if needs_sudo { "sudo " } else { "" };

    if !pid_path.exists() {
        if is_proxy_running(port) {
            println!("{}", format!("PID file is missing but port {} is still in use.", port).yellow());
            if let Some(pid) = find_pid_on_port(port) {
                match nix::sys::signal::kill(
                    nix::unistd::Pid::from_raw(pid as i32),
                    nix::sys::signal::Signal::SIGTERM,
                ) {
                    Ok(_) => {
                        let _ = fs::remove_file(&port_path);
                        println!("{}", format!("Killed process {}. Proxy stopped.", pid).green());
                    }
                    Err(nix::errno::Errno::EPERM) => {
                        eprintln!("{}", "Permission denied. The proxy was started with sudo.".red());
                        eprintln!("{}", "Stop it with:".blue());
                        eprintln!("{}", "  sudo portless proxy stop".cyan());
                    }
                    Err(e) => {
                        eprintln!("{}", format!("Failed to stop proxy: {}", e).red());
                    }
                }
            } else {
                eprintln!("{}", "Could not identify the process on this port.".red());
                eprintln!("{}", "Try manually:".blue());
                eprintln!("{}", format!("  {}lsof -ti tcp:{} | xargs kill", sudo_hint, port).cyan());
            }
        } else {
            println!("{}", "Proxy is not running.".yellow());
        }
        return Ok(());
    }

    let pid_str = fs::read_to_string(&pid_path)?;
    let pid: i32 = match pid_str.trim().parse() {
        Ok(p) => p,
        Err(_) => {
            eprintln!("{}", "Corrupted PID file. Removing it.".red());
            let _ = fs::remove_file(&pid_path);
            return Ok(());
        }
    };

    // Check if the process is still alive
    if nix::sys::signal::kill(nix::unistd::Pid::from_raw(pid), None).is_err() {
        println!("{}", "Proxy process is no longer running. Cleaning up stale files.".yellow());
        let _ = fs::remove_file(&pid_path);
        let _ = fs::remove_file(&port_path);
        return Ok(());
    }

    // Verify the process is actually the portless proxy (PID may be recycled)
    if !is_proxy_running(port) {
        println!(
            "{}",
            format!(
                "PID file exists but port {} is not listening. The PID may have been recycled.",
                port
            )
            .yellow()
        );
        println!("{}", "Removing stale PID file.".yellow());
        let _ = fs::remove_file(&pid_path);
        return Ok(());
    }

    match nix::sys::signal::kill(
        nix::unistd::Pid::from_raw(pid),
        nix::sys::signal::Signal::SIGTERM,
    ) {
        Ok(_) => {
            let _ = fs::remove_file(&pid_path);
            let _ = fs::remove_file(&port_path);
            println!("{}", "Proxy stopped.".green());
        }
        Err(nix::errno::Errno::EPERM) => {
            eprintln!("{}", "Permission denied. The proxy was started with sudo.".red());
            eprintln!("{}", "Stop it with:".blue());
            eprintln!("{}", format!("  {}portless proxy stop", sudo_hint).cyan());
        }
        Err(e) => {
            eprintln!("{}", format!("Failed to stop proxy: {}", e).red());
            eprintln!("{}", "Check if the process is still running:".blue());
            eprintln!("{}", format!("  lsof -ti tcp:{}", port).cyan());
        }
    }

    Ok(())
}

fn cmd_list(state_dir: PathBuf, proxy_port: u16) -> Result<()> {
    let store = RouteStore::new(state_dir)?;
    let routes = store.load(false)?;

    if routes.is_empty() {
        println!("{}", "No active routes.".yellow());
        println!("{}", "Start an app with: portless <name> <command>".dimmed());
        return Ok(());
    }

    println!("{}", "\nActive routes:\n".bold().blue());
    for route in &routes {
        let url = format_url(&route.hostname, proxy_port);
        println!(
            "  {}  {}  {}  {}",
            url.cyan(),
            "->".dimmed(),
            format!("localhost:{}", route.port).white(),
            format!("(pid {})", route.pid).dimmed()
        );
    }
    println!();

    Ok(())
}

async fn cmd_run(
    name: String,
    cmd: Vec<String>,
    proxy_port: u16,
    state_dir: PathBuf,
) -> Result<()> {
    let hostname = parse_hostname(&name)?;
    let app_url = format_url(&hostname, proxy_port);

    println!("{}", format!("\nportless v{}\n", env!("CARGO_PKG_VERSION")).bold().blue());
    println!("{}", format!("-- {} (auto-resolves to 127.0.0.1)", hostname).dimmed());

    // Auto-start proxy if not running
    if !is_proxy_running(proxy_port) {
        let needs_sudo = proxy_port < PRIVILEGED_PORT_THRESHOLD;

        if needs_sudo {
            // Privileged port: check if stdin is a TTY for interactive prompt
            if !atty_check() {
                eprintln!("{}", "Proxy is not running.".red());
                eprintln!("{}", "Start the proxy first (requires sudo for this port):".blue());
                eprintln!("{}", format!("  sudo portless proxy start -p {}", proxy_port).cyan());
                eprintln!("{}", "Or use the default port (no sudo needed):".blue());
                eprintln!("{}", "  portless proxy start".cyan());
                std::process::exit(1);
            }

            let answer = prompt("Proxy not running. Start it? [Y/n/skip] ");
            let answer = answer.trim().to_ascii_lowercase();

            if answer == "n" || answer == "no" {
                println!("{}", "Cancelled.".dimmed());
                std::process::exit(0);
            }

            if answer == "s" || answer == "skip" {
                println!("{}", "Skipping proxy, running command directly...\n".dimmed());
                return run_passthrough(&cmd);
            }

            println!("{}", "Starting proxy (requires sudo)...".yellow());
            let exe = env::current_exe()?;
            let status = std::process::Command::new("sudo")
                .arg(&exe)
                .args(["proxy", "start"])
                .status()?;
            if !status.success() {
                eprintln!("{}", "Failed to start proxy.".red());
                eprintln!("{}", "Try starting it manually:".blue());
                eprintln!("{}", "  sudo portless proxy start".cyan());
                std::process::exit(1);
            }
        } else {
            println!("{}", "Starting proxy...".yellow());
            let exe = env::current_exe()?;
            let status = std::process::Command::new(&exe)
                .args(["proxy", "start"])
                .status()?;
            if !status.success() {
                eprintln!("{}", "Failed to start proxy.".red());
                eprintln!("{}", "Try starting it manually:".blue());
                eprintln!("{}", "  portless proxy start".cyan());
                std::process::exit(1);
            }
        }

        if !is_proxy_running(proxy_port) {
            eprintln!("{}", "Proxy failed to start (timed out).".red());
            std::process::exit(1);
        }

        println!("{}", "Proxy started in background".green());
    } else {
        println!("{}", "-- Proxy is running".dimmed());
    }

    let port = find_free_port()?;
    println!("{}", format!("-- Using port {}", port).green());

    let store = RouteStore::new(state_dir.clone())?;
    let my_pid = std::process::id();

    store.add(Route {
        hostname: hostname.clone(),
        port,
        pid: my_pid,
    })?;

    println!("{}", format!("\n  -> {}\n", app_url).cyan().bold());
    println!(
        "{}",
        format!("Running: PORT={} {}\n", port, cmd.join(" ")).dimmed()
    );

    let program = cmd[0].clone();
    let args = &cmd[1..];

    let mut child = TokioCommand::new(&program)
        .args(args)
        .env("PORT", port.to_string())
        .spawn()
        .map_err(|e| {
            if e.kind() == std::io::ErrorKind::NotFound {
                anyhow!(
                    "Failed to run command: {}\nIs \"{}\" installed and in your PATH?",
                    e,
                    program
                )
            } else {
                anyhow!("Failed to spawn '{}': {}", program, e)
            }
        })?;

    let child_pid = child.id().unwrap_or(0);

    let mut sigint = signal(SignalKind::interrupt())?;
    let mut sigterm = signal(SignalKind::terminate())?;

    let exit_status = tokio::select! {
        status = child.wait() => {
            status.ok()
        }
        _ = sigint.recv() => {
            forward_signal(child_pid, nix::sys::signal::Signal::SIGINT);
            let _ = store.remove(&hostname);
            shutdown_proxy_if_idle(&store, &state_dir);
            std::process::exit(signal_exit_code(nix::sys::signal::Signal::SIGINT));
        }
        _ = sigterm.recv() => {
            forward_signal(child_pid, nix::sys::signal::Signal::SIGTERM);
            let _ = store.remove(&hostname);
            shutdown_proxy_if_idle(&store, &state_dir);
            std::process::exit(signal_exit_code(nix::sys::signal::Signal::SIGTERM));
        }
    };

    let _ = store.remove(&hostname);
    shutdown_proxy_if_idle(&store, &state_dir);

    if let Some(status) = exit_status {
        let code = status.code().unwrap_or(1);
        if code != 0 {
            std::process::exit(code);
        }
    }

    Ok(())
}

/// Stop the background proxy if no routes remain after an app exits.
fn shutdown_proxy_if_idle(store: &RouteStore, state_dir: &std::path::Path) {
    let remaining = store.load(true).unwrap_or_default();
    if !remaining.is_empty() {
        return;
    }

    let pid_path = state_dir.join("proxy.pid");
    let port_path = state_dir.join("proxy.port");

    let Ok(pid_str) = fs::read_to_string(&pid_path) else {
        return;
    };
    let Ok(pid) = pid_str.trim().parse::<i32>() else {
        return;
    };

    if nix::sys::signal::kill(
        nix::unistd::Pid::from_raw(pid),
        nix::sys::signal::Signal::SIGTERM,
    )
    .is_ok()
    {
        let _ = fs::remove_file(&pid_path);
        let _ = fs::remove_file(&port_path);
        println!("{}", "Proxy stopped (no active routes).".dimmed());
    }
}

fn forward_signal(pid: u32, sig: nix::sys::signal::Signal) {
    if pid == 0 {
        return;
    }
    let _ = nix::sys::signal::kill(nix::unistd::Pid::from_raw(pid as i32), sig);
}

fn run_passthrough(cmd: &[String]) -> Result<()> {
    if cmd.is_empty() {
        return Err(anyhow!("No command specified"));
    }
    let status = std::process::Command::new(&cmd[0])
        .args(&cmd[1..])
        .status()
        .map_err(|e| {
            if e.kind() == std::io::ErrorKind::NotFound {
                anyhow!(
                    "Failed to run command: {}\nIs \"{}\" installed and in your PATH?",
                    e,
                    cmd[0]
                )
            } else {
                anyhow!("Failed to run '{}': {}", cmd[0], e)
            }
        })?;

    if !status.success() {
        std::process::exit(status.code().unwrap_or(1));
    }
    Ok(())
}

fn prompt(question: &str) -> String {
    print!("{}", question);
    std::io::stdout().flush().ok();
    let stdin = std::io::stdin();
    let mut line = String::new();
    let _ = stdin.lock().read_line(&mut line);
    line.trim().to_string()
}

fn atty_check() -> bool {
    // Check if stdin is a TTY
    use std::os::unix::io::AsRawFd;
    unsafe { nix::libc::isatty(std::io::stdin().as_raw_fd()) == 1 }
}
