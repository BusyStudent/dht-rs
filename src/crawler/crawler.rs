#![allow(dead_code, unused_imports)] // Let it shutup!

use crate::{
    bt::*,
    crawler::{
        downloader::{Downloader, DownloaderConfig, DownloaderController},
        peer_finder::{PeerFinder, PeerFinderConfig, PeerFinderController}, sampler::{Sampler, SamplerObserver},
    },
    dht::*,
    krpc::*,
    utp::UtpContext,
    InfoHash, NodeId,
};
use std::{
    collections::BTreeSet, io, net::SocketAddr, num::NonZero, sync::{atomic::{AtomicBool, Ordering}, Arc, Mutex, MutexGuard}
};
use async_trait::async_trait;
use tokio::{net::UdpSocket, sync::{Semaphore, mpsc}, task::JoinSet};
use tracing::{error, info, warn, trace};
use lru::LruCache;

struct CrawlerInner {
    udp: Arc<UdpSocket>,
    dht_session: DhtSession,
    utp_context: UtpContext,
    downloader: Downloader,
    peer_finder: PeerFinder,
    sampler: Sampler,
    controller: Arc<dyn CrawlerController + Send + Sync>,

    // State
    hash_lru: Mutex<LruCache<InfoHash, ()> >,
    auto_sample: AtomicBool,
    too_many_hash: AtomicBool,
}

/// The cralwer is responsible for collect info hash and download metadata
#[derive(Clone)]
pub struct Crawler {
    inner: Arc<CrawlerInner>,
}

pub struct CrawlerConfig {
    pub id: NodeId,
    pub ip: SocketAddr, // Bind addr
    pub hash_lru_cache_size: NonZero<usize>,
    pub controller: Arc<dyn CrawlerController + Send + Sync>,
    pub trackers: Vec<String>,
}

pub trait CrawlerController {
    /// Called when we found a new hash, it may duplicate because LruCache
    fn on_info_hash_found(&self, hash: InfoHash);

    /// Called when a metadata is downloaded
    fn on_metadata_downloaded(&self, hash: InfoHash, _data: Vec<u8>);

    /// Called when an message should be send to the ui
    fn on_message(&self, message: String);

    /// Check did we has this metadata?
    fn has_metadata(&self, hash: InfoHash) -> bool;
}

impl Crawler {
    pub async fn new(config: CrawlerConfig) -> Result<Crawler, io::Error> {
        let udp = Arc::new(UdpSocket::bind(config.ip).await?);
        let krpc = KrpcContext::new(udp.clone());
        let utp = UtpContext::new(udp.clone());
        let session = DhtSession::new(config.id, krpc);

        // The peerid, begin 2 bytes is for client, we use DI (DHT Indexer), then 4 bytes for version, then 12 bytes for random bytes.
        let mut id = [0u8; 20];
        id[0] = b'D';
        id[1] = b'I';
        id[2] = b'0';
        id[3] = b'0';
        id[4] = b'0';
        id[5] = b'1'; // Version 1
        for b in id[6..].iter_mut() {
            *b = fastrand::u8(..);
        }
        let id = PeerId::from(id);

        let finder_config = PeerFinderConfig {
            dht_session: session.clone(),
            udp_socket: udp.clone(),
            max_concurrent: 20, // 5 may be too small, use 20?
            max_retries: 2,
            bind_ip: config.ip,
            peer_id: id,
        };

        let downloader_config = DownloaderConfig {
            utp_context: utp.clone(),
            peer_id: id,
            bind_ip: config.ip,
        };

        let this = Crawler {
            inner: Arc::new(CrawlerInner {
                udp: udp,
                dht_session: session.clone(),
                utp_context: utp,
                downloader: Downloader::new(downloader_config),
                peer_finder: PeerFinder::new(finder_config),
                sampler: Sampler::new(session.clone()),
                controller: config.controller,

                hash_lru: Mutex::new(LruCache::new(config.hash_lru_cache_size)),
                auto_sample: AtomicBool::new(false),
                too_many_hash: AtomicBool::new(false),
            }),
        };

        let weak = Arc::downgrade(&this.inner);
        this.inner.peer_finder.set_controller(weak);

        // Set the observer for dht session
        let weak = Arc::downgrade(&this.inner);
        this.inner.dht_session.set_observer(weak);

        // Set the controller
        let weak = Arc::downgrade(&this.inner);
        this.inner.downloader.set_controller(weak);

        // Set the observer for sampler
        let weak = Arc::downgrade(&this.inner);
        this.inner.sampler.set_observer(weak);
        
        // Add Udp Tracker
        this.inner.peer_finder.add_trackers(config.trackers).await;

        return Ok(this);
    }

