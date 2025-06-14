#![allow(dead_code)] // Let it shutup!
use serde::{Deserialize, Serialize};
use std::{
    collections::{BTreeSet}, 
    fs, 
    io::{self, Write}, 
    net::SocketAddr, 
    sync::{Arc, Mutex, MutexGuard}
};
use tokio::{
    io::AsyncReadExt, net, task::JoinHandle
};
use axum::{
    Router, 
    routing::{get, post}, 
    response::{Html, IntoResponse, Json},
    extract::{State, Path},
    extract,
};

use serde_json::{
    Value,
    json
};

// Import our own modules
use dht_rs::{
    bt::Torrent, 
    crawler::{Crawler, CrawlerConfig, CrawlerObserver}, 
    dht::DhtSession, 
    *
};

use tracing::{info, error};

#[derive(Deserialize, Serialize, Clone)]
struct Config {
    webui_port: u16,
    #[serde(default = "default_ip")]
    webui_ip: String,
    bind_addr: SocketAddr,
    node_id: String,
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

struct AppInner {
    config: Mutex<Config>,
    crawler: Mutex<Option<(Crawler, JoinHandle<()>)> >, // The cralwer and its task handle
    info_hashes: Mutex<BTreeSet<InfoHash> >
}

#[derive(Clone)]
pub struct App {
    inner: Arc<AppInner>,
}

fn default_ip() -> String {
    return "127.0.0.1".into();
}

const MAX_SEARCH_PER_PAGES: usize = 50;

impl CrawlerObserver for AppInner {
    fn on_info_hash_found(&self, hash: InfoHash) -> bool {
        let is_new = self.info_hashes.lock().unwrap().insert(hash);
        if is_new { // Check exist in the filesystem?
            if self.has_metadata(hash) {
                return false; // Already exists
            }
            info!("Found a new info hash: {}", hash);
        }
        return is_new;
    }

    fn has_metadata(&self, hash: InfoHash) -> bool {
        return fs::exists(format!("./data/torrents/{}.torrent", hash)).unwrap_or(false);
    }

