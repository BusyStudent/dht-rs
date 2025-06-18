#![allow(dead_code, unused_imports)] // Let it shutup!

use crate::{
    bt::*,
    crawler::{
        downloader::{Downloader, DownloaderController},
        peer_finder::{PeerFinder, PeerFinderConfig}, sampler::{Sampler, SamplerObserver},
    },
    dht::*,
    krpc::*,
    utp::UtpContext,
    InfoHash, NodeId,
};
use std::{
    collections::BTreeSet,
    io,
    net::SocketAddr,
    sync::{atomic::{AtomicBool, Ordering}, Arc, Mutex, MutexGuard},
};
use async_trait::async_trait;
use tokio::{net::UdpSocket, sync::{Semaphore, mpsc}, task::JoinSet};
use tracing::{error, info};

struct CrawlerInner {
    udp: Arc<UdpSocket>,
    dht_session: DhtSession,
    utp_context: UtpContext,
    downloader: Downloader,
    peer_finder: PeerFinder,
    sampler: Sampler,
    observer: Arc<dyn CrawlerObserver + Send + Sync>,

    auto_sample: AtomicBool,
}

/// The cralwer is responsible for collect info hash and download metadata
#[derive(Clone)]
pub struct Crawler {
    inner: Arc<CrawlerInner>,
}

pub struct CrawlerConfig {
    pub id: NodeId,
    pub ip: SocketAddr, // Bind addr
    pub observer: Arc<dyn CrawlerObserver + Send + Sync>,
}

pub trait CrawlerObserver {
    /// Called when a peer announce a info hash, return false if the info hash is already in the list
    fn on_info_hash_found(&self, _info_hash: InfoHash) -> bool {
        return true;
    }

    /// Called when a metadata is downloaded
    fn on_metadata_downloaded(&self, _info_hash: InfoHash, _data: Vec<u8>);

    /// Check did we has this metadata?
    fn has_metadata(&self, info_hash: InfoHash) -> bool;
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
            max_concurrent: 20, // 5 may be too small, use 20?
            max_retries: 2,
        };

        let this = Crawler {
            inner: Arc::new(CrawlerInner {
                udp: udp,
                dht_session: session.clone(),
                utp_context: utp.clone(),
                downloader: Downloader::new(utp, id),
                peer_finder: PeerFinder::new(finder_config),
                sampler: Sampler::new(session.clone()),
                observer: config.observer,

                auto_sample: AtomicBool::new(true),
            }),
        };

        let weak = Arc::downgrade(&this.inner);
        this.inner.peer_finder.set_callback(Box::new(move |info_hash, addr| match weak.upgrade() {
            Some(this) => Crawler::on_peer_found(this, info_hash, addr),
            None => {}
        }));

        // Set the observer for dht session
        let weak = Arc::downgrade(&this.inner);
        this.inner.dht_session.set_observer(weak);

        // Set the controller
        let weak = Arc::downgrade(&this.inner);
        this.inner.downloader.set_controller(weak);

        // Set the observer for sampler
        let weak = Arc::downgrade(&this.inner);
        this.inner.sampler.set_observer(weak);

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
                // Try utp?
            }
            if self.inner.utp_context.process_udp(&buf[..n], &addr) {
                continue;
            }
        }
    }

    /// From peer finder
    fn on_peer_found(inner: Arc<CrawlerInner>, info_hash: InfoHash, ip: SocketAddr) {
        if !inner.observer.has_metadata(info_hash) {
            inner.downloader.add_peer(info_hash, ip);
        }
    }

    /// Get the routing table of the crawler
    pub fn dht_session(&self) -> &DhtSession {
        return &self.inner.dht_session;
    }
    
    /// Add a info hash to the crawler, let the crawler find the peers and download the metadata
    pub fn add_hash(&self, info_hash: InfoHash) {
        return self.inner.peer_finder.add_hash(info_hash);
    }

    /// Enable or disable the auto sample
    pub fn set_auto_sample(&self, enable: bool) {
        self.inner.auto_sample.store(enable, Ordering::Relaxed);
    }

    /// Start the crawler
    pub async fn run(self) {
        tokio::join!(
            self.process_udp(),           // The network loop
            self.inner.dht_session.run(), // The dht loop
            self.inner.downloader.run(),  // The download loop
            self.inner.sampler.run()      // The sampler loop
        );
    }
}

// TODO.. Delegate the weak to avoid memory leak

// Delegate the downloader controller to the crawler
impl DownloaderController for CrawlerInner {
    fn on_metadata_downloaded(&self, info_hash: InfoHash, data: Vec<u8>) {
        self.peer_finder.cancel(info_hash); // Cancel the peer finding, we got the metadata
        return self.observer.on_metadata_downloaded(info_hash, data);
    }

    fn has_metadata(&self, info_hash: InfoHash) -> bool {
        return self.observer.has_metadata(info_hash);
    }
}

#[async_trait]
impl DhtSessionObserver for CrawlerInner {
    /// From dht session
    async fn on_peer_announce(&self, info_hash: InfoHash, ip: SocketAddr) {
        if self.observer.has_metadata(info_hash) { // Already has the metadata
            return;
        }
        if self.observer.on_info_hash_found(info_hash) {
            // New, add to peer finder
            self.peer_finder.add_hash(info_hash);
        }
        self.downloader.add_peer(info_hash, ip);
    }

    async fn on_query(&self, _: &[u8], ip: SocketAddr) {
        // Try to sample it?
        if self.auto_sample.load(Ordering::Relaxed) {
            self.sampler.add_sample_node(ip);
        }
    }
}

#[async_trait]
impl SamplerObserver for CrawlerInner {
    async fn on_hash_sampled(&self, hashes: Vec<InfoHash>) {
        for hash in hashes {
            if self.has_metadata(hash) {
                continue;
            }
            if self.observer.on_info_hash_found(hash) {
                // New, add to peer finder
                self.peer_finder.add_hash(hash);
            }
        }
    }
}