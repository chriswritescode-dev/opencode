use opencode_contract::codec::{JsonMap, decode_json, encode_json};
use opencode_contract::generated::{
    CredentialExternal, CredentialExternalType, CredentialValue, FormField1, FormStringField1, IntegrationID,
    IntegrationMethod, IntegrationMethodID, ModelInfo, ModelSettings, ProviderID, ProviderInfo, ProviderInfoActivation,
    ProviderSettings,
};
use serde_json::{Value, json};

use crate::builtin::{Harness, Input, start};
use crate::provider_plugin::support::{plugin_context, provider_settings_from, settings_json, wait_for_setting};

const VERTEX: &str = "@opencode/ai/providers/google-vertex";

const VERTEX_CHAT: &str = "@opencode/ai/providers/google-vertex/chat";

const TEMPLATE: &str =
    "https://${GOOGLE_VERTEX_ENDPOINT}/v1/projects/${GOOGLE_VERTEX_PROJECT}/locations/${GOOGLE_VERTEX_LOCATION}";

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

async fn seed_model(
    harness: &Harness,
    provider_id: &str,
    model_id: &str,
    package: &str,
    settings: Value,
) -> opencode_location::state::Registration {
    let provider_id = ProviderID(provider_id.to_owned());
    let model_id = model_id.to_owned();
    let package = package.to_owned();
    let settings: ModelSettings = decode_json(&settings.to_string()).expect("the model settings decode");
    harness
        .services
        .provider
        .transform(move |editor| {
            editor.models_update(&provider_id, &model_id, |model| {
                model.package = Some(package.clone());
                model.settings = Some(settings.clone());
            });
            Ok(())
        })
        .await
}

async fn provider(harness: &Harness, id: &str) -> ProviderInfo {
    harness.services.provider.get(&ProviderID(id.to_owned())).await.unwrap().unwrap()
}

fn model_settings_json(model: &ModelInfo) -> Option<Value> {
    model.settings.as_ref().map(|settings| serde_json::from_str(&encode_json(settings)).expect("the model settings"))
}

fn settings(provider: &ProviderInfo) -> Value {
    settings_json(provider).expect("the provider settings")
}

