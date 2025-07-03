#![allow(dead_code, unused_imports)]
use async_trait::async_trait;
use libc::close;
use reqwest::{Client, Url};
use tracing::{debug, trace};
use std::collections::HashMap;
use urlencoding::encode_binary;
use std::net::SocketAddr;
use std::sync::Arc;
use crate::core::compact::parse_your_ip;
use crate::{core::compact, InfoHash};
use crate::bencode::Object;
use super::{Tracker, AnnounceInfo, AnnounceResult, ScrapedItem, TrackerError, Event};

#[derive(Debug, Clone)]
pub struct HttpTracker {
    client: Client,
    url: Url,
    key: u32,
}

fn check_failure(object: &Object) -> Result<(), TrackerError> {
    if let Some(reason) = object.get(b"failure reason").and_then(|s| s.as_string()) {
        return Err(TrackerError::Error(String::from_utf8_lossy(&reason).into()));
    }
    return Ok(());
}

fn parse_peers(peers: &Object, out: &mut Vec<SocketAddr>, chunk_size: usize) -> Result<(), TrackerError> {
    match peers {
        Object::String(peers) => { // Compact format
            for peer in peers.chunks(chunk_size).map(|item| compact::parse_ip(item)) {
                out.push(peer.ok_or(TrackerError::InvalidReply)?);
            }
        }
        Object::List(list) => {
            for item in list {
                let _peer_id = item.get(b"peer id")
                    .and_then(|s| s.as_string())
                    .ok_or(TrackerError::InvalidReply)?; // We does not care it, check, make the standard happy

                let ip = item.get(b"ip")
                    .and_then(|s| s.as_string())
                    .ok_or(TrackerError::InvalidReply)?;

                let port: u16 = item.get(b"port")
                    .and_then(|s| s.as_int())
                    .ok_or(TrackerError::InvalidReply)?
                    .clone().try_into() // Cast into u16
                    .map_err(|_| TrackerError::InvalidReply)?;
                
                let addr = SocketAddr::new(
                    compact::parse_your_ip(&ip).ok_or(TrackerError::InvalidReply)?,
                    port
                );
                out.push(addr);
            }
        }
        _ => return Err(TrackerError::InvalidReply),
    };
    return Ok(());
}

impl HttpTracker {
    pub fn new(url: &Url, client: Client) -> Option<Self> {
        // Check the scheme
        match url.scheme() {
            "http" | "https" => (),
            _ => return None,
        }

        return Some(Self {
            client: client,
            url: url.clone(),
            key: fastrand::u32(..),
        });
    }

    pub fn into_dyn(self) -> Arc<dyn Tracker + Send + Sync> {
        return Arc::new(self);
    }

    async fn announce_impl(&self, info: AnnounceInfo) -> Result<AnnounceResult, TrackerError> {
        let event = match info.event {
            Event::Started => "started",
            Event::Stopped => "stopped",
            Event::Completed => "completed",
            Event::None => "empty",
        };
        // Build the query by hand :(
        let hash = encode_binary(info.hash.as_slice());
        let peer_id = encode_binary(info.peer_id.as_slice());
        let url = format!(
            "{}?info_hash={}&peer_id={}&port={}&uploaded={}&downloaded={}&left={}&event={}&compact=1&key={}",
            self.url.as_str(), hash, peer_id, info.port, info.uploaded, info.downloaded, info.left, event, self.key
        );
        trace!("Announce url: {}", url);

        let reply = self.client
            .get(url)
            .send()
            .await
            .map_err(|e| TrackerError::NetworkError(e.to_string()))?
            .bytes()
            .await
            .map_err(|e| TrackerError::NetworkError(e.to_string()))?
        ;
        let object = match Object::parse(&reply) {
            Some(val) => val,
            None => return Err(TrackerError::InvalidReply),
        };
        // The tracker failed
        check_failure(&object)?;

        trace!("Announce reply: {:?}", object);

        // Parse...
        let interval: u32 = object.get(b"interval")
            .and_then(|i| i.as_int())
            .ok_or(TrackerError::InvalidReply)?
            .clone().try_into() // Cast into u32
            .map_err(|_| TrackerError::InvalidReply)?;

        let mut peers = Vec::new();
        let mut has_peers_field = false;
        if let Some(obj) = object.get(b"peers") {
            parse_peers(obj, &mut peers, 6)?; // 6 bytes for an ip
            has_peers_field = true;
        }
        if let Some(obj) = object.get(b"peers6") {
            parse_peers(obj, &mut peers, 18)?; // 18 bytes for an ipv6
            has_peers_field = true;
        }
        if !has_peers_field {
            return Err(TrackerError::InvalidReply);
        }
        // Extension field
        let seeders = object.get(b"complete")
            .and_then(|i| i.as_int())
            .and_then(|i| i.clone().try_into().ok());
        let leechers = object.get(b"incomplete")
            .and_then(|i| i.as_int())
            .and_then(|i| i.clone().try_into().ok());
        let completed = object.get(b"downloaded")
            .and_then(|i| i.as_int())
            .and_then(|i| i.clone().try_into().ok());
        let external_ip = object.get(b"external ip")
            .and_then(|i| i.as_string())
            .and_then(|s| compact::parse_ip(&s));

        return Ok(AnnounceResult {
            interval: interval,
            peers: peers,
            seeders: seeders,
            leechers: leechers,
            completed: completed,
            external_ip: external_ip
        })
    }

