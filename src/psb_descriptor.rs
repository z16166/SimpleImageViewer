// Simple Image Viewer - A high-performance, cross-platform image viewer
// Copyright (C) 2024-2026 Simple Image Viewer Contributors
//
// This program is free software: you can redistribute it and/or modify
// it under the terms of the GNU General Public License as published by
// the Free Software Foundation, either version 3 of the License, or
// (at your option) any later version.
//
// This program is distributed in the hope that it will be useful,
// but WITHOUT ANY WARRANTY; without even the implied warranty of
// MERCHANTABILITY or FITNESS FOR A PARTICULAR PURPOSE.  See the
// GNU General Public License for more details.
//
// You should have received a copy of the GNU General Public License
// along with this program.  If not, see <https://www.gnu.org/licenses/>.

#![allow(dead_code)]

use std::collections::HashMap;

#[derive(Debug, Clone, PartialEq)]
pub(crate) enum DescriptorValue {
    Object(DescriptorObject),
    List(Vec<DescriptorValue>),
    Long(i32),
    Bool(bool),
    Text(String),
    Skipped,
}

pub(crate) type DescriptorObject = HashMap<String, DescriptorValue>;

pub(crate) fn parse_versioned_descriptor(bytes: &[u8]) -> Option<DescriptorObject> {
    let mut parser = DescriptorParser { bytes, pos: 0 };
    let version = parser.read_u32()?;
    if version != 16 {
        return None;
    }
    parser.parse_descriptor()
}

struct DescriptorParser<'a> {
    bytes: &'a [u8],
    pos: usize,
}

impl DescriptorParser<'_> {
    fn parse_descriptor(&mut self) -> Option<DescriptorObject> {
        self.read_unicode_string()?;
        self.read_id_string()?;
        let item_count = self.read_u32()? as usize;
        let mut object = DescriptorObject::with_capacity(item_count);
        for _ in 0..item_count {
            let key = self.read_id_string()?;
            let value = self.parse_value()?;
            object.insert(key, value);
        }
        Some(object)
    }

    fn parse_value(&mut self) -> Option<DescriptorValue> {
        let ostype = self.read_bytes(4)?;
        match ostype {
            b"Objc" | b"GlbO" => self.parse_descriptor().map(DescriptorValue::Object),
            b"VlLs" => {
                let count = self.read_u32()? as usize;
                let mut values = Vec::with_capacity(count);
                for _ in 0..count {
                    values.push(self.parse_value()?);
                }
                Some(DescriptorValue::List(values))
            }
            b"long" => self.read_i32().map(DescriptorValue::Long),
            b"bool" => {
                let value = *self.read_bytes(1)?.first()?;
                Some(DescriptorValue::Bool(value != 0))
            }
            b"TEXT" => self.read_unicode_string().map(DescriptorValue::Text),
            b"UntF" => {
                self.read_bytes(4)?;
                self.read_bytes(8)?;
                Some(DescriptorValue::Skipped)
            }
            b"doub" => {
                self.read_bytes(8)?;
                Some(DescriptorValue::Skipped)
            }
            b"enum" => {
                self.read_id_string()?;
                self.read_id_string()?;
                Some(DescriptorValue::Skipped)
            }
            _ => None,
        }
    }

    fn read_unicode_string(&mut self) -> Option<String> {
        let unit_count = self.read_u32()? as usize;
        let byte_len = unit_count.checked_mul(2)?;
        let bytes = self.read_bytes(byte_len)?;
        let mut units = Vec::with_capacity(unit_count);
        for chunk in bytes.chunks_exact(2) {
            units.push(u16::from_be_bytes([chunk[0], chunk[1]]));
        }
        if units.last().copied() == Some(0) {
            units.pop();
        }
        String::from_utf16(&units).ok()
    }

    fn read_id_string(&mut self) -> Option<String> {
        let len = self.read_u32()? as usize;
        let byte_len = if len == 0 { 4 } else { len };
        let bytes = self.read_bytes(byte_len)?;
        std::str::from_utf8(bytes).ok().map(ToOwned::to_owned)
    }

    fn read_i32(&mut self) -> Option<i32> {
        let bytes: [u8; 4] = self.read_bytes(4)?.try_into().ok()?;
        Some(i32::from_be_bytes(bytes))
    }

    fn read_u32(&mut self) -> Option<u32> {
        let bytes: [u8; 4] = self.read_bytes(4)?.try_into().ok()?;
        Some(u32::from_be_bytes(bytes))
    }

    fn read_bytes(&mut self, len: usize) -> Option<&[u8]> {
        let end = self.pos.checked_add(len)?;
        let bytes = self.bytes.get(self.pos..end)?;
        self.pos = end;
        Some(bytes)
    }
}

