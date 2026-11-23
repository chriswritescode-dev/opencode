pub mod checks;
pub mod codec;
pub mod generated;
pub mod request;
pub mod route;

pub use route::{BodyKind, Method, Route, Scope, Stability, SuccessKind, route};

pub const OPENAPI_JSON: &str = include_str!("../openapi.json");
pub const SERVED_OPENAPI_JSON: &str = include_str!("../served-openapi.json");
