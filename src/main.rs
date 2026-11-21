use std::path::PathBuf;
use std::process::ExitCode;

use hermes_native::model::Config;
use hermes_native::start_preview;
use hermes_native::ui::{self, Launch};

fn main() -> ExitCode {
    match arguments() {
        Ok(options) => {
            ui::launch(options);
            ExitCode::SUCCESS
        }
        Err(()) => ExitCode::FAILURE,
    }
}

fn arguments() -> Result<Launch, ()> {
    dotenvy::from_path(".env").ok();
    let mut trusted_tunnel = false;
    let mut preview = false;
    for argument in std::env::args().skip(1) {
        if !known_argument(&argument, &mut trusted_tunnel, &mut preview) {
            eprintln!("Unknown argument. Use --help.");
            return Err(());
        }
    }
    let config = if preview {
        preview_config()?
    } else {
        Config::from_env(trusted_tunnel)
    };
    Ok(Launch {
        config,
        preview_dir: preview.then(preview_dir),
    })
}

fn known_argument(argument: &str, trusted: &mut bool, preview: &mut bool) -> bool {
    match argument {
        "--trusted-tunnel" => *trusted = true,
        "--preview" => *preview = true,
        "--help" | "-h" => {
            print_help();
            std::process::exit(0);
        }
        "--version" | "-V" => {
            println!("hermes-native {}", env!("CARGO_PKG_VERSION"));
            std::process::exit(0);
        }
        _ => return false,
    }
    true
}

fn print_help() {
    println!(
        "Hermes Native\n\nUsage: hermes-native [--trusted-tunnel] [--preview]\n\nSet HERMES_GATEWAY_URL, HERMES_USER and HERMES_PASSWORD in the environment or project .env.\n--trusted-tunnel allows HTTP only on loopback/Tailscale, inside an encrypted tunnel you trust.\n--preview starts a loopback gateway and walks the main views.\nCredentials stay in memory. Public HTTP and invalid TLS certificates are rejected."
    );
}

fn preview_config() -> Result<Result<Config, String>, ()> {
    let port = start_preview().map_err(|error| {
        eprintln!("{error}");
    })?;
    eprintln!("preview gateway on 127.0.0.1:{port}");
    let url = url::Url::parse(&format!("http://127.0.0.1:{port}"))
        .map_err(|_| "The preview gateway URL was not usable.".to_string());
    Ok(url.and_then(|url| {
        Config::validate_url(&url, true)?;
        Ok(Config {
            url,
            username: "preview".into(),
            password: "preview".into(),
        })
    }))
}

fn preview_dir() -> PathBuf {
    std::env::var_os("HERMES_PREVIEW_DIR")
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from("/tmp/hermes-native-preview"))
}
