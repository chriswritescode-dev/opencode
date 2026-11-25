use tokio::sync::oneshot;
use url::Url;

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Pasted {
    Code { code: String, params: Vec<(String, String)> },
    Error { error: String, params: Vec<(String, String)> },
}

#[derive(Debug, PartialEq, Eq, thiserror::Error)]
pub enum RedirectError {
    #[error("Paste the full URL from the browser's address bar")]
    NotAUrl,
    #[error("This URL belongs to a different sign-in")]
    WrongState,
    #[error("The URL does not contain an authorization code")]
    MissingCode,
}

pub fn pasted(text: &str, state: &str) -> Result<Pasted, RedirectError> {
    let url = Url::parse(text.trim()).map_err(|_| RedirectError::NotAUrl)?;
    let params = url.query_pairs().map(|(key, value)| (key.into_owned(), value.into_owned())).collect::<Vec<_>>();

    if first(&params, "state") != Some(state) {
        return Err(RedirectError::WrongState);
    }

    let failure = first(&params, "error_description").or_else(|| first(&params, "error"));
    if let Some(value) = failure.filter(|value| !value.is_empty()) {
        return Ok(Pasted::Error { error: value.to_owned(), params });
    }

    let code = first(&params, "code").filter(|code| !code.is_empty()).ok_or(RedirectError::MissingCode)?.to_owned();

    Ok(Pasted::Code { code, params })
}

pub fn submit(
    state: &str,
    text: &str,
    callback: &mut Option<oneshot::Sender<Result<String, String>>>,
) -> Result<(), RedirectError> {
    match pasted(text, state)? {
        Pasted::Code { code, .. } => {
            if let Some(callback) = callback.take() {
                let _ = callback.send(Ok(code));
            }
        }
        Pasted::Error { error, .. } => {
            if let Some(callback) = callback.take() {
                let _ = callback.send(Err(error));
            }
        }
    }

    Ok(())
}

fn first<'a>(params: &'a [(String, String)], key: &str) -> Option<&'a str> {
    params.iter().find(|(name, _)| name.as_str() == key).map(|(_, value)| value.as_str())
}