    fn on_metadata_downloaded(&self, hash: InfoHash, data: Vec<u8>) {
        let torrent = Torrent::from_info_bytes(&data).expect("It should never failed");
        let data = torrent.object().encode();
        let mut file = match fs::File::create(format!("./data/torrents/{}.torrent", hash)) {
            Ok(file) => file,
            Err(err) => {
                error!("Can not create the file: {}", err);
                return;
            }
        };
        if let Err(err) = file.write_all(&data) {
            error!("Can not write the file: {}", err);
        }
    }
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
                    node_id: NodeId::rand().hex(),
                }
            },
        };
        
        return App {
            inner: Arc::new(
                AppInner {
                    config: Mutex::new(config),
                    crawler: Mutex::new(None),
                    info_hashes: Mutex::new(BTreeSet::new()),
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
            .route("/api/v1/is_dht_running", get(App::is_dht_running_handler))
            
            // Info Query
            .route("/api/v1/get_routing_table", get(App::get_routing_table_handler))

            // Debug Tools
            .route("/api/v1/tools/{*tool}", post(App::tools_handler))

            // Search
            .route("/api/v1/search_metadata", post(App::search_metadata_handler))

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

    // Basic API
    async fn start_dht_handler(State(app): State<App>) -> impl IntoResponse {
        {
            let crawler = app.inner.crawler.lock().unwrap();
            if crawler.is_some() {
                return String::from("DHT Already started");
            }
        }
        let (id, addr) = {
            let config = app.config();
            let id = if config.node_id.is_empty() {
                NodeId::rand()
            }
            else {
                NodeId::from_hex(config.node_id.as_str()).unwrap()
            };

            (id, config.bind_addr)
        };
        let config = CrawlerConfig {
            id: id,
            ip: addr,
            observer: app.inner.clone(),
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

    async fn stop_dht_handler(State(_app): State<App>) -> impl IntoResponse {
        let (_crawler, handle) = {
            let mut crawler = _app.inner.crawler.lock().unwrap();
            let mut cur: Option<_> = None;
            std::mem::swap(&mut *crawler, &mut cur);

            match cur {
                Some(what) => what,
                None => return "DHT already stopped",
            }
        };
        handle.abort();
        let _ = handle.await;
        info!("DHT Stopped");
        return "DHT stopped";
    }

    async fn is_dht_running_handler(State(app): State<App>) -> impl IntoResponse {
        let crawler = app.inner.crawler.lock().unwrap();
        if crawler.is_some() {
            return String::from("true");
        }
        return String::from("false");
    }

    // Info Query
    async fn get_routing_table_handler(State(app): State<App>) -> impl IntoResponse {
        let mtx = app.inner.crawler.lock().unwrap();
        let cralwer = match &*mtx {
            Some((cralwer, _)) => cralwer,
            None => return String::from("[]"),
        };
        let nodes: Vec<(NodeId, SocketAddr)> = cralwer.dht_session().routing_table().iter().collect();
        if nodes.is_empty() {
            return String::from("[]");
        }
        let mut list = Vec::new();
        for (node_id, ip) in nodes {
            list.push(json!({
                "id": node_id.hex(),
                "ip": ip,
            }));
        }
        return json!(list).to_string();
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
    async fn search_metadata_handler_impl(State(_app): State<App>, extract::Json(search): extract::Json<MetadataSearch>) -> Result<String, io::Error> {
        let length_to_string = |length: u64| {
            if length < 1024 {
                return format!("{} bytes", length);
            }
            if length < 1024 * 1024 {
                return format!("{} KB", length / 1024);
            }
            if length < 1024 * 1024 * 1024 {
                return format!("{} MB", length / 1024 / 1024);
            }
            return format!("{} GB", length / 1024 / 1024 / 1024);
        };

        let _pattern = search.query;
        let page = search.page;
        // Enumerate all the files in in torrents folder
        let mut items = Vec::new();
        let mut items_len = 0; // The number of max item
        let mut entries = tokio::fs::read_dir("./data/torrents").await?;
        // Broswer from filesystem
        while let Some(entry) = entries.next_entry().await? {
            items_len += 1;
            if page * MAX_SEARCH_PER_PAGES < items_len - 1 {
                continue;
            }
            if (page + 1) * MAX_SEARCH_PER_PAGES <= items_len - 1 {
                continue; // We need the count of items to calc the max page
            }
            let mut file = tokio::fs::File::open(entry.path()).await?;
            let mut vec = Vec::new();
            file.read_to_end(&mut vec).await?;
            let filename = entry.file_name().into_string().unwrap();

            // Parse the torrent
            let torrent = Torrent::from_bytes(&vec).
                ok_or(io::Error::new(io::ErrorKind::Other, "Invalid torrent file"))?;
            let hash_name = filename.split('.').next().unwrap_or(filename.as_str());
            let mut files = Vec::new();

            for (name, length) in torrent.files() {
                files.push(json!({
                    "name": name,
                    "size": length_to_string(length),
                }));
            }
            items.push(Metadata {
                info_hash: hash_name.to_string(),
                name: Some(torrent.name().into()),
                size: Some(length_to_string(torrent.length())),
                files: Some(json!(files)),
            });
        }
        // Broswer from the memory
        for hash in _app.inner.info_hashes.lock().unwrap().iter() {
            items_len += 1;
            if page * MAX_SEARCH_PER_PAGES < items_len - 1 {
                continue;
            }
            if (page + 1) * MAX_SEARCH_PER_PAGES <= items_len -1 {
                continue; // We need the count of items to calc the max page
            }
            items.push(Metadata {
                info_hash: hash.hex(),
                .. Default::default()
            });
        }

        let json = json!({
            "results": items,
            "totalPages": items_len / MAX_SEARCH_PER_PAGES,
            "currentPage": search.page,
        });
        return Ok(json.to_string());
    }

    // Tools
    async fn tools_handler_impl(session: DhtSession, path: String, json: Value) -> Result<Value, String> {
        // Get the address..
        let addr = json["address"].as_str().ok_or("Missing address in json")?;
        let addr = addr.parse::<SocketAddr>().map_err(|err| err.to_string() )?;
        match path.as_str() {
            "ping" => {
                let res = session.ping(addr).await;
                let id = res.map_err(|e| e.to_string() )?;
                return Ok(json!({
                    "id": id.hex()
                }));
            }
            "sample_infohashes" => {
                let target = json["target"].as_str().ok_or("Missing target in json")?;
                let target = NodeId::from_hex(target).ok_or("Invalid target")?;
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
            _ => {
                return Err("Invalid tool".into());
            }
        }
    }

    async fn tools_handler(State(app): State<App>, Path(tool): Path<String>, extract::Json(json): extract::Json<Value>) -> impl IntoResponse {
        info!("Calling tools {tool} handler with args {json}");
        let session = {
            let mtx = app.inner.crawler.lock().unwrap();
            match &*mtx {
                Some((cralwer, _)) => cralwer.dht_session().clone(),
                None => return Json(json!({
                    "error": "No crawler is running"
                })),
            }
        };
        match App::tools_handler_impl(session.clone(), tool, json).await {
            Ok(val) => return Json(val),
            Err(err) => return Json(json!({
                "error" : err 
            })),
        }

    }

    async fn search_metadata_handler(app: State<App>, json: extract::Json<MetadataSearch>) -> impl IntoResponse {
        return match App::search_metadata_handler_impl(app, json).await {
            Ok(json) => json,
            Err(err) => format!("Error to search metadata ({err})"),
        }
    }
}