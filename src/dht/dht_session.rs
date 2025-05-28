#![allow(dead_code)] // Let it shutup!

use std::collections::{BTreeMap, BTreeSet};
use std::net::SocketAddr;
use std::sync::{Arc, Mutex};
use std::time::Duration;
use tokio::task::JoinSet;
use tracing::{debug, error, warn};



use super::{NodeId, RoutingTable};
use super::krpc::*;

const MAX_ITERATIONS_WITH_CONVERGENCE: usize = 3; // The max iterations with convergence
const MAX_ITERATIONS: usize = 16; // The max iterations of the find_node / get_peers
const MAX_ALPHA: usize = 3;

struct DhtSessionInner {
    routing_table: Mutex<RoutingTable>,
    peers: Mutex<BTreeMap<InfoHash, BTreeSet<SocketAddr> > >, // The peers we know about, indexed by infohash
    krpc: KrpcContext,
    id: NodeId, // The self id
    ipv4: bool, // does it run on ipv4? ,false on ipv6
    timeout: Duration, // The timeout for the all krpc requests
}

#[derive(Clone)]
pub struct DhtSession {
    inner: Arc<DhtSessionInner>
}

pub enum FindNodeError {
    AllFailed, // We try to do rpc to the nodes, but all failed, no-one give us reply
}

type NodeEndpoint = (NodeId, SocketAddr);

/// Sort the nodes by distance to the target node, and remove duplicates
/// based on the NodeId.
fn sort_node_and_unique(nodes: &mut Vec<NodeEndpoint>, target: NodeId){
    nodes.sort_by(|(a, _), (b, _)| {
        let a_dis = *a ^ target;
        let b_dis = *b ^ target;
        return a_dis.cmp(&b_dis);
    });
    nodes.dedup_by(|a, b| a == b);
}

// Extract the query part from the message
// Returns the tid, query method, and the "a" part of the message
fn extract_query_part(msg: &Object) -> Option<(&[u8], &[u8], &Object)> {
    // ping Query = {"t":"aa", "y":"q", "q":"ping", "a":{"id":"abcdefghij0123456789"}}
    let dict = msg.as_dict()?;
    let tid = dict.get(b"t".as_slice())?.as_string()?;
    let y = dict.get(b"y".as_slice())?.as_string()?;
    let q = dict.get(b"q".as_slice())?.as_string()?;
    let a = dict.get(b"a".as_slice())?;
    if y != b"q" {
        return None; // Not a query
    }
    return Some((tid, q, a));
}

impl DhtSession {
    /// Check if the given address is a native address for this session.
    fn is_native_addr(&self, addr: &SocketAddr) -> bool {
        match addr {
            SocketAddr::V4(_) => return self.inner.ipv4,
            SocketAddr::V6(_) => return !self.inner.ipv4,
        }
    }

    pub fn new(id: NodeId, krpc: KrpcContext) -> DhtSession {
        let ipv4 = krpc.is_ipv4();
        return DhtSession {
            inner: Arc::new(
                DhtSessionInner {
                    routing_table: Mutex::new(RoutingTable::new(id)),
                    peers: Mutex::new(BTreeMap::new()),
                    krpc: krpc,
                    id: id,
                    ipv4: ipv4,
                    timeout: Duration::from_secs(3),
                }
            )
        };
    }

    /// Wrapper for do krpc, manage the life time
    async fn do_krpc<T: KrpcQuery>(self, msg: T, ip: SocketAddr) -> (Result<T::Reply, KrpcError>, SocketAddr) {
        let mut idx = 0;
        loop {
            debug!("Sending {} KRPC message to {}, Try {}", str::from_utf8(T::method_name()).unwrap(), ip, idx);
            let result = self.inner.krpc.send_krpc_with_timeout(&msg, &ip, self.inner.timeout).await;
            if let Err(KrpcError::TimedOut) = result {
                debug!("KRPC request timed out for {}, retrying...", ip);
                idx += 1;
                if idx >= 3 {
                    return (result, ip);
                }
            }
            return (result, ip); // Done
        }
    }
    
