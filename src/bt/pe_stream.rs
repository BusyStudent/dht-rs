#![allow(dead_code)] // Let it shutup!

use tokio::io::{AsyncBufReadExt, AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt, BufReader, ReadBuf};
use thiserror::Error;
use num_bigint::BigUint;
use std::{
    cmp::min, io::{self}, pin::{pin, Pin}, task::{Context, Poll}
};
use rc4::{KeyInit, Rc4, StreamCipher}; /// FFFFFUCK!, The Sha1 need Digest::new() and rc4 need KeyInit::new()
use sha1::{Digest, Sha1};
use tracing::{trace};
use crate::{InfoHash};

// From spec
const PE_KEY_LEN: usize = 96;
const PE_MAX_PAD_LEN: usize = 512;
const PE_PRIME_STR: &[u8] = b"FFFFFFFFFFFFFFFFC90FDAA22168C234C4C6628B80DC1CD129024E088A67CC74020BBEA63B139B22514A08798E3404DDEF9519B3CD3A431B302B0A6DF25F14374FE1356D6D51C245E485B576625E7EC6F44C42E9A63A36210000000000090563";
const PE_VC: [u8; 8] = [0u8; 8];

lazy_static::lazy_static! {
    static ref PE_PRIME: BigUint = BigUint::parse_bytes(PE_PRIME_STR, 16).unwrap();
    static ref PE_G: BigUint = BigUint::from(2u64);
}

#[derive(Debug, Copy, Clone, PartialEq, Eq)]
enum Cipher {
    PassThrough = 0x0, // No-encrption (No PE)
    PlainText   = 0x1, // Plain text
    Rc4         = 0x2, // RC4 encryption
}

#[derive(Error, Debug)]
pub enum PeError {
    #[error("Failed to handshake")]
    HanshakeFailed,

    #[error("We are not interested in this torrent")]
    InfoHashMismatch,

    #[error("Network error: {0}")]
    NetworkError(#[from] io::Error),
}

pub struct PeStream<T> {
    stream: BufReader<T>, // The underlying stream, use BufReader to easily impl the handshake :(
    cipher: Cipher,
    
    // Init...
    init_payload: Vec<u8>,
    
    // Encryption state
    decryptor: Option<Box<dyn StreamCipher> >,
    encryptor: Option<Box<dyn StreamCipher> >,
    encryptor_buf: Vec<u8>,
    encryptor_buf_offset: usize,
}

mod dh {
    use super::*;

    pub fn make_private_key() -> BigUint {
        // 160 bits
        let mut buf = [0u8; 20];
        fastrand::fill(&mut buf);
        return BigUint::from_bytes_be(&buf);
    }

    pub fn make_public_key(private_key: &BigUint) -> BigUint {
        // y = g^x mod p
        return PE_G.modpow(private_key, &PE_PRIME);
    }
    
    pub fn make_shared_key(private_key: &BigUint, public_key: &BigUint) -> BigUint {
        return public_key.modpow(private_key, &PE_PRIME);
    }

    pub fn make_key_to_bytes(key: &BigUint) -> [u8; 96] {
        let mut bytes = [0u8; 96];
        let buf = key.to_bytes_be();
        let offset = 96 - buf.len();
        bytes[offset..].copy_from_slice(&buf[..]);
        return bytes;
    }
}

mod utils {
    use super::*;

    pub fn make_pad_to(buf: &mut Vec<u8>) -> usize {
        let n = fastrand::usize(0..PE_MAX_PAD_LEN);
        let mut bytes = [0u8; PE_MAX_PAD_LEN];
        fastrand::fill(&mut bytes[0..n]);
        buf.extend_from_slice(&bytes[0..n]);
        return n;
    }

    pub fn make_pad_with(buf: &mut Vec<u8>, n: usize) {
        let mut bytes = [0u8; PE_MAX_PAD_LEN];
        fastrand::fill(&mut bytes[0..n]);
        buf.extend_from_slice(&bytes[0..n]);
    }

    pub fn xor_hash(a: &[u8], b: &[u8]) -> [u8; 20] {
        debug_assert!(a.len() == 20 && b.len() == 20);

        let mut out = [0u8; 20];
        for i in 0..20 {
            out[i] = a[i] ^ b[i];
        }
        return out;
    }

    pub fn make_req_hash1(share_key: &BigUint) -> [u8; 20] {
        return <Sha1 as Digest>::new()
            .chain_update(b"req1")
            .chain_update(share_key.to_bytes_be())
            .finalize().into();
    }

