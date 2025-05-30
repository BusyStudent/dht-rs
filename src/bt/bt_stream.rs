#![allow(dead_code)] // Let it shutup!
use tokio::io::{AsyncRead, AsyncWrite, AsyncWriteExt, AsyncReadExt};
use tokio::io;
use thiserror::Error;
use crate::bencode::Object;
use crate::InfoHash;

// LEN(1) + "Bittorrent protocol"(19) + reserved bytes(8) + infohash(20) + peerid(20)
// struct alignas (1) BtHandshakeMessage {
//     std::byte pstrlen;
//     std::array<std::byte, 19> pstr;
//     std::array<std::byte, 8> reserved;
//     InfoHash infoHash;
//     PeerId peerId; 20 bytes
// };

const BT_PROTOCOL_STR : &'static str = "Bittorrent protocol";
const BT_PROTOCOL_LEN : usize = BT_PROTOCOL_STR.len();

pub type PeerId = [u8; 20];

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u8)]
pub enum BtMessageId {
    Choke = 0,
    Unchoke = 1,
    Interested = 2,
    NotInterested = 3,
    Have     = 4,
    Bitfield = 5,
    Request  = 6,
    Piece    = 7,
    Cancel   = 8,
    Extended = 20, // The message from extension
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BtMessage {
    id: BtMessageId,
    ext: Option<(u8, Object)>, // The extension object
}

struct BtStreamInner {

}

/// The information to handshake
#[derive(Clone)]
pub struct BtHandshakeInfo {
    hash      : InfoHash,
    peer_id   : PeerId,
    extensions: Option<Object>, // The dict, {"m": {xxx}, "v":"xxx"}
}

pub struct BtStream<T> {
    stream: T, // The underlying stream
    local_info: BtHandshakeInfo,
    remote_info: BtHandshakeInfo, // The remote handshake info
}

#[derive(Debug, Error)]
pub enum BtError {
    #[error("Failed to handshake, because of the wrong message format")]
    HandshakeFailed,

    #[error("We received an invalid message")]
    InvalidMessage,

    #[error("The message we received has too large len")]
    MessageLenToLarge,

    #[error("NetworkError {:?}", .0)]
    NetworkError(#[from] io::Error),
}

mod utils {
    use super::*;

    /// Helper function to write the common handshake info
    pub async fn write_handshake<T: AsyncWrite + Unpin>(stream: &mut T, info: &BtHandshakeInfo) -> io::Result<()> {
        // LEN(1) + "Bittorrent protocol"(19) + reserved bytes(8) + infohash(20) + peerid(20)
        let mut buffer = [0u8; 68];
        let mut cursor = &mut buffer[..];

        // Write Bittorrent protocol
        cursor[0] = BT_PROTOCOL_LEN as u8;
        cursor[1..BT_PROTOCOL_LEN + 1].copy_from_slice(BT_PROTOCOL_STR.as_bytes());
        cursor = &mut cursor[BT_PROTOCOL_LEN + 1..];

        if !info.extensions.is_none() {
            cursor[5] = 0x10; // We have extensions
        }
        cursor = &mut cursor[8..];

        // Copy hash and peer_id
        cursor[0..20].copy_from_slice(info.hash.as_slice());
        cursor[20..].copy_from_slice(info.peer_id.as_slice());

        stream.write_all(buffer.as_slice()).await?;
        return Ok(());
    }

    /// Helper function to read the common handshake
    pub async fn read_handshake<T: AsyncRead + Unpin>(stream: &mut T) -> Result<(InfoHash, PeerId, bool), BtError> {
        // LEN(1) + "Bittorrent protocol"(19) + reserved bytes(8) + infohash(20) + peerid(20)
        let mut buffer = [0u8, 68];
        stream.read_exact(&mut buffer).await?;
        // Check headers
        let mut cursor = &buffer[..];
        if cursor[0] as usize != BT_PROTOCOL_LEN ||  &cursor[1..BT_PROTOCOL_LEN + 1] != BT_PROTOCOL_STR.as_bytes() {
            return Err(BtError::HandshakeFailed);
        }
        cursor = &cursor[BT_PROTOCOL_LEN + 1..];

        // Check reverse bytes
        let reverse_bytes = &cursor[0..8];
        let has_ext = (reverse_bytes[5] & 0x10) != 0;
        cursor = &cursor[8..];

        // Get the info hash and peer_id;
        let mut hash = [0u8; 20];
        let mut peer_id = [0u8; 20];
        hash.copy_from_slice(&cursor[0..20]);
        peer_id.copy_from_slice(&cursor[20..]);

        // All done
        return Ok((InfoHash::from(hash), peer_id, has_ext));
    }

