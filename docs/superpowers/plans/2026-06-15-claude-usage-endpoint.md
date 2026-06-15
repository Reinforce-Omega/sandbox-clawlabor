# Claude Subscription Usage Endpoint Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Add `GET /v1/agents/{agent}/usage` (capability-gated to Claude) plus a thin CLI subcommand that surface the Claude subscription 5-hour / weekly windows and plan info.

**Architecture:** A pure `claude_usage` module fetches and normalizes Anthropic's `/api/oauth/usage` and `/api/oauth/profile` responses using the extracted OAuth token. The router handler gates on `AgentId::Claude`, extracts credentials, calls the module, and maps errors to problem+json. The CLI subcommand is a thin HTTP client over the endpoint.

**Tech Stack:** Rust, axum, reqwest (async), serde_json, utoipa, tokio.

> **NOTE — NO COMMITS THIS SESSION.** The user asked that nothing be committed. Each task ends with a verification checkpoint instead of a commit. Leave all changes in the working tree.

> **NOTE — line numbers are approximate.** Locate edit sites by the named symbol (function/struct), not the line number, since earlier edits shift lines.

---

## File Structure

- **Create** `server/packages/sandbox-agent/src/claude_usage.rs` — response structs, `UsageError`, pure `normalize_usage`, async `fetch_claude_usage`, shared `OnceLock<reqwest::Client>`, unit tests for normalization.
- **Modify** `server/packages/sandbox-agent/src/lib.rs` — declare `mod claude_usage;`.
- **Modify** `server/packages/sandbox-agent/src/router.rs` — `get_v1_agent_usage` handler, route registration, `#[utoipa::path]`, ApiDoc `paths(...)` + `components(schemas(...))`, error mapping.
- **Modify** `server/packages/sandbox-agent/src/cli.rs` — `api agents usage <agent>` subcommand (HTTP client + pretty printer + `--json`).
- **Modify** `docs/openapi.json` (regenerate) and `docs/cli.mdx` (document subcommand).

---

## Task 1: Core module — types and pure normalization (TDD)

**Files:**
- Create: `server/packages/sandbox-agent/src/claude_usage.rs`
- Modify: `server/packages/sandbox-agent/src/lib.rs` (add `mod claude_usage;`)

- [ ] **Step 1: Create the module file with types and a failing-to-compile normalize stub + tests**

Create `server/packages/sandbox-agent/src/claude_usage.rs`:

