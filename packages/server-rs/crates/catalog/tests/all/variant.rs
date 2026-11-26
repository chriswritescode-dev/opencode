use opencode_ai::ProviderPackages;
use opencode_catalog::variant::{Budget, Model, Reasoning, Support, supports};
use serde_json::{Value, json};

use crate::corpus;

fn model(package: &str, model_id: &str, output: Option<i64>, provider_id: &str) -> Model {
    Model {
        id: model_id.to_string(),
        model_id: model_id.to_string(),
        provider_id: provider_id.to_string(),
        package: Some(package.to_string()),
        output: output.unwrap_or(32_000),
    }
}

fn resolve(input: &Model, supports: &[Support]) -> Value {
    serde_json::to_value(opencode_catalog::variant::resolve(input, supports)).expect("variants serialize")
}

fn effort(values: &[&str]) -> Support {
    Support::Effort { values: Some(values.iter().map(|value| value.to_string()).collect()) }
}

fn effort_default() -> Support {
    Support::Effort { values: None }
}

fn ids(value: &Value) -> Vec<String> {
    value
        .as_array()
        .expect("variants")
        .iter()
        .map(|variant| variant["id"].as_str().expect("an id").to_string())
        .collect()
}

#[test]
fn spells_messages_variants_for_each_provider() {
    let anthropic = "@opencode/ai/providers/anthropic";
    assert_eq!(
        resolve(
            &model(anthropic, "claude-opus-4-5", None, "test"),
            &[effort(&["low", "high"]), Support::BudgetTokens { min: Some(1024), max: None }],
        ),
        json!([
            {
                "id": "low",
                "settings": { "effort": "low", "thinking": { "type": "enabled", "budgetTokens": 16000 } },
            },
            {
                "id": "high",
                "settings": { "effort": "high", "thinking": { "type": "enabled", "budgetTokens": 16000 } },
            },
        ]),
    );
    assert_eq!(
        resolve(&model(anthropic, "claude-opus-4-8", None, "test"), &[effort(&["low", "max"])]),
        json!([
            {
                "id": "low",
                "settings": { "effort": "low", "thinking": { "type": "adaptive", "display": "summarized" } },
            },
            {
                "id": "max",
                "settings": { "effort": "max", "thinking": { "type": "adaptive", "display": "summarized" } },
            },
        ]),
    );
    assert_eq!(
        resolve(&model("@opencode/ai/providers/minimax/messages", "MiniMax-M3", None, "test"), &[Support::Toggle],),
        json!([
            { "id": "none", "settings": { "thinking": { "type": "disabled" } } },
            { "id": "thinking", "settings": { "thinking": { "type": "adaptive" } } },
        ]),
    );
    assert_eq!(
        resolve(
            &model("@opencode/ai/providers/moonshot/messages", "k3", None, "test"),
            &[Support::Toggle, effort(&["low", "high", "max"])],
        ),
        json!([
            { "id": "low", "settings": { "effort": "low" } },
            { "id": "high", "settings": { "effort": "high" } },
            { "id": "max", "settings": { "effort": "max" } },
        ]),
    );
    assert_eq!(
        resolve(
            &model("@opencode/ai/providers/alibaba/messages", "qwen3.8-max", None, "test"),
            &[
                Support::Toggle,
                effort(&["low", "medium", "xhigh"]),
                Support::BudgetTokens { min: Some(1024), max: Some(262_144) },
            ],
        ),
        json!([
            { "id": "none", "settings": { "thinking": { "type": "disabled" } } },
            { "id": "low", "settings": { "effort": "low", "thinking": { "type": "enabled" } } },
            { "id": "medium", "settings": { "effort": "medium", "thinking": { "type": "enabled" } } },
            { "id": "xhigh", "settings": { "effort": "xhigh", "thinking": { "type": "enabled" } } },
        ]),
    );
    assert_eq!(
        resolve(
            &model("@opencode/ai/providers/zai-coding-plan/messages", "glm-5.3", None, "test"),
            &[Support::Toggle, effort(&["low", "high", "max"]), Support::BudgetTokens { min: Some(1024), max: None },],
        ),
        json!([
            { "id": "low", "settings": { "effort": "low", "thinking": { "type": "enabled" } } },
            { "id": "high", "settings": { "effort": "high", "thinking": { "type": "enabled" } } },
            { "id": "max", "settings": { "effort": "max", "thinking": { "type": "enabled" } } },
        ]),
    );
}

