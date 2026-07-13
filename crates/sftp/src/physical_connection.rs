use std::sync::{Arc, Mutex, MutexGuard};
use std::time::Duration;

use macsftp_core::{
    AppEvent, AuthCredential, AuthFailure, ConnectionSettings,
    HostKeyMismatch, HostKeyPrompt, RemoteEventScope, RemoteScoped, TrustDecision,
    TrustRequestId, UserFacingError, ErrorCode
};
use russh::client;
use russh::keys::{PrivateKeyWithHashAlg, load_secret_key};
use tokio::sync::oneshot;
use tokio_util::sync::CancellationToken;
use tracing::{debug, info, warn};

use crate::known_hosts::{HostKeyCheckResult, KnownHostsStore, fingerprint_sha256, key_algorithm};
use crate::trust::{TrustRegistry, TrustRegistryEntry};
use crate::session_actor::HostTrustConfig;

pub fn lock_store(store: &Mutex<KnownHostsStore>) -> MutexGuard<'_, KnownHostsStore> {
    store.lock().unwrap_or_else(|poisoned| poisoned.into_inner())
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum HostKeyRejection {
    Mismatch,
    UserRejected,
    PromptTimeout,
}

#[derive(Debug, Clone)]
pub enum ConnectFailure {
    HostKeyMismatch,
    TrustRejected,
    TrustTimeout,
    AuthFailed(AuthFailure),
    Connection(UserFacingError),
}

pub struct ClientHandler {
    pub host: String,
    pub port: u16,
    pub scope: RemoteEventScope,
    pub trust_request_id: TrustRequestId,
    pub known_hosts: Arc<Mutex<KnownHostsStore>>,
    pub trust_config: Arc<HostTrustConfig>,
    pub trust_registry: Arc<TrustRegistry>,
    pub event_tx: flume::Sender<AppEvent>,
    pub rejection: Arc<Mutex<Option<HostKeyRejection>>>,
    pub connection_lost: CancellationToken,
}

impl ClientHandler {
    pub fn record_rejection(&self, rejection: HostKeyRejection) {
        *self
            .rejection
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner()) = Some(rejection);
    }
}

impl client::Handler for ClientHandler {
    type Error = russh::Error;

    async fn check_server_key(
        &mut self,
        server_key: &russh::keys::PublicKey,
    ) -> Result<bool, Self::Error> {
        let check_result = lock_store(&self.known_hosts).check(&self.host, self.port, server_key);

        match check_result {
            HostKeyCheckResult::Match => {
                debug!(
                    host = self.host.as_str(),
                    port = self.port,
                    "host key matched known_hosts"
                );
                Ok::<bool, russh::Error>(true)
            }
            HostKeyCheckResult::Mismatch => {
                warn!(
                    host = self.host.as_str(),
                    port = self.port,
                    "host key MISMATCH — connection blocked (potential MITM)"
                );
                let expected =
                    lock_store(&self.known_hosts).expected_fingerprint(&self.host, self.port);
                let _ = self
                    .event_tx
                    .send_async(AppEvent::HostKeyMismatch(HostKeyMismatch {
                        tab_id: self.scope.tab_id,
                        host: self.host.clone(),
                        port: self.port,
                        expected_fingerprint_sha256: expected,
                        actual_fingerprint_sha256: fingerprint_sha256(server_key),
                    }))
                    .await;
                self.record_rejection(HostKeyRejection::Mismatch);
                Ok::<bool, russh::Error>(false)
            }
            HostKeyCheckResult::NotFound => {
                info!(
                    host = self.host.as_str(),
                    port = self.port,
                    algorithm = key_algorithm(server_key).as_str(),
                    fingerprint = fingerprint_sha256(server_key).as_str(),
                    "unknown host key; prompting user to trust"
                );
                let (responder, decision_rx) = oneshot::channel();
                self.trust_registry.register(
                    self.trust_request_id,
                    TrustRegistryEntry {
                        tab_id: self.scope.tab_id,
                        session_epoch: self.scope.session_epoch,
                        responder,
                    },
                );
                let _ = self
                    .event_tx
                    .send_async(AppEvent::HostKeyUnknown(HostKeyPrompt {
                        request_id: self.trust_request_id,
                        tab_id: self.scope.tab_id,
                        session_id: self.scope.session_id,
                        session_epoch: self.scope.session_epoch,
                        host: self.host.clone(),
                        port: self.port,
                        algorithm: key_algorithm(server_key),
                        fingerprint_sha256: fingerprint_sha256(server_key),
                    }))
                    .await;

                let decision =
                    tokio::time::timeout(self.trust_config.trust_prompt_timeout, decision_rx).await;
                match decision {
                    Ok(Ok(TrustDecision::TrustAndSave)) => {
                        let persist_result = lock_store(&self.known_hosts).add_trusted(
                            &self.host,
                            self.port,
                            server_key,
                            &self.trust_config.app_known_hosts_path,
                        );
                        if let Err(error) = persist_result {
                            // Trust was granted for this session either way;
                            // the user will be prompted again next time.
                            warn!(error = %error, "known_hosts: failed to persist trusted key");
                        }
                        Ok::<bool, russh::Error>(true)
                    }
                    Ok(Ok(_)) | Ok(Err(_)) => {
                        self.record_rejection(HostKeyRejection::UserRejected);
                        Ok::<bool, russh::Error>(false)
                    }
                    Err(_elapsed) => {
                        warn!(
                            host = self.host.as_str(),
                            port = self.port,
                            "host key trust prompt timed out"
                        );
                        // Clean up the registry entry so it can't be
                        // resolved later.
                        self.trust_registry
                            .resolve(self.trust_request_id, TrustDecision::TimedOut);
                        self.record_rejection(HostKeyRejection::PromptTimeout);
                        Ok::<bool, russh::Error>(false)
                    }
                }
            }
        }
    }

