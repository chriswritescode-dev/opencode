use http::StatusCode;
use opencode_contract::codec::encode_json;
use opencode_contract::generated::operations::reference_list;
use opencode_contract::generated::{
    AbsolutePath, LocationPublicRef, ReferenceGitSource, ReferenceGitSourceType, ReferenceInfo, ReferenceListResponse,
    ReferenceLocalSource, ReferenceLocalSourceType, ReferenceSource,
};
use opencode_contract::request::RawRequest;
use opencode_vcs::reference::{Info, Source};

use crate::app::App;
use crate::handlers::invalid;
use crate::response;

pub async fn list(app: &App, raw: &RawRequest) -> response::Response {
    if let Err(error) = reference_list::decode_query(raw) {
        return invalid::request(invalid::Kind::Query, error);
    }
    let lease = match app.location(raw).await {
        Ok(lease) => lease,
        Err(response) => return response,
    };
    let data = lease.services().reference.list().await.into_iter().map(info).collect();
    let body = ReferenceListResponse {
        location: LocationPublicRef { directory: AbsolutePath(lease.services().info.directory.clone()) },
        data,
    };
    response::bytes(StatusCode::OK, "application/json", encode_json(&body))
}

fn info(info: Info) -> ReferenceInfo {
    let source = match info.source {
        Source::Local(source) => ReferenceSource::Local(ReferenceLocalSource {
            r#type: ReferenceLocalSourceType::Local,
            path: AbsolutePath(source.path),
        }),
        Source::Git(source) => ReferenceSource::Git(ReferenceGitSource {
            r#type: ReferenceGitSourceType::Git,
            repository: source.repository,
            branch: source.branch,
        }),
    };
    ReferenceInfo {
        name: info.name,
        path: AbsolutePath(info.path),
        description: info.description,
        hidden: info.hidden,
        source,
    }
}