#[test]
fn recognizes_claude_version_spellings_and_future_models() {
    let anthropic = "@opencode/ai/providers/anthropic";
    for id in ["claude-sonnet-3.7", "claude-sonnet-3-7", "claude-3.7-sonnet", "claude-3-7-sonnet"] {
        assert_eq!(
            resolve(&model(anthropic, id, None, "test"), &[effort_default()]),
            json!([
                { "id": "high", "settings": { "thinking": { "type": "enabled", "budgetTokens": 16000 } } },
                { "id": "max", "settings": { "thinking": { "type": "enabled", "budgetTokens": 31999 } } },
            ]),
            "{id}",
        );
    }
    assert_eq!(
        resolve(&model(anthropic, "claude-opus-6", None, "test"), &[effort_default()]),
        json!([
            { "id": "low", "settings": { "effort": "low", "thinking": { "type": "adaptive", "display": "summarized" } } },
            {
                "id": "medium",
                "settings": { "effort": "medium", "thinking": { "type": "adaptive", "display": "summarized" } },
            },
            {
                "id": "high",
                "settings": { "effort": "high", "thinking": { "type": "adaptive", "display": "summarized" } },
            },
            {
                "id": "xhigh",
                "settings": { "effort": "xhigh", "thinking": { "type": "adaptive", "display": "summarized" } },
            },
            { "id": "max", "settings": { "effort": "max", "thinking": { "type": "adaptive", "display": "summarized" } } },
        ]),
    );
    assert_eq!(
        resolve(
            &model(anthropic, "claude-haiku-4-5", Some(64_000), "test"),
            &[Support::BudgetTokens { min: Some(1_024), max: Some(64_000) }],
        ),
        json!([
            { "id": "high", "settings": { "thinking": { "type": "enabled", "budgetTokens": 16000 } } },
            { "id": "max", "settings": { "thinking": { "type": "enabled", "budgetTokens": 31999 } } },
        ]),
    );
}

#[test]
fn spells_cloudflare_ai_gateway_variants_for_their_upstream_routes() {
    let package = "@opencode/ai/providers/cloudflare-ai-gateway";
    assert_eq!(
        resolve(&model(package, "openai/gpt-5.4", None, "test"), &[effort(&["low", "xhigh"])]),
        json!([
            {
                "id": "low",
                "settings": {
                    "reasoningEffort": "low",
                    "reasoningSummary": "auto",
                    "include": ["reasoning.encrypted_content"],
                },
            },
            {
                "id": "xhigh",
                "settings": {
                    "reasoningEffort": "xhigh",
                    "reasoningSummary": "auto",
                    "include": ["reasoning.encrypted_content"],
                },
            },
        ]),
    );
    assert_eq!(
        resolve(&model(package, "anthropic/claude-sonnet-4-6", None, "test"), &[effort(&["low", "high"])]),
        json!([
            {
                "id": "low",
                "settings": { "effort": "low", "thinking": { "type": "adaptive", "display": "summarized" } },
            },
            {
                "id": "high",
                "settings": { "effort": "high", "thinking": { "type": "adaptive", "display": "summarized" } },
            },
        ]),
    );
    assert_eq!(
        resolve(&model(package, "xai/grok-4.6", None, "test"), &[effort(&["low", "high"])]),
        json!([
            { "id": "low", "settings": { "reasoningEffort": "low" } },
            { "id": "high", "settings": { "reasoningEffort": "high" } },
        ]),
    );
}

