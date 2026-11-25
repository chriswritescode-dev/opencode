use std::sync::Arc;

use opencode_contract::generated::{
    ConnectionCredentialInfo, ConnectionCredentialInfoMethod, ConnectionCredentialInfoType, ConnectionEnvInfo,
    ConnectionEnvInfoType, ConnectionInfo, ConnectionStatus, CredentialID, CredentialKey, CredentialKeyType,
    CredentialValue, IntegrationID, IntegrationMethod,
};

use crate::credential::{self, Update};
use crate::integration::{
    AuthorizationError, ConfigEnv, IntegrationEntry, IntegrationError, IntegrationService, status_key,
};

pub fn key(connection: &ConnectionInfo) -> String {
    match connection {
        ConnectionInfo::Credential(connection) => format!("credential:{}", connection.id.0),
        ConnectionInfo::Env(connection) => format!("env:{}", connection.name),
    }
}

pub(crate) fn with_status(connection: ConnectionInfo, status: Option<ConnectionStatus>) -> ConnectionInfo {
    match connection {
        ConnectionInfo::Credential(mut connection) => {
            connection.status = status;
            ConnectionInfo::Credential(connection)
        }
        ConnectionInfo::Env(mut connection) => {
            connection.status = status;
            ConnectionInfo::Env(connection)
        }
    }
}

pub(crate) fn resolve_connections(
    entry: Option<&IntegrationEntry>,
    saved: &[credential::Info],
    environment: &ConfigEnv,
) -> Vec<ConnectionInfo> {
    let mut connections: Vec<ConnectionInfo> = saved
        .iter()
        .map(|saved| {
            ConnectionInfo::Credential(ConnectionCredentialInfo {
                r#type: ConnectionCredentialInfoType::Credential,
                id: saved.id.clone(),
                label: saved.label.clone(),
                method: match saved.value {
                    CredentialValue::Oauth(_) => ConnectionCredentialInfoMethod::Oauth,
                    CredentialValue::Key(_) => ConnectionCredentialInfoMethod::Key,
                    CredentialValue::External(_) => ConnectionCredentialInfoMethod::External,
                },
                status: None,
            })
        })
        .collect();
    connections.reverse();
    let Some(entry) = entry else {
        return connections;
    };
    for method in &entry.info.methods {
        let IntegrationMethod::Env(method) = method else {
            continue;
        };
        for name in &method.names {
            if environment(name).is_some_and(|value| !value.is_empty()) {
                connections.push(ConnectionInfo::Env(ConnectionEnvInfo {
                    r#type: ConnectionEnvInfoType::Env,
                    name: name.clone(),
                    status: None,
                }));
            }
        }
    }
    connections
}

pub struct ConnectionService<'a> {
    pub(crate) service: &'a IntegrationService,
}

pub struct StatusInput {
    pub integration_id: IntegrationID,
    pub connection: ConnectionInfo,
    pub status: Option<ConnectionStatus>,
}

impl ConnectionService<'_> {
    pub async fn active(&self, id: &IntegrationID) -> Result<Option<ConnectionInfo>, IntegrationError> {
        let registry = self.service.state.get().await?;
        let entry = registry.entries.get(&id.0);
        let saved = self.service.credentials.list(id)?;
        Ok(resolve_connections(entry, &saved, &self.service.environment).into_iter().next())
    }

    pub async fn resolve(&self, connection: &ConnectionInfo) -> Result<Option<CredentialValue>, AuthorizationError> {
        match connection {
            ConnectionInfo::Env(connection) => {
                Ok((self.service.environment)(&connection.name).filter(|value| !value.is_empty()).map(|value| {
                    CredentialValue::Key(CredentialKey {
                        r#type: CredentialKeyType::Key,
                        key: value.to_string_lossy().into_owned(),
                        metadata: None,
                    })
                }))
            }
            ConnectionInfo::Credential(connection) => self.resolve_credential(connection).await,
        }
    }

    pub async fn activate(&self, id: &CredentialID) -> Result<(), IntegrationError> {
        self.service.credentials.activate(id)?;
        Ok(())
    }

    pub async fn update(&self, id: &CredentialID, label: Option<String>) -> Result<(), IntegrationError> {
        self.service.credentials.update(id, Update { label, value: None })?;
        Ok(())
    }

    pub async fn remove(&self, id: &CredentialID) -> Result<(), IntegrationError> {
        let credential = self.service.credentials.get(id)?;
        if let Some(credential) = &credential
            && let CredentialValue::Oauth(value) = &credential.value
        {
            let registry = self.service.state.get().await?;
            let revoke = registry
                .entries
                .get(&credential.integration_id.0)
                .and_then(|entry| entry.implementations.get(&value.method_id.0))
                .and_then(|implementation| implementation.revoke());
            if let Some(revoke) = revoke
                && revoke(value.clone()).await.is_err()
            {
                log::warn!("remote sign-out was not confirmed; removing the local credential");
            }
        }
        self.service.credentials.remove(id)?;
        Ok(())
    }

    pub fn status(&self, input: StatusInput) -> Result<(), IntegrationError> {
        let key = status_key(&input.integration_id, &input.connection);
        {
            let mut statuses = self.service.statuses.lock().expect("lock integration statuses");
            if statuses.get(&key) == input.status.as_ref() {
                return Ok(());
            }
            match &input.status {
                Some(status) => statuses.insert(key, status.clone()),
                None => statuses.remove(&key),
            };
        }
        self.service.publish_updated()
    }

    async fn resolve_credential(
        &self,
        connection: &ConnectionCredentialInfo,
    ) -> Result<Option<CredentialValue>, AuthorizationError> {
        let Some(credential) = self.service.credentials.get(&connection.id).map_err(AuthorizationError::new)? else {
            return Ok(None);
        };
        let value = match credential.value {
            CredentialValue::Oauth(value) => value,
            value => return Ok(Some(value)),
        };
        let refresh = {
            let registry = self.service.state.get().await.map_err(AuthorizationError::new)?;
            registry
                .entries
                .get(&credential.integration_id.0)
                .and_then(|entry| entry.implementations.get(&value.method_id.0))
                .and_then(|implementation| implementation.refresh())
        };
        let Some(refresh) = refresh else {
            return Ok(Some(CredentialValue::Oauth(value)));
        };
        if !credential::expiring(&value, opencode_util::time::now_ms()) {
            return Ok(Some(CredentialValue::Oauth(value)));
        }
        let refreshed = self
            .service
            .credentials
            .refresh(&credential.id, credential::expiring, move |current| async move {
                refresh(current).await.map_err(|error| Arc::new(error) as credential::RefreshError)
            })
            .await
            .map_err(AuthorizationError::from_shared)?;
        Ok(refreshed.map(CredentialValue::Oauth))
    }
}
