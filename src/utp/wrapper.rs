#![allow(non_camel_case_types)]

use super::addr::*;
use super::ffi::*;
use libc::{c_int, c_void, size_t};
use tracing::warn;
// use tracing::info;
use std::fmt;
use std::sync::Weak;
use std::{
    collections::VecDeque,
    io, mem,
    net::SocketAddr,
    pin::Pin,
    future::Future,
    sync::{Arc, Mutex},
    task::{Context, Poll, Waker},
    time::Duration,
};
use tokio::{
    io::{AsyncRead, AsyncWrite, ReadBuf},
    net::UdpSocket,
    sync::mpsc,
};

use tracing::{debug, error};

#[derive(Debug, Clone, Copy)]
struct utp_context_t(*mut utp_context);

#[derive(Debug, Clone, Copy)]
struct utp_socket_t(*mut utp_socket);

// The context is the main object that is used to create sockets
struct UtpContextInner {
    utp_ctxt: Mutex<utp_context_t>, // The UTP context is not thread safe, so we need to lock it
    udp_send: mpsc::Sender<(Vec<u8>, SocketAddr)>,
    sock_send: Mutex<Option<mpsc::Sender<utp_socket_t> > >, // The socket send channel, for accept new socket
}

#[derive(Default)]
struct UtpSocketState {
    eof: bool,       // Did the stream eof?
    connected: bool, // Did we connect?
    read_buf: VecDeque<u8>,
    read_waker: Option<Waker>,
    write_waker: Option<Waker>,
    error_code: Option<UTP_ERROR>, // The error code from the libutp
}
struct UtpSocketInner {
    utp_socket: utp_socket_t, // The managed utp socket ptr
    state: Mutex<UtpSocketState>,
}

struct UtpConnectFuture {
    sock: Option<UtpSocket>,
}

/// The UTP Context, shared
#[derive(Clone)]
pub struct UtpContext {
    inner: Arc<UtpContextInner>,
}

/// The UTP Socket
pub struct UtpSocket {
    ctxt: Arc<UtpContextInner>,
    inner: Box<UtpSocketInner>,
}

pub struct UtpListener {
    ctxt: Arc<UtpContextInner>,
    receiver: mpsc::Receiver<utp_socket_t>,
}

impl UtpContext {
    async fn utp_sendto_loop(udp: Arc<UdpSocket>, mut rx: mpsc::Receiver<(Vec<u8>, SocketAddr)>) {
        while let Some((data, addr)) = rx.recv().await {
            let _ = udp.send_to(&data, &addr).await;
        }
    }

    async fn utp_timer_loop(weak: Weak<UtpContextInner>) {
        loop {
            tokio::time::sleep(Duration::from_millis(500)).await;
            let this = match weak.upgrade() {
                Some(val) => val,
                None => return, // The context is gone, so we can stop
            };
            let ctxt = this.utp_ctxt.lock().unwrap();
            unsafe {
                utp_check_timeouts(ctxt.get());
                utp_issue_deferred_acks(ctxt.get());
            }
        }
    }

    pub fn new(udp: Arc<UdpSocket>) -> UtpContext {
        unsafe {
            let utp_ctxt = utp_init(2); // We use version 2
            assert!(!utp_ctxt.is_null());

            // Ok Bind it
            let (tx, rx) = mpsc::channel(1024);
            let inner = Arc::new(UtpContextInner {
                utp_ctxt: Mutex::new(utp_context_t(utp_ctxt)),
                udp_send: tx,
                sock_send: Mutex::new(None),
            });
            let ptr = inner.as_ref() as *const UtpContextInner;
            utp_context_set_userdata(utp_ctxt, ptr as *mut c_void);

            // Bind the callbacks
            utp_set_callback(
                utp_ctxt,
                UTP_CALLBACK::SENDTO as c_int,
                Some(UtpContextInner::callback_wrapper),
            );
            utp_set_callback(
                utp_ctxt,
                UTP_CALLBACK::ON_READ as c_int,
                Some(UtpContextInner::callback_wrapper),
            );
            utp_set_callback(
                utp_ctxt,
                UTP_CALLBACK::ON_ERROR as c_int,
                Some(UtpContextInner::callback_wrapper),
            );
            utp_set_callback(
                utp_ctxt,
                UTP_CALLBACK::ON_STATE_CHANGE as c_int,
                Some(UtpContextInner::callback_wrapper),
            );

            // Start an async task to send UDP packets
            let ctxt = UtpContext { inner: inner };
            let weak = Arc::downgrade(&ctxt.inner);
            tokio::spawn(async move {
                tokio::join!(UtpContext::utp_sendto_loop(udp, rx), UtpContext::utp_timer_loop(weak));
            });
            return ctxt;
        };
    }

