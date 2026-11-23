use std::ffi::OsString;

pub const USAGE: &str = "usage: opencode-server <serve|service|--version>";

pub struct Serve {
    pub hostname: Option<String>,
    pub port: Option<u16>,
    pub cors: Vec<String>,
    pub service: bool,
    pub stdio: bool,
}

pub enum Command {
    Version,
    Serve(Serve),
    Service(ServiceCommand),
}

pub enum ServiceCommand {
    Start,
    Restart,
    Status,
    Stop,
    Get { key: Option<String>, name: Option<String> },
    Set { key: String, value: String, nested: Option<String> },
    Unset { key: String, name: Option<String> },
}

pub enum Error {
    Usage,
    Message(String),
}

impl Error {
    pub fn report(&self) {
        match self {
            Error::Usage => eprintln!("{USAGE}"),
            Error::Message(message) => eprintln!("{message}"),
        }
    }
}

pub fn parse(args: impl IntoIterator<Item = OsString>) -> Result<Command, Error> {
    let args =
        args.into_iter().map(|arg| arg.into_string().map_err(|_| Error::Usage)).collect::<Result<Vec<_>, _>>()?;
    let Some((command, rest)) = args.split_first() else {
        return Err(Error::Usage);
    };
    match command.as_str() {
        "--version" if rest.is_empty() => Ok(Command::Version),
        "--version" => Err(Error::Usage),
        "serve" => parse_serve(rest).map(Command::Serve),
        "service" => parse_service(rest),
        _ => Err(Error::Message(format!("unknown command: {command}"))),
    }
}

fn parse_service(args: &[String]) -> Result<Command, Error> {
    let Some((subcommand, rest)) = args.split_first() else {
        return Err(Error::Usage);
    };
    let command = match subcommand.as_str() {
        "start" if rest.is_empty() => ServiceCommand::Start,
        "restart" if rest.is_empty() => ServiceCommand::Restart,
        "status" if rest.is_empty() => ServiceCommand::Status,
        "stop" if rest.is_empty() => ServiceCommand::Stop,
        "start" | "restart" | "status" | "stop" => return Err(Error::Usage),
        "get" => match rest {
            [] => ServiceCommand::Get { key: None, name: None },
            [key] => ServiceCommand::Get { key: Some(key.clone()), name: None },
            [key, name] => ServiceCommand::Get { key: Some(key.clone()), name: Some(name.clone()) },
            _ => return Err(Error::Usage),
        },
        "set" => match rest {
            [key, value] => ServiceCommand::Set { key: key.clone(), value: value.clone(), nested: None },
            [key, value, nested] => {
                ServiceCommand::Set { key: key.clone(), value: value.clone(), nested: Some(nested.clone()) }
            }
            _ => return Err(Error::Usage),
        },
        "unset" => match rest {
            [key] => ServiceCommand::Unset { key: key.clone(), name: None },
            [key, name] => ServiceCommand::Unset { key: key.clone(), name: Some(name.clone()) },
            _ => return Err(Error::Usage),
        },
        _ => return Err(Error::Message(format!("unknown command: {subcommand}"))),
    };
    Ok(Command::Service(command))
}

fn parse_serve(args: &[String]) -> Result<Serve, Error> {
    let mut serve = Serve { hostname: None, port: None, cors: Vec::new(), service: false, stdio: false };
    let mut tokens = args.iter();
    while let Some(token) = tokens.next() {
        let (flag, inline) = split_flag(token);
        match flag {
            "--hostname" => serve.hostname = Some(take_value(inline, &mut tokens)?.to_owned()),
            "--port" => {
                let text = take_value(inline, &mut tokens)?;
                serve.port = Some(text.parse().map_err(|_| Error::Message(format!("invalid port: {text}")))?);
            }
            "--cors" => serve.cors.push(take_value(inline, &mut tokens)?.to_owned()),
            "--service" => {
                take_bare(inline)?;
                serve.service = true;
            }
            "--stdio" => {
                take_bare(inline)?;
                serve.stdio = true;
            }
            _ => return Err(unexpected(flag)),
        }
    }
    if serve.service && serve.stdio {
        return Err(Error::Message("--service and --stdio cannot be combined".to_owned()));
    }
    Ok(serve)
}

fn split_flag(token: &str) -> (&str, Option<&str>) {
    match token.split_once('=') {
        Some((flag, value)) => (flag, Some(value)),
        None => (token, None),
    }
}

fn take_value<'a>(inline: Option<&'a str>, tokens: &mut std::slice::Iter<'a, String>) -> Result<&'a str, Error> {
    match inline {
        Some(value) => Ok(value),
        None => tokens.next().map(String::as_str).ok_or(Error::Usage),
    }
}

fn take_bare(inline: Option<&str>) -> Result<(), Error> {
    match inline {
        Some(_) => Err(Error::Usage),
        None => Ok(()),
    }
}

fn unexpected(token: &str) -> Error {
    if token.starts_with('-') {
        return Error::Message(format!("unknown flag: {token}"));
    }
    Error::Usage
}
