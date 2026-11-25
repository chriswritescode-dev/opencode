use http::StatusCode;
use opencode_contract::codec::{encode_json, write_optional, write_value};
use opencode_contract::generated::operations::{model_default, model_list};
use opencode_contract::generated::{
    ModelListResponse, ModelType, ModelTypeString, ModelTypeString2, ModelTypeString3, ModelTypeString4,
};
use opencode_contract::request::RawRequest;

use crate::handlers::invalid;
use crate::response;
use crate::services::{LocationServices, service_unavailable};

pub async fn list(raw: &RawRequest, services: &LocationServices) -> response::Response {
    let query = match model_list::decode_query(raw) {
        Ok(query) => query,
        Err(error) => return invalid::request(invalid::Kind::Query, error),
    };
    let types = query.types.flatten().map(|types| types.into_iter().map(contract_type).collect::<Vec<_>>());
    match services.models.available(types.as_deref()).await {
        Ok(models) => response::bytes(
            StatusCode::OK,
            "application/json",
            encode_json(&ModelListResponse { location: services.public_ref(), data: models }),
        ),
        Err(error) => service_unavailable("model", error),
    }
}

pub async fn default(raw: &RawRequest, services: &LocationServices) -> response::Response {
    if let Err(error) = model_default::decode_query(raw) {
        return invalid::request(invalid::Kind::Query, error);
    }
    let model = match services.models.default_model().await {
        Ok(model) => model,
        Err(error) => return service_unavailable("model", error),
    };
    let mut out = String::from("{");
    let mut first = true;
    write_value(&mut out, &mut first, "location", &services.public_ref());
    write_optional(&mut out, &mut first, "data", &model);
    out.push('}');
    response::bytes(StatusCode::OK, "application/json", out)
}

fn contract_type(value: opencode_contract::generated::requests::ModelType) -> ModelType {
    match value {
        opencode_contract::generated::requests::ModelType::String(_) => ModelType::String(ModelTypeString::Chat),
        opencode_contract::generated::requests::ModelType::String2(_) => ModelType::String2(ModelTypeString2::Decision),
        opencode_contract::generated::requests::ModelType::String3(_) => ModelType::String3(ModelTypeString3::Image),
        opencode_contract::generated::requests::ModelType::String4(_) => ModelType::String4(ModelTypeString4::Video),
        opencode_contract::generated::requests::ModelType::String5(value) => ModelType::String5(value),
    }
}