```rust
use serde::{Deserialize, Serialize};
use serde_json::Value;

#[derive(Debug, Clone, Serialize, Deserialize, schemars::JsonSchema, utoipa::ToSchema)]
pub struct ClaudeUsageResponse {
    pub agent: String,
    pub plan: PlanInfo,
    pub windows: Vec<UsageWindow>,
}

#[derive(Debug, Clone, Serialize, Deserialize, schemars::JsonSchema, utoipa::ToSchema)]
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

#[derive(Debug, Clone, Serialize, Deserialize, schemars::JsonSchema, utoipa::ToSchema)]
pub struct UsageWindow {
    pub key: String,
    pub utilization_pct: f64,
    pub remaining_pct: f64,
    pub resets_at: Option<String>,
}

/// Normalize raw `/api/oauth/usage` and `/api/oauth/profile` JSON into the
/// response shape. Any top-level key in `usage` whose value is an object with a
/// numeric `utilization` becomes a window; everything else is skipped.
pub fn normalize_usage(usage: &Value, profile: &Value) -> ClaudeUsageResponse {
    let mut windows = Vec::new();
    if let Some(obj) = usage.as_object() {
        for (key, val) in obj {
            let Some(inner) = val.as_object() else { continue };
            let Some(util) = inner.get("utilization").and_then(Value::as_f64) else {
                continue;
            };
            windows.push(UsageWindow {
                key: key.clone(),
                utilization_pct: util,
                remaining_pct: 100.0 - util,
                resets_at: inner
                    .get("resets_at")
                    .and_then(Value::as_str)
                    .map(str::to_string),
            });
        }
    }
    windows.sort_by(|a, b| a.key.cmp(&b.key));

    let account = profile.get("account");
    let org = profile.get("organization");
    let plan = PlanInfo {
        organization_name: str_field(org, "name"),
        organization_type: str_field(org, "organization_type"),
        seat_tier: str_field(org, "seat_tier"),
        subscription_status: str_field(org, "subscription_status"),
        rate_limit_tier: str_field(org, "rate_limit_tier"),
        has_claude_max: bool_field(account, "has_claude_max"),
        has_claude_pro: bool_field(account, "has_claude_pro"),
        extra_usage_enabled: bool_field(org, "has_extra_usage_enabled"),
    };

    ClaudeUsageResponse {
        agent: "claude".to_string(),
        plan,
        windows,
    }
}

fn str_field(parent: Option<&Value>, key: &str) -> Option<String> {
    parent?.get(key)?.as_str().map(str::to_string)
}

fn bool_field(parent: Option<&Value>, key: &str) -> bool {
    parent
        .and_then(|p| p.get(key))
        .and_then(Value::as_bool)
        .unwrap_or(false)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn sample_usage() -> Value {
        json!({
            "five_hour": { "utilization": 38.0, "resets_at": "2026-06-15T08:00:00+00:00" },
            "seven_day": { "utilization": 13.0, "resets_at": "2026-06-18T03:00:00+00:00" },
            "seven_day_opus": null,
            "extra_usage": { "is_enabled": false, "utilization": null }
        })
    }

    fn sample_profile() -> Value {
        json!({
            "account": { "has_claude_max": false, "has_claude_pro": false },
            "organization": {
                "name": "Reinforce-omega",
                "organization_type": "claude_team",
                "seat_tier": "team_standard",
                "subscription_status": "active",
                "rate_limit_tier": "default_raven",
                "has_extra_usage_enabled": false
            }
        })
    }

    #[test]
    fn normalizes_windows_and_skips_non_windows() {
        let r = normalize_usage(&sample_usage(), &sample_profile());
        // five_hour and seven_day only; seven_day_opus (null) and extra_usage
        // (utilization null) skipped.
        assert_eq!(r.windows.len(), 2);
        let five = r.windows.iter().find(|w| w.key == "five_hour").unwrap();
        assert_eq!(five.utilization_pct, 38.0);
        assert_eq!(five.remaining_pct, 62.0);
        assert_eq!(five.resets_at.as_deref(), Some("2026-06-15T08:00:00+00:00"));
        assert!(r.windows.iter().all(|w| w.key != "extra_usage"));
        assert!(r.windows.iter().all(|w| w.key != "seven_day_opus"));
    }

    #[test]
    fn maps_plan_fields() {
        let r = normalize_usage(&sample_usage(), &sample_profile());
        assert_eq!(r.agent, "claude");
        assert_eq!(r.plan.organization_type.as_deref(), Some("claude_team"));
        assert_eq!(r.plan.seat_tier.as_deref(), Some("team_standard"));
        assert_eq!(r.plan.subscription_status.as_deref(), Some("active"));
        assert!(!r.plan.has_claude_max);
        assert!(!r.plan.extra_usage_enabled);
    }

    #[test]
    fn tolerates_missing_profile_fields() {
        let r = normalize_usage(&sample_usage(), &json!({}));
        assert!(r.plan.organization_type.is_none());
        assert!(!r.plan.has_claude_pro);
        assert_eq!(r.windows.len(), 2);
    }
}
```

Add to `server/packages/sandbox-agent/src/lib.rs` near the other `mod` declarations (e.g. after `mod acp_proxy_runtime;` / wherever modules are declared):

```rust
mod claude_usage;
```

- [ ] **Step 2: Run the tests to verify they pass**

Run: `cargo test -p sandbox-agent --lib claude_usage`
Expected: PASS — `normalizes_windows_and_skips_non_windows`, `maps_plan_fields`, `tolerates_missing_profile_fields` all green.

(Note: `schemars`/`utoipa`/`serde` are already workspace dependencies of this crate — confirm by checking that other structs in `router/types.rs` derive `JsonSchema`/`ToSchema`. If `serde_json::json!` is unavailable in tests, `serde_json` is already a dependency.)

- [ ] **Step 3: Checkpoint (no commit)**

Run: `cargo build -p sandbox-agent`
Expected: compiles. Leave changes uncommitted.

---

## Task 2: Core module — async fetch wrapper

**Files:**
- Modify: `server/packages/sandbox-agent/src/claude_usage.rs`

- [ ] **Step 1: Add the error type, shared client, and fetch function**

Append to `server/packages/sandbox-agent/src/claude_usage.rs` (above the `#[cfg(test)]` module):

