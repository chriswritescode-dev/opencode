use bytes::Bytes;
use futures_util::StreamExt;
use opencode_ai::route::executor::{HttpResponse, HttpSender, PreparedHttp};

const MAX_REDIRECTS: usize = 5;

const REDIRECT_STATUSES: [u16; 5] = [301, 302, 303, 307, 308];

const CREDENTIAL_HEADERS: [&str; 4] = ["authorization", "cookie", "proxy-authorization", "x-org-id"];

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum FollowError {
    Transport(String),
    Status(u16),
    Redirect(String),
}

impl std::fmt::Display for FollowError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Transport(message) => f.write_str(message),
            Self::Status(status) => write!(f, "HTTP status {status}"),
            Self::Redirect(message) => f.write_str(message),
        }
    }
}

impl std::error::Error for FollowError {}

pub async fn follow(
    http: &dyn HttpSender,
    mut request: PreparedHttp,
    mut redirects: usize,
) -> Result<HttpResponse, FollowError> {
    loop {
        let response =
            http.send(request.clone()).await.map_err(|error| FollowError::Transport(error.message().to_owned()))?;
        let location = response
            .headers
            .iter()
            .find(|(name, _)| name.eq_ignore_ascii_case("location"))
            .map(|(_, value)| value.clone());
        let Some(location) = location.filter(|_| REDIRECT_STATUSES.contains(&response.status)) else {
            if !(200..=299).contains(&response.status) {
                return Err(FollowError::Status(response.status));
            }
            return Ok(response);
        };
        if redirects >= MAX_REDIRECTS {
            return Err(FollowError::Redirect(format!("Too many redirects (more than {MAX_REDIRECTS})")));
        }
        let target = redirect_target(&request.url, &location)?;
        let status = response.status;
        drain(response).await;
        request = redirected(request, &target, status);
        redirects += 1;
    }
}

async fn drain(response: HttpResponse) {
    let mut body = response.body;
    while body.next().await.is_some() {}
}

fn redirect_target(base: &str, location: &str) -> Result<url::Url, FollowError> {
    let base =
        url::Url::parse(base).map_err(|error| FollowError::Redirect(format!("Invalid redirect base URL: {error}")))?;
    let target =
        base.join(location).map_err(|error| FollowError::Redirect(format!("Invalid redirect URL: {error}")))?;
    if target.scheme() != "http" && target.scheme() != "https" {
        return Err(FollowError::Redirect(format!("redirect to unsupported protocol {}", target.scheme())));
    }
    Ok(target)
}

fn redirected(request: PreparedHttp, target: &url::Url, status: u16) -> PreparedHttp {
    let get = status == 303 || ((status == 301 || status == 302) && request.method == "POST");
    let same_origin = url::Url::parse(&request.url).is_ok_and(|url| url.origin() == target.origin());
    let headers = request
        .headers
        .into_iter()
        .filter(|(name, _)| same_origin || !CREDENTIAL_HEADERS.iter().any(|header| header.eq_ignore_ascii_case(name)))
        .collect();
    let body = if get { Bytes::new() } else { request.body };
    PreparedHttp::new(if get { "GET" } else { request.method.as_str() }, target.as_str(), headers, body)
}
