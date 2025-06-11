#![allow(non_camel_case_types)]
#![allow(dead_code)] // SHUT UP!!!

use libc::{c_int, c_void, size_t, sockaddr, ssize_t};

// Some structs are defined in utp.h
#[repr(C)]
pub struct utp_context {
    _private: [u8; 0],
}

#[repr(C)]
pub struct utp_socket {
    _private: [u8; 0],
}

#[repr(C)]
#[derive(Debug, PartialEq, Eq)]
pub enum UTP_STATE {
	// socket has reveived syn-ack (notification only for outgoing connection completion)
	// this implies writability
	CONNECT = 1,

	// socket is able to send more data
	WRITABLE = 2,

	// connection closed
	EOF = 3,

	// socket is being destroyed, meaning all data has been sent if possible.
	// it is not valid to refer to the socket after this state change occurs
	DESTROYING = 4,
}

#[repr(C)]
#[derive(Debug, PartialEq, Eq)]
// Errors codes that can be passed to UTP_ON_ERROR callback
pub enum UTP_ERROR {
	ECONNREFUSED = 0,
	ECONNRESET,
	ETIMEDOUT,
}

#[repr(C)]
#[derive(Debug, PartialEq, Eq)]
pub enum UTP_CALLBACK {
	// callback names
	ON_FIREWALL = 0,
	ON_ACCEPT,
	ON_CONNECT,
	ON_ERROR,
	ON_READ,
	ON_OVERHEAD_STATISTICS,
	ON_STATE_CHANGE,
	GET_READ_BUFFER_SIZE,
	ON_DELAY_SAMPLE,
	GET_UDP_MTU,
	GET_UDP_OVERHEAD,
	GET_MILLISECONDS,
	GET_MICROSECONDS,
	GET_RANDOM,
	LOG,
	SENDTO,
    
    // context and socket options that may be set/queried
    LOG_NORMAL,
    LOG_MTU,
    LOG_DEBUG,
	SNDBUF,
	RCVBUF,
	TARGET_DELAY,

	ARRAY_SIZE,	// must be last
}

// typedef struct {
// 	utp_context *context;
// 	utp_socket *socket;
// 	size_t len;
// 	uint32 flags;
// 	int callback_type;
// 	const byte *buf;

// 	union {
// 		const struct sockaddr *address;
// 		int send;
// 		int sample_ms;
// 		int error_code;
// 		int state;
// 	};

// 	union {
// 		socklen_t address_len;
// 		int type;
// 	};
// } utp_callback_arguments;
#[repr(C)]
pub union utp_callback_arguments_union1 {
    pub address: *const sockaddr,
    pub send: c_int,
    pub sample_ms: c_int,
    pub error_code: c_int,
    pub state: c_int,
}

#[repr(C)]
pub union utp_callback_arguments_union2 {
    pub address_len: c_int,
    pub type_: c_int,
}

#[repr(C)]
pub struct utp_callback_arguments {
    pub context: *mut utp_context,
    pub socket: *mut utp_socket,
    pub len: size_t,
    pub flags: u32,
    pub callback_type: c_int,
    pub buf: *const u8,

	pub data1: utp_callback_arguments_union1, // :(, FUCK IT
	pub data2: utp_callback_arguments_union2,
}

pub type utp_callback_t = unsafe extern "C" fn(args: *mut utp_callback_arguments) -> u64;

#[link(name = "libutp", kind = "static")]
unsafe extern "C" {
    pub fn utp_init(version: c_int) -> *mut utp_context;
    pub fn utp_destroy(ctx: *mut utp_context);
    pub fn utp_set_callback(ctx: *mut utp_context, name: c_int, callback: Option<utp_callback_t>);
	pub fn utp_context_set_userdata(ctx: *mut utp_context, userdata: *mut c_void) -> *mut c_void;
	pub fn utp_context_get_userdata(ctx: *mut utp_context) -> *mut c_void;
	// int				utp_context_set_option			(utp_context *ctx, int opt, int val);
	// int				utp_context_get_option			(utp_context *ctx, int opt);
	pub fn utp_process_udp(ctx: *mut utp_context, buf: *const u8, len: size_t, to: *const sockaddr, tolen: c_int) -> c_int;
	// int				utp_process_icmp_error			(utp_context *ctx, const byte *buffer, size_t len, const struct sockaddr *to, socklen_t tolen);
	// int				utp_process_icmp_fragmentation	(utp_context *ctx, const byte *buffer, size_t len, const struct sockaddr *to, socklen_t tolen, uint16 next_hop_mtu);
	pub fn utp_check_timeouts(ctx: *mut utp_context);
	pub fn utp_issue_deferred_acks(ctx: *mut utp_context);
	// utp_context_stats* utp_get_context_stats		(utp_context *ctx);
	pub fn utp_create_socket(ctx: *mut utp_context) -> *mut utp_socket;
	pub fn utp_set_userdata(s: *mut utp_socket, userdata: *mut c_void) -> *mut c_void;
	pub fn utp_get_userdata(s: *mut utp_socket) -> *mut c_void;
	// int				utp_setsockopt					(utp_socket *s, int opt, int val);
	// int				utp_getsockopt					(utp_socket *s, int opt);
	pub fn utp_connect(s: *mut utp_socket, to: *const sockaddr, tolen: c_int) -> c_int;
	pub fn utp_write(s: *mut utp_socket, buf: *const u8, count: usize) -> ssize_t; // WTF, why it is not const void*?
	// ssize_t			utp_writev						(utp_socket *s, struct utp_iovec *iovec, size_t num_iovecs);
	pub fn utp_getpeername(s: *mut utp_socket, addr: *mut sockaddr, addrlen: *mut c_int) -> c_int;
	// void			utp_read_drained				(utp_socket *s);
	// int				utp_get_delays					(utp_socket *s, uint32 *ours, uint32 *theirs, uint32 *age);
	// utp_socket_stats* utp_get_stats					(utp_socket *s);
	// utp_context*	utp_get_context					(utp_socket *s);
	// void			utp_shutdown					(utp_socket *s, int how);
	pub fn utp_close(s: *mut utp_socket);
}