```rust
use std::sync::OnceLock;
use std::time::Duration;

const USAGE_URL: &str = "https://api.anthropic.com/api/oauth/usage";
const PROFILE_URL: &str = "https://api.anthropic.com/api/oauth/profile";
const OAUTH_BETA: &str = "oauth-2025-04-20";

#[derive(Debug)]
pub enum UsageError {
    /// Upstream rejected the token (401/403): expired or invalid.
    Unauthorized,
    /// Upstream returned another non-2xx status.
    Upstream { status: u16 },
    /// Request timed out.
    Timeout,
    /// Transport/connection failure.
    Transport(String),
    /// Response body could not be parsed as JSON.
    Parse(String),
}

fn client() -> &'static reqwest::Client {
    static CLIENT: OnceLock<reqwest::Client> = OnceLock::new();
    CLIENT.get_or_init(|| {
        reqwest::Client::builder()
            .timeout(Duration::from_secs(5))
            .build()
            .expect("failed to build reqwest client")
    })
}

async fn get_json(token: &str, url: &str) -> Result<Value, UsageError> {
    let resp = client()
        .get(url)
        .header("Authorization", format!("Bearer {token}"))
        .header("anthropic-beta", OAUTH_BETA)
        .header("Content-Type", "application/json")
        .send()
        .await
        .map_err(|err| {
            if err.is_timeout() {
                UsageError::Timeout
            } else {
                UsageError::Transport(err.to_string())
            }
        })?;

    let status = resp.status();
    if status == reqwest::StatusCode::UNAUTHORIZED || status == reqwest::StatusCode::FORBIDDEN {
        return Err(UsageError::Unauthorized);
    }
    if !status.is_success() {
        return Err(UsageError::Upstream {
            status: status.as_u16(),
        });
    }
    resp.json::<Value>()
        .await
        .map_err(|err| UsageError::Parse(err.to_string()))
}

/// Fetch and normalize Claude subscription usage + plan using an OAuth token.
pub async fn fetch_claude_usage(token: &str) -> Result<ClaudeUsageResponse, UsageError> {
    let (usage, profile) = tokio::join!(get_json(token, USAGE_URL), get_json(token, PROFILE_URL));
    Ok(normalize_usage(&usage?, &profile?))
}
```

- [ ] **Step 2: Verify it compiles**

Run: `cargo build -p sandbox-agent`
Expected: compiles cleanly. (`reqwest` is already a dependency — confirm it is present in `server/packages/sandbox-agent/Cargo.toml`; it is used in `daemon.rs`. If `daemon.rs` only enables the `blocking` feature, ensure the default async client + `json` feature are enabled in Cargo.toml; add `features = ["json"]` to the reqwest dependency if missing.)

- [ ] **Step 3: Checkpoint (no commit)**

Leave changes uncommitted.

---

## Task 3: Router handler + route + error mapping (TDD)

**Files:**
- Modify: `server/packages/sandbox-agent/src/router.rs`
- Test: `server/packages/sandbox-agent/tests/v1_api.rs`

- [ ] **Step 1: Write failing router tests**

Add to `server/packages/sandbox-agent/tests/v1_api.rs` (follow the existing test harness in that file for building the app / sending requests — reuse its helper that spins up the router and its auth-token handling). Mirror the style of existing `/v1/agents` tests:

```rust
#[tokio::test]
async fn usage_returns_501_for_non_claude_agent() {
    let app = test_app().await; // use existing helper that builds the router
    let resp = app
        .get("/v1/agents/codex/usage") // use the existing request helper + auth
        .await;
    assert_eq!(resp.status(), 501);
}

#[tokio::test]
async fn usage_returns_400_for_unknown_agent() {
    let app = test_app().await;
    let resp = app.get("/v1/agents/bogus/usage").await;
    assert_eq!(resp.status(), 400);
}
```

(If the existing tests use a different harness signature, adapt these to match — the assertions on status code 501/400 are the point. Do NOT assert against live Anthropic; the 200 path needs a real token and is verified manually.)

- [ ] **Step 2: Run to verify they fail**

Run: `cargo test -p sandbox-agent --test v1_api usage_returns`
Expected: FAIL — route not found (currently 404/405).

- [ ] **Step 3: Add the response import and handler in `router.rs`**

Near the top of `router.rs` with other `use` statements:

