pub mod generated;
pub mod route;

pub use route::{BodyKind, Method, Route, Scope, Stability, SuccessKind, route};

pub const OPENAPI_JSON: &str = include_str!("../openapi.json");
