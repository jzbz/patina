use super::ffi;
use std::ffi::{CStr, c_char};

/// An owned copy of an `mpv_node`: the value of a property or command result.
#[derive(Debug, Clone, Default, PartialEq)]
pub enum Node {
    #[default]
    None,
    String(String),
    Flag(bool),
    Int(i64),
    Double(f64),
    Array(Vec<Node>),
    Map(Vec<(String, Node)>),
    Bytes(Vec<u8>),
}

impl Node {
    /// Looks up a key of a map node.
    pub fn get(&self, key: &str) -> Option<&Node> {
        match self {
            Node::Map(entries) => entries.iter().find(|(k, _)| k == key).map(|(_, v)| v),
            _ => None,
        }
    }

    pub fn as_str(&self) -> Option<&str> {
        match self {
            Node::String(value) => Some(value),
            _ => None,
        }
    }

    pub fn as_bool(&self) -> Option<bool> {
        match self {
            Node::Flag(value) => Some(*value),
            _ => None,
        }
    }

    pub fn as_i64(&self) -> Option<i64> {
        match self {
            Node::Int(value) => Some(*value),
            Node::Double(value) if value.fract() == 0.0 => Some(*value as i64),
            _ => None,
        }
    }

    /// Numeric value, accepting both integer and floating point nodes.
    pub fn as_f64(&self) -> Option<f64> {
        match self {
            Node::Double(value) => Some(*value),
            Node::Int(value) => Some(*value as f64),
            _ => None,
        }
    }

    pub fn as_array(&self) -> &[Node] {
        match self {
            Node::Array(values) => values,
            _ => &[],
        }
    }

    pub fn str_or_default(&self, key: &str) -> String {
        self.get(key)
            .and_then(Node::as_str)
            .unwrap_or_default()
            .to_owned()
    }

    pub fn bool_or(&self, key: &str, default: bool) -> bool {
        self.get(key).and_then(Node::as_bool).unwrap_or(default)
    }

    /// Copies a node returned by mpv.
    ///
    /// # Safety
    /// `node` must be a valid, initialized `mpv_node` owned by mpv for the
    /// duration of the call.
    pub(crate) unsafe fn from_raw(node: &ffi::mpv_node) -> Node {
        // SAFETY (whole function): the union member read is the one selected by
        // `format`, and mpv guarantees list/array pointers are valid for `num`
        // elements while the node is alive.
        unsafe {
            match node.format {
                ffi::MPV_FORMAT_STRING => Node::String(string_from_ptr(node.u.string)),
                ffi::MPV_FORMAT_FLAG => Node::Flag(node.u.flag != 0),
                ffi::MPV_FORMAT_INT64 => Node::Int(node.u.int64),
                ffi::MPV_FORMAT_DOUBLE => Node::Double(node.u.double_),
                ffi::MPV_FORMAT_NODE_ARRAY => {
                    let list = &*node.u.list;
                    let values = slice(list.values, list.num);
                    Node::Array(values.iter().map(|value| Node::from_raw(value)).collect())
                }
                ffi::MPV_FORMAT_NODE_MAP => {
                    let list = &*node.u.list;
                    let values = slice(list.values, list.num);
                    let keys = slice(list.keys, list.num);
                    Node::Map(
                        keys.iter()
                            .zip(values)
                            .map(|(key, value)| (string_from_ptr(*key), Node::from_raw(value)))
                            .collect(),
                    )
                }
                ffi::MPV_FORMAT_BYTE_ARRAY => {
                    let bytes = &*node.u.ba;
                    Node::Bytes(slice(bytes.data.cast::<u8>(), bytes.size as i32).to_vec())
                }
                _ => Node::None,
            }
        }
    }
}

/// # Safety
/// `ptr` must be null or point to `len` initialized values.
unsafe fn slice<'a, T>(ptr: *const T, len: i32) -> &'a [T] {
    if ptr.is_null() || len <= 0 {
        &[]
    } else {
        // SAFETY: checked non-null; the caller guarantees `len` elements.
        unsafe { std::slice::from_raw_parts(ptr, len as usize) }
    }
}

/// # Safety
/// `ptr` must be null or a valid NUL-terminated string.
pub(crate) unsafe fn string_from_ptr(ptr: *const c_char) -> String {
    if ptr.is_null() {
        String::new()
    } else {
        // SAFETY: the caller guarantees a valid C string.
        unsafe { CStr::from_ptr(ptr) }
            .to_string_lossy()
            .into_owned()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn map(entries: &[(&str, Node)]) -> Node {
        Node::Map(
            entries
                .iter()
                .map(|(k, v)| ((*k).to_owned(), v.clone()))
                .collect(),
        )
    }

    #[test]
    fn lookups_and_numeric_coercion() {
        let node = map(&[
            ("id", Node::Int(3)),
            ("title", Node::String("Commentary".into())),
            ("selected", Node::Flag(true)),
            ("fps", Node::Double(23.976)),
        ]);
        assert_eq!(node.get("id").and_then(Node::as_f64), Some(3.0));
        assert_eq!(node.get("fps").and_then(Node::as_i64), None);
        assert_eq!(node.str_or_default("title"), "Commentary");
        assert_eq!(node.str_or_default("missing"), "");
        assert!(node.bool_or("selected", false));
        assert!(Node::Int(1).get("x").is_none());
        assert!(Node::None.as_array().is_empty());
    }
}