async fn integration_fields(harness: &Harness) -> (Vec<String>, Vec<FormStringField1>) {
    let integration =
        harness.services.integration.get(&IntegrationID("google-vertex".to_owned())).await.unwrap().unwrap();
    let env = integration
        .methods
        .iter()
        .find_map(|method| match method {
            IntegrationMethod::Env(env) => Some(env.names.clone()),
            _ => None,
        })
        .expect("the env method is registered");
    let fields = integration
        .methods
        .into_iter()
        .find_map(|method| match method {
            IntegrationMethod::External(external) if external.id.0 == "google-adc" => Some(external.form),
            _ => None,
        })
        .expect("the google-adc method is registered")
        .expect("the google-adc form")
        .0
        .into_iter()
        .map(|field| match field {
            FormField1::String(field) => field,
            other => panic!("expected a string field, found {other:?}"),
        })
        .collect();
    (env, fields)
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn ignores_openai_compatible_providers_that_are_not_google_vertex() {
    let mut harness = start(Input::new(Vec::new())).await;
    let _seeded = seed(
        &harness,
        "opencode",
        "@opencode/ai/providers/openai-compatible",
        Some(provider_settings_from(json!({"baseURL": "https://opencode.ai/zen/v1"}))),
    )
    .await;
    let context = plugin_context(&harness);
    harness.register_provider_plugins(&["google_vertex"], context).await;

    assert_eq!(
        settings_json(&provider(&harness, "opencode").await),
        Some(json!({"baseURL": "https://opencode.ai/zen/v1"}))
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn resolves_project_and_location_from_env_using_legacy_precedence() {
    let mut harness = start(
        Input::new(Vec::new())
            .environment("GOOGLE_CLOUD_PROJECT", "google-cloud-project")
            .environment("GCP_PROJECT", "gcp-project")
            .environment("GCLOUD_PROJECT", "gcloud-project")
            .environment("GOOGLE_VERTEX_LOCATION", "google-vertex-location")
            .environment("GOOGLE_CLOUD_LOCATION", "google-cloud-location")
            .environment("VERTEX_LOCATION", "vertex-location"),
    )
    .await;
    let _seeded =
        seed(&harness, "google-vertex", VERTEX_CHAT, Some(provider_settings_from(json!({"baseURL": TEMPLATE})))).await;
    let context = plugin_context(&harness);
    harness.register_provider_plugins(&["google_vertex"], context).await;

    let provider = provider(&harness, "google-vertex").await;
    assert_eq!(settings(&provider).get("project").and_then(Value::as_str), Some("google-cloud-project"));
    assert_eq!(settings(&provider).get("location").and_then(Value::as_str), Some("google-vertex-location"));
    assert_eq!(
        settings_json(&provider),
        Some(json!({
            "baseURL": "https://google-vertex-location-aiplatform.googleapis.com/v1/projects/google-cloud-project/locations/google-vertex-location",
            "project": "google-cloud-project",
            "location": "google-vertex-location",
        }))
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn enables_the_provider_when_a_project_resolves_and_leaves_it_automatic_otherwise() {
    let mut harness = start(Input::new(Vec::new())).await;
    let _seeded = seed(&harness, "google-vertex", VERTEX, None).await;
    let context = plugin_context(&harness);
    harness.register_provider_plugins(&["google_vertex"], context).await;

    assert_eq!(provider(&harness, "google-vertex").await.activation, ProviderInfoActivation::Auto);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn enables_the_provider_when_a_project_resolves_from_env() {
    let mut harness = start(Input::new(Vec::new()).environment("GOOGLE_CLOUD_PROJECT", "adc-project")).await;
    let _seeded = seed(&harness, "google-vertex", VERTEX, None).await;
    let context = plugin_context(&harness);
    harness.register_provider_plugins(&["google_vertex"], context).await;

    assert_eq!(provider(&harness, "google-vertex").await.activation, ProviderInfoActivation::Enabled);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn resolves_the_advertised_google_vertex_project_env_for_provider_updates_and_sdks() {
    let mut harness = start(
        Input::new(Vec::new())
            .environment("GOOGLE_VERTEX_PROJECT", "vertex-project")
            .environment("GOOGLE_VERTEX_LOCATION", "europe-west4"),
    )
    .await;
    let _seeded =
        seed(&harness, "google-vertex", VERTEX_CHAT, Some(provider_settings_from(json!({"baseURL": TEMPLATE})))).await;
    let context = plugin_context(&harness);
    harness.register_provider_plugins(&["google_vertex"], context).await;

    let provider = provider(&harness, "google-vertex").await;
    assert_eq!(settings(&provider).get("project").and_then(Value::as_str), Some("vertex-project"));
    assert_eq!(
        settings_json(&provider),
        Some(json!({
            "baseURL": "https://europe-west4-aiplatform.googleapis.com/v1/projects/vertex-project/locations/europe-west4",
            "project": "vertex-project",
            "location": "europe-west4",
        }))
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn keeps_configured_project_and_location_over_env_and_uses_global_endpoint() {
    let mut harness = start(
        Input::new(Vec::new())
            .environment("GOOGLE_CLOUD_PROJECT", "env-project")
            .environment("GCP_PROJECT", "env-gcp-project")
            .environment("GCLOUD_PROJECT", "env-gcloud-project")
            .environment("GOOGLE_VERTEX_LOCATION", "env-location")
            .environment("GOOGLE_CLOUD_LOCATION", "env-google-cloud-location")
            .environment("VERTEX_LOCATION", "env-vertex-location"),
    )
    .await;
    let provider_id = ProviderID("google-vertex".to_owned());
    let _seeded = harness
        .services
        .provider
        .transform(move |editor| {
            editor.update(&provider_id, |provider| {
                provider.package = VERTEX_CHAT.to_owned();
                provider.settings = Some(provider_settings_from(json!({
                    "baseURL": TEMPLATE,
                    "project": "config-project",
                    "location": "global",
                })));
            });
            editor.models_update(&provider_id, "gemini", |_| {});
            Ok(())
        })
        .await;
    let context = plugin_context(&harness);
    harness.register_provider_plugins(&["google_vertex"], context).await;

    let provider = provider(&harness, "google-vertex").await;
    let model = harness.services.model.get(&ProviderID("google-vertex".to_owned()), "gemini").await.unwrap().unwrap();
    assert_eq!(settings(&provider).get("project").and_then(Value::as_str), Some("config-project"));
    assert_eq!(settings(&provider).get("location").and_then(Value::as_str), Some("global"));
    assert_eq!(
        settings_json(&provider),
        Some(json!({
            "baseURL": "https://aiplatform.googleapis.com/v1/projects/config-project/locations/global",
            "project": "config-project",
            "location": "global",
        }))
    );
    assert_eq!(model_settings_json(&model), settings_json(&provider));
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn expands_vertex_endpoint_templates_to_the_multi_region_host() {
    for location in ["eu", "us"] {
        let mut harness = start(Input::new(Vec::new())).await;
        let _seeded = seed(
            &harness,
            "google-vertex",
            VERTEX_CHAT,
            Some(provider_settings_from(json!({
                "baseURL": TEMPLATE,
                "project": "config-project",
                "location": location,
            }))),
        )
        .await;
        let context = plugin_context(&harness);
        harness.register_provider_plugins(&["google_vertex"], context).await;

        assert_eq!(
            settings_json(&provider(&harness, "google-vertex").await),
            Some(json!({
                "baseURL": format!("https://aiplatform.{location}.rep.googleapis.com/v1/projects/config-project/locations/{location}"),
                "project": "config-project",
                "location": location,
            })),
            "{location}"
        );
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn expands_endpoint_templates_on_vertex_chat_models() {
    let mut harness = start(Input::new(Vec::new())).await;
    let model_id = "meta/llama-maas";
    let _seeded = seed(
        &harness,
        "google-vertex",
        VERTEX,
        Some(provider_settings_from(json!({"project": "config-project", "location": "eu"}))),
    )
    .await;
    let _model = seed_model(
        &harness,
        "google-vertex",
        model_id,
        VERTEX_CHAT,
        json!({
            "baseURL": "https://${GOOGLE_VERTEX_ENDPOINT}/v1/projects/${GOOGLE_VERTEX_PROJECT}/locations/${GOOGLE_VERTEX_LOCATION}/endpoints/openapi",
        }),
    )
    .await;
    let context = plugin_context(&harness);
    harness.register_provider_plugins(&["google_vertex"], context).await;

    let model = harness.services.model.get(&ProviderID("google-vertex".to_owned()), model_id).await.unwrap().unwrap();
    assert_eq!(model.package.as_deref(), Some(VERTEX_CHAT));
    assert_eq!(
        model_settings_json(&model),
        Some(json!({
            "baseURL": "https://aiplatform.eu.rep.googleapis.com/v1/projects/config-project/locations/eu/endpoints/openapi",
            "project": "config-project",
            "location": "eu",
        }))
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn defaults_location_to_global_when_only_project_is_configured() {
    let mut harness = start(Input::new(Vec::new())).await;
    let _seeded =
        seed(&harness, "google-vertex", VERTEX, Some(provider_settings_from(json!({"project": "config-project"}))))
            .await;
    let context = plugin_context(&harness);
    harness.register_provider_plugins(&["google_vertex"], context).await;

    let provider = provider(&harness, "google-vertex").await;
    assert_eq!(settings(&provider).get("project").and_then(Value::as_str), Some("config-project"));
    assert_eq!(settings(&provider).get("location").and_then(Value::as_str), Some("global"));
}

fn adc(project: &str, location: &str) -> CredentialValue {
    CredentialValue::External(CredentialExternal {
        r#type: CredentialExternalType::External,
        method_id: IntegrationMethodID("google-adc".to_owned()),
        metadata: Some(JsonMap(vec![("project".to_owned(), json!(project)), ("location".to_owned(), json!(location))])),
    })
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn adopts_a_vertex_adc_login_and_reloads_when_it_switches() {
    let mut harness = start(Input::new(Vec::new())).await;
    let _seeded =
        seed(&harness, "google-vertex", VERTEX_CHAT, Some(provider_settings_from(json!({"baseURL": TEMPLATE})))).await;
    let context = plugin_context(&harness);
    harness.register_provider_plugins(&["google_vertex"], context).await;

    harness.credential("google-vertex", adc("adc-project", "europe-west4")).await;
    let provider = wait_for_setting(&harness, "google-vertex", "project", "adc-project").await;
    assert_eq!(provider.activation, ProviderInfoActivation::Enabled);
    assert_eq!(
        settings(&provider),
        json!({
            "baseURL": "https://europe-west4-aiplatform.googleapis.com/v1/projects/adc-project/locations/europe-west4",
            "project": "adc-project",
            "location": "europe-west4",
        })
    );

    harness.credential("google-vertex", adc("next-project", "us-central1")).await;
    let provider = wait_for_setting(&harness, "google-vertex", "project", "next-project").await;
    assert_eq!(
        settings(&provider),
        json!({
            "baseURL": "https://us-central1-aiplatform.googleapis.com/v1/projects/next-project/locations/us-central1",
            "project": "next-project",
            "location": "us-central1",
        })
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn registers_the_vertex_env_method_and_lists_gcloud_projects_and_locations() {
    let mut harness = start(Input::new(Vec::new())).await;
    let home = harness.root().join("home");
    let gcloud = home.join(".config").join("gcloud");
    let configurations = gcloud.join("configurations");
    std::fs::create_dir_all(&configurations).expect("create the gcloud configurations directory");
    std::fs::write(gcloud.join("active_config"), "work\n").expect("write the active config");
    std::fs::write(configurations.join("config_work"), "project = work-project\nregion = europe-west4\n")
        .expect("write the work config");
    std::fs::write(configurations.join("config_default"), "project = default-project\nregion = us-central1\n")
        .expect("write the default config");
    std::fs::write(
        gcloud.join("application_default_credentials.json"),
        json!({"quota_project_id": "quota-project"}).to_string(),
    )
    .expect("write the application default credentials");
    harness.environment.insert("HOME".to_owned(), home.to_string_lossy().into_owned());

    let context = plugin_context(&harness);
    harness.register_provider_plugins(&["google_vertex"], context).await;

    let (env, fields) = integration_fields(&harness).await;
    assert_eq!(env, ["GOOGLE_VERTEX_API_KEY"]);
    let project = &fields[0];
    assert_eq!(project.key, "project");
    assert_eq!(project.default, Some("work-project".to_owned()));
    assert_eq!(
        project
            .options
            .as_ref()
            .expect("the project options")
            .iter()
            .map(|option| option.value.clone())
            .collect::<Vec<_>>(),
        ["work-project", "default-project", "quota-project"]
    );
    assert_eq!(project.description, Some("Found 3 projects in your gcloud configuration.".to_owned()));
    let location = &fields[1];
    assert_eq!(location.key, "location");
    assert_eq!(location.default, Some("global".to_owned()));
    assert_eq!(
        location
            .options
            .as_ref()
            .expect("the location options")
            .iter()
            .map(|option| option.value.clone())
            .collect::<Vec<_>>(),
        ["global", "us", "eu", "europe-west4", "us-central1"]
    );
}
