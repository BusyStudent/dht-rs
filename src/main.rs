use tracing_subscriber::EnvFilter;
mod app;

#[tokio::main]
async fn main() {
    #[cfg(debug_assertions)]
    unsafe { // In debug mode, we want to see the all backtrace
        color_backtrace::install();
        std::env::set_var("RUST_BACKTRACE", "full");
    }
    let filter = EnvFilter::new("info")
        .add_directive("dht_rs::bt::udp_tracker=trace".parse().unwrap())
        .add_directive("dht_rs::dht=warn".parse().unwrap());

    tracing_subscriber::fmt()
        .with_max_level(tracing::Level::DEBUG)
        .with_thread_ids(true)
        .with_env_filter(filter)
        .pretty()
        .init();

    let app = app::App::new();
    app.run().await;
}