    pub fn process_udp(&self, data: &[u8], ip: &SocketAddr) -> bool {
        let ctxt = self.inner.utp_ctxt.lock().unwrap();
        unsafe {
            let os_addr = rs_addr_to_c(ip);
            let (addr, len) = os_addr.to_ptr_len();
            return utp_process_udp(ctxt.get(), data.as_ptr(), data.len() as size_t, addr, len)
                == 1;
        };
    }
}

impl UtpContextInner {
    unsafe extern "C" fn callback_wrapper(args_ptr: *mut utp_callback_arguments) -> u64 {
        unsafe {
            let args = &*args_ptr;
            let this = utp_context_get_userdata(args.context) as *mut UtpContextInner;

            debug_assert!(!this.is_null()); // Impossible to happen, we always set the userdata
            return (*this).callback(&args);
        }
    }

    /// Get the socket reference from the arguments
    fn get_socket(args: &utp_callback_arguments) -> Option<&UtpSocketInner> {
        if args.socket.is_null() {
            return None;
        }
        let socket = unsafe { utp_get_userdata(args.socket) } as *mut UtpSocketInner;
        if socket.is_null() {
            // Maybe close already
            return None;
        }
        return Some(unsafe { &*socket });
    }

    fn callback(&mut self, args: &utp_callback_arguments) -> u64 {
        let cb: UTP_CALLBACK = unsafe { std::mem::transmute(args.callback_type) }; // The UTP_CALLBACK directly taken from the C headers, so it is safe to transmute
        match cb {
            // Context...
            UTP_CALLBACK::SENDTO => {
                let data = unsafe { std::slice::from_raw_parts(args.buf, args.len) };
                let addr = c_addr_to_rs(unsafe { args.data1.address });
                return self.on_sendto(data, addr);
            }

            // Socket...
            UTP_CALLBACK::ON_STATE_CHANGE => {
                let state = unsafe { std::mem::transmute(args.data1.state) }; // As same as above
                debug!("UtpSocket {:?} state changed to {:?}", args.socket, state);
                if let Some(sock) = UtpContextInner::get_socket(args) {
                    sock.on_state_change(state);
                }
                return 0;
            }
            UTP_CALLBACK::ON_ERROR => {
                let error = unsafe { std::mem::transmute(args.data1.error_code) }; // As same as above
                debug!("UtpSocket {:?} error {:?}", args.socket, error);
                if let Some(sock) = UtpContextInner::get_socket(args) {
                    sock.on_error(error);
                }
                return 0;
            }
            UTP_CALLBACK::ON_READ => {
                let data = unsafe { std::slice::from_raw_parts(args.buf, args.len) };
                if let Some(sock) = UtpContextInner::get_socket(args) {
                    sock.on_read(data);
                }
                else { // Probably in the accept
                    warn!("UtpSocket {:?} read but no socket bind, data maybe lost", args.socket);
                }
                return 0;
            }

            // Listener...
            UTP_CALLBACK::ON_ACCEPT => {
                // FIXME: Race condition, the data may be lost if it arrive before the accept
                let sock = args.socket;
                if let Some(send) = self.sock_send.lock().unwrap().as_ref() {
                    if send.try_send(utp_socket_t(sock)).is_ok() {
                        return 0;
                    }
                }
                // The queue is full, we need to close the socket
                unsafe { utp_close(sock) };
                return 0;
            }
            _ => {
                return 0;
            }
        }
    }

    fn on_sendto(&mut self, data: &[u8], addr: SocketAddr) -> u64 {
        // We need to send the data
        match self.udp_send.try_send((data.to_vec(), addr)) {
            Ok(_) => {}
            Err(err) => {
                error!("Failed to send UDP packet: {:?}, maybe queue is full", err);
            }
        }
        return 0;
    }
}

