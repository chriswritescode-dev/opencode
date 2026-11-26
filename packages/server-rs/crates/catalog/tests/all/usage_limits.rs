use std::sync::Arc;

use opencode_catalog::credential::CreateInput;
use opencode_catalog::provider_plugin::http::PreparedHttp;
use opencode_catalog::usage_limits::UsageLimits;
use opencode_contract::codec::JsonMap;
use opencode_contract::generated::{
    CredentialOAuth, CredentialOAuthType, CredentialValue, IntegrationID, IntegrationMethodID, MoneyUSD,
    MoneyUSDPerMillionTokens, ProviderID, UsageLimitsBudget, UsageLimitsBudgetCoverage, UsageLimitsBudgetModel,
    UsageLimitsBudgetPer, UsageLimitsBudgetScope, UsageLimitsGo, UsageLimitsInfo, UsageLimitsInfoWorkspace,
    UsageLimitsMeter,
};
use serde_json::json;

use crate::provider_plugin::support::Routes;

const BUDGETS: &str = "https://dev.opencode.ai/console/api/policies/current-budgets";
const GO: &str = "https://dev.opencode.ai/zen/go/v1/usage";

fn signed_in() -> CredentialValue {
    CredentialValue::Oauth(CredentialOAuth {
        r#type: CredentialOAuthType::Oauth,
        method_id: IntegrationMethodID("device".to_owned()),
        refresh: "console-refresh".to_owned(),
        access: "console-access".to_owned(),
        expires: 1.0,
        metadata: Some(JsonMap(vec![
            ("server".to_owned(), json!("https://dev.opencode.ai/console")),
            ("orgID".to_owned(), json!("wrk_anomaly")),
            ("orgName".to_owned(), json!("Anomaly")),
        ])),
    })
}

fn read(credential: Option<CredentialValue>) -> (crate::TempDir, Arc<Routes>, UsageLimits) {
    let (directory, _database, _bus, credentials, integrations) = crate::integration::setup();
    if let Some(value) = credential {
        credentials
            .create(CreateInput {
                id: None,
                integration_id: IntegrationID("opencode".to_owned()),
                value,
                label: None,
                activate: None,
            })
            .expect("seed the OpenCode Console credential");
    }
    let routes = Routes::new();
    let limits = UsageLimits::new(integrations, routes.clone());
    (directory, routes, limits)
}

fn header(request: &PreparedHttp, name: &str) -> Option<String> {
    request.headers.iter().find(|(key, _)| key == name).map(|(_, value)| value.clone())
}

fn reset() -> f64 {
    opencode_util::time::parse_rfc3339("2026-10-24T17:00:00.000Z").expect("a valid reset date") as f64
}

