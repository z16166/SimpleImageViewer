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

/// Maximum items in a single descriptor object or list. Caps allocation from
/// untrusted PSD/PSB descriptor counts before any element is read.
const MAX_DESCRIPTOR_ITEMS: usize = 10_000;
/// Maximum nesting depth for Objc/GlbO/VlLs descriptors (stack DoS guard).
const MAX_DESCRIPTOR_DEPTH: usize = 64;
/// Adobe OSType descriptor version used by Layer Comp IR and related blocks.
const DESCRIPTOR_VERSION_V16: u32 = 16;

pub(crate) fn parse_versioned_descriptor(bytes: &[u8]) -> Option<DescriptorObject> {
    let mut parser = DescriptorParser {
        bytes,
        pos: 0,
        depth: 0,
    };
    let version = parser.read_u32()?;
    if version != DESCRIPTOR_VERSION_V16 {
        // Layer Comp / cmls IR only documents version 16; other versions are
        // unsupported (not silently treated as empty). Callers map None to
        // "no comps" -- surface the rejection so checklist #15 is not silent.
        log::debug!(
            "PSD/PSB descriptor version {version} unsupported (expected {DESCRIPTOR_VERSION_V16})"
        );
        return None;
    }
    parser.parse_descriptor()
}

struct DescriptorParser<'a> {
    bytes: &'a [u8],
    pos: usize,
    depth: usize,
}

