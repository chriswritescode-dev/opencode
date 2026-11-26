use opencode_catalog::integration::Implementation;
use opencode_contract::codec::JsonMap;
use opencode_contract::generated::{
    CredentialExternal, CredentialExternalType, CredentialValue, FormField1, FormStringField1, IntegrationEnvMethod,
    IntegrationEnvMethodType, IntegrationID, IntegrationKeyMethod, IntegrationKeyMethodType, IntegrationMethod,
    IntegrationMethodID, ProviderID, ProviderInfo, ProviderInfoActivation, ProviderSettings,
};
use serde_json::json;

use crate::builtin::{Harness, Input, start};
use crate::provider_plugin::support::{plugin_context, provider_settings_from, settings_json};

const PACKAGE: &str = "@opencode/ai/providers/amazon-bedrock";

const AMBIENT_VARIABLES: [&str; 5] = [
    "AWS_PROFILE",
    "AWS_ACCESS_KEY_ID",
    "AWS_WEB_IDENTITY_TOKEN_FILE",
    "AWS_CONTAINER_CREDENTIALS_RELATIVE_URI",
    "AWS_CONTAINER_CREDENTIALS_FULL_URI",
];

async fn seed(
    harness: &Harness,
    id: &str,
    package: &str,
    settings: Option<ProviderSettings>,
) -> opencode_location::state::Registration {
    let id = ProviderID(id.to_owned());
    let package = package.to_owned();
    harness
        .services
        .provider
        .transform(move |editor| {
            editor.update(&id, |provider| {
                provider.package = package.clone();
                if let Some(settings) = settings.clone() {
                    provider.settings = Some(settings);
                }
            });
            Ok(())
        })
        .await
}

async fn provider(harness: &Harness, id: &str) -> ProviderInfo {
    harness.services.provider.get(&ProviderID(id.to_owned())).await.unwrap().unwrap()
}