#[test]
fn spells_vercel_ai_gateway_variants_for_their_selected_routes() {
    let package = "@opencode/ai/providers/vercel-ai-gateway";
    assert_eq!(
        resolve(&model(package, "openai/gpt-5.4", None, "test"), &[effort(&["low", "xhigh"])]),
        json!([
            {
                "id": "low",
                "settings": {
                    "reasoningEffort": "low",
                    "reasoningSummary": "auto",
                    "include": ["reasoning.encrypted_content"],
                },
            },
            {
                "id": "xhigh",
                "settings": {
                    "reasoningEffort": "xhigh",
                    "reasoningSummary": "auto",
                    "include": ["reasoning.encrypted_content"],
                },
            },
        ]),
    );
    assert_eq!(
        ids(&resolve(&model(package, "spacexai/grok-4.7", None, "test"), &[effort_default()])),
        ["low", "medium", "high"],
    );
    assert_eq!(
        resolve(&model(package, "anthropic/claude-sonnet-4.6", None, "test"), &[effort(&["none", "low", "high"])],),
        json!([
            { "id": "none", "settings": { "thinking": { "type": "disabled" } } },
            {
                "id": "low",
                "settings": { "effort": "low", "thinking": { "type": "adaptive", "display": "summarized" } },
            },
            {
                "id": "high",
                "settings": { "effort": "high", "thinking": { "type": "adaptive", "display": "summarized" } },
            },
        ]),
    );
    assert_eq!(
        resolve(&model(package, "meta/muse-spark-1.3", None, "test"), &[effort(&["low", "high"])]),
        json!([
            { "id": "low", "settings": { "reasoningEffort": "low" } },
            { "id": "high", "settings": { "reasoningEffort": "high" } },
        ]),
    );
    assert_eq!(
        resolve(
            &model(package, "alibaba/qwen-3-32b", Some(38_912), "test"),
            &[Support::Toggle, Support::BudgetTokens { min: Some(1), max: Some(38_912) }],
        ),
        json!([
            { "id": "none", "settings": { "thinking": { "type": "disabled" } } },
            { "id": "high", "settings": { "thinking": { "type": "enabled", "budgetTokens": 19456 } } },
            { "id": "max", "settings": { "thinking": { "type": "enabled", "budgetTokens": 38911 } } },
        ]),
    );
    assert_eq!(
        resolve(&model(&format!("{package}/chat"), "anthropic/claude-sonnet-4.6", None, "test"), &[effort(&["high"])],),
        json!([{ "id": "high", "settings": { "reasoningEffort": "high" } }]),
    );
    assert_eq!(
        resolve(
            &model(&format!("{package}/messages"), "anthropic/claude-sonnet-4.6", None, "test"),
            &[effort(&["high"])],
        ),
        json!([{
            "id": "high",
            "settings": { "effort": "high", "thinking": { "type": "adaptive", "display": "summarized" } },
        }]),
    );
    assert_eq!(
        ids(&resolve(&model(&format!("{package}/responses"), "spacexai/grok-4.7", None, "test"), &[effort_default()],)),
        ["low", "medium", "high"],
    );
}

#[test]
fn spells_xai_responses_variants_with_catalog_effort_levels() {
    let package = "@opencode/ai/providers/xai";
    let supports = [effort(&["low", "medium", "high", "xhigh"])];
    assert_eq!(
        resolve(&model(package, "grok-4.6", None, "test"), &supports),
        json!([
            {
                "id": "low",
                "settings": {
                    "reasoningEffort": "low",
                    "reasoningSummary": "auto",
                    "include": ["reasoning.encrypted_content"],
                },
            },
            {
                "id": "medium",
                "settings": {
                    "reasoningEffort": "medium",
                    "reasoningSummary": "auto",
                    "include": ["reasoning.encrypted_content"],
                },
            },
            {
                "id": "high",
                "settings": {
                    "reasoningEffort": "high",
                    "reasoningSummary": "auto",
                    "include": ["reasoning.encrypted_content"],
                },
            },
            {
                "id": "xhigh",
                "settings": {
                    "reasoningEffort": "xhigh",
                    "reasoningSummary": "auto",
                    "include": ["reasoning.encrypted_content"],
                },
            },
        ]),
    );
    assert_eq!(
        resolve(&model(package, "grok-4.3", None, "test"), &[effort(&["none", "low"])]),
        json!([
            {
                "id": "none",
                "settings": {
                    "reasoningEffort": "none",
                    "reasoningSummary": "auto",
                    "include": ["reasoning.encrypted_content"],
                },
            },
            {
                "id": "low",
                "settings": {
                    "reasoningEffort": "low",
                    "reasoningSummary": "auto",
                    "include": ["reasoning.encrypted_content"],
                },
            },
        ]),
    );
    assert_eq!(
        ids(&resolve(&model(package, "grok-4.6", None, "test"), &[effort_default()])),
        ["low", "medium", "high"],
    );
}