    // Doing the find logical
    pub async fn find_node_impl(&self, mut queue: Vec<NodeEndpoint>, target: NodeId) -> Result<Vec<NodeEndpoint>, FindNodeError> {
        if queue.is_empty() {
            panic!("Queue should not be empty");
        }
        sort_node_and_unique(&mut queue, target);
        let mut join_set = JoinSet::new();
        let mut visited = BTreeSet::new();
        let mut collected = Vec::new();
        let mut output = Vec::new();
        let mut iteration_with_convergence = 0;
        for _iteration in 0..MAX_ITERATIONS {
            if queue.is_empty() {
                warn!("No more nodes to query, stopping the search.");
                break;
            }
            let num = queue.len().min(MAX_ALPHA);
            for (_node_id, ip) in queue.drain(0..num) {
                // Send the find_node request
                let msg = FindNodeQuery {
                    id: self.inner.id,
                    target: target,
                };

                let fut = self.clone().do_krpc(msg, ip);
                join_set.spawn(fut);
            }
            // Wait for all futures to complete
            while let Some(res) = join_set.join_next().await {
                let (result, ip) = match res {
                    Ok(what) => what,
                    Err(_) => continue, // Set is empty...
                };
                let reply = match result {
                    Ok(reply) => reply,
                    Err(err) => {
                        // TODO: Notify the routing table, it failed
                        debug!("Failed to do krpc on {} => {}", ip, err);
                        visited.insert(ip.clone());
                        continue;
                    }
                };
                // Try add it to the routing table, it give us reply
                let _ = self.inner.routing_table.lock().expect("Mutex poisoned").add_node(reply.id, &ip);
                visited.insert(ip);

                // Update the nodes...
                for (id, ip) in reply.nodes.iter() {
                    if !self.is_native_addr(ip) {
                        continue; // Skip the non-native addresses
                    }
                    if visited.contains(ip) {
                        continue; // Already visited this node
                    }
                    queue.push((*id, *ip));
                }
                collected.extend_from_slice(reply.nodes.as_slice());
            }
            sort_node_and_unique(&mut queue, target);
            sort_node_and_unique(&mut collected, target);

            // Check if the K-nearest nodes are the same, if so more than 3 iterations, we can stop
            let collected_len = collected.len().min(8);
            let len = output.len().min(collected_len);
            if output[0..len] != collected[0..len] || len < 8 { // If the output is not the same as collected(0..K), or we have less than 8 nodes
                // Update the K-nearest nodes
                output.clear();
                output.extend_from_slice(&collected[0..collected_len]);
                iteration_with_convergence = 0; // Reset the convergence counter
            }
            else {
                iteration_with_convergence += 1;
                if iteration_with_convergence >= MAX_ITERATIONS_WITH_CONVERGENCE {
                    debug!("Convergence reached, stopping the search.");
                    break; // We have convergence, stop the search
                }
            }
            
            // Avoid to grow too large
            queue.truncate(8 * 8);
            collected.truncate(8 * 8);
        }
        // Done
        join_set.shutdown().await;
        if output.is_empty() {
            error!("Failed to find any nodes for target: {}", target);
            return Err(FindNodeError::AllFailed);
        }
        debug!("Found {} nodes for target: {}", output.len(), target);
        return Ok(output);
    }

    // Process the incoming udp packet
    pub async fn process_udp(&self, bytes: &[u8], ip: &SocketAddr) -> bool {
        let query = match self.inner.krpc.process_udp(bytes, ip) {
            KrpcProcess::Query(query) => query,
            KrpcProcess::Reply => return true,
            KrpcProcess::InvalidMessage => return false,
        };
        let (tid, method, msg) = match extract_query_part(&query) {
            Some(val) => val,
            None => {
                error!("Invalid query format: {:?}", query);
                return false; // Invalid query
            }
        };
        // debug!("Received query {}: ", str::from_utf8(method));
        match method {
            b"ping" => {
                let query = match PingQuery::from_bencode(msg) {
                    Some(val) => val,
                    None => return false,
                };
                let reply = PingReply {
                    id: self.inner.id,
                };
                let _ = self.inner.routing_table.lock().expect("Mutex poisoned").update_node(query.id, ip);
                let _ = self.inner.krpc.send_reply(tid, reply, ip).await;
                return true;
            },
            b"find_node" => {
                let query = match FindNodeQuery::from_bencode(msg) {
                    Some(val) => val,
                    None => return false,
                };
                let nodes = self.inner.routing_table.lock().expect("Mutex poisoned").find_node(query.target);
                let reply = FindNodeReply {
                    id: self.inner.id,
                    nodes: nodes,
                };
                let _ = self.inner.krpc.send_reply(tid, reply, ip).await;
                return true;
            },
            b"get_peers" => {
                let query = match GetPeersQuery::from_bencode(msg) {
                    Some(val) => val,
                    None => return false,
                };
                // Find the infohash in the routing table and collect the peers if exists
                let mut values = Vec::new();
                let nodes = self.inner.routing_table.lock().expect("Mutex poisoned").find_node(query.info_hash);
                if let Some(peers) = self.inner.peers.lock().expect("Mutex poisoned").get(&query.info_hash) {
                    for peer in peers {
                        values.push(peer.clone());
                    }
                }
                if values.len() > 8 {
                    // Randomly select 8 peers
                    fastrand::shuffle(values.as_mut_slice());
                    values.truncate(8);
                }
                let reply = GetPeersReply {
                    id: self.inner.id,
                    nodes: nodes,
                    token: ip.to_string().into_bytes(), // Use the ip as token
                    values: values,
                };
                let _ = self.inner.krpc.send_reply(tid, reply, ip).await;
                return true;
            },
            b"announce_peer" => {
                let query = match AnnouncePeerQuery::from_bencode(msg) {
                    Some(val) => val,
                    None => return false,
                };
                if query.token != ip.to_string().into_bytes() {
                    // Invalid token, ignore it
                    error!("Invalid token: {:?}", query.token);
                    let reply = ErrorReply {
                        code: 203, // Invalid token
                        msg: "Invalid token".to_string(),
                    };
                    let _ = self.inner.krpc.send_reply(tid, reply, ip).await;
                    return true; // Still a valid query for process udp
                }
                let mut peer_ip = ip.clone();
                if !query.implied_port {
                    peer_ip.set_port(query.port);
                }

                // Add the peer to the routing table
                {
                    let mut set = self.inner.peers.lock().expect("Mutex poisoned");
                    set.entry(query.info_hash).or_default().insert(peer_ip);
                } // Avoid the lock across the await point
                let reply = AnnouncePeerReply {
                    id: self.inner.id,
                };
                let _ = self.inner.krpc.send_reply(tid, reply, ip).await;
                return true;
            },
            _ => {
                error!("Unknown method: {}", String::from_utf8_lossy(method));
                let reply = ErrorReply {
                    code: 202, // Unknown method
                    msg: format!("Unknown method: {}", String::from_utf8_lossy(method)),
                };
                let _ = self.inner.krpc.send_reply(tid, reply, ip).await;
                return true; // Unknown method, but still a valid query for process udp
            },
        }
    }

