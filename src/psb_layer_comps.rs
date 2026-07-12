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

use crate::psb_descriptor::{DescriptorObject, DescriptorValue, parse_versioned_descriptor};
use crate::psb_layer_composite::{
    LayerRecord, compute_effective_visibility, compute_effective_visibility_with_flags,
};

const IR_LAYER_COMPS: u16 = 1065;
const HIDDEN_FLAG: u8 = 0x02;

pub struct LayerCompInfo {
    pub id: i32,
    pub name: String,
}

pub struct LayerCompsResource {
    pub comps: Vec<LayerCompInfo>,
    pub last_applied: Option<i32>,
}

pub fn parse_layer_comps_from_ir(
    bytes: &[u8],
    ir_start: u64,
    ir_end: u64,
) -> Option<LayerCompsResource> {
    let payload = crate::psb_reader::find_image_resource(bytes, ir_start, ir_end, IR_LAYER_COMPS)?;
    let descriptor = parse_versioned_descriptor(payload)?;
    let comps = parse_comp_list(&descriptor)?;
    if comps.is_empty() {
        return None;
    }
    let last_applied = descriptor_long(&descriptor, "lastApplied");
    Some(LayerCompsResource {
        comps,
        last_applied,
    })
}

pub fn select_layer_comp(
    comps: &[LayerCompInfo],
    last_applied: Option<i32>,
) -> Option<&LayerCompInfo> {
    if let Some(id) = last_applied
        && let Some(comp) = comps.iter().find(|comp| comp.id == id)
    {
        return Some(comp);
    }
    comps.last()
}

/// Build visibility from records' cmls: if a layer has a setting whose compList
/// contains selected id, use that setting's `enab`; else keep file flag.
pub fn visibility_from_layer_comp(records: &[LayerRecord], comp_id: i32) -> Vec<bool> {
    let mut adjusted_flags: Option<Vec<u8>> = None;
    for (i, record) in records.iter().enumerate() {
        let Some(payload) = record.cmls_payload.as_deref() else {
            continue;
        };
        let Some(enabled) = cmls_enabled_for_comp(payload, comp_id) else {
            continue;
        };
        let flags = adjusted_flags.get_or_insert_with(|| records.iter().map(|r| r.flags).collect());
        if enabled {
            flags[i] &= !HIDDEN_FLAG;
        } else {
            flags[i] |= HIDDEN_FLAG;
        }
    }
    match adjusted_flags.as_deref() {
        Some(flags) => compute_effective_visibility_with_flags(records, Some(flags)),
        None => compute_effective_visibility(records),
    }
}

fn parse_comp_list(descriptor: &DescriptorObject) -> Option<Vec<LayerCompInfo>> {
    let DescriptorValue::List(items) = descriptor.get("list")? else {
        return None;
    };
    let mut comps = Vec::new();
    for item in items {
        let DescriptorValue::Object(comp) = item else {
            return None;
        };
        let id = descriptor_long(comp, "compID")?;
        let name = descriptor_text(comp, "Nm ").or_else(|| descriptor_text(comp, "Nm  "))?;
        comps.push(LayerCompInfo { id, name });
    }
    Some(comps)
}

fn cmls_enabled_for_comp(payload: &[u8], comp_id: i32) -> Option<bool> {
    let descriptor = parse_versioned_descriptor(payload)?;
    let DescriptorValue::List(settings) = descriptor.get("layerSettings")? else {
        return None;
    };
    for setting in settings {
        let DescriptorValue::Object(setting) = setting else {
            continue;
        };
        if setting_comp_list_contains(setting, comp_id)
            && let Some(enabled) = descriptor_bool(setting, "enab")
        {
            return Some(enabled);
        }
    }
    None
}

fn setting_comp_list_contains(setting: &DescriptorObject, comp_id: i32) -> bool {
    let Some(DescriptorValue::List(comp_list)) = setting.get("compList") else {
        return false;
    };
    comp_list
        .iter()
        .any(|value| matches!(value, DescriptorValue::Long(id) if *id == comp_id))
}

