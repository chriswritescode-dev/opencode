use std::sync::Arc;

use opencode_config::paths::GlobalPaths;
use opencode_fs::filesystem::{EntryKind, LocationFileSystem};
use opencode_fs::ripgrep::Ripgrep;
use opencode_fs::ripgrep::binary::{Binary, releases};
use opencode_fs::search::fuzzysort::{self, Prepared};
use opencode_fs::search::{DEFAULT_LIMIT, FindInput, FindKind, Search, SearchOptions};
use serde_json::{Value, json};

use crate::fixture::{self, TempDir};

#[test]
fn fuzzysort_corpus_matches_expected_output() {
    for case in fixture::cases("fuzzysort") {
        let name = fixture::field(&case, "name");
        let query = fixture::field(&case, "query");
        let targets: Vec<String> = case["targets"]
            .as_array()
            .unwrap_or_else(|| panic!("case {name}: targets is an array"))
            .iter()
            .map(|target| target.as_str().unwrap_or_else(|| panic!("case {name}: target is a string")).to_owned())
            .collect();
        let prepared: Vec<Prepared> = targets.iter().map(|target| fuzzysort::prepare(target)).collect();
        let references: Vec<&Prepared> = prepared.iter().collect();
        let limit = case["limit"].as_u64().map_or(0, |limit| limit as usize);
        let found = fuzzysort::go(query, &references, limit);
        let expected = case["results"].as_array().unwrap_or_else(|| panic!("case {name}: results is an array"));
        assert_eq!(found.len(), expected.len(), "case {name}: result count");
        for (found, expected) in found.iter().zip(expected) {
            assert_eq!(Value::String(found.target.clone()), expected["target"], "case {name}: target");
            assert_eq!(Value::String(format!("{:.9}", found.score)), expected["score"], "case {name}: score");
            assert_eq!(Some(found.raw_score), expected["raw"].as_f64(), "case {name}: raw score for {}", found.target);
            assert_eq!(json!(found.indexes), expected["indexes"], "case {name}: indexes");
        }
    }
}

#[tokio::test]
async fn search_corpus_matches_expected_output() {
    let ripgrep = Arc::new(Ripgrep::new(Binary::new(
        GlobalPaths::from_env().expect("global paths"),
        |name| std::env::var_os(name),
        releases(),
    )));
    for case in fixture::cases("search") {
        let name = fixture::field(&case, "name");
        let temp = TempDir::new(name);
        let directory = temp.path().join(name);
        std::fs::create_dir_all(&directory).expect("creates the case directory");
        fixture::materialize(&directory, &case["fixture"]);
        let text = directory.to_string_lossy().into_owned();
        let search = Search::new(
            ripgrep.clone(),
            SearchOptions { directory: text.clone(), home: "/nonexistent-home".to_owned(), version_controlled: false },
        );
        let filesystem = LocationFileSystem::new(text).await.unwrap_or_else(|error| panic!("case {name}: {error}"));
        for query in case["queries"].as_array().unwrap_or_else(|| panic!("case {name}: queries is an array")) {
            let kind = match query["type"].as_str() {
                Some("file") => FindKind::File,
                Some("directory") => FindKind::Directory,
                _ => FindKind::All,
            };
            let input = FindInput {
                query: query["query"].as_str().unwrap_or_default().to_owned(),
                kind,
                limit: query["limit"].as_u64().map_or(DEFAULT_LIMIT, |limit| limit as usize),
            };
            let entries = filesystem.find(&search, input).await.unwrap_or_else(|error| panic!("case {name}: {error}"));
            let actual: Vec<Value> = entries
                .iter()
                .map(|entry| {
                    json!({
                        "path": entry.path,
                        "type": match entry.kind { EntryKind::File => "file", EntryKind::Directory => "directory" },
                    })
                })
                .collect();
            assert_eq!(Value::Array(actual), query["entries"], "case {name}: query {query}");
        }
    }
}