```rust
use crate::claude_usage::{self, ClaudeUsageResponse, UsageError};
use sandbox_agent_agent_credentials::AuthType;
```

(Confirm `extract_all_credentials` and `CredentialExtractionOptions` are already imported in `router.rs` — they are used by `get_v1_agents`. Reuse those imports.)

Add the handler near `get_v1_agent` (the `/v1/agents/{agent}` handler):

```rust
/// Get Claude subscription usage.
///
/// Returns the rolling 5-hour and weekly usage windows plus plan information for
/// the Claude agent. Only the Claude agent supports usage; other agents return
/// 501.
#[utoipa::path(
    get,
    path = "/v1/agents/{agent}/usage",
    tag = "v1",
    params(
        ("agent" = String, Path, description = "Agent id (only `claude` is supported)")
    ),
    responses(
        (status = 200, description = "Claude subscription usage and plan info", body = ClaudeUsageResponse),
        (status = 400, description = "Unknown agent, or credential is not a subscription (OAuth) credential", body = ProblemDetails),
        (status = 401, description = "Authentication required, or subscription token expired/invalid", body = ProblemDetails),
        (status = 501, description = "Usage is not supported for this agent", body = ProblemDetails),
        (status = 502, description = "Upstream error contacting Anthropic", body = ProblemDetails),
        (status = 504, description = "Timed out contacting Anthropic", body = ProblemDetails)
    )
)]
async fn get_v1_agent_usage(
    State(_state): State<Arc<AppState>>,
    Path(agent): Path<String>,
) -> Result<Json<ClaudeUsageResponse>, ApiError> {
    let agent_id = AgentId::parse(&agent).ok_or_else(|| SandboxError::UnsupportedAgent {
        agent: agent.clone(),
    })?;

    if agent_id != AgentId::Claude {
        return Err(ApiError::Problem(ProblemDetails {
            type_: "urn:sandbox-agent:error:usage_unsupported".to_string(),
            title: "Usage Not Supported".to_string(),
            status: 501,
            detail: Some(format!("usage is not supported for agent '{agent}'")),
            instance: None,
            extensions: Default::default(),
        }));
    }

    let credentials = tokio::task::spawn_blocking(move || {
        extract_all_credentials(&CredentialExtractionOptions::new())
    })
    .await
    .map_err(|err| SandboxError::StreamError {
        message: format!("failed to resolve credentials: {err}"),
    })?;

    let cred = credentials
        .anthropic
        .ok_or_else(|| usage_problem(401, "Authentication required",
            "no Anthropic credentials available"))?;

    if cred.auth_type != AuthType::Oauth {
        return Err(usage_problem(400, "Subscription Required",
            "usage requires a Claude subscription (OAuth) credential").into());
    }

    match claude_usage::fetch_claude_usage(&cred.api_key).await {
        Ok(usage) => Ok(Json(usage)),
        Err(err) => Err(map_usage_error(err).into()),
    }
}

fn usage_problem(status: u16, title: &str, detail: &str) -> ProblemDetails {
    ProblemDetails {
        type_: "urn:sandbox-agent:error:usage".to_string(),
        title: title.to_string(),
        status,
        detail: Some(detail.to_string()),
        instance: None,
        extensions: Default::default(),
    }
}

fn map_usage_error(err: UsageError) -> ProblemDetails {
    match err {
        UsageError::Unauthorized => usage_problem(401, "Token Expired",
            "Claude subscription token expired or invalid"),
        UsageError::Upstream { status } => usage_problem(502, "Upstream Error",
            &format!("Anthropic returned status {status}")),
        UsageError::Timeout => usage_problem(504, "Upstream Timeout",
            "timed out contacting Anthropic"),
        UsageError::Transport(msg) => usage_problem(502, "Upstream Error",
            &format!("failed to contact Anthropic: {msg}")),
        UsageError::Parse(msg) => usage_problem(502, "Upstream Error",
            &format!("failed to parse Anthropic response: {msg}")),
    }
}
```

