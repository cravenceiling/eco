//! Runs `eco serve` as a per-user background service: launchd on macOS,
//! systemd on Linux.

use std::io::{Read, Write};
use std::net::{IpAddr, Ipv4Addr, Ipv6Addr, SocketAddr, TcpStream};
use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::{Duration, Instant};

use crate::config::{Config, tilde};
use crate::network::reachable_addr;
use crate::print_row;

enum State {
    NotInstalled,
    Stopped,
    Running(u32),
}

pub fn install(config_path: &Path) -> Result<(), String> {
    // Fail here, not in a crash-looping service, if the config is broken.
    let config = Config::load(config_path)?;
    config.ffmpeg()?;
    let exe = std::env::current_exe()
        .and_then(|p| p.canonicalize())
        .map_err(|e| format!("Could not find the eco executable: {e}"))?;
    if exe.components().any(|c| c.as_os_str() == "target") {
        eprintln!(
            "Note: the service will run {}, inside a build folder. `cargo install` gives it a stable location.\n",
            exe.display()
        );
    }
    let config_path = config_path.canonicalize().map_err(|e| format!("{}: {e}", tilde(config_path)))?;

    platform::install(&exe, &config_path)?;
    // Loading the model takes a few seconds.
    let deadline = Instant::now() + Duration::from_secs(30);
    while !probe(config.listen) {
        if Instant::now() > deadline {
            return Err(format!("The service is installed but isn't answering. Logs: {}", platform::logs_hint()));
        }
        std::thread::sleep(Duration::from_millis(250));
    }

    let address = reachable_addr(config.listen).unwrap_or(config.listen);
    println!("eco is running on {address} and starts when you log in.");
    println!("Logs: {}", platform::logs_hint());
    #[cfg(target_os = "linux")]
    println!("To keep eco running while you're logged out: loginctl enable-linger $USER");
    println!("\nNext: eco pair");
    Ok(())
}

pub fn uninstall() -> Result<(), String> {
    platform::uninstall()?;
    println!("Service removed. Your config is untouched.");
    Ok(())
}

pub fn restart() -> Result<(), String> {
    platform::restart()?;
    println!("Service restarted.");
    Ok(())
}

/// Prints the service state. Returns the exit code: 0 answering, 1 running but
/// not answering, 3 not installed or not running.
pub fn status(config_path: &Path) -> i32 {
    let pid = match platform::state() {
        State::NotInstalled => {
            print_row("Service", "not installed. Run `eco service install`.");
            return 3;
        }
        State::Stopped => {
            print_row("Service", format!("installed but not running. Logs: {}", platform::logs_hint()));
            return 3;
        }
        State::Running(pid) => pid,
    };
    print_row("Service", format!("running (pid {pid})"));
    let config = match Config::load(config_path) {
        Ok(config) => config,
        Err(e) => {
            print_row("Config", e);
            return 1;
        }
    };
    let answering = probe(config.listen);
    let address = reachable_addr(config.listen).unwrap_or(config.listen);
    print_row("Server", format!("{} on {address}", if answering { "answering" } else { "not answering" }));
    print_row(
        "Network",
        match config.home_network {
            Some(home) if home.is_current() => "home; other devices are answered",
            Some(_) => "away from home; other devices are refused",
            None => "no home check; other devices are answered on every network",
        },
    );
    print_row("Logs", platform::logs_hint());
    if answering { 0 } else { 1 }
}

/// Whether eco answers `/health` at `listen` on this machine.
pub fn probe(listen: SocketAddr) -> bool {
    let ip = match listen.ip() {
        IpAddr::V4(ip) if ip.is_unspecified() => IpAddr::V4(Ipv4Addr::LOCALHOST),
        IpAddr::V6(ip) if ip.is_unspecified() => IpAddr::V6(Ipv6Addr::LOCALHOST),
        ip => ip,
    };
    let Ok(mut stream) = TcpStream::connect_timeout(&SocketAddr::new(ip, listen.port()), Duration::from_secs(1)) else {
        return false;
    };
    let _ = stream.set_read_timeout(Some(Duration::from_secs(2)));
    let request = "GET /health HTTP/1.1\r\nHost: localhost\r\nConnection: close\r\n\r\n";
    let mut response = [0u8; 12];
    stream.write_all(request.as_bytes()).is_ok()
        && stream.read_exact(&mut response).is_ok()
        && response.starts_with(b"HTTP/1.1 200")
}

fn run(program: &str, args: &[&str]) -> Result<String, String> {
    let output = Command::new(program).args(args).output().map_err(|e| format!("Could not run {program}: {e}"))?;
    if output.status.success() {
        Ok(String::from_utf8_lossy(&output.stdout).into_owned())
    } else {
        let stderr = String::from_utf8_lossy(&output.stderr);
        Err(format!("`{program} {}` failed: {}", args.join(" "), stderr.trim()))
    }
}

/// Writes a service definition, creating its directory.
fn write_file(path: &Path, contents: &str) -> Result<(), String> {
    std::fs::create_dir_all(path.parent().unwrap())
        .and_then(|()| std::fs::write(path, contents))
        .map_err(|e| format!("{}: {e}", tilde(path)))
}

#[cfg(target_os = "macos")]
mod platform {
    use super::*;
    use crate::config::home;

    const LABEL: &str = "io.github.cravenceiling.eco";