    pub fn make_skey_hash(hash: InfoHash) -> [u8; 20] {
        return <Sha1 as Digest>::new()
            .chain_update(b"req2")
            .chain_update(hash.as_slice())
            .finalize().into();
    }

    pub fn make_req_hash2(skey_hash: &[u8], share_key: &BigUint) -> [u8; 20] {
        let h: [u8; 20] = <Sha1 as Digest>::new()
            .chain_update(b"req3")
            .chain_update(&share_key.to_bytes_be())
            .finalize().into();
        return xor_hash(skey_hash, &h);
    }

    pub async fn sync_for<T: AsyncRead + Unpin>(stream: &mut BufReader<T>, sync_mark: &[u8], max: usize) -> Result<(), PeError> {
        debug_assert!(sync_mark.len() > 0);
        let contain_sync = |buf: &[u8]| {
            return buf.windows(sync_mark.len()).position(|win| &win[..] == sync_mark);
        };
        let mut part = Vec::new();
        loop {
            let prev_part_len = part.len();
            let buf = stream.buffer();

            // Add it to part
            part.extend_from_slice(&buf);
            if let Some(pos) = contain_sync(&part) {
                stream.consume(pos - prev_part_len);
                stream.consume(sync_mark.len()); // Discard sync mark
                return Ok(());
            }
            stream.consume(buf.len());

            if part.len() >= max {
                return Err(PeError::HanshakeFailed);
            }

            // Try read more
            if stream.fill_buf().await?.len() == 0 { // NOTE: The tokio fill buf only do io when the buf is empty
                // WTF, EOF?
                return Err(io::Error::new(io::ErrorKind::UnexpectedEof, "Unexpected EOF").into());
            }
        }
    }

    // Read the stream, try sync, return the hash2
    pub async fn server_sync_stream<T: AsyncRead + Unpin>(stream: &mut BufReader<T>, hash1: [u8; 20]) -> Result<[u8; 20], PeError> {
        // PAD... HASH1, HASH2
        trace!("Server sync stream for hash1: {:?}", hash1);
        sync_for(stream, &hash1, PE_MAX_PAD_LEN + hash1.len()).await?;
        let mut hash2 = [0u8; 20];
        stream.read_exact(&mut hash2).await?;
        return Ok(hash2);
    }

    pub async fn client_sync_stream<T: AsyncRead + Unpin>(stream: &mut BufReader<T>, vc: [u8; 8]) -> Result<(), PeError> {
        trace!("Client sync stream for vc: {:?}", vc);
        sync_for(stream, &vc, PE_MAX_PAD_LEN + vc.len()).await?;
        return Ok(());
    }
}

impl<T> PeStream<T> where T: AsyncRead + AsyncWrite + Unpin {

    /// Check if the stream is encrypted
    pub fn has_encryption(&self) -> bool {
        return self.cipher != Cipher::PlainText;
    }

