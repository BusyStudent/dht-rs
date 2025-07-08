use serde::{Deserialize, Serialize};
use tokio_stream::Stream;
use std::{
    convert::Infallible, fs, io::Write, net::SocketAddr, num::NonZero, sync::{Arc, Mutex, MutexGuard}, time::Duration
};
use tokio::{
    net, task::JoinHandle, sync::broadcast,
};
use axum::{
    extract::{self, Path, State}, 
    http::{HeaderMap, StatusCode}, 
    response::{sse::{Event, KeepAlive, Sse}, Html, IntoResponse, Json}, 
    routing::{get, post}, 
    Router
};

use serde_json::{
    Value,
    json
};

// Import our own modules
use dht_rs::{
    bt::Torrent, 
    crawler::{Crawler, CrawlerConfig, CrawlerController}, 
    *
};

use tracing::{error, info, warn};

#[derive(Deserialize, Serialize, Clone)]
struct Config {
    webui_addr: SocketAddr,
    bind_addr: SocketAddr,
    node_id: NodeId,
    hash_lru: NonZero<usize>,
    trackers: Vec<String>, // A list of trackers
}

/// The request from the webui
#[derive(Debug, Deserialize)]
struct MetadataSearch {
    query: String,
    page: usize,
}

/// The reply to the webui
#[derive(Debug, Serialize, Default)]
struct Metadata {
    info_hash: String, // The info hash of the torrent
    name: Option<String>,
    size: Option<String>, // 1 MB or 1.5 GB String
    files: Option<Value>, // The files in the torrent [{ name: 'ubuntu.iso', size: '4.7 GB' }, { name: 'README.txt', size: '1.2 KB' }]
}

#[derive(Debug, Serialize, Default)]
struct Status {
    running: bool,

    // Field only when running
    auto_sample: Option<bool>,
}

struct AppInner {
    config: Mutex<Config>,
    crawler: Mutex<Option<(Crawler, JoinHandle<()>)> >, // The cralwer and its task handle
    storage: Storage, // The database 
    broadcast: broadcast::Sender<Option<String> >, // Push the events to the webui, none on shutdown
}

#[derive(Clone)]
pub struct App {
    inner: Arc<AppInner>,
}

const MAX_SEARCH_PER_PAGES: usize = 25;

impl CrawlerController for AppInner {
    fn on_info_hash_found(&self, _hash: InfoHash) {
        // info!("Found a new info hash: {}", hash); // On Console!
        // let msg = format!("Found a new info hash: {}", _hash);
        // let _ = self.broadcast.send(Some(msg)); // On WebUI!
    }

    fn has_metadata(&self, hash: InfoHash) -> bool {
        let have = self.storage.has_torrent(hash);
        if let Err(e) = &have {
            error!("Can not check if the metadata exists: {}", e);
        }
        return have.unwrap_or(false);
    }

    fn on_message(&self, message: String) {
        let _ = self.broadcast.send(Some(message)); // On WebUI!
    }

    fn on_metadata_downloaded(&self, _hash: InfoHash, data: Vec<u8>) {
        let torrent = match Torrent::from_info_bytes(&data) {
            None => { // Save it to the disk
                warn!("Can not parse the torrent, save {} it to the disk", _hash);
                let mut file = fs::File::create(format!("./data/{}.torrent", _hash)).unwrap();
                let _ = file.write_all(&data);
                return;
            }
            Some(val) => val,
        };
        let _ = self.broadcast.send(Some(format!("Downloaded a new torrent: {}", torrent.name()))); // On WebUI!
        let _ = self.storage.add_torrent(&torrent);
    }
}

impl App {
    pub fn new() -> App {
        // Check dir exists
        if fs::exists("./data").unwrap() == false {
            fs::create_dir_all("./data").unwrap();
        }

        // Try to load config from the disk
        let result = fs::read_to_string("./data/config.json")
            .ok()
            .and_then(|content| serde_json::from_str(&content).ok() );
        let config = match result  {
            Some(val) => val,
            None => {
                // Using default config
                Config {
                    webui_addr: "127.0.0.1:10721".parse().unwrap(), // Ciallo～(∠・ω< )
                    bind_addr: "0.0.0.0:0".parse().unwrap(),
                    node_id: NodeId::rand(),

                    // LRU Config
                    hash_lru: NonZero::new(1000 * 10).unwrap(), // 10K

                    // Trackers
                    trackers: Vec::new(),
                }
            }
        };
        let (sx, _) = broadcast::channel(100);
        
        return App {
            inner: Arc::new(
                AppInner {
                    config: Mutex::new(config),
                    crawler: Mutex::new(None),
                    storage: Storage::open("./data/torrents.db").expect("Can not open the storage database"),
                    broadcast: sx,
                }
            )
        };
    }

