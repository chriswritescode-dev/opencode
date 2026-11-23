#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum Method {
    Get,
    Put,
    Post,
    Delete,
    Options,
    Head,
    Patch,
    Trace,
}

impl Method {
    pub fn as_str(self) -> &'static str {
        match self {
            Method::Get => "GET",
            Method::Put => "PUT",
            Method::Post => "POST",
            Method::Delete => "DELETE",
            Method::Options => "OPTIONS",
            Method::Head => "HEAD",
            Method::Patch => "PATCH",
            Method::Trace => "TRACE",
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum Scope {
    Global,
    Location,
    Session,
    Form,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum Stability {
    Stable,
    Experimental,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum BodyKind {
    None,
    Json,
    Binary,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum SuccessKind {
    Json,
    Data,
    Empty,
    Binary,
    Sse,
    WebSocket,
}

pub struct Route {
    pub operation_id: &'static str,
    pub identifier: &'static str,
    pub group: &'static str,
    pub tag: &'static str,
    pub method: Method,
    pub path: &'static str,
    pub scope: Scope,
    pub stability: Stability,
    pub body: BodyKind,
    pub success: SuccessKind,
    pub success_status: u16,
    pub wildcard: bool,
    pub errors: &'static [(&'static str, u16)],
}

pub fn route(operation_id: &str) -> Option<&'static Route> {
    crate::generated::ROUTES.iter().find(|route| route.operation_id == operation_id)
}