impl UtpSocket {
    /// Create an new UtpSocket from the context
    fn new(ctxt: &UtpContext) -> UtpSocket {
        let utp_ctxt = ctxt.inner.utp_ctxt.lock().unwrap();
        unsafe {
            let utp_socket = utp_create_socket(utp_ctxt.get());
            assert!(!utp_socket.is_null());

            return UtpSocket::from(ctxt.inner.clone(), utp_socket_t(utp_socket));
        }
    }

    /// Build an UtpSocket from the raw pointer
    fn from(ctxt: Arc<UtpContextInner>, utp_socket: utp_socket_t) -> UtpSocket {
        let inner = unsafe {
            // We need bind it
            let inner = Box::new(UtpSocketInner {
                utp_socket: utp_socket,
                state: Mutex::new(UtpSocketState {
                    eof: false,
                    connected: false,
                    read_buf: VecDeque::new(),
                    ..Default::default()
                }),
            });
            let ptr = inner.as_ref() as *const UtpSocketInner;
            utp_set_userdata(utp_socket.get(), ptr as *mut c_void);

            inner
        };
        return UtpSocket {
            ctxt: ctxt,
            inner: inner,
        };
    }

    /// Get the context of the socket
    pub fn context(&self) -> UtpContext {
        return UtpContext {
            inner: self.ctxt.clone(),
        };
    }

    /// Get the peer address of the socket
    pub fn peer_addr(&self) -> Option<SocketAddr> {
        unsafe {
            let _lock = self.ctxt.utp_ctxt.lock().unwrap(); // Maybe not needed, but just in case
            let mut storage: sockaddr_storage = mem::zeroed();
            let mut addrlen = mem::size_of::<sockaddr_storage>() as c_int;
            let addr = (&mut storage as *mut sockaddr_storage) as *mut libc::sockaddr;

            if utp_getpeername(self.inner.utp_socket.get(), addr, &mut addrlen as *mut i32) < 0 {
                return None;
            }
            return Some(c_addr_to_rs(addr));
        }
    }

    /// Create an socket and connect to the remote address
    pub async fn connect(ctxt: &UtpContext, addr: SocketAddr) -> io::Result<UtpSocket> {
        let sock = UtpSocket::new(ctxt);
        unsafe {
            // info!("UtpSocket {:?} connecting to {}", sock.inner.utp_socket, addr);
            let os_addr = rs_addr_to_c(&addr);
            let (addr, len) = os_addr.to_ptr_len();
            let ret = utp_connect(sock.inner.utp_socket.get(), addr, len);
            if ret < 0 {
                // See source code. i this <0 almost never happen
                return Err(io::Error::new(io::ErrorKind::Other, "Failed to connect"));
            }
        }
        // Wait it done...
        return UtpConnectFuture { sock: Some(sock) }.await;
    }
}

impl UtpSocketInner {
    fn on_state_change(&self, state: UTP_STATE) {
        let mut this = self.state.lock().unwrap();
        // info!("UtpSocket {:?} state changed to {:?}", self.utp_socket, state);
        match state {
            UTP_STATE::WRITABLE | UTP_STATE::CONNECT => {
                // On connect or writeable, we can write
                if state == UTP_STATE::CONNECT {
                    this.connected = true;
                }
                if let Some(waker) = mem::take(&mut this.write_waker) {
                    waker.wake();
                }
            }
            UTP_STATE::EOF => {
                this.eof = true;

                // Wake up the read and write waker, because EOF happened
                if let Some(waker) = mem::take(&mut this.read_waker) {
                    waker.wake();
                }
                if let Some(waker) = mem::take(&mut this.write_waker) {
                    waker.wake();
                }
            }
            _ => {}
        }
    }

    fn on_error(&self, error: UTP_ERROR) {
        // info!("UtpSocket {:?} into error, code: {:?}", self.utp_socket, error);
        let mut this = self.state.lock().unwrap();
        this.error_code = Some(error);

        // Wake up the read and write waker, because error happened
        if let Some(waker) = mem::take(&mut this.read_waker) {
            waker.wake();
        }
        if let Some(waker) = mem::take(&mut this.write_waker) {
            waker.wake();
        }
    }

