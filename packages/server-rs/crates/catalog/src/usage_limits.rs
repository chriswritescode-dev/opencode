use std::sync::Arc;
use std::time::Duration;

use opencode_ai::HttpSender;
use opencode_contract::generated::{
    CredentialValue, IntegrationID, MoneyUSD, MoneyUSDPerMillionTokens, ProviderID, UsageLimitsBudget,
    UsageLimitsBudgetCoverage, UsageLimitsBudgetModel, UsageLimitsBudgetPer, UsageLimitsBudgetScope, UsageLimitsGo,
    UsageLimitsInfo, UsageLimitsInfoWorkspace, UsageLimitsMeter,
};
use serde::Deserialize;
use serde_json::Value;

use crate::console_access;
use crate::integration::IntegrationService;
use crate::provider_plugin::http::{self, HttpError};

const TIMEOUT: Duration = Duration::from_secs(15);

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[error("{0}")]
pub struct UnavailableError(pub String);

pub struct UsageLimits {
    integrations: Arc<IntegrationService>,
    http: Arc<dyn HttpSender>,
}

impl UsageLimits {
    pub fn new(integrations: Arc<IntegrationService>, http: Arc<dyn HttpSender>) -> Self {
        Self { integrations, http }
    }

    pub async fn get(&self) -> Result<Option<UsageLimitsInfo>, UnavailableError> {
        let connection = self
            .integrations
            .connection()
            .active(&IntegrationID("opencode".to_owned()))
            .await
            .map_err(|error| UnavailableError(error.to_string()))?;
        let Some(connection) = connection else {
            return Ok(None);
        };
        let credential = self
            .integrations
            .connection()
            .resolve(&connection)
            .await
            .map_err(|error| UnavailableError(error.to_string()))?;
        let Some(credential) = credential else {
            return Ok(None);
        };
        if matches!(credential, CredentialValue::External(_)) {
            return Ok(None);
        }
        let Some(workspace_id) = console_access::metadata_field(&credential, "orgID").and_then(Value::as_str) else {
            return Ok(None);
        };
        let workspace_id = workspace_id.to_owned();
        let server = console_access::server(&credential);
        let (budgets, go) = tokio::time::timeout(TIMEOUT, async {
            tokio::try_join!(
                read_budgets(self.http.as_ref(), &server, &credential),
                read_go(self.http.as_ref(), &server, &credential, &workspace_id),
            )
        })
        .await
        .map_err(|_| UnavailableError("OpenCode Console usage limits timed out".to_owned()))??;
        let workspace = UsageLimitsInfoWorkspace {
            id: workspace_id.clone(),
            name: console_access::metadata_field(&credential, "orgName").and_then(Value::as_str).map(str::to_owned),
            usage_url: format!("{server}/{workspace_id}/activity/overview"),
        };
        Ok(Some(UsageLimitsInfo { workspace, go, budgets }))
    }
}

async fn read_budgets(
    http: &dyn HttpSender,
    server: &str,
    credential: &CredentialValue,
) -> Result<Vec<UsageLimitsBudget>, UnavailableError> {
    let mut headers = console_access::headers(credential);
    headers.push(("accept".to_owned(), "application/json".to_owned()));
    let url = format!("{server}/api/policies/current-budgets");
    let response = match http::send(http, http::get(&url, headers), None).await {
        Ok(response) => response,
        Err(HttpError::Status(response)) if response.status == 403 => return Ok(Vec::new()),
        Err(error) => return Err(budgets_unavailable(http_error_message(&error, "GET", &url))),
    };
    let budgets: Vec<ConsoleBudget> = serde_json::from_slice(&response.body).map_err(budgets_unavailable)?;
    budgets.into_iter().map(map_budget).collect::<Result<_, _>>().map_err(budgets_unavailable)
}

async fn read_go(
    http: &dyn HttpSender,
    server: &str,
    credential: &CredentialValue,
    workspace_id: &str,
) -> Result<Option<UsageLimitsGo>, UnavailableError> {
    let origin = url::Url::parse(server).map_err(go_unavailable)?.origin().ascii_serialization();
    let token = console_access::token(credential).expect("a non-external credential authenticates");
    let headers = vec![
        ("accept".to_owned(), "application/json".to_owned()),
        ("authorization".to_owned(), format!("Bearer {token}")),
        ("x-opencode-org-id".to_owned(), workspace_id.to_owned()),
    ];
    let url = format!("{origin}/zen/go/v1/usage");
    let response = match http::send(http, http::get(&url, headers), None).await {
        Ok(response) => response,
        Err(HttpError::Status(response)) if response.status == 403 => return Ok(None),
        Err(error) => return Err(go_unavailable(http_error_message(&error, "GET", &url))),
    };
    let usage: GoUsage = serde_json::from_slice(&response.body).map_err(go_unavailable)?;
    let go = UsageLimitsGo {
        rolling: map_meter(usage.usage.rolling).map_err(go_unavailable)?,
        weekly: map_meter(usage.usage.weekly).map_err(go_unavailable)?,
        monthly: map_meter(usage.usage.monthly).map_err(go_unavailable)?,
    };
    Ok(Some(go))
}