    async fn process_udp(&self) {
        let mut buf = [0u8; 65535];
        loop {
            let (n, addr) = match self.inner.udp.recv_from(&mut buf).await {
                Ok(val) => val,
                Err(err) => {
                    error!("Failed to recv data from udp socket: {}", err);
                    continue;
                }
            };
            if n == 0 {
                continue;
            }
            if buf[0] == b'd' { // Dict, almost krpc
                if self.inner.dht_session.process_udp(&buf[..n], &addr).await { // Successfully process it
                    continue;
                }
            }
            // Check is Udp Trakcer?
            if n >= 8 {
                // u32: action
                // u32: transaction id
                // payload...
                let action = u32::from_be_bytes(buf[0..4].try_into().unwrap());
                if action < 4 { // Connect(0) ... Announce(3)
                    if self.inner.peer_finder.process_udp(&buf[..n], &addr) {
                        continue;
                    }
                }
            }

            // Try utp?
            if self.inner.utp_context.process_udp(&buf[..n], &addr) {
                continue;
            }
            trace!("Unknown udp packet from {}: len: {}", addr, n);
        }
    }

    /// Get the routing table of the crawler
    pub fn dht_session(&self) -> &DhtSession {
        return &self.inner.dht_session;
    }

    /// Get the auto sample is enabled or not
    pub fn auto_sample(&self) -> bool {
        return self.inner.auto_sample.load(Ordering::Relaxed);
    }

    /// Add a info hash to the crawler, let the crawler find the peers and download the metadata
    pub fn add_hash(&self, info_hash: InfoHash) {
        return self.inner.peer_finder.add_hash(info_hash);
    }

    /// Enable or disable the auto sample, return the previous value
    pub fn set_auto_sample(&self, enable: bool) -> bool {
        return self.inner.auto_sample.swap(enable, Ordering::Relaxed);
    }

    /// Start the crawler
    pub async fn run(self) {
        tokio::join!(
            self.process_udp(),           // The network loop
            self.inner.dht_session.run(), // The dht loop
            self.inner.downloader.run(),  // The download loop
        );
    }
}

impl CrawlerInner {
    fn check_hash_lru(&self, hash: InfoHash) -> bool {
        let mut lru = self.hash_lru.lock().unwrap();
        return lru.put(hash, ()).is_none();
    }
}

// Delegate the downloader controller to the crawler
impl DownloaderController for CrawlerInner {
    fn on_metadata_downloaded(&self, hash: InfoHash, data: Vec<u8>) {
        self.peer_finder.cancel(hash); // Cancel the peer finding, we got the metadata
        return self.controller.on_metadata_downloaded(hash, data);
    }

    fn has_metadata(&self, hash: InfoHash) -> bool {
        return self.controller.has_metadata(hash);
    }
}

impl PeerFinderController for CrawlerInner {
    fn on_peers_found(&self, hash: InfoHash, peers: Vec<SocketAddr>) {
        if !self.controller.has_metadata(hash) {
            for peer in peers {
                self.downloader.add_peer(hash, peer);                
            }
        }
    }

    fn on_tasks_count_changed(&self, count: usize) {
        let new = match count {
            c if c > 1000 => true,
            c if c < 20 => false,
            _ => return, // Middle, do nothing
        };
        let old = self.too_many_hash.swap(new, Ordering::Relaxed);
        if old != new {
            let msg = if new { 
                "Too many hash, auto sample is disabled" 
            } 
            else { 
                "Hash count is normal, auto sample is enabled"
            };
            self.controller.on_message(msg.into());
            info!("{msg}");
        }
    }
}

#[async_trait]
impl DhtSessionObserver for CrawlerInner {
    /// From dht session
    async fn on_peer_announce(&self, hash: InfoHash, ip: SocketAddr) {
        if self.controller.has_metadata(hash) { // Already has the metadata
            return;
        }
        if self.check_hash_lru(hash) { // Not Exist in lru cache?
            self.controller.on_info_hash_found(hash);
            self.peer_finder.add_hash(hash);
        }
        self.downloader.add_peer(hash, ip);
    }

    async fn on_query(&self, _: &[u8], ip: SocketAddr) {
        // Try to sample it?
        if !self.auto_sample.load(Ordering::Relaxed) {
            return; // Auto sample is disabled
        }
        if self.too_many_hash.load(Ordering::Relaxed) {
            return; // Too many hash, don't sample
        }
        self.sampler.add_sample_node(ip);
    }
}

#[async_trait]
impl SamplerObserver for CrawlerInner {
    async fn on_hash_sampled(&self, hashes: Vec<InfoHash>) {
        for hash in hashes {
            if self.has_metadata(hash) {
                continue;
            }
            if self.check_hash_lru(hash) { // Not Exist in lru cache?
                // New, add to peer finder & notify
                self.controller.on_info_hash_found(hash);
                self.peer_finder.add_hash(hash);
                self.downloader.add_hash(hash); // Notify it we may have the incoming peer about this hash
            }
        }
    }
}