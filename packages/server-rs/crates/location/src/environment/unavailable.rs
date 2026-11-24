use futures_util::future::BoxFuture;

use super::{CommandOutput, CommandSpec, EnvironmentError, Spawner};

pub const MESSAGE: &str =
    "This location has no execution plane: no workspace is attached and the host cannot spawn processes";

#[derive(Clone, Copy, Debug, Default)]
pub struct UnavailableSpawner;

impl UnavailableSpawner {
    pub fn new() -> Self {
        UnavailableSpawner
    }
}

impl Spawner for UnavailableSpawner {
    fn spawn<'a>(&'a self, command: &'a CommandSpec) -> BoxFuture<'a, Result<CommandOutput, EnvironmentError>> {
        Box::pin(async move { Err(EnvironmentError::failed(command.program.clone(), MESSAGE)) })
    }
}

pub fn spawner() -> UnavailableSpawner {
    UnavailableSpawner::new()
}