    fn on_read(&self, data: &[u8]) {
        // info!("UtpSocket {:?} into readable, {} bytes can read", self.utp_socket, data.len());
        let mut this = self.state.lock().unwrap();
        this.read_buf.extend(data);

        if let Some(waker) = mem::take(&mut this.read_waker) {
            waker.wake();
        }
    }
}

// Impl the connecting...
impl Future for UtpConnectFuture {
    type Output = io::Result<UtpSocket>;

    fn poll(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Self::Output> {
        // The self.sock should never be none
        let mut this = self.sock.as_ref().unwrap().inner.state.lock().unwrap();
        if let Some(error) = &this.error_code {
            let err = error.to_io_error();
            return Poll::Ready(Err(err));
        }
        if this.connected {
            drop(this);
            let sock = self.sock.take().unwrap();
            return Poll::Ready(Ok(sock));
        }
        this.write_waker = Some(cx.waker().clone());

        return Poll::Pending;
    }
}

// Impl the reading...
impl AsyncRead for UtpSocket {
    fn poll_read(
        self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        buf: &mut ReadBuf<'_>,
    ) -> Poll<io::Result<()> > {
        let mut this = self.inner.state.lock().unwrap();
        debug_assert!(this.connected); // We should be connected

        if let Some(error) = &this.error_code {
            let err = error.to_io_error();
            return Poll::Ready(Err(err));
        }
        if !this.read_buf.is_empty() {
            let (first, second) = this.read_buf.as_slices();
            
            // Put the slice 1
            let len1 = std::cmp::min(buf.remaining(), first.len()); // Calculate the length
            buf.put_slice(&first[..len1]);
            
            // Put the slice 2
            let len2 = std::cmp::min(buf.remaining(), second.len()); // Calculate the length
            buf.put_slice(&second[..len2]);

            let total = len1 + len2;
            this.read_buf.drain(..total);

            // info!("UtpSocket {:?} read {} bytes {} bytes left", self.inner.utp_socket, total, this.read_buf.len());
            return Poll::Ready(Ok(()));
        }
        if this.eof {
            return Poll::Ready(Ok(()));
        }

        // Save until we have data
        // info!("UtpSocket {:?} now is not readable, waiting for data", self.inner.utp_socket);
        this.read_waker = Some(cx.waker().clone());
        return Poll::Pending;
    }
}

impl AsyncWrite for UtpSocket {
    fn poll_write(
        self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        buf: &[u8],
    ) -> Poll<io::Result<usize> > {
        // Lock the context first, because we may need use it to write data, and lock it first can avoid the dead lock
        let _lock = self.ctxt.utp_ctxt.lock().unwrap();
        if buf.len() == 0 {
            return Poll::Ready(Ok(0));
        }
        // First check our state
        let mut this = self.inner.state.lock().unwrap();
        debug_assert!(this.connected); // We should be connected

        if let Some(error) = &this.error_code {
            let err = error.to_io_error();
            return Poll::Ready(Err(err));
        }
        if this.eof {
            return Poll::Ready(Ok(0));
        }

        // Try to write some data
        let size = unsafe { utp_write(self.inner.utp_socket.get(), buf.as_ptr(), buf.len()) };
        if size < 0 {
            // Seems impossible
            let err = io::Error::new(io::ErrorKind::Other, "utp_write return -1");
            return Poll::Ready(Err(err));
        }
        if size != 0 {
            // Has some bytes write out
            return Poll::Ready(Ok(size as usize));
        }

        // No longer writable we need to suspend self
        // info!("UtpSocket {:?} now is not writable, waiting for writable", self.inner.utp_socket);
        this.write_waker = Some(cx.waker().clone());
        return Poll::Pending;
    }

    // UTP flush is no-op
    fn poll_flush(self: Pin<&mut Self>, _cx: &mut Context<'_>) -> Poll<io::Result<()>> {
        return Poll::Ready(Ok(()));
    }

