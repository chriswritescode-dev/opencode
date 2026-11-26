use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::Duration;

use opencode_bus::{Bus, BusOptions};
use opencode_catalog::credential::{CreateInput, CredentialService, RefreshError, Update};
use opencode_contract::generated::{
    CredentialID, CredentialOAuth, CredentialOAuthType, CredentialValue, IntegrationID, IntegrationMethodID,
};
use opencode_storage::{Database, DatabaseLocation};
use tokio::sync::Semaphore;

use crate::TempDir;

fn setup() -> (TempDir, Arc<Database>, Arc<Bus>, Arc<CredentialService>, PathBuf) {
    let dir = TempDir::new();
    let (database, _) = Database::open(DatabaseLocation::Memory, dir.path()).expect("open the database");
    let database = Arc::new(database);
    let bus = Bus::new(database.clone(), BusOptions::default());
    let locks = dir.path().join("locks");
    let service = CredentialService::new(database.clone(), bus.clone(), locks.clone());
    (dir, database, bus, service, locks)
}

fn oauth(access: &str, refresh: &str, expires: i64) -> CredentialOAuth {
    CredentialOAuth {
        r#type: CredentialOAuthType::Oauth,
        method_id: IntegrationMethodID("oauth".to_owned()),
        refresh: refresh.to_owned(),
        access: access.to_owned(),
        expires: expires as f64,
        metadata: None,
    }
}

fn create(service: &CredentialService) -> CredentialID {
    service
        .create(CreateInput {
            id: None,
            integration_id: IntegrationID("test".to_owned()),
            value: CredentialValue::Oauth(oauth("old-access", "old-refresh", 1)),
            label: None,
            activate: None,
        })
        .expect("create the oauth credential")
        .id
}

fn stored(service: &CredentialService, id: &CredentialID) -> CredentialOAuth {
    let info = service.get(id).expect("read the credential").expect("the credential exists");
    let CredentialValue::Oauth(value) = info.value else {
        panic!("expected an oauth credential");
    };
    value
}

#[derive(Debug)]
struct Unavailable;

impl std::fmt::Display for Unavailable {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str("token endpoint unavailable")
    }
}

impl std::error::Error for Unavailable {}

#[tokio::test]
async fn saves_the_refreshed_value() {
    let (_dir, _database, _bus, service, _locks) = setup();
    let id = create(&service);
    let value = oauth("new-access", "new-refresh", 9_007_199_254_740_991);
    let expected = value.clone();

    let result =
        service.refresh(&id, |_, _| true, move |_| async move { Ok(value) }).await.expect("refresh the credential");

    assert_eq!(result, Some(expected.clone()));
    assert_eq!(stored(&service, &id), expected);
}

#[tokio::test]
async fn returns_the_stored_value_when_it_is_fresh() {
    let (_dir, _database, _bus, service, _locks) = setup();
    let id = create(&service);
    let calls = Arc::new(AtomicUsize::new(0));
    let counter = calls.clone();

    let result = service
        .refresh(
            &id,
            |_, _| false,
            move |_| {
                let counter = counter.clone();
                async move {
                    counter.fetch_add(1, Ordering::SeqCst);
                    Ok(oauth("new-access", "new-refresh", 2))
                }
            },
        )
        .await
        .expect("refresh the credential");

    assert_eq!(result.expect("the credential exists").access, "old-access");
    assert_eq!(calls.load(Ordering::SeqCst), 0);
}

#[tokio::test]
async fn keeps_a_value_that_changed_while_the_refresh_ran() {
    let (_dir, _database, _bus, service, _locks) = setup();
    let id = create(&service);
    let replaced = oauth("other-access", "other-refresh", 3);
    let updated = replaced.clone();
    let updater = service.clone();
    let target = id.clone();

    let result = service
        .refresh(
            &id,
            |_, _| true,
            move |_| {
                let updater = updater.clone();
                let target = target.clone();
                let updated = updated.clone();
                async move {
                    updater
                        .update(&target, Update { label: None, value: Some(CredentialValue::Oauth(updated)) })
                        .expect("replace the credential while refreshing");
                    Ok(oauth("new-access", "new-refresh", 2))
                }
            },
        )
        .await
        .expect("refresh the credential");

    assert_eq!(result, Some(replaced.clone()));
    assert_eq!(stored(&service, &id), replaced);
}

#[tokio::test]
async fn concurrent_callers_share_one_refresh() {
    let (_dir, _database, _bus, service, _locks) = setup();
    let id = create(&service);
    let entered = Arc::new(Semaphore::new(0));
    let release = Arc::new(Semaphore::new(0));
    let calls = Arc::new(AtomicUsize::new(0));

    let first_service = service.clone();
    let first_id = id.clone();
    let first_calls = calls.clone();
    let first_entered = entered.clone();
    let first_release = release.clone();
    let first = tokio::spawn(async move {
        first_service
            .refresh(
                &first_id,
                |_, _| true,
                move |_| {
                    let calls = first_calls.clone();
                    let entered = first_entered.clone();
                    let release = first_release.clone();
                    async move {
                        calls.fetch_add(1, Ordering::SeqCst);
                        entered.add_permits(1);
                        let _ = release.acquire().await;
                        Ok(oauth("new-access", "new-refresh", 2))
                    }
                },
            )
            .await
    });

    entered.acquire().await.expect("wait for the refresh to enter").forget();

    let second_service = service.clone();
    let second_id = id.clone();
    let second_calls = calls.clone();
    let second = tokio::spawn(async move {
        second_service
            .refresh(
                &second_id,
                |_, _| true,
                move |_| {
                    let calls = second_calls.clone();
                    async move {
                        calls.fetch_add(1, Ordering::SeqCst);
                        Ok(oauth("second-access", "second-refresh", 3))
                    }
                },
            )
            .await
    });

    tokio::time::sleep(Duration::from_millis(50)).await;
    release.add_permits(1);

    let first_value = first.await.expect("join the first caller").expect("refresh the credential");
    let second_value = second.await.expect("join the second caller").expect("refresh the credential");

    assert_eq!(first_value, second_value);
    assert_eq!(first_value.expect("the credential exists").access, "new-access");
    assert_eq!(calls.load(Ordering::SeqCst), 1);
}

