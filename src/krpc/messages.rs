#![allow(dead_code)] // Let it shutup!

use crate::{InfoHash, NodeId, bencode::Object, core::compact};
use std::{collections::BTreeMap, net::{IpAddr, Ipv4Addr, Ipv6Addr, SocketAddr}};

pub trait Message : Sized {
    fn to_bencode(&self) -> Object;
    fn from_bencode(obj: &Object) -> Option<Self>;
}

pub trait KrpcReply : Message {
    
}

pub trait KrpcQuery : Message {
    type Reply: KrpcReply;

    /// The method name of the query, raw string used in krpc
    /// e.g. "find_node", "get_peers", "ping"
    fn method_name() -> &'static [u8];
}

#[derive(Debug, PartialEq, Eq)]
pub struct FindNodeQuery {
    pub id: NodeId, // Which node send this query?
    pub target: NodeId, // The target node id want to find
}

#[derive(Debug, PartialEq, Eq)]
pub struct FindNodeReply {
    pub id: NodeId,
    pub nodes: Vec<(NodeId, SocketAddr)>,
}

#[derive(Debug, PartialEq, Eq)]
pub struct PingQuery {
    pub id: NodeId,
}

#[derive(Debug, PartialEq, Eq)]
pub struct PingReply {
    pub id: NodeId,
}

#[derive(Debug, PartialEq, Eq)]
pub struct GetPeersQuery {
    pub id: NodeId,
    pub info_hash: InfoHash,
}

#[derive(Debug, PartialEq, Eq)]
pub struct GetPeersReply {
    pub id: NodeId,
    pub token: Vec<u8>,
    pub nodes: Vec<(NodeId, SocketAddr)>,
    pub values: Vec<SocketAddr>,
}

#[derive(Debug, PartialEq, Eq)]
pub struct AnnouncePeerQuery {
    pub id: NodeId,
    pub info_hash: InfoHash,
    pub port: u16,
    pub token: Vec<u8>,
    pub implied_port: bool,
}

#[derive(Debug, PartialEq, Eq)]
pub struct AnnouncePeerReply {
    pub id: NodeId,
}

#[derive(Debug, PartialEq, Eq)]
pub struct SampleInfoHashesQuery {
    pub id: NodeId,
    pub target: InfoHash,
}

#[derive(Debug, PartialEq, Eq)]
pub struct SampleInfoHashesReply {
    pub id: NodeId,
    pub interval: u32, // The interval in seconds
    pub nodes: Vec<(NodeId, SocketAddr)>,
    pub num: u32, // The number of info in storage
    pub info_hashes: Vec<InfoHash>, // The subset info hashes
}

#[derive(Debug, PartialEq, Eq)]
pub struct ErrorReply {
    pub code: i64,
    pub msg: String,
}

fn parse_node_id(id: &[u8]) -> Option<NodeId> {
    let id = id.try_into().ok()?;
    return Some(id);
}

/// Parse the compressed node string (NodeId 20 bytes, ipv4(4) or ipv6(16) addr, 2 bytes port)
fn parse_nodes(mut slice: &[u8]) -> Option<Vec<(NodeId, SocketAddr)> > {
    let v4;
    if slice.len() % 26 == 0 {
        v4 = true;
    }
    else if slice.len() % 38 == 0 {
        v4 = false;
    }
    else {
        return None;
    }

    let mut vec = Vec::new();
    while !slice.is_empty() {
        // Parse the id first
        if slice.len() < 20 {
            return None;
        }
        let id: NodeId = slice[0..20].try_into().ok()?;
        slice = &slice[20..];
        // Check how many bytes we left
        let ip = if v4 {
            if slice.len() < 6 {
                return None;
            }
            let addr: [u8; 4] = slice[0..4].try_into().ok()?;
            let port: [u8; 2] = slice[4..6].try_into().ok()?;
            slice = &slice[6..];
            
            SocketAddr::new(IpAddr::V4(Ipv4Addr::from(addr)), u16::from_be_bytes(port))
        }
        else {
            if slice.len() < 18 {
                return None;
            }
            let addr: [u8; 16] = slice[0..16].try_into().ok()?;
            let port: [u8; 2] = slice[16..18].try_into().ok()?;
            slice = &slice[18..];

            SocketAddr::new(IpAddr::V6(Ipv6Addr::from(addr)), u16::from_be_bytes(port))
        };

        vec.push((id, ip));
    }

    return Some(vec);
}