    async fn disconnected(
        &mut self,
        reason: client::DisconnectReason<Self::Error>,
    ) -> Result<(), Self::Error> {
        self.connection_lost.cancel();
        match reason {
            client::DisconnectReason::ReceivedDisconnect(_) => Ok(()),
            client::DisconnectReason::Error(error) => Err(error),
        }
    }
}

pub fn connection_error(host: &str, port: u16, error: &dyn std::fmt::Display) -> UserFacingError {
    let mut user_error = UserFacingError::new(
        ErrorCode::ChannelClosed,
        "Connection failed",
        format!("Could not connect to {host}:{port}."),
    )
    .with_retryable(true);
    user_error.detail = Some(error.to_string());
    user_error
}

async fn authenticate(
    handle: &mut client::Handle<ClientHandler>,
    settings: &ConnectionSettings,
    scope: &RemoteEventScope,
    event_tx: &flume::Sender<AppEvent>,
) -> Result<(), ConnectFailure> {
    let method = match &settings.auth {
        AuthCredential::Password { .. } => "password",
        AuthCredential::PrivateKey { .. } => "private_key",
    };
    info!(
        host = settings.host.as_str(),
        username = settings.username.as_str(),
        method,
        "authenticating"
    );

    let auth_result = match &settings.auth {
        AuthCredential::Password { password } => {
            handle
                .authenticate_password(settings.username.clone(), password.clone())
                .await
                .map_err(|error| {
                    ConnectFailure::Connection(connection_error(
                        &settings.host,
                        settings.port,
                        &error,
                    ))
                })?
        }
        AuthCredential::PrivateKey {
            key_path,
            passphrase,
        } => {
            // Surface key-load failures as an `AuthFailed` event, mirroring
            // the `AuthResult::Failure` branch below so the UI always sees a
            // single, consistent auth-failure signal regardless of *where*
            // the authentication step failed.
            let key = match load_secret_key(key_path, passphrase.as_deref()) {
                Ok(key) => key,
                Err(error) => {
                    let reason = UserFacingError::new(
                        ErrorCode::AuthFailed,
                        "Could not load private key",
                        &error.to_string(),
                    );
                    let _ = event_tx
                        .send_async(AppEvent::AuthFailed(RemoteScoped::new(
                            scope.clone(),
                            AuthFailure {
                                reason: reason.clone(),
                            },
                        )))
                        .await;
                    return Err(ConnectFailure::AuthFailed(AuthFailure { reason }));
                }
            };
            let best_hash = handle
                .best_supported_rsa_hash()
                .await
                .map_err(|error| {
                    ConnectFailure::Connection(connection_error(
                        &settings.host,
                        settings.port,
                        &error,
                    ))
                })?
                .flatten();
            handle
                .authenticate_publickey(settings.username.clone(), PrivateKeyWithHashAlg::new(Arc::new(key), best_hash))
                .await
                .map_err(|error| {
                    ConnectFailure::Connection(connection_error(
                        &settings.host,
                        settings.port,
                        &error,
                    ))
                })?
        }
    };

    match auth_result {
        russh::client::AuthResult::Success => {
            debug!("authentication successful");
            Ok(())
        }
        russh::client::AuthResult::Failure { .. } => {
            let reason = UserFacingError::new(ErrorCode::AuthFailed, "Authentication failed", "The server rejected the credentials.");
            warn!(method, "authentication rejected by server");
            let _ = event_tx
                .send_async(AppEvent::AuthFailed(RemoteScoped::new(
                    scope.clone(),
                    AuthFailure {
                        reason: reason.clone(),
                    },
                )))
                .await;
            Err(ConnectFailure::AuthFailed(AuthFailure { reason }))
        }
    }
}


