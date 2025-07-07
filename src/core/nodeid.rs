use std::array::TryFromSliceError;
use std::ops::{BitXor, Index, IndexMut};
use std::fmt;

use serde::{Deserialize, Serialize};

/// The NodeId of the DHT, 160 bits
/// 
#[derive(PartialOrd, Ord, PartialEq, Eq, Clone, Copy, Hash)]
pub struct NodeId {
    pub data: [u8; 20],
}

impl BitXor for NodeId {
    type Output = NodeId;

    fn bitxor(self, rhs: Self) -> Self::Output {
        let mut res = [0u8; 20];
        for (idx, elem) in res.iter_mut().enumerate() {
            *elem = self.data[idx] ^ rhs.data[idx];
        }
        return NodeId::new(res);
    }
}

impl Index<usize> for NodeId {
    type Output = u8;

    fn index(&self, index: usize) -> &Self::Output {
        return &self.data[index];
    }
}

impl IndexMut<usize> for NodeId {    
    fn index_mut(&mut self, index: usize) -> &mut u8 {
        return &mut self.data[index];
    }
}

impl NodeId {

    /// Create an NodeId with all zero
    pub fn zero() -> NodeId {
        return NodeId::new([0u8; 20]); 
    }

    /// Create an NodeId with given array
    pub fn new(arr: [u8; 20]) -> NodeId {
        return NodeId { 
            data: arr 
        };
    }

    /// Randomly generate a NodeId
    pub fn rand() -> NodeId {
        let mut arr = [0u8; 20];
        for i in arr.iter_mut() {
            *i = fastrand::u8(..);
        }
        return NodeId::new(arr);
    }

    /// Generates a NodeId that shares the first prefix_len bits with id, differs at bit prefix_len (if prefix_len < 160), and has random bits thereafter
    pub fn rand_with_prefix(id: NodeId, prefix_len: usize) -> NodeId {
        assert!(prefix_len <= 160, "Prefix must be less than or equal to 160 bits.");
        let mut out = NodeId::rand();
        for i in 0..prefix_len {
            out.bit_set(i, id.bit_test(i)); // Cppy the bits from the given id
        }
        // Flip the next bit to ensure the prefix are same
        if prefix_len < 160 {
            out.bit_set(prefix_len, !id.bit_test(prefix_len)); 
        }
        return out;
    }

    pub fn from_hex(hex: &str) -> Option<NodeId> {
        if hex.len() != 40 {
            return None;
        }
        let mut arr = [0u8; 20];
        for i in 0..20 {
            let byte_str = &hex[i * 2..i * 2 + 2];
            match u8::from_str_radix(byte_str, 16) {
                Ok(byte) => arr[i] = byte,
                Err(_) => return None,
            }
        }
        return Some(NodeId::new(arr));
    }

    /// Cout How may leading zeros in this NodeId, if all zeros, return 160
    pub fn leading_zeros(&self) -> u32 {
        for (idx, value) in self.data.iter().enumerate() {
            if *value != 0 {
                return (idx * 8) as u32 + value.leading_zeros();
            }
        }
        return 160; // All zeros
    }

    /// Check if this NodeId is all zero
    pub fn is_zero(&self) -> bool {
        return *self == NodeId::zero();
    }

    /// Make a hex string of the id
    pub fn hex(&self) -> String {
        let mut res = String::new();
        for i in self.data.iter() {
            res.push_str(&format!("{:02x}", i));
        }
        return res;
    }

    /// Make a binary string of the id
    pub fn bin(&self) -> String {
        let mut res = String::new();
        for i in self.data.iter() {
            res.push_str(&format!("{:08b}", i));
        }
        return res;
    }

    /// Check the bit at the given index
    pub fn bit_test(&self, bit: usize) -> bool {
        if bit >= 160 {
            panic!("Bit index out of range: {}. NodeId is 160 bits long.", bit);
        }
        let byte_index = bit / 8;
        let bit_index = bit % 8;
        return (self.data[byte_index] & (1 << (7 - bit_index))) != 0;
    }

    /// Flip the bit at the given index
    pub fn bit_flip(&mut self, bit: usize) {
        if bit >= 160 {
            panic!("Bit index out of range: {}. NodeId is 160 bits long.", bit);
        }
        let byte_index = bit / 8;
        let bit_index = bit % 8;
        self.data[byte_index] ^= 1 << (7 - bit_index);
    }

