// FIXME: It may bug here, the tid mismatch in many times...
use tokio::net::UdpSocket;
use thiserror::Error;
use tokio::sync::broadcast;
use tokio::sync::oneshot;
use tracing::warn;

use crate::InfoHash;
use crate::PeerId;
use std::collections::HashMap;
use std::net::IpAddr;
use std::net::Ipv4Addr;
use std::net::Ipv6Addr;
use std::net::SocketAddr;
use std::sync::atomic::{AtomicU32, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;
use std::time::Instant;

// All the constants are from BitTorrent spec
const CONNECTION_EXPIRY: Duration = Duration::from_secs(60);
const PROTOCOL_ID: u64 = 0x41727101980;
const MAX_SCRAPE: usize = 74;

// Our 
const MAX_TIMEOUT: Duration = Duration::from_secs(10);

// Request
// u32: action
// u32: transaction id
// data left...

#[repr(u32)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Action {
    Connect  = 0,
    Announce = 1,
    Scrape   = 2,
    Error    = 3,
}

#[repr(u32)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Event {
    None      = 0,
    Completed = 1,
    Started   = 2,
    Stopped   = 3,
}

#[derive(Default)]
struct ConnectionIdState {
    id: Option<(u64, Instant)>,
    worker: Option<broadcast::Sender<Result<u64, TrackerError> >  >
}

struct UdpTrackerInner {
    addr: SocketAddr,
    sockfd: Arc<UdpSocket>, // Used for sending and receiving

    // State...
    pending: Mutex<HashMap<
        u32, 
        oneshot::Sender<
            (u32, Vec<u8>)
        > 
    > >, // Mapping the transation id to the sender ,The Vec<u8> is the reply data (without action and transaction id)

    tid: AtomicU32, // Transaction id, added by 1 each time
    key: u32,
    con_state: Mutex<ConnectionIdState>,
}

#[derive(Clone)]
pub struct UdpTracker {
    inner: Arc<UdpTrackerInner>,
}


#[derive(Debug, Clone)]
pub struct AnnounceInfo {
    pub hash: InfoHash,
    pub peer_id: PeerId,
    pub port: u16,

    pub downloaded: u64,
    pub uploaded: u64,
    pub left: u64,
    pub event: Event,

    pub num_want: Option<u32>, // None means default (-1 in the proto)
}

pub struct AnnounceResult {
    pub interval: u32,
    pub seeders: u32,
    pub leechers: u32,
    pub peers: Vec<SocketAddr>,
}

#[derive(Debug, Clone, Copy)]
pub struct ScrapedItem {
    pub seeders: u32,
    pub leechers: u32,
    pub completed: u32,
}

#[derive(Error, Debug, Clone)]
pub enum TrackerError {
    #[error("Invalid request to tracker, we can't send more than {MAX_SCRAPE} scrape requests at once")]
    MaxScrapeReached,

    #[error("Error reply from tracker: {0}")]
    Error(String),

    #[error("Invalid reply from tracker")]
    InvalidReply,

    #[error("Unknown error")]
    Unknown,

    #[error("Request Timed out")]
    TimedOut,

    #[error("Network error: {0}")]
    NetworkError(String) // The io::Error is not cloneable :(, use String to store the message
}

struct CancelGuard<'a> {
    tid: u32,
    tracker: &'a UdpTrackerInner,
    disarm: bool,
}

impl UdpTracker {
    pub fn new(addr: SocketAddr, sockfd: Arc<UdpSocket>) -> Self {
        return Self { inner: Arc::new(UdpTrackerInner {
            addr: addr,
            sockfd: sockfd,
            pending: Mutex::new(HashMap::new()),
            tid: AtomicU32::new(0),
            key: fastrand::u32(..),
            con_state: Mutex::new(ConnectionIdState {
                ..Default::default()
            })
        })};
    }

    // Get the peer address of the tracker
    pub fn peer_addr(&self) -> SocketAddr {
        return self.inner.addr;
    }

    // Do a connect request to the tracker and return the connection id
    async fn connect(&self) -> Result<u64, TrackerError> {
        // Connect Query
        // u64: protocol id
        // u32: action
        // u32: transaction id
        let bytes = self.send_request(PROTOCOL_ID, Action::Connect, &[]).await?;
        let con_id = match bytes[..].try_into() {
            Err(_) => return Err(TrackerError::InvalidReply),
            Ok(id) => u64::from_be_bytes(id),
        };
        debug_assert!(con_id != PROTOCOL_ID); // Check will be better?
        return Ok(con_id);
    }

    async fn connect_worker(self, sender: broadcast::Sender<Result<u64, TrackerError> >) {
        let id = self.connect().await;
        let mut state = self.inner.con_state.lock().unwrap();
        // Update it if
        if let Ok(id) = id.as_ref() {
            state.id = Some((*id, Instant::now()));
        }
        state.worker = None; // Clear the worker
        drop(state);

        let _ = sender.send(id); // Got it, broadcast to all the subscribers need this connection id
    } 