#[tokio::test]
async fn resolves_none_for_a_missing_credential() {
    let (_dir, _database, _bus, service, _locks) = setup();
    let calls = Arc::new(AtomicUsize::new(0));
    let counter = calls.clone();

    let result = service
        .refresh(
            &CredentialID("cred_missing".to_owned()),
            |_, _| true,
            move |_| {
                let counter = counter.clone();
                async move {
                    counter.fetch_add(1, Ordering::SeqCst);
                    Ok(oauth("new-access", "new-refresh", 2))
                }
            },
        )
        .await
        .expect("refresh the missing credential");

    assert_eq!(result, None);
    assert_eq!(calls.load(Ordering::SeqCst), 0);
}

#[tokio::test]
async fn shares_failures_and_allows_a_later_refresh() {
    let (_dir, _database, _bus, service, _locks) = setup();
    let id = create(&service);
    let entered = Arc::new(Semaphore::new(0));
    let release = Arc::new(Semaphore::new(0));
    let calls = Arc::new(AtomicUsize::new(0));

    let first_service = service.clone();
    let first_id = id.clone();
    let first_calls = calls.clone();
    let first_entered = entered.clone();
    let first_release = release.clone();
    let first = tokio::spawn(async move {
        first_service
            .refresh(
                &first_id,
                |_, _| true,
                move |_| {
                    let calls = first_calls.clone();
                    let entered = first_entered.clone();
                    let release = first_release.clone();
                    async move {
                        calls.fetch_add(1, Ordering::SeqCst);
                        entered.add_permits(1);
                        let _ = release.acquire().await;
                        Err(Arc::new(Unavailable) as RefreshError)
                    }
                },
            )
            .await
    });

    entered.acquire().await.expect("wait for the refresh to enter").forget();

    let second_service = service.clone();
    let second_id = id.clone();
    let second = tokio::spawn(async move {
        second_service
            .refresh(&second_id, |_, _| true, |_| async { Ok(oauth("second-access", "second-refresh", 2)) })
            .await
    });

    tokio::time::sleep(Duration::from_millis(50)).await;
    release.add_permits(1);

    let first_error = first.await.expect("join the first caller").expect_err("the refresh fails");
    let second_error = second.await.expect("join the second caller").expect_err("the refresh fails");

    assert!(Arc::ptr_eq(&first_error, &second_error));
    assert_eq!(first_error.to_string(), "token endpoint unavailable");
    assert_eq!(calls.load(Ordering::SeqCst), 1);

    let value = oauth("new-access", "new-refresh", 3);
    let expected = value.clone();
    let result = service.refresh(&id, |_, _| true, move |_| async move { Ok(value) }).await.expect("refresh again");
    assert_eq!(result, Some(expected));
}

#[tokio::test]
async fn a_panicking_refresh_fails_waiters_and_allows_a_later_refresh() {
    let (_dir, _database, _bus, service, _locks) = setup();
    let id = create(&service);

    let failed = tokio::time::timeout(
        Duration::from_secs(1),
        service.refresh(&id, |_, _| true, |_| async { panic!("refresh panicked") }),
    )
    .await
    .expect("a panicking refresh reports an aborted refresh instead of hanging")
    .expect_err("the refresh fails");

    assert_eq!(failed.to_string(), "credential refresh task ended without a result");

    let value = oauth("new-access", "new-refresh", 9_007_199_254_740_991);
    let expected = value.clone();
    let result = service.refresh(&id, |_, _| true, move |_| async move { Ok(value) }).await.expect("refresh again");

    assert_eq!(result, Some(expected.clone()));
    assert_eq!(stored(&service, &id), expected);
}

#[tokio::test]
async fn waits_for_another_process_holding_the_refresh_lock() {
    let (_dir, _database, _bus, service, locks) = setup();
    let id = create(&service);
    let key = format!("credential-refresh:{}", id.0);

    let holder = {
        let locks = locks.clone();
        let key = key.clone();
        tokio::task::spawn_blocking(move || {
            opencode_local::lock::acquire(&locks, &key, &opencode_local::lock::Options::default())
        })
        .await
        .expect("join the lock holder")
        .expect("acquire the refresh lock")
    };

    let refresh_service = service.clone();
    let refresh_id = id.clone();
    let mut refresh = tokio::spawn(async move {
        refresh_service.refresh(&refresh_id, |_, _| true, |_| async { Ok(oauth("new-access", "new-refresh", 2)) }).await
    });

    assert!(
        tokio::time::timeout(Duration::from_millis(100), &mut refresh).await.is_err(),
        "the refresh waits for the lock held by another owner"
    );

    tokio::task::spawn_blocking(move || holder.release())
        .await
        .expect("join the lock release")
        .expect("release the refresh lock");

    let value = refresh.await.expect("join the refresh").expect("refresh the credential");
    assert_eq!(value.expect("the credential exists").access, "new-access");
}
