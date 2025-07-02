mod app;

#[tokio::main]
async fn main() {
    #[cfg(debug_assertions)]
    unsafe { // In debug mode, we want to see the all backtrace
        color_backtrace::install();
        std::env::set_var("RUST_BACKTRACE", "full");
    }

    tracing_subscriber::fmt()
        .with_max_level(tracing::Level::INFO)
        .with_thread_ids(true)
        .init();

    let app = app::App::new();
    app.run().await;
}