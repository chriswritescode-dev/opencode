use std::collections::HashMap;

use futures_util::future::join_all;
use http::StatusCode;
use opencode_contract::codec::{JsonMap, encode_json};
use opencode_contract::generated::operations::{plugin_check, plugin_gui, plugin_list, plugin_update};
use opencode_contract::generated::{
    InvalidRequestError, InvalidRequestErrorTag, PluginCheckResponse, PluginGuiBundle, PluginGuiResponse, PluginInfo,
    PluginListResponse, PluginNotFoundError, PluginNotFoundErrorTag, ServiceUnavailableError,
    ServiceUnavailableErrorTag,
};
use opencode_contract::request::RawRequest;
use opencode_plugin::{Info, Source, Updates};

use crate::handlers::{defect, invalid};
use crate::response;
use crate::services::{LocationServices, public_ref};

pub async fn list(raw: &RawRequest, services: &LocationServices) -> response::Response {
    if let Err(error) = plugin_list::decode_query(raw) {
        return invalid::request(invalid::Kind::Query, error);
    }
    let body = PluginListResponse { location: public_ref(&services.info), data: inventory(services) };
    response::bytes(StatusCode::OK, "application/json", encode_json(&body))
}

pub async fn gui(raw: &RawRequest, services: &LocationServices) -> response::Response {
    let query = match plugin_gui::decode_query(raw) {
        Ok(query) => query,
        Err(error) => return invalid::request(invalid::Kind::Query, error),
    };
    let id = query.id.0;
    match services.plugins.registry().gui(&id) {
        Some(gui) => match tokio::fs::read_to_string(&gui.file).await {
            Ok(code) => response::bytes(
                StatusCode::OK,
                "application/json",
                encode_json(&PluginGuiResponse {
                    location: public_ref(&services.info),
                    data: PluginGuiBundle {
                        code,
                        revision: gui.revision,
                        options: JsonMap(gui.options.into_iter().collect()),
                    },
                }),
            ),
            Err(error) => defect::response("failed to read plugin gui bundle", error),
        },
        None => {
            let message = format!("No active plugin with a ./gui-experimental bundle: {id}");
            response::bytes(
                StatusCode::NOT_FOUND,
                "application/json",
                encode_json(&PluginNotFoundError {
                    tag: PluginNotFoundErrorTag::PluginNotFoundError,
                    plugin_id: id,
                    message,
                }),
            )
        }
    }
}

pub async fn check(raw: &RawRequest, services: &LocationServices, updates: &Updates) -> response::Response {
    let body = match plugin_check::decode_body(raw) {
        Ok(body) => body,
        Err(error) => return invalid::request(invalid::Kind::Body, error),
    };
    let inventory = services.plugins.registry().list();
    let available = package_targets(&inventory);
    let selected = match body.target {
        None => available,
        Some(None) => {
            return invalid_field("Plugin package is not in the current server inventory: null".to_owned(), "target");
        }
        Some(Some(target)) => {
            let selected: Vec<String> = available.into_iter().filter(|candidate| *candidate == target).collect();
            if selected.is_empty() {
                return invalid_field(
                    format!("Plugin package is not in the current server inventory: {target}"),
                    "target",
                );
            }
            selected
        }
    };
    let checks = join_all(selected.iter().map(|target| updates.check(target, true))).await;
    let checked: HashMap<String, bool> = selected.into_iter().zip(checks).collect();
    let data = inventory.into_iter().map(|info| check_info(info, &checked)).collect();
    let body = PluginCheckResponse { location: public_ref(&services.info), data };
    response::bytes(StatusCode::OK, "application/json", encode_json(&body))
}

pub async fn update(raw: &RawRequest, services: &LocationServices, updates: &Updates) -> response::Response {
    let body = match plugin_update::decode_body(raw) {
        Ok(body) => body,
        Err(error) => return invalid::request(invalid::Kind::Body, error),
    };
    let available = package_targets(&services.plugins.registry().list());
    let unknown: Vec<String> = body.targets.iter().filter(|target| !available.contains(*target)).cloned().collect();
    if !unknown.is_empty() {
        return invalid_field(
            format!("Plugin packages are not in the current server inventory: {}", unknown.join(", ")),
            "targets",
        );
    }
    let results = join_all(body.targets.iter().map(|target| updates.update(target))).await;
    let failures: Vec<String> = body
        .targets
        .iter()
        .zip(results)
        .filter_map(|(target, result)| result.err().map(|error| format!("{target}: {}", error.message)))
        .collect();
    if !failures.is_empty() {
        return response::bytes(
            StatusCode::SERVICE_UNAVAILABLE,
            "application/json",
            encode_json(&ServiceUnavailableError {
                tag: ServiceUnavailableErrorTag::ServiceUnavailableError,
                message: format!("Failed to update plugin packages: {}", failures.join("; ")),
                service: Some(Some("plugin".to_owned())),
            }),
        );
    }
    response::empty(StatusCode::NO_CONTENT)
}

fn inventory(services: &LocationServices) -> Vec<PluginInfo> {
    services.plugins.registry().list().into_iter().map(|info| info.to_wire()).collect()
}

fn package_targets(inventory: &[Info]) -> Vec<String> {
    let mut targets = Vec::new();
    for info in inventory {
        if let Source::Package { target, .. } = &info.source
            && !targets.contains(target)
        {
            targets.push(target.clone());
        }
    }
    targets
}

fn check_info(mut info: Info, checked: &HashMap<String, bool>) -> PluginInfo {
    if let Source::Package { target, version, updating, .. } = info.source.clone()
        && let Some(outdated) = checked.get(&target)
    {
        info.source = Source::Package { target, version, outdated: *outdated, updating };
    }
    info.to_wire()
}

fn invalid_field(message: String, field: &str) -> response::Response {
    response::bytes(
        StatusCode::BAD_REQUEST,
        "application/json",
        encode_json(&InvalidRequestError {
            tag: InvalidRequestErrorTag::InvalidRequestError,
            message,
            kind: None,
            field: Some(Some(field.to_owned())),
        }),
    )
}
