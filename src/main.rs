mod app;
mod config;
mod event;
mod font;
mod input;
mod pty;
mod renderer;
mod terminal;

use app::App;
use event::AppEvent;
use tracing_subscriber::{EnvFilter, fmt};
use winit::event_loop::{ControlFlow, EventLoop};

fn main() -> Result<(), winit::error::EventLoopError> {
    init_tracing();

    let config = match config::Config::load() {
        Ok(config) => config,
        Err(error) => {
            tracing::error!(%error, "failed to load configuration; using defaults");
            config::Config::default()
        }
    };

    let event_loop = EventLoop::<AppEvent>::with_user_event().build()?;
    event_loop.set_control_flow(ControlFlow::Wait);

    let event_proxy = event_loop.create_proxy();

    tracing::info!("starting Flash");
    event_loop.run_app(&mut App::new(event_proxy, config))
}

fn init_tracing() {
    let filter =
        EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new("flash=info,warn"));

    fmt()
        .with_env_filter(filter)
        .with_target(false)
        .compact()
        .init();
}
