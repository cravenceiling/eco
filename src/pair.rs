use std::path::Path;

use qrcode::QrCode;
use qrcode::render::unicode::Dense1x2;

use crate::config::Config;
use crate::network::reachable_addr;
use crate::{print_row, service};

/// Prints what a phone's dictation app needs to reach eco. Android dictation
/// keyboards can't import settings from a link yet, so the endpoint and the key
/// each get their own QR code to scan and paste.
pub fn run(config_path: &Path, show_qr: bool) -> Result<(), String> {
    let config = Config::load(config_path)?;
    let address = reachable_addr(config.listen)
        .ok_or("Could not find this machine's network address. Is it connected to a network?")?;
    let endpoint = format!("http://{address}/v1");

    if !service::probe(config.listen) {
        eprintln!("Note: eco isn't answering right now. Start it with `eco service install`.\n");
    }
    if let Some(home) = config.home_network
        && !home.is_current()
    {
        eprintln!("Note: this machine isn't on its home network, so eco refuses other devices until it is.\n");
    }

    println!("Enter these in your dictation app, or any OpenAI-compatible transcription client:\n");
    print_row("Endpoint", &endpoint);
    print_row("API key", &config.token);
    print_row("Model", "any value; eco ignores it");
    println!();
    if show_qr {
        print_qr("Endpoint", &endpoint)?;
        print_qr("API key", &config.token)?;
    }
    println!("Tip: give this machine a fixed address in your router's settings, so the endpoint keeps working.");
    Ok(())
}

fn print_qr(label: &str, data: &str) -> Result<(), String> {
    let code = QrCode::new(data).map_err(|e| e.to_string())?;
    // Inverted so the code reads as dark-on-light on dark terminal themes too.
    let rendered = code.render::<Dense1x2>().dark_color(Dense1x2::Light).light_color(Dense1x2::Dark).build();
    println!("{label}\n{rendered}\n");
    Ok(())
}