#[test]
fn spells_chat_completions_variants_for_direct_providers() {
    assert_eq!(
        resolve(
            &model("@opencode/ai/providers/deepseek", "deepseek-v4-flash", None, "test"),
            &[Support::Toggle, effort(&["low", "high", "max"])],
        ),
        json!([
            { "id": "none", "body": { "thinking": { "type": "disabled" } } },
            {
                "id": "low",
                "settings": { "reasoningEffort": "low" },
                "body": { "thinking": { "type": "enabled" } },
            },
            {
                "id": "high",
                "settings": { "reasoningEffort": "high" },
                "body": { "thinking": { "type": "enabled" } },
            },
            {
                "id": "max",
                "settings": { "reasoningEffort": "max" },
                "body": { "thinking": { "type": "enabled" } },
            },
        ]),
    );
    assert_eq!(
        resolve(
            &model("@opencode/ai/providers/moonshot/chat", "kimi-k3", None, "test"),
            &[Support::Toggle, effort(&["low", "high", "max"])],
        ),
        json!([
            { "id": "low", "settings": { "reasoningEffort": "low" } },
            { "id": "high", "settings": { "reasoningEffort": "high" } },
            { "id": "max", "settings": { "reasoningEffort": "max" } },
        ]),
    );
    assert_eq!(
        resolve(&model("@opencode/ai/providers/moonshot/chat", "kimi-k2.6", None, "test"), &[Support::Toggle]),
        json!([
            { "id": "none", "settings": { "thinking": { "type": "disabled" } } },
            { "id": "thinking", "settings": { "thinking": { "type": "enabled" } } },
        ]),
    );
    assert_eq!(
        resolve(
            &model("@opencode/ai/providers/alibaba/chat", "qwen3.8-max", None, "test"),
            &[
                Support::Toggle,
                effort(&["low", "medium", "xhigh"]),
                Support::BudgetTokens { min: Some(0), max: Some(262_144) },
            ],
        ),
        json!([
            { "id": "none", "settings": { "enableThinking": false } },
            { "id": "low", "settings": { "enableThinking": true, "reasoningEffort": "low" } },
            { "id": "medium", "settings": { "enableThinking": true, "reasoningEffort": "medium" } },
            { "id": "xhigh", "settings": { "enableThinking": true, "reasoningEffort": "xhigh" } },
        ]),
    );
    assert_eq!(
        resolve(
            &model("@opencode/ai/providers/alibaba/chat", "qwen3.7-plus", Some(300_000), "test"),
            &[Support::Toggle, Support::BudgetTokens { min: Some(0), max: Some(262_144) }],
        ),
        json!([
            { "id": "none", "settings": { "enableThinking": false } },
            { "id": "high", "settings": { "enableThinking": true, "thinkingBudget": 32000 } },
            { "id": "max", "settings": { "enableThinking": true, "thinkingBudget": 63999 } },
        ]),
    );
    assert_eq!(
        resolve(
            &model("@opencode/ai/providers/zai/chat", "glm-5.3", None, "test"),
            &[Support::Toggle, effort(&["low", "high", "max"])],
        ),
        json!([
            {
                "id": "low",
                "settings": { "thinking": { "type": "enabled", "clear_thinking": false }, "reasoningEffort": "low" },
            },
            {
                "id": "high",
                "settings": { "thinking": { "type": "enabled", "clear_thinking": false }, "reasoningEffort": "high" },
            },
            {
                "id": "max",
                "settings": { "thinking": { "type": "enabled", "clear_thinking": false }, "reasoningEffort": "max" },
            },
        ]),
    );
    assert_eq!(
        resolve(&model("@opencode/ai/providers/zai/chat", "glm-4.7", None, "test"), &[Support::Toggle]),
        json!([
            { "id": "none", "settings": { "thinking": { "type": "disabled" } } },
            { "id": "thinking", "settings": { "thinking": { "type": "enabled", "clear_thinking": false } } },
        ]),
    );
}

#[test]
fn spells_bedrock_converse_claude_budgets_as_a_thinking_setting() {
    assert_eq!(
        resolve(
            &model(
                "@opencode/ai/providers/amazon-bedrock",
                "us.anthropic.claude-haiku-4-5-20251001-v1:0",
                Some(64_000),
                "test",
            ),
            &[Support::BudgetTokens { min: Some(1024), max: None }],
        ),
        json!([
            { "id": "high", "settings": { "thinking": { "type": "enabled", "budgetTokens": 16000 } } },
            { "id": "max", "settings": { "thinking": { "type": "enabled", "budgetTokens": 31999 } } },
        ]),
    );
}

