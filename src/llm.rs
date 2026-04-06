use serde::{Deserialize, Serialize};
use std::collections::HashMap;

// ---------------------------------------------------------------------------
// Public types (canonical, used throughout the codebase)
// ---------------------------------------------------------------------------

#[derive(Debug, Clone)]
pub struct BlockEntry {
    pub x: i32,
    pub y: i32,
    pub z: i32,
    pub block: String,
}

#[derive(Debug, Clone)]
pub struct Structure {
    pub blocks: Vec<BlockEntry>,
    pub materials: HashMap<String, u32>,
}

// ---------------------------------------------------------------------------
// Internal API wire types
// ---------------------------------------------------------------------------

#[derive(Serialize, Clone)]
struct ChatMessage {
    role: &'static str,
    content: String,
}

#[derive(Serialize)]
struct ChatRequest {
    model: String,
    messages: Vec<ChatMessage>,
    response_format: ResponseFormat,
}

#[derive(Serialize)]
struct ResponseFormat {
    #[serde(rename = "type")]
    kind: &'static str,
}

#[derive(Deserialize)]
struct ChatResponse {
    choices: Vec<ChatChoice>,
}

#[derive(Deserialize)]
struct ChatChoice {
    message: ChatChoiceMessage,
}

#[derive(Deserialize)]
struct ChatChoiceMessage {
    content: String,
}

// ---------------------------------------------------------------------------
// Internal LLM response format types
// ---------------------------------------------------------------------------

/// Tier 1: Palette + compact tuple array.
/// `{"p":["dirt","oak_planks"],"b":[[x,y,z,i],...]}`
#[derive(Deserialize)]
struct TupleFormat {
    p: Vec<String>,
    b: Vec<[i32; 4]>,
}

/// Tier 2: Layered 2D character grids.
/// `{"p":{"a":"dirt"},"l":["row0/row1",...],"offset":[ox,oy,oz]}`
#[derive(Deserialize)]
struct GridFormat {
    p: HashMap<String, String>,
    l: Vec<String>,
    #[serde(default)]
    offset: [i32; 3],
}

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

fn inventory_summary(inventory: &HashMap<String, u32>) -> String {
    let item_types = inventory.len();
    let total_blocks: u32 = inventory.values().copied().sum();
    let mut sample = inventory
        .iter()
        .map(|(item, count)| (item.as_str(), *count))
        .collect::<Vec<_>>();
    sample.sort_by(|a, b| b.1.cmp(&a.1).then_with(|| a.0.cmp(b.0)));
    let sample = sample
        .into_iter()
        .take(3)
        .map(|(item, count)| format!("{item}={count}"))
        .collect::<Vec<_>>()
        .join(", ");

    if sample.is_empty() {
        format!("{item_types} item types, {total_blocks} total blocks")
    } else {
        format!("{item_types} item types, {total_blocks} total blocks [{sample}]")
    }
}

fn content_preview(content: &str, max_len: usize) -> String {
    if content.len() <= max_len {
        return content.to_owned();
    }
    format!("{}...", &content[..max_len])
}

// ---------------------------------------------------------------------------
// LLM response parsers
// ---------------------------------------------------------------------------

fn parse_tuple_format(content: &str) -> Result<Structure, String> {
    let fmt: TupleFormat = serde_json::from_str(content)
        .map_err(|e| format!("tuple format parse error: {e}"))?;

    let mut blocks = Vec::with_capacity(fmt.b.len());
    let mut materials: HashMap<String, u32> = HashMap::new();

    for entry in &fmt.b {
        let [x, y, z, idx] = *entry;
        let idx = idx as usize;
        let short_name = fmt.p.get(idx).ok_or_else(|| {
            format!(
                "palette index {idx} out of range (palette has {} entries)",
                fmt.p.len()
            )
        })?;
        let block = format!("minecraft:{short_name}");
        *materials.entry(block.clone()).or_insert(0) += 1;
        blocks.push(BlockEntry { x, y, z, block });
    }

    Ok(Structure { blocks, materials })
}

