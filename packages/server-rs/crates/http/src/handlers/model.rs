use http::StatusCode;
use opencode_contract::codec::encode_json;
use opencode_contract::generated::operations::model_list;
use opencode_contract::generated::requests::ModelType;
use opencode_contract::generated::{AbsolutePath, LocationPublicRef, ModelListResponse};
use opencode_contract::request::RawRequest;

use crate::app::App;
use crate::handlers::invalid;
use crate::response;

pub async fn list(app: &App, raw: &RawRequest) -> response::Response {
    let lease = match app.location(raw).await {
        Ok(lease) => lease,
        Err(response) => return response,
    };
    let query = match model_list::decode_query(raw) {
        Ok(query) => query,
        Err(error) => return invalid::request(invalid::Kind::Query, error),
    };
    let types = query.types.flatten().map(|types| {
        types
            .into_iter()
            .map(|r#type| match r#type {
                ModelType::String(_) => "chat".to_owned(),
                ModelType::String2(_) => "decision".to_owned(),
                ModelType::String3(_) => "image".to_owned(),
                ModelType::String4(_) => "video".to_owned(),
                ModelType::String5(value) => value,
            })
            .collect::<Vec<String>>()
    });
    let data = lease.services().catalog.read().expect("the catalog lock is never poisoned").models(types.as_deref());
    let location = LocationPublicRef { directory: AbsolutePath(lease.services().info.directory.clone()) };
    response::bytes(StatusCode::OK, "application/json", encode_json(&ModelListResponse { location, data }))
}
