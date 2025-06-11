// SHUT UP!!!
#![allow(dead_code)]
#![allow(non_camel_case_types)]

use super::ffi::*;
use super::addr::*;
use std::future::Future;
use std::{
    collections::VecDeque, mem, net::{SocketAddr}, pin::Pin, sync::{Arc, Mutex}, task::{Waker, Context, Poll}, time::Duration, io
};
use tokio::io::AsyncWrite;
use tokio::{
    io::{AsyncRead, ReadBuf}, net::UdpSocket, sync::mpsc
};
use libc::{c_int, c_void, size_t};

use tracing::{debug, error};

#[derive(Debug, Clone, Copy)]
struct utp_context_t(*mut utp_context); // Wrapper for Impl Send, FUCK IT

// The context is the main object that is used to create sockets
struct UtpContextInner {
    utp_ctxt: Mutex<utp_context_t>, // The UTP context is not thread safe, so we need to lock it
    udp_send: mpsc::Sender<(Vec<u8>, SocketAddr)>,
}

#[derive(Default)]
struct UtpSocketState {
    eof: bool, // Did the stream eof?
    connected: bool, // Did we connect?
    read_buf: VecDeque<u8>,
    read_waker: Option<Waker>,
    write_waker: Option<Waker>,
    error_code: Option<UTP_ERROR>, // The error code from the libutp
}
struct UtpSocketInner {
    utp_socket: *mut utp_socket, // The managed utp socket ptr
    state: Mutex<UtpSocketState>,
}

struct UtpConnectFuture {
    sock: Option<UtpSocket>,
}

#[derive(Clone)]
pub struct UtpContext {
    inner: Arc<UtpContextInner>,
}
pub struct UtpSocket {
    ctxt: Arc<UtpContextInner>,
    inner: Box<UtpSocketInner>,
}

impl UtpContext {
    async fn utp_sendto_loop(udp: Arc<UdpSocket>, mut rx: mpsc::Receiver<(Vec<u8>, SocketAddr)>) {
        while let Some((data, addr)) = rx.recv().await {
            let _ = udp.send_to(&data, &addr).await;
        }
    }

    async fn utp_timer_loop(self) {
        loop {
            tokio::time::sleep(Duration::from_millis(500)).await;
            let ctxt = self.inner.utp_ctxt.lock().unwrap();
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
            });
            let ptr = inner.as_ref() as *const UtpContextInner;
            utp_context_set_userdata(utp_ctxt, ptr as *mut c_void);

            // Bind the callbacks
            utp_set_callback(utp_ctxt, UTP_CALLBACK::SENDTO as c_int, Some(UtpContextInner::callback_wrapper));
            utp_set_callback(utp_ctxt, UTP_CALLBACK::ON_READ as c_int, Some(UtpContextInner::callback_wrapper));
            utp_set_callback(utp_ctxt, UTP_CALLBACK::ON_ERROR as c_int, Some(UtpContextInner::callback_wrapper));
            utp_set_callback(utp_ctxt, UTP_CALLBACK::ON_STATE_CHANGE as c_int, Some(UtpContextInner::callback_wrapper));

            // Start an async task to send UDP packets
            let ctxt = UtpContext { inner: inner };
            let ctxt2: UtpContext = ctxt.clone();
            tokio::spawn(async move {
                tokio::join!(UtpContext::utp_sendto_loop(udp, rx), ctxt2.utp_timer_loop());
            });
            return ctxt;
        };
    }

    pub fn process_udp(&self, data: &[u8], ip: &SocketAddr) -> bool {
        let ctxt = self.inner.utp_ctxt.lock().unwrap();
        unsafe { 
            let os_addr = rs_addr_to_c(ip);
            let (addr, len) = os_addr.to_ptr_len();
            return utp_process_udp(ctxt.get(), data.as_ptr(), data.len() as size_t, addr, len) == 1;
        };
    }
}

impl UtpContextInner {
    unsafe extern "C" fn callback_wrapper(args_ptr: *mut utp_callback_arguments) -> u64 {
        let args = &*args_ptr;
        let this = utp_context_get_userdata(args.context) as *mut UtpContextInner;

        debug_assert!(!this.is_null()); // Impossible to happen, we always set the userdata
        return (*this).callback(&args);
    }

    /// Get the socket reference from the arguments
    unsafe fn get_socket(args: &utp_callback_arguments) -> Option<&UtpSocketInner> {
        if args.socket.is_null() {
            return None;
        }
        let socket = utp_get_userdata(args.socket) as *mut UtpSocketInner;
        if socket.is_null() { // Maybe close already
            return None;
        }
        return Some(&*socket);
    }

