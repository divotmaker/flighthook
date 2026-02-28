#![cfg_attr(
    all(target_os = "windows", feature = "gui"),
    windows_subsystem = "windows"
)]

use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::AtomicBool;

use clap::Parser;
use tokio::sync::broadcast;
use tracing_subscriber::EnvFilter;

mod actors;
mod bus;
mod state;

use actors::Actor;
use bus::BusSender;
use flighthook::FlighthookMessage;
use state::SystemState;

#[derive(Parser, Debug, Clone)]
#[command(name = "flighthook", about = "Launch monitor bridge")]
struct Config {
    /// Config file path (default: ~/.config/flighthook/config.toml)
    #[arg(long)]
    config: Option<PathBuf>,

    /// Run without the native GUI window (web dashboard only)
    #[arg(long)]
    headless: bool,
}

// ---------------------------------------------------------------------------
// Main
// ---------------------------------------------------------------------------

fn main() -> anyhow::Result<()> {
    let filter = if std::env::var("RUST_LOG").is_ok() {
        EnvFilter::from_default_env()
    } else {
        EnvFilter::new("flighthook=info")
    };
    tracing_subscriber::fmt().with_env_filter(filter).init();

    tracing::debug!("debug logging enabled");

    let cli = Config::parse();

    // Load (or create) config file
    let config_path = cli
        .config
        .clone()
        .unwrap_or_else(state::config::default_config_path);
    let _persisted = state::config::load(&config_path);

    // Create tokio runtime manually -- eframe::run_native() needs the main thread
    let rt = tokio::runtime::Runtime::new()?;
    let _guard = rt.enter();

    // Single unified bus
    let (bus_tx, _) = broadcast::channel::<FlighthookMessage>(1024);

    // Build shared state root
    let (system_state, game_writer) = SystemState::new(config_path);
    let state = Arc::new(system_state);

    // Auto-enable web server for native GUI (the GUI connects via HTTP/WS).
    // Persists to config so the file reflects the running state.
    if !cli.headless {
        let snap = state.system.snapshot();
        if snap.webserver.is_empty() {
            state.system.update(|p| {
                p.webserver.insert(
                    "0".into(),
                    flighthook::WebserverSection {
                        name: "Web Server".into(),
                        bind: "127.0.0.1:3030".into(),
                    },
                );
            });
        }
    }

    // System actor — always-on internal housekeeping (GameState updates via writer).
    // Must be fully up before other actors start so no bus events are missed.
    {
        let shutdown = Arc::new(AtomicBool::new(false));
        let sender = BusSender::new("system".into(), bus_tx.clone(), Arc::clone(&shutdown));
        let receiver = sender.subscribe();
        let (actor, ready_rx) =
            actors::system::SystemActor::new(game_writer, Arc::clone(&state), bus_tx.clone());
        actor.start(Arc::clone(&state), sender, receiver);
        ready_rx.recv().expect("system actor failed to start");
        state.register_actor("system".into(), Box::new(actor), shutdown);
    }

    // Start all actors from config (launch monitors, integrations, webserver)
    let snap = state.system.snapshot();
    for ra in actors::resolve_actors(&snap) {
        tracing::info!("starting actor '{}' ({})", ra.id, ra.name);
        actors::start_actor(ra.id, ra.actor, &state, &bus_tx);
    }

    // Drain bus (keeps broadcast channel healthy when no other subscriber)
    let mut drain_rx = bus_tx.subscribe();
    let drain_handle = tokio::spawn(async move {
        loop {
            match drain_rx.recv().await {
                Ok(_) => {}
                Err(broadcast::error::RecvError::Closed) => break,
                Err(broadcast::error::RecvError::Lagged(n)) => {
                    tracing::warn!("drain subscriber lagged, dropped {n} events");
                }
            }
        }
    });

    if cli.headless {
        tracing::info!("running headless (no native GUI)");
        rt.block_on(async { tokio::signal::ctrl_c().await })?;
    } else {
        #[cfg(feature = "gui")]
        {
            let web_addr: std::net::SocketAddr = state
                .system
                .snapshot()
                .webserver
                .values()
                .next()
                .and_then(|w| w.bind.parse().ok())
                .ok_or_else(|| anyhow::anyhow!("no valid webserver bind address in config"))?;
            let gui_url = if web_addr.ip().is_unspecified() {
                format!("http://127.0.0.1:{}", web_addr.port())
            } else {
                format!("http://{web_addr}")
            };
            flighthook_ui::net::set_base_url(gui_url);

            let icon = {
                let png = image::load_from_memory(include_bytes!("../assets/icon.png"))
                    .expect("embedded icon.png")
                    .into_rgba8();
                let (w, h) = png.dimensions();
                egui::IconData {
                    rgba: png.into_raw(),
                    width: w,
                    height: h,
                }
            };
            let native_options = eframe::NativeOptions {
                viewport: egui::ViewportBuilder::default()
                    .with_inner_size([1024.0, 768.0])
                    .with_icon(std::sync::Arc::new(icon)),
                ..Default::default()
            };

            tracing::info!("launching native GUI");
            eframe::run_native(
                "Flighthook",
                native_options,
                Box::new(|cc| Ok(Box::new(flighthook_ui::app::FlighthookApp::new(cc)))),
            )
            .map_err(|e| anyhow::anyhow!("{e}"))?;
        }
        #[cfg(not(feature = "gui"))]
        {
            tracing::info!("running headless (no native GUI -- built without gui feature)");
            rt.block_on(async { tokio::signal::ctrl_c().await })?;
        }
    }

    // Shutdown — stop all actors (including webserver) via registry
    tracing::info!("shutting down...");
    for id in state.actor_ids() {
        state.stop_actor(&id);
    }
    // Drop bus_tx closes the broadcast channel as secondary signal
    drop(bus_tx);
    drain_handle.abort();

    Ok(())
}