    // Get the connection id in the tracker, if already haven, return it, if not or expired, do a connect request
    async fn connection_id(&self) -> Result<u64, TrackerError> {
        let mut receiver = {
            let mut state = self.inner.con_state.lock().unwrap();
            if let Some((id, time)) = state.id {
                if time.elapsed() < CONNECTION_EXPIRY {
                    return Ok(id);
                }
                state.id = None; // Clear the connection id, it is expired
            }
            
            // Start one or wait for the connect worker
            match state.worker.as_ref() {
                Some(sender) => sender.subscribe(),
                None => {
                    let (tx, rx) = broadcast::channel(1);
                    tokio::spawn(self.clone().connect_worker(tx.clone()));
                    state.worker = Some(tx);

                    rx
                }
            }
        };
        // Wait for the connection id
        return receiver.recv().await.map_err(|_| TrackerError::TimedOut)?;
    }

    // Send a request to the tracker and wait for the reply
    async fn send_request_impl(&self, connection_id: u64, action: Action, data: &[u8]) -> Result<Vec<u8>, TrackerError> {
        #[cfg(debug_assertions)]
        if action == Action::Connect && connection_id != PROTOCOL_ID {
            panic!("Connection id must be PROTOCOL_ID when action is Connect");
        }

        let mut buf = Vec::new();
        let tid = self.inner.tid.fetch_add(1, Ordering::SeqCst);
        
        // u64: connection id (or protocol id for connect)
        // u32: action
        // u32: transaction id
        buf.extend_from_slice(&connection_id.to_be_bytes());
        buf.extend_from_slice(&(action as u32).to_be_bytes());
        buf.extend_from_slice(&tid.to_be_bytes());

        // Payload left
        buf.extend_from_slice(data);

        // Request build done, now we send it
        let (tx, rx) = oneshot::channel();
        let mut guard = CancelGuard::new(tid, &self.inner);
        self.inner.pending.lock().unwrap().insert(tid, tx);
        self.inner.sockfd.send_to(&buf, &self.inner.addr)
            .await
            .map_err(|e| TrackerError::NetworkError(e.to_string()) )?;

        let (ret_action, data) = match rx.await {
            Ok(val) => val,
            Err(e) => {
                warn!("WTF, the sender is dropped? {}", e);
                return Err(TrackerError::Unknown);
            },
        };
        // Done, async get the result, now we can remove the guard
        guard.disarm();

        if ret_action == Action::Error as u32 { // Error
            let err = String::from_utf8_lossy(&data[..]).to_string();
            return Err(TrackerError::Error(err));
        }
        if ret_action != action as u32 { // Emm?, Why?
            return Err(TrackerError::InvalidReply);
        }
        return Ok(data);
    }

    async fn send_request(&self, connection_id: u64, action: Action, data: &[u8]) -> Result<Vec<u8>, TrackerError> {
        match tokio::time::timeout(MAX_TIMEOUT, self.send_request_impl(connection_id, action, data)).await {
            Ok(val) => return val,
            Err(_) => return Err(TrackerError::TimedOut),
        }
    }
    
    pub fn process_udp(&self, data: &[u8], addr: &SocketAddr) -> bool {
        // Reply
        // u32: action
        // u32: transaction id
        // data left...
        if addr.ip() != self.inner.addr.ip() { // Emm?, did we need this? I think yes...
            return false;
        }
        if data.len() < 8 {
            return false;
        }
        let action = u32::from_be_bytes(data[0..4].try_into().unwrap());
        let tid = u32::from_be_bytes(data[4..8].try_into().unwrap());

        // Ok then we try to find the transaction id in the pending map
        let sender = self.inner.pending.lock().unwrap().remove(&tid);
        if let Some(sender) = sender {
            let bytes = data[8..].to_vec();
            let _ = sender.send((action, bytes));
            return true;
        }
        warn!("Trakcer for {}, get a reply with unknown tid: {}", self.inner.addr, tid);
        return true; // Valid packet, belong us, but the tid did not match
    }

