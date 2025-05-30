#![allow(dead_code)] // Let it shutup!

use thiserror::Error;
use tracing::warn;
use std::collections::BTreeMap;
use std::net::SocketAddr;
use std::sync::{Arc, Mutex, atomic};
use atomic::AtomicU16;
use tokio::sync::oneshot;
use tokio::net::UdpSocket;
use tokio::io;

use super::{ErrorReply, KrpcQuery, KrpcReply};
use super::Object;

#[derive(Debug, Error)]
pub enum KrpcError {
    #[error("The remote give us an invalid reply")]
    InvalidReply,

    #[error("The remote give us an error reply")]
    Error(ErrorReply),

    #[error("The remote did not reply in time")]
    TimedOut,

    #[error("Unknown error")]
    Unknown,

    #[error("NetworkError {:?}", .0)]
    NetworkError(#[from] io::Error)
}

pub enum KrpcProcess {
    Query(Object), // Let the caller handle the query
    Reply,
    InvalidMessage,
}

struct KrpcContextInner {
    sockfd: Arc<UdpSocket>,
    pending: Mutex<BTreeMap<
        u16, 
        oneshot::Sender<
            io::Result<Object>
        >
    > >, // The pending query
    tid: AtomicU16, // The tid of the 
}

struct CancelGuard {
    tid: u16,
    ctxt: Arc<KrpcContextInner>,
    disarm: bool,
}

#[derive(Clone)]
pub struct KrpcContext {
    inner: Arc<KrpcContextInner>
}

fn extract_tid(obj: &Object) -> Option<u16> {
    let dict = obj.as_dict()?;
    let tid = dict.get(b"t".as_slice())?.as_string()?;
    let id: [u8; 2] = match tid.as_slice().try_into() {
        Ok(val) => val,
        Err(_) => return None,
    };
    return Some(u16::from_be_bytes(id));
}

fn extract_reply<T: KrpcReply>(obj: &Object) -> Option<T> {
    let dict = obj.as_dict()?;
    if dict.get(b"y".as_slice())?.as_string()? != b"r".as_slice() { // Not a reply
        return None;
    }
    let r = dict.get(b"r".as_slice())?;
    return T::from_bencode(&r);
}

fn extract_query(obj: &Object) -> Option<Object> {
    let dict = obj.as_dict()?;
    if dict.get(b"y".as_slice())?.as_string()? != b"q".as_slice() { // Not a query
        return None;
    }
    return Some(obj.clone());
}

impl Drop for CancelGuard {
    fn drop(&mut self) {
        if !self.disarm { // Cleanup when the task is canceled
            self.ctxt.pending.lock().unwrap().remove(&self.tid);
        }
    }
}

impl CancelGuard {
    fn new(tid: u16, ctxt: Arc<KrpcContextInner>) -> CancelGuard {
        return CancelGuard {
            tid: tid,
            ctxt: ctxt,
            disarm: false
        };
    }

    fn disarm(&mut self) {
        self.disarm = true;
    }
}

impl KrpcContext {
    /// Create an Krpc Context 
    /// 
    /// sender: is the sender send the udp packet
    pub fn new(sockfd: Arc<UdpSocket>) -> KrpcContext {
        return KrpcContext { 
            inner: Arc::new(KrpcContextInner {
                sockfd: sockfd,
                pending: Mutex::new(BTreeMap::new()),
                tid: AtomicU16::new(0)
            })
        };
    }

    pub fn is_ipv4(&self) -> bool {
        return self.inner.sockfd.local_addr().expect("It should never failed").is_ipv4();
    }

    /// Process the incoming udp packet, return query if it is a query
    pub fn process_udp(&self, bytes: &[u8], _ip: &SocketAddr) -> KrpcProcess {
        let (obj, _) = match Object::decode(bytes) {
            Some(val) => val,
            None => return KrpcProcess::InvalidMessage,
        };
        // Check if it is a query
        match extract_query(&obj) {
            Some(val) => return KrpcProcess::Query(val),
            _ => (),
        };

        let tid = match extract_tid(&obj) {
            Some(val) => val,
            None => return KrpcProcess::InvalidMessage,
        };
        // debug!("Incoming message {:?} from {}", obj, ip);
        let sender = match self.inner.pending.lock().expect("Mutex posioned").remove(&tid) {
            Some(val) => val,
            None => return KrpcProcess::InvalidMessage,
        };
        let _ = sender.send(Ok(obj));
        return KrpcProcess::Reply;
    }

