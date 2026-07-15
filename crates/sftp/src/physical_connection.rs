use std::sync::{Arc, Mutex, MutexGuard};
use std::time::Duration;

use macsftp_core::{
    AppEvent, AuthCredential, AuthFailure, ConnectionSettings, ErrorCode, HostKeyMismatch,
    HostKeyPrompt, RemoteEventScope, RemoteScoped, TrustDecision, TrustRequestId, UserFacingError,
};
use russh::client;
use russh::keys::{PrivateKeyWithHashAlg, load_secret_key};
use ssh_key::Algorithm;
use tokio::sync::oneshot;
use tokio_util::sync::CancellationToken;
use tracing::{debug, info, warn};

use crate::known_hosts::{HostKeyCheckResult, KnownHostsStore, fingerprint_sha256, key_algorithm};
use crate::session_actor::HostTrustConfig;
use crate::trust::{TrustRegistry, TrustRegistryEntry};

pub fn lock_store(store: &Mutex<KnownHostsStore>) -> MutexGuard<'_, KnownHostsStore> {
    store
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
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
                    target: "macsftp_sftp::connection",
                    host = self.host.as_str(),
                    port = self.port,
                    tab_id = self.scope.tab_id.0,
                    session_id = self.scope.session_id.0,
                    session_epoch = self.scope.session_epoch,
                    "host key matched known_hosts"
                );
                Ok::<bool, russh::Error>(true)
            }
            HostKeyCheckResult::Mismatch => {
                warn!(
                    target: "macsftp_sftp::connection",
                    host = self.host.as_str(),
                    port = self.port,
                    tab_id = self.scope.tab_id.0,
                    session_id = self.scope.session_id.0,
                    session_epoch = self.scope.session_epoch,
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
                    target: "macsftp_sftp::connection",
                    host = self.host.as_str(),
                    port = self.port,
                    tab_id = self.scope.tab_id.0,
                    session_id = self.scope.session_id.0,
                    session_epoch = self.scope.session_epoch,
                    algorithm = key_algorithm(server_key).as_str(),
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
                        if persist_result.is_err() {
                            // Trust was granted for this session either way;
                            // the user will be prompted again next time.
                            warn!(
                                target: "macsftp_sftp::connection",
                                host = self.host.as_str(),
                                port = self.port,
                                tab_id = self.scope.tab_id.0,
                                session_id = self.scope.session_id.0,
                                session_epoch = self.scope.session_epoch,
                                "trusted host key could not be persisted; technical detail redacted"
                            );
                        }
                        Ok::<bool, russh::Error>(true)
                    }
                    Ok(Ok(_)) | Ok(Err(_)) => {
                        self.record_rejection(HostKeyRejection::UserRejected);
                        Ok::<bool, russh::Error>(false)
                    }
                    Err(_elapsed) => {
                        warn!(
                            target: "macsftp_sftp::connection",
                            host = self.host.as_str(),
                            port = self.port,
                            tab_id = self.scope.tab_id.0,
                            session_id = self.scope.session_id.0,
                            session_epoch = self.scope.session_epoch,
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

pub fn connection_error(host: &str, port: u16, _error: &dyn std::fmt::Display) -> UserFacingError {
    let mut user_error = UserFacingError::new(
        ErrorCode::ChannelClosed,
        "Connection failed",
        format!("Could not connect to {host}:{port}."),
    )
    .with_retryable(true);
    // Third-party technical text is deliberately not copied into UI state
    // because it is not a trustworthy redaction boundary.
    user_error.detail =
        Some("Check the server address, network connection, and SSH configuration.".to_string());
    user_error
}

pub fn sftp_connection_error(
    title: &'static str,
    message: &'static str,
    _error: &dyn std::fmt::Display,
) -> UserFacingError {
    UserFacingError::new(ErrorCode::ChannelClosed, title, message).with_retryable(true)
}

fn validate_private_key_algorithm(algorithm: Algorithm) -> Result<(), UserFacingError> {
    if matches!(algorithm, Algorithm::Rsa { .. }) {
        return Err(UserFacingError::new(
            ErrorCode::AuthFailed,
            "RSA private keys are disabled",
            "Use an Ed25519 or ECDSA private key. RSA signing is disabled until its dependency provides a constant-time implementation.",
        ));
    }
    Ok(())
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
        target: "macsftp_sftp::connection",
        host = settings.host.as_str(),
        port = settings.port,
        username = settings.username.as_str(),
        tab_id = scope.tab_id.0,
        session_id = scope.session_id.0,
        session_epoch = scope.session_epoch,
        method,
        "authentication started"
    );

    let auth_result = match &settings.auth {
        AuthCredential::Password { password } => handle
            .authenticate_password(settings.username.clone(), password.clone())
            .await
            .map_err(|error| {
                ConnectFailure::Connection(connection_error(&settings.host, settings.port, &error))
            })?,
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
                Err(_) => {
                    let key_file = private_key_file_name(key_path);
                    warn!(
                        key_file,
                        "private key could not be read or decrypted; technical detail redacted"
                    );
                    let reason = UserFacingError::new(
                        ErrorCode::AuthFailed,
                        "Could not load private key",
                        "The selected private key could not be read or decrypted.",
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
            if let Err(reason) = validate_private_key_algorithm(key.algorithm()) {
                let key_file = private_key_file_name(key_path);
                warn!(
                    key_file,
                    "RSA private key authentication blocked by security policy"
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
                .authenticate_publickey(
                    settings.username.clone(),
                    PrivateKeyWithHashAlg::new(Arc::new(key), best_hash),
                )
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
            info!(
                target: "macsftp_sftp::connection",
                host = settings.host.as_str(),
                port = settings.port,
                tab_id = scope.tab_id.0,
                session_id = scope.session_id.0,
                session_epoch = scope.session_epoch,
                method,
                "authentication succeeded"
            );
            Ok(())
        }
        russh::client::AuthResult::Failure { .. } => {
            let reason = UserFacingError::new(
                ErrorCode::AuthFailed,
                "Authentication failed",
                "The server rejected the credentials.",
            );
            warn!(
                target: "macsftp_sftp::connection",
                host = settings.host.as_str(),
                port = settings.port,
                tab_id = scope.tab_id.0,
                session_id = scope.session_id.0,
                session_epoch = scope.session_epoch,
                method,
                "authentication rejected by server"
            );
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

/// Trust registry, known_hosts, and event_tx are co-required for host-key flow.
#[allow(clippy::too_many_arguments)]
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

    info!(
        target: "macsftp_sftp::connection",
        host = settings.host.as_str(),
        port = settings.port,
        tab_id = scope.tab_id.0,
        session_id = scope.session_id.0,
        session_epoch = scope.session_epoch,
        "SSH transport and handshake started"
    );

    let address = (settings.host.as_str(), settings.port);
    let mut handle = match client::connect(config, address, handler).await {
        Ok(handle) => handle,
        Err(error) => {
            warn!(
                target: "macsftp_sftp::connection",
                host = settings.host.as_str(),
                port = settings.port,
                tab_id = scope.tab_id.0,
                session_id = scope.session_id.0,
                session_epoch = scope.session_epoch,
                "ssh tcp/handshake failed; technical detail redacted"
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

pub fn log_connect_failure(
    host: &str,
    port: u16,
    scope: &RemoteEventScope,
    failure: &ConnectFailure,
) {
    match failure {
        ConnectFailure::HostKeyMismatch => {
            warn!(
                target: "macsftp_sftp::connection",
                host,
                port,
                tab_id = scope.tab_id.0,
                session_id = scope.session_id.0,
                session_epoch = scope.session_epoch,
                failure = "host_key_mismatch",
                "connection failed"
            );
        }
        ConnectFailure::TrustRejected => {
            info!(
                target: "macsftp_sftp::connection",
                host,
                port,
                tab_id = scope.tab_id.0,
                session_id = scope.session_id.0,
                session_epoch = scope.session_epoch,
                failure = "host_key_rejected",
                "connection aborted"
            );
        }
        ConnectFailure::TrustTimeout => {
            warn!(
                target: "macsftp_sftp::connection",
                host,
                port,
                tab_id = scope.tab_id.0,
                session_id = scope.session_id.0,
                session_epoch = scope.session_epoch,
                failure = "host_key_prompt_timeout",
                "connection failed"
            );
        }
        ConnectFailure::AuthFailed(_) => {
            warn!(
                target: "macsftp_sftp::connection",
                host,
                port,
                tab_id = scope.tab_id.0,
                session_id = scope.session_id.0,
                session_epoch = scope.session_epoch,
                failure = "authentication_failed",
                "connection failed"
            );
        }
        ConnectFailure::Connection(_) => {
            warn!(
                target: "macsftp_sftp::connection",
                host,
                port,
                tab_id = scope.tab_id.0,
                session_id = scope.session_id.0,
                session_epoch = scope.session_epoch,
                failure = "network_or_protocol",
                "connection failed"
            );
        }
    }
}

fn private_key_file_name(path: &str) -> &str {
    std::path::Path::new(path)
        .file_name()
        .and_then(|name| name.to_str())
        .unwrap_or("[unknown]")
}

#[cfg(test)]
mod tests {
    use ssh_key::Algorithm;

    use super::{
        connection_error, private_key_file_name, sftp_connection_error,
        validate_private_key_algorithm,
    };

    #[test]
    fn connection_error_does_not_copy_untrusted_technical_detail() {
        let sensitive = "password=do-not-log /Users/alex/.ssh/private-key";

        let error = connection_error("example.com", 22, &sensitive);
        let rendered = format!(
            "{} {} {}",
            error.title,
            error.message,
            error.detail.as_deref().unwrap_or_default()
        );

        assert!(!rendered.contains("do-not-log"));
        assert!(!rendered.contains("/Users/alex/.ssh"));
    }

    #[test]
    fn sftp_connection_error_does_not_copy_untrusted_technical_detail() {
        let sensitive = "password=do-not-log /Users/alex/.ssh/private-key";

        let error = sftp_connection_error(
            "Could not start SFTP.",
            "The SFTP subsystem did not become ready.",
            &sensitive,
        );
        let rendered = format!(
            "{} {} {}",
            error.title,
            error.message,
            error.detail.as_deref().unwrap_or_default()
        );

        assert!(!rendered.contains("do-not-log"));
        assert!(!rendered.contains("/Users/alex/.ssh"));
    }

    #[test]
    fn private_key_log_label_omits_parent_directories() {
        assert_eq!(
            private_key_file_name("/Users/alex/.ssh/id_ed25519"),
            "id_ed25519"
        );
    }

    #[test]
    fn rsa_private_key_authentication_is_blocked() {
        let Err(error) = validate_private_key_algorithm(Algorithm::Rsa { hash: None }) else {
            panic!("RSA private keys must remain blocked while RUSTSEC-2023-0071 applies");
        };

        assert_eq!(error.code, macsftp_core::ErrorCode::AuthFailed);
        assert!(validate_private_key_algorithm(Algorithm::Ed25519).is_ok());
    }
}
