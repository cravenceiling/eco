use std::io::{Read, Write};
use std::path::{Path, PathBuf};

use crate::config::{self, DEFAULT_PORT, HomeNetworkConfig, tilde};
use crate::network::{self, format_mac};
use crate::print_row;

const MODEL_FILE: &str = "parakeet-unified-en-0.6b-Q8_0.gguf";
// Pinned to a commit so a resumed download can't mix two versions of the file.
const MODEL_URL: &str = "https://huggingface.co/handy-computer/parakeet-unified-en-0.6b-gguf/resolve/843b1b175ace04f5a89d379c79e2518c0d065b25/parakeet-unified-en-0.6b-Q8_0.gguf";

pub struct Options {
    pub force: bool,
    pub no_home_check: bool,
    pub model: Option<PathBuf>,
}

pub fn run(path: &Path, options: Options) -> Result<(), String> {
    if path.exists() && !options.force {
        return Err(format!(
            "{} already exists. Pass --force to replace it; that also replaces the API key your devices use.",
            tilde(path)
        ));
    }

    let home = if options.no_home_check { None } else { Some(detect_home()?) };
    let (model, model_source) = match options.model {
        // The service runs from another working directory, so the path must be absolute.
        Some(model) if model.is_file() => {
            (model.canonicalize().map_err(|e| format!("{}: {e}", model.display()))?, "--model")
        }
        Some(model) => return Err(format!("{} does not exist.", model.display())),
        None => find_or_download_model()?,
    };
    let ffmpeg = config::find_ffmpeg();
    let token = random_token()?;

    write_private(path, &render_config(&token, &model, ffmpeg.as_deref(), home.as_ref()))?;

    println!("Wrote {}\n", tilde(path));
    print_row("API key", "generated; `eco pair` shows it");
    print_row("Model", format!("{} ({model_source})", tilde(&model)));
    match &ffmpeg {
        Some(ffmpeg) => print_row("ffmpeg", ffmpeg.display()),
        None => {
            print_row("ffmpeg", "not found. Install it (brew install ffmpeg, apt install ffmpeg) before starting eco.")
        }
    }
    print_row("Listen", format!("0.0.0.0:{DEFAULT_PORT}"));
    match &home {
        Some(home) => {
            print_row("Home network", format!("{}, router {}", home.subnet, home.router_mac));
            print_row("", "Other devices are answered only while this machine is on this network.");
        }
        None => print_row("Home network", "off; eco answers on every network"),
    }
    println!("\nNext: eco service install");
    Ok(())
}

fn detect_home() -> Result<HomeNetworkConfig, String> {
    let undetected =
        |what: &str| format!("Could not detect {what}. Run this on your home network, or pass --no-home-check.");
    let (ip, netmask) = network::lan_address().ok_or_else(|| undetected("this machine's network address"))?;
    let router_mac = network::router_mac().ok_or_else(|| undetected("your router"))?;
    Ok(HomeNetworkConfig { subnet: network::subnet_of(ip, netmask), router_mac: format_mac(router_mac) })
}

/// Prefers the model Hex already downloaded, then one from an earlier `eco init`,
/// and otherwise downloads it.
fn find_or_download_model() -> Result<(PathBuf, &'static str), String> {
    let hex = config::home().join("Library/Application Support/hex/models").join(MODEL_FILE);
    if hex.is_file() {
        return Ok((hex, "Hex's copy"));
    }
    let ours = config::data_dir().join("models").join(MODEL_FILE);
    if ours.is_file() {
        return Ok((ours, "downloaded earlier"));
    }

    println!("Downloading the Parakeet English model (about 730 MB) to {}", tilde(&ours));
    let dir = ours.parent().unwrap();
    std::fs::create_dir_all(dir).map_err(|e| format!("{}: {e}", tilde(dir)))?;
    let partial = ours.with_extension("gguf.part");
    let status = std::process::Command::new("curl")
        .args(["--fail", "--location", "--progress-bar", "--continue-at", "-", "--output"])
        .arg(&partial)
        .arg(MODEL_URL)
        .status();
    match status {
        Ok(status) if status.success() => {}
        Ok(_) => {
            return Err(format!(
                "Download failed. Run `eco init` again to resume, or download {MODEL_URL} and pass --model."
            ));
        }
        Err(e) => return Err(format!("Could not run curl ({e}). Download {MODEL_URL} and pass --model.")),
    }
    std::fs::rename(&partial, &ours).map_err(|e| format!("{}: {e}", tilde(&ours)))?;
    Ok((ours, "downloaded"))
}

