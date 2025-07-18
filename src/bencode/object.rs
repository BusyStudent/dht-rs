use std::{collections::BTreeMap, fmt};

#[derive(PartialEq, Eq, Clone)]
pub enum Object {
    Int(i64),
    String(Vec<u8>),
    List(Vec<Object>),
    Dict(BTreeMap<Vec<u8>, Object>),
}

fn parse_int(bytes: &[u8]) -> Option<(i64, &[u8])> {
    let mut end = &bytes[..];
    while !end.is_empty() {
        let ch = end.first()?;
        if *ch != b'-' && !ch.is_ascii_digit() {
            break;
        }
        end = &end[1..]; // Advance 1
    }
    let offset = end.as_ptr() as usize - bytes.as_ptr() as usize;
    let numstr = match std::str::from_utf8(&bytes[..offset]) {
        Ok(val) => val,
        Err(_) => return None,
    };
    return match numstr.parse::<i64>() {
        Ok(num) => Some((num, end)),
        Err(_) => None
    }
}

impl Object {
    // Cast
    pub fn as_int(&self) -> Option<&i64> {
        return match self {
            Object::Int(i) => Some(i),
            _ => None
        }
    }

    pub fn as_string(&self) -> Option<&Vec<u8> > {
        return match self {
            Object::String(s) => Some(s),
            _ => None
        }
    }

    pub fn as_list(&self) -> Option<&Vec<Object> > {
        return match self {
            Object::List(list) => Some(list),
            _ => None
        }
    }

    pub fn as_dict(&self) -> Option<&BTreeMap<Vec<u8>, Object> > {
        return match self {
            Object::Dict(dict) => Some(dict),
            _ => None
        }
    }

    // Cast mut
    pub fn as_mut_int(&mut self) -> Option<&mut i64> {
        return match self {
            Object::Int(i) => Some(i),
            _ => None
        }
    }

    pub fn as_mut_string(&mut self) -> Option<&mut Vec<u8> > {
        return match self {
            Object::String(s) => Some(s),
            _ => None
        }
    }

    pub fn as_mut_list(&mut self) -> Option<&mut Vec<Object> > {
        return match self {
            Object::List(list) => Some(list),
            _ => None
        }
    }

    pub fn as_mut_dict(&mut self) -> Option<&mut BTreeMap<Vec<u8>, Object> > {
        return match self {
            Object::Dict(dict) => Some(dict),
            _ => None
        }
    }

    /// Get the dict object value by key, it will check if it is dict
    pub fn get(&self, key: &[u8]) -> Option<&Object> {
        return self.as_dict()?.get(key);
    }

    /// Get the list object value by index, it will check if it is list
    pub fn at(&self, index: usize) -> Option<&Object> {
        return self.as_list()?.get(index);
    }

    /// Check if the dict object contains the key, it will check if it is dict
    pub fn contains_key(&self, key: &[u8]) -> Option<bool> {
        return Some(self.as_dict()?.contains_key(key));
    }

    /// Parse any bencoded input, raw version
    pub fn decode(bytes: &[u8]) -> Option<(Object, &[u8])> {
        return match bytes.first()? {
            b'0'..=b'9' => Object::decode_string(bytes),
            b'i' => Object::decode_int(bytes),
            b'l' => Object::decode_list(bytes),
            b'd' => Object::decode_dict(bytes),
            _ => None,
        }
    }

    /// Parse any bencoded input, return the object version
    pub fn parse(bytes: &[u8]) -> Option<Object> {
        let (obj, _) = Object::decode(bytes)?;
        return Some(obj);
    }

    /// Parse int, input like i11e
    pub fn decode_int(mut bytes: &[u8]) -> Option<(Object, &[u8])> {
        if *bytes.first()? != b'i' {
            return None;
        }
        bytes = &bytes[1..];

        let (num, left) = parse_int(bytes)?;
        bytes = left;
        if *bytes.first()? != b'e' { // It should end this string
            return None
        }
        bytes = &bytes[1..];
        return Some((Object::Int(num), bytes));
    }

    /// Parse string, input like 4:aaaa
    pub fn decode_string(mut bytes: &[u8]) -> Option<(Object, &[u8])> {
        // Get the string length
        let (strlen, left) = parse_int(bytes)?;
        if *left.first()? != b':' || strlen < 0 {
            return None;
        }
        let strlen = strlen as usize;
        bytes = &left[1..];
        if bytes.len() < strlen {
            return None; // Not encough 
        }
        let vec = Vec::from(&bytes[..strlen]);
        let left = &bytes[strlen..];
        return Some((Object::String(vec), left));
    }