#[cfg(test)]
mod tests {
    use super::{DescriptorValue, parse_versioned_descriptor};

    fn push_unicode_string(bytes: &mut Vec<u8>, text: &str) {
        let units: Vec<u16> = text.encode_utf16().collect();
        bytes.extend_from_slice(&(units.len() as u32).to_be_bytes());
        for unit in units {
            bytes.extend_from_slice(&unit.to_be_bytes());
        }
    }

    fn push_class_id(bytes: &mut Vec<u8>, id: &[u8; 4]) {
        bytes.extend_from_slice(&0u32.to_be_bytes());
        bytes.extend_from_slice(id);
    }

    fn push_key(bytes: &mut Vec<u8>, key: &[u8; 4]) {
        bytes.extend_from_slice(&0u32.to_be_bytes());
        bytes.extend_from_slice(key);
    }

    fn push_descriptor_header(bytes: &mut Vec<u8>, item_count: u32) {
        push_unicode_string(bytes, "");
        push_class_id(bytes, b"null");
        bytes.extend_from_slice(&item_count.to_be_bytes());
    }

    #[test]
    fn parses_object_list_and_scalar_values() {
        let mut bytes = Vec::new();
        bytes.extend_from_slice(&16u32.to_be_bytes());
        push_descriptor_header(&mut bytes, 3);

        push_key(&mut bytes, b"list");
        bytes.extend_from_slice(b"VlLs");
        bytes.extend_from_slice(&2u32.to_be_bytes());
        bytes.extend_from_slice(b"long");
        bytes.extend_from_slice(&7i32.to_be_bytes());
        bytes.extend_from_slice(b"bool");
        bytes.push(1);

        push_key(&mut bytes, b"name");
        bytes.extend_from_slice(b"TEXT");
        push_unicode_string(&mut bytes, "Comp A");

        push_key(&mut bytes, b"objc");
        bytes.extend_from_slice(b"Objc");
        push_descriptor_header(&mut bytes, 1);
        push_key(&mut bytes, b"flag");
        bytes.extend_from_slice(b"bool");
        bytes.push(0);

        let parsed = parse_versioned_descriptor(&bytes).expect("descriptor");

        assert_eq!(
            parsed.get("list"),
            Some(&DescriptorValue::List(vec![
                DescriptorValue::Long(7),
                DescriptorValue::Bool(true)
            ]))
        );
        assert_eq!(
            parsed.get("name"),
            Some(&DescriptorValue::Text("Comp A".to_string()))
        );
        let Some(DescriptorValue::Object(obj)) = parsed.get("objc") else {
            panic!("expected nested object");
        };
        assert_eq!(obj.get("flag"), Some(&DescriptorValue::Bool(false)));
    }

    #[test]
    fn unknown_value_type_fails_closed() {
        let mut bytes = Vec::new();
        bytes.extend_from_slice(&16u32.to_be_bytes());
        push_descriptor_header(&mut bytes, 1);
        push_key(&mut bytes, b"bad ");
        bytes.extend_from_slice(b"????");

        assert!(parse_versioned_descriptor(&bytes).is_none());
    }
}