// Encode the (NodeId 20 bytes, ipv4(4) or ipv6(16) addr, 2 bytes port) to the vec's back
fn encode_nodes_to(out: &mut Vec<u8>, nodes: &[(NodeId, SocketAddr)]) {
    for (id, ip) in nodes {
        out.extend_from_slice(id.as_slice());
        compact::encode_ip_to(out, ip);
    }
}

// FindNode...
impl Message for FindNodeQuery {
    fn to_bencode(&self) -> Object {
        // {"id" : "<querying nodes id>", "target" : "<id of target node>"}
        let dict = BTreeMap::from([
            (b"id".to_vec(),     Object::from(self.id.as_slice())),
            (b"target".to_vec(), Object::from(self.target.as_slice())),
        ]);
        return Object::from(dict);
    }

    fn from_bencode(obj: &Object) -> Option<Self> {
        let dict = obj.as_dict()?;
        let id = dict.get(b"id".as_slice())?.as_string()?;
        let target = dict.get(b"target".as_slice())?.as_string()?;

        return Some(Self {
            id: parse_node_id(id)?,
            target: parse_node_id(target)?,
        });
    }
}

impl KrpcQuery for FindNodeQuery {
    type Reply = FindNodeReply;

    fn method_name() -> &'static [u8] {
        return b"find_node".as_slice();
    }
}

impl Message for FindNodeReply {
    fn to_bencode(&self) -> Object {
        // {"id" : "<querying nodes id>", "nodes" : "<compressed nodes>"} ipv4 format
        // {"id" : "<querying nodes id>", "nodes6" : "<compressed nodes>"} ipv6 format
        let mut nodes = Vec::new();
        let v4 = if self.nodes.len() > 0 {
            self.nodes[0].1.is_ipv4()
        }
        else {
            true // Emm, empty list, default to v4
        };
        encode_nodes_to(&mut nodes, &self.nodes);
        let mut dict = BTreeMap::from([
            (b"id".to_vec(), Object::from(self.id.as_slice())),
        ]);
        if v4 {
            dict.insert(b"nodes".to_vec(), Object::from(nodes));
        }
        else {
            dict.insert(b"nodes6".to_vec(), Object::from(nodes));
        }
        return Object::from(dict);
    }

    fn from_bencode(obj: &Object) -> Option<Self> {
        let dict = obj.as_dict()?;
        let id = dict.get(b"id".as_slice())?.as_string()?;
        let mut nodes = dict.get(b"nodes".as_slice());
        if nodes.is_none() {
            nodes = dict.get(b"nodes6".as_slice());
        }
        let nodes = nodes?.as_string()?;

        return Some(Self {
            id: parse_node_id(id)?,
            nodes: parse_nodes(nodes)?,
        });
    }
}

impl KrpcReply for FindNodeReply {

}

/// Ping...
impl Message for PingQuery {
    fn to_bencode(&self) -> Object {
        // {"id" : "<querying nodes id>"}
        let dict = BTreeMap::from([
            (b"id".to_vec(), Object::from(self.id.as_slice())),
        ]);
        return Object::from(dict);
    }

    fn from_bencode(obj: &Object) -> Option<Self> {
        let dict = obj.as_dict()?;
        let id = dict.get(b"id".as_slice())?.as_string()?;

        return Some(Self {
            id: parse_node_id(id)?,
        });
    }
}

impl KrpcQuery for PingQuery {
    type Reply = PingReply;

    fn method_name() -> &'static [u8] {
        return b"ping".as_slice();
    }
}

impl Message for PingReply {
    fn to_bencode(&self) -> Object {
        // {"id" : "<querying nodes id>"}
        let dict = BTreeMap::from([
            (b"id".to_vec(), Object::from(self.id.as_slice())),
        ]);
        return Object::from(dict);
    }

    fn from_bencode(obj: &Object) -> Option<Self> {
        let dict = obj.as_dict()?;
        let id = dict.get(b"id".as_slice())?.as_string()?;

        return Some(Self {
            id: parse_node_id(id)?,
        });
    }
}

impl KrpcReply for PingReply {

}