impl DescriptorParser<'_> {
    fn parse_descriptor(&mut self) -> Option<DescriptorObject> {
        if self.depth >= MAX_DESCRIPTOR_DEPTH {
            return None;
        }
        self.depth += 1;
        let result = self.parse_descriptor_inner();
        self.depth -= 1;
        result
    }

    fn parse_descriptor_inner(&mut self) -> Option<DescriptorObject> {
        self.read_unicode_string()?;
        self.read_id_string()?;
        let item_count = self.read_u32()? as usize;
        if item_count > MAX_DESCRIPTOR_ITEMS {
            return None;
        }
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
                if self.depth >= MAX_DESCRIPTOR_DEPTH {
                    return None;
                }
                self.depth += 1;
                let list = (|| {
                    let count = self.read_u32()? as usize;
                    if count > MAX_DESCRIPTOR_ITEMS {
                        return None;
                    }
                    let mut values = Vec::with_capacity(count);
                    for _ in 0..count {
                        values.push(self.parse_value()?);
                    }
                    Some(DescriptorValue::List(values))
                })();
                self.depth -= 1;
                list
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
            // --- Unit Float: 4-byte unit ID + 8-byte double ---
            b"UnFl" => {
                self.read_bytes(4)?; // unit ID
                self.read_bytes(8)?; // double value
                Some(DescriptorValue::Skipped)
            }
            // --- Global Class: same structure as Objc/GlbO ---
            b"GlbC" => self.parse_descriptor().map(DescriptorValue::Object),
            // --- Alias: 4-byte length + data ---
            b"alis" => {
                let alias_len = self.read_u32()? as usize;
                self.read_bytes(alias_len)?;
                Some(DescriptorValue::Skipped)
            }
            // --- Raw Data: 4-byte length + data ---
            b"tdta" => {
                let data_len = self.read_u32()? as usize;
                self.read_bytes(data_len)?;
                Some(DescriptorValue::Skipped)
            }
            // --- Compound/Inline Structure: stored as descriptor ---
            b"comp" => {
                self.parse_descriptor()?;
                Some(DescriptorValue::Skipped)
            }
            // --- Object Reference: reference form + form-specific data ---
            b"obj " => {
                let form = self.read_bytes(4)?;
                let form_arr: [u8; 4] = form.try_into().ok()?;
                self.skip_reference_form(form_arr)?;
                Some(DescriptorValue::Skipped)
            }
            // --- Type Tool Info Reference: class ID + descriptor ---
            b"type" => {
                self.read_id_string()?;
                self.parse_descriptor()?;
                Some(DescriptorValue::Skipped)
            }
            _ => None,
        }
    }

    /// Skip a reference form body for the `obj ` OSType.
    ///
    /// Each form is identified by a 4-byte OSType code followed by form-specific
    /// data. Handles all forms documented in the Adobe PSD specification:
    /// `Clss`, `Enmr`, `Idnt`, `indx`, `name`, `rele`, `Alis`, `desc`, `prop`.
    fn skip_reference_form(&mut self, form: [u8; 4]) -> Option<()> {
        match &form {
            b"Clss" => {
                // Class reference: className (Unicode) + classID (ID string)
                self.read_unicode_string()?;
                self.read_id_string()?;
                Some(())
            }
            b"Enmr" => {
                // Enumerated reference: classID + enumType + enumValue
                self.read_id_string()?;
                self.read_id_string()?;
                self.read_id_string()?;
                Some(())
            }
            b"Idnt" => {
                // Identifier reference: 4-byte integer
                self.read_bytes(4)?;
                Some(())
            }
            b"indx" => {
                // Index reference: 4-byte integer
                self.read_bytes(4)?;
                Some(())
            }
            b"name" => {
                // Name reference: Unicode string
                self.read_unicode_string()?;
                Some(())
            }
            b"rele" => {
                // Offset reference: container class ID + 4-byte offset
                self.read_id_string()?;
                self.read_bytes(4)?;
                Some(())
            }
            b"Alis" => {
                // Alias reference: length-prefixed alias data
                let alias_len = self.read_u32()? as usize;
                self.read_bytes(alias_len)?;
                Some(())
            }
            b"desc" => {
                // Descriptor reference: inline descriptor
                self.parse_descriptor()?;
                Some(())
            }
            b"prop" => {
                // Property reference: classID + propertyID
                self.read_id_string()?;
                self.read_id_string()?;
                Some(())
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
        // Adobe descriptor: zero length means a 4-byte ASCII class/key ID.
        let byte_len = if len == 0 { 4 } else { len };
        let bytes = self.read_bytes(byte_len)?;
        match std::str::from_utf8(bytes) {
            Ok(s) => Some(s.to_owned()),
            Err(e) => {
                log::debug!(
                    "PSD/PSB descriptor id string is not valid UTF-8 (len={byte_len}): {e}"
                );
                None
            }
        }
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

    #[test]
    fn new_ostypes_are_gracefully_skipped() {
        // Each new OSType must be parseable without failing the descriptor.
        // Test each type in its own single-key descriptor.

        // --- UnFl (Unit Float): 4-byte unit + 8-byte double ---
        {
            let mut bytes = Vec::new();
            bytes.extend_from_slice(&16u32.to_be_bytes());
            push_descriptor_header(&mut bytes, 1);
            push_key(&mut bytes, b"val ");
            bytes.extend_from_slice(b"UnFl");
            bytes.extend_from_slice(&1u32.to_be_bytes()); // unit ID (angle)
            bytes.extend_from_slice(&42.5f64.to_be_bytes());
            assert!(parse_versioned_descriptor(&bytes).is_some());
        }

        // --- GlbC (Global Class): same as Objc, a descriptor ---
        {
            let mut bytes = Vec::new();
            bytes.extend_from_slice(&16u32.to_be_bytes());
            push_descriptor_header(&mut bytes, 1);
            push_key(&mut bytes, b"cls ");
            bytes.extend_from_slice(b"GlbC");
            push_descriptor_header(&mut bytes, 1);
            push_key(&mut bytes, b"id  ");
            bytes.extend_from_slice(b"long");
            bytes.extend_from_slice(&42i32.to_be_bytes());
            assert!(parse_versioned_descriptor(&bytes).is_some());
        }

        // --- alis (Alias): 4-byte length + data ---
        {
            let mut bytes = Vec::new();
            bytes.extend_from_slice(&16u32.to_be_bytes());
            push_descriptor_header(&mut bytes, 1);
            push_key(&mut bytes, b"fil ");
            bytes.extend_from_slice(b"alis");
            bytes.extend_from_slice(&4u32.to_be_bytes()); // length = 4
            bytes.extend_from_slice(b"file");
            assert!(parse_versioned_descriptor(&bytes).is_some());
        }

        // --- tdta (Raw Data): 4-byte length + data ---
        {
            let mut bytes = Vec::new();
            bytes.extend_from_slice(&16u32.to_be_bytes());
            push_descriptor_header(&mut bytes, 1);
            push_key(&mut bytes, b"raw ");
            bytes.extend_from_slice(b"tdta");
            bytes.extend_from_slice(&3u32.to_be_bytes()); // length = 3
            bytes.extend_from_slice(b"\x01\x02\x03");
            assert!(parse_versioned_descriptor(&bytes).is_some());
        }

        // --- comp (Compound/Inline Structure): descriptor ---
        {
            let mut bytes = Vec::new();
            bytes.extend_from_slice(&16u32.to_be_bytes());
            push_descriptor_header(&mut bytes, 1);
            push_key(&mut bytes, b"cval");
            bytes.extend_from_slice(b"comp");
            push_descriptor_header(&mut bytes, 0); // compound has descriptor format
            assert!(parse_versioned_descriptor(&bytes).is_some());
        }

        // --- obj  (Object Reference): Clss form ---
        {
            let mut bytes = Vec::new();
            bytes.extend_from_slice(&16u32.to_be_bytes());
            push_descriptor_header(&mut bytes, 1);
            push_key(&mut bytes, b"ref ");
            bytes.extend_from_slice(b"obj ");
            bytes.extend_from_slice(b"Clss");
            push_unicode_string(&mut bytes, "Layer");
            push_class_id(&mut bytes, b"Lyr ");
            assert!(parse_versioned_descriptor(&bytes).is_some());
        }

        // --- type (Type Tool Info Reference): class ID + descriptor ---
        {
            let mut bytes = Vec::new();
            bytes.extend_from_slice(&16u32.to_be_bytes());
            push_descriptor_header(&mut bytes, 1);
            push_key(&mut bytes, b"txt ");
            bytes.extend_from_slice(b"type");
            push_class_id(&mut bytes, b"TxLr"); // class ID
            push_descriptor_header(&mut bytes, 0); // sub-descriptor
            assert!(parse_versioned_descriptor(&bytes).is_some());
        }
    }

    #[test]
    fn object_reference_all_forms_are_skipped() {
        // Test each obj  reference form to ensure it doesn\'t corrupt parsing.
        // Each test descriptor has two keys: the reference under test and a
        // trailing `long` key.  If the reference skip is correct the `long`
        // value is still readable.

        fn make_descriptor_with_ref(form: &[u8], ref_body: &[u8]) -> Vec<u8> {
            let mut bytes = Vec::new();
            bytes.extend_from_slice(&16u32.to_be_bytes());
            push_descriptor_header(&mut bytes, 2);
            // Key 1: reference under test
            push_key(&mut bytes, b"ref ");
            bytes.extend_from_slice(b"obj ");
            bytes.extend_from_slice(form);
            bytes.extend_from_slice(ref_body);
            // Key 2: sentinel long value — must be reachable after the ref
            push_key(&mut bytes, b"val ");
            bytes.extend_from_slice(b"long");
            bytes.extend_from_slice(&99i32.to_be_bytes());
            bytes
        }

        // Clss: className (Unicode) + classID (ID)
        {
            let mut ref_body = Vec::new();
            push_unicode_string(&mut ref_body, "MyClass");
            push_class_id(&mut ref_body, b"Cls ");
            let bytes = make_descriptor_with_ref(b"Clss", &ref_body);
            let parsed = parse_versioned_descriptor(&bytes).expect("Clss ref");
            assert_eq!(
                parsed.get("val "),
                Some(&DescriptorValue::Long(99)),
                "Clss: trailing value corrupted"
            );
        }

        // Enmr: classID + enumType + enumValue
        {
            let mut ref_body = Vec::new();
            push_class_id(&mut ref_body, b"Lyr ");
            push_class_id(&mut ref_body, b"Md  ");
            push_class_id(&mut ref_body, b"Nrml");
            let bytes = make_descriptor_with_ref(b"Enmr", &ref_body);
            let parsed = parse_versioned_descriptor(&bytes).expect("Enmr ref");
            assert_eq!(
                parsed.get("val "),
                Some(&DescriptorValue::Long(99)),
                "Enmr: trailing value corrupted"
            );
        }

        // Idnt: 4-byte integer
        {
            let ref_body = 12345u32.to_be_bytes();
            let bytes = make_descriptor_with_ref(b"Idnt", &ref_body);
            let parsed = parse_versioned_descriptor(&bytes).expect("Idnt ref");
            assert_eq!(
                parsed.get("val "),
                Some(&DescriptorValue::Long(99)),
                "Idnt: trailing value corrupted"
            );
        }

        // indx: 4-byte integer
        {
            let ref_body = 7u32.to_be_bytes();
            let bytes = make_descriptor_with_ref(b"indx", &ref_body);
            let parsed = parse_versioned_descriptor(&bytes).expect("indx ref");
            assert_eq!(
                parsed.get("val "),
                Some(&DescriptorValue::Long(99)),
                "indx: trailing value corrupted"
            );
        }

        // name: Unicode string
        {
            let ref_body = {
                let mut v = Vec::new();
                push_unicode_string(&mut v, "MyLayer");
                v
            };
            let bytes = make_descriptor_with_ref(b"name", &ref_body);
            let parsed = parse_versioned_descriptor(&bytes).expect("name ref");
            assert_eq!(
                parsed.get("val "),
                Some(&DescriptorValue::Long(99)),
                "name: trailing value corrupted"
            );
        }

        // rele: container class ID + 4-byte offset
        {
            let mut ref_body = Vec::new();
            push_class_id(&mut ref_body, b"Lyr ");
            ref_body.extend_from_slice(&100u32.to_be_bytes());
            let bytes = make_descriptor_with_ref(b"rele", &ref_body);
            let parsed = parse_versioned_descriptor(&bytes).expect("rele ref");
            assert_eq!(
                parsed.get("val "),
                Some(&DescriptorValue::Long(99)),
                "rele: trailing value corrupted"
            );
        }

        // Alis: 4-byte length + alias data
        {
            let mut ref_body = Vec::new();
            ref_body.extend_from_slice(&4u32.to_be_bytes());
            ref_body.extend_from_slice(b"path");
            let bytes = make_descriptor_with_ref(b"Alis", &ref_body);
            let parsed = parse_versioned_descriptor(&bytes).expect("Alis ref");
            assert_eq!(
                parsed.get("val "),
                Some(&DescriptorValue::Long(99)),
                "Alis: trailing value corrupted"
            );
        }

        // desc: inline descriptor
        {
            let ref_body = {
                let mut v = Vec::new();
                push_descriptor_header(&mut v, 0);
                v
            };
            let bytes = make_descriptor_with_ref(b"desc", &ref_body);
            let parsed = parse_versioned_descriptor(&bytes).expect("desc ref");
            assert_eq!(
                parsed.get("val "),
                Some(&DescriptorValue::Long(99)),
                "desc: trailing value corrupted"
            );
        }

        // prop: classID + propertyID
        {
            let mut ref_body = Vec::new();
            push_class_id(&mut ref_body, b"Lyr ");
            push_class_id(&mut ref_body, b"nm  ");
            let bytes = make_descriptor_with_ref(b"prop", &ref_body);
            let parsed = parse_versioned_descriptor(&bytes).expect("prop ref");
            assert_eq!(
                parsed.get("val "),
                Some(&DescriptorValue::Long(99)),
                "prop: trailing value corrupted"
            );
        }
    }

    #[test]
    fn unknown_reference_form_fails_closed() {
        let mut bytes = Vec::new();
        bytes.extend_from_slice(&16u32.to_be_bytes());
        push_descriptor_header(&mut bytes, 1);
        push_key(&mut bytes, b"ref ");
        bytes.extend_from_slice(b"obj ");
        bytes.extend_from_slice(b"????"); // unknown reference form

        assert!(parse_versioned_descriptor(&bytes).is_none());
    }

    #[test]
    fn unsupported_descriptor_version_returns_none() {
        let mut bytes = Vec::new();
        bytes.extend_from_slice(&15u32.to_be_bytes());
        push_descriptor_header(&mut bytes, 0);
        assert!(parse_versioned_descriptor(&bytes).is_none());
    }
}
