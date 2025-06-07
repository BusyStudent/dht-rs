mod app;

#[tokio::main]
async fn main() {
    color_backtrace::install();
    let app = app::App::new();
    app.run().await;
}