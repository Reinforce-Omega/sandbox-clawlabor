# Claude Subscription Usage Endpoint — Design

Date: 2026-06-15
Status: Approved (pending spec review)

## Goal

Expose Claude subscription usage information through Sandbox Agent, specifically
the rolling **5-hour** window and **weekly (7-day)** window remaining, plus
**plan** details (organization type, seat tier, subscription status). This is the
data the interactive Claude Code `/usage` command shows.

Motivation: `/usage` is a `local-jsx` (terminal-UI-only) command in Claude Code.
It is not exposed over the ACP / headless Agent SDK surface, so it cannot be
invoked through Sandbox Agent's normal session/prompt path. Instead we replicate
what `/usage` does internally: call Anthropic's OAuth account endpoints with the
subscription OAuth token and return a normalized result.

Not all agents have this concept, so the capability is gated to the Claude agent.

## Scope

In scope:
- New HTTP endpoint `GET /v1/agents/{agent}/usage`, capability-gated to Claude.
- New CLI subcommand `sandbox-agent api agents usage <agent>` (thin HTTP client).
- Normalized typed response (OpenAPI documented).

Out of scope (explicitly):
- Inspector UI panel.
- TypeScript SDK method.
- Usage for non-Claude agents (returns 501 until/if implemented).
- Restoring the interactive `/usage` panel over ACP (not possible — local-jsx).

## Data Source (reverse-engineered, verified working)

The Claude Code `/usage` panel calls these OAuth endpoints with the subscription
access token:

- `GET https://api.anthropic.com/api/oauth/usage`
- `GET https://api.anthropic.com/api/oauth/profile`

Required request headers:

```
Authorization: Bearer <oauth-access-token>
anthropic-beta: oauth-2025-04-20
Content-Type: application/json
```

Sample `/api/oauth/usage` response:

```json
{
  "five_hour": { "utilization": 38.0, "resets_at": "2026-06-15T08:00:00.449409+00:00" },
  "seven_day": { "utilization": 13.0, "resets_at": "2026-06-18T03:00:00.449431+00:00" },
  "seven_day_opus": null,
  "seven_day_sonnet": null,
  "extra_usage": { "is_enabled": false, "monthly_limit": null, "used_credits": null,
                   "utilization": null, "currency": null, "disabled_reason": null }
}
```

Sample `/api/oauth/profile` response (relevant fields):

```json
{
  "account": { "has_claude_max": false, "has_claude_pro": false },
  "organization": {
    "name": "Reinforce-omega",
    "organization_type": "claude_team",
    "seat_tier": "team_standard",
    "subscription_status": "active",
    "rate_limit_tier": "default_raven",
    "has_extra_usage_enabled": false
  }
}
```

The OAuth access token is obtained at runtime from the existing credential
extraction (`extract_all_credentials`), which sources the Anthropic OAuth token
from `~/.claude/.credentials.json`, `~/.claude-oauth-credentials.json`, the
`CLAUDE_CODE_OAUTH_TOKEN` env var, or `ANTHROPIC_AUTH_TOKEN` env var. Only
`AuthType::Oauth` credentials carry subscription usage; API-key credentials do
not.

## Architecture

```
CLI: sandbox-agent api agents usage <agent>  ──HTTP GET──┐
                                                          ▼
                       GET /v1/agents/{agent}/usage   (router.rs handler)
                                  1. AgentId::parse(agent); not Claude -> 501
                                  2. extract_all_credentials(); anthropic OAuth or error
                                  3. claude_usage::fetch_claude_usage(token, &client)
                                                          ▼
              tokio::join!( GET /api/oauth/usage , GET /api/oauth/profile )
                                  -> normalize -> ClaudeUsageResponse (JSON)
```

The core fetch + normalization logic lives in a new `claude_usage` module with no
knowledge of axum or routing, so it is unit-testable in isolation. The router
handler and (transitively, via the endpoint) the CLI are thin layers over it.

### Files

- **New** `server/packages/sandbox-agent/src/claude_usage.rs` — response structs,
  `UsageError`, async `fetch_claude_usage`, unit tests for normalization.
- **Modify** `server/packages/sandbox-agent/src/router.rs` — route registration,
  `get_v1_agent_usage` handler, `#[utoipa::path]` doc, ApiDoc paths + component
  schemas. Add a shared `reqwest::Client` field to `AppState`.
- **Modify** `server/packages/sandbox-agent/src/cli.rs` — `api agents usage`
  subcommand (thin HTTP client + pretty printer + `--json`).
- **Modify** `docs/openapi.json` (regenerate), `docs/cli.mdx` (new subcommand).

## Endpoint Contract

`GET /v1/agents/{agent}/usage` (requires auth, consistent with other `/v1`).

Response body `ClaudeUsageResponse`:

