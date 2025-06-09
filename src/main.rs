mod app;

#[tokio::main]
async fn main() {
    color_backtrace::install();
    tracing_subscriber::fmt()
        .with_max_level(tracing::Level::INFO)
        .with_thread_ids(true)
        .init();

    let app = app::App::new();
    app.run().await;
}