Notes for the implementer:
- `ProblemDetails` field names: confirm against `server/packages/error/src/lib.rs` — the field is `type_` (serde-renamed to `type`), plus `title`, `status`, `detail`, `instance`, `extensions`. Match exactly.
- `usage_problem(...).into()` relies on `ApiError: From<ProblemDetails>`. If `ApiError` does not implement `From<ProblemDetails>`, wrap explicitly with `ApiError::Problem(...)`. Check the `ApiError` enum; the helper returns `ProblemDetails`, and the 401-credentials line uses `?` so it must convert — if no `From` impl exists, change those sites to `ApiError::Problem(usage_problem(...))`.
- `usage_problem` returns `ProblemDetails`; the `.ok_or_else(...)?` for the 401 case must yield something `?`-convertible to `ApiError`. If only `From<SandboxError>` exists, instead construct `ApiError::Problem(usage_problem(401, ...))` directly (do not use `?` there). Keep all three error paths consistent with the actual `ApiError` conversions present in the file.

- [ ] **Step 4: Register the route**

Find the agents route registrations (near `.route("/agents/{agent}", get(get_v1_agent))`). Add:

```rust
.route("/agents/{agent}/usage", get(get_v1_agent_usage))
```

(Match the path-param syntax used by the surrounding routes — axum 0.7 uses `:agent`, axum 0.8 uses `{agent}`. Use whatever the neighboring `/agents/{agent}` route uses.)

- [ ] **Step 5: Register in OpenAPI ApiDoc**

In the `#[derive(OpenApi)] #[openapi(...)]` block (the `ApiDoc` struct):
- Add `get_v1_agent_usage` to the `paths(...)` list.
- Add `claude_usage::ClaudeUsageResponse`, `claude_usage::PlanInfo`, `claude_usage::UsageWindow` to `components(schemas(...))`.

- [ ] **Step 6: Run router tests to verify they pass**

Run: `cargo test -p sandbox-agent --test v1_api usage_returns`
Expected: PASS — 501 for codex, 400 for unknown agent.

- [ ] **Step 7: Full build + clippy checkpoint (no commit)**

Run: `cargo build -p sandbox-agent && cargo clippy -p sandbox-agent`
Expected: compiles, no new warnings. Leave uncommitted.

---

## Task 4: CLI subcommand `api agents usage`

**Files:**
- Modify: `server/packages/sandbox-agent/src/cli.rs`

- [ ] **Step 1: Locate the `api` subcommand structure**

In `cli.rs`, find the clap enum for `api` subcommands (where `sessions create`, `sessions send-message`, etc. are defined) and the agents-related ones if present. Identify the shared helpers for: building the endpoint base URL, attaching the bearer token, and the blocking HTTP client (`reqwest::blocking`) used by existing `api` commands.

- [ ] **Step 2: Add the `agents usage` subcommand variant + args**

Add a subcommand mirroring an existing `api` command's arg struct (with `--endpoint`, `--token`, and add `--json`). Example arg struct:

```rust
#[derive(Debug, clap::Args)]
pub struct AgentsUsageArgs {
    /// Agent id (only `claude` is supported)
    pub agent: String,
    #[arg(long)]
    pub endpoint: Option<String>,
    #[arg(long)]
    pub token: Option<String>,
    /// Print raw JSON instead of formatted output
    #[arg(long)]
    pub json: bool,
}
```

Wire it into the `api` command dispatch the same way existing commands are routed.

- [ ] **Step 3: Implement the handler (thin HTTP client + pretty printer)**

```rust
fn run_agents_usage(args: &AgentsUsageArgs, ctx: &ApiContext) -> Result<(), CliError> {
    // Reuse the existing helper that builds "{base}/v1/agents/{agent}/usage" and
    // performs an authenticated GET, matching other `api` commands in this file.
    let path = format!("/v1/agents/{}/usage", args.agent);
    let resp = ctx.get(&path)?;            // use the same accessor other api cmds use
    let status = resp.status();
    let text = resp.text()?;
    if !status.is_success() {
        print_error_body(&text)?;          // reuse existing error printer
        return Err(CliError::HttpStatus(status));
    }
    if args.json {
        write_stdout_line(&text)?;
        return Ok(());
    }
    let v: serde_json::Value = serde_json::from_str(&text)
        .map_err(|e| CliError::Server(format!("invalid response: {e}")))?;
    print_usage_pretty(&v)?;
    Ok(())
}

fn print_usage_pretty(v: &serde_json::Value) -> Result<(), CliError> {
    let plan = &v["plan"];
    write_stdout_line(&format!(
        "Plan: {} ({}, seat {}) — {}",
        plan["organization_name"].as_str().unwrap_or("?"),
        plan["organization_type"].as_str().unwrap_or("?"),
        plan["seat_tier"].as_str().unwrap_or("?"),
        plan["subscription_status"].as_str().unwrap_or("?"),
    ))?;
    if let Some(windows) = v["windows"].as_array() {
        for w in windows {
            let key = w["key"].as_str().unwrap_or("?");
            let used = w["utilization_pct"].as_f64().unwrap_or(0.0);
            let remaining = w["remaining_pct"].as_f64().unwrap_or(0.0);
            let filled = ((used / 5.0).round() as usize).min(20);
            let bar: String = "#".repeat(filled) + &"-".repeat(20 - filled);
            let resets = w["resets_at"].as_str().unwrap_or("");
            write_stdout_line(&format!(
                "  {key:10} used {used:5.1}%  left {remaining:5.1}%  [{bar}]  resets: {resets}"
            ))?;
        }
    }
    Ok(())
}
```

