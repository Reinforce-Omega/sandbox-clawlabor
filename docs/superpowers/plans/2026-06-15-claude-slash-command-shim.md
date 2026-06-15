# Claude Slash-Command Shim (`/usage` in chat) — Implementation Plan

> Execute via superpowers:subagent-driven-development. NO COMMITS this session — verification checkpoints only; leave changes in the working tree on `main`.

**Goal:** Intercept `/usage` in the ACP prompt flow for the Claude agent and return a synthetic assistant turn with formatted subscription usage, rendered in the Inspector and any ACP client.

**Architecture:** A pure `slash_commands` module detects the command and formats output; `AdapterRuntime::inject_notification` pushes a synthetic `session/update` into the live SSE stream; `AcpProxyRuntime::post` hooks detection in before forwarding the prompt. Reuses `claude_usage::fetch_claude_usage`.

> Line numbers approximate — locate by symbol. cargo PATH: `export PATH="/Users/kun/Library/Caches/puccinialin/rustup/toolchains/stable-aarch64-apple-darwin/bin:$PATH"`.

---

## Task 1: `AdapterRuntime::inject_notification`

**Files:** Modify `server/packages/acp-http-adapter/src/process.rs`

- [ ] Add a public method on `AdapterRuntime` that injects a synthetic notification into the broadcast + ring, mirroring the existing stdout-notification path (around process.rs:449-463):

```rust
/// Inject a synthetic notification into the SSE stream (broadcast + replay ring),
/// as if it had come from the agent. Used by the proxy to answer locally-handled
/// slash commands.
pub async fn inject_notification(&self, payload: Value) {
    let seq = self.sequence.fetch_add(1, Ordering::SeqCst) + 1;
    let message = StreamMessage {
        sequence: seq,
        payload,
    };
    {
        let mut guard = self.ring.lock().await;
        guard.push_back(message.clone());
        while guard.len() > RING_BUFFER_SIZE {
            guard.pop_front();
        }
    }
    let _ = self.sender.send(message);
}
```

Match the EXACT ring-trim logic used elsewhere in this file (find how the stdout loop bounds the ring — use the same constant/expression, e.g. `RING_BUFFER_SIZE`). `StreamMessage` must derive/already be `Clone` (it holds `u64` + `Value`); if it is not `Clone`, add `#[derive(Clone)]` to it. Ensure `Value`, `Ordering` are in scope (they are, used above).

- [ ] Unit test (in the `#[cfg(test)]` module of process.rs if one exists; otherwise add one): construct or obtain an `AdapterRuntime` and assert an injected payload becomes visible. Prefer the Mock agent if the test harness already builds runtimes that way; otherwise, if constructing a runtime requires a live child process, instead write a minimal test that exercises the ring/broadcast logic, OR document that injection is covered by the manual e2e and skip a brittle unit test. Do not spawn real network. Report which path you took.

- [ ] Verify: `cargo build -p acp-http-adapter` and `cargo build -p sandbox-agent` compile.

---

## Task 2: `slash_commands` module (pure detect + format)

**Files:** Create `server/packages/sandbox-agent/src/slash_commands.rs`; modify `server/packages/sandbox-agent/src/lib.rs` (`mod slash_commands;`).

- [ ] Create the module:

```rust
use serde_json::{json, Value};
use sandbox_agent_agent_management::AgentId;
use crate::claude_usage::ClaudeUsageResponse;

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
    Some(SlashCommand::Usage { session_id, request_id })
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
    out.push_str("\n");
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
```

Confirm the real names: `AgentId` import path (check how `acp_proxy_runtime.rs`/`router.rs` import it — likely `sandbox_agent_agent_management::AgentId`), and `ClaudeUsageResponse`/`PlanInfo` field names (from `claude_usage.rs`: `organization_name`, `organization_type`, `seat_tier`, `subscription_status`, and windows `key`/`utilization_pct`/`remaining_pct`/`resets_at`). Adapt if any differ.

- [ ] Add `mod slash_commands;` to `lib.rs` near the other module declarations.

- [ ] Unit tests in the module:

```rust
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
        assert_eq!(got, Some(SlashCommand::Usage { session_id: "s1".into(), request_id: json!(7) }));
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
                UsageWindow { key: "five_hour".into(), utilization_pct: 38.0, remaining_pct: 62.0, resets_at: Some("2026-06-15T16:00:00+08:00".into()) },
                UsageWindow { key: "seven_day".into(), utilization_pct: 13.0, remaining_pct: 87.0, resets_at: None },
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
```

- [ ] Verify: `cargo test -p sandbox-agent --lib slash_commands` passes.

---

## Task 3: Hook into `AcpProxyRuntime::post`

