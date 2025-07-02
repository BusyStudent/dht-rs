#![allow(dead_code)] // Let it shutup!
use std::collections::BTreeMap;
use std::future::Future;
use tokio::io::{AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt};
use tokio::io;
use tracing::{trace, debug};
use thiserror::Error;
use crate::bencode::Object;
use crate::InfoHash;
use super::PeError;

// LEN(1) + "BitTorrent protocol"(19) + reserved bytes(8) + infohash(20) + peerid(20)
// struct alignas (1) BtHandshakeMessage {
//     std::byte pstrlen;
//     std::array<std::byte, 19> pstr;
//     std::array<std::byte, 8> reserved;
//     InfoHash infoHash;
//     PeerId peerId; 20 bytes
// };

const BT_PROTOCOL_STR : &[u8] = b"BitTorrent protocol";
const BT_PROTOCOL_LEN : usize = BT_PROTOCOL_STR.len();
const MAX_MESSAGE_LEN : u32 = 1024 * 1024 * 50;

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
pub enum BtMessage {
    /// No Payload
    Choke,
    Unchoke,
    Interested,
    NotInterested,

    // Message have payload
    Have { 
        piece_index: u32 
    },
    Bitfield {
        data: Vec<u8>
    },
    Request { 
        index: u32, 
        begin: u32, 
        length: u32
    },
    Piece { 
        index: u32, 
        begin: u32, 
        block: Vec<u8>
    },
    Cancel { 
        index: u32, 
        begin: u32, 
        length: u32
    },
    Extended {
        id: u8, 
        msg: Object, 
        payload: Vec<u8> // The payload behind the data
    },
}

struct BtStreamInner {

}

#[derive(Clone, Debug, Copy)]
pub struct PeerId {
    data: [u8; 20]
}

/// The information of the full handshake
#[derive(Clone, Debug)]
pub struct BtHandshakeInfo {
    pub hash      : InfoHash,
    pub peer_id   : PeerId,
    pub extensions: Option<Object>, // The dict, {"m": {xxx}, "v":"xxx"}
}

/// The information of the handshake request (normal bt handshake)
#[derive(Clone, Debug)]
pub struct BtHandshakeRequest {
    pub hash      : InfoHash,
    pub peer_id   : PeerId,
    pub extensions: bool,
}

#[derive(Debug)]
pub struct BtStream<T> {
    stream: T, // The underlying stream
    read_buf: Vec<u8>,
    write_buf: Vec<u8>,
    local_info: BtHandshakeInfo,
    peer_info: BtHandshakeInfo, // The remote handshake info
}