    async fn send_krpc_impl(&self, name: &[u8], obj: Object, ip: &SocketAddr) -> Result<Object, KrpcError> {
        // Make channel
        let (sx, rx) = oneshot::channel();
        let tid = self.inner.tid.fetch_add(1, atomic::Ordering::SeqCst);

        // Build the query
        // ping Query = {"t":"aa", "y":"q", "q":"ping", "a":{"id":"abcdefghij0123456789"}}
        let query = BTreeMap::from([
            (b"t".to_vec(), Object::from(tid.to_be_bytes().to_vec())),
            (b"y".to_vec(), Object::from(b"q".as_slice())),
            (b"q".to_vec(), Object::from(name)),
            (b"a".to_vec(), obj)
        ]);
        
        self.inner.pending.lock().expect("Mutex posioned").insert(tid, sx);
        let mut guard = CancelGuard::new(tid, self.inner.clone());

        // Send the packet out
        let bytes = Object::from(query).encode();
        // debug!("Send packet to {}", ip);
        match self.inner.sockfd.send_to(bytes.as_slice(), ip).await {
            Ok(_) => (),
            Err(err) => return Err(KrpcError::NetworkError(err)),
        }

        // Get the result from the channel
        let res = match rx.await {
            Ok(val) => val,
            Err(_) => return Err(KrpcError::Unknown), 
        };

        // All done
        guard.disarm();
        
        match res {
            Ok(obj) => return Ok(obj),
            Err(err) => return Err(KrpcError::NetworkError(err))
        }
    }

    /// Send the krpc to the target endpoint and get the reply from it
    pub async fn send_krpc<T: KrpcQuery>(&self, msg: &T, ip: &SocketAddr) -> Result<T::Reply, KrpcError> {
        // Reply = {"t":"aa", "y":"r", "r": {"id":"mnopqrstuvwxyz123456"}}
        let obj = self.send_krpc_impl(T::method_name(),msg.to_bencode(), ip).await?;
        let reply = match extract_reply::<T::Reply>(&obj) {
            Some(reply) => reply,
            None => return Err(KrpcError::InvalidReply),
        };
        return Ok(reply);
    }

    /// Send the krpc to the target endpoint and get the reply from it 
    /// 
    /// with the timeout support
    pub async fn send_krpc_with_timeout<T: KrpcQuery>(&self, msg: &T, ip: &SocketAddr, timeout: std::time::Duration) -> Result<T::Reply, KrpcError> {
        match tokio::time::timeout(timeout, self.send_krpc(msg, ip)).await {
            Ok(res) => return res,
            Err(_) => return Err(KrpcError::TimedOut),
        }
    }

    async fn send_reply_impl(&self, tid: &[u8], msg: Object, ip: &SocketAddr) -> io::Result<()> {
        // Reply = {"t":"aa", "y":"r", "r": {"id":"mnopqrstuvwxyz123456"}}
        let reply = BTreeMap::from([
            (b"t".to_vec(), Object::from(tid.to_vec())),
            (b"y".to_vec(), Object::from(b"r".as_slice())),
            (b"r".to_vec(), msg)
        ]);
        let encoded = Object::from(reply).encode();
        let len = self.inner.sockfd.send_to(encoded.as_slice(), ip).await?;
        if encoded.len() != len {
            warn!("Send reply to {} failed, expected {} bytes, but sent {} bytes", ip, encoded.len(), len); // Why?
        }
        return Ok(());
    }

    /// Send the reply to the target endpoint
    pub async fn send_reply<T: KrpcReply>(&self, tid: &[u8], msg: T, ip: &SocketAddr) -> io::Result<()> {
        return self.send_reply_impl(tid, msg.to_bencode(), ip).await;
    }
}

#[cfg(test)]
mod tests {
    use tokio::net::UdpSocket;
    use crate::{krpc::PingQuery, NodeId};
    use tracing::debug;
    use super::*;

    async fn process_udp_to_krpc(ctxt: KrpcContext, sockfd: Arc<UdpSocket>) -> io::Result<()> {
        let mut buffer = [0u8; 65535];
        loop {
            let (len, ip) = sockfd.recv_from(&mut buffer).await?;
            debug!("recv packet from {}", ip);
            let _ = ctxt.process_udp(&buffer[0..len], &ip);
        }
    }
    #[tokio::test]
    #[ignore]
    async fn test_krpc() -> io::Result<()> {
        let sockfd = Arc::new(UdpSocket::bind("[::0]:0").await?);
        let ctxt = KrpcContext::new(sockfd.clone());
        
        // Start the basic server
        let h1 = tokio::spawn(process_udp_to_krpc(ctxt.clone(), sockfd.clone()));

        // Then we send an ping krpc to the dht bootstrap server
        let _ = tokio::spawn(async move {
            let ping = PingQuery {
                id: NodeId::rand()
            };
            let ip = tokio::net::lookup_host("dht.transmissionbt.com:6881").await.unwrap().next().unwrap();
            let res = ctxt.send_krpc_with_timeout(&ping, &ip, std::time::Duration::new(5, 0)).await;
            if let Ok(res) = res {
                debug!("got ping reply {:?}", res);
            }
            else {
                debug!("got error {:?}", res);
            }
        }).await;

        h1.abort();
        let _ = h1.await;
        return Ok(());
    }
}