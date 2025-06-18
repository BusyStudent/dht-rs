#![allow(dead_code, unused_imports)] // Let it shutup!

use crate::utp::*;
use crate::bt::*;
use crate::InfoHash;
use sha1::Digest;
use tokio::io::AsyncRead;
use tokio::io::AsyncWrite;
use tokio::join;
use tokio::net::TcpStream;
use tokio::task::JoinSet;
use tracing::warn;
use std::{
    collections::{BTreeMap, BTreeSet},
    net::SocketAddr,
    sync::{Arc, Mutex, Weak, OnceLock},
    time::Duration,
    io
};
use tokio::net;
use tokio::sync::broadcast;
use tracing::{error, info};

const MAX_WORKERS: usize = 5;
const MAX_CONCURRENT: usize = 10; // Max concurrent downloading for pre each worker
const MAX_DOWNLOAD_TIME: Duration = Duration::from_secs(60);

struct WorkerState {
    
}

struct DownloadState {
    map: BTreeMap<InfoHash, BTreeSet<SocketAddr> >, // Mapping info hash to the list of peers
    workers: BTreeSet<InfoHash>,                    // Mapping the workers of currently downloading
}

struct DownloaderInner {
    state: Mutex<DownloadState>,
    start_worker: broadcast::Sender<InfoHash>,
    controller: OnceLock<Weak<dyn DownloaderController + Sync + Send> >,
    utp_context: UtpContext,
}

// pub struct DownloaderConfig {
//     controller: Arc<dyn CrawlerObserver + Sync + Send>,
//     utp_context: UtpContext,
//     peer_id: PeerId,
// }

pub trait DownloaderController {
    fn on_metadata_downloaded(&self, info_hash: InfoHash, data: Vec<u8>);
    fn has_metadata(&self, info_hash: InfoHash) -> bool;
}

/// It is responsible for downloading the metadata of given hash and ip.
#[derive(Clone)]
pub struct Downloader {
    inner: Arc<DownloaderInner>,
}

impl Downloader {
    // Create an downloader
    pub fn new(ctxt: UtpContext) -> Downloader {
        let (sx, _rx) = broadcast::channel(114514);
        let downloader = Downloader {
            inner: Arc::new(DownloaderInner {
                state: Mutex::new(
                    DownloadState {
                        map: BTreeMap::new(),
                        workers: BTreeSet::new(),
                    }
                ),
                start_worker: sx.clone(),
                controller: OnceLock::new(),
                utp_context: ctxt,
            }),
        };
        return downloader;
    }

    /// Run the workers of downloader
    pub async fn run(&self) {
        let mut set = JoinSet::new();
        for _ in 0..MAX_WORKERS {
            set.spawn(self.clone().worker_main());
        }
        let _ = tokio::join!(set.join_all(), self.listener_main());
    }

    // Pulling the job from the queue
    async fn worker_main(self) {
        let mut rx = self.inner.start_worker.subscribe();
        loop {
            let hash = match rx.recv().await {
                Ok(hash) => hash,
                Err(_) => break,
            }; // We Got an new task
            {
                let mut locked = self.inner.state.lock().unwrap();
                if !locked.workers.insert(hash) {
                    continue; // Already has a worker do it
                }
            }
            self.download_for(hash).await;
            {
                let mut locked = self.inner.state.lock().unwrap();
                locked.workers.remove(&hash); // We finished all the work
            }
        }
    }

    async fn listener_main(&self) {
        let mut listener = UtpListener::new(&self.inner.utp_context, 10); // MAX 10 pending.
        loop {
            let (stream, peer) = match listener.accept().await {
                Ok(val) => val,
                Err(e) => {
                    error!("Failed to use UtpListener::accept() => {e}");
                    return;
                }
            };
            info!("Utp: Incoming peer from {}", peer);

            tokio::spawn(self.clone().handle_incoming(stream));
        }
    }

    // Downloading the metadata
    fn get_peer(&self, hash: InfoHash) -> Option<SocketAddr> {
        let locked = self.inner.state.lock().unwrap();
        let peers = locked.map.get(&hash)?;
        return Some(peers.first()?.clone());
    }

    fn clear_peer(&self, hash: InfoHash, ip: SocketAddr) -> Option<()> {
        let mut locked = self.inner.state.lock().unwrap();
        let peers = locked.map.get_mut(&hash)?;
        peers.remove(&ip);
        return Some(());
    }

    fn done_job(&self, hash: InfoHash) {
        let mut locked = self.inner.state.lock().unwrap();
        locked.map.remove(&hash);
    }

