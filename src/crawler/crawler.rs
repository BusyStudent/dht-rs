#![allow(dead_code, unused_imports)] // Let it shutup!

use crate::{
    bt::*, crawler::downloader::Downloader, dht::*, krpc::*, utp::UtpContext, InfoHash, NodeId
};
use std::{
    collections::BTreeSet, 
    io, 
    net::SocketAddr, 
    sync::{Arc, Mutex, MutexGuard}
};
use tracing::{info, error};
use tokio::{
    net::UdpSocket,
    sync::Semaphore,
    task::JoinSet,
};

struct CrawlerInner {
    udp: Arc<UdpSocket>,
    dht_session: DhtSession,
    utp_context: UtpContext,
    downloader: Downloader,
    observer: Arc<dyn CrawlerObserver + Send + Sync>,
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

    fn on_metadata_downloaded(&self, _info_hash: InfoHash, _data: Vec<u8>) {
        // 
    }

    /// Check did we has this metadata?
    fn has_metadata(&self, _info_hash: InfoHash) -> bool {
        return false;
    }
}

impl Crawler {
    pub async fn new(config: CrawlerConfig) -> Result<Crawler, io::Error> {
        let udp = Arc::new(UdpSocket::bind(config.ip).await?);
        let krpc = KrpcContext::new(udp.clone());
        let utp = UtpContext::new(udp.clone());
        let mut session = DhtSession::new(config.id, krpc);

        let this = Crawler {
            inner: Arc::new(CrawlerInner {
                udp: udp,
                dht_session: session.clone(),
                utp_context: utp.clone(),
                downloader: Downloader::new(utp, config.observer.clone()),
                observer: config.observer,
            }),
        };

        // Set the callback of it
        let weak = Arc::downgrade(&this.inner);
        session.set_on_peer_announce(Box::new(move |info_hash, addr| {
            match weak.upgrade() {
                Some(this) => Crawler::on_peer_announce(this, info_hash, addr),
                None => {}
            }
        }));

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

    fn on_peer_announce(inner: Arc<CrawlerInner>, info_hash: InfoHash, ip: SocketAddr) {
        inner.observer.on_info_hash_found(info_hash);
        if !inner.observer.has_metadata(info_hash) {
            inner.downloader.add_peer(info_hash, ip);
        }
    }

    /// Get the routing table of the crawler
    pub fn dht_session(&self) -> &DhtSession {
        return &self.inner.dht_session;
    }

    /// Start the crawler
    pub async fn run(self) {
        tokio::join!(
            self.process_udp(), // The network loop
            self.inner.dht_session.run(), // The dht loop
            self.inner.downloader.run()   // The download loop
        );
    }
}