fn budgets_unavailable(error: impl std::fmt::Display) -> UnavailableError {
    UnavailableError(format!("OpenCode Console budgets are unavailable: {error}"))
}

fn go_unavailable(error: impl std::fmt::Display) -> UnavailableError {
    UnavailableError(format!("OpenCode Go usage is unavailable: {error}"))
}

fn http_error_message(error: &HttpError, method: &str, url: &str) -> String {
    match error {
        HttpError::Status(response) => {
            format!("StatusCode: non 2xx status code ({} {method} {url})", response.status)
        }
        HttpError::Transport(_) | HttpError::Timeout => error.to_string(),
    }
}

fn map_budget(budget: ConsoleBudget) -> Result<UsageLimitsBudget, String> {
    let scope = match budget.scope.as_str() {
        "each" => UsageLimitsBudgetScope::Each,
        "combined" => UsageLimitsBudgetScope::Combined,
        value => return Err(format!("expected one of \"each\", \"combined\", found \"{value}\"")),
    };
    let per = match budget.per.as_str() {
        "set" => UsageLimitsBudgetPer::Set,
        "model" => UsageLimitsBudgetPer::Model,
        value => return Err(format!("expected one of \"set\", \"model\", found \"{value}\"")),
    };
    let coverage = match budget.coverage.as_str() {
        "all" => UsageLimitsBudgetCoverage::All,
        "models" => UsageLimitsBudgetCoverage::Models,
        value => return Err(format!("expected one of \"all\", \"models\", found \"{value}\"")),
    };
    Ok(UsageLimitsBudget {
        id: budget.rule_id,
        limit: MoneyUSD(budget.limit_dollars),
        spent: MoneyUSD(budget.spent_dollars),
        scope,
        per,
        coverage,
        models: budget
            .models
            .into_iter()
            .map(|model| UsageLimitsBudgetModel {
                provider_id: ProviderID(model.provider_id),
                model_id: model.model_id,
                name: model.name,
                spent: model.spent_dollars.map(MoneyUSD),
            })
            .collect(),
        vendors: budget.vendors.into_iter().map(|vendor| vendor.name).collect(),
        input_price_at_least: budget.input_price_at_least.map(MoneyUSDPerMillionTokens),
        picked: budget.picked,
        shared: budget.shared,
        resets_at: parse_reset(&budget.resets_at)?,
    })
}

fn map_meter(meter: ConsoleMeter) -> Result<UsageLimitsMeter, String> {
    let limited = match meter.status.as_str() {
        "ok" => false,
        "rate-limited" => true,
        value => return Err(format!("expected one of \"ok\", \"rate-limited\", found \"{value}\"")),
    };
    Ok(UsageLimitsMeter { percent: meter.percent, limited, resets_at: parse_reset(&meter.resets_at)? })
}

fn parse_reset(value: &str) -> Result<f64, String> {
    opencode_util::time::parse_rfc3339(value)
        .map(|millis| millis as f64)
        .ok_or_else(|| format!("expected an RFC 3339 date, found \"{value}\""))
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct ConsoleBudget {
    #[serde(rename = "ruleID")]
    rule_id: String,
    limit_dollars: f64,
    spent_dollars: f64,
    scope: String,
    per: String,
    coverage: String,
    models: Vec<ConsoleBudgetModel>,
    vendors: Vec<ConsoleVendor>,
    input_price_at_least: Option<f64>,
    picked: bool,
    shared: bool,
    resets_at: String,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct ConsoleBudgetModel {
    #[serde(rename = "providerID")]
    provider_id: String,
    #[serde(rename = "modelID")]
    model_id: String,
    name: String,
    spent_dollars: Option<f64>,
}

#[derive(Deserialize)]
struct ConsoleVendor {
    #[serde(rename = "id")]
    _id: String,
    name: String,
}

#[derive(Deserialize)]
struct GoUsage {
    usage: GoUsageMeters,
}

#[derive(Deserialize)]
struct GoUsageMeters {
    rolling: ConsoleMeter,
    weekly: ConsoleMeter,
    monthly: ConsoleMeter,
}

#[derive(Deserialize)]
struct ConsoleMeter {
    status: String,
    percent: f64,
    #[serde(rename = "resetsAt")]
    resets_at: String,
}
