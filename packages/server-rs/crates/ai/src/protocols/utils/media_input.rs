use crate::schema::{MediaAsset, MediaSource};

pub fn ref_id(asset: &MediaAsset, provider: &str) -> Option<String> {
    let MediaSource::Ref { provider: source_provider, id, .. } = &asset.source else {
        return None;
    };
    if source_provider != provider {
        return None;
    }
    Some(id.clone())
}
