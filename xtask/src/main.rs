//! `cargo dev`: builds auth_server, game_server and game_client, then runs
//! them together in this console, each output line tagged with its process.
//! Development only -- in production the two servers run on a host and every
//! player starts their own client.
//!
//! Closing every game window stops the servers; Ctrl+C stops everything,
//! after the servers have saved (they get the same Ctrl+C).

use std::io::{BufRead, BufReader, Read, Write};
use std::net::TcpStream;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, ExitCode, Stdio};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::thread;
use std::time::{Duration, Instant};

const USAGE: &str = "\
usage:
  cargo dev                 auth server + game server + one game window
  cargo dev --clients 2     ...with two game windows (log in with two accounts)
  cargo dev --no-client     auth + game server only (e.g. a client on another PC)";

const BINARIES: [&str; 3] = ["auth_server", "game_server", "game_client"];
/// `auth_server`'s own default; `ARPG_AUTH_ADDR` overrides both.
const DEFAULT_AUTH_ADDR: &str = "127.0.0.1:5001";
/// How long the servers get to save and stop after Ctrl+C before they're
/// killed -- the game server allows its last saves 8 s.
const SERVER_STOP_GRACE: Duration = Duration::from_secs(10);

struct Process {
    name: String,
    child: Child,
}

fn main() -> ExitCode {
    let args: Vec<String> = std::env::args().skip(1).collect();
    if args.iter().any(|a| a == "-h" || a == "--help") {
        println!("{USAGE}");
        return ExitCode::SUCCESS;
    }
    let clients = match parse_client_count(&args) {
        Ok(clients) => clients,
        Err(message) => {
            eprintln!("[dev] {message}\n{USAGE}");
            return ExitCode::from(2);
        }
    };
    let root = Path::new(env!("CARGO_MANIFEST_DIR")).parent().expect("xtask sits inside the workspace");

    let running = already_running();
    if !running.is_empty() {
        eprintln!("[dev] already running: {}", running.join(", "));
        eprintln!("[dev] a running .exe can't be rebuilt and keeps its port busy -- stop it first:");
        for exe in &running {
            eprintln!("        taskkill /IM {exe} /F");
        }
        return ExitCode::FAILURE;
    }

    if !build(root) {
        return ExitCode::FAILURE;
    }

    let bin_dir = std::env::var_os("CARGO_TARGET_DIR").map_or_else(|| root.join("target"), PathBuf::from).join("debug");
    let client_names: Vec<String> = match clients {
        1 => vec!["client".to_string()],
        n => (1..=n).map(|i| format!("client{i}")).collect(),
    };
    let width = client_names.iter().map(String::len).chain(["auth".len(), "server".len()]).max().unwrap_or(0);

    match clients {
        0 => println!("[dev] auth + server only -- Ctrl+C stops them"),
        n => println!("[dev] auth + server + {n} game window(s) -- closing the game stops everything, so does Ctrl+C"),
    }

    // Ctrl+C reaches every process in this console. Without a handler this
    // runner would die first, and a server printing its last lines into
    // the closed pipe would crash before it finished saving.
    let stop = Arc::new(AtomicBool::new(false));
    {
        let stop = stop.clone();
        if let Err(e) = ctrlc::set_handler(move || stop.store(true, Ordering::SeqCst)) {
            eprintln!("[dev] can't catch Ctrl+C ({e}) -- it will stop the servers without saving");
        }
    }

    let mut servers = Vec::new();
    let mut windows = Vec::new();
    let code = match supervise(root, &bin_dir, &client_names, width, &mut servers, &mut windows, &stop) {
        Ok(code) => code,
        Err(message) => {
            eprintln!("[dev] {message} -- stopping everything");
            ExitCode::FAILURE
        }
    };
    if stop.load(Ordering::SeqCst) {
        println!("[dev] Ctrl+C -- waiting for the servers to save and stop");
        wait_for_exit(&mut servers, SERVER_STOP_GRACE);
    }
    for process in servers.iter_mut().chain(windows.iter_mut()) {
        let _ = process.child.kill();
        let _ = process.child.wait();
    }
    code
}

fn parse_client_count(args: &[String]) -> Result<usize, String> {
    let mut clients = 1;
    let mut args = args.iter();
    while let Some(arg) = args.next() {
        match arg.as_str() {
            "--no-client" => clients = 0,
            "--clients" => {
                let value = args.next().ok_or("--clients needs a number")?;
                clients = value.parse().map_err(|_| format!("--clients: '{value}' isn't a number"))?;
            }
            other => return Err(format!("unknown argument '{other}'")),
        }
    }
    Ok(clients)
}

/// Copies of our binaries already running (another `cargo dev`, or one left
/// over from an earlier session) keep their .exe locked -- the build can't
/// replace it -- and hold their ports. Windows-only: elsewhere a running
/// binary can be replaced.
fn already_running() -> Vec<String> {
    if !cfg!(windows) {
        return Vec::new();
    }
    BINARIES
        .iter()
        .map(|bin| format!("{bin}.exe"))
        .filter(|exe| {
            Command::new("tasklist")
                .args(["/FI", &format!("IMAGENAME eq {exe}"), "/FO", "CSV", "/NH"])
                .output()
                .is_ok_and(|out| String::from_utf8_lossy(&out.stdout).contains(&format!("\"{exe}\"")))
        })
        .collect()
}

