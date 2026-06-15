use crate::claude_usage::ClaudeUsageResponse;
use sandbox_agent_agent_management::agents::AgentId;
use serde_json::{json, Value};

/// A locally-handled informational slash command detected in a session/prompt.
#[derive(Debug, Clone, PartialEq)]
pub enum SlashCommand {
    Usage { session_id: String, request_id: Value },
}

/// Detect a supported informational slash command in an incoming ACP payload.
/// Returns None unless the agent is Claude, the method is `session/prompt`, and
/// the prompt is a single text block equal to `/usage` (trimmed).
pub fn detect(agent: AgentId, payload: &Value) -> Option<SlashCommand> {
    if agent != AgentId::Claude {
        return None;
    }
    if payload.get("method").and_then(Value::as_str) != Some("session/prompt") {
        return None;
    }
    let params = payload.get("params")?;
    let session_id = params.get("sessionId").and_then(Value::as_str)?.to_string();
    let prompt = params.get("prompt").and_then(Value::as_array)?;
    if prompt.len() != 1 {
        return None;
    }
    let block = &prompt[0];
    if block.get("type").and_then(Value::as_str) != Some("text") {
        return None;
    }
    let text = block.get("text").and_then(Value::as_str)?.trim();
    if text != "/usage" {
        return None;
    }
    let request_id = payload.get("id").cloned().unwrap_or(Value::Null);
    Some(SlashCommand::Usage {
        session_id,
        request_id,
    })
}

/// Build the synthetic `session/update` notification carrying assistant text.
pub fn assistant_message_update(session_id: &str, text: &str) -> Value {
    json!({
        "jsonrpc": "2.0",
        "method": "session/update",
        "params": {
            "sessionId": session_id,
            "update": {
                "sessionUpdate": "agent_message_chunk",
                "content": { "type": "text", "text": text }
            }
        }
    })
}

/// Build the synthetic `session/prompt` response that ends the turn.
pub fn end_turn_response(request_id: &Value) -> Value {
    json!({
        "jsonrpc": "2.0",
        "id": request_id,
        "result": { "stopReason": "end_turn" }
    })
}

/// Format a usage response as markdown for display in chat.
pub fn format_usage(usage: &ClaudeUsageResponse) -> String {
    let p = &usage.plan;
    let plan_bits: Vec<String> = [
        p.organization_type.clone(),
        p.seat_tier.clone(),
        p.subscription_status.clone(),
    ]
    .into_iter()
    .flatten()
    .collect();
    let header = match &p.organization_name {
        Some(name) => format!("**Claude usage** — {name} ({})", plan_bits.join(" · ")),
        None => format!("**Claude usage** ({})", plan_bits.join(" · ")),
    };
    let mut out = String::from(&header);
    out.push('\n');
    for w in &usage.windows {
        let label = match w.key.as_str() {
            "five_hour" => "5-hour",
            "seven_day" => "Weekly",
            other => other,
        };
        let resets = w
            .resets_at
            .as_deref()
            .map(|r| format!(" (resets {r})"))
            .unwrap_or_default();
        out.push_str(&format!(
            "\n- **{label}**: {:.0}% used, {:.0}% left{resets}",
            w.utilization_pct, w.remaining_pct
        ));
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::claude_usage::{ClaudeUsageResponse, PlanInfo, UsageWindow};

    fn prompt_payload(agent_text: &str) -> Value {
        json!({
            "jsonrpc":"2.0","id":7,"method":"session/prompt",
            "params":{"sessionId":"s1","prompt":[{"type":"text","text":agent_text}]}
        })
    }

    #[test]
    fn detects_usage_for_claude() {
        let got = detect(AgentId::Claude, &prompt_payload("/usage"));
        assert_eq!(
            got,
            Some(SlashCommand::Usage {
                session_id: "s1".into(),
                request_id: json!(7)
            })
        );
    }

    #[test]
    fn trims_whitespace() {
        assert!(detect(AgentId::Claude, &prompt_payload("  /usage  ")).is_some());
    }

    #[test]
    fn ignores_non_claude() {
        assert!(detect(AgentId::Codex, &prompt_payload("/usage")).is_none());
    }

    #[test]
    fn ignores_other_text() {
        assert!(detect(AgentId::Claude, &prompt_payload("hello")).is_none());
        assert!(detect(AgentId::Claude, &prompt_payload("/usagex")).is_none());
        assert!(detect(AgentId::Claude, &prompt_payload("/cost")).is_none());
    }

    #[test]
    fn ignores_non_prompt_method() {
        let p = json!({"method":"session/new","params":{"sessionId":"s1","prompt":[{"type":"text","text":"/usage"}]}});
        assert!(detect(AgentId::Claude, &p).is_none());
    }

    #[test]
    fn ignores_multi_block_prompt() {
        let p = json!({
            "method":"session/prompt",
            "params":{"sessionId":"s1","prompt":[
                {"type":"text","text":"/usage"},
                {"type":"text","text":"extra"}
            ]}
        });
        assert!(detect(AgentId::Claude, &p).is_none());
    }

    #[test]
    fn request_id_defaults_to_null_when_absent() {
        let p = json!({
            "method":"session/prompt",
            "params":{"sessionId":"s1","prompt":[{"type":"text","text":"/usage"}]}
        });
        assert_eq!(
            detect(AgentId::Claude, &p),
            Some(SlashCommand::Usage {
                session_id: "s1".into(),
                request_id: Value::Null
            })
        );
    }

    #[test]
    fn assistant_message_update_shape() {
        let v = assistant_message_update("s1", "hi");
        assert_eq!(v["method"], "session/update");
        assert_eq!(v["params"]["sessionId"], "s1");
        assert_eq!(v["params"]["update"]["sessionUpdate"], "agent_message_chunk");
        assert_eq!(v["params"]["update"]["content"]["type"], "text");
        assert_eq!(v["params"]["update"]["content"]["text"], "hi");
    }

    #[test]
    fn end_turn_response_shape() {
        let v = end_turn_response(&json!(7));
        assert_eq!(v["id"], json!(7));
        assert_eq!(v["result"]["stopReason"], "end_turn");
    }

    #[test]
    fn formats_usage_markdown() {
        let usage = ClaudeUsageResponse {
            agent: "claude".into(),
            plan: PlanInfo {
                organization_name: Some("Reinforce-omega".into()),
                organization_type: Some("claude_team".into()),
                seat_tier: Some("team_standard".into()),
                subscription_status: Some("active".into()),
                rate_limit_tier: None,
                has_claude_max: false,
                has_claude_pro: false,
                extra_usage_enabled: false,
            },
            windows: vec![
                UsageWindow {
                    key: "five_hour".into(),
                    utilization_pct: 38.0,
                    remaining_pct: 62.0,
                    resets_at: Some("2026-06-15T16:00:00+08:00".into()),
                },
                UsageWindow {
                    key: "seven_day".into(),
                    utilization_pct: 13.0,
                    remaining_pct: 87.0,
                    resets_at: None,
                },
            ],
        };
        let md = format_usage(&usage);
        assert!(md.contains("Reinforce-omega"));
        assert!(md.contains("claude_team"));
        assert!(md.contains("5-hour"));
        assert!(md.contains("62% left"));
        assert!(md.contains("Weekly"));
    }
}