#[tokio::test]
async fn reads_the_members_go_allowance_and_budgets_from_the_console_they_signed_in_to() {
    let (_directory, routes, limits) = read(Some(signed_in()));
    routes.route_json(
        "GET",
        BUDGETS,
        json!([{
            "policyID": "policy_engineering",
            "ruleID": "rule_astra",
            "limitDollars": 20,
            "spentDollars": 15,
            "scope": "each",
            "per": "model",
            "modelCount": 1,
            "coverage": "models",
            "models": [{
                "providerID": "opencode",
                "connectionID": "conn_managed",
                "modelID": "gpt-6-astra",
                "name": "GPT-6 Astra",
                "spentDollars": 15,
            }],
            "vendors": [{ "id": "openai", "name": "OpenAI" }],
            "inputPriceAtLeast": 3,
            "picked": false,
            "shared": false,
            "resetsAt": "2026-11-01T00:00:00.000Z",
        }]),
    );
    routes.route_json(
        "GET",
        GO,
        json!({
            "usage": {
                "rolling": { "status": "ok", "percent": 42, "resetsAt": "2026-10-24T17:00:00.000Z" },
                "weekly": { "status": "ok", "percent": 18, "resetsAt": "2026-10-24T17:00:00.000Z" },
                "monthly": { "status": "rate-limited", "percent": 100, "resetsAt": "2026-10-24T17:00:00.000Z" },
            },
        }),
    );

    let info = limits.get().await.expect("read usage limits").expect("the console has usage limits");

    let meter = |percent: f64, limited: bool| UsageLimitsMeter { percent, limited, resets_at: reset() };
    assert_eq!(
        info,
        UsageLimitsInfo {
            workspace: UsageLimitsInfoWorkspace {
                id: "wrk_anomaly".to_owned(),
                name: Some("Anomaly".to_owned()),
                usage_url: "https://dev.opencode.ai/console/wrk_anomaly/activity/overview".to_owned(),
            },
            go: Some(UsageLimitsGo {
                rolling: meter(42.0, false),
                weekly: meter(18.0, false),
                monthly: meter(100.0, true),
            }),
            budgets: vec![UsageLimitsBudget {
                id: "rule_astra".to_owned(),
                limit: MoneyUSD(20.0),
                spent: MoneyUSD(15.0),
                scope: UsageLimitsBudgetScope::Each,
                per: UsageLimitsBudgetPer::Model,
                coverage: UsageLimitsBudgetCoverage::Models,
                models: vec![UsageLimitsBudgetModel {
                    provider_id: ProviderID("opencode".to_owned()),
                    model_id: "gpt-6-astra".to_owned(),
                    name: "GPT-6 Astra".to_owned(),
                    spent: Some(MoneyUSD(15.0)),
                }],
                vendors: vec!["OpenAI".to_owned()],
                input_price_at_least: Some(MoneyUSDPerMillionTokens(3.0)),
                picked: false,
                shared: false,
                resets_at: opencode_util::time::parse_rfc3339("2026-11-01T00:00:00.000Z").expect("a valid reset date")
                    as f64,
            }],
        }
    );

    let mut requests = routes.requests();
    requests.sort_by(|a, b| a.url.cmp(&b.url));
    assert_eq!(requests.len(), 2);
    assert_eq!(requests[0].url, BUDGETS);
    assert_eq!(header(&requests[0], "authorization").as_deref(), Some("Bearer console-access"));
    assert_eq!(header(&requests[0], "x-org-id").as_deref(), Some("wrk_anomaly"));
    assert_eq!(header(&requests[0], "x-opencode-org-id"), None);
    assert_eq!(requests[1].url, GO);
    assert_eq!(header(&requests[1], "authorization").as_deref(), Some("Bearer console-access"));
    assert_eq!(header(&requests[1], "x-opencode-org-id").as_deref(), Some("wrk_anomaly"));
    assert_eq!(header(&requests[1], "x-org-id"), None);
}

#[tokio::test]
async fn has_no_go_allowance_or_budgets_to_show_where_console_refuses_them() {
    let (_directory, routes, limits) = read(Some(signed_in()));
    routes.set_status("GET", BUDGETS, 403);
    routes.set_status("GET", GO, 403);

    let info = limits.get().await.expect("read usage limits").expect("the workspace is still reported");

    assert_eq!(
        info,
        UsageLimitsInfo {
            workspace: UsageLimitsInfoWorkspace {
                id: "wrk_anomaly".to_owned(),
                name: Some("Anomaly".to_owned()),
                usage_url: "https://dev.opencode.ai/console/wrk_anomaly/activity/overview".to_owned(),
            },
            go: None,
            budgets: Vec::new(),
        }
    );
}

#[tokio::test]
async fn has_nothing_to_show_for_a_location_that_isnt_signed_in_to_opencode_console() {
    let (_directory, routes, limits) = read(None);

    assert_eq!(limits.get().await.expect("read usage limits"), None);
    assert!(routes.requests().is_empty());
}

#[tokio::test]
async fn reports_a_console_that_cant_answer_as_unavailable() {
    let (_directory, routes, limits) = read(Some(signed_in()));
    routes.set_status("GET", BUDGETS, 502);
    routes.set_status("GET", GO, 403);

    let error = limits.get().await.expect_err("the console cannot answer");

    assert_eq!(error.to_string(), "OpenCode Console budgets are unavailable: HTTP status 502");
}