#[test]
fn spells_bedrock_converse_effort_for_grok_and_nova() {
    let supports = [effort(&["low", "xhigh"])];
    assert_eq!(
        resolve(&model("@opencode/ai/providers/amazon-bedrock", "us.xai.grok-4.6", None, "test"), &supports,),
        json!([
            { "id": "low", "body": { "additionalModelRequestFields": { "reasoning": { "effort": "low" } } } },
            { "id": "xhigh", "body": { "additionalModelRequestFields": { "reasoning": { "effort": "xhigh" } } } },
        ]),
    );
    assert_eq!(
        resolve(&model("@opencode/ai/providers/amazon-bedrock", "us.amazon.nova-2-lite-v1:0", None, "test"), &supports,),
        json!([
            {
                "id": "low",
                "body": {
                    "additionalModelRequestFields": {
                        "reasoningConfig": { "type": "enabled", "maxReasoningEffort": "low" },
                    },
                },
            },
            {
                "id": "xhigh",
                "body": {
                    "additionalModelRequestFields": {
                        "reasoningConfig": { "type": "enabled", "maxReasoningEffort": "xhigh" },
                    },
                },
            },
        ]),
    );
}

#[test]
fn caps_alibaba_thinking_budget_variants_at_64k() {
    let supports = [Support::Toggle, Support::BudgetTokens { min: None, max: None }];
    assert_eq!(
        resolve(&model("@opencode/ai/providers/alibaba/chat", "kimi-k2.6", Some(262_144), "test"), &supports,),
        json!([
            { "id": "none", "settings": { "enableThinking": false } },
            { "id": "high", "settings": { "enableThinking": true, "thinkingBudget": 32000 } },
            { "id": "max", "settings": { "enableThinking": true, "thinkingBudget": 63999 } },
        ]),
    );
    assert_eq!(
        resolve(&model("@opencode/ai/providers/alibaba/messages", "kimi-k2.6", Some(262_144), "test"), &supports,),
        json!([
            { "id": "none", "settings": { "thinking": { "type": "disabled" } } },
            { "id": "high", "settings": { "thinking": { "type": "enabled", "budgetTokens": 32000 } } },
            { "id": "max", "settings": { "thinking": { "type": "enabled", "budgetTokens": 63999 } } },
        ]),
    );
    assert_eq!(
        resolve(&model("@opencode/ai/providers/alibaba/chat", "kimi-k2.5", Some(32_768), "test"), &supports,),
        json!([
            { "id": "none", "settings": { "enableThinking": false } },
            { "id": "high", "settings": { "enableThinking": true, "thinkingBudget": 16384 } },
            { "id": "max", "settings": { "enableThinking": true, "thinkingBudget": 32767 } },
        ]),
    );
}

#[test]
fn spells_workers_ai_thinking_controls_through_the_chat_template() {
    let package = "@opencode/ai/providers/cloudflare-workers-ai";
    assert_eq!(
        resolve(
            &model(package, "@cf/deepseek-ai/deepseek-v4-flash-0731", None, "test"),
            &[effort(&["none", "low", "high", "max"])],
        ),
        json!([
            { "id": "none", "body": { "chat_template_kwargs": { "enable_thinking": false, "thinking": false } } },
            { "id": "low", "settings": { "reasoningEffort": "low" } },
            { "id": "high", "settings": { "reasoningEffort": "high" } },
            { "id": "max", "settings": { "reasoningEffort": "max" } },
        ]),
    );
    assert_eq!(
        resolve(&model(package, "@cf/zai-org/glm-4.7-flash", None, "test"), &[Support::Toggle]),
        json!([
            { "id": "none", "body": { "chat_template_kwargs": { "enable_thinking": false, "thinking": false } } },
            { "id": "thinking", "body": { "chat_template_kwargs": { "enable_thinking": true, "thinking": true } } },
        ]),
    );
    assert_eq!(
        resolve(
            &model(package, "@cf/qwen/qwen3.8-27b", None, "test"),
            &[Support::Toggle, effort(&["low", "medium", "xhigh"])],
        ),
        json!([
            { "id": "none", "body": { "chat_template_kwargs": { "enable_thinking": false, "thinking": false } } },
            { "id": "low", "settings": { "reasoningEffort": "low" } },
            { "id": "medium", "settings": { "reasoningEffort": "medium" } },
            { "id": "xhigh", "settings": { "reasoningEffort": "xhigh" } },
        ]),
    );
}