    pub async fn run(&self) {
        let router = Router::new()
            // Static files
            .route("/", get(|| async {
                return Html(include_str!("../static/index.html"));
            }))
            .route("/styles.css", get(|| async {
                let mut header = HeaderMap::new();
                header.insert("Content-Type", "text/css".parse().unwrap());
                
                return (header, include_str!("../static/styles.css"));
            }))
            .route("/script.js", get(|| async {
                return include_str!("../static/script.js");
            }))

            // Basic API
            .route("/api/v1/start_dht", get(App::start_dht_handler))
            .route("/api/v1/stop_dht", get(App::stop_dht_handler))
            .route("/api/v1/status", get(App::status_handler))
            .route("/api/v1/set/{*var}", post(App::set_var_handler))

            // SSE
            .route("/api/v1/sse/events", get(App::sse_events_handler))
            
            // Info Query
            .route("/api/v1/get_routing_table", get(App::get_routing_table_handler))

            // Debug Tools
            .route("/api/v1/tools/{*tool}", post(App::tools_handler))

            // Search
            .route("/api/v1/search_metadata", post(App::search_metadata_handler))
            .route("/api/v1/torrents/{*info_hash}", get(App::get_torrent_handler))

            // Config...
            .route("/api/v1/get_config", get(App::get_config_handler))
            .route("/api/v1/set_config", post(App::set_config_handler))
            .with_state(self.clone())
        ;
        let addr = self.config().webui_addr;
        let listener = net::TcpListener::bind(addr).await.unwrap();
        println!("WebUI Listening on http://{}/", listener.local_addr().unwrap());
        let _ = axum::serve(listener, router)
            .with_graceful_shutdown(self.clone().shutdown_signal())
            .await;
    }

    // Helper function to get the config
    fn config(&self) -> MutexGuard<Config> {
        return self.inner.config.lock().unwrap();
    }

    // Shutdown signal
    async fn shutdown_signal(self) {
        let ctrl_c = async {
            tokio::signal::ctrl_c()
                .await
                .expect("Failed to install Ctrl-C handler");
        };

        tokio::select! {
            _ = ctrl_c => { info!("Shutdown signal received, cleanup..."); },
        }

        // Shutdown the DHT if it's running
        self.stop_dht().await;
        let _ = self.inner.broadcast.send(None);
    }

    // Stop the DHT
    // Return true if the DHT is running, false on already stopped
    async fn stop_dht(&self) -> bool {
        let (_crawler, handle) = {
            let mut lock = self.inner.crawler.lock().unwrap();
            match lock.take() {
                Some(what) => what,
                None => return false,
            }
        };
        handle.abort();
        let _ = handle.await;
        info!("DHT Stopped");
        return true;
    }

    // Basic API
    async fn start_dht_handler(State(app): State<App>) -> impl IntoResponse {
        {
            let crawler = app.inner.crawler.lock().unwrap();
            if crawler.is_some() {
                return String::from("DHT Already started");
            }
        }
        // Create the crawler config by the config
        let config = {
            let conf = app.config();

            CrawlerConfig {
                id: conf.node_id,
                ip: conf.bind_addr,
                hash_lru_cache_size: conf.hash_lru,
                controller: app.inner.clone(),
                trackers: conf.trackers.clone(),
            }
        };
        let crawler = match Crawler::new(config).await {
            Ok(crawler) => crawler,
            Err(err) => return format!("Error to start the DHT ({err})"),
        };
        let handle = tokio::spawn(crawler.clone().run());
        *app.inner.crawler.lock().unwrap() = Some((crawler, handle));
        info!("DHT Started");
        return String::from("DHT started");
    }

    async fn stop_dht_handler(State(app): State<App>) -> impl IntoResponse {
        if app.stop_dht().await {
            return String::from("DHT stopped");
        }
        return String::from("DHT already stopped");
    }