    /// Doing PE handshake as client, it will success if the server is also PE-enabled
    pub async fn client_handshake(stream: T, hash: InfoHash) -> Result<Self, PeError> {
        trace!("Client: handshake for {}", hash);
        let mut stream = BufReader::new(stream);
        // Prepare the key
        let private_key = dh::make_private_key();
        let public_key = dh::make_public_key(&private_key);

        // Send it to server
        {
            let mut buf = Vec::with_capacity(96 + PE_MAX_PAD_LEN);
            buf.extend_from_slice(&dh::make_key_to_bytes(&public_key));
            utils::make_pad_to(&mut buf);
            stream.write_all(&buf).await?;
            stream.flush().await?;
        }

        // Step 2, We read the server's public key
        let share_key = {
            let mut tmp_key = [0u8; 96];
            stream.read_exact(&mut tmp_key).await?;
            let peer_public_key = BigUint::from_bytes_be(&tmp_key);
            let share_key = dh::make_shared_key(&private_key, &peer_public_key);
            trace!("Client: share_key: private_key: {:?}, peer_public_key: {:?}, share_key: {:?}", private_key, peer_public_key, share_key);
            share_key
        };

        // After this, We need send our info hash
        let hash1 = utils::make_req_hash1(&share_key);
        let hash2 = {
            let skey_hash = utils::make_skey_hash(hash);
            utils::make_req_hash2(&skey_hash, &share_key)
        };
        let mut encryptor = {
            let send_key: [u8; 20] = <Sha1 as Digest>::new()
                .chain_update(b"keyA")
                .chain_update(share_key.to_bytes_be())
                .chain_update(hash.as_slice())
                .finalize().into();
            let mut rc4 = Rc4::new(&send_key.into());
            let mut discard = vec![0u8; 1024];
            rc4.apply_keystream(&mut discard);
            rc4
        };
        {
            // Write Hash1 Hash2, Encrypted(VC, crypto_provide, LEN PadC, PadC, LEN IA, IA)
            trace!("Client: send hash1: {:?}, hash2: {:?}", hash1, hash2);
            stream.write_all(&hash1).await?;
            stream.write_all(&hash2).await?;

            let mut enc_buf = Vec::with_capacity(1024);
            let pad_len = fastrand::usize(0..=PE_MAX_PAD_LEN);
            enc_buf.extend_from_slice(&PE_VC);
            enc_buf.extend_from_slice(&(0x3 as u32).to_be_bytes()); // PlainText | Rc4
            enc_buf.extend_from_slice(&(pad_len as u16).to_be_bytes());
            utils::make_pad_with(&mut enc_buf, pad_len);
            enc_buf.extend_from_slice(&(0x0 as u16).to_be_bytes()); // No IA

            encryptor.apply_keystream(&mut enc_buf);
            stream.write_all(&enc_buf).await?;
            stream.flush().await?;
        }
        // Prepare the decryptor
        let mut decryptor = {
            let recv_key: [u8; 20] = <Sha1 as Digest>::new()
                .chain_update(b"keyB")
                .chain_update(share_key.to_bytes_be())
                .chain_update(hash.as_slice())
                .finalize().into();
            let mut rc4 = Rc4::new(&recv_key.into());
            let mut discard = vec![0u8; 1024];
            rc4.apply_keystream(&mut discard);
            rc4
        };

        // Skip the PadB here... do sync, we can only sync here by the VC
        let mut vc = [0u8; 8];
        decryptor.apply_keystream(&mut vc);
        utils::client_sync_stream(&mut stream, vc).await?;

        // We already custom the VC in sync
        // Then read the server's (crypto, LEN Pad, Pad)
        let mut header = [0u8; 6];
        stream.read_exact(&mut header).await?;
        decryptor.apply_keystream(&mut header);
        let crypto = u32::from_be_bytes(header[..4].try_into().unwrap());
        let pad_len = u16::from_be_bytes(header[4..].try_into().unwrap()) as usize;
        let cipher = match crypto {
            0x1 => Cipher::PlainText,
            0x2 => Cipher::Rc4,
            _ => return Err(PeError::HanshakeFailed),
        };
        if pad_len > PE_MAX_PAD_LEN {
            return Err(PeError::HanshakeFailed);
        }
        // Read the pad
        {
            let mut pad_buf = vec![0u8; pad_len];
            stream.read_exact(&mut pad_buf).await?;
            decryptor.apply_keystream(&mut pad_buf);
        }

        // Done
        return Ok(PeStream {
            stream: stream,
            cipher: cipher,
            init_payload: Vec::new(),
            encryptor: match cipher {
                Cipher::PlainText | Cipher::PassThrough => None, // No encryption
                Cipher::Rc4 => Some(Box::new(encryptor)),
            },
            decryptor: match cipher {
                Cipher::PlainText | Cipher::PassThrough => None, // No encryption
                Cipher::Rc4 => Some(Box::new(decryptor)),
            },
            encryptor_buf: Vec::new(),
            encryptor_buf_offset: 0,
        })
    }