pub async fn establish_physical_connection(
    settings: &ConnectionSettings,
    scope: &RemoteEventScope,
    trust_request_id: TrustRequestId,
    known_hosts: Arc<Mutex<KnownHostsStore>>,
    trust_config: Arc<HostTrustConfig>,
    trust_registry: Arc<TrustRegistry>,
    event_tx: flume::Sender<AppEvent>,
    connection_lost: CancellationToken,
) -> Result<client::Handle<ClientHandler>, ConnectFailure> {
    let config = Arc::new(client::Config {
        keepalive_interval: Some(Duration::from_secs(15)),
        keepalive_max: 3,
        ..client::Config::default()
    });

    let rejection: Arc<Mutex<Option<HostKeyRejection>>> = Arc::new(Mutex::new(None));
    let handler = ClientHandler {
        host: settings.host.clone(),
        port: settings.port,
        scope: scope.clone(),
        trust_request_id,
        known_hosts,
        trust_config,
        trust_registry,
        event_tx: event_tx.clone(),
        rejection: rejection.clone(),
        connection_lost,
    };

    let address = (settings.host.as_str(), settings.port);
    let mut handle = match client::connect(config, address, handler).await {
        Ok(handle) => handle,
        Err(error) => {
            warn!(
                host = settings.host.as_str(),
                port = settings.port,
                error = %error,
                "ssh tcp/handshake failed"
            );
            let recorded = rejection
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner())
                .take();
            return Err(match recorded {
                Some(HostKeyRejection::Mismatch) => ConnectFailure::HostKeyMismatch,
                Some(HostKeyRejection::UserRejected) => ConnectFailure::TrustRejected,
                Some(HostKeyRejection::PromptTimeout) => ConnectFailure::TrustTimeout,
                None => ConnectFailure::Connection(connection_error(
                    &settings.host,
                    settings.port,
                    &error,
                )),
            });
        }
    };

    authenticate(&mut handle, settings, scope, &event_tx).await?;

    Ok(handle)
}

pub fn log_connect_failure(host: &str, failure: &ConnectFailure) {
    match failure {
        ConnectFailure::HostKeyMismatch => {
            warn!(host, "connection failed: host key mismatch");
        }
        ConnectFailure::TrustRejected => {
            info!(host, "connection aborted: user rejected host key");
        }
        ConnectFailure::TrustTimeout => {
            warn!(host, "connection failed: trust prompt timed out");
        }
        ConnectFailure::AuthFailed(auth_failure) => {
            warn!(host, title = %auth_failure.reason.title, "connection failed: authentication failed");
        }
        ConnectFailure::Connection(error) => {
            warn!(host, title = %error.title, detail = ?error.detail, "connection failed: protocol or network error");
        }
    }
}