    // Do the announce request to the tracker and return the list of peers
    pub async fn announce(&self, info: AnnounceInfo) -> Result<AnnounceResult, TrackerError> {
        // 20B: info_hash
        // 20B: peer_id
        // u64: downloaded
        // u64: left
        // u64: uploaded
        // u32: event
        // u32: IP address (optional)
        // u32: key (random id per client)
        // i32: num_want (-1 = default)
        // u16: port
        let num_want = if let Some(val) = info.num_want { val as i32 } else { -1 };
        let mut buf = Vec::with_capacity(98);
        buf.extend_from_slice(info.hash.as_slice());
        buf.extend_from_slice(info.peer_id.as_slice());
        buf.extend_from_slice(&info.downloaded.to_be_bytes());
        buf.extend_from_slice(&info.left.to_be_bytes());
        buf.extend_from_slice(&info.uploaded.to_be_bytes());
        buf.extend_from_slice(&(info.event as u32).to_be_bytes());
        buf.extend_from_slice(&0u32.to_be_bytes());
        buf.extend_from_slice(&self.inner.key.to_be_bytes());
        buf.extend_from_slice(&num_want.to_be_bytes());
        buf.extend_from_slice(&info.port.to_be_bytes());

        let con_id = self.connection_id().await?;
        let bytes = self.send_request(con_id, Action::Announce, &buf).await?;

        // u32: interval
        // u32: leechers
        // u32: seeders
        // 6B or 18B: ip address (optional)

        if bytes.len() < 12 {
            return Err(TrackerError::InvalidReply);
        }
        let interval = u32::from_be_bytes(bytes[0..4].try_into().unwrap());
        let leechers = u32::from_be_bytes(bytes[4..8].try_into().unwrap());
        let seeders = u32::from_be_bytes(bytes[8..12].try_into().unwrap());
        let left = &bytes[12..];
        let mut peers = Vec::new();

        if left.len() % 6 == 0 { // IPV4
            for chunk in left.chunks_exact(6) {
                let ip: [u8; 4] = chunk[0..4].try_into().unwrap();
                let port: [u8; 2] = chunk[4..6].try_into().unwrap();

                peers.push(SocketAddr::new(IpAddr::V4(Ipv4Addr::from(ip)), u16::from_be_bytes(port)));
            }
        }
        else if left.len() % 18 == 0 { // IPV6
            for chunk in left.chunks_exact(18) {
                let ip: [u8; 16] = chunk[0..16].try_into().unwrap();
                let port: [u8; 2] = chunk[16..18].try_into().unwrap();
                
                peers.push(SocketAddr::new(IpAddr::V6(Ipv6Addr::from(ip)), u16::from_be_bytes(port)));
            }
        }
        else {
            return Err(TrackerError::InvalidReply);
        }
    
        return Ok(AnnounceResult {
            interval: interval,
            leechers: leechers,
            seeders: seeders,
            peers: peers,
        });
    }

    pub async fn scrape(&self, hashes: &[InfoHash]) -> Result<Vec<ScrapedItem>, TrackerError> {
        if hashes.len() > MAX_SCRAPE {
            return Err(TrackerError::MaxScrapeReached);
        }
        let mut buf = Vec::new();
        for hash in hashes.iter() {
            buf.extend_from_slice(hash.as_slice());
        }
        let con_id = self.connection_id().await?;
        let bytes = self.send_request(con_id, Action::Scrape, &buf).await?;

        // Array of this 3 * u32:
        // u32: seeders
        // u32: leechers
        // u32: completed
        if bytes.len() % 12 != 0 {
            return Err(TrackerError::InvalidReply);
        }
        let mut ret = Vec::new();
        for chunk in bytes.chunks_exact(12) {
            let seeders = u32::from_be_bytes(chunk[0..4].try_into().unwrap());
            let leechers = u32::from_be_bytes(chunk[4..8].try_into().unwrap());
            let completed = u32::from_be_bytes(chunk[8..12].try_into().unwrap());

            ret.push(ScrapedItem {
                seeders: seeders,
                leechers: leechers,
                completed: completed,
            });
        }
        return Ok(ret);
    }
}

impl<'a> CancelGuard<'a> {
    fn new(tid: u32, tracker: &'a UdpTrackerInner) -> Self {
        // warn!("Adding pending task: {} on tracker {}", tid, tracker.addr);
        return Self {
            tid: tid,
            tracker: tracker,
            disarm: false,
        };
    }

    fn disarm(&mut self) {
        self.disarm = true;   
    }
}

impl<'a> Drop for CancelGuard<'a> {
    fn drop(&mut self) {
        if !self.disarm { // Cleanup when task was canceled
            // warn!("Removing pending task: {} on tracker {}", self.tid, self.tracker.addr);
            self.tracker.pending.lock().unwrap().remove(&self.tid);
        }
    }
}

#[cfg(test)]
mod tests {
    use tracing::{error, info};

    use super::*;

    #[tokio::test]
    #[ignore]
    async fn smoke_test() {
        // udp://tracker.opentrackr.org:1337/announce
        let mut ips = tokio::net::lookup_host("tracker.opentrackr.org:1337").await.unwrap();
        let udp = Arc::new(UdpSocket::bind("0.0.0.0:0").await.unwrap());
        let addr = ips.find(|v| v.is_ipv4()).unwrap();
        let tracker = UdpTracker::new(addr, udp.clone());

        // Try connect
        let tracker2 = tracker.clone();
        let handle = tokio::spawn(async move {
            let mut buf = [0u8; 65535];
            loop {
                match udp.recv_from(&mut buf).await {
                    Ok((len, addr)) => {
                        let _ = tracker2.process_udp(&buf[..len], &addr);
                    }
                    Err(err) => {
                        error!("Error: {err}");
                    }
                }
            }
        });

        // Ubuntu 25.04
        let hash = InfoHash::from_hex("8a19577fb5f690970ca43a57ff1011ae202244b8").unwrap();
        let items = tracker.scrape(std::slice::from_ref(&hash)).await.unwrap();
        let _result = tracker.announce(AnnounceInfo {
            hash: hash,
            peer_id: PeerId::rand(),
            port: 11451,
            uploaded: 0,
            downloaded: 0,
            left: 1,
            event: Event::None,
            num_want: None,
        }).await.unwrap();
        info!("Scraped: {items:?}");
        handle.abort();
        let _ = handle.await;
    }
}