#![allow(dead_code, unused_imports)] // Let it shutup!

use crate::bt::*;
use crate::crawler::CrawlerObserver;
use crate::InfoHash;
use sha1::Digest;
use std::{
    collections::{BTreeMap, BTreeSet},
    net::SocketAddr,
    sync::{Arc, Mutex, Weak},
};
use tokio::net;
use tokio::sync::broadcast;
use tracing::{error, info};
const MAX_WORKERS: usize = 5;

struct DownloaderInner {
    map: BTreeMap<InfoHash, BTreeSet<SocketAddr>>, // Mapping info hash to the list of peers
    workers: BTreeSet<InfoHash>,                   // Mapping the workers of currently downloading
    start_worker: broadcast::Sender<InfoHash>,
    observer: Arc<dyn CrawlerObserver + Sync + Send>,
}

/// It is responsible for downloading the metadata of given hash and ip.
#[derive(Clone)]
pub struct Downloader {
    inner: Arc<Mutex<DownloaderInner>>,
}

impl Downloader {
    // Create an downloader
    pub fn new(observer: Arc<dyn CrawlerObserver + Sync + Send>) -> Downloader {
        let (sx, _rx) = broadcast::channel(114514);
        let downloader = Downloader {
            inner: Arc::new(Mutex::new(DownloaderInner {
                map: BTreeMap::new(),
                workers: BTreeSet::new(),
                start_worker: sx.clone(),
                observer: observer,
            })),
        };
        for _ in 0..MAX_WORKERS {
            // Start N workers
            let inner = Arc::downgrade(&downloader.inner);
            tokio::spawn(Downloader::worker_main(inner, sx.subscribe()));
        }
        return downloader;
    }

    // Pulling the job from the queue
    async fn worker_main(
        inner: Weak<Mutex<DownloaderInner>>,
        mut rx: broadcast::Receiver<InfoHash>,
    ) {
        loop {
            let hash = match rx.recv().await {
                Ok(hash) => hash,
                Err(_) => break,
            }; // We Got an new task
            let inner = match inner.upgrade() {
                Some(inner) => inner,
                None => break,
            };
            let observer = {
                let mut locked = inner.lock().unwrap();
                if !locked.workers.insert(hash) {
                    continue; // Already has a worker do it
                }

                locked.observer.clone()
            };
            Downloader::download_for(&inner, hash, observer).await;
            {
                let mut locked = inner.lock().unwrap();
                locked.workers.remove(&hash); // We finished all the work
            }
        }
    }

    // Downloading the metadata
    fn get_peer(inner: &Mutex<DownloaderInner>, hash: InfoHash) -> Option<SocketAddr> {
        let locked = inner.lock().unwrap();
        let peers = locked.map.get(&hash)?;
        return Some(peers.first()?.clone());
    }
    fn clear_peer(inner: &Mutex<DownloaderInner>, hash: InfoHash, ip: SocketAddr) -> Option<()> {
        let mut locked = inner.lock().unwrap();
        let peers = locked.map.get_mut(&hash)?;
        peers.remove(&ip);
        return Some(());
    }
    fn done_job(inner: &Mutex<DownloaderInner>, hash: InfoHash) {
        let mut locked = inner.lock().unwrap();
        locked.map.remove(&hash);
    }

    async fn download_for(
        inner: &Mutex<DownloaderInner>,
        hash: InfoHash,
        observer: Arc<dyn CrawlerObserver + Sync + Send>,
    ) {
        info!("Downloading metadata for {}", hash);
        while let Some(peer) = Downloader::get_peer(inner, hash) {
            let torrent = match Downloader::download(hash, peer).await {
                Ok(torrent) => torrent,
                Err(e) => {
                    info!("Failed to download metadata from {}: {}", peer, e);
                    Downloader::clear_peer(inner, hash, peer);
                    continue;
                }
            };
            info!("Downloaded metadata from {}", peer);

            Downloader::done_job(inner, hash);

            // Save it
            observer.on_metadata_downloaded(hash, torrent);
            return;
        }
    }

    async fn download(hash: InfoHash, ip: SocketAddr) -> Result<Vec<u8>, BtError> {
        info!("Connecting to {ip}");
        const UT_METADATA_ID: u8 = 1;
        let tcp = net::TcpStream::connect(ip).await?;
        let mut info = BtHandshakeInfo {
            hash: hash,
            peer_id: PeerId::make(),
            extensions: None,
        };
        info.set_metadata_id(UT_METADATA_ID); // We want to download the metadata use this
        let mut stream = BtStream::client_handshake(tcp, info).await?;
        let metadata_id = stream
            .peer_info()
            .metadata_id()
            .ok_or(BtError::UnsupportedExtension)?;
        let metadata_size = stream
            .peer_info()
            .metadata_size()
            .ok_or(BtError::UserDefined("Peer doesn't have metadata".into()))?;

        let pieces = (metadata_size + 16383) / 16384; // 16KB per piece
        let mut torrent = Vec::new();
        for i in 0..pieces {
            let request = UtMetadataMessage::Request { piece: i };
            let msg = BtMessage::Extended {
                id: metadata_id,
                msg: request.to_bencode(),
                payload: Vec::new(),
            };
            stream.write_message(&msg).await?;
            let payload = loop {
                let (id, msg, payload) = match stream.read_message().await? {
                    BtMessage::Extended { id, msg, payload } => (id, msg, payload),
                    _ => continue,
                };
                if id != UT_METADATA_ID {
                    continue; // Ignore
                }
                let ut_msg =
                    UtMetadataMessage::from_bencode(&msg).ok_or(BtError::InvalidMessage)?;
                match ut_msg {
                    UtMetadataMessage::Data {
                        piece,
                        total_size: _,
                    } => {
                        if piece != i {
                            error!("Got wrong piece {piece}");
                            return Err(BtError::InvalidMessage);
                        }
                        break payload;
                    }
                    _ => {
                        error!("Got wrong message {ut_msg:?}");
                        return Err(BtError::InvalidMessage);
                    }
                }
            };
            torrent.extend_from_slice(&payload);
        }
        // Sha1 check
        let mut sha1 = sha1::Sha1::new();
        sha1.update(&torrent);
        if sha1.finalize().as_slice() != hash.as_slice() {
            return Err(BtError::UserDefined("Hash mismatch".into()));
        }
        return Ok(torrent);
    }

    pub fn add_peer(&self, hash: InfoHash, ip: SocketAddr) {
        let mut inner = self.inner.lock().unwrap();
        let inserted = inner.map.entry(hash).or_insert(BTreeSet::new()).insert(ip);
        if !inserted {
            return; // Duplicate
        }
        if inner.workers.contains(&hash) {
            return;
        }
        // Start it
        inner.start_worker.send(hash).expect("It shouldn't fail");
    }
}