    /// Parse list, input like l4:spame
    pub fn decode_list(mut bytes: &[u8]) -> Option<(Object, &[u8])> {
        if *bytes.first()? != b'l' {
            return None;
        }
        bytes = &bytes[1..];
        let mut list = Vec::new();
        while *bytes.first()? != b'e' {
            let (obj, left) = Object::decode(bytes)?;
            list.push(obj);
            bytes = left;
        }
        bytes = &bytes[1..];
        return Some((Object::List(list), bytes));
    }

    /// Parse dict, input like d1:s2:sse
    pub fn decode_dict(mut bytes: &[u8]) -> Option<(Object, &[u8])> {
        if *bytes.first()? != b'd' {
            return None;
        }
        bytes = &bytes[1..];
        let mut dict = BTreeMap::new();
        while *bytes.first()? != b'e' {
            let (string, left) = Object::decode_string(bytes)?;
            let string = match string {
                Object::String(str) => str,
                _ => return None,
            };
            let (obj, left) = Object::decode(left)?;
            bytes = left;
            dict.insert(string, obj);
        }
        bytes = &bytes[1..];
        return Some((Object::Dict(dict), bytes));
    }

    pub fn encode(&self) -> Vec<u8> {
        let mut buf = Vec::new();
        self.encode_to(&mut buf);
        return buf;
    }

    pub fn encode_to(&self, out: &mut Vec<u8>) {
        match self {
            Object::Int(i) => { // i123e
                out.push(b'i');
                out.extend_from_slice(i.to_string().as_bytes());
                out.push(b'e');
            }
            Object::String(s) => { // 4:spam
                out.extend_from_slice(s.len().to_string().as_bytes());
                out.push(b':');
                out.extend_from_slice(s.as_slice());
            }
            Object::List(list) => { // lxxxe
                out.push(b'l');
                for each in list {
                    each.encode_to(out);
                }
                out.push(b'e');
            }
            Object::Dict(dict) => { // dxxxe
                out.push(b'd');
                for (key, value) in dict {
                    // Write the k and than value
                    out.extend_from_slice(key.len().to_string().as_bytes());
                    out.push(b':');
                    out.extend_from_slice(key.as_slice());

                    value.encode_to(out);
                }
                out.push(b'e');
            }
        }
    }
}

impl From<i64> for Object {
    fn from(item: i64) -> Self {
        return Object::Int(item);
    }
}

impl From<&[u8]> for Object {
    fn from(item: &[u8]) -> Self {
        return Object::String(item.to_vec());
    }
}

impl From<&str> for Object {
    fn from(item: &str) -> Self {
        return Object::String(item.as_bytes().to_vec());
    }
}

impl From<Vec<u8> > for Object {
    fn from(item: Vec<u8>) -> Self {
        return Object::String(item);
    }
}

impl From<Vec<Object> > for Object {
    fn from(item: Vec<Object>) -> Self {
        return Object::List(item);
    }
}

impl From<BTreeMap<Vec<u8>, Object> > for Object {
    fn from(item: BTreeMap<Vec<u8>, Object>) -> Self {
        return Object::Dict(item);
    }
}

// To String
impl<const N: usize> From<&[u8; N]> for Object {
    fn from(value: &[u8; N]) -> Self {
        return Object::String(value.to_vec());
    }
}

// To List
impl<const N: usize> From<[Object; N]> for Object {
    fn from(value: [Object; N]) -> Self {
        return Object::List(Vec::from(value));
    }
}

// To Dict
impl<const N: usize> From<[(Vec<u8>, Object); N]> for Object {
    /// Create an dict object from the array of key and value
    fn from(arr: [(Vec<u8>, Object); N]) -> Self {
        return Object::Dict(BTreeMap::from(arr));
    }
}