    async fn download_for(&self, hash: InfoHash) {
        info!("Downloading metadata for {}", hash);
        while let Some(peer) = self.get_peer(hash) {
            let res = tokio::time::timeout(
                MAX_DOWNLOAD_TIME, 
                self.download(hash, peer)
            ).await;
            // Flattern the result..., translate the timeout error
            let res = match res {
                Ok(val) => val,
                Err(_) => {
                    let io_error = io::Error::new(
                        io::ErrorKind::TimedOut,
                        "It take too loong to download the meatadata"
                    );
                    Err(BtError::NetworkError(io_error))
                }
            };
            // Handle it
            let torrent = match res {
                Ok(torrent) => torrent,
                Err(e) => {
                    info!("Failed to download metadata from {}: {}", peer, e);
                    self.clear_peer( hash, peer);
                    continue;
                }
            };
            info!("Downloaded metadata hash {} from {}", hash, peer);


            // Save it
            self.done_job(hash);

            if let Some(controller) = self.inner.controller.wait().upgrade() {
                controller.on_metadata_downloaded(hash, torrent);
            }
            return;
        }
        info!("Downloading metadata for {} suspended, no more peers available", hash);
    }

    // Do the attually download for given stream and hash
    async fn do_download<T: AsyncRead + AsyncWrite + Unpin>(mut stream: BtStream<T>, hash: InfoHash) -> Result<Vec<u8>, BtError> {
        info!("Peer extension info: {:?}", stream.peer_info().extensions);
        let metadata_id = stream
            .peer_info()
            .metadata_id()
            .ok_or(BtError::UnsupportedExtension)?;
        let metadata_size = stream
            .peer_info()
            .metadata_size()
            .ok_or(BtError::UserDefined("Peer doesn't have metadata".into()))?;
        let local_metadata_id = stream
            .local_info()
            .metadata_id()
            .ok_or(BtError::UnsupportedExtension)?;

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
                info!("Ext message from peer: {id:?} {msg:?}");
                if id != local_metadata_id {
                    warn!("Unexpected message id {id}");
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

    async fn download(&self, hash: InfoHash, ip: SocketAddr) -> Result<Vec<u8>, BtError> {
        let mut info = BtHandshakeInfo {
            hash: hash,
            peer_id: PeerId::make(),
            extensions: None,
        };
        info.set_metadata_id(1); // We want to download the metadata use this

        // Try try utp first
        info!("Utp: Connecting to {ip}");
        if let Ok(utp) = UtpSocket::connect(&self.inner.utp_context, ip).await {
            let stream = BtStream::client_handshake(utp, info).await?;
            return Downloader::do_download(stream, hash).await;
        }
        // Try tcp ...
        info!("Tcp: Connecting to {ip}");
        let tcp = TcpStream::connect(ip).await?;
        let stream = BtStream::client_handshake(tcp, info).await?;
        return Downloader::do_download(stream, hash).await;
    }

    async fn handle_incoming(self, stream: UtpSocket) -> Result<(), BtError>{
        let controller = match self.inner.controller.wait().upgrade() {
            Some(val) => val,
            None => {
                // We can't do anything without controller
                return Err(BtError::UserDefined("Controller dropped".into()));
            }
        };
        let stream = BtStream::server_handshake(stream, |req| async move {
            info!("Handshake request: for hash {}", req.hash);
            if controller.has_metadata(req.hash) {
                info!("Already have metadata for {}", req.hash);
                return Err(BtError::HandshakeFailed);
            }
            let mut info = BtHandshakeInfo {
                hash: req.hash,
                peer_id: PeerId::make(),
                extensions: None,
            };
            info.set_metadata_id(1); // We want to download the metadata use this
            return Ok(info);
        }).await?;
        let hash = stream.peer_info().hash;
        let res = tokio::time::timeout(
        MAX_DOWNLOAD_TIME, 
        Downloader::do_download(stream, hash)
        ).await;
        let torrent = match res {
            Ok(val) => val,
            Err(_) => return Ok(()),
        }?;

        // Save it
        self.done_job(hash);
        if let Some(controller) = self.inner.controller.wait().upgrade() {
            controller.on_metadata_downloaded(hash, torrent);
        }
        return Ok(());

    }

    /// Add a new peer to the downloader
    pub fn add_peer(&self, hash: InfoHash, ip: SocketAddr) {
        debug_assert!(self.inner.controller.get().is_some(), "Controller should be set");

        let mut state = self.inner.state.lock().unwrap();
        let inserted = state.map.entry(hash).or_insert(BTreeSet::new()).insert(ip);
        if !inserted {
            return; // Duplicate
        }
        if state.workers.contains(&hash) {
            return;
        }
        // Start it
        self.inner.start_worker.send(hash).expect("It shouldn't fail");
    }

    /// Cancel a download
    pub fn cancel(&self, hash: InfoHash) {
        self.done_job(hash);
    }

    /// Set the controller
    pub fn set_controller(&self, controller: Weak<dyn DownloaderController + Send + Sync>) {
        let _ = self.inner.controller.set(controller);
    }
}