fn random_token() -> Result<String, String> {
    let mut bytes = [0u8; 24];
    std::fs::File::open("/dev/urandom")
        .and_then(|mut f| f.read_exact(&mut bytes))
        .map_err(|e| format!("Could not read /dev/urandom: {e}"))?;
    Ok(bytes.iter().map(|b| format!("{b:02x}")).collect())
}

fn toml_string(value: &str) -> String {
    toml::Value::String(value.to_owned()).to_string()
}

fn render_config(token: &str, model: &Path, ffmpeg: Option<&Path>, home: Option<&HomeNetworkConfig>) -> String {
    let ffmpeg = match ffmpeg {
        Some(path) => format!("ffmpeg = {}", toml_string(&path.to_string_lossy())),
        None => "# ffmpeg = \"/path/to/ffmpeg\"".to_owned(),
    };
    let home = match home {
        Some(home) => format!(
            "[home_network]\nsubnet = {}\nrouter_mac = {}\n",
            toml_string(&home.subnet),
            toml_string(&home.router_mac)
        ),
        None => "# [home_network]\n# subnet = \"192.168.1.0/24\"\n# router_mac = \"aa:bb:cc:dd:ee:ff\"\n".to_owned(),
    };
    format!(
        r#"# eco configuration. After editing, run `eco service restart`.

# Clients send this as "Authorization: Bearer <token>". Keep it secret.
token = {token}

# A transcribe.cpp GGUF speech model.
model = {model}

# Address and port to listen on. Use a specific address, such as a Tailscale IP,
# to limit where eco can be reached.
listen = "0.0.0.0:{DEFAULT_PORT}"

# Found on PATH when unset. Background services get a minimal PATH, so an
# absolute path is safest.
{ffmpeg}

# Answer other devices only while this machine is on this network. Café and
# office networks often reuse the same subnet, so the router's hardware address
# is checked too. Remove this section to answer on every network.
{home}"#,
        token = toml_string(token),
        model = toml_string(&model.to_string_lossy()),
    )
}

/// Writes `contents` readable only by the current user, in a directory only
/// they can enter, since the file holds the API key.
fn write_private(path: &Path, contents: &str) -> Result<(), String> {
    use std::os::unix::fs::{DirBuilderExt, OpenOptionsExt, PermissionsExt};
    let dir = path.parent().ok_or("The config path has no parent directory.")?;
    std::fs::DirBuilder::new().recursive(true).mode(0o700).create(dir).map_err(|e| format!("{}: {e}", tilde(dir)))?;
    let mut file = std::fs::OpenOptions::new()
        .write(true)
        .create(true)
        .truncate(true)
        .mode(0o600)
        .open(path)
        .map_err(|e| format!("{}: {e}", tilde(path)))?;
    // `mode` only applies when the file is created; tighten an existing one too.
    file.set_permissions(std::fs::Permissions::from_mode(0o600))
        .and_then(|()| file.write_all(contents.as_bytes()))
        .map_err(|e| format!("{}: {e}", tilde(path)))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::Config;

    #[test]
    fn rendered_config_parses_back() {
        let home = HomeNetworkConfig { subnet: "192.168.1.0/24".into(), router_mac: "aa:bb:cc:dd:ee:ff".into() };
        let model = Path::new("/models/a \"quoted\" name.gguf");
        let text = render_config("abc", model, Some(Path::new("/usr/bin/ffmpeg")), Some(&home));
        let config: Config = toml::from_str(&text).unwrap();
        assert_eq!(config.token, "abc");
        assert_eq!(config.model, model);
        assert!(config.home_network.is_some());

        let config: Config = toml::from_str(&render_config("abc", model, None, None)).unwrap();
        assert!(config.ffmpeg.is_none() && config.home_network.is_none());
    }
}