    async fn scrape_impl(&self, hashes: &[InfoHash]) -> Result<HashMap<InfoHash, ScrapedItem>, TrackerError> {
        let url = {
            let mut url = self.url.as_str().to_string().replace("announce", "scrape");
            url += "?";
            for hash in hashes {
                url += &format!("info_hash={}&", encode_binary(hash.as_slice()));
            }
            url.pop(); // Remove the last &

            url
        };
        
        debug!("Scraping with query {}", url);
        let reply = self.client.get(url)
            .send()
            .await
            .map_err(|e| TrackerError::NetworkError(e.to_string()))?
            .bytes()
            .await
            .map_err(|e| TrackerError::NetworkError(e.to_string()))?
        ;
        let object = match Object::parse(&reply) {
            Some(val) => val,
            None => return Err(TrackerError::InvalidReply),
        };
        // The tracker failed
        check_failure(&object)?;

        let files = object.get(b"files")
            .and_then(|f| f.as_dict())
            .ok_or(TrackerError::InvalidReply)?;
        let mut map = HashMap::new();
        for (hash, item) in files.iter() {
            let hash: InfoHash = hash.try_into().map_err(|_| TrackerError::InvalidReply)?;
            let complete = item.get(b"complete")
                .and_then(|i| i.as_int())
                .and_then(|i| i.clone().try_into().ok())
                .ok_or(TrackerError::InvalidReply)?;

            let incomplete = item.get(b"incomplete")
                .and_then(|i| i.as_int())
                .and_then(|i| i.clone().try_into().ok())
                .ok_or(TrackerError::InvalidReply)?;

            let downloaded = item.get(b"downloaded")
                .and_then(|i| i.as_int())
                .and_then(|i| i.clone().try_into().ok())
                .ok_or(TrackerError::InvalidReply)?;

            let item = ScrapedItem {
                seeders: complete,
                leechers: incomplete,
                completed: downloaded,
            };
            map.insert(hash, item);
        }
        return Ok(map);
    }
}

#[async_trait]
impl Tracker for HttpTracker {
    fn url(&self) -> Url {
        return self.url.clone();
    }
    
    async fn announce(&self, info: AnnounceInfo) -> Result<AnnounceResult, TrackerError> {
        return self.announce_impl(info).await;
    }

    async fn scrape(&self, hashes: &[InfoHash]) -> Result<HashMap<InfoHash, ScrapedItem>, TrackerError> {
        return self.scrape_impl(hashes).await;
    }
}

#[cfg(test)]
mod tests {
    use tracing::{error, info};
    use crate::{PeerId, bt::Event};

    use super::*;

    #[tokio::test]
    #[ignore]
    async fn smoke_test() {
        // udp://tracker.opentrackr.org:1337/announce
        let client = Client::new();
        let url = Url::parse("https://tracker.zhuqiy.top:443/announce").unwrap();
        let tracker = HttpTracker::new(&url, client).unwrap();

        // Ubuntu 25.04
        let hash = [InfoHash::from_hex("8a19577fb5f690970ca43a57ff1011ae202244b8").unwrap()];
        let items = tracker.scrape(&hash).await.unwrap();
        info!("items: {:?}", items);
        let mut info = AnnounceInfo {
            hash: hash[0],
            peer_id: PeerId::rand(),
            event: Event::Started,
            port: 6881,
            uploaded: 0,
            downloaded: 0,
            left: 0,
            num_want: None,
        };
        let result = tracker.announce(info).await.unwrap();
        info!("result: {:?}", result);
        info.event = Event::Stopped;
        let result = tracker.announce(info).await.unwrap();
        info!("result: {:?}", result);
    }
}