    pub fn bit_set(&mut self, bit: usize, value: bool) {
        if bit >= 160 {
            panic!("Bit index out of range: {}. NodeId is 160 bits long.", bit);
        }
        let byte_index = bit / 8;
        let bit_index = bit % 8;
        if value {
            self.data[byte_index] |= 1 << (7 - bit_index);
        }
        else {
            self.data[byte_index] &= !(1 << (7 - bit_index));
        }
    }

    /// Get the slice of the id's data
    pub fn as_slice(&self) -> &[u8] {
        return self.data.as_slice();
    }

    /// Get the mut slice of the id's data
    pub fn as_mut_slice(&mut self) -> &mut [u8] {
        return self.data.as_mut_slice();
    }

    // Let self cast into vector
    pub fn to_vec(&self) -> Vec<u8> {
        return Vec::from(self.as_slice());
    }
}

impl fmt::Display for NodeId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        return write!(f, "{}", self.hex());
    }
}

impl fmt::Debug for NodeId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        return write!(f, "{}", self.hex());
    }
}

impl From<[u8; 20]> for NodeId {
    fn from(value: [u8; 20]) -> Self {
        return NodeId::new(value);
    }
}

impl TryFrom<&[u8]> for NodeId {
    type Error = TryFromSliceError;
    
    fn try_from(value: &[u8]) -> Result<Self, Self::Error> {
        let arr: [u8; 20] = value.try_into()?;
        return Ok(NodeId::new(arr));
    }
}

impl TryFrom<&Vec<u8> > for NodeId {
    type Error = TryFromSliceError;
    fn try_from(value: &Vec<u8>) -> Result<Self, Self::Error> {
        let id = value.as_slice().try_into()?;
        return Ok(id);
    }
}


impl Serialize for NodeId {
    fn serialize<S: serde::Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        return serializer.serialize_str(&self.hex());
    }
}

impl<'de> Deserialize<'de> for NodeId {
    fn deserialize<D: serde::Deserializer<'de> >(deserializer: D) -> Result<Self, D::Error> {
        let hex_str = String::deserialize(deserializer)?;
        return NodeId::from_hex(&hex_str).ok_or_else(|| serde::de::Error::custom("Invalid NodeId hex string"));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_leading_zeros() {
        assert_eq!(NodeId::zero().leading_zeros(), 160)
    }

    #[test]
    fn test_bitxor() {
        let id1 = NodeId::new([10u8; 20]);
        let id2 = NodeId::new([5u8; 20]); // 00001010 ^ 00000101 = 00001111 (15)
        let expected_xor_data = [10u8 ^ 5u8; 20];
        assert_eq!(id1 ^ id2, NodeId::new(expected_xor_data));

        let zero = NodeId::zero();
        let rand_id = NodeId::rand();
        assert_eq!(rand_id ^ zero, rand_id);
        assert_eq!(rand_id ^ rand_id, zero);
    }

    #[test]
    fn test_hex() {
        let id = NodeId::new([0x12, 0x34, 0x56, 0x78, 0x9a, 0xbc, 0xde, 0xf0,
                              0x12, 0x34, 0x56, 0x78, 0x9a, 0xbc, 0xde, 0xf0,
                              0x12, 0x34, 0x56, 0x78]);
        assert_eq!(id.hex(), "123456789abcdef0123456789abcdef012345678");

        for i in 0..20 {
            let id = NodeId::rand();
            let hex = id.hex();
            let parsed_id = NodeId::from_hex(&hex).unwrap();
            assert_eq!(id, parsed_id, "Failed to parse hex for id {}", i);
        }
    }

    #[test]
    fn test_bit_test() {
        let mut id = NodeId::new([0b00000000; 20]);
        id.bit_set(0, true);
        id.bit_set(1, true);
        id.bit_set(2, true);
        assert!(id.bit_test(0));
        assert!(id.bit_test(1));
        assert!(id.bit_test(2));
        assert!(!id.bit_test(3));
        
        id.bit_flip(0);
        assert!(!id.bit_test(0));
    }

    #[test]
    fn test_rand_with_prefix() {
        for _ in 0..100 {
            let id = NodeId::rand();
            let prefix = fastrand::usize(0..160);
            let new_id = NodeId::rand_with_prefix(id, prefix);
            let xor = (id ^ new_id).leading_zeros();
            assert_eq!(xor, prefix as u32, "Failed for id: {:?}, new id {:?}, prefix: {}", id, new_id, prefix);
        }
    }
}