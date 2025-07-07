#![allow(dead_code)] // Let it shutup!

use crate::core::compact;
use crate::utp::{UtpContext, UtpSocket, UtpListener};
use crate::bt::{BtError, BtHandshakeInfo, BtMessage, BtStream, PeStream, PeError, PeerId, UtMetadataMessage};
use crate::InfoHash;
use crate::bencode::Object;
use lru::LruCache;
use sha1::Digest;
use tokio::io::AsyncRead;
use tokio::io::AsyncWrite;
use tokio::net::{TcpListener, TcpStream};
use tokio::sync::mpsc::error::TrySendError;
use tokio::sync::Semaphore;
use tokio::task::AbortHandle;
use tokio::task::JoinSet;
use tracing::{debug, instrument, warn};
use std::{
    net::{SocketAddr, IpAddr},
    sync::{Arc, Mutex, Weak, OnceLock, RwLock, atomic::AtomicU64},
    collections::HashMap,
    time::Duration,
};
use tokio::sync::{mpsc, watch};
use tracing::{error, info};

const MAX_WORKERS: usize = 10;
const MAX_CONCURRENT: usize = 10; // Max concurrent downloading for pre each worker
const MAX_PENDING_PEERS: usize = 100;
const MAX_DOWNLOAD_TIME: Duration = Duration::from_secs(60);

struct DownloaderInner {
    workers: Mutex<HashMap<InfoHash, (AbortHandle, mpsc::Sender<SocketAddr>) > >, // Mapping info hash to the workers
    controller: OnceLock<Weak<dyn DownloaderController + Sync + Send> >,
    sem: Semaphore, // For limit max running workers
    cancel_watch: OnceLock<watch::Receiver<bool> >,
    hashes: Mutex<LruCache<InfoHash, ()> >, // Lru for store downloading hash, for PeStream::handshake
    my_ips: RwLock<LruCache<IpAddr, ()> >, // Lru for store self ip, (from dht ext handshake yourip)
    utp_context: UtpContext,
    
    // Config
    peer_id: PeerId,
    bind_ip: SocketAddr,

    // Status
    connections: AtomicU64,
}

pub struct DownloaderConfig {
    pub utp_context: UtpContext,
    pub peer_id: PeerId,
    pub bind_ip: SocketAddr,
}

pub trait DownloaderController {
    fn on_metadata_downloaded(&self, info_hash: InfoHash, data: Vec<u8>);
    fn has_metadata(&self, info_hash: InfoHash) -> bool;
}

/// It is responsible for downloading the metadata of given hash and ip.
#[derive(Clone)]
pub struct Downloader {
    inner: Arc<DownloaderInner>,
}

struct CancelGuard {
    sender: watch::Sender<bool>
}

impl Downloader {
    // Create an downloader
    pub fn new(config: DownloaderConfig) -> Self {
        let _1k = 1000.try_into().unwrap();  // TODO: Make 1000 configurable
        let _3 = 3.try_into().unwrap();
        let downloader = Self { inner: Arc::new(DownloaderInner {
            workers: Mutex::new(HashMap::new()),
            controller: OnceLock::new(),
            sem: Semaphore::new(MAX_WORKERS),
            cancel_watch: OnceLock::new(),
            hashes: Mutex::new(LruCache::new(_1k)),
            my_ips: RwLock::new(LruCache::new(_3)),
            utp_context: config.utp_context,
            peer_id: config.peer_id,
            bind_ip: config.bind_ip,
            connections: AtomicU64::new(0),
        })};
        return downloader;
    }

    /// Run the downloader, guard for the cancel signal
    pub async fn run(&self) {
        let (sx, rx) = watch::channel(false);
        let _guard = CancelGuard { sender: sx };
        self.inner.cancel_watch.set(rx).unwrap(); // Set the cancel watch for the worker

        // Begin Listen
        tokio::join!(self.utp_listener_main(), self.tcp_listener_main() );
    }

    fn make_handshake_info(&self, hash: InfoHash, peer_addr: SocketAddr) -> BtHandshakeInfo {
        return BtHandshakeInfo {
            hash: hash,
            peer_id: self.inner.peer_id.clone(),
            extensions: Some(Object::from([
                (b"m".to_vec(), Object::from([(b"ut_metadata".to_vec(), Object::from(1))])),
                (b"v".to_vec(), Object::from(b"DHT Indexer")),
                (b"yourip".to_vec(), Object::from(compact::encode_your_ip(&peer_addr.ip()))),
            ]))
        }
    }

    fn is_self_ip(&self, addr: SocketAddr) -> bool {
        return self.inner.my_ips.read().unwrap().contains(&addr.ip());
    }

    async fn utp_listener_main(&self) {
        let mut listener = UtpListener::new(&self.inner.utp_context, 10); // MAX 10 pending.
        loop {
            let (stream, peer) = match listener.accept().await {
                Ok(val) => val,
                Err(e) => {
                    error!("Failed to use UtpListener::accept() => {e}");
                    return;
                }
            };
            tokio::spawn(self.clone().handle_incoming("Utp", stream, peer));
        }
    }