/// GetPeers...
impl Message for GetPeersQuery {
    fn to_bencode(&self) -> Object {
        // {"id" : "<querying nodes id>", "info_hash" : "<info hash>"}
        let dict = BTreeMap::from([
            (b"id".to_vec(), Object::from(self.id.as_slice())),
            (b"info_hash".to_vec(), Object::from(self.info_hash.as_slice())),
        ]);
        return Object::from(dict);
    }

    fn from_bencode(obj: &Object) -> Option<Self> {
        let dict = obj.as_dict()?;
        let id = dict.get(b"id".as_slice())?.as_string()?;
        let info_hash = dict.get(b"info_hash".as_slice())?.as_string()?;

        return Some(Self {
            id: parse_node_id(id)?,
            info_hash: parse_node_id(info_hash)?,
        });
    }
}

impl KrpcQuery for GetPeersQuery {
    type Reply = GetPeersReply;

    fn method_name() -> &'static [u8] {
        return b"get_peers".as_slice();
    }
}

impl Message for GetPeersReply {
    fn to_bencode(&self) -> Object {
        // {"id" : "<queried nodes id>", "token" :"<opaque write token>",  "nodes" : "<compact node info>", "values" : ["<peer 1 info string>", "<peer 2 info string>"]}
        let mut nodes = Vec::new();
        let v4 = if self.nodes.len() > 0 {
            self.nodes[0].1.is_ipv4()
        }
        else {
            true // Emm, empty list, default to v4
        };
        encode_nodes_to(&mut nodes, &self.nodes);

        let mut dict = BTreeMap::from([
            (b"id".to_vec(), Object::from(self.id.as_slice())),
            (b"token".to_vec(), Object::from(self.token.as_slice())),
        ]);

        if v4 {
            dict.insert(b"nodes".to_vec(), Object::from(nodes));
        }
        else {
            dict.insert(b"nodes6".to_vec(), Object::from(nodes));
        }
        if !self.values.is_empty() {
            let mut values = Vec::new();
            for ip in &self.values {
                let mut buf = Vec::new();
                compact::encode_ip_to(&mut buf, ip);
                values.push(Object::from(buf));
            }
            dict.insert(b"values".to_vec(), Object::from(values));
        }
        return Object::from(dict);
    }

    fn from_bencode(obj: &Object) -> Option<Self> {
        let dict = obj.as_dict()?;
        let id = dict.get(b"id".as_slice())?.as_string()?;
        let token = dict.get(b"token".as_slice())?.as_string()?;
        let values = dict.get(b"values".as_slice());
        let mut nodes = dict.get(b"nodes".as_slice());
        if nodes.is_none() {
            nodes = dict.get(b"nodes6".as_slice());
        }

        let mut out_nodes = Vec::new();
        if let Some(val) = nodes {
            let nodes = val.as_string()?;
            out_nodes = parse_nodes(nodes)?;
        }

        let mut out_values = Vec::new();
        if let Some(val) = values {
            for value in val.as_list()? {
                let ip = value.as_string()?;
                let ip = compact::parse_ip(ip)?;
                out_values.push(ip);
            }
        }

        return Some(Self {
            id: parse_node_id(id)?,
            token: token.to_vec(),
            nodes: out_nodes,
            values: out_values,
        });
    }
}

impl KrpcReply for GetPeersReply {

}

/// AnnouncePeer...
impl Message for AnnouncePeerQuery {
    fn to_bencode(&self) -> Object {
        // {"id" : "<querying nodes id>", "info_hash" : "<info hash>", "port": <port>, "token": "<token>", "implied_port": <0 or 1>}
        let mut dict = BTreeMap::from([
            (b"id".to_vec(), Object::from(self.id.as_slice())),
            (b"info_hash".to_vec(), Object::from(self.info_hash.as_slice())),
            (b"port".to_vec(), Object::from(self.port as i64)),
            (b"token".to_vec(), Object::from(self.token.as_slice())),
        ]);
        if self.implied_port {
            dict.insert(b"implied_port".to_vec(), Object::from(1i64));
        }
        else {
            // BEP 5 states: "implied_port which value is either 0 or 1."
            // Some clients might expect 0 if not implied, though not explicitly stated for omission.
            // For safety and clarity, explicitly sending 0 when false.
            dict.insert(b"implied_port".to_vec(), Object::from(0i64));
        }
        return Object::from(dict);
    }

