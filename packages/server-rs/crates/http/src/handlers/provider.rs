use http::StatusCode;
use opencode_contract::codec::encode_json;
use opencode_contract::generated::ProviderNotFoundErrorTag;
use opencode_contract::generated::operations::{provider_get, provider_list};
use opencode_contract::generated::{ProviderGetResponse, ProviderID, ProviderListResponse, ProviderNotFoundError};
use opencode_contract::request::RawRequest;

use crate::handlers::invalid;
use crate::response;
use crate::services::{LocationServices, service_unavailable};

pub async fn list(raw: &RawRequest, services: &LocationServices) -> response::Response {
    if let Err(error) = provider_list::decode_query(raw) {
        return invalid::request(invalid::Kind::Query, error);
    }
    match services.providers.available().await {
        Ok(providers) => response::bytes(
            StatusCode::OK,
            "application/json",
            encode_json(&ProviderListResponse { location: services.public_ref(), data: providers }),
        ),
        Err(error) => service_unavailable("provider", error),
    }
}

pub async fn get(raw: &RawRequest, services: &LocationServices) -> response::Response {
    let params = match provider_get::decode_params(raw) {
        Ok(params) => params,
        Err(error) => return invalid::request(invalid::Kind::Params, error),
    };
    if let Err(error) = provider_get::decode_query(raw) {
        return invalid::request(invalid::Kind::Query, error);
    }
    let provider_id = ProviderID(params.provider_id.0.clone());
    match services.providers.get(&provider_id).await {
        Ok(Some(provider)) => response::bytes(
            StatusCode::OK,
            "application/json",
            encode_json(&ProviderGetResponse { location: services.public_ref(), data: provider }),
        ),
        Ok(None) => response::bytes(
            StatusCode::NOT_FOUND,
            "application/json",
            encode_json(&ProviderNotFoundError {
                tag: ProviderNotFoundErrorTag::ProviderNotFoundError,
                provider_id: provider_id.0.clone(),
                message: format!("Provider not found: {}", provider_id.0),
            }),
        ),
        Err(error) => service_unavailable("provider", error),
    }
}