#[test]
fn spells_cohere_native_thinking_and_compatibility_effort_variants() {
    assert_eq!(
        resolve(
            &model("@opencode/ai/providers/cohere", "command-a-reasoning-08-2025", Some(32_000), "test"),
            &[Support::Toggle, Support::BudgetTokens { min: Some(1), max: None }],
        ),
        json!([
            { "id": "none", "settings": { "thinking": { "type": "disabled" } } },
            { "id": "high", "settings": { "thinking": { "type": "enabled", "tokenBudget": 16000 } } },
            { "id": "max", "settings": { "thinking": { "type": "enabled", "tokenBudget": 31999 } } },
        ]),
    );
    assert_eq!(
        resolve(
            &model("@opencode/ai/providers/cohere/chat", "north-mini-code-1-0", None, "test"),
            &[effort(&["none", "high"])],
        ),
        json!([
            { "id": "none", "settings": { "reasoningEffort": "none" } },
            { "id": "high", "settings": { "reasoningEffort": "high" } },
        ]),
    );
}

#[test]
fn spells_chat_completions_variants_for_hosting_providers() {
    assert_eq!(
        resolve(
            &model("@opencode/ai/providers/digitalocean", "openai-gpt-5-nano", None, "digitalocean",),
            &[effort(&["minimal", "low", "medium", "high"])],
        ),
        json!([
            { "id": "minimal", "settings": { "reasoningEffort": "minimal" } },
            { "id": "low", "settings": { "reasoningEffort": "low" } },
            { "id": "medium", "settings": { "reasoningEffort": "medium" } },
            { "id": "high", "settings": { "reasoningEffort": "high" } },
        ]),
    );
    assert_eq!(
        resolve(
            &model("@opencode/ai/providers/openai-compatible", "deepseek-ai/deepseek-v4-pro", None, "nvidia",),
            &[effort(&["none", "high", "max"])],
        ),
        json!([
            { "id": "none", "body": { "chat_template_kwargs": { "thinking": false } } },
            {
                "id": "high",
                "body": { "chat_template_kwargs": { "thinking": true, "reasoning_effort": "high" } },
            },
            {
                "id": "max",
                "body": { "chat_template_kwargs": { "thinking": true, "reasoning_effort": "max" } },
            },
        ]),
    );
    assert_eq!(
        resolve(
            &model("@opencode/ai/providers/openai-compatible", "moonshotai/kimi-k2.6", None, "nvidia",),
            &[effort(&["none", "low", "high", "max"])],
        ),
        json!([
            { "id": "none", "body": { "chat_template_kwargs": { "thinking": false } } },
            { "id": "thinking", "body": { "chat_template_kwargs": { "thinking": true } } },
        ]),
    );
    assert_eq!(
        resolve(
            &model("@opencode/ai/providers/openai-compatible", "moonshotai/kimi-k3", None, "nvidia"),
            &[Support::Toggle, effort(&["low", "high", "max"])],
        ),
        json!([
            { "id": "none", "body": { "chat_template_kwargs": { "thinking": false } } },
            { "id": "low", "settings": { "reasoningEffort": "low" } },
            { "id": "high", "settings": { "reasoningEffort": "high" } },
            { "id": "max", "settings": { "reasoningEffort": "max" } },
        ]),
    );
    assert_eq!(
        resolve(
            &model("@opencode/ai/providers/openai-compatible", "minimaxai/minimax-m3", None, "nvidia",),
            &[Support::Toggle],
        ),
        json!([
            { "id": "none", "body": { "chat_template_kwargs": { "thinking_mode": "disabled" } } },
            { "id": "thinking", "body": { "chat_template_kwargs": { "thinking_mode": "enabled" } } },
        ]),
    );
    assert_eq!(
        resolve(
            &model("@opencode/ai/providers/baseten", "zai-org/GLM-5.2", None, "test"),
            &[effort(&["none", "high", "max"])],
        ),
        json!([
            {
                "id": "none",
                "settings": { "reasoningEffort": "none" },
                "body": { "chat_template_args": { "enable_thinking": false } },
            },
            {
                "id": "high",
                "settings": { "reasoningEffort": "high" },
                "body": { "chat_template_args": { "enable_thinking": true } },
            },
            {
                "id": "max",
                "settings": { "reasoningEffort": "max" },
                "body": { "chat_template_args": { "enable_thinking": true } },
            },
        ]),
    );
    assert_eq!(
        resolve(&model("@opencode/ai/providers/baseten", "moonshotai/Kimi-K2.6", None, "test"), &[Support::Toggle]),
        json!([
            { "id": "none", "body": { "chat_template_args": { "enable_thinking": false } } },
            { "id": "thinking", "body": { "chat_template_args": { "enable_thinking": true } } },
        ]),
    );
    assert_eq!(
        resolve(
            &model("@opencode/ai/providers/deepinfra", "zai-org/GLM-5.2", None, "test"),
            &[Support::Toggle, effort(&["low", "high", "xhigh"])],
        ),
        json!([
            { "id": "none", "body": { "reasoning": { "enabled": false } } },
            { "id": "low", "settings": { "reasoningEffort": "low" } },
            { "id": "high", "settings": { "reasoningEffort": "high" } },
            { "id": "xhigh", "settings": { "reasoningEffort": "xhigh" } },
        ]),
    );
}