    fn from_bencode(obj: &Object) -> Option<Self> {
        let dict = obj.as_dict()?;
        let id = dict.get(b"id".as_slice())?.as_string()?;
        let info_hash = dict.get(b"info_hash".as_slice())?.as_string()?;
        let port = *dict.get(b"port".as_slice())?.as_int()? as u16;
        let token = dict.get(b"token".as_slice())?.as_string()?;
        let implied_port = match dict.get(b"implied_port".as_slice()) {
            Some(val) => *val.as_int()? == 1,
            None => false, // If not present, assume false.
        };

        return Some(Self {
            id: parse_node_id(id)?,
            info_hash: parse_node_id(info_hash)?, 
            port,
            token: token.to_vec(),
            implied_port,
        });
    }
}

impl KrpcQuery for AnnouncePeerQuery {
    type Reply = AnnouncePeerReply;

    fn method_name() -> &'static [u8] {
        return b"announce_peer".as_slice();
    }
}

impl Message for AnnouncePeerReply {
    fn to_bencode(&self) -> Object {
        // {"id" : "<responding nodes id>"}
        let dict = BTreeMap::from([
            (b"id".to_vec(), Object::from(self.id.as_slice())),
        ]);
        return Object::from(dict);
    }

    fn from_bencode(obj: &Object) -> Option<Self> {
        let dict = obj.as_dict()?;
        let id = dict.get(b"id".as_slice())?.as_string()?;

        return Some(Self {
            id: parse_node_id(id)?,
        });
    }
}

impl KrpcReply for AnnouncePeerReply {

}

// Sample InfoHash...
impl Message for SampleInfoHashesQuery {
    fn to_bencode(&self) -> Object {
        // {
        //     "id": <20 byte id of sending node (string)>,
        //     "target": <20 byte ID for nodes>,
        // },
        let dict: BTreeMap<Vec<u8>, Object> = BTreeMap::from([
            (b"id".to_vec(), Object::from(self.id.as_slice())),
            (b"target".to_vec(), Object::from(self.target.as_slice())),
        ]);
        return Object::from(dict);
    }

    fn from_bencode(obj: &Object) -> Option<Self> {
        let dict = obj.as_dict()?;
        let id = dict.get(b"id".as_slice())?.as_string()?;
        let target = dict.get(b"target".as_slice())?.as_string()?;

        let id = parse_node_id(id)?;
        let target = parse_node_id(target)?;

        return Some(Self {
            id,
            target,
        })
    }
}

impl KrpcQuery for SampleInfoHashesQuery {
    type Reply = SampleInfoHashesReply;

    fn method_name() -> &'static [u8] {
        return b"sample_infohashes";
    }
}

impl Message for SampleInfoHashesReply {
    fn to_bencode(&self) -> Object {
        // {
        //     "id": <20 byte id of sending node (string)>,
        //     "interval": <the subset refresh interval in seconds (integer)>,
        //     "nodes": <nodes close to 'target'>,
        //     "num": <number of infohashes in storage (integer)>,
        //     "samples": <subset of stored infohashes, N × 20 bytes (string)>
        // },
        let mut samples = Vec::new();
        for sample in self.info_hashes.iter() {
            samples.extend_from_slice(sample.as_slice());
        }

        let mut nodes = Vec::new();
        let v4 = if self.nodes.len() > 0 {
            self.nodes[0].1.is_ipv4()
        }
        else {
            true // Emm, empty list, default to v4
        };
        encode_nodes_to(&mut nodes, &self.nodes);

        let mut dict = BTreeMap::from([
            (b"id".to_vec(), Object::from(self.id.as_slice())),
            (b"interval".to_vec(), Object::from(self.interval as i64)),
            (b"sum".to_vec(), Object::from(self.num as i64)),
            (b"samples".to_vec(), Object::from(samples)),
        ]);

        if v4 {
            dict.insert(b"nodes".to_vec(), Object::from(nodes));
        }
        else {
            dict.insert(b"nodes6".to_vec(), Object::from(nodes));
        }

        return Object::from(dict);
    }

