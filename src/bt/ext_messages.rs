#![allow(dead_code)] // Let it shutup!

use crate::bencode::Object;

/// The message of the ut_metadata
/// 
/// See more on https://www.bittorrent.org/beps/bep_0009.html
#[derive(Clone, Debug, Copy)]
pub enum UtMetadataMessage {
    Request {
        piece: u64,
    },
    Data { 
        piece: u64, 
        total_size: u64
    },
    Reject {
        piece: u64,
    },

    Unknown, // According to the spec, we should unrecognized message ID, so i added it
}

impl UtMetadataMessage {
    pub fn to_bencode(&self) -> Object {
        match self {
            UtMetadataMessage::Request { piece } => {
                return Object::from([
                    (b"msg_type".to_vec(), Object::from(0)),
                    (b"piece".to_vec(), Object::from(*piece as i64)),
                ])
            },
            UtMetadataMessage::Data { piece, total_size } => {
                return Object::from([
                    (b"msg_type".to_vec(), Object::from(1)),
                    (b"piece".to_vec(), Object::from(*piece as i64)),
                    (b"total_size".to_vec(), Object::from(*total_size as i64)),
                ]);
            },
            UtMetadataMessage::Reject { piece } => {
                return Object::from([
                    (b"msg_type".to_vec(), Object::from(2)),
                    (b"piece".to_vec(), Object::from(*piece as i64)),
                ])
            },
            _ => panic!("You should not call this function on UtMetadataMessage::Unknown")
        }
    }

    pub fn from_bencode(obj: &Object) -> Option<UtMetadataMessage> {
        let msg_type = obj.get(b"msg_type")?.as_int()?;
        match msg_type {
            0 => {
                let piece = obj.get(b"piece")?.as_int()?;
                return Some(UtMetadataMessage::Request { piece: *piece as u64 });
            },
            1 => {
                let piece = obj.get(b"piece")?.as_int()?;
                let total_size = obj.get(b"total_size")?.as_int()?;
                return Some(UtMetadataMessage::Data { piece: *piece as u64, total_size: *total_size as u64 });
            },
            2 => {
                let piece = obj.get(b"piece")?.as_int()?;
                return Some(UtMetadataMessage::Reject { piece: *piece as u64 });
            },
            _ => return Some(UtMetadataMessage::Unknown),
        }
    }
}