use std::net::SocketAddr;
use std::path::{Path, PathBuf};

use serde::Deserialize;

use crate::network::HomeNetwork;

pub const DEFAULT_PORT: u16 = 8816;

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Config {
    pub token: String,
    pub model: PathBuf,
    #[serde(default = "default_listen")]
    pub listen: SocketAddr,
    /// Found on PATH when unset. Services start with a minimal PATH, so `eco init`
    /// writes the absolute path it finds.
    pub ffmpeg: Option<PathBuf>,
    pub home_network: Option<HomeNetwork>,
}

/// `[home_network]` as written in the file; parsed into a `HomeNetwork` on load.
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub struct HomeNetworkConfig {
    pub subnet: String,
    pub router_mac: String,
}

fn default_listen() -> SocketAddr {
    SocketAddr::from(([0, 0, 0, 0], DEFAULT_PORT))
}

impl Config {
    pub fn load(path: &Path) -> Result<Config, String> {
        let text = std::fs::read_to_string(path).map_err(|e| match e.kind() {
            std::io::ErrorKind::NotFound => format!("{} not found. Run `eco init` first.", tilde(path)),
            _ => format!("{}: {e}", tilde(path)),
        })?;
        let config: Config = toml::from_str(&text).map_err(|e| format!("{}: {e}", tilde(path)))?;
        if config.token.len() < 16 {
            return Err(format!("{}: The token must be at least 16 characters.", tilde(path)));
        }
        Ok(config)
    }

    pub fn ffmpeg(&self) -> Result<PathBuf, String> {
        self.ffmpeg
            .clone()
            .or_else(find_ffmpeg)
            .ok_or_else(|| "ffmpeg not found. Install it, or set `ffmpeg` in the config.".into())
    }
}

/// `$XDG_CONFIG_HOME/eco/config.toml`, else `~/.config/eco/config.toml`, on
/// macOS too, as most command-line tools do.
pub fn default_path() -> PathBuf {
    config_home().join("eco/config.toml")
}

/// `$XDG_CONFIG_HOME`, else `~/.config`.
pub fn config_home() -> PathBuf {
    xdg_dir("XDG_CONFIG_HOME", ".config")
}

/// Where `eco init` downloads models: `$XDG_DATA_HOME/eco`, else `~/.local/share/eco`.
pub fn data_dir() -> PathBuf {
    xdg_dir("XDG_DATA_HOME", ".local/share").join("eco")
}

fn xdg_dir(var: &str, fallback: &str) -> PathBuf {
    match std::env::var_os(var).map(PathBuf::from) {
        // The XDG spec says to ignore relative values.
        Some(dir) if dir.is_absolute() => dir,
        _ => home().join(fallback),
    }
}

pub fn home() -> PathBuf {
    std::env::var_os("HOME").map(PathBuf::from).expect("HOME is not set")
}

pub fn find_ffmpeg() -> Option<PathBuf> {
    let path = std::env::var_os("PATH").unwrap_or_default();
    let common = ["/opt/homebrew/bin", "/usr/local/bin", "/usr/bin"].map(PathBuf::from);
    std::env::split_paths(&path).chain(common).map(|dir| dir.join("ffmpeg")).find(|p| p.is_file())
}

/// Shortens paths under the home directory to `~/...` for display.
pub fn tilde(path: &Path) -> String {
    match path.strip_prefix(home()) {
        Ok(rest) => format!("~/{}", rest.display()),
        Err(_) => path.display().to_string(),
    }
}
