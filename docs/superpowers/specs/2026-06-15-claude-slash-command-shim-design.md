# Claude Slash-Command Shim (`/usage` in chat) — Design

Date: 2026-06-15
Status: Approved (user said proceed to implementation)

## Goal

Make typing `/usage` in a chat (Inspector or any ACP client) return the Claude
subscription usage info as a normal assistant reply, instead of producing an
empty turn. This is the first of Claude's informational local slash commands to
be supported over the headless ACP path.

Background: Claude Code slash commands are typed `prompt` (work over ACP),
`local`, or `local-jsx` (terminal-UI-only). `/usage` is `local-jsx`, so the
headless Agent SDK never renders it; the text "/usage" reaches the model as a
literal message and yields an empty turn. We shim it in the Sandbox Agent proxy
layer.

## Scope

In scope (v1):
- Intercept `/usage` for the **Claude** agent in the ACP prompt flow; return a
  synthetic assistant turn with formatted usage info.
- Reuse the existing `claude_usage::fetch_claude_usage`.

Out of scope (v1, deferred):
- `/cost`, `/context` and other informational commands (need per-session
  `usage_update` accumulation — a separate stateful component).
- Interactive commands (`/login`, `/model`, `/theme`, ...) — not feasible headless.
- Non-Claude agents.

## Trigger / behavior

- Interception is transparent for the Claude agent: prompt text whose single text
  block equals `/usage` (trimmed) never reaches the model; everything else is
  forwarded unchanged.
- Only matches an exact `/usage` (no args in v1).
- Works in the Inspector and any ACP client, because the synthetic reply is
  injected into the same SSE stream the client already listens to.

## Architecture

```
Inspector chat: "/usage"
  -> POST /v1/acp/{sid}  {method:"session/prompt", prompt:[{text:"/usage"}], id:N}
  -> AcpProxyRuntime::post (inherent method)
       1. get_or_create_instance(sid)  -> existing Claude instance
       2. slash_commands::detect(instance.agent, &payload) = Some(Usage{session_id, request_id:N})
       3. resolve Anthropic OAuth token (spawn_blocking extract_all_credentials)
       4. text = fetch_claude_usage(token).await -> format_usage()   (graceful text on error)
       5. instance.runtime.inject_notification(session/update agent_message_chunk{text})
       6. return Response({jsonrpc, id:N, result:{stopReason:"end_turn"}})
  -> client renders the injected agent_message_chunk as an assistant message
```

Interception happens AFTER instance resolution (the real chat flow always runs
`initialize` + `session/new` first, so the instance and its SSE stream already
exist; we inject into that stream). `/usage` does not reach the agent subprocess.

### Components / files

- **New** `server/packages/sandbox-agent/src/slash_commands.rs`
  - `enum SlashCommand { Usage { session_id: String, request_id: Value } }`
  - `fn detect(agent: AgentId, payload: &Value) -> Option<SlashCommand>` (pure):
    matches when `agent == AgentId::Claude`, `payload["method"] == "session/prompt"`,
    and the prompt's single text block trims to exactly `/usage`. Extracts
    `params.sessionId` and the request `id`.
  - `fn format_usage(&ClaudeUsageResponse) -> String` -> markdown.
  - Helpers to build the synthetic `session/update` and the `session/prompt`
    response JSON.
- **Modify** `server/packages/acp-http-adapter/src/process.rs`
  - `pub async fn inject_notification(&self, payload: Value)` on `AdapterRuntime`:
    `let seq = self.sequence.fetch_add(1, SeqCst) + 1;` build `StreamMessage{sequence:seq, payload}`,
    push to `ring` (respecting the existing ring cap behavior), `sender.send(...)`.
    Mirrors the existing stdout-notification broadcast path (process.rs:449-463).
- **Modify** `server/packages/sandbox-agent/src/acp_proxy_runtime.rs`
  - In the inherent `post`, after instance resolution and before forwarding a
    `session/prompt`, call `slash_commands::detect`; if `Some`, run the handler
    (token + fetch + format + inject + synth response) and return
    `ProxyPostOutcome::Response(...)` without writing to the agent stdin.
- **Modify** `server/packages/sandbox-agent/src/lib.rs` — `mod slash_commands;`.

## Synthetic messages

Injected notification (rendered as assistant text):
```json
{ "jsonrpc": "2.0", "method": "session/update",
  "params": { "sessionId": "<sid>",
    "update": { "sessionUpdate": "agent_message_chunk",
                "content": { "type": "text", "text": "<markdown>" } } } }
```
POST response:
```json
{ "jsonrpc": "2.0", "id": <N>, "result": { "stopReason": "end_turn" } }
```

Formatted text (markdown), e.g.:
```
**Claude usage** — Reinforce-omega (claude_team · team_standard · active)

- **5-hour**: 38% used, 62% left (resets 2026-06-15T16:00:00+08:00)
- **Weekly**: 13% used, 87% left (resets 2026-06-18T11:00:00+08:00)
```

## Error handling (graceful, always a visible chat reply)

- No Anthropic credential, or credential not OAuth -> inject assistant text:
  "⚠️ /usage requires a Claude subscription (OAuth) login; no subscription
  credential is available." then `end_turn`.
- `fetch_claude_usage` error (expired/timeout/upstream) -> inject:
  "⚠️ Could not fetch usage: <reason>." then `end_turn`.
- `sender.send` with no subscribers is ignored (matches existing code); the ring
  is still updated so SSE replay shows the message. Sequence comes from the shared
  counter so ordering/replay stay consistent.

## Testing

- Unit (`slash_commands.rs`): `detect` returns Some for Claude + exact `/usage`;
  None for non-Claude agent, non-`session/prompt` method, other text
  (`"hello"`, `"/usagex"`, `"/cost"`), and extracts `sessionId`/`id` correctly.
  `format_usage` on a sample `ClaudeUsageResponse` contains the plan line and one
  line per window with used/left percentages.
- Unit (`process.rs`): `inject_notification` makes the payload visible via
  `subscribe(None)` (or the ring) with a monotonic sequence. Use the Mock agent /
  a constructed runtime; if constructing a runtime in a test is impractical, cover
  injection through the Mock agent path or document it as manually verified.
- Manual e2e: local server + Inspector, type `/usage` -> assistant reply renders.

## Notes / risks

- Transparent interception means an ACP client can no longer send the literal
  text "/usage" to the model for the Claude agent. This is acceptable: that text
  produced only an empty turn anyway. Documented behavior.
- `agent_message_chunk` is the same update shape Claude already emits for streamed
  text, so existing clients render it without changes (verified earlier when the
  real agent streamed `agent_message_chunk`).