    async fn tcp_listener_main(&self) {
        let listener = match TcpListener::bind(&self.inner.bind_ip).await {
            Ok(val) => val,
            Err(e) => {
                error!("Failed to use TcpListener::bind() => {e}");
                return;
            }
        };
        loop {
            let (stream, peer) = match listener.accept().await {
                Ok(val) => val,
                Err(e) => {
                    error!("Failed to use TcpListener::accept() => {e}");
                    return;
                }
            };
            tokio::spawn(self.clone().handle_incoming("Tcp", stream, peer));
        }
    }

    // Pulling the job from the queue
    #[instrument(skip(self, receiver))]
    async fn worker_main(&self, hash: InfoHash, mut receiver: mpsc::Receiver<SocketAddr>) {
        let _premit = match self.inner.sem.acquire().await {
            Ok(val) => val,
            Err(_) => return,
        };
        let mut set = JoinSet::new();
        let mut once_empty = false; // Once empty, we can exit
        loop {
            // Try recv until reach the max concurrent and start more task
            while set.len() < MAX_CONCURRENT {
                match receiver.try_recv() {
                    Ok(peer) => {
                        let task = self.clone().download(hash, peer);
                        let task = tokio::time::timeout(MAX_DOWNLOAD_TIME, task); // Add an timeout
                        once_empty = false;
                        set.spawn(task);
                    },
                    Err(_) => break,
                }
            }
            let compelete = match set.join_next().await {
                Some(val) => val.expect("The task could not be canceled"),
                None => {
                    if once_empty { // No more job, exit
                        break;
                    }
                    once_empty = true;
                    tokio::time::sleep(Duration::from_millis(100)).await; // Wait for a while
                    continue;
                },
            };
            let compelete = match compelete {
                Ok(val) => val,
                Err(_) => continue, // Timeout...
            };
            if let Ok(data) = compelete {
                if let Some(controller) = self.inner.controller.wait().upgrade() {
                    controller.on_metadata_downloaded(hash, data);
                }
                break; // Downloaded, exit
            }
        }
        set.shutdown().await; // Cleanup....
    }

