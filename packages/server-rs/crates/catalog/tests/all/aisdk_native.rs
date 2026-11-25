use opencode_ai::ProviderPackages;
use opencode_catalog::aisdk_native::{Context, Target, aisdk, is_aisdk, native, package_name, rewrite};

use crate::corpus;

#[test]
fn aisdk_native_corpus_matches_the_pinned_typescript() {
    let cases = corpus::area("aisdk-native");
    assert_eq!(cases.len(), 152, "the corpus holds every pinned aisdk-native case");

    let builtins = ProviderPackages::builtin().specifiers();

    for (name, case) in &cases {
        match case["call"].as_str().expect("a call name") {
            "native" => {
                let npm = case["npm"].as_str().expect("npm");
                let context: Context = serde_json::from_value(case.clone()).expect("a native context");
                let expected = case["result"].as_str().map(str::to_owned);
                assert_eq!(native(npm, &context), expected, "case {name}");
            }
            "rewrite" => {
                let mut target: Target = serde_json::from_value(case["target"].clone()).expect("a target");
                let input: Context = serde_json::from_value(case["input"].clone()).expect("an input");
                rewrite(&mut target, &input, &builtins);
                assert_eq!(serde_json::to_value(&target).expect("a serialized target"), case["result"], "case {name}");
            }
            call => panic!("case {name} has an unknown call {call}"),
        }
    }
}

#[test]
fn package_name_migrates_and_strips_the_prefix() {
    assert_eq!(package_name("@opencode-ai/ai/providers/anthropic"), "@opencode/ai/providers/anthropic");
    assert_eq!(package_name("aisdk:@ai-sdk/openai"), "@ai-sdk/openai");
    assert_eq!(package_name("@ai-sdk/openai"), "@ai-sdk/openai");
    assert_eq!(package_name(&aisdk("@ai-sdk/openai")), "@ai-sdk/openai");
    assert!(is_aisdk("aisdk:@ai-sdk/openai"));
    assert!(!is_aisdk("@ai-sdk/openai"));
    assert_eq!(aisdk("@ai-sdk/openai"), "aisdk:@ai-sdk/openai");
    assert_eq!(aisdk("aisdk:@ai-sdk/openai"), "aisdk:@ai-sdk/openai");
}
