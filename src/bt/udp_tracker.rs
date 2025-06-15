#![allow(dead_code)] // Let it shutup!
// TODO:
// use crate::InfoHash;
// use crate::PeerId;
use std::net::SocketAddr;

pub struct UdpTracker {
    ip: SocketAddr,
    url: String,
}