async fn profile_field(harness: &Harness) -> FormStringField1 {
    let integration =
        harness.services.integration.get(&IntegrationID("amazon-bedrock".to_owned())).await.unwrap().unwrap();
    let external = integration
        .methods
        .into_iter()
        .find_map(|method| match method {
            IntegrationMethod::External(external) if external.id.0 == "aws-profile" => Some(external),
            _ => None,
        })
        .expect("the aws-profile method is registered");
    match external.form.expect("the aws-profile form").0.into_iter().next().expect("the profile field") {
        FormField1::String(field) => field,
        other => panic!("expected a string field, found {other:?}"),
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn moves_endpoint_setting_to_base_url() {
    let mut harness = start(Input::new(Vec::new())).await;
    let _seeded = seed(
        &harness,
        "amazon-bedrock",
        PACKAGE,
        Some(provider_settings_from(json!({"endpoint": "https://bedrock.example"}))),
    )
    .await;
    let context = plugin_context(&harness);
    harness.register_provider_plugins(&["amazon_bedrock"], context).await;

    let provider = provider(&harness, "amazon-bedrock").await;
    assert_eq!(provider.package, PACKAGE);
    assert_eq!(settings_json(&provider), Some(json!({"baseURL": "https://bedrock.example", "region": "us-east-1"})));
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn gives_the_package_the_profile_of_a_stored_aws_profile_login() {
    let mut harness = start(Input::new(Vec::new())).await;
    let _seeded = seed(&harness, "amazon-bedrock", PACKAGE, None).await;
    harness
        .credential(
            "amazon-bedrock",
            CredentialValue::External(CredentialExternal {
                r#type: CredentialExternalType::External,
                method_id: IntegrationMethodID("aws-profile".to_owned()),
                metadata: Some(JsonMap(vec![("profile".to_owned(), json!("work"))])),
            }),
        )
        .await;
    let context = plugin_context(&harness);
    harness.register_provider_plugins(&["amazon_bedrock"], context).await;

    let provider = provider(&harness, "amazon-bedrock").await;
    assert_eq!(settings_json(&provider), Some(json!({"profile": "work", "region": "us-east-1"})));
    assert_eq!(provider.activation, ProviderInfoActivation::Enabled);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn keeps_an_explicit_base_url_over_endpoint() {
    let mut harness = start(Input::new(Vec::new())).await;
    let _seeded = seed(
        &harness,
        "amazon-bedrock",
        PACKAGE,
        Some(provider_settings_from(
            json!({"baseURL": "https://base.example", "endpoint": "https://endpoint.example"}),
        )),
    )
    .await;
    let context = plugin_context(&harness);
    harness.register_provider_plugins(&["amazon_bedrock"], context).await;

    assert_eq!(
        settings_json(&provider(&harness, "amazon-bedrock").await),
        Some(json!({"baseURL": "https://base.example", "region": "us-east-1"}))
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn only_treats_the_bearer_token_env_var_as_a_key_credential() {
    let mut harness = start(Input::new(Vec::new())).await;
    let _seeded = harness
        .services
        .integration
        .transform(|editor| {
            let integration_id = IntegrationID("amazon-bedrock".to_owned());
            editor.method_update(Implementation::Method {
                integration_id: integration_id.clone(),
                method: IntegrationMethod::Key(IntegrationKeyMethod {
                    r#type: IntegrationKeyMethodType::Key,
                    label: None,
                    form: None,
                }),
            });
            editor.method_update(Implementation::Method {
                integration_id,
                method: IntegrationMethod::Env(IntegrationEnvMethod {
                    r#type: IntegrationEnvMethodType::Env,
                    names: vec![
                        "AWS_ACCESS_KEY_ID".to_owned(),
                        "AWS_SECRET_ACCESS_KEY".to_owned(),
                        "AWS_REGION".to_owned(),
                        "AWS_BEARER_TOKEN_BEDROCK".to_owned(),
                    ],
                }),
            });
            Ok(())
        })
        .await;
    let context = plugin_context(&harness);
    harness.register_provider_plugins(&["amazon_bedrock"], context).await;

    let integration =
        harness.services.integration.get(&IntegrationID("amazon-bedrock".to_owned())).await.unwrap().unwrap();
    let env_methods = integration
        .methods
        .into_iter()
        .filter_map(|method| match method {
            IntegrationMethod::Env(env) => Some(env.names),
            _ => None,
        })
        .collect::<Vec<_>>();
    assert_eq!(env_methods, vec![vec!["AWS_BEARER_TOKEN_BEDROCK".to_owned()]]);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn leaves_activation_on_auto_without_ambient_aws_configuration() {
    let mut harness = start(Input::new(Vec::new())).await;
    let _seeded = seed(&harness, "amazon-bedrock", PACKAGE, None).await;
    let context = plugin_context(&harness);
    harness.register_provider_plugins(&["amazon_bedrock"], context).await;

    assert_eq!(provider(&harness, "amazon-bedrock").await.activation, ProviderInfoActivation::Auto);
    let field = profile_field(&harness).await;
    assert_eq!(field.description, Some("No profiles found in your AWS configuration.".to_owned()));
    assert!(field.options.expect("the profile options").is_empty());
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn enables_the_provider_when_an_ambient_aws_variable_is_set() {
    for name in AMBIENT_VARIABLES {
        let mut harness = start(Input::new(Vec::new()).environment(name, "value")).await;
        let _seeded = seed(&harness, "amazon-bedrock", PACKAGE, None).await;
        let context = plugin_context(&harness);
        harness.register_provider_plugins(&["amazon_bedrock"], context).await;
        assert_eq!(
            provider(&harness, "amazon-bedrock").await.activation,
            ProviderInfoActivation::Enabled,
            "{name} enables the provider"
        );
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn enables_the_provider_when_a_profile_is_configured() {
    let mut harness = start(Input::new(Vec::new())).await;
    let _seeded =
        seed(&harness, "amazon-bedrock", PACKAGE, Some(provider_settings_from(json!({"profile": "work"})))).await;
    let context = plugin_context(&harness);
    harness.register_provider_plugins(&["amazon_bedrock"], context).await;

    let provider = provider(&harness, "amazon-bedrock").await;
    assert_eq!(provider.activation, ProviderInfoActivation::Enabled);
    assert_eq!(settings_json(&provider), Some(json!({"profile": "work", "region": "us-east-1"})));
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn does_not_override_a_disabled_provider() {
    let mut harness = start(Input::new(Vec::new()).environment("AWS_PROFILE", "work")).await;
    let _seeded = seed(&harness, "amazon-bedrock", PACKAGE, None).await;
    let _disabled = harness
        .services
        .provider
        .transform(|editor| {
            editor.update(&ProviderID("amazon-bedrock".to_owned()), |provider| {
                provider.activation = ProviderInfoActivation::Disabled;
            });
            Ok(())
        })
        .await;
    let context = plugin_context(&harness);
    harness.register_provider_plugins(&["amazon_bedrock"], context).await;

    assert_eq!(provider(&harness, "amazon-bedrock").await.activation, ProviderInfoActivation::Disabled);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn fills_region_from_aws_region_then_aws_default_region_without_overriding_config() {
    let mut harness = start(
        Input::new(Vec::new()).environment("AWS_REGION", "eu-west-1").environment("AWS_DEFAULT_REGION", "us-west-2"),
    )
    .await;
    let _seeded = seed(&harness, "amazon-bedrock", PACKAGE, None).await;
    let context = plugin_context(&harness);
    harness.register_provider_plugins(&["amazon_bedrock"], context).await;

    assert_eq!(settings_json(&provider(&harness, "amazon-bedrock").await), Some(json!({"region": "eu-west-1"})));

    let _configured = harness
        .services
        .provider
        .transform(|editor| {
            editor.update(&ProviderID("amazon-bedrock".to_owned()), |provider| {
                provider.settings = Some(provider_settings_from(json!({"region": "ap-southeast-2"})));
            });
            Ok(())
        })
        .await;
    assert_eq!(settings_json(&provider(&harness, "amazon-bedrock").await), Some(json!({"region": "ap-southeast-2"})));
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn falls_back_to_aws_default_region_then_us_east_1() {
    let mut harness = start(Input::new(Vec::new()).environment("AWS_DEFAULT_REGION", "us-west-2")).await;
    let _seeded = seed(&harness, "amazon-bedrock", PACKAGE, None).await;
    let context = plugin_context(&harness);
    harness.register_provider_plugins(&["amazon_bedrock"], context).await;
    assert_eq!(settings_json(&provider(&harness, "amazon-bedrock").await), Some(json!({"region": "us-west-2"})));

    let mut fallback = start(Input::new(Vec::new())).await;
    let _seeded = seed(&fallback, "amazon-bedrock", PACKAGE, None).await;
    let context = plugin_context(&fallback);
    fallback.register_provider_plugins(&["amazon_bedrock"], context).await;
    assert_eq!(settings_json(&provider(&fallback, "amazon-bedrock").await), Some(json!({"region": "us-east-1"})));
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn applies_to_mantle_and_native_bedrock_packages() {
    let mut harness = start(Input::new(Vec::new()).environment("AWS_PROFILE", "work")).await;
    let _mantle = seed(&harness, "mantle", "@opencode/ai/providers/amazon-bedrock/mantle/responses", None).await;
    let _native = seed(&harness, "native", PACKAGE, None).await;
    let _other = seed(&harness, "other", "@opencode/ai/providers/anthropic", None).await;
    let context = plugin_context(&harness);
    harness.register_provider_plugins(&["amazon_bedrock"], context).await;

    assert_eq!(provider(&harness, "mantle").await.activation, ProviderInfoActivation::Enabled);
    assert_eq!(provider(&harness, "native").await.activation, ProviderInfoActivation::Enabled);
    assert_eq!(provider(&harness, "other").await.activation, ProviderInfoActivation::Auto);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn lists_profiles_from_the_aws_config_and_credentials_files() {
    let mut harness = start(Input::new(Vec::new())).await;
    let home = harness.root().join("home");
    let aws = home.join(".aws");
    std::fs::create_dir_all(&aws).expect("create the aws directory");
    std::fs::write(aws.join("config"), "[default]\n[profile work]\n[sso-session corp]\n").expect("write the config");
    std::fs::write(aws.join("credentials"), "[personal]\n[work]\n").expect("write the credentials");
    harness.environment.insert("HOME".to_owned(), home.to_string_lossy().into_owned());

    let context = plugin_context(&harness);
    harness.register_provider_plugins(&["amazon_bedrock"], context).await;

    let field = profile_field(&harness).await;
    let values = field.options.expect("the profile options").into_iter().map(|option| option.value).collect::<Vec<_>>();
    assert_eq!(values, vec!["default".to_owned(), "personal".to_owned(), "work".to_owned()]);
    assert_eq!(field.description, Some("Found 3 profiles in your AWS configuration.".to_owned()));
}