#[test]
fn spells_gitlab_duo_variants_for_gitlab_ai_provider_routes() {
    let gitlab = |id: &str, output: Option<i64>| model("aisdk:gitlab-ai-provider", id, output, "gitlab");
    assert_eq!(
        resolve(&gitlab("duo-chat-opus-5-5", None), &[effort(&["low", "medium", "high", "xhigh", "max"])]),
        json!([
            { "id": "low", "settings": { "thinking": { "type": "adaptive", "effort": "low" } } },
            { "id": "medium", "settings": { "thinking": { "type": "adaptive", "effort": "medium" } } },
            { "id": "high", "settings": { "thinking": { "type": "adaptive", "effort": "high" } } },
            { "id": "xhigh", "settings": { "thinking": { "type": "adaptive", "effort": "xhigh" } } },
            { "id": "max", "settings": { "thinking": { "type": "adaptive", "effort": "max" } } },
        ]),
    );
    assert_eq!(
        resolve(&gitlab("duo-chat-sonnet-5", None), &[Support::Toggle, effort(&["low", "max"])]),
        json!([
            { "id": "none", "settings": { "thinking": { "type": "disabled" } } },
            { "id": "low", "settings": { "thinking": { "type": "adaptive", "effort": "low" } } },
            { "id": "max", "settings": { "thinking": { "type": "adaptive", "effort": "max" } } },
        ]),
    );
    assert_eq!(
        resolve(&gitlab("duo-chat-fable-5-1", None), &[Support::Toggle, effort(&["high"])]),
        json!([{ "id": "high", "settings": { "thinking": { "type": "adaptive", "effort": "high" } } }]),
    );
    assert_eq!(
        resolve(&gitlab("duo-chat-opus-4-5", Some(64_000)), &[effort_default()]),
        json!([
            { "id": "high", "settings": { "thinking": { "type": "enabled", "budgetTokens": 16000 } } },
            { "id": "max", "settings": { "thinking": { "type": "enabled", "budgetTokens": 31999 } } },
        ]),
    );
    assert_eq!(
        resolve(&gitlab("duo-chat-gpt-6-sol", None), &[effort(&["none", "low", "max"])]),
        json!([
            { "id": "none", "settings": { "reasoningEffort": "none" } },
            { "id": "low", "settings": { "reasoningEffort": "low" } },
            { "id": "max", "settings": { "reasoningEffort": "max" } },
        ]),
    );
    assert_eq!(
        resolve(&gitlab("duo-chat-gpt-5-3-codex", None), &[effort(&["low"])]),
        json!([{ "id": "low", "settings": { "reasoningEffort": "low" } }]),
    );
    assert_eq!(resolve(&gitlab("duo-chat-gpt-5-4", None), &[]), json!([]));
    assert_eq!(resolve(&gitlab("duo-workflow-opus-5-5", None), &[effort_default()]), json!([]));
}

#[test]
fn spells_reasoning_variants_for_every_bundled_native_package() {
    let supports = [Support::Toggle, Support::Effort { values: None }, Support::BudgetTokens { min: None, max: None }];
    let packages = ProviderPackages::builtin();
    let missing: Vec<&str> = packages
        .specifiers()
        .into_iter()
        .filter(|package| !has_non_text_suffix(package) && !PACKAGES_WITHOUT_CHAT_ENTRY.contains(package))
        .filter(|package| {
            resolve(&model(package, "test-model", Some(32_000), "test"), &supports).as_array().is_none_or(Vec::is_empty)
        })
        .collect();
    assert!(missing.is_empty(), "missing variants for {missing:?}");
}

