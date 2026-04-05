use std::collections::HashMap;
use serde::{Deserialize, Serialize};

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

pub async fn call_llm(
    description: &str,
    inventory: &HashMap<String, u32>,
) -> Result<Structure, String> {
    let base_url = std::env::var("OPENROUTER_BASE_URL")
        .map_err(|_| "OPENROUTER_BASE_URL not set".to_owned())?;
    let api_key = std::env::var("OPENROUTER_API_KEY")
        .map_err(|_| "OPENROUTER_API_KEY not set".to_owned())?;
    let model = std::env::var("OPENROUTER_MODEL")
        .map_err(|_| "OPENROUTER_MODEL not set".to_owned())?;

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
            ChatMessage { role: "system", content: system.to_owned() },
            ChatMessage { role: "user", content: user },
        ],
        response_format: ResponseFormat { kind: "json_object" },
    };

    let client = reqwest::Client::new();
    let url = format!("{}/chat/completions", base_url.trim_end_matches('/'));

    let response = client
        .post(&url)
        .bearer_auth(&api_key)
        .json(&request)
        .send()
        .await
        .map_err(|e| format!("request failed: {e}"))?;

    if !response.status().is_success() {
        let status = response.status();
        let body = response.text().await.unwrap_or_default();
        return Err(format!("HTTP {status}: {body}"));
    }

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

    serde_json::from_str::<Structure>(&content)
        .map_err(|e| format!("failed to parse structure JSON: {e}\nContent: {content}"))
}

#[cfg(test)]
mod tests {
    use super::*;

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
