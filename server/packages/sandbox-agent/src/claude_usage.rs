use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use utoipa::ToSchema;

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct ClaudeUsageResponse {
    pub agent: String,
    pub plan: PlanInfo,
    pub windows: Vec<UsageWindow>,
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct PlanInfo {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub organization_name: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub organization_type: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub seat_tier: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub subscription_status: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub rate_limit_tier: Option<String>,
    pub has_claude_max: bool,
    pub has_claude_pro: bool,
    pub extra_usage_enabled: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct UsageWindow {
    pub key: String,
    pub utilization_pct: f64,
    pub remaining_pct: f64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub resets_at: Option<String>,
}

/// Normalize raw `/api/oauth/usage` and `/api/oauth/profile` JSON into the
/// response shape. Any top-level key in `usage` whose value is an object with a
/// numeric `utilization` becomes a window; everything else is skipped.
/// A `utilization` of `null` (an upstream "not applicable" sentinel, e.g. `extra_usage`) is therefore skipped.
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

    #[test]
    fn serializes_camel_case_and_omits_absent_optionals() {
        let r = normalize_usage(&sample_usage(), &sample_profile());
        let v = serde_json::to_value(&r).unwrap();
        // camelCase keys present
        assert!(v["windows"][0].get("utilizationPct").is_some());
        assert!(v["windows"][0].get("remainingPct").is_some());
        assert!(v["plan"].get("organizationType").is_some());
        assert!(v["plan"].get("hasClaudeMax").is_some());
        // snake_case keys absent
        assert!(v["windows"][0].get("utilization_pct").is_none());
        // absent optional omitted: build a result with no resets_at
        let usage = serde_json::json!({ "five_hour": { "utilization": 10.0 } });
        let r2 = normalize_usage(&usage, &serde_json::json!({}));
        let v2 = serde_json::to_value(&r2).unwrap();
        assert!(v2["windows"][0].get("resetsAt").is_none());
        assert!(v2["plan"].get("organizationName").is_none());
    }
}
