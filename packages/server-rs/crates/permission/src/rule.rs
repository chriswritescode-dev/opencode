use opencode_contract::generated::{PermissionEffect, PermissionRule};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Platform {
    Macos,
    Other,
}

pub fn current_platform() -> Platform {
    if cfg!(target_os = "macos") { Platform::Macos } else { Platform::Other }
}

pub fn ignores_case(action: &str, platform: Platform) -> bool {
    platform == Platform::Macos && matches!(action, "external_directory" | "read" | "edit")
}

pub fn evaluate(action: &str, resource: &str, rulesets: &[&[PermissionRule]]) -> PermissionRule {
    evaluate_on(current_platform(), action, resource, rulesets)
}

pub fn evaluate_on(platform: Platform, action: &str, resource: &str, rulesets: &[&[PermissionRule]]) -> PermissionRule {
    rulesets
        .iter()
        .flat_map(|rules| rules.iter())
        .rev()
        .find(|rule| {
            opencode_config::wildcard::matches(action, &rule.action, false)
                && opencode_config::wildcard::matches(resource, &rule.resource, ignores_case(action, platform))
        })
        .cloned()
        .unwrap_or_else(|| PermissionRule {
            action: action.to_string(),
            resource: "*".to_string(),
            effect: PermissionEffect::Ask,
        })
}

pub fn merge(rulesets: &[&[PermissionRule]]) -> Vec<PermissionRule> {
    rulesets.iter().flat_map(|rules| rules.iter().cloned()).collect()
}
