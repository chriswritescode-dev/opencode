use opencode_contract::codec::{decode_json, encode_json};
use opencode_contract::generated::ArtifactProviderID;

#[test]
fn the_artifact_provider_id_trims_and_rejects_blank_values() {
    let trimmed: ArtifactProviderID = decode_json(r#"" x ""#).unwrap();
    assert_eq!(trimmed.0, "x");
    assert_eq!(encode_json(&trimmed), r#""x""#);

    for blank in [r#""""#, r#"" ""#, r#""\t""#, r#""\u00a0""#, r#""\u3000""#] {
        assert!(decode_json::<ArtifactProviderID>(blank).is_err(), "expected {blank} to be rejected");
    }
}

#[test]
fn the_artifact_provider_id_trims_exactly_the_javascript_whitespace() {
    let bom: ArtifactProviderID = decode_json("\"\\ufeffx\\ufeff\"").unwrap();
    assert_eq!(bom.0, "x");

    let next_line: ArtifactProviderID = decode_json("\"\\u0085\"").unwrap();
    assert_eq!(next_line.0, "\u{0085}");
}