    pub fn logs_hint() -> String {
        tilde(&log_path())
    }

    fn plist_path() -> PathBuf {
        home().join("Library/LaunchAgents").join(format!("{LABEL}.plist"))
    }

    fn log_path() -> PathBuf {
        home().join("Library/Logs/eco/eco.log")
    }

    fn domain() -> Result<String, String> {
        Ok(format!("gui/{}", run("id", &["-u"])?.trim()))
    }

    /// The service as launchctl names it.
    fn target() -> Result<String, String> {
        Ok(format!("{}/{LABEL}", domain()?))
    }

    fn xml_escape(value: &str) -> String {
        value.replace('&', "&amp;").replace('<', "&lt;").replace('>', "&gt;")
    }

    pub fn install(exe: &Path, config: &Path) -> Result<(), String> {
        let log = log_path();
        std::fs::create_dir_all(log.parent().unwrap()).map_err(|e| format!("{}: {e}", tilde(&log)))?;
        let [exe, config, log] = [exe, config, &log].map(|p| xml_escape(&p.to_string_lossy()));
        let plist = format!(
            r#"<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0">
<dict>
  <key>Label</key><string>{LABEL}</string>
  <key>ProgramArguments</key>
  <array><string>{exe}</string><string>serve</string><string>--config</string><string>{config}</string></array>
  <key>RunAtLoad</key><true/>
  <key>KeepAlive</key><true/>
  <key>StandardOutPath</key><string>{log}</string>
  <key>StandardErrorPath</key><string>{log}</string>
</dict>
</plist>
"#
        );
        let path = plist_path();
        write_file(&path, &plist)?;

        // Replace a running copy so a reinstall picks up a new binary or config.
        let _ = run("launchctl", &["bootout", &target()?]);
        let (domain, path) = (domain()?, path.to_string_lossy());
        // launchd rejects a bootstrap while the old instance is still shutting down.
        let mut attempts = 0;
        loop {
            match run("launchctl", &["bootstrap", &domain, &path]) {
                Ok(_) => return Ok(()),
                Err(_) if attempts < 20 => {
                    attempts += 1;
                    std::thread::sleep(Duration::from_millis(250));
                }
                Err(e) => return Err(e),
            }
        }
    }

    pub fn uninstall() -> Result<(), String> {
        let _ = run("launchctl", &["bootout", &target()?]);
        let path = plist_path();
        match std::fs::remove_file(&path) {
            Ok(()) => Ok(()),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Err("The service is not installed.".into()),
            Err(e) => Err(format!("{}: {e}", tilde(&path))),
        }
    }

    pub fn restart() -> Result<(), String> {
        run("launchctl", &["kickstart", "-k", &target()?]).map(drop)
    }

    pub fn state() -> State {
        if !plist_path().exists() {
            return State::NotInstalled;
        }
        let output = target().and_then(|target| run("launchctl", &["print", &target])).unwrap_or_default();
        match output.lines().find_map(|line| line.trim().strip_prefix("pid = ")?.parse().ok()) {
            Some(pid) => State::Running(pid),
            None => State::Stopped,
        }
    }
}

#[cfg(target_os = "linux")]
mod platform {
    use super::*;
    use crate::config::config_home;

    const UNIT: &str = "eco.service";

    pub fn logs_hint() -> String {
        "journalctl --user -u eco -f".into()
    }

    fn unit_path() -> PathBuf {
        config_home().join("systemd/user").join(UNIT)
    }

    /// Quotes one ExecStart argument: systemd splits on spaces and expands `%` and `$`.
    fn systemd_quote(value: &str) -> String {
        let escaped = value.replace('\\', "\\\\").replace('"', "\\\"").replace('%', "%%").replace('$', "$$");
        format!("\"{escaped}\"")
    }

    pub fn install(exe: &Path, config: &Path) -> Result<(), String> {
        let unit = format!(
            "[Unit]\nDescription=eco speech-to-text server\nAfter=network-online.target\n\n\
             [Service]\nExecStart={} serve --config {}\nRestart=always\nRestartSec=2\n\n\
             [Install]\nWantedBy=default.target\n",
            systemd_quote(&exe.to_string_lossy()),
            systemd_quote(&config.to_string_lossy()),
        );
        write_file(&unit_path(), &unit)?;
        run("systemctl", &["--user", "daemon-reload"])?;
        run("systemctl", &["--user", "enable", UNIT])?;
        run("systemctl", &["--user", "restart", UNIT]).map(drop)
    }

    pub fn uninstall() -> Result<(), String> {
        let path = unit_path();
        if !path.exists() {
            return Err("The service is not installed.".into());
        }
        let _ = run("systemctl", &["--user", "disable", "--now", UNIT]);
        std::fs::remove_file(&path).map_err(|e| format!("{}: {e}", tilde(&path)))?;
        run("systemctl", &["--user", "daemon-reload"]).map(drop)
    }

    pub fn restart() -> Result<(), String> {
        run("systemctl", &["--user", "restart", UNIT]).map(drop)
    }

    pub fn state() -> State {
        if !unit_path().exists() {
            return State::NotInstalled;
        }
        let pid = run("systemctl", &["--user", "show", "--property", "MainPID", "--value", UNIT])
            .ok()
            .and_then(|out| out.trim().parse().ok());
        match pid {
            Some(pid) if pid != 0 => State::Running(pid),
            _ => State::Stopped,
        }
    }
}
