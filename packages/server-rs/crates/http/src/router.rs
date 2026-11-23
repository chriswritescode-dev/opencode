use std::sync::OnceLock;

use opencode_api_types::js::decode_component;
use opencode_contract::Route;

pub struct Match {
    pub route: &'static Route,
    pub params: Vec<(String, String)>,
}

enum Segment {
    Static(&'static str),
    Param(&'static str),
    Wildcard,
}

struct Template {
    route: &'static Route,
    segments: Vec<Segment>,
}

struct Candidate {
    ranks: Vec<u8>,
    params: Vec<(String, String)>,
}

const WILDCARD: &str = "*";
const MAX_PARAM_LENGTH: usize = 100;

pub fn find(method: &http::Method, path: &str) -> Option<Match> {
    let segments = segments(path)?;
    best(method, &segments)
        .or_else(|| if method == http::Method::HEAD { best(&http::Method::GET, &segments) } else { None })
}

pub fn matches_static(path: &str, route_path: &str) -> bool {
    let Some(segments) = segments(path) else {
        return false;
    };
    let expected = route_path.strip_prefix('/').unwrap_or(route_path).split('/').collect::<Vec<_>>();
    segments.len() == expected.len()
        && segments.iter().zip(expected).all(|(segment, expected)| segment.eq_ignore_ascii_case(expected))
}

fn segments(path: &str) -> Option<Vec<&str>> {
    Some(path.strip_prefix('/')?.split('/').filter(|segment| !segment.is_empty()).collect())
}

fn best(method: &http::Method, segments: &[&str]) -> Option<Match> {
    let mut best: Option<(Vec<u8>, Match)> = None;
    for template in templates() {
        if template.route.method.as_str() != method.as_str() {
            continue;
        }
        let Some(candidate) = template.matches(segments) else { continue };
        if best.as_ref().is_none_or(|(best_ranks, _)| candidate.ranks < *best_ranks) {
            best = Some((candidate.ranks, Match { route: template.route, params: candidate.params }));
        }
    }
    best.map(|(_, matched)| matched)
}

fn templates() -> &'static [Template] {
    static TEMPLATES: OnceLock<Vec<Template>> = OnceLock::new();
    TEMPLATES.get_or_init(|| {
        opencode_contract::generated::ROUTES
            .iter()
            .map(|route| Template { route, segments: compile(route.path) })
            .collect()
    })
}

fn compile(path: &'static str) -> Vec<Segment> {
    path.strip_prefix('/')
        .unwrap_or(path)
        .split('/')
        .map(|segment| match segment.strip_prefix('{').and_then(|name| name.strip_suffix('}')) {
            Some(name) => Segment::Param(name),
            None if segment == WILDCARD => Segment::Wildcard,
            None => Segment::Static(segment),
        })
        .collect()
}

impl Template {
    fn matches(&self, segments: &[&str]) -> Option<Candidate> {
        let mut ranks = Vec::with_capacity(self.segments.len());
        let mut params = Vec::new();
        let mut index = 0;
        for segment in &self.segments {
            match segment {
                Segment::Static(value) => {
                    if !segments.get(index).is_some_and(|segment| segment.eq_ignore_ascii_case(value)) {
                        return None;
                    }
                    ranks.push(0);
                    index += 1;
                }
                Segment::Param(name) => {
                    let value = decode(segments.get(index)?);
                    if value.encode_utf16().count() > MAX_PARAM_LENGTH {
                        return None;
                    }
                    params.push(((*name).to_owned(), value));
                    ranks.push(1);
                    index += 1;
                }
                Segment::Wildcard => {
                    let rest = segments.get(index..)?.join("/");
                    if rest.is_empty() {
                        return None;
                    }
                    params.push((WILDCARD.to_owned(), decode(&rest)));
                    ranks.push(2);
                    index = segments.len();
                }
            }
        }
        (index == segments.len()).then_some(Candidate { ranks, params })
    }
}

fn decode(value: &str) -> String {
    decode_component(value).unwrap_or_else(|| value.to_owned())
}
