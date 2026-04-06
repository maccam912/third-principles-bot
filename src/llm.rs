use serde::{Deserialize, Serialize};
use std::collections::HashMap;

#[derive(Debug, Clone, Deserialize)]
pub struct BlockEntry {
    pub x: i32,
    pub y: i32,
    pub z: i32,
    pub block: String,
}

#[derive(Debug, Clone, Deserialize)]
pub struct Structure {
    pub blocks: Vec<BlockEntry>,
    pub materials: HashMap<String, u32>,
}

#[derive(Serialize)]
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
            .map(|(k, v)| format!("  {k}: {v}"))
            .collect::<Vec<_>>()
            .join("\n")
    };

    let system = "You are a Minecraft structure generator. Given a description and available \
        inventory, output a JSON object describing a voxel structure centered at the origin. \
        x/y/z are integer offsets from origin; y=0 is ground level. Use Minecraft namespaced \
        block IDs (e.g. \"minecraft:dirt\"). Prefer blocks from the provided inventory but you \
        may also use other common minable blocks (stone, wood, dirt, gravel, cobblestone) if \
        needed for a coherent structure. Keep structures small (under 10x10x10). \
        Output ONLY the JSON object — no markdown fences, no explanation.";

    let user = format!("Description: {description}\nAvailable inventory:\n{inventory_str}");

    let request = ChatRequest {
        model,
        messages: vec![
            ChatMessage {
                role: "system",
                content: system.to_owned(),
            },
            ChatMessage {
                role: "user",
                content: user,
            },
        ],
        response_format: ResponseFormat {
            kind: "json_object",
        },
    };

    let client = reqwest::Client::new();
    let url = format!("{}/chat/completions", base_url.trim_end_matches('/'));
    eprintln!("[llm] sending POST {url}");

    let response = client
        .post(&url)
        .bearer_auth(&api_key)
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

    let structure = serde_json::from_str::<Structure>(&content)
        .map_err(|e| format!("failed to parse structure JSON: {e}\nContent: {content}"))?;
    eprintln!(
        "[llm] parsed structure blocks={} materials={}",
        structure.blocks.len(),
        structure.materials.len()
    );
    Ok(structure)
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
    fn parses_structure_from_llm_json() {
        let json = r#"{
            "blocks": [
                {"x": 0, "y": 0, "z": 0, "block": "minecraft:dirt"},
                {"x": 1, "y": 1, "z": 0, "block": "minecraft:oak_planks"}
            ],
            "materials": {
                "minecraft:dirt": 1,
                "minecraft:oak_planks": 1
            }
        }"#;
        let structure: Structure = serde_json::from_str(json).unwrap();
        assert_eq!(structure.blocks.len(), 2);
        assert_eq!(structure.blocks[0].block, "minecraft:dirt");
        assert_eq!(structure.blocks[0].x, 0);
        assert_eq!(structure.blocks[1].y, 1);
        assert_eq!(structure.materials["minecraft:dirt"], 1);
        assert_eq!(structure.materials["minecraft:oak_planks"], 1);
    }

    #[test]
    fn structure_parse_fails_on_missing_blocks_field() {
        let json = r#"{"materials": {"minecraft:dirt": 1}}"#;
        let result: Result<Structure, _> = serde_json::from_str(json);
        assert!(result.is_err());
    }

    #[test]
    fn structure_parse_fails_on_missing_materials_field() {
        let json = r#"{"blocks": []}"#;
        let result: Result<Structure, _> = serde_json::from_str(json);
        assert!(result.is_err());
    }
}
