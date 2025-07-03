// Tracker abstraction
use crate::{PeerId, InfoHash};
use async_trait::async_trait;
use std::collections::HashMap;
use std::net::SocketAddr;
use thiserror::Error;
use url::Url;

#[repr(u32)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Event {
    None      = 0,
    Completed = 1,
    Started   = 2,
    Stopped   = 3,
}

#[derive(Debug, Clone, Copy)]
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

#[derive(Debug, Clone)]
pub struct AnnounceResult {
    pub interval: u32,
    pub peers: Vec<SocketAddr>,

    // This field is extemsion in HTTP trackers, so we have to make it optional :(
    pub seeders: Option<u32>,  // Useable in UDP trackers
    pub leechers: Option<u32>, // Useable in UDP trackers
    pub completed: Option<u32>,
    pub external_ip: Option<SocketAddr>,
}

#[derive(Debug, Clone, Copy)]
pub struct ScrapedItem {
    pub seeders: u32,
    pub leechers: u32,
    pub completed: u32,
}

#[derive(Error, Debug, Clone)]
pub enum TrackerError {
    #[error("Invalid request to tracker, we can't send more than 74 scrape requests at once")]
    MaxScrapeReached,

    #[error("Error reply from tracker: {0}")]
    Error(String),

    #[error("Invalid reply from tracker")]
    InvalidReply,

    #[error("Unknown error")]
    Unknown,

    #[error("Request Timed out")]
    TimedOut,

    #[error("This operation is not supported by the tracker")]
    UnsupporttedOperation,

    #[error("Network error: {0}")]
    NetworkError(String) // The io::Error is not cloneable :(, use String to store the message
}

/// Public Trakcer API, can be implemented by any tracker (HTTP or UDP)
#[async_trait]
pub trait Tracker {
    /// Announce a peer to the tracker
    async fn announce(&self, info: AnnounceInfo) -> Result<AnnounceResult, TrackerError>;

    /// Scrape a list of infohashes from the tracker
    async fn scrape(&self, hashes: &[InfoHash]) -> Result<HashMap<InfoHash, ScrapedItem>, TrackerError>;

    /// Get the URL of the tracker
    fn url(&self) -> Url;
}