    async fn status_handler(State(app): State<App>) -> impl IntoResponse {
        let lock = app.inner.crawler.lock().unwrap();
        let (crawler, _) = match lock.as_ref() {
            None => return Json(Status{
                running: false,
                ..Default::default()
            }),
            Some(crawler) => crawler,
        };
        return Json(Status{
            running: true,
            auto_sample: Some(crawler.auto_sample()),
        });
    }

    // Set the var
    async fn set_var_handler(State(app): State<App>, extract::Path(var): extract::Path<String>, extract::Json(value): extract::Json<Value>) -> impl IntoResponse {
        match var.as_str() {
            "auto_sample" => { // Enable or disable the auto sample
                let lock = app.inner.crawler.lock().unwrap();
                let (crawler, _) = match lock.as_ref() {
                    None => return Json(json!({
                        "error": "DHT not started"
                    })),
                    Some(val) => val,
                };
                let enable = match value.as_bool() {
                    None => return Json(json!({
                        "error": "Invalid value, expected a boolean"
                    })),
                    Some(val) => val,
                };

                let prev = crawler.set_auto_sample(enable);
                return Json(json!({
                    "previous": prev,
                    "current": enable,
                }));
            }
            _ => {
                return Json(json!({
                    "error": format!("Unknown var {var}")
                }));
            }
        }
    }

    // Info Query
    async fn get_routing_table_handler(State(app): State<App>) -> impl IntoResponse {
        let crawler = {
            let mtx = app.inner.crawler.lock().unwrap();
            match &*mtx {
                Some((crawler, _)) => crawler.clone(),
                None => return Json(json!([])),
            }
        };
        let table = crawler.dht_session().routing_table().await;
        if table.is_empty() {
            return Json(json!([]));
        }
        let mut list = Vec::new();
        for (node_id, ip) in table.iter() {
            list.push(json!({
                "id": node_id.hex(),
                "ip": ip,
            }));
        }
        return Json(json!(list));
    }

    // Config
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

    // Search
    async fn search_metadata_handler_impl(State(app): State<App>, extract::Json(search): extract::Json<MetadataSearch>) -> Result<Value, storage::Error> {
        let size_to_string = |size: u64| {
            if size < 1024 {
                return format!("{} bytes", size);
            }
            if size < 1024 * 1024 {
                return format!("{} KB", size / 1024);
            }
            if size < 1024 * 1024 * 1024 {
                return format!("{} MB", size / 1024 / 1024);
            }
            return format!("{} GB", size / 1024 / 1024 / 1024);
        };

        let pattern = search.query;
        let page = search.page.saturating_sub(1); // Page is 1-indexed, to 0-indexed
        let mut items = Vec::new();
        let info = app.inner.storage.search_torrent(&pattern, (page * MAX_SEARCH_PER_PAGES) as u64, MAX_SEARCH_PER_PAGES as u64)?;
        for torrent in info.torrents.iter() {
            let mut files = Vec::new();

            for file in torrent.files.iter() {
                files.push(json!({
                    "name": file.name.clone(),
                    "size": size_to_string(file.size),
                }));
            }

            items.push(Metadata {
                info_hash: torrent.hash.hex(),
                name: Some(torrent.name.clone()),
                size: Some(size_to_string(torrent.size)),
                files: Some(json!(files)),
            })
        }
        // Add 

        let json = json!({
            "results": items,
            "totalPages": (info.total as usize + MAX_SEARCH_PER_PAGES - 1) / MAX_SEARCH_PER_PAGES,
            "currentPage": search.page,
        });
        return Ok(json);
    }

    async fn search_metadata_handler(app: State<App>, json: extract::Json<MetadataSearch>) -> impl IntoResponse {
        return match App::search_metadata_handler_impl(app, json).await {
            Ok(json) => Json(json),
            Err(err) => Json(json!({
                "error": format!("Error to search metadata ({err})")
            })),
        }
    }

    async fn get_torrent_handler(State(app): State<App>, Path(hash): Path<String>) -> impl IntoResponse {
        let torrent = match InfoHash::from_hex(&hash) {
            None => return Err(StatusCode::BAD_REQUEST),
            Some(hash) => {
                app.inner.storage.get_torrent(hash).ok()
            }
        };
        // Build the reply
        if let Some(data) = torrent {
            let mut header = HeaderMap::new();
            header.insert("Content-Type", "application/octet-stream".parse().unwrap());

            return Ok((header, data));
        }
        return Err(StatusCode::NOT_FOUND);
    }

