pub mod bencode;
pub mod crawler;
pub mod core;
pub mod krpc;
pub mod dht;
pub mod utp;
pub mod bt;
pub use core::NodeId;
pub use core::InfoHash;

pub fn hello_world() {
    println!("Hello world");
}

#[cfg(test)]
mod tests {
    use ctor::ctor;
    #[ctor]
    fn init() {
        color_backtrace::install();
        let _ = tracing_subscriber::fmt()
            .with_max_level(tracing::Level::TRACE)
            .with_thread_ids(true)
            .try_init();
    }
}