    fn from_bencode(obj: &Object) -> Option<Self> {
        let id = obj.get(b"id")?.as_string()?;
        let interval = obj.get(b"interval")?.as_int()?;
        let num = obj.get(b"num")?.as_int()?;
        let samples = obj.get(b"samples")?.as_string()?;

        let mut nodes = obj.get(b"nodes");
        if nodes.is_none() {
            nodes = obj.get(b"nodes6");
        }
        let nodes = nodes?.as_string()?;
        
        let mut info_hashes = Vec::new();
        for i in 0..(samples.len() / 20) {
            let start = i * 20;
            let end = start + 20;
            let info_hash = &samples[start..end];
            info_hashes.push(parse_node_id(info_hash)?);
        }
        // Convert num
        let interval = (*interval).try_into().ok()?;
        let num = (*num).try_into().ok()?;

        return Some(Self {
            id: parse_node_id(id)?,
            interval: interval,
            num: num,
            info_hashes: info_hashes,
            nodes: parse_nodes(&nodes)?,
        })
    }
}

impl KrpcReply for SampleInfoHashesReply {
    
}


// ErrorReply...
impl Message for ErrorReply {
    fn to_bencode(&self) -> Object {
        // [<error code>, "<error message>"]
        return Object::from(vec![
            Object::from(self.code),
            Object::from(self.msg.as_bytes()),
        ]);
    }

    fn from_bencode(obj: &Object) -> Option<Self> {
        let list = obj.as_list()?;
        if list.len() != 2 {
            return None;
        }
        let code = list[0].as_int()?;
        let msg = list[1].as_string()?;
        let msg = String::from_utf8(msg.clone()).ok()?;

        return Some(Self {
            code: *code,
            msg: msg,
        });
    }
}

impl KrpcReply for ErrorReply {
    
}

#[cfg(test)]
mod tests {
    use ctor::ctor;

    use super::*;

    #[ctor]
    fn init_color_backtrace() {
        color_backtrace::install();
    }

    fn encode_and_parse<T>(msg: T) where T: Message + std::fmt::Debug + PartialEq {
        let obj = msg.to_bencode();
        let back = T::from_bencode(&obj);
        assert!(back.is_some(), "Failed to parse back the object");
        assert_eq!(msg, back.unwrap(), "Failed to parse back the object, not eq");
    }

    #[test]
    fn test_find_node_query() {
        let id = NodeId::new([1u8; 20]);
        let target = NodeId::new([2u8; 20]);
        let msg = FindNodeQuery { id, target };
        encode_and_parse(msg);
    }

    #[test]
    fn test_find_node_reply() {
        let id = NodeId::new([1u8; 20]);
        let nodes = vec![
            (NodeId::new([2u8; 20]), SocketAddr::from(([127, 0, 0, 1], 6881))),
            (NodeId::new([3u8; 20]), SocketAddr::from(([127, 0, 0, 1], 6882))),
        ];
        let msg = FindNodeReply { id, nodes };
        encode_and_parse(msg);
    }

    #[test]
    fn test_ping_query() {
        let id = NodeId::new([1u8; 20]);
        let msg = PingQuery { id };
        encode_and_parse(msg);
    }

    #[test]
    fn test_ping_reply() {
        let id = NodeId::new([1u8; 20]);
        let msg = PingReply { id };
        encode_and_parse(msg);
    }

    #[test]
    fn test_get_peers_query() {
        let id = NodeId::new([1u8; 20]);
        let info_hash = InfoHash::new([2u8; 20]);
        let msg = GetPeersQuery { id, info_hash };
        encode_and_parse(msg);
    }

    #[test]
    fn test_get_peers_reply() {
        let id = NodeId::new([1u8; 20]);
        let nodes = vec![
            (NodeId::new([2u8; 20]), SocketAddr::from(([127, 0, 0, 1], 6881))),
            (NodeId::new([3u8; 20]), SocketAddr::from(([127, 0, 0, 1], 6882))),
        ];
        let values = vec![
            SocketAddr::from(([127, 0, 0, 1], 6881)),
            SocketAddr::from(([127, 0, 0, 1], 6882))
        ];
        let msg = GetPeersReply {
            id: id,
            token: b"token".to_vec(),
            nodes: nodes,
            values: values
        };
        encode_and_parse(msg);
    }
}