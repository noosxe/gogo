//! The signed-in account, behind a trait so that tests inject one (design 25 §2).
//!
//! A session never leaves this module as anything but a [`TokenProvider`].

use std::sync::Arc;

use async_trait::async_trait;
use gogo::auth::{Session, TokenProvider, store::TokenStore};
use gogo::gog::error::AuthError;
use tokio::sync::Mutex;

#[async_trait]
pub(crate) trait Account: Send + Sync {
    /// The session, or `None` when nobody is signed in.
    async fn session(&self) -> Result<Option<Arc<dyn TokenProvider>>, AuthError>;

    /// The account's user name. Never the email address.
    async fn username(&self, session: &dyn TokenProvider) -> Result<Option<String>, AuthError> {
        gogo::auth::username(session).await
    }

    /// Keep a refreshed token (design 02 §4).
    async fn persist(&self) -> Result<(), AuthError> {
        Ok(())
    }
}

/// The account in the configured token store, shared with the CLI.
pub(crate) struct StoredAccount {
    store: Arc<dyn TokenStore>,
    /// `None` until the store has been read.
    session: Mutex<Option<Option<Arc<Session>>>>,
}

impl StoredAccount {
    pub(crate) fn new(store: Arc<dyn TokenStore>) -> Self {
        StoredAccount {
            store,
            session: Mutex::new(None),
        }
    }

    async fn restored(&self) -> Result<Option<Arc<Session>>, AuthError> {
        let mut cached = self.session.lock().await;
        if let Some(session) = &*cached {
            return Ok(session.clone());
        }
        let session = match Session::restore(Arc::clone(&self.store)).await {
            Ok(session) => Some(Arc::new(session)),
            Err(AuthError::Unauthorized) => None,
            // Not remembered: a keychain nobody answered is asked again next time.
            Err(e) => return Err(e),
        };
        *cached = Some(session.clone());
        Ok(session)
    }
}

#[async_trait]
impl Account for StoredAccount {
    async fn session(&self) -> Result<Option<Arc<dyn TokenProvider>>, AuthError> {
        Ok(self.restored().await?.map(|s| s as Arc<dyn TokenProvider>))
    }

    async fn persist(&self) -> Result<(), AuthError> {
        let Some(Some(session)) = self.session.lock().await.clone() else {
            return Ok(());
        };
        let store = Arc::clone(&self.store);
        // A keychain write can wait for the user: never on a runtime worker.
        tokio::task::spawn_blocking(move || session.persist_if_changed(store.as_ref()))
            .await
            .map_err(|_| AuthError::Store("the token store write did not finish".into()))?
    }
}
