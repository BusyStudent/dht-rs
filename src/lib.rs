pub mod bencode;
pub mod core;
pub mod krpc;
pub use core::NodeId;
pub use core::InfoHash;

pub fn hello_world() {
    println!("Hello world");
}