/// One `cargo build` for all three, so shared crates compile once. Drops the
/// package variables `cargo run` set for this runner (`CARGO_PKG_NAME`,
/// `CARGO_MANIFEST_DIR`, ...): some build scripts (ring's) rerun whenever
/// those change, which would rebuild ring/rustls/ureq on every switch
/// between `cargo dev` and a plain `cargo build`.
fn build(root: &Path) -> bool {
    let mut command = Command::new(std::env::var_os("CARGO").unwrap_or_else(|| "cargo".into()));
    command.current_dir(root).arg("build");
    for bin in BINARIES {
        command.args(["-p", bin]);
    }
    for (key, _) in std::env::vars_os() {
        let name = key.to_string_lossy();
        if name.starts_with("CARGO_PKG_")
            || name.starts_with("CARGO_MANIFEST_")
            || matches!(name.as_ref(), "CARGO_CRATE_NAME" | "CARGO_BIN_NAME" | "CARGO_PRIMARY_PACKAGE")
        {
            command.env_remove(&key);
        }
    }
    matches!(command.status(), Ok(status) if status.success())
}

/// Starts everything, then waits until it's time to stop: Ctrl+C or all
/// game windows closed (`Ok`), or a server exiting on its own (`Err`, e.g.
/// a crash or a port already in use). The caller stops whatever is still
/// running.
fn supervise(
    root: &Path,
    bin_dir: &Path,
    client_names: &[String],
    width: usize,
    servers: &mut Vec<Process>,
    windows: &mut Vec<Process>,
    stop: &AtomicBool,
) -> Result<ExitCode, String> {
    let exe = |bin: &str| bin_dir.join(format!("{bin}{}", std::env::consts::EXE_SUFFIX));

    servers.push(start("auth", &exe("auth_server"), root, width)?);
    let auth_addr = std::env::var("ARPG_AUTH_ADDR").unwrap_or_else(|_| DEFAULT_AUTH_ADDR.to_string());
    wait_for_port(&auth_addr, &mut servers[0].child)?;
    servers.push(start("server", &exe("game_server"), root, width)?);
    for name in client_names {
        windows.push(start(name, &exe("game_client"), root, width)?);
    }

    loop {
        thread::sleep(Duration::from_millis(200));
        // Before checking the servers: after Ctrl+C they stop on purpose.
        if stop.load(Ordering::SeqCst) {
            return Ok(ExitCode::SUCCESS);
        }
        for server in servers.iter_mut() {
            if let Ok(Some(status)) = server.child.try_wait() {
                return Err(format!("{} stopped ({status})", server.name));
            }
        }
        if !client_names.is_empty() {
            windows.retain_mut(|window| !matches!(window.child.try_wait(), Ok(Some(_))));
            if windows.is_empty() {
                println!("[dev] game window closed -- stopping the servers");
                return Ok(ExitCode::SUCCESS);
            }
        }
    }
}

/// Waits up to `grace` for every process to exit on its own.
fn wait_for_exit(processes: &mut [Process], grace: Duration) {
    let deadline = Instant::now() + grace;
    while Instant::now() < deadline {
        if processes.iter_mut().all(|process| matches!(process.child.try_wait(), Ok(Some(_)))) {
            return;
        }
        thread::sleep(Duration::from_millis(100));
    }
    eprintln!("[dev] servers still running after {}s -- stopping them", grace.as_secs());
}

fn start(name: &str, exe: &Path, root: &Path, width: usize) -> Result<Process, String> {
    let mut child = Command::new(exe)
        .current_dir(root)
        // The game server honors the debug tools' level-up and teleport
        // only when told to -- in development, unless set otherwise.
        .env("ARPG_DEBUG_COMMANDS", std::env::var("ARPG_DEBUG_COMMANDS").unwrap_or_else(|_| "1".to_string()))
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .map_err(|e| format!("couldn't start {}: {e}", exe.display()))?;
    let label = format!("{name:<width$} | ");
    if let Some(stdout) = child.stdout.take() {
        forward(stdout, label.clone());
    }
    if let Some(stderr) = child.stderr.take() {
        forward(stderr, label);
    }
    Ok(Process { name: name.to_string(), child })
}

/// Copies a child's output into this console line by line, prefixed with
/// its label. Ends on its own when the child exits.
fn forward(stream: impl Read + Send + 'static, label: String) {
    thread::spawn(move || {
        let mut reader = BufReader::new(stream);
        let mut line = Vec::new();
        while matches!(reader.read_until(b'\n', &mut line), Ok(n) if n > 0) {
            let text = String::from_utf8_lossy(&line);
            let _ = writeln!(std::io::stdout().lock(), "{label}{}", text.trim_end());
            line.clear();
        }
    });
}

/// Waits until auth accepts connections, so the login screen never races
/// it. Gives up waiting (but keeps going) after 15 s, in case it listens
/// somewhere else.
fn wait_for_port(addr: &str, auth: &mut Child) -> Result<(), String> {
    let deadline = Instant::now() + Duration::from_secs(15);
    while Instant::now() < deadline {
        if TcpStream::connect(addr).is_ok() {
            return Ok(());
        }
        if let Ok(Some(status)) = auth.try_wait() {
            return Err(format!("auth stopped ({status})"));
        }
        thread::sleep(Duration::from_millis(100));
    }
    eprintln!("[dev] auth isn't answering on {addr} yet -- starting the rest anyway");
    Ok(())
}