fn descriptor_long(descriptor: &DescriptorObject, key: &str) -> Option<i32> {
    match descriptor.get(key) {
        Some(DescriptorValue::Long(value)) => Some(*value),
        _ => None,
    }
}

fn descriptor_bool(descriptor: &DescriptorObject, key: &str) -> Option<bool> {
    match descriptor.get(key) {
        Some(DescriptorValue::Bool(value)) => Some(*value),
        _ => None,
    }
}

fn descriptor_text(descriptor: &DescriptorObject, key: &str) -> Option<String> {
    match descriptor.get(key) {
        Some(DescriptorValue::Text(value)) => Some(value.clone()),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::{
        LayerCompInfo, parse_layer_comps_from_ir, select_layer_comp, visibility_from_layer_comp,
    };
    use crate::psb_layer_composite::LayerRecord;

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

    fn push_key(bytes: &mut Vec<u8>, key: &[u8]) {
        if key.len() == 4 {
            bytes.extend_from_slice(&0u32.to_be_bytes());
        } else {
            bytes.extend_from_slice(&(key.len() as u32).to_be_bytes());
        }
        bytes.extend_from_slice(key);
    }

    fn push_descriptor_header(bytes: &mut Vec<u8>, item_count: u32) {
        push_unicode_string(bytes, "");
        push_class_id(bytes, b"null");
        bytes.extend_from_slice(&item_count.to_be_bytes());
    }

    fn push_comp_object(bytes: &mut Vec<u8>, id: i32, name: &str) {
        bytes.extend_from_slice(b"Objc");
        push_descriptor_header(bytes, 3);
        push_key(bytes, b"Nm ");
        bytes.extend_from_slice(b"TEXT");
        push_unicode_string(bytes, name);
        push_key(bytes, b"compID");
        bytes.extend_from_slice(b"long");
        bytes.extend_from_slice(&id.to_be_bytes());
        push_key(bytes, b"capturedInfo");
        bytes.extend_from_slice(b"Objc");
        push_descriptor_header(bytes, 0);
    }

    fn layer_comps_resource() -> Vec<u8> {
        let mut payload = Vec::new();
        payload.extend_from_slice(&16u32.to_be_bytes());
        push_descriptor_header(&mut payload, 2);
        push_key(&mut payload, b"list");
        payload.extend_from_slice(b"VlLs");
        payload.extend_from_slice(&2u32.to_be_bytes());
        push_comp_object(&mut payload, 101, "Draft");
        push_comp_object(&mut payload, 202, "Final");
        push_key(&mut payload, b"lastApplied");
        payload.extend_from_slice(b"long");
        payload.extend_from_slice(&101i32.to_be_bytes());
        payload
    }

    fn image_resource(rid: u16, data: &[u8]) -> Vec<u8> {
        let mut bytes = Vec::new();
        bytes.extend_from_slice(b"8BIM");
        bytes.extend_from_slice(&rid.to_be_bytes());
        bytes.push(0);
        bytes.push(0);
        bytes.extend_from_slice(&(data.len() as u32).to_be_bytes());
        bytes.extend_from_slice(data);
        if !data.len().is_multiple_of(2) {
            bytes.push(0);
        }
        bytes
    }

    fn test_record(
        hidden: bool,
        section_type: Option<u32>,
        cmls_payload: Option<Vec<u8>>,
    ) -> LayerRecord {
        LayerRecord {
            top: 0,
            left: 0,
            bottom: 1,
            right: 1,
            name: String::new(),
            layer_id: None,
            cmls_payload,
            channels: Vec::new(),
            blend: *b"norm",
            opacity: 255,
            clipping: 0,
            flags: if hidden { 2 } else { 0 },
            mask_size: 0,
            mask: None,
            real_mask: None,
            is_section_divider: section_type.is_some(),
            section_type,
        }
    }

    fn cmls_payload(comp_id: i32, enab: bool) -> Vec<u8> {
        let mut payload = Vec::new();
        payload.extend_from_slice(&16u32.to_be_bytes());
        push_descriptor_header(&mut payload, 1);
        push_key(&mut payload, b"layerSettings");
        payload.extend_from_slice(b"VlLs");
        payload.extend_from_slice(&1u32.to_be_bytes());
        payload.extend_from_slice(b"Objc");
        push_descriptor_header(&mut payload, 2);
        push_key(&mut payload, b"compList");
        payload.extend_from_slice(b"VlLs");
        payload.extend_from_slice(&1u32.to_be_bytes());
        payload.extend_from_slice(b"long");
        payload.extend_from_slice(&comp_id.to_be_bytes());
        push_key(&mut payload, b"enab");
        payload.extend_from_slice(b"bool");
        payload.push(u8::from(enab));
        payload
    }

    #[test]
    fn parses_layer_comps_resource_1065() {
        let mut ir = image_resource(1039, b"icc");
        ir.extend_from_slice(&image_resource(1065, &layer_comps_resource()));

        let parsed = parse_layer_comps_from_ir(&ir, 0, ir.len() as u64).expect("layer comps");

        assert_eq!(parsed.last_applied, Some(101));
        assert_eq!(parsed.comps.len(), 2);
        assert_eq!(parsed.comps[0].id, 101);
        assert_eq!(parsed.comps[0].name, "Draft");
        assert_eq!(parsed.comps[1].id, 202);
        assert_eq!(parsed.comps[1].name, "Final");
    }

    #[test]
    fn malformed_comp_list_entry_fails_closed() {
        // One valid comp + one object missing compID -- whole IR 1065 parse
        // must fail closed (None), not return a partial list.
        let mut payload = Vec::new();
        payload.extend_from_slice(&16u32.to_be_bytes());
        push_descriptor_header(&mut payload, 1);
        push_key(&mut payload, b"list");
        payload.extend_from_slice(b"VlLs");
        payload.extend_from_slice(&2u32.to_be_bytes());
        push_comp_object(&mut payload, 101, "Draft");
        payload.extend_from_slice(b"Objc");
        push_descriptor_header(&mut payload, 1);
        push_key(&mut payload, b"Nm ");
        payload.extend_from_slice(b"TEXT");
        push_unicode_string(&mut payload, "Broken");

        let ir = image_resource(1065, &payload);
        assert!(parse_layer_comps_from_ir(&ir, 0, ir.len() as u64).is_none());
    }

    #[test]
    fn select_layer_comp_prefers_last_applied_else_last() {
        let comps = vec![
            LayerCompInfo {
                id: 1,
                name: "One".to_string(),
            },
            LayerCompInfo {
                id: 2,
                name: "Two".to_string(),
            },
        ];

        assert_eq!(
            select_layer_comp(&comps, Some(1)).map(|comp| comp.id),
            Some(1)
        );
        assert_eq!(
            select_layer_comp(&comps, Some(99)).map(|comp| comp.id),
            Some(2)
        );
        assert_eq!(select_layer_comp(&comps, None).map(|comp| comp.id), Some(2));
    }

    #[test]
    fn visibility_from_cmls_applies_enab_then_group_visibility() {
        let records = vec![
            test_record(false, Some(3), None),
            test_record(true, None, Some(cmls_payload(7, true))),
            test_record(true, Some(1), Some(cmls_payload(7, true))),
        ];

        let visible = visibility_from_layer_comp(&records, 7);

        assert_eq!(visible, vec![true, true, true]);
    }

    #[test]
    fn visibility_from_cmls_keeps_file_flag_without_matching_comp() {
        let records = vec![test_record(true, None, Some(cmls_payload(3, true)))];

        let visible = visibility_from_layer_comp(&records, 7);

        assert_eq!(visible, vec![false]);
    }

    #[test]
    fn malformed_cmls_payload_is_skipped() {
        let records = vec![test_record(false, None, Some(vec![0, 0, 0, 16, b'b']))];

        let visible = visibility_from_layer_comp(&records, 7);

        assert_eq!(visible, vec![true]);
    }
}