    unsafe fn callback(&mut self, args: &utp_callback_arguments) -> u64 {
        let cb: UTP_CALLBACK = std::mem::transmute(args.callback_type); // The UTP_CALLBACK directly taken from the C headers, so it is safe to transmute
        match cb {
            // Context...
            UTP_CALLBACK::SENDTO => {
                let data = std::slice::from_raw_parts(args.buf, args.len);
                let addr = c_addr_to_rs(args.data1.address);
                return self.on_sendto(data, addr);
            },

            // Socket...
            UTP_CALLBACK::ON_STATE_CHANGE => {
                let state = std::mem::transmute(args.data1.state); // As same as above
                debug!("UtpSocket {:?} state changed to {:?}", args.socket, state);
                if let Some(sock) = UtpContextInner::get_socket(args) {
                    sock.on_state_chage(state);
                }
                return 0;
            },
            UTP_CALLBACK::ON_ERROR => {
                let error = std::mem::transmute(args.data1.error_code); // As same as above
                debug!("UtpSocket {:?} error {:?}", args.socket, error);
                if let Some(sock) = UtpContextInner::get_socket(args) {
                    sock.on_error(error);
                }
                return 0;
            },
            UTP_CALLBACK::ON_READ => {
                let data = std::slice::from_raw_parts(args.buf, args.len);
                if let Some(sock) = UtpContextInner::get_socket(args) {
                    sock.on_read(data);
                }
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
            Ok(_) => {},
            Err(err) => {
                error!("Failed to send UDP packet: {:?}, maybe queue is full", err);
            }
        }
        return 0;
    }
}

impl UtpSocket {
    fn new(ctxt: &UtpContext) -> UtpSocket {
        let utp_ctxt = ctxt.inner.utp_ctxt.lock().unwrap();
        let inner = unsafe {
            let utp_socket = utp_create_socket(utp_ctxt.get());
            assert!(!utp_socket.is_null());

            // We need bind it
            let inner = Box::new(UtpSocketInner {
                utp_socket: utp_socket,
                state: Mutex::new(UtpSocketState {
                    eof: false,
                    connected: false,
                    read_buf: VecDeque::new(),
                    ..Default::default()
                })
            });
            let ptr = inner.as_ref() as *const UtpSocketInner;
            utp_set_userdata(utp_socket, ptr as *mut c_void);

            inner
        };
        return UtpSocket {
            ctxt: ctxt.inner.clone(),
            inner: inner,
        };
    }

    /// Get the context of the socket
    pub fn context(&self) -> UtpContext {
        return UtpContext {
            inner: self.ctxt.clone(),
        };
    }

    /// Create an socket and connect to the remote address
    pub async fn connect(ctxt: &UtpContext, addr: &SocketAddr) -> io::Result<UtpSocket> {
        let sock = UtpSocket::new(ctxt);
        unsafe {
            let os_addr = rs_addr_to_c(addr);
            let (addr, len) = os_addr.to_ptr_len();
            let ret = utp_connect(sock.inner.utp_socket, addr, len);
            if ret < 0 { // See source code. i this <0 almost never happen
                return Err(io::Error::new(io::ErrorKind::Other, "Failed to connect"));
            }
        }
        // Wait it done...
        return UtpConnectFuture { sock: Some(sock) }.await;
    }
}

impl UtpSocketInner {
    fn on_state_chage(&self, state: UTP_STATE) {
        let mut this = self.state.lock().unwrap();
        match state {
            UTP_STATE::WRITABLE | UTP_STATE::CONNECT => { // On connect or writeable, we can write
                if state == UTP_STATE::CONNECT {
                    this.connected = true;
                }
                if let Some(waker) = mem::take(&mut this.write_waker) {
                    waker.wake();
                }
            },
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
            _ => {

            },
        }
    }

    fn on_error(&self, error: UTP_ERROR) {
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
        ) -> Poll<io::Result<()> > 
    {
        let mut this = self.inner.state.lock().unwrap();
        debug_assert!(this.connected); // We should be connected

        if let Some(error) = &this.error_code {
            let err = error.to_io_error();
            return Poll::Ready(Err(err));
        }
        if !this.read_buf.is_empty() {
            let (first, _second) = this.read_buf.as_slices();
            let len = std::cmp::min(buf.remaining(), first.len()); // Calculate the length
            
            buf.put_slice(&first[..len]);
            this.read_buf.drain(..len);
            
            return Poll::Ready(Ok(()));
        }
        if this.eof {
            return Poll::Ready(Ok(()));
        }

        // Save until we have data
        this.read_waker = Some(cx.waker().clone());
        return Poll::Pending;
    }
}

impl AsyncWrite for UtpSocket {
    fn poll_write(
        self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        buf: &[u8],
        ) -> Poll<io::Result<usize> > 
    {
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
        let size = unsafe {
            utp_write(self.inner.utp_socket, buf.as_ptr(), buf.len())
        };
        if size < 0 { // Seems impossible
            let err = io::Error::new(io::ErrorKind::Other, "utp_write return -1");
            return Poll::Ready(Err(err));
        }
        if size != 0 { // Has some bytes write out
            return Poll::Ready(Ok(size as usize));
        }

        // No longer writable we need to suspend self
        this.write_waker = Some(cx.waker().clone());
        return Poll::Pending;
    }
    
    // UTP flush is no-op
    fn poll_flush(self: Pin<&mut Self>, _cx: &mut Context<'_>) -> Poll<io::Result<()> > {
        return Poll::Ready(Ok(()));
    }

    fn poll_shutdown(self: Pin<&mut Self>, _cx: &mut Context<'_>) -> Poll<io::Result<()> > {
        return Poll::Ready(Ok(()));
    }
}

impl Drop for UtpSocket {
    fn drop(&mut self) {
        unsafe {
            let _lock = self.ctxt.utp_ctxt.lock().unwrap(); // We need to lock the context to close the socket
            utp_set_userdata(self.inner.utp_socket, std::ptr::null_mut()); // Clear the inner we bind
            utp_close(self.inner.utp_socket);
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
            UTP_ERROR::ECONNREFUSED => io::Error::new(io::ErrorKind::ConnectionRefused, "Connection refused"),
            UTP_ERROR::ECONNRESET => io::Error::new(io::ErrorKind::ConnectionReset, "Connection reset"),
            UTP_ERROR::ETIMEDOUT => io::Error::new(io::ErrorKind::TimedOut, "Timed out"),
        }
    }
}

// :( Fuck the compiler, we have to impl Send to let it SHUT UP!!!
impl utp_context_t {
    fn get(&self) -> *mut utp_context {
        return self.0;
    }
}

unsafe impl Send for utp_context_t {

}

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