#![allow(dead_code)] // Let it shutup!

use crate::utp::{UtpContext, UtpSocket, UtpListener};
use crate::bt::{BtStream, BtHandshakeInfo, BtMessage, BtError, PeerId, UtMetadataMessage};
use crate::InfoHash;
use crate::bencode::Object;
use sha1::Digest;
use tokio::io::AsyncRead;
use tokio::io::AsyncWrite;
use tokio::net::TcpStream;
use tokio::sync::mpsc::error::TrySendError;
use tokio::sync::Semaphore;
use tokio::task::AbortHandle;
use tokio::task::JoinSet;
use tracing::warn;
use std::collections::HashMap;
use std::{
    net::SocketAddr,
    sync::{Arc, Mutex, Weak, OnceLock},
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
    utp_context: UtpContext,
    peer_id: PeerId,
}

// pub struct DownloaderConfig {
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

struct CancelGuard {
    sender: watch::Sender<bool>
}

impl Downloader {
    // Create an downloader
    pub fn new(ctxt: UtpContext, id: PeerId) -> Self {
        let downloader = Self { inner: Arc::new(DownloaderInner {
            workers: Mutex::new(HashMap::new()),
            controller: OnceLock::new(),
            sem: Semaphore::new(MAX_WORKERS),
            cancel_watch: OnceLock::new(),
            utp_context: ctxt,
            peer_id: id,
        })};
        return downloader;
    }

    /// Run the downloader, guard for the cancel signal
    pub async fn run(&self) {
        let (sx, rx) = watch::channel(false);
        let _guard = CancelGuard { sender: sx };
        self.inner.cancel_watch.set(rx).unwrap(); // Set the cancel watch for the worker

        // Begin Listen
        self.listener_main().await;
    }

    fn make_handshake_info(&self, hash: InfoHash) -> BtHandshakeInfo {
        return BtHandshakeInfo {
            hash: hash,
            peer_id: self.inner.peer_id.clone(),
            extensions: Some(Object::from([
                (b"m".to_vec(), Object::from([(b"ut_metadata".to_vec(), Object::from(1))])),
                (b"v".to_vec(), Object::from(b"DHT Indexer"))
            ]))
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

    // Pulling the job from the queue
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

    async fn download(self, hash: InfoHash, ip: SocketAddr) -> Result<Vec<u8>, BtError> {
        let info = self.make_handshake_info(hash);
        // Try try utp first
        info!("Utp: Connecting to {ip} for hash {hash}");
        if let Ok(utp) = UtpSocket::connect(&self.inner.utp_context, ip).await {
            let stream = BtStream::client_handshake(utp, info).await?;
            return Downloader::do_download(stream, hash).await;
        }
        // Try tcp ...
        info!("Tcp: Connecting to {ip} for hash {hash}");
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
        let this = self.clone();
        let stream = BtStream::server_handshake(stream, |req| async move {
            info!("Handshake request: for hash {}", req.hash);
            if controller.has_metadata(req.hash) {
                info!("Already have metadata for {}", req.hash);
                return Err(BtError::HandshakeFailed);
            }
            let info = this.make_handshake_info(req.hash);
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
        self.cancel(hash); // The hash is collected, no need to keep the worker
        if let Some(controller) = self.inner.controller.wait().upgrade() {
            controller.on_metadata_downloaded(hash, torrent);
        }
        return Ok(());

    }

    /// Add a new peer to the downloader
    pub fn add_peer(&self, hash: InfoHash, ip: SocketAddr) {
        debug_assert!(self.inner.controller.get().is_some(), "Controller should be set");
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
                    info!("Worker for {hash} cancelled");
                }
                _ = this.worker_main(hash, receiver) => {
                    info!("Worker for {hash} exited");
                }
            }
            this.cancel(hash); // Doing cleanup
        });
        // Insert to the workers
        workers.insert(hash, (handle.abort_handle(), sender));
    }

    /// Cancel a download
    pub fn cancel(&self, hash: InfoHash) {
        // self.done_job(hash);
        if let Some((handle, _)) = self.inner.workers.lock().unwrap().remove(&hash) {
            handle.abort();
        }
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