    fn poll_shutdown(self: Pin<&mut Self>, _cx: &mut Context<'_>) -> Poll<io::Result<()>> {
        return Poll::Ready(Ok(()));
    }
}

impl UtpListener {
    /// Create an new listener, it only can be created once
    pub fn new(ctxt: &UtpContext, max_cons: usize) -> UtpListener {
        let (sx, rx) = mpsc::channel(max_cons); // We need a channel to send the socket
        let utp_ctxt = ctxt.inner.utp_ctxt.lock().unwrap();
        unsafe {
            // Bind the socket
            utp_set_callback(
                utp_ctxt.get(),
                UTP_CALLBACK::ON_ACCEPT as c_int,
                Some(UtpContextInner::callback_wrapper),
            );

            // Swap the sender
            let prev = ctxt.inner.sock_send.lock().unwrap().replace(sx);
            assert!(prev.is_none(), "UtpListener only can be created once"); // Should be none

            return UtpListener { ctxt: ctxt.inner.clone(), receiver: rx };
        }
    }

    /// Accept an new connection
    pub async fn accept(&mut self) -> io::Result<(UtpSocket, SocketAddr)> {
        let sock = match self.receiver.recv().await {
            Some(sock) => sock,
            None => {
                return Err(io::Error::new(
                    io::ErrorKind::Other,
                    "UtpListener is closed",
                ))
            }
        };
        let sock = UtpSocket::from(self.ctxt.clone(), sock);
        let addr = match sock.peer_addr() {
            Some(addr) => addr,
            None => {
                return Err(io::Error::new(
                    io::ErrorKind::Other,
                    "Can not get addr from sock",
                ))
            }
        };
        // Set the connected flag
        sock.inner.state.lock().unwrap().connected = true;
        return Ok((sock, addr));
    }
}

impl fmt::Debug for UtpSocket {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self.peer_addr() {
            Some(addr) => return write!(f, "UtpSocket({:?} on {})", self.inner.utp_socket.get(), addr),
            None => return write!(f, "UtpSocket({:?})", self.inner.utp_socket.get()),
        }
    }
}

impl Drop for UtpSocket {
    fn drop(&mut self) {
        unsafe {
            // info!("{:?} is dropped", self);
            let _lock = self.ctxt.utp_ctxt.lock().unwrap(); // We need to lock the context to close the socket
            utp_set_userdata(self.inner.utp_socket.get(), std::ptr::null_mut()); // Clear the inner we bind
            utp_close(self.inner.utp_socket.get());
        }
    }
}

impl Drop for UtpListener {
    fn drop(&mut self) {
        unsafe {
            let utp_ctxt = self.ctxt.utp_ctxt.lock().unwrap();
            let _ = self.ctxt.sock_send.lock().unwrap().take(); // Clear the sender

            // Close all socket in channel
            loop {
                match self.receiver.try_recv() {
                    Ok(sock) => utp_close(sock.get()),
                    Err(_) => break,
                }
            }

            // Unbind it
            utp_set_callback(utp_ctxt.get(), UTP_CALLBACK::ON_ACCEPT as c_int, None);
        }
    }
}

impl Drop for UtpContextInner {
    fn drop(&mut self) {
        unsafe {
            utp_destroy(self.utp_ctxt.lock().unwrap().get());
        }
    }
}

// For the UTP_ERROR
impl UTP_ERROR {
    fn to_io_error(&self) -> io::Error {
        return match self {
            UTP_ERROR::ECONNREFUSED => {
                io::Error::new(io::ErrorKind::ConnectionRefused, "Connection refused")
            }
            UTP_ERROR::ECONNRESET => {
                io::Error::new(io::ErrorKind::ConnectionReset, "Connection reset")
            }
            UTP_ERROR::ETIMEDOUT => io::Error::new(io::ErrorKind::TimedOut, "Timed out"),
        };
    }
}

// SAFETY: `utp_context_t` is a raw pointer to a `utp_context`. The `utp_context`
// itself is not thread-safe. However, we are wrapping it in a `Mutex` in `UtpContextInner`.
// This ensures that all access to the underlying `utp_context` is synchronized.
// Therefore, it is safe to send the `utp_context_t` wrapper across threads,
// as long as the synchronization contract is upheld.
impl utp_context_t {
    fn get(&self) -> *mut utp_context {
        return self.0;
    }
}

impl utp_socket_t {
    fn get(&self) -> *mut utp_socket {
        return self.0;
    }
}

unsafe impl Send for utp_context_t {}
unsafe impl Send for utp_socket_t {}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io;

    #[tokio::test]
    async fn test() -> io::Result<()> {
        let udp = UdpSocket::bind("127.0.0.1:0").await?;
        let _ctxt = UtpContext::new(Arc::new(udp));

        return Ok(());
    }
}