    /// Doing PE handshake as server, it will success if the client is PE-enabled or no-encryption
    pub async fn server_handshake<F, It>(stream: T, func: F) -> Result<Self, PeError> 
    where F: FnOnce() -> It, It: Iterator<Item = InfoHash>
    {
        trace!("Server: PE handshake");
        // First read 20 bytes
        let mut stream = BufReader::new(stream);
        let mut tmp_buf = [0u8; 96];

        stream.read_exact(&mut tmp_buf[0..20]).await?;
        if &tmp_buf[0..20] == b"\x13BitTorrent protocol" {
            // Not PE, just pass through
            let init_payload = stream.buffer().to_vec();
            stream.consume(init_payload.len());
            return Ok(Self {
                stream: stream,
                cipher: Cipher::PassThrough,

                // Save the buffer, avoid loss data
                init_payload: init_payload, 

                // Decryptor and encryptor
                decryptor: None,
                encryptor: None,
                encryptor_buf: Vec::new(),
                encryptor_buf_offset: 0,
            });
        }

        // May be PE, do DH, try at least 96 bytes for Key
        // Read the key
        let (public_key, share_key) = {
            stream.read_exact(&mut tmp_buf[20..]).await?;
            let peer_public_key = BigUint::from_bytes_be(&tmp_buf);
            let private_key = dh::make_private_key();
            let public_key = dh::make_public_key(&private_key);
            let share_key = dh::make_shared_key(&private_key, &peer_public_key);
            trace!("Server: public key: {:?}, private key: {:?}, shared key: {:?}", public_key, private_key, share_key);

            (public_key, share_key)
        };


        // Send back
        {
            let mut back_buf = Vec::with_capacity(96 + PE_MAX_PAD_LEN);
            back_buf.extend_from_slice(&dh::make_key_to_bytes(&public_key));
            utils::make_pad_to(&mut back_buf);

            stream.write_all(&back_buf).await?;
            stream.flush().await?;
        }

        // Now, read buffer has PadA, hash1, ...
        let hash1 = utils::make_req_hash1(&share_key);

        // Read until hash1, begin sync, and then find the hash2
        let hash2 = utils::server_sync_stream(&mut stream, hash1).await?;

        let skey_hash = {
            let com: [u8; 20] = <Sha1 as Digest>::new()
                .chain_update(b"req3")
                .chain_update(share_key.to_bytes_be())
                .finalize().into();

            utils::xor_hash(&hash2, &com)
        };

        // Found the hash we need
        let mut hash = None;
        for item in func() {
            let req2: [u8; 20] = <Sha1 as Digest>::new()
                .chain_update(b"req2")
                .chain_update(item.as_slice())
                .finalize().into();

            if req2 == skey_hash {
                // Found it
                hash = Some(item);
                break;
            }
        }
        let hash = hash.ok_or(PeError::InfoHashMismatch)?;
        let recv_key: [u8; 20] = <Sha1 as Digest>::new()
            .chain_update(b"keyA")
            .chain_update(share_key.to_bytes_be())
            .chain_update(hash.as_slice())
            .finalize().into();

        let mut decryptor = {
            let mut rc4 = Rc4::new(&recv_key.into());
            let mut discard = vec![0u8; 1024];
            rc4.apply_keystream(&mut discard);
            rc4
        };

        // End sync, we need to read the rest
        // VC(8b) + crypto_provide(4b) + PadC LEN(2b) + PadC + IA LEN(2b) + IA
        let mut header = [0u8; 14];
        stream.read_exact(&mut header).await?;
        decryptor.apply_keystream(&mut header);
        if &header[0..8] != &PE_VC { // Invalid header
            return Err(PeError::HanshakeFailed);
        }
        let crypto_provide = u32::from_be_bytes(header[8..12].try_into().unwrap());
        let mut encryptor = {
            let send_key: [u8; 20] = <Sha1 as Digest>::new()
                .chain_update(b"keyB")
                .chain_update(share_key.to_bytes_be())
                .chain_update(hash.as_slice())
                .finalize().into();

            let mut encryptor = Rc4::new(&send_key.into());
            let mut discard = vec![0u8; 1024];
            encryptor.apply_keystream(&mut discard);
            encryptor
        };
        let cipher = match crypto_provide {
            0x1 => Cipher::PlainText, // Only Plain Text
            0x2 | 0x3 => Cipher::Rc4, // RC4
            _ => return Err(PeError::HanshakeFailed), // Invalid 
        };

        // Send back the info
        {
            let pad_len = fastrand::usize(0..=PE_MAX_PAD_LEN);
            let mut back_buf = Vec::with_capacity(14 + PE_MAX_PAD_LEN + 20);
            back_buf.extend_from_slice(&PE_VC); // VC
            back_buf.extend_from_slice(&(cipher as u32).to_be_bytes()); // Cipher
            back_buf.extend_from_slice(&(pad_len as u16).to_be_bytes()); // PadC LEN
            utils::make_pad_with(&mut back_buf, pad_len);
            encryptor.apply_keystream(&mut back_buf);
            stream.write_all(&back_buf).await?;
            stream.flush().await?;
        }

        // Skip the PadD and read the IA
        let init_buf = {
            let pad_len = u16::from_be_bytes(header[12..14].try_into().unwrap()) as usize;
            if pad_len as usize > PE_MAX_PAD_LEN {
                return Err(PeError::HanshakeFailed);
            }
            let mut discard = vec![0u8; pad_len];
            stream.read_exact(&mut discard).await?;
            decryptor.apply_keystream(&mut discard);

            let mut ia_len = [0u8; 2];
            stream.read_exact(&mut ia_len).await?;
            decryptor.apply_keystream(&mut ia_len);

            let ia_len = u16::from_be_bytes(ia_len) as usize;
            let mut ia_buf = vec![0u8; ia_len];
            stream.read_exact(&mut ia_buf).await?;
            decryptor.apply_keystream(&mut ia_buf);
            
            ia_buf
        };

        // Handshake done
        return Ok(PeStream {
            stream: stream,
            cipher: cipher,
            init_payload: init_buf,
            encryptor: match cipher {
                Cipher::PlainText | Cipher::PassThrough => None, // No encryption
                Cipher::Rc4 => Some(Box::new(encryptor)),
            },
            decryptor: match cipher {
                Cipher::PlainText | Cipher::PassThrough => None, // No encryption
                Cipher::Rc4 => Some(Box::new(decryptor)),
            },
            encryptor_buf: Vec::new(),
            encryptor_buf_offset: 0,
        });
    }
}

impl<T> AsyncRead for PeStream<T> where T: AsyncRead + AsyncWrite + Unpin {
    fn poll_read(
        self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        buf: &mut ReadBuf<'_>,
        ) -> Poll<io::Result<()> > 
    {
        let this = self.get_mut();
        if !this.init_payload.is_empty() {
            // Has some init payload in it, firstly custom it
            let len = min(buf.remaining(), this.init_payload.len());
            buf.put_slice(&this.init_payload[..len]);
            this.init_payload.drain(..len);
            return Poll::Ready(Ok(()));
        }
        match this.cipher {
            Cipher::PlainText | Cipher::PassThrough => { // Just forward
                return pin!(&mut this.stream).poll_read(cx, buf);
            }
            Cipher::Rc4 => {
                let prev = buf.filled().len();
                let res = match pin!(&mut this.stream).poll_read(cx, buf) {
                    Poll::Ready(what) => what,
                    Poll::Pending => return Poll::Pending,
                };
                // Ok, decrypt it
                if let Ok(_) = &res {
                    let content = &mut buf.filled_mut()[prev..];
                    this.decryptor.as_mut().unwrap().apply_keystream(content);
                }
                return Poll::Ready(res);
            }
        }
    }
}

impl<T> AsyncWrite for PeStream<T> where T: AsyncRead + AsyncWrite + Unpin {
    fn poll_write(
        self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        buf: &[u8],
        ) -> Poll<io::Result<usize> > 
    {
        let this = self.get_mut();
        match this.cipher {
            Cipher::PlainText | Cipher::PassThrough => { // Just forward
                return pin!(&mut this.stream).poll_write(cx, buf);
            }
            Cipher::Rc4 => {
                // Try to encrypt it
                if this.encryptor_buf.is_empty() {
                    this.encryptor_buf.extend_from_slice(buf);
                    this.encryptor.as_mut().unwrap().apply_keystream(&mut this.encryptor_buf);
                }
                // Send all the encrypted data
                while this.encryptor_buf_offset < this.encryptor_buf.len() { // Not full sent
                    match pin!(&mut this.stream).poll_write(cx, &this.encryptor_buf[this.encryptor_buf_offset..]) {
                        Poll::Pending => return Poll::Pending,
                        Poll::Ready(Err(e)) => return Poll::Ready(Err(e)),
                        Poll::Ready(Ok(len)) => this.encryptor_buf_offset += len,
                    }
                }
                debug_assert!(buf.len() == this.encryptor_buf.len());
                this.encryptor_buf_offset = 0;
                this.encryptor_buf.clear();
                return Poll::Ready(Ok(buf.len()));
            }
        }
    }

    // We do no-op on it, just forward it
    fn poll_flush(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Result<(), io::Error> > {
        return pin!(&mut self.get_mut().stream).poll_flush(cx);
    }

    fn poll_shutdown(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Result<(), io::Error> > {
        return pin!(&mut self.get_mut().stream).poll_shutdown(cx);
    }
}


#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn test_pe_stream() {
        let (a, b) = tokio::io::duplex(1024);
        let hash = InfoHash::zero();
        // Start handshake...
        let (a, b) = tokio::join!(
            PeStream::server_handshake(a, || {
                return vec![hash].into_iter();
            }),
            PeStream::client_handshake(b, hash),
        );
        let mut left = a.unwrap();
        let mut right = b.unwrap();

        // Try to write some data
        let data = b"Hello, world!";
        left.write_all(data).await.unwrap();
        let mut buf = [0u8; 1024];
        let n = right.read(&mut buf).await.unwrap();
        assert_eq!(n, data.len());
        assert_eq!(&buf[..n], data);
    }
}