    pub async fn bootstrap(&self) -> bool {
        let routers = [
            "dht.transmissionbt.com:6881",
            "router.bittorrent.com:6881",
            "router.utorrent.com:6881",
        ];
        let mut queue = Vec::new();
        for each in routers {
            let res = match tokio::net::lookup_host(each).await {
                Ok(res) => res,
                Err(e) => {
                    error!("Failed to lookup host {}: {}", each, e);
                    continue; // Skip this router if lookup fails
                }
            };
            let mut table = self.inner.routing_table.lock().expect("Mutex poisoned");
            for ip in res {
                table.add_router(&ip); // Add this ip as router, to filter 
                if self.is_native_addr(&ip) {
                    debug!("Adding router {} to the queue", ip);
                    queue.push((NodeId::zero(), ip)); // Use zero node id as an placeholder
                }
            }
        }
        // Begin find self
        let _ = match self.find_node_impl(queue, self.inner.id).await {
            Ok(nodes) => nodes,
            Err(_) => {
                error!("Failed to bootstrap, no nodes found");
                return false;
            }
        };
        let mut len = self.inner.routing_table.lock().expect("Mutex poisoned").nodes_len();
        let mut iter = 0;
        if len == 0 {
            // Emm, we have no nodes in the routing table, let run retry
            return false;
        }
        while len <= 50 && iter < 10 { // Doing random search to fill our bucket
            let id = NodeId::rand();
            let nodes = self.inner.routing_table.lock().expect("Mutex poisoned").find_node(id);
            if nodes.is_empty() {
                // No nodes found, try again
                iter += 1;
                continue;
            }
            let _ = self.find_node_impl(nodes, id).await;
            len = self.inner.routing_table.lock().expect("Mutex poisoned").nodes_len();
            iter += 1;
        }
        return true;
    }

    pub async fn run(&self) {
        // Doing the bootstrap
        loop {
            if self.bootstrap().await {
                debug!("DHT session bootstrap completed successfully.");
                break;
            } 
            else {
                error!("DHT session bootstrap failed, retrying...");
                tokio::time::sleep(std::time::Duration::from_secs(30)).await;
            }
        }
        // TODO: add subtask for handling refreshing the routing table
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tokio::{io, net};
    use ctor::ctor;

    #[ctor]
    fn init() {
        color_backtrace::install();
        tracing_subscriber::fmt()
            .with_max_level(tracing::Level::DEBUG)
            .with_thread_ids(true)
            .init();
    }

    #[tokio::test]
    #[ignore]
    async fn test_dht_session() -> io::Result<()> {
        let sockfd = Arc::new(net::UdpSocket::bind("[::0]:11451").await?);
        let ctxt = KrpcContext::new(sockfd.clone());
        let id = NodeId::from_hex("38c14d2d6652a0c1f97db18f67609eceaabe6ce3").unwrap();
        let session = DhtSession::new(id, ctxt);
        let session_ = session.clone();
        
        // Start the DHT session
        let worker = tokio::spawn(async move {
            let mut buffer = [0u8; 65535];
            loop {
                let (len, ip) = match sockfd.recv_from(&mut buffer).await {
                    Ok(res) => res,
                    Err(e) => {
                        error!("Failed to receive from socket: {}", e);
                        continue;
                    }
                };
                debug!("Received packet from {}", ip);
                session.process_udp(&buffer[0..len], &ip).await;
            }
        });
        session_.run().await;
        let _ = worker.await;
        
        Ok(())
    }
}