/// Format like human readable json
impl fmt::Debug for Object {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let write_string = |f: &mut fmt::Formatter<'_>, s: &[u8]| {
            if let Ok(utf8) = std::str::from_utf8(s) { // Valid utf8
                return write!(f, "\"{}\"", utf8);
            }
            // not valid u8, print like \x01\x02\x03
            for each in s.iter() {
                write!(f, "\\x{:02x}", each)?;
            }
            return Ok(());
        };

        return match self {
            Object::Int(i) => {
                write!(f, "{}", i)
            }
            Object::String(s) => {
                write_string(f, &s)
            }
            Object::List(list) => {
                write!(f, "[")?;
                for (i, each) in list.iter().enumerate() {
                    write!(f, "{:?}", each)?;
                    if i != list.len() - 1 {
                        write!(f, ", ")?;
                    }
                }
                write!(f, "]")?;

                Ok(())
            }
            Object::Dict(dict) => {
                write!(f, "{{")?;
                for (i, (key, value)) in dict.iter().enumerate() {
                    write_string(f, &key)?;
                    write!(f, ": ")?;
                    write!(f, "{:?}", value)?;
                    if i != dict.len() - 1 {
                        write!(f, ", ")?;
                    }
                }
                write!(f, "}}")?;

                Ok(())
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn parse_and_cmp(bytes: &[u8], expected: Object) {
        if let Some((obj, left)) = Object::decode(bytes) {
            assert_eq!(obj, expected);
            assert!(left.is_empty());
        }
        else {
            panic!("decode error!");
        }
    }

    fn assert_encode_decode_roundtrip(original_obj: &Object, expected_encoded: &[u8]) {
        // Test encoding
        let encoded_bytes = original_obj.encode();
        assert_eq!(encoded_bytes, expected_encoded, "not eq: {:?}", original_obj);

        // Test decoding the result of our encode
        match Object::decode(&encoded_bytes) {
            Some((decoded_obj, left)) => {
                assert_eq!(&decoded_obj, original_obj, "not eq: {:?}", encoded_bytes);
                assert!(left.is_empty());
            }
            None => {
                panic!("can not decode: {:?}", encoded_bytes);
            }
        }
    }

    #[test]
    fn test_decode_int() {
        parse_and_cmp(b"i0e", Object::Int(0));
        parse_and_cmp(b"i42e", Object::Int(42));
        parse_and_cmp(b"i-42e", Object::Int(-42));
        parse_and_cmp(b"i1234567890e", Object::Int(1234567890));
        parse_and_cmp(b"i9223372036854775807e", Object::Int(i64::MAX));
        parse_and_cmp(b"i-9223372036854775808e", Object::Int(i64::MIN));
    }

    #[test]
    fn test_decode_string() {
        parse_and_cmp(b"4:spam", Object::String(b"spam".to_vec()));
        parse_and_cmp(b"0:", Object::String(b"".to_vec()));
        parse_and_cmp(b"10:0123456789", Object::String(b"0123456789".to_vec()));
        parse_and_cmp(b"3:a b", Object::String(b"a b".to_vec()));
    }

    #[test]
    fn test_decode_list() {
        let list = Object::List(
            vec![
                Object::Int(1),
                Object::Int(2),
            ]
        );
        let bytes = b"li1ei2ee";
        parse_and_cmp(bytes, list);
    }

    #[test]
    fn test_encode_int() {
        assert_encode_decode_roundtrip(&Object::Int(123), b"i123e");
        assert_encode_decode_roundtrip(&Object::Int(0), b"i0e");
        assert_encode_decode_roundtrip(&Object::Int(-45), b"i-45e");
    }

    #[test]
    fn test_encode_string() {
        assert_encode_decode_roundtrip(&Object::String(b"spam".to_vec()), b"4:spam");
        assert_encode_decode_roundtrip(&Object::String(b"".to_vec()), b"0:");
    }

    #[test]
    fn test_encode_list() {
        assert_encode_decode_roundtrip(&Object::List(Vec::new()), b"le");
        let list = Object::List(vec![
            Object::Int(1),
            Object::String(b"two".to_vec()),
        ]);
        assert_encode_decode_roundtrip(&list, b"li1e3:twoe");
    }

    #[test]
    fn test_encode_dict() {
        assert_encode_decode_roundtrip(&Object::Dict(BTreeMap::new()), b"de");
        let mut dict = BTreeMap::new();
        dict.insert(b"key1".to_vec(), Object::Int(100));
        dict.insert(b"alpha".to_vec(), Object::String(b"val".to_vec())); // "alpha" < "key1"
        assert_encode_decode_roundtrip(&Object::Dict(dict), b"d5:alpha3:val4:key1i100ee");
    }

    #[test]
    fn test_encode_nested_structure() {
        let mut inner_dict = BTreeMap::new();
        inner_dict.insert(b"c".to_vec(), Object::Int(3));

        let obj = Object::List(vec![
            Object::Int(1),
            Object::Dict(inner_dict),
            Object::String(b"end".to_vec()),
        ]);
        // l i1e d 1:c i3e e 3:end e
        assert_encode_decode_roundtrip(&obj, b"li1ed1:ci3ee3:ende");
    }

}