#[derive(Debug, Error)]
pub enum BtError {
    // Pe Part
    #[error("Error from the low level pe part: {}", .0)]
    EncryptionFailed(#[from] PeError),

    // Common Part
    #[error("Failed to do the extension handshake")]
    ExtHanshakeFailed,

    #[error("Failed to handshake, because of the wrong protocol string: {:?}", .0)]
    InvalidProtocolString([u8; 20]),

    #[error("Failed to handshake, because of the hanshake request infohash mismatch")]
    InfoHashMismatch(InfoHash),

    #[error("We received an invalid message")]
    InvalidMessage,

    #[error("The message we received has too large len")]
    MessageTooLarge,

    #[error("The peer doesn't support extension we want")]
    UnsupportedExtension,

    #[error("No piece index in the request")]
    NoPieceIndex,

    #[error("User defined error: {}", .0)]
    UserDefined(String),

    // Network Part
    #[error("NetworkError {:?}", .0)]
    NetworkError(#[from] io::Error),
}


mod utils {
    use tracing::warn;

    use super::*;

    /// Helper function to write the common handshake info
    pub async fn write_handshake<T: AsyncWrite + Unpin>(stream: &mut T, info: &BtHandshakeInfo) -> io::Result<()> {
        // LEN(1) + "BitTorrent protocol"(19) + reserved bytes(8) + infohash(20) + peerid(20)
        trace!("Write handshake...");
        let mut buffer = [0u8; 68];
        let mut cursor = buffer.as_mut();

        // Write BitTorrent protocol
        cursor[0] = BT_PROTOCOL_LEN as u8;
        cursor[1..BT_PROTOCOL_LEN + 1].copy_from_slice(BT_PROTOCOL_STR);
        cursor = &mut cursor[BT_PROTOCOL_LEN + 1..];

        if !info.extensions.is_none() {
            cursor[5] = 0x10; // We have extensions
        }
        cursor = &mut cursor[8..];

        // Copy hash and peer_id
        cursor[0..20].copy_from_slice(info.hash.as_slice());
        cursor[20..].copy_from_slice(info.peer_id.as_slice());

        stream.write_all(buffer.as_slice()).await?;
        stream.flush().await?;
        trace!("Write handshake done");
        return Ok(());
    }

    /// Helper function to read the common handshake
    pub async fn read_handshake<T: AsyncRead + Unpin>(stream: &mut T) -> Result<BtHandshakeRequest, BtError> {
        // LEN(1) + "BitTorrent protocol"(19) + reserved bytes(8) + infohash(20) + peerid(20)
        trace!("Read handshake...");
        let mut buffer = [0u8; 68];
        stream.read_exact(&mut buffer).await?;
        // Check headers
        let mut cursor = &buffer[..];
        if cursor[0] as usize != BT_PROTOCOL_LEN ||  &cursor[1..BT_PROTOCOL_LEN + 1] != BT_PROTOCOL_STR {
            // Dump it
            let span = cursor[0..20].try_into().unwrap();
            #[cfg(debug_assertions)] { // Only dump in debug mode
                let backtrace = std::backtrace::Backtrace::capture();
                warn!("Invalid protocol string str: {:?}, backtrace: {:?}", span, backtrace);
            }
            return Err(BtError::InvalidProtocolString(span));
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
        trace!("Read hanshake(hash: {}, peer_id: {:?}, has_ext: {}", InfoHash::from(hash), PeerId::from(peer_id), has_ext);
        let request = BtHandshakeRequest {
            hash: InfoHash::from(hash),
            peer_id: PeerId::from(peer_id),
            extensions: has_ext,
        };
        return Ok(request);
    }

    // Message...
    pub async fn write_message<T: AsyncWrite + Unpin>(stream: &mut T, buffer: &mut Vec<u8>, msg: &BtMessage) -> io::Result<()> {
        // Header u32 + u8(id)
        buffer.clear();
        msg.encode_to(buffer);
        trace!("Write {:?} message, len {}", msg.id(), buffer.len());
        stream.write_all(&buffer).await?;
        stream.flush().await?;
        return Ok(());
    }

    pub async fn read_message<T: AsyncRead + Unpin>(stream: &mut T, buffer: &mut Vec<u8>) -> Result<BtMessage, BtError> {
        // Header u32 + u8(id)
        if buffer.len() < 4 {
            buffer.resize(4, 0);
        }
        loop {
            stream.read_exact(&mut buffer[0..4]).await?;
            let len = u32::from_be_bytes(buffer[0..4].try_into().unwrap());
            if len == 0 { // Keep alive
                continue;
            }
            if len > MAX_MESSAGE_LEN { // 50MB, too large
                return Err(BtError::MessageTooLarge);
            }
            buffer.resize((len + 4) as usize, 0);
            stream.read_exact(&mut buffer[4..]).await?;
            
            let msg = BtMessage::decode(&buffer)?;
            debug!("Read {:?} message", msg.id());
            return Ok(msg);
        }
    }
}

impl From<[u8; 20]> for PeerId {
    fn from(value: [u8; 20]) -> Self {
        return Self { data: value };
    }
}

impl TryFrom<u8> for BtMessageId {
    type Error = BtError;

    fn try_from(value: u8) -> Result<Self, Self::Error> {
        match value {
            0 => return Ok(BtMessageId::Choke),
            1 => return Ok(BtMessageId::Unchoke),
            2 => return Ok(BtMessageId::Interested),
            3 => return Ok(BtMessageId::NotInterested),
            4 => return Ok(BtMessageId::Have),
            5 => return Ok(BtMessageId::Bitfield),
            6 => return Ok(BtMessageId::Request),
            7 => return Ok(BtMessageId::Piece),
            8 => return Ok(BtMessageId::Cancel),
            20 => return Ok(BtMessageId::Extended),
            _ => return Err(BtError::InvalidMessage),
        }
    }
}

impl PeerId {
    /// Get the slice of the peer
    pub fn as_slice(&self) -> &[u8] {
        return self.data.as_slice();
    }

    /// Random create an peer id
    pub fn rand() -> PeerId {
        let mut arr = [0u8; 20];
        for i in &mut arr {
            *i = fastrand::u8(..);
        }
        return PeerId::from(arr);
    }

    pub fn make() -> PeerId {
        let pre = b"-IL00000-";
        let mut id = PeerId::rand();
        id.data[0..pre.len()].copy_from_slice(pre);
        return id;
    }
}

impl BtHandshakeInfo {
    /// Query the pex extension id
    pub fn pex_id(&self) -> Option<u8> {
        return self.extension_id(b"ut_pex");
    }

    /// Query the metadata extension id
    pub fn metadata_id(&self) -> Option<u8> {
        return self.extension_id(b"ut_metadata");
    }
    
    /// Query the extension id
    pub fn extension_id(&self, key: &[u8]) -> Option<u8> {
        // "m" : { "key": id }
        let id = self.extensions.as_ref()?.get(b"m")?.get(key)?.as_int()?;
        if *id < 0 || *id > 255 {
            return None;
        }
        return Some(*id as u8);
    }

    /// Get the metadata size
    pub fn metadata_size(&self) -> Option<u64> {
        // "metadata_size" : size
        let size = self.extensions.as_ref()?.get(b"metadata_size")?.as_int()?;
        if *size < 0 {
            return None;
        }
        return Some(*size as u64);
    }

    /// Set the extension id by key and value
    pub fn set_extension_id(&mut self, key: &[u8], id: u8) {
        // "m" : { "key": id }
        if self.extensions.is_none() { // Init, make new dict
            let dict = BTreeMap::from([
                (b"m".to_vec(), Object::from(BTreeMap::new()))
            ]);
            self.extensions = Some(Object::from(dict));
        }
        let dict = self.extensions.as_mut().unwrap().as_mut_dict().unwrap();
        let dict = dict.get_mut(b"m".as_slice()).expect("It should has m key").as_mut_dict().expect("The m value should is dict");
        dict.insert(key.to_vec(), Object::from(id as i64));
    }

    pub fn set_pex_id(&mut self, id: u8) {
        self.set_extension_id(b"ut_pex", id);
    }

    pub fn set_metadata_id(&mut self, id: u8) {
        self.set_extension_id(b"ut_metadata", id);
    }

}

impl BtMessage {
    /// Get the id of the message
    pub fn id(&self) -> BtMessageId {
        return match self {
            BtMessage::Choke => BtMessageId::Choke,
            BtMessage::Unchoke => BtMessageId::Unchoke,
            BtMessage::Interested => BtMessageId::Interested,
            BtMessage::NotInterested => BtMessageId::NotInterested,
            BtMessage::Have {..} => BtMessageId::Have,
            BtMessage::Bitfield {..} => BtMessageId::Bitfield,
            BtMessage::Request {..} => BtMessageId::Request,
            BtMessage::Piece {..} => BtMessageId::Piece,
            BtMessage::Cancel {..} => BtMessageId::Cancel,
            BtMessage::Extended {..} => BtMessageId::Extended,
        }
    }

    /// Encode the message to vector
    pub fn encode_to(&self, out: &mut Vec<u8>) {
        // Header u32(len, contains this id) + u8(id)
        let pos = out.len();
        out.extend_from_slice(&[0u8; 4]); // As placeholder for the len
        out.push(self.id() as u8);

        let len = match self {
            BtMessage::Have { piece_index } => {
                let bytes = piece_index.to_be_bytes();
                out.extend_from_slice(&bytes);
                
                bytes.len()
            },
            BtMessage::Bitfield { data } => {
                out.extend_from_slice(data);

                data.len()
            },
            BtMessage::Request {..} => todo!(),
            BtMessage::Piece {..} => todo!(),
            BtMessage::Cancel {..} => todo!(),
            BtMessage::Extended {id, msg, payload} => {
                let cur_len = out.len();
                out.push(*id);
                msg.encode_to(out);
                out.extend_from_slice(payload);

                out.len() - cur_len
            },
            // Another message doesnot have payload
            _ => 0,
        } + 1; // +1 for contains the id

        // Set the len
        out[pos..pos + 4].copy_from_slice(&(len as u32).to_be_bytes());
    }

    /// Decode the message from slice
    pub fn decode(bytes: &[u8]) -> Result<BtMessage, BtError> {
        // Header u32(len, contains this id) + u8(id)
        if bytes.len() < 5 { // Al least 5
            return Err(BtError::InvalidMessage);
        }
        let len = u32::from_be_bytes(bytes[0..4].try_into().unwrap());
        let id: BtMessageId = bytes[4].try_into()?;

        if len < 1 {
            return Err(BtError::InvalidMessage);
        }
        if len > MAX_MESSAGE_LEN { // 50MB, too large
            return Err(BtError::MessageTooLarge);
        }

        // Move to the payload begin
        let payload = &bytes[5..];

        match id {
            BtMessageId::Choke => return Ok(BtMessage::Choke),
            BtMessageId::Unchoke => return Ok(BtMessage::Unchoke),
            BtMessageId::Interested => return Ok(BtMessage::Interested),
            BtMessageId::NotInterested => return Ok(BtMessage::NotInterested),
            
            // Have payload
            BtMessageId::Have => {
                let data: [u8; 4] = match payload.try_into() {
                    Ok(val) => val,
                    Err(_) => return Err(BtError::InvalidMessage),
                };
                return Ok(BtMessage::Have { piece_index: u32::from_be_bytes(data) });
            },
            BtMessageId::Bitfield => {
                let data = Vec::from(payload);
                return Ok(BtMessage::Bitfield { data: data });
            },
            BtMessageId::Request => todo!("impl Request"),
            BtMessageId::Piece => todo!("impl Piece"),
            BtMessageId::Cancel => todo!("impl Cancel"),
            BtMessageId::Extended => {
                if payload.len() < 3 { // At least need extension id and an empty dict
                    return Err(BtError::InvalidMessage);
                }
                let ext_id = payload[0];
                let (msg, left) = match Object::decode(&payload[1..]) {
                    Some(val) => val,
                    None => return Err(BtError::InvalidMessage),
                };
                return Ok(BtMessage::Extended { id: ext_id, msg: msg, payload: Vec::from(left) });
            },
        }
    }

}

impl<T> BtStream<T> where T: AsyncRead + AsyncWrite + Unpin {

    pub fn peer_info(&self) -> &BtHandshakeInfo {
        return &self.peer_info;
    }

    pub fn local_info(&self) -> &BtHandshakeInfo {
        return &self.local_info;
    }

    /// Do the ext handshake (if needed) between client and server and than build it
    /// 
    /// `request`: The handshake request from remote
    /// 
    /// `info`: The handshake info of local
    async fn build(mut stream: T, request: BtHandshakeRequest, info: BtHandshakeInfo) -> Result<BtStream<T> , BtError> {
        let mut read_buf = Vec::new();
        let mut write_buf = Vec::new();

        // Begin the ext handshake
        let extensions = if request.extensions && info.extensions.is_some() {
            // Write our handle shake out
            let out_msg = BtMessage::Extended { id: 0, msg: info.extensions.as_ref().unwrap().clone(), payload: Vec::new() };
            utils::write_message(&mut stream, &mut write_buf, &out_msg).await?;

            // Read the handleshake
            let in_msg = utils::read_message(&mut stream, &mut read_buf).await?;

            // Check it
            match in_msg {
                BtMessage::Extended { id, msg, payload } => {
                    if id != 0 || !payload.is_empty() {
                        return Err(BtError::ExtHanshakeFailed);
                    }
                    Some(msg)
                },
                _ => return Err(BtError::ExtHanshakeFailed),
            }
        }
        else {
            None
        };

        return Ok(
            BtStream {
                stream: stream,
                read_buf: read_buf,
                write_buf: write_buf,
                local_info: info,
                peer_info: BtHandshakeInfo {
                    hash: request.hash,
                    peer_id: request.peer_id,
                    extensions: extensions
                }
            }
        );
    }

    /// Handshake like an client
    pub async fn client_handshake(mut stream: T, info: BtHandshakeInfo) -> Result<BtStream<T> , BtError> {
        utils::write_handshake(&mut stream, &info).await?;
        let request = utils::read_handshake(&mut stream).await?;
        if request.hash != info.hash { // Info Hash mismatch
            return Err(BtError::InfoHashMismatch(request.hash));
        }
        return BtStream::build(stream, request, info).await;
    }

    /// Handshake like an server, and call the callback to get the handshake info
    /// 
    /// `cb`: The async callback to get the handshake info, the result is Result<BtHandahakeInfo, BtError>
    pub async fn server_handshake<F, Fut>(mut stream: T, cb: F) -> Result<BtStream<T> , BtError> 
        where F: FnOnce(BtHandshakeRequest) -> Fut, Fut: Future<Output = Result<BtHandshakeInfo, BtError> >
    {
        let request = utils::read_handshake(&mut stream).await?;
        let info = cb(request.clone()).await?;
        return BtStream::build(stream, request, info).await;
    }

    /// Read an message from the stream
    pub async fn read_message(&mut self) -> Result<BtMessage, BtError> {
        return utils::read_message(&mut self.stream, &mut self.read_buf).await;
    }


    /// Write an message to the stream
    pub async fn write_message(&mut self, msg: &BtMessage) -> io::Result<()> {
        return utils::write_message(&mut self.stream, &mut self.write_buf, &msg).await;
    }
}

#[cfg(test)]
mod tests {
    #![allow(unused_imports)]
    use crate::{bt::{PeStream, UtMetadataMessage}, utp::{UtpContext, UtpSocket} };

    use super::*;
    use tracing::error;
    use tokio::{net};
    use std::sync::Arc;

    #[tokio::test]
    #[ignore]
    async fn test_bt_stream() -> Result<(), BtError> {
        // Test on my bittorrent
        // let stream = net::TcpStream::connect("127.0.0.1:35913").await?;
        let udp = Arc::new(net::UdpSocket::bind("127.0.0.1:0").await?);
        let utp_context = UtpContext::new(udp.clone());
        let utp_context2 = utp_context.clone();
        let worker = tokio::spawn(async move {
            let mut buf = [0u8; 65535];
            loop {
                match udp.recv_from(&mut buf).await {
                    Ok((len, addr)) => {
                        let _ = utp_context2.process_udp(&buf[..len], &addr);
                    },
                    Err(err) => {
                        error!("Error: {err}");
                    }
                }
            }
        });
        let stream = UtpSocket::connect(&utp_context, "127.0.0.1:35913".parse().unwrap()).await?;

        let info = BtHandshakeInfo {
            hash: InfoHash::from_hex("4ce5c1ec28454f6f0e5c009e74df3a62a9efafa8").unwrap(),
            peer_id: PeerId::make(),
            extensions: Some(
                Object::from([
                    (b"m".to_vec(), Object::from([(b"ut_metadata".to_vec(), Object::from(1))])),
                    (b"v".to_vec(), Object::from(b"Test BitTorrent"))
                ])
            )
        };

        // Test Use PE?
        let stream = PeStream::client_handshake(stream, info.hash).await.unwrap();

        // TODO:_
        let mut bt_stream = BtStream::client_handshake(stream, info).await?;
        let metadata_size = match bt_stream.peer_info().metadata_size() {
            Some(val) => val,
            None => panic!("It should have size"),
        };
        let peer_metadata_id = match bt_stream.peer_info().metadata_id() {
            Some(val) => val,
            None => panic!("It should have size"),
        };
        let pieces = (metadata_size + 16383) / 16384; // 16KB per piece
        let mut torrent = Vec::new();
        for i in 0..pieces {
            // Request it
            let request = UtMetadataMessage::Request { piece: i };
            bt_stream.write_message(&BtMessage::Extended { id: peer_metadata_id, msg: request.to_bencode(), payload: Vec::new() }).await?;
            loop {
                let (id, msg, payload) = match bt_stream.read_message().await? {
                    BtMessage::Extended { id, msg, payload } => (id, msg, payload),
                    _ => continue,
                };
                if id != 1 {
                    continue; // Ignore
                }
                let ut_msg = UtMetadataMessage::from_bencode(&msg).unwrap(); // I don't want to check it, it just as a test
                match ut_msg {
                    UtMetadataMessage::Data { piece, total_size: _ } => {
                        if piece != i {
                            error!("Got wrong piece {piece}");
                            break;
                        }
                    },
                    _ => {
                        error!("Got wrong message {ut_msg:?}");
                        return Err(BtError::InvalidMessage);
                    }
                }

                // Copy into it
                torrent.extend_from_slice(payload.as_slice());
                break;
            }
        }
        // Try to decode it
        let (obj, _) = match Object::decode(torrent.as_slice()) {
            Some(val) => val,
            None => return Ok(()),
        };
        debug!("Got torrent {:?}", obj);
        worker.abort();
        let _ = worker.await;
        return Ok(());
    }
}