    // Do the attually download for given stream and hash
    #[instrument(skip_all, fields(pieces, cur_index))]
    async fn download_impl<T: AsyncRead + AsyncWrite + Unpin>(&self, mut stream: BtStream<T>, hash: InfoHash) -> Result<Vec<u8>, BtError> {
        info!("Peer extension info: {:?}", stream.peer_info().extensions);
        // Emm get yourip here
        if let Some(ip) = stream.peer_info().extensions.as_ref()
            .and_then(|e| e.get(b"yourip") )
            .and_then(|s| s.as_string() )
            .and_then(|s| compact::parse_your_ip(&s))
        {
            // Set our ip to lru
            // info!("Self ip: {ip}");
            self.inner.my_ips.write().unwrap().put(ip, ());
        }
        let span = tracing::Span::current();

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
        span.record("pieces", pieces);
        for i in 0..pieces {
            span.record("cur_index", i);
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
                    UtMetadataMessage::Reject { piece } => {
                        if piece != i {
                            error!("Got wrong piece {piece}");
                            return Err(BtError::InvalidMessage);
                        }
                        return Err(BtError::UserDefined("Peer reject too many times".into()));
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
            warn!("Hash mismatch ?");
            return Err(BtError::UserDefined("Hash mismatch".into()));
        }
        return Ok(torrent);
    }

    #[instrument(skip(self))]
    async fn download(self, hash: InfoHash, ip: SocketAddr) -> Result<Vec<u8>, BtError> {
        let info = self.make_handshake_info(hash, ip);
        // Try try utp first
        debug!("Utp: Connecting to {ip} for hash {hash}");
        if let Ok(utp) = UtpSocket::connect(&self.inner.utp_context, ip).await {
            let pe = PeStream::client_handshake(utp, hash).await?;
            let stream = BtStream::client_handshake(pe, info).await?;
            return self.download_impl(stream, hash).await;
        }
        // Try tcp ...
        debug!("Tcp: Connecting to {ip} for hash {hash}");
        let tcp = TcpStream::connect(ip).await?;
        let stream = BtStream::client_handshake(tcp, info).await?;
        return self.download_impl(stream, hash).await;
    }

    async fn handle_incoming_impl<T: AsyncRead + AsyncWrite + Unpin>(&self, stream: T, ip: SocketAddr) -> Result<InfoHash, BtError> {
        let controller = match self.inner.controller.wait().upgrade() {
            Some(val) => val,
            None => {
                // We can't do anything without controller
                return Err(BtError::UserDefined("Controller dropped".into()));
            }
        };
        let this = self.clone();
        let stream = BtStream::server_handshake(stream, async move |req| {
            debug!("Handshake request: for hash {}", req.hash);
            if controller.has_metadata(req.hash) {
                debug!("Already have metadata for {}", req.hash);
                return Err(BtError::UserDefined("Already have metadata".into()));
            }
            let info = this.make_handshake_info(req.hash, ip);
            return Ok(info);
        }).await?;
        let hash = stream.peer_info().hash;
        let res = tokio::time::timeout(MAX_DOWNLOAD_TIME, self.download_impl(stream, hash)).await;
        let torrent = match res {
            Ok(val) => val,
            Err(_) => return Err(BtError::UserDefined("TimedOut".into())),
        }?;

        // Save it
        self.cancel(hash); // The hash is collected, no need to keep the worker
        if let Some(controller) = self.inner.controller.wait().upgrade() {
            controller.on_metadata_downloaded(hash, torrent);
        }
        return Ok(hash);

    }

    #[instrument(skip_all, fields(proto = %proto, peer = %peer, encryption))]
    async fn handle_incoming<T: AsyncRead + AsyncWrite + Unpin>(self, proto: &'static str, stream: T, peer: SocketAddr) {
        // Try PE?
        let collect_hashes = || {
            let hashes = self.inner.hashes.lock().unwrap();
            let mut res = Vec::new();
            for (hash, _) in hashes.iter() {
                res.push(hash.clone());
            }
            return res.into_iter();
        };
        let stream = match PeStream::server_handshake(stream, collect_hashes).await {
            Ok(stream) => {
                if stream.has_encryption() { // Encrypted,
                    debug!("Got PE handshake from peer");
                }
                stream
            }
            // This part, no importance
            Err(PeError::NetworkError(e)) => {
                debug!("Failed to pe handshake: network error: {e}");
                return;
            }
            Err(PeError::InfoHashNotFound) => {
                debug!("Failed to pe handshake: info hash not found");
                return;
            }
            // Emm?, May our impl is wrong?, so use warn
            Err(PeError::HanshakeFailed) => {
                warn!("Failed to pe handshake: handshake failed");
                return;
            }
            // Err(pe) => {
            //     debug!("Failed to pe handshake with error: {pe}");
            //     return;
            // }
        };
        if stream.has_encryption() {
            tracing::Span::current().record("encryption", "true");
        }
        match self.handle_incoming_impl(stream, peer).await {
            Ok(hash) => {
                info!("Successfully downloaded metadata {hash}");
            }
            Err(BtError::InvalidProtocolString(str)) => {
                warn!("Invalid protocol string from peer: str: {str:?}");
            }
            Err(err) => { // Another error
                debug!("Failed to get any metadata from error: {err}");
            }
        }
        return;
    }

    /// Add a new peer to the downloader
    pub fn add_peer(&self, hash: InfoHash, ip: SocketAddr) {
        debug_assert!(self.inner.controller.get().is_some(), "Controller should be set");
        if self.is_self_ip(ip) {
            // Self, ignore it
            return;
        }
        
        // Add it to the lru, for decrypting the PeStream
        self.add_hash(hash);
        let mut workers = self.inner.workers.lock().unwrap();
        if let Some((_, sender)) = workers.get(&hash) {
            // Already has worker for this hash
            match sender.try_send(ip) {
                Ok(_) => return,
                Err(TrySendError::Full(_)) => { // FIXME: It may have a race condition, the worker is going to be dropped and we try to send to it
                    error!("Worker for {hash} is too busy");
                    return;
                },
                Err(TrySendError::Closed(_)) => {
                    error!("Worker for {hash} is closed");
                    return;
                }
            }
        }
        // Start an new worker
        let (sender, receiver) = mpsc::channel(MAX_PENDING_PEERS);
        let mut watch = self.inner.cancel_watch.wait().clone(); // For support cancellation
        let this = self.clone();
        sender.try_send(ip).expect("It should not fail"); // Send the first peer
        let handle = tokio::spawn(async move {
            tokio::select! {
                _ = watch.changed() => {
                    debug!("Worker for {hash} cancelled");
                }
                _ = this.worker_main(hash, receiver) => {
                    debug!("Worker for {hash} exited");
                }
            }
            this.cancel(hash); // Doing cleanup
        });
        // Insert to the workers
        workers.insert(hash, (handle.abort_handle(), sender));
    }

    /// Add an hash to the internal lru cache, it will used to decrypt the PeStream
    pub fn add_hash(&self, hash: InfoHash) {
        self.inner.hashes.lock().unwrap().push(hash, ());
    }

    /// Cancel a download
    pub fn cancel(&self, hash: InfoHash) {
        if let Some((handle, _)) = self.inner.workers.lock().unwrap().remove(&hash) {
            handle.abort();
        }
        // Remove from the hash_lru
        self.inner.hashes.lock().unwrap().pop(&hash);
    }

    /// Set the controller
    pub fn set_controller(&self, controller: Weak<dyn DownloaderController + Send + Sync>) {
        let _ = self.inner.controller.set(controller);
    }
}

impl Drop for CancelGuard {
    fn drop(&mut self) {
        let _ = self.sender.send(true);
    }
}
