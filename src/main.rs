use adw::prelude::*;
use hermes_native::{model::Config, ui};

fn main() -> gtk::glib::ExitCode {
    dotenvy::from_path(".env").ok();
    let mut trusted_tunnel = false;
    for argument in std::env::args().skip(1) {
        match argument.as_str() {
            "--trusted-tunnel" => trusted_tunnel = true,
            "--help" | "-h" => {
                println!(
                    "Hermes Native\n\nUsage: hermes-native [--trusted-tunnel]\n\nSet HERMES_GATEWAY_URL, HERMES_USER and HERMES_PASSWORD in the environment or project .env.\n--trusted-tunnel allows HTTP only on loopback/Tailscale, inside an encrypted tunnel you trust.\nCredentials stay in memory. Public HTTP and invalid TLS certificates are rejected."
                );
                return gtk::glib::ExitCode::SUCCESS;
            }
            "--version" | "-V" => {
                println!("hermes-native {}", env!("CARGO_PKG_VERSION"));
                return gtk::glib::ExitCode::SUCCESS;
            }
            _ => {
                eprintln!("Unknown argument. Use --help.");
                return gtk::glib::ExitCode::FAILURE;
            }
        }
    }
    let config = Config::from_env(trusted_tunnel);
    let app = adw::Application::builder()
        .application_id("io.github.hermes_native.Client")
        .build();
    app.connect_activate(move |app| {
        if let Some(window) = app.active_window() {
            window.present();
            return;
        }
        adw::StyleManager::default().set_color_scheme(adw::ColorScheme::PreferDark);
        let provider = gtk::CssProvider::new();
        provider.load_from_string(include_str!("../resources/style.css"));
        if let Some(display) = gtk::gdk::Display::default() {
            gtk::style_context_add_provider_for_display(
                &display,
                &provider,
                gtk::STYLE_PROVIDER_PRIORITY_APPLICATION,
            );
        }
        ui::build(app, config.clone());
    });
    app.run_with_args::<&str>(&[])
}
