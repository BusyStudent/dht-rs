#![allow(dead_code)] // Let it shutup!
use serde::{Deserialize, Serialize};
use std::{
    fs, io::Write, net::SocketAddr, sync::{Arc, Mutex, MutexGuard}
};
use tokio::net;
use axum::{
    Router, 
    routing::{get, post}, 
    response::{Html, IntoResponse, Json},
    extract::State,
    extract,
};

// Import our own modules
use dht_rs::*;

#[derive(Deserialize, Serialize, Clone)]
struct Config {
    webui_port: u16,
    #[serde(default = "default_ip")]
    webui_ip: String,
    bind_addr: SocketAddr,
    node_id: String,
}
struct AppInner {
    config: Mutex<Config>,
}

#[derive(Clone)]
pub struct App {
    inner: Arc<AppInner>,
}

fn default_ip() -> String {
    return "127.0.0.1".into();
}

impl App {
    pub fn new() -> App {
        // Check dir exists
        if fs::exists("./data/torrents").unwrap() == false {
            fs::create_dir_all("./data/torrents").unwrap();
        }

        // Try to load config from the disk
        let config = match fs::read_to_string("./data/config.json") {
            Ok(content) => {
                serde_json::from_str(&content).expect("Can not read the config from the disk")
            },
            Err(_) => {
                // Using default config
                Config {
                    webui_port: 10721, // Ciallo～(∠・ω< )
                    webui_ip: "127.0.0.1".to_string(),
                    bind_addr: "0.0.0.0:0".parse().unwrap(),
                    node_id: String::new(),
                }
            },
        };
        
        return App {
            inner: Arc::new(
                AppInner {
                    config: Mutex::new(config)
                }
            )
        };
    }

    pub async fn run(&self) {
        let router = Router::new()
            .route("/", get(|| async {
                return Html(include_str!("../static/index.html"));
            }))

            // Basic API
            .route("/api/v1/start_dht", get(App::start_dht_handler))
            .route("/api/v1/stop_dht", get(App::stop_dht_handler))

            // Config...
            .route("/api/v1/get_config", get(App::get_config_handler))
            .route("/api/v1/set_config", post(App::set_config_handler))
            .with_state(self.clone())
        ;
        let addr: SocketAddr = {
            let conf = self.config();
            format!("{}:{}", conf.webui_ip, conf.webui_port).parse().expect("Not a valid address")
        };
        let listener = net::TcpListener::bind(addr).await.unwrap();
        println!("WebUI Listening on http://{}/", listener.local_addr().unwrap());
        let _ = axum::serve(listener, router).await;
    }

    // Helper function to get the config
    fn config(&self) -> MutexGuard<Config> {
        return self.inner.config.lock().unwrap();
    }

    async fn start_dht_handler(State(_app): State<App>) -> impl IntoResponse {
        return "DHT started";
    }

    async fn stop_dht_handler(State(_app): State<App>) -> impl IntoResponse {
        return "DHT stopped";
    }

    async fn get_config_handler(State(app): State<App>) -> impl IntoResponse {
        return Json(app.config().clone());
    }

    async fn set_config_handler(State(app): State<App>, extract::Json(config): extract::Json<Config>) -> impl IntoResponse {
        *app.config() = config.clone();
        let mut file = match fs::File::create("./data/config.json") {
            Ok(file) => file,
            Err(err) => return format!("Error to save the config ({err})"),
        };
        let json = serde_json::to_string_pretty(&config).expect("It should not be failed");
        file.write_all(json.as_bytes()).unwrap();
        println!("App config saved as {}", json);
        return "OK".into();
    }
}