fn parse_grid_format(content: &str) -> Result<Structure, String> {
    let fmt: GridFormat = serde_json::from_str(content)
        .map_err(|e| format!("grid format parse error: {e}"))?;

    let [ox, oy, oz] = fmt.offset;
    let mut blocks = Vec::new();
    let mut materials: HashMap<String, u32> = HashMap::new();

    for (yi, layer_str) in fmt.l.iter().enumerate() {
        let y = yi as i32 + oy;
        for (zi, row) in layer_str.split('/').enumerate() {
            let z = zi as i32 + oz;
            for (xi, ch) in row.chars().enumerate() {
                if ch == '.' {
                    continue;
                }
                let x = xi as i32 + ox;
                let ch_str = ch.to_string();
                let short_name = fmt.p.get(&ch_str).ok_or_else(|| {
                    format!("palette character '{ch}' not defined in palette")
                })?;
                let block = format!("minecraft:{short_name}");
                *materials.entry(block.clone()).or_insert(0) += 1;
                blocks.push(BlockEntry { x, y, z, block });
            }
        }
    }

    Ok(Structure { blocks, materials })
}

fn parse_llm_response(content: &str) -> Result<Structure, String> {
    let v: serde_json::Value =
        serde_json::from_str(content).map_err(|e| format!("invalid JSON: {e}"))?;

    if v.get("l").is_some() {
        parse_grid_format(content)
    } else if v.get("b").is_some() {
        parse_tuple_format(content)
    } else {
        Err(
            "unrecognized format: expected \"b\" key (tuple array) or \"l\" key (layered grids)"
                .to_owned(),
        )
    }
}

// ---------------------------------------------------------------------------
// HTTP helper
// ---------------------------------------------------------------------------

async fn call_api_once(
    client: &reqwest::Client,
    url: &str,
    api_key: &str,
    model: &str,
    messages: Vec<ChatMessage>,
) -> Result<String, String> {
    let request = ChatRequest {
        model: model.to_owned(),
        messages,
        response_format: ResponseFormat { kind: "json_object" },
    };

    eprintln!("[llm] sending POST {url}");
    let response = client
        .post(url)
        .bearer_auth(api_key)
        .json(&request)
        .send()
        .await
        .map_err(|e| format!("request failed: {e}"))?;
    eprintln!("[llm] received HTTP {}", response.status());

    if !response.status().is_success() {
        let status = response.status();
        let body = response.text().await.unwrap_or_default();
        eprintln!(
            "[llm] request failed status={} body_preview={}",
            status,
            content_preview(&body, 200)
        );
        return Err(format!("HTTP {status}: {body}"));
    }

    eprintln!("[llm] parsing API response body");
    let chat_resp: ChatResponse = response
        .json()
        .await
        .map_err(|e| format!("failed to parse API response: {e}"))?;

    let content = chat_resp
        .choices
        .into_iter()
        .next()
        .ok_or_else(|| "no choices in LLM response".to_owned())?
        .message
        .content;
    eprintln!(
        "[llm] received content chars={} preview={}",
        content.len(),
        content_preview(&content, 200)
    );

    Ok(content)
}

// ---------------------------------------------------------------------------
// System prompt
// ---------------------------------------------------------------------------

const SYSTEM_PROMPT: &str = "You are a Minecraft structure generator. Given a description and \
available inventory, output a compact JSON object for a voxel structure.\n\
\n\
RULES:\n\
- Do NOT use \"minecraft:\" prefixes — use short names: \"dirt\", \"oak_planks\", \"stone\", etc.\n\
- Do NOT include a \"materials\" field — it will be computed automatically.\n\
- Omit air and empty spaces — only output solid blocks.\n\
- Keep structures small (under 10x10x10). Prefer blocks from the inventory.\n\
- Output ONLY the JSON — no markdown fences, no explanation.\n\
\n\
Choose one of two formats:\n\
\n\
FORMAT A — Tuple array (works for any structure):\n\
{\"p\":[\"block_a\",\"block_b\"],\"b\":[[x,y,z,i],...]}\n\
  p = palette array of block names; b = array of [x,y,z,palette_index]; y=0 is ground level.\n\
Example (4-block stone pillar):\n\
{\"p\":[\"stone\"],\"b\":[[0,0,0,0],[0,1,0,0],[0,2,0,0],[0,3,0,0]]}\n\
\n\
FORMAT B — Layered character grids (best for dense box-like structures):\n\
{\"p\":{\"a\":\"block_a\",\"b\":\"block_b\"},\"l\":[\"layer_y0\",\"layer_y1\",...],\"offset\":[ox,oy,oz]}\n\
  p = single-char palette map; l = one string per Y level with rows separated by \"/\";\n\
  offset = [x,y,z] applied to all grid positions (default [0,0,0]). Use \".\" for air gaps.\n\
Example (3x1x2 dirt floor centered at origin):\n\
{\"p\":{\"d\":\"dirt\"},\"l\":[\"ddd/ddd\"],\"offset\":[-1,0,-1]}";

