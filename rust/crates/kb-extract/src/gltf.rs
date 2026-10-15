//! `.gltf` / `.glb` → a text summary of the model's metadata.
//!
//! A 3D model has no prose, so the TypeScript service does not try to extract
//! any: it writes a readable inventory of the asset, scenes, nodes, meshes,
//! materials, animations, textures, images and extensions
//! (`src/lib/files/parsers.ts:170`). The output is copied exactly, including
//! its leading `[3D Model: name]` heading and `(unnamed)` placeholders, because
//! the heading is what shows up as the document's first line in search results.

use serde_json::Value;

use crate::ExtractError;

/// `glTF` binary magic, little-endian.
const GLB_MAGIC: u32 = 0x4654_6C67;
/// magic + version + length + chunk header.
const GLB_HEADER_BYTES: usize = 20;

/// The inventory of a `.gltf` file.
pub fn parse_gltf(bytes: &[u8], file_name: &str) -> Result<String, ExtractError> {
    let text = crate::text::to_utf8(bytes);
    let json: Value = serde_json::from_str(&text)
        .map_err(|e| ExtractError::Container(format!("not a glTF JSON document: {e}")))?;
    if !json.is_object() {
        return Err(ExtractError::Container("glTF JSON must be an object".to_string()));
    }
    Ok(summarize(&json, file_name))
}

/// The inventory of a `.glb` file: the same JSON, carried in the first chunk.
pub fn parse_glb(bytes: &[u8], file_name: &str) -> Result<String, ExtractError> {
    if bytes.len() < GLB_HEADER_BYTES {
        return Err(ExtractError::Container("GLB file too small".to_string()));
    }
    let magic = u32::from_le_bytes([bytes[0], bytes[1], bytes[2], bytes[3]]);
    if magic != GLB_MAGIC {
        return Err(ExtractError::Container(
            "Invalid GLB magic number".to_string(),
        ));
    }
    let json_length = u32::from_le_bytes([bytes[12], bytes[13], bytes[14], bytes[15]]) as usize;
    let end = GLB_HEADER_BYTES.saturating_add(json_length).min(bytes.len());
    let text = crate::text::to_utf8(&bytes[GLB_HEADER_BYTES..end]);
    let json: Value = serde_json::from_str(&text)
        .map_err(|e| ExtractError::Container(format!("GLB JSON chunk is unreadable: {e}")))?;
    if !json.is_object() {
        return Err(ExtractError::Container("GLB JSON must be an object".to_string()));
    }
    Ok(summarize(&json, file_name))
}

fn summarize(json: &Value, file_name: &str) -> String {
    let mut lines: Vec<String> = vec![format!("[3D Model: {file_name}]"), String::new()];

    if let Some(asset) = json.get("asset") {
        lines.push(format!(
            "Format: glTF {}",
            truthy(asset.get("version")).unwrap_or("2.0")
        ));
        if let Some(generator) = truthy(asset.get("generator")) {
            lines.push(format!("Generator: {generator}"));
        }
        lines.push(String::new());
    }

    for (key, label) in [
        ("scenes", "Scenes"),
        ("nodes", "Nodes"),
        ("meshes", "Meshes"),
        ("materials", "Materials"),
        ("animations", "Animations"),
    ] {
        let Some(entries) = array(json.get(key)) else {
            continue;
        };
        if entries.is_empty() {
            continue;
        }
        lines.push(format!("{label} ({}):", entries.len()));
        for entry in entries {
            lines.push(format!("  - {}", truthy(entry.get("name")).unwrap_or("(unnamed)")));
        }
        lines.push(String::new());
    }

    if let Some(count) = array(json.get("textures")).filter(|items| !items.is_empty()) {
        lines.push(format!("Textures: {}", count.len()));
    }
    if let Some(count) = array(json.get("images")).filter(|items| !items.is_empty()) {
        lines.push(format!("Images: {}", count.len()));
    }
    if let Some(extensions) = array(json.get("extensionsUsed")).filter(|items| !items.is_empty())
    {
        let names: Vec<&str> = extensions.iter().filter_map(Value::as_str).collect();
        lines.push(format!("Extensions: {}", names.join(", ")));
    }

    lines.join("\n").trim().to_string()
}

/// A non-empty string field, matching JavaScript's `value || fallback` where an
/// empty string is falsy.
fn truthy(value: Option<&Value>) -> Option<&str> {
    value
        .and_then(Value::as_str)
        .filter(|text| !text.is_empty())
}

fn array(value: Option<&Value>) -> Option<&Vec<Value>> {
    value.and_then(Value::as_array)
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used, clippy::panic, clippy::cast_possible_truncation)]
mod tests {
    use super::*;

    const MODEL: &str = r#"{
      "asset": {"version": "2.0", "generator": "hand written"},
      "scenes": [{"name": "Scene"}, {}],
      "nodes": [{"name": "Cube"}],
      "textures": [{"source": 0}],
      "extensionsUsed": ["KHR_materials_clearcoat"]
    }"#;

    #[test]
    fn the_inventory_matches_the_typescript_template() {
        assert_eq!(
            parse_gltf(MODEL.as_bytes(), "chair.gltf").unwrap(),
            "[3D Model: chair.gltf]\n\nFormat: glTF 2.0\nGenerator: hand written\n\n\
             Scenes (2):\n  - Scene\n  - (unnamed)\n\nNodes (1):\n  - Cube\n\n\
             Textures: 1\nExtensions: KHR_materials_clearcoat"
        );
    }

    #[test]
    fn a_missing_asset_block_keeps_the_heading_only() {
        assert_eq!(
            parse_gltf(b"{}", "chair.gltf").unwrap(),
            "[3D Model: chair.gltf]"
        );
    }

    #[test]
    fn a_glb_reads_the_json_chunk() {
        let mut glb = Vec::new();
        glb.extend_from_slice(&GLB_MAGIC.to_le_bytes());
        glb.extend_from_slice(&2u32.to_le_bytes());
        let json = MODEL.as_bytes();
        let total_length = (GLB_HEADER_BYTES + json.len()) as u32;
        glb.extend_from_slice(&total_length.to_le_bytes());
        glb.extend_from_slice(&(json.len() as u32).to_le_bytes());
        glb.extend_from_slice(&0x4E4F_534Au32.to_le_bytes());
        glb.extend_from_slice(json);
        let text = parse_glb(&glb, "chair.glb").unwrap();
        assert!(text.starts_with("[3D Model: chair.glb]"), "{text}");
        assert!(text.contains("Format: glTF 2.0"), "{text}");
    }

    #[test]
    fn a_short_or_wrong_file_says_why() {
        assert_eq!(
            parse_glb(b"tiny", "chair.glb").unwrap_err().to_string(),
            "GLB file too small"
        );
        let mut wrong = vec![0u8; 24];
        wrong[0] = b'X';
        assert_eq!(
            parse_glb(&wrong, "chair.glb").unwrap_err().to_string(),
            "Invalid GLB magic number"
        );
    }

    #[test]
    fn json_that_is_not_an_object_is_a_container_error() {
        let err = parse_gltf(b"[1, 2, 3]", "chair.gltf").unwrap_err();
        assert!(matches!(err, ExtractError::Container(_)), "{err}");
    }
}
