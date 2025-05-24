use std::ops::{BitXor, Index, IndexMut};

/// The NodeId of the DHT, 160 bits
/// 
#[derive(PartialOrd, Ord, PartialEq, Eq, Debug, Clone, Copy)]
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

    pub fn rand() -> NodeId {
        let mut arr = [0u8; 20];
        for i in arr.iter_mut() {
            *i = fastrand::u8(..);
        }
        return NodeId::new(arr);
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

impl From<[u8; 20]> for NodeId {
    fn from(value: [u8; 20]) -> Self {
        return NodeId::new(value);
    }
}

impl TryFrom<&[u8]> for NodeId {
    type Error = ();
    
    fn try_from(value: &[u8]) -> Result<Self, Self::Error> {
        if value.len() != 20 {
            return Err(());
        }
        let mut arr = [0u8; 20];
        arr.copy_from_slice(value);
        return Ok(NodeId::new(arr));
    }
}

impl TryFrom<&Vec<u8> > for NodeId {
    type Error = ();
    fn try_from(value: &Vec<u8>) -> Result<Self, Self::Error> {
        let id = value.as_slice().try_into()?;
        return Ok(id);
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
}