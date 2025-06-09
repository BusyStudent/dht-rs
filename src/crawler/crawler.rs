#![allow(dead_code, unused_imports)] // Let it shutup!

use crate::{
    bt::*, 
    dht::*, 
    krpc::*, 
    InfoHash, 
    NodeId
};
use std::{
    collections::BTreeSet, 
    io, 
    net::SocketAddr, 
    sync::{Arc, Mutex, MutexGuard}
};
use tracing::info;
use tokio::net::UdpSocket;

struct CrawlerInner {
    udp: Arc<UdpSocket>,
    dht_session: DhtSession,
    observer: Arc<dyn CrawlerObserver + Send + Sync>,

    // For 
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
}

impl Crawler {
    pub async fn new(config: CrawlerConfig) -> Result<Crawler, io::Error> {
        let udp = Arc::new(UdpSocket::bind(config.ip).await?);
        let krpc = KrpcContext::new(udp.clone());
        let mut session = DhtSession::new(config.id, krpc);

        let this = Crawler {
            inner: Arc::new(CrawlerInner {
                udp: udp.clone(),
                dht_session: session.clone(),
                observer: config.observer
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
            let (n, addr) = self.inner.udp.recv_from(&mut buf).await.unwrap();
            self.inner.dht_session.process_udp(&buf[..n], &addr).await;
        }
    }

    fn on_peer_announce(inner: Arc<CrawlerInner>, info_hash: InfoHash, _addr: SocketAddr) {
        let new = {
            // info!("Found info hash: {}", info_hash);
            inner.observer.on_info_hash_found(info_hash)
        };
        if !new { // Already in the list
            return;
        }
    }

    /// Get the routing table of the crawler
    pub fn dht_session(&self) -> &DhtSession {
        return &self.inner.dht_session;
    }

    /// Start the crawler
    pub async fn run(self) {
        tokio::join!(self.process_udp(), self.inner.dht_session.run());
    }
}