const PACKAGES_WITHOUT_CHAT_ENTRY: [&str; 8] = [
    "@opencode/ai/providers/assemblyai",
    "@opencode/ai/providers/byteplus",
    "@opencode/ai/providers/cartesia",
    "@opencode/ai/providers/deepgram",
    "@opencode/ai/providers/elevenlabs",
    "@opencode/ai/providers/opencode",
    "@opencode/ai/providers/typesafe-ai",
    "@opencode/ai/providers/volcengine",
];

fn has_non_text_suffix(package: &str) -> bool {
    ["/images", "/videos", "/decisions", "/embeddings"].iter().any(|suffix| package.ends_with(suffix))
}

#[test]
fn spells_mantle_variants_for_the_api_each_model_is_routed_through() {
    assert_eq!(
        resolve(
            &model("@opencode/ai/providers/amazon-bedrock/mantle/messages", "anthropic.claude-opus-4-8", None, "test",),
            &[effort(&["low", "max"])],
        ),
        json!([
            {
                "id": "low",
                "settings": { "effort": "low", "thinking": { "type": "adaptive", "display": "summarized" } },
            },
            {
                "id": "max",
                "settings": { "effort": "max", "thinking": { "type": "adaptive", "display": "summarized" } },
            },
        ]),
    );
    assert_eq!(
        resolve(
            &model("@opencode/ai/providers/amazon-bedrock/mantle/chat", "openai.gpt-oss-120b", None, "test"),
            &[effort_default()],
        ),
        json!([
            { "id": "low", "settings": { "reasoningEffort": "low" } },
            { "id": "medium", "settings": { "reasoningEffort": "medium" } },
            { "id": "high", "settings": { "reasoningEffort": "high" } },
        ]),
    );
}

#[test]
fn spells_google_interactions_variants_as_thinking_levels() {
    for package in ["@opencode/ai/providers/google/interactions", "@opencode/ai/providers/google-vertex/interactions"] {
        assert_eq!(
            resolve(&model(package, "gemini-3.5-pro", None, "test"), &[effort(&["low", "high"])]),
            json!([
                { "id": "low", "settings": { "thinkingLevel": "low", "thinkingSummaries": "auto" } },
                { "id": "high", "settings": { "thinkingLevel": "high", "thinkingSummaries": "auto" } },
            ]),
            "{package}",
        );
    }
}

#[test]
fn only_adds_google_interactions_thinking_toggles_for_toggle_only_models() {
    let package = "@opencode/ai/providers/google/interactions";
    assert_eq!(
        resolve(
            &model(package, "gemma-4-31b-it", None, "test"),
            &supports(&Reasoning { supported: true, toggle: true, effort: None, budget: None }),
        ),
        json!([
            { "id": "none", "settings": { "thinkingLevel": "minimal" } },
            { "id": "thinking", "settings": { "thinkingLevel": "high", "thinkingSummaries": "auto" } },
        ]),
    );
    assert_eq!(
        resolve(
            &model(package, "gemini-2.5-flash", None, "test"),
            &supports(&Reasoning {
                supported: true,
                toggle: true,
                effort: None,
                budget: Some(Budget { min: Some(0), max: Some(24_576) }),
            }),
        ),
        json!([]),
    );
}

#[test]
fn reads_models_dev_v2_reasoning_as_variant_support() {
    assert_eq!(
        supports(&Reasoning { supported: false, toggle: false, effort: None, budget: None }),
        Vec::<Support>::new(),
    );
    assert_eq!(
        supports(&Reasoning { supported: true, toggle: false, effort: None, budget: None }),
        Vec::<Support>::new(),
    );
    assert_eq!(
        supports(&Reasoning {
            supported: true,
            toggle: true,
            effort: Some(vec!["default".to_string(), "low".to_string()]),
            budget: Some(Budget { min: Some(1024), max: Some(8000) }),
        }),
        vec![
            Support::Toggle,
            Support::Effort { values: Some(vec!["low".to_string()]) },
            Support::BudgetTokens { min: Some(1024), max: Some(8000) },
        ],
    );
}

#[test]
fn variants_corpus_matches_the_pinned_typescript() {
    let cases = corpus::area("variants");
    for (name, case) in &cases {
        let model: Model = serde_json::from_value(case["model"].clone()).expect("a model");
        let supports: Vec<Support> = serde_json::from_value(case["supports"].clone()).expect("supports");
        let actual = serde_json::to_value(resolve(&model, &supports)).expect("variants serialize");
        assert_eq!(actual, case["result"], "case {name}");
    }
}