    // Tools
    async fn tools_handler_impl(crawler: Crawler, path: String, json: Value) -> Result<Value, String> {
        // Get the address..
        let get_addr = |json: &Value| -> Result<SocketAddr, String> {
            let addr = json["address"].as_str().ok_or("Missing address in json")?;
            return addr.parse::<SocketAddr>().map_err(|err| err.to_string() );
        };
        let session = crawler.dht_session().clone();
        match path.as_str() {
            "ping" => {
                let addr = get_addr(&json)?;
                let res = session.ping(addr).await;
                let id = res.map_err(|e| e.to_string() )?;
                return Ok(json!({
                    "id": id.hex()
                }));
            }
            "sample_infohashes" => {
                let target = json["target"].as_str().ok_or("Missing target in json")?;
                let target = NodeId::from_hex(target).ok_or("Invalid target")?;
                let addr = get_addr(&json)?;
                let reply = session.sample_infohashes(addr, target).await.map_err(|e| {
                    return e.to_string()
                })?;
                // Convert it
                let nodes: Vec<Value> = reply.nodes.iter().map(|(id, ip)| {
                    return json!({
                        "id": id.hex(),
                        "ip": ip.to_string(),
                    })
                }).collect();
                let info_hashes: Vec<Value> = reply.info_hashes.iter().map(|hash| {
                    return json!(hash.hex());
                }).collect();
                return Ok(json!({
                    "id": reply.id.hex(),
                    "num": reply.num,
                    "interval": reply.interval,
                    "nodes": nodes,
                    "info_hashes": info_hashes,
                }));
            }
            "get_peers" => {
                let info_hash = json["info_hash"].as_str().ok_or("Missing info_hash in json")?;
                let info_hash = InfoHash::from_hex(info_hash).ok_or("Invalid info_hash")?;
                let reply = session.get_peers(info_hash).await.map_err(|e| {
                    return e.to_string()
                })?;
                // Convert it
                let nodes: Vec<Value> = reply.nodes.iter().map(|node| {
                    let mut token = String::new();
                    for byte in node.token.iter() {
                        token.push_str(&format!("{:02x}", byte));
                    }
                    return json!({
                        "id": node.id.hex(),
                        "ip": node.ip.to_string(),
                        "token": token,
                    });
                }).collect();

                return Ok(json!({
                    "nodes": nodes,
                    "peers": reply.peers,
                }));
            }
            "add_hash" => {
                let info_hash = json["info_hash"].as_str().ok_or("Missing info_hash in json")?;
                let info_hash = InfoHash::from_hex(info_hash).ok_or("Invalid info_hash")?;
                crawler.add_hash(info_hash);
                return Ok(json!({
                    "status": "success"
                }))
            }
            _ => {
                return Err("Invalid tool".into());
            }
        }
    }

    async fn tools_handler(State(app): State<App>, Path(tool): Path<String>, extract::Json(json): extract::Json<Value>) -> impl IntoResponse {
        info!("Calling tools {tool} handler with args {json}");
        let crawler = {
            let mtx = app.inner.crawler.lock().unwrap();
            match &*mtx {
                Some((cralwer, _)) => cralwer.clone(),
                None => return Json(json!({
                    "error": "No crawler is running"
                })),
            }
        };
        match App::tools_handler_impl(crawler, tool, json).await {
            Ok(val) => return Json(val),
            Err(err) => return Json(json!({
                "error" : err 
            })),
        }
    }

    // SSE
    async fn sse_events_handler(State(app): State<App>) -> Sse<impl Stream<Item = Result<Event, Infallible> > > {
        let mut rx = app.inner.broadcast.subscribe();
        let stream = async_stream::stream! {
            loop {
                match rx.recv().await {
                    Ok(Some(msg)) => {
                        yield Ok(Event::default().data(&msg));
                    }
                    Ok(None) => {
                        return; // We are shutting down
                    }
                    Err(_) => {
                        break;
                    }
                }
            }
        };

        return Sse::new(stream)
            .keep_alive(KeepAlive::new().interval(Duration::from_secs(30)));

    }
}