Notes:
- Use the SAME context/accessor pattern existing `api` commands use (the snippet assumes an `ApiContext` with a `get(path)` method and helpers `write_stdout_line`, `print_error_body`, `CliError::HttpStatus`, `CliError::Server` — verify these exact names in `cli.rs` and adapt). The `credentials extract-env` command (`run_credentials`) is a good reference for `write_stdout_line`.
- Localization of `resets_at` to local time is optional; keep the RFC3339 string for v1 to avoid pulling timezone deps. (`time` is available if desired, but YAGNI — leave raw.)

- [ ] **Step 4: Build + manual smoke check**

Run: `cargo build -p sandbox-agent`
Expected: compiles.

Manual (requires the running container with a valid token — optional, not CI):
```bash
sandbox-agent api agents usage claude --endpoint http://127.0.0.1:2468 --no-token
sandbox-agent api agents usage codex --endpoint http://127.0.0.1:2468 --no-token   # expect 501
```

- [ ] **Step 5: Checkpoint (no commit)**

Leave uncommitted.

---

## Task 5: Docs sync

**Files:**
- Modify: `docs/openapi.json`
- Modify: `docs/cli.mdx`

- [ ] **Step 1: Regenerate the OpenAPI spec**

The repo generates `docs/openapi.json` from the utoipa `ApiDoc`. Find the generator (look for `openapi-gen` crate / a `just`/script target — e.g. `cargo run -p openapi-gen` or a `justfile` recipe). Run it and confirm the new path `/v1/agents/{agent}/usage` and the `ClaudeUsageResponse`/`PlanInfo`/`UsageWindow` schemas appear.

Run: locate and run the generator (e.g. `just openapi` or `cargo run -p sandbox-agent-openapi-gen`); verify diff in `docs/openapi.json`.
Expected: new endpoint + schemas present.

- [ ] **Step 2: Document the CLI subcommand**

In `docs/cli.mdx`, add a section for `api agents usage <agent>` next to the other `api` commands, with the `--endpoint`/`--token`/`--json` flags and a short example. Follow the existing doc style. Do NOT mention ACP or protocol method names (repo CLAUDE.md rule).

- [ ] **Step 3: Final checkpoint (no commit)**

Run: `cargo test -p sandbox-agent --lib claude_usage && cargo test -p sandbox-agent --test v1_api usage_returns && cargo build -p sandbox-agent`
Expected: all green. Leave everything uncommitted (per user instruction).

---

## Self-Review Notes (author)

- **Spec coverage:** endpoint contract (Task 3), normalized schema incl. windows array + plan (Task 1), capability gating 501 (Task 3), status table incl. 400/401/502/504 (Task 3 `map_usage_error`/`usage_problem`), core module isolation + unit tests (Tasks 1-2), CLI thin client + `--json` (Task 4), docs sync (Task 5). All covered.
- **Deviation from spec:** shared `reqwest::Client` lives in a module-level `OnceLock` in `claude_usage.rs` instead of an `AppState` field, to avoid touching every `AppState` construction site. Same outcome (one shared client), smaller blast radius, matches the repo's existing `OnceLock` pattern.
- **Type consistency:** `ClaudeUsageResponse`/`PlanInfo`/`UsageWindow`/`UsageError` names and fields used identically across Tasks 1-4.
- **Open verification points flagged inline:** `ApiError`'s `From` impls, `ProblemDetails` field names, reqwest `json` feature, axum path-param syntax, exact `cli.rs` helper names — each task tells the implementer to confirm against the actual code before relying on the snippet.