    pub async fn write_handshake_ext<T: AsyncWrite + Unpin>(stream: &mut T, info: &BtHandshakeInfo) -> io::Result<()> {
        match &info.extensions {
            None => panic!("You should not call it if the ext is none"),
            Some(ext) => {
                return write_message_ext(stream, 0, ext).await;
            }
        }
    }

    pub async fn read_handshake_ext<T: AsyncRead + Unpin>(stream: &mut T) -> Result<Object, BtError> {
        let mut buf = Vec::new();
        let (msg, payload) = read_message(stream, &mut buf).await?;
        if msg.id != BtMessageId::Extended || !payload.is_empty() {
            return Err(BtError::HandshakeFailed);
        }
        let (ext_id, obj) = msg.ext.unwrap();
        if ext_id != 0 { // Not handshake
            return Err(BtError::HandshakeFailed);
        }
        return Ok(obj);
    }

    // Message...
    pub async fn write_message<T: AsyncWrite + Unpin>(stream: &mut T, id: BtMessageId, buf: &[u8]) -> io::Result<()> {
        // Header u32 + u8(id)
        let mut header = [0u8; 5];
        let len = (buf.len() + 1) as u32; // + 1 for contains the id
        header[0..4].copy_from_slice(len.to_be_bytes().as_slice());
        header[4] = id as u8;

        stream.write_all(header.as_slice()).await?;
        stream.write_all(buf).await?;
        return Ok(());
    }

    pub async fn write_message_ext<T: AsyncWrite + Unpin>(stream: &mut T, ext_id: u8, obj: &Object) -> io::Result<()> {
        let mut vec = Vec::new();
        vec.push(ext_id);
        obj.encode_to(&mut vec);
        return write_message(stream, BtMessageId::Extended, vec.as_slice()).await;
    }

    pub async fn read_message<'a, T: AsyncRead + Unpin>(stream: &mut T, buffer: &'a mut Vec<u8>) -> Result<(BtMessage, &'a [u8]), BtError> {
        // Header u32 + u8(id)
        let mut header = [0u8; 4];
        loop {
            stream.read_exact(&mut header).await?;
            let len = u32::from_be_bytes(header);
            if len == 0 { // Keep alive
                continue;
            }
            if len > 1024 * 1024 * 50 { // 50MB, too large
                return Err(BtError::MessageLenToLarge);
            }
            buffer.resize(len as usize, 0);
            stream.read_exact(buffer.as_mut_slice()).await?;

            // Parse message id
            let raw_id = buffer[0];
            if raw_id > 8 && raw_id != 20 {
                return Err(BtError::InvalidMessage);
            }
            let id: BtMessageId = unsafe { // TAKE an short cut :), we alreay checked
                std::mem::transmute(raw_id)
            };

            // Parse the extension id
            let mut cursor = &buffer[1..]; // Skip the first id
            let mut msg = BtMessage {
                id: id,
                ext: None,
            };
            if id == BtMessageId::Extended {
                let ext_id = cursor[0];
                let (obj, left) = match Object::decode(&cursor[1..]) {
                    Some(val) => val,
                    None => return Err(BtError::InvalidMessage),
                };
                msg.ext = Some((ext_id, obj));
                cursor = left;
            }
            return Ok((msg, cursor));
        }
    }
}

impl<T> BtStream<T> where T: AsyncRead + AsyncWrite + Unpin {

    // Handshake like an client
    pub async fn client_handshake(mut stream: T, info: &BtHandshakeInfo) -> Result<BtStream<T> , BtError> {
        utils::write_handshake(&mut stream, &info).await?;
        let (info_hash, peer_id, has_ext) = utils::read_handshake(&mut stream).await?;
        if info_hash != info.hash { // Info Hash mismatch
            return Err(BtError::HandshakeFailed);
        }
        
        // Begin the ext handshake
        let extensions = if has_ext {
            utils::write_handshake_ext(&mut stream, &info).await?;
            let obj = utils::read_handshake_ext(&mut stream).await?;
            
            Some(obj)
        }
        else {
            None
        };

        return Ok(
            BtStream {
                stream: stream,
                local_info: info.clone(),
                remote_info: BtHandshakeInfo {
                    hash: info_hash,
                    peer_id: peer_id,
                    extensions: extensions
                }
            }
        );
    }

    pub async fn read_message<'a>(&mut self, buffer: &'a mut Vec<u8>) -> Result<(BtMessage, &'a [u8]), BtError> {
        return utils::read_message(&mut self.stream, buffer).await;
    }

    // pub async fn write_message(&mut self, msg: &BtMessage, payload: &[u8]) -> io::Result<()> {
    //     if msg.id == BtMessageId::Extended {

    //     }
    // }
}