**Files:** Modify `server/packages/sandbox-agent/src/acp_proxy_runtime.rs`.

- [ ] In the inherent `AcpProxyRuntime::post` (the one returning `ProxyPostOutcome`, called by the `AcpDispatch` impl), AFTER the instance has been resolved (`get_or_create_instance`) and BEFORE the payload is forwarded to the adapter, add interception:

```rust
// Locally handle informational slash commands (e.g. /usage) instead of
// forwarding them to the agent, which cannot render them headlessly.
if let Some(cmd) = crate::slash_commands::detect(instance.agent, &payload) {
    return self.handle_slash_command(&instance, cmd).await;
}
```

Use the actual local variable names present (the resolved instance and `payload`). `instance.agent` is the `AgentId` on `ProxyInstance` (confirm the field). The function returns `Result<ProxyPostOutcome, SandboxError>` (confirm the exact Ok/Err types and adapt).

- [ ] Add the handler method on `AcpProxyRuntime`:

```rust
async fn handle_slash_command(
    &self,
    instance: &ProxyInstance,
    cmd: crate::slash_commands::SlashCommand,
) -> Result<ProxyPostOutcome, SandboxError> {
    use crate::slash_commands::{assistant_message_update, end_turn_response, SlashCommand};
    let crate::slash_commands::SlashCommand::Usage { session_id, request_id } = cmd;

    let text = match self.resolve_anthropic_oauth_token().await {
        Some(token) => match crate::claude_usage::fetch_claude_usage(&token).await {
            Ok(usage) => crate::slash_commands::format_usage(&usage),
            Err(err) => format!("⚠️ Could not fetch usage: {}", describe_usage_error(&err)),
        },
        None => "⚠️ /usage requires a Claude subscription (OAuth) login; no subscription credential is available.".to_string(),
    };

    instance
        .runtime
        .inject_notification(assistant_message_update(&session_id, &text))
        .await;
    Ok(ProxyPostOutcome::Response(end_turn_response(&request_id)))
}
```

- [ ] Add a small helper to resolve the OAuth token (mirror the router's usage handler credential step), returning `Option<String>` (Some only when an Anthropic OAuth credential exists):

```rust
async fn resolve_anthropic_oauth_token(&self) -> Option<String> {
    use sandbox_agent_agent_credentials::{extract_all_credentials, AuthType, CredentialExtractionOptions};
    let creds = tokio::task::spawn_blocking(|| {
        extract_all_credentials(&CredentialExtractionOptions::new())
    })
    .await
    .ok()?;
    let cred = creds.anthropic?;
    if cred.auth_type == AuthType::Oauth {
        Some(cred.api_key)
    } else {
        None
    }
}
```

- [ ] Add `describe_usage_error` (free fn) mapping `claude_usage::UsageError` variants to short strings (Unauthorized -> "token expired or invalid"; Upstream{status} -> format!("Anthropic returned {status}"); Timeout -> "timed out"; Transport(m)/Parse(m) -> the message). Do NOT include the token.

Confirm against actual code: the import path for `extract_all_credentials`/`AuthType`/`CredentialExtractionOptions` (same crate the router uses), `ProxyInstance` field names (`agent`, `runtime`), and that `runtime` is the `AdapterRuntime` (so `.inject_notification(...)` resolves). Adapt to real signatures. If `ProxyInstance.runtime` is wrapped (e.g. `Arc<AdapterRuntime>`), the method call still works through the Arc.

- [ ] Verify: `cargo build -p sandbox-agent` compiles; `cargo test -p sandbox-agent --lib slash_commands` still passes.

---

## Task 4: Manual end-to-end (no automated integration test)

- [ ] Build the binary: `SANDBOX_AGENT_SKIP_INSPECTOR=1 cargo build -p sandbox-agent --bin sandbox-agent`.
- [ ] Run locally with the keychain OAuth token on port 2469 (reuse the established pattern), then drive a session via the ACP API: `initialize` -> `session/new` -> `session/prompt` with text `/usage`, while reading the SSE stream. Confirm an `agent_message_chunk` with the formatted usage text appears and the prompt response is `stopReason: end_turn`. (Controller will run this directly; not a cargo test.)

---

## Self-review (author)
- Spec coverage: detect (T2), format (T2), inject (T1), hook + token + error text (T3), manual e2e (T4). Covered.
- Type consistency: `SlashCommand::Usage { session_id, request_id }`, `detect`, `format_usage`, `assistant_message_update`, `end_turn_response`, `inject_notification` used identically across tasks.
- No commits; verification checkpoints only.
- Flagged uncertain real-code names inline (AgentId path, ProxyInstance fields, ProxyPostOutcome types, credential imports, ring-trim constant).