```jsonc
{
  "agent": "claude",
  "plan": {
    "organization_name": "Reinforce-omega",
    "organization_type": "claude_team",
    "seat_tier": "team_standard",
    "subscription_status": "active",
    "rate_limit_tier": "default_raven",
    "has_claude_max": false,
    "has_claude_pro": false,
    "extra_usage_enabled": false
  },
  "windows": [
    { "key": "five_hour", "utilization_pct": 38.0, "remaining_pct": 62.0,
      "resets_at": "2026-06-15T08:00:00.449409+00:00" },
    { "key": "seven_day", "utilization_pct": 13.0, "remaining_pct": 87.0,
      "resets_at": "2026-06-18T03:00:00.449431+00:00" }
  ]
}
```

- `windows` is an array, not fixed fields: any usage key whose value is an object
  containing a non-null numeric `utilization` becomes a window (so newly added
  windows like `seven_day_opus` flow through automatically). `extra_usage` and
  objects without `utilization` are skipped.
- `remaining_pct = 100 - utilization_pct`.
- `resets_at` is passed through verbatim (RFC3339 with timezone). Localization is
  a CLI presentation concern, not the server's.
- `plan` fields map directly from `/api/oauth/profile`; missing fields become
  null/false.

### Status codes

| Scenario | Status | problem+json detail |
|---|---|---|
| Success | 200 | — |
| `agent` is a valid agent but not Claude (e.g. codex) | 501 Not Implemented | "usage is not supported for agent '{agent}'" |
| Unknown agent id | 400 | "unsupported agent" |
| No Anthropic credentials available | 401 | "Authentication required" |
| Anthropic credential present but not OAuth (API key only) | 400 | "usage requires a Claude subscription (OAuth) credential" |
| Anthropic returns 401/403 (token expired/invalid) | 401 | "Claude subscription token expired or invalid" |
| Anthropic returns other non-2xx | 502 Bad Gateway | includes upstream status code |
| Request to Anthropic times out | 504 Gateway Timeout | "timed out contacting Anthropic" |

## Core Module Detail

```rust
pub struct ClaudeUsageResponse { pub agent: String, pub plan: PlanInfo, pub windows: Vec<UsageWindow> }
pub struct PlanInfo {
    pub organization_name: Option<String>,
    pub organization_type: Option<String>,
    pub seat_tier: Option<String>,
    pub subscription_status: Option<String>,
    pub rate_limit_tier: Option<String>,
    pub has_claude_max: bool,
    pub has_claude_pro: bool,
    pub extra_usage_enabled: bool,
}
pub struct UsageWindow { pub key: String, pub utilization_pct: f64, pub remaining_pct: f64, pub resets_at: Option<String> }

pub enum UsageError {
    NotSubscription,            // -> 400
    Unauthorized,               // -> 401 (upstream 401/403)
    Upstream { status: u16 },   // -> 502
    Timeout,                    // -> 504
    Transport(String),         // -> 502
    Parse(String),             // -> 502
}

pub async fn fetch_claude_usage(token: &str, client: &reqwest::Client)
    -> Result<ClaudeUsageResponse, UsageError>;
```

- Concurrent (`tokio::join!`) requests to `/api/oauth/usage` and `/api/oauth/profile`.
- 5-second timeout per request.
- Headers: `Authorization: Bearer <token>`, `anthropic-beta: oauth-2025-04-20`,
  `Content-Type: application/json`.
- Normalization iterates the usage JSON object's top-level keys; for each value
  that is an object with a numeric `utilization`, emit a `UsageWindow`.
- The token is never logged or included in any error body.

The router handler maps `UsageError` to `ProblemDetails` per the status table,
gates on `AgentId`, extracts credentials via `tokio::task::spawn_blocking`
(matching the existing `get_v1_agents` pattern), and uses a shared
`reqwest::Client` held on `AppState`.

## CLI Subcommand

`sandbox-agent api agents usage <agent> [--endpoint URL] [--token TOKEN] [--json]`

- Follows the existing `api` thin-client pattern (HTTP GET to the running server,
  not a direct Anthropic call).
- Default output: a formatted plan line plus per-window progress bars with
  localized reset times (mirrors the verified `claude-usage.sh` prototype).
- `--json`: prints the raw endpoint JSON for scripting.

## Testing

- **Unit tests** (`claude_usage.rs`): feed sample `usage`/`profile` JSON to the
  normalization function (factored to take parsed JSON, no network). Assert:
  windows array contents, `remaining_pct` computation, `extra_usage` skipped,
  windows with null/absent `utilization` ignored, plan field mapping.
- **Router tests** (`tests/v1_api.rs` style): `GET /v1/agents/codex/usage` -> 501;
  request with no Anthropic credentials -> 401.
- Live Anthropic calls are not exercised in CI (no token there); verified manually
  against the real endpoints during design (both endpoints confirmed working).

## Docs Sync (per repo CLAUDE.md)

- Regenerate `docs/openapi.json` after the contract is implemented.
- Update `docs/cli.mdx` with the new subcommand.
- Handler carries a full `#[utoipa::path]` summary + description; every response
  entry includes a description.

## Risks / Notes

- The `oauth-2025-04-20` beta header and the two endpoint paths are observed from
  the bundled Claude Code SDK; if Anthropic changes them, the endpoint returns
  502/401 with a clear detail rather than crashing.
- Expired OAuth tokens surface as 401 with a clear message so callers know to
  re-authenticate (the CLI/keychain refresh is the operator's responsibility).