// ---------------------------------------------------------------------------
// Public API
// ---------------------------------------------------------------------------

pub async fn call_llm(
    description: &str,
    inventory: &HashMap<String, u32>,
) -> Result<Structure, String> {
    eprintln!(
        "[llm] preparing request description_len={} inventory={}",
        description.len(),
        inventory_summary(inventory)
    );

    let base_url = std::env::var("OPENROUTER_BASE_URL")
        .map_err(|_| "OPENROUTER_BASE_URL not set".to_owned())?;
    let api_key =
        std::env::var("OPENROUTER_API_KEY").map_err(|_| "OPENROUTER_API_KEY not set".to_owned())?;
    let model =
        std::env::var("OPENROUTER_MODEL").map_err(|_| "OPENROUTER_MODEL not set".to_owned())?;
    eprintln!(
        "[llm] config loaded base_url={} model={}",
        base_url.trim_end_matches('/'),
        model
    );

    let inventory_str = if inventory.is_empty() {
        "  (empty)".to_owned()
    } else {
        inventory
            .iter()
            .map(|(k, v)| {
                let short = k.strip_prefix("minecraft:").unwrap_or(k.as_str());
                format!("  {short}: {v}")
            })
            .collect::<Vec<_>>()
            .join("\n")
    };

    let user = format!("Description: {description}\nAvailable inventory:\n{inventory_str}");

    let base_messages = vec![
        ChatMessage {
            role: "system",
            content: SYSTEM_PROMPT.to_owned(),
        },
        ChatMessage {
            role: "user",
            content: user,
        },
    ];

    let client = reqwest::Client::new();
    let url = format!("{}/chat/completions", base_url.trim_end_matches('/'));

    let content = call_api_once(&client, &url, &api_key, &model, base_messages.clone()).await?;

    match parse_llm_response(&content) {
        Ok(structure) => {
            eprintln!(
                "[llm] parsed structure blocks={} materials={}",
                structure.blocks.len(),
                structure.materials.len()
            );
            Ok(structure)
        }
        Err(parse_err) => {
            eprintln!(
                "[llm] first parse failed: {}; retrying with error context",
                parse_err
            );
            let repair_messages = vec![
                base_messages[0].clone(),
                base_messages[1].clone(),
                ChatMessage {
                    role: "assistant",
                    content: content.clone(),
                },
                ChatMessage {
                    role: "user",
                    content: format!(
                        "Your response could not be parsed. Error: {parse_err}\n\
                         Please output ONLY the corrected JSON."
                    ),
                },
            ];
            let content2 =
                call_api_once(&client, &url, &api_key, &model, repair_messages).await?;
            let structure = parse_llm_response(&content2).map_err(|e2| {
                format!("failed to parse structure after retry: {e2}\nContent: {content2}")
            })?;
            eprintln!(
                "[llm] retry succeeded blocks={} materials={}",
                structure.blocks.len(),
                structure.materials.len()
            );
            Ok(structure)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn inventory_summary_reports_total_and_sample_items() {
        let inventory = HashMap::from([
            ("minecraft:dirt".to_owned(), 12),
            ("minecraft:oak_planks".to_owned(), 4),
            ("minecraft:cobblestone".to_owned(), 20),
            ("minecraft:glass".to_owned(), 2),
        ]);

        let summary = inventory_summary(&inventory);

        assert!(summary.contains("4 item types"));
        assert!(summary.contains("38 total blocks"));
        assert!(summary.contains("minecraft:cobblestone=20"));
    }

    #[test]
    fn content_preview_truncates_long_content() {
        let content = "abcdefghijklmnopqrstuvwxyz0123456789";

        let preview = content_preview(content, 16);

        assert_eq!(preview, "abcdefghijklmnop...");
    }

    #[test]
    fn parse_tuple_format_parses_blocks_and_computes_materials() {
        let json = r#"{"p":["dirt","oak_planks"],"b":[[-2,0,-2,0],[-1,0,-2,0],[0,0,-2,1]]}"#;
        let structure = parse_llm_response(json).unwrap();
        assert_eq!(structure.blocks.len(), 3);
        assert_eq!(structure.blocks[0].block, "minecraft:dirt");
        assert_eq!(structure.blocks[0].x, -2);
        assert_eq!(structure.blocks[2].block, "minecraft:oak_planks");
        assert_eq!(structure.materials["minecraft:dirt"], 2);
        assert_eq!(structure.materials["minecraft:oak_planks"], 1);
    }

    #[test]
    fn parse_tuple_format_prepends_minecraft_namespace() {
        let json = r#"{"p":["cobblestone"],"b":[[0,0,0,0],[0,1,0,0]]}"#;
        let structure = parse_llm_response(json).unwrap();
        assert!(structure.blocks.iter().all(|b| b.block.starts_with("minecraft:")));
    }

    #[test]
    fn parse_tuple_format_out_of_range_index_errors() {
        let json = r#"{"p":["dirt"],"b":[[0,0,0,5]]}"#;
        let result = parse_llm_response(json);
        assert!(result.is_err());
    }

    #[test]
    fn parse_grid_format_basic() {
        let json = r#"{"p":{"d":"dirt"},"l":["ddd/ddd"],"offset":[-1,0,-1]}"#;
        let structure = parse_llm_response(json).unwrap();
        assert_eq!(structure.blocks.len(), 6);
        assert!(structure.blocks.iter().all(|b| b.block == "minecraft:dirt"));
        assert_eq!(structure.materials["minecraft:dirt"], 6);
        // Check one specific position
        assert!(structure.blocks.iter().any(|b| b.x == -1 && b.y == 0 && b.z == -1));
        assert!(structure.blocks.iter().any(|b| b.x == 1 && b.y == 0 && b.z == 0));
    }

    #[test]
    fn parse_grid_format_with_default_offset() {
        let json = r#"{"p":{"s":"stone"},"l":["ss/ss"]}"#;
        let structure = parse_llm_response(json).unwrap();
        assert_eq!(structure.blocks.len(), 4);
        // offset defaults to [0,0,0]: positions (0,0,0),(1,0,0),(0,0,1),(1,0,1)
        assert!(structure.blocks.iter().any(|b| b.x == 0 && b.y == 0 && b.z == 0));
        assert!(structure.blocks.iter().any(|b| b.x == 1 && b.y == 0 && b.z == 1));
    }

    #[test]
    fn parse_grid_format_skips_dots() {
        let json = r#"{"p":{"s":"stone"},"l":["s.s/..."]}"#;
        let structure = parse_llm_response(json).unwrap();
        // Only 2 stone blocks; dots and the all-dot row produce nothing
        assert_eq!(structure.blocks.len(), 2);
    }

    #[test]
    fn parse_grid_format_multi_layer() {
        let json = r#"{"p":{"d":"dirt","g":"glass"},"l":["dd/dd","gg/gg"]}"#;
        let structure = parse_llm_response(json).unwrap();
        assert_eq!(structure.blocks.len(), 8);
        assert_eq!(structure.materials["minecraft:dirt"], 4);
        assert_eq!(structure.materials["minecraft:glass"], 4);
        // y=0 layer is dirt, y=1 layer is glass
        assert!(structure.blocks.iter().any(|b| b.y == 0 && b.block == "minecraft:dirt"));
        assert!(structure.blocks.iter().any(|b| b.y == 1 && b.block == "minecraft:glass"));
    }

    #[test]
    fn parse_grid_format_undefined_palette_char_errors() {
        let json = r#"{"p":{"d":"dirt"},"l":["dx"]}"#;
        let result = parse_llm_response(json);
        assert!(result.is_err());
    }

    #[test]
    fn parse_llm_response_detects_grid_format_by_l_key() {
        let json = r#"{"p":{"s":"stone"},"l":["s"]}"#;
        let result = parse_llm_response(json);
        assert!(result.is_ok());
        assert_eq!(result.unwrap().blocks.len(), 1);
    }

    #[test]
    fn parse_llm_response_errors_on_unknown_format() {
        let json = r#"{"blocks":[],"materials":{}}"#;
        let result = parse_llm_response(json);
        assert!(result.is_err());
        assert!(result.unwrap_err().contains("unrecognized format"));
    }

    #[test]
    fn parse_llm_response_errors_on_invalid_json() {
        let result = parse_llm_response("not json at all");
        assert!(result.is_err());
    }
}
