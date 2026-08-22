use std::sync::{Arc, Mutex, MutexGuard};
use std::time::Duration;

use macsftp_core::{
    AppEvent, AuthCredential, AuthFailure, ConnectionSettings, ErrorCode, HostKeyMismatch,
    HostKeyPrompt, RemoteEventScope, RemoteScoped, TrustDecision, TrustRequestId, UserFacingError,
};
use russh::client;
use russh::keys::{PrivateKeyWithHashAlg, load_secret_key};
use ssh_key::{Algorithm, EcdsaCurve, HashAlg};
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
pub struct HostKeyMismatchDetails {
    pub host: String,
    pub port: u16,
    pub expected_fingerprint_sha256: Option<String>,
    pub actual_fingerprint_sha256: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum HostKeyRejection {
    Mismatch(HostKeyMismatchDetails),
    UserRejected,
    PromptTimeout,
}

#[derive(Debug, Clone)]
pub enum ConnectFailure {
    HostKeyMismatch(HostKeyMismatchDetails),
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
    /// Written before `connection_lost` is cancelled, so actors waking up on
    /// the token can always read the classified cause of the drop.
    pub disconnect_cause: Arc<Mutex<Option<PhysicalDisconnectCause>>>,
}

impl ClientHandler {
    pub fn record_rejection(&self, rejection: HostKeyRejection) {
        *self
            .rejection
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner()) = Some(rejection);
    }
}

fn legacy_rsa_host_key_bits(server_key: &russh::keys::PublicKey) -> Option<u32> {
    server_key
        .key_data()
        .rsa()
        .map(|public_key| public_key.key_size())
        .filter(|bits| *bits < 2048)
}

impl client::Handler for ClientHandler {
    type Error = russh::Error;

    async fn check_server_key(
        &mut self,
        server_key: &russh::keys::PublicKey,
    ) -> Result<bool, Self::Error> {
        if let Some(rsa_bits) = legacy_rsa_host_key_bits(server_key) {
            warn!(
                target: "macsftp_sftp::connection",
                host = self.host.as_str(),
                port = self.port,
                tab_id = self.scope.tab_id.0,
                session_id = self.scope.session_id.0,
                session_epoch = self.scope.session_epoch,
                rsa_bits,
                "legacy RSA host key accepted; server upgrade recommended"
            );
        }
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
                let actual = fingerprint_sha256(server_key);
                let details = HostKeyMismatchDetails {
                    host: self.host.clone(),
                    port: self.port,
                    expected_fingerprint_sha256: expected,
                    actual_fingerprint_sha256: actual,
                };
                self.record_rejection(HostKeyRejection::Mismatch(details));
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
        // Record the cause BEFORE cancelling the token: actors wake up on
        // cancellation and read this immediately to build their
        // TabDisconnected reason. Intentional local teardown (`None`) keeps
        // the legacy unclassified path — no live actor should observe it.
        let classified = classify_mid_session_disconnect(&reason);
        if let Some(cause) = classified
            .as_ref()
            .map(PhysicalDisconnectCause::from_mid_session_disconnect)
        {
            *self
                .disconnect_cause
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner()) = Some(cause);
        }
        // Mid-session drops were previously invisible to diagnostics: the
        // audited connection log (plan §17) only covered the connect
        // lifecycle, so "why did my connection drop" was unanswerable.
        match classify_mid_session_disconnect(&reason) {
            Some(MidSessionDisconnect::ServerClosed(reason_code)) => info!(
                target: "macsftp_sftp::connection",
                host = self.host.as_str(),
                port = self.port,
                tab_id = self.scope.tab_id.0,
                session_id = self.scope.session_id.0,
                session_epoch = self.scope.session_epoch,
                failure = "server_disconnected",
                server_reason = ?reason_code,
                "SSH connection closed by the server"
            ),
            Some(MidSessionDisconnect::TransportFailed(failure)) => warn!(
                target: "macsftp_sftp::connection",
                host = self.host.as_str(),
                port = self.port,
                tab_id = self.scope.tab_id.0,
                session_id = self.scope.session_id.0,
                session_epoch = self.scope.session_epoch,
                failure,
                "SSH connection lost"
            ),
            None => {}
        }
        self.connection_lost.cancel();
        match reason {
            client::DisconnectReason::ReceivedDisconnect(_) => Ok(()),
            client::DisconnectReason::Error(error) => Err(error),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum TransportFailureKind {
    LocalNetworkPermissionDenied,
    ConnectionRefused,
    TimedOut,
    HostKeyAlgorithmUnsupported,
    ProtocolNegotiationFailed,
    Other,
}

impl TransportFailureKind {
    fn from_russh_error(error: &russh::Error) -> Self {
        match error {
            russh::Error::IO(error) => match error.kind() {
                std::io::ErrorKind::PermissionDenied => Self::LocalNetworkPermissionDenied,
                std::io::ErrorKind::ConnectionRefused => Self::ConnectionRefused,
                std::io::ErrorKind::TimedOut => Self::TimedOut,
                _ => Self::Other,
            },
            russh::Error::ConnectionTimeout
            | russh::Error::KeepaliveTimeout
            | russh::Error::InactivityTimeout
            | russh::Error::Elapsed(_) => Self::TimedOut,
            russh::Error::NoCommonAlgo {
                kind: russh::AlgorithmKind::Key,
                ..
            } => Self::HostKeyAlgorithmUnsupported,
            russh::Error::KexInit
            | russh::Error::NoCommonAlgo { .. }
            | russh::Error::Version
            | russh::Error::Kex
            | russh::Error::PacketAuth
            | russh::Error::WrongServerSig
            | russh::Error::StrictKeyExchangeViolation { .. } => Self::ProtocolNegotiationFailed,
            _ => Self::Other,
        }
    }

    fn as_str(self) -> &'static str {
        match self {
            Self::LocalNetworkPermissionDenied => "local_network_permission_denied",
            Self::ConnectionRefused => "connection_refused",
            Self::TimedOut => "timed_out",
            Self::HostKeyAlgorithmUnsupported => "host_key_algorithm_unsupported",
            Self::ProtocolNegotiationFailed => "protocol_negotiation_failed",
            Self::Other => "network_or_protocol",
        }
    }
}

/// Classified reason for a mid-session SSH disconnect, for the audited
/// connection log. `None` means intentional local teardown: russh reports a
/// dropped handle / runtime shutdown through the same `Error::Disconnect`
/// sentinel it uses for the run loop's own exit path. Logging that sentinel
/// would turn every user-initiated disconnect into a WARN incident and bury
/// real drops, so it must stay unlogged.
#[derive(Debug)]
enum MidSessionDisconnect<'a> {
    /// The server sent an SSH DISCONNECT message. Only the enumerated
    /// reason code is logged — the free-text description from the wire is
    /// untrusted third-party content (plan §17 redaction boundary).
    ServerClosed(&'a russh::Disconnect),
    /// The transport failed locally; label from the shared transport
    /// failure taxonomy (`KeepaliveTimeout` maps to `timed_out`).
    TransportFailed(&'static str),
}

fn classify_mid_session_disconnect(
    reason: &client::DisconnectReason<russh::Error>,
) -> Option<MidSessionDisconnect<'_>> {
    match reason {
        client::DisconnectReason::ReceivedDisconnect(info) => {
            Some(MidSessionDisconnect::ServerClosed(&info.reason_code))
        }
        client::DisconnectReason::Error(russh::Error::Disconnect) => None,
        client::DisconnectReason::Error(error) => Some(MidSessionDisconnect::TransportFailed(
            TransportFailureKind::from_russh_error(error).as_str(),
        )),
    }
}

/// Coarse, user-visible bucket for a mid-session physical disconnect. This
/// is the state-machine counterpart of the P1 log classification: the UI
/// distinguishes "the server hung up" from "the network went silent", which
/// need different recovery guidance.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PhysicalDisconnectCause {
    /// The server sent an SSH DISCONNECT message.
    ServerClosed,
    /// No inbound traffic for ~60s — keepalive gave up (network path dead:
    /// sleep/wake, Wi-Fi roaming, VPN reconnect...).
    NetworkTimeout,
    /// Any other local transport failure.
    NetworkError,
}

impl PhysicalDisconnectCause {
    fn from_mid_session_disconnect(disconnect: &MidSessionDisconnect<'_>) -> Self {
        match disconnect {
            MidSessionDisconnect::ServerClosed(_) => Self::ServerClosed,
            // `timed_out` is exactly the keepalive/inactivity/IO-timeout
            // family; everything else stays a generic network error.
            MidSessionDisconnect::TransportFailed("timed_out") => Self::NetworkTimeout,
            MidSessionDisconnect::TransportFailed(_) => Self::NetworkError,
        }
    }

    /// Build the user-facing disconnect reason carried by
    /// `AppEvent::TabDisconnected`. Reuses the existing
    /// `DisconnectReason::Error(UserFacingError)` channel so core's state
    /// machine stays untouched; new `ErrorCode`s let the UI map each cause
    /// to distinct copy.
    pub fn disconnect_reason(self) -> macsftp_core::DisconnectReason {
        let (code, title, message) = match self {
            Self::ServerClosed => (
                ErrorCode::ServerDisconnected,
                "Server closed the connection",
                "The remote server ended the SSH session.",
            ),
            Self::NetworkTimeout => (
                ErrorCode::NetworkTimeout,
                "Connection lost",
                "No response from the server for about a minute. \
                 Check your network connection and reconnect.",
            ),
            Self::NetworkError => (
                ErrorCode::NetworkError,
                "Connection lost",
                "A network error interrupted the connection.",
            ),
        };
        macsftp_core::DisconnectReason::Error(
            UserFacingError::new(code, title, message).with_retryable(true),
        )
    }
}

pub fn connection_error(host: &str, port: u16, error: &russh::Error) -> UserFacingError {
    let failure = TransportFailureKind::from_russh_error(error);
    if failure == TransportFailureKind::LocalNetworkPermissionDenied {
        let mut user_error = UserFacingError::new(
            ErrorCode::LocalNetworkPermissionDenied,
            "Local network access required",
            format!("macSFTP was not allowed to connect to {host}:{port}."),
        )
        .with_retryable(true);
        user_error.detail = Some(
            "Enable macSFTP in System Settings → Privacy & Security → Local Network, then retry."
                .to_string(),
        );
        return user_error;
    }

    if failure == TransportFailureKind::HostKeyAlgorithmUnsupported {
        let mut user_error = UserFacingError::new(
            ErrorCode::ChannelClosed,
            "Unsupported SSH host key",
            format!("{host}:{port} does not offer a host key macSFTP can verify safely."),
        );
        user_error.detail = Some(
            "Ask the server administrator to enable an Ed25519 or ECDSA host key.".to_string(),
        );
        return user_error;
    }

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

fn client_config() -> client::Config {
    let mut config = client::Config {
        keepalive_interval: Some(Duration::from_secs(15)),
        keepalive_max: 3,
        ..client::Config::default()
    };
    // The local russh patch verifies RSA-SHA2 host signatures with AWS-LC.
    // Legacy SHA-1 `ssh-rsa` remains excluded, as does RSA private-key auth.
    config.preferred.key = vec![
        Algorithm::Ed25519,
        Algorithm::Ecdsa {
            curve: EcdsaCurve::NistP256,
        },
        Algorithm::Ecdsa {
            curve: EcdsaCurve::NistP384,
        },
        Algorithm::Ecdsa {
            curve: EcdsaCurve::NistP521,
        },
        Algorithm::Rsa {
            hash: Some(HashAlg::Sha512),
        },
        Algorithm::Rsa {
            hash: Some(HashAlg::Sha256),
        },
    ]
    .into();
    config
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
    disconnect_cause: Arc<Mutex<Option<PhysicalDisconnectCause>>>,
) -> Result<client::Handle<ClientHandler>, ConnectFailure> {
    let config = Arc::new(client_config());

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
        disconnect_cause,
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
            let failure = TransportFailureKind::from_russh_error(&error).as_str();
            warn!(
                target: "macsftp_sftp::connection",
                host = settings.host.as_str(),
                port = settings.port,
                tab_id = scope.tab_id.0,
                session_id = scope.session_id.0,
                session_epoch = scope.session_epoch,
                failure,
                "ssh tcp/handshake failed; technical detail redacted"
            );
            let recorded = rejection
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner())
                .take();
            return Err(match recorded {
                Some(HostKeyRejection::Mismatch(details)) => {
                    ConnectFailure::HostKeyMismatch(details)
                }
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

/// Translate physical handshake mismatch details into a logical,
/// session-scoped `AppEvent`. Each logical caller supplies its own
/// authoritative `RemoteEventScope`; the physical `HostKeyMismatchDetails`
/// are scope-free so they can be cloned for every waiter on a pooled
/// handshake without collapsing their identities to the first caller.
pub fn host_key_mismatch_event(
    scope: RemoteEventScope,
    details: HostKeyMismatchDetails,
) -> AppEvent {
    AppEvent::HostKeyMismatch(HostKeyMismatch {
        scope,
        host: details.host,
        port: details.port,
        expected_fingerprint_sha256: details.expected_fingerprint_sha256,
        actual_fingerprint_sha256: details.actual_fingerprint_sha256,
    })
}

pub fn log_connect_failure(
    host: &str,
    port: u16,
    scope: &RemoteEventScope,
    failure: &ConnectFailure,
) {
    match failure {
        ConnectFailure::HostKeyMismatch(_) => {
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
        ConnectFailure::Connection(error) => {
            let failure = if error.code == ErrorCode::LocalNetworkPermissionDenied {
                "local_network_permission_denied"
            } else {
                "network_or_protocol"
            };
            warn!(
                target: "macsftp_sftp::connection",
                host,
                port,
                tab_id = scope.tab_id.0,
                session_id = scope.session_id.0,
                session_epoch = scope.session_epoch,
                failure,
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
    use std::io;

    use ssh_key::{Algorithm, HashAlg};

    use super::{
        AppEvent, ClientHandler, HostKeyMismatchDetails, MidSessionDisconnect,
        PhysicalDisconnectCause, TransportFailureKind, classify_mid_session_disconnect,
        client_config, connection_error, host_key_mismatch_event, legacy_rsa_host_key_bits,
        private_key_file_name, sftp_connection_error, validate_private_key_algorithm,
    };
    use crate::session_actor::HostTrustConfig;
    use crate::trust::TrustRegistry;
    use macsftp_core::{ErrorCode, RemoteEventScope, SessionId, TabId, TrustRequestId};
    use russh::client;
    #[allow(unused_imports)]
    use russh::client::Handler as _;
    use std::path::PathBuf;
    use std::sync::{Arc, Mutex};
    use tokio_util::sync::CancellationToken;

    #[test]
    fn connection_error_does_not_copy_untrusted_technical_detail() {
        let sensitive = russh::Error::InvalidConfig(
            "password=do-not-log /Users/alex/.ssh/private-key".to_string(),
        );

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
    fn connection_permission_denied_points_to_local_network_settings() {
        let transport_error = russh::Error::IO(io::Error::from(io::ErrorKind::PermissionDenied));

        let error = connection_error("10.0.0.10", 22, &transport_error);

        assert_eq!(
            error.code,
            macsftp_core::ErrorCode::LocalNetworkPermissionDenied
        );
        assert!(error.retryable);
        assert!(
            error
                .detail
                .as_deref()
                .is_some_and(|detail| detail.contains("Local Network"))
        );
    }

    #[test]
    fn transport_failures_have_stable_redacted_log_labels() {
        let refused = russh::Error::IO(io::Error::from(io::ErrorKind::ConnectionRefused));

        assert_eq!(
            TransportFailureKind::from_russh_error(&refused).as_str(),
            "connection_refused"
        );
        assert_eq!(
            TransportFailureKind::from_russh_error(&russh::Error::ConnectionTimeout).as_str(),
            "timed_out"
        );
        assert_eq!(
            TransportFailureKind::from_russh_error(&russh::Error::Version).as_str(),
            "protocol_negotiation_failed"
        );
        assert_eq!(
            TransportFailureKind::from_russh_error(&russh::Error::NoCommonAlgo {
                kind: russh::AlgorithmKind::Key,
                ours: vec!["ssh-ed25519".to_string()],
                theirs: vec!["rsa-sha2-512".to_string()],
            })
            .as_str(),
            "host_key_algorithm_unsupported"
        );
    }

    #[test]
    fn client_advertises_only_rsa_sha2_host_key_algorithms() {
        let config = client_config();

        assert!(config.preferred.key.iter().any(|algorithm| matches!(
            algorithm,
            Algorithm::Rsa {
                hash: Some(HashAlg::Sha512)
            }
        )));
        assert!(config.preferred.key.iter().any(|algorithm| matches!(
            algorithm,
            Algorithm::Rsa {
                hash: Some(HashAlg::Sha256)
            }
        )));
        assert!(
            config
                .preferred
                .key
                .iter()
                .all(|algorithm| !matches!(algorithm, Algorithm::Rsa { hash: None }))
        );
    }

    #[test]
    fn legacy_rsa_host_key_is_identified_for_diagnostics() {
        let public_key = ssh_key::PublicKey::from_openssh(
            "ssh-rsa AAAAB3NzaC1yc2EAAAADAQABAAAAgQDRlWNDvO+ijXnvpTOKXqqFDPe2SdQjjo2INk7DrpiRlhr0x4xGtl9prDIy/ETQfnT/a6W6/ljLyZNGQPqei6bvnUXq9iYfQM0O0miFYREV8fo0J6oEI++Tz3iuwVVWb6LKggTNeNT+h1rx0Rb9fG/YBTjVrt+7/bmU4OI3U47gXQ==",
        )
        .expect("static RSA host-key diagnostic vector must parse");

        assert_eq!(legacy_rsa_host_key_bits(&public_key), Some(1024));
    }

    #[test]
    fn host_key_mismatch_event_uses_logical_scope() {
        let scope = RemoteEventScope::new(TabId(7), SessionId(3), 2);
        let details = HostKeyMismatchDetails {
            host: "example.com".to_string(),
            port: 22,
            expected_fingerprint_sha256: Some("SHA256:expected".to_string()),
            actual_fingerprint_sha256: "SHA256:actual".to_string(),
        };
        let event = host_key_mismatch_event(scope.clone(), details.clone());

        match event {
            AppEvent::HostKeyMismatch(mismatch) => {
                assert_eq!(mismatch.scope, scope, "event must carry the logical scope");
                assert_eq!(mismatch.host, details.host);
                assert_eq!(mismatch.port, details.port);
                assert_eq!(
                    mismatch.expected_fingerprint_sha256,
                    details.expected_fingerprint_sha256
                );
                assert_eq!(
                    mismatch.actual_fingerprint_sha256,
                    details.actual_fingerprint_sha256
                );
            }
            other => panic!("expected HostKeyMismatch, got {other:?}"),
        }
    }

    #[test]
    fn unsupported_host_key_algorithm_has_actionable_guidance() {
        let transport_error = russh::Error::NoCommonAlgo {
            kind: russh::AlgorithmKind::Key,
            ours: vec!["ssh-ed25519".to_string()],
            theirs: vec!["rsa-sha2-512".to_string()],
        };

        let error = connection_error("10.0.0.10", 8022, &transport_error);

        assert_eq!(error.title, "Unsupported SSH host key");
        assert!(!error.retryable);
        assert!(
            error
                .detail
                .as_deref()
                .is_some_and(|detail| detail.contains("Ed25519 or ECDSA"))
        );
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

    #[test]
    fn local_teardown_disconnect_is_not_logged_as_an_incident() {
        // Dropping a handle or shutting the runtime down surfaces here as
        // the same `Error::Disconnect` sentinel; it must stay unlogged so
        // real mid-session drops stay visible in the WARN stream.
        let reason = client::DisconnectReason::Error(russh::Error::Disconnect);

        assert!(classify_mid_session_disconnect(&reason).is_none());
    }

    #[test]
    fn keepalive_timeout_is_classified_as_transport_timeout() {
        let reason = client::DisconnectReason::Error(russh::Error::KeepaliveTimeout);

        match classify_mid_session_disconnect(&reason) {
            Some(MidSessionDisconnect::TransportFailed("timed_out")) => {}
            other => panic!("expected TransportFailed(\"timed_out\"), got {other:?}"),
        }
    }

    #[test]
    fn server_disconnect_keeps_only_the_enumerated_reason_code() {
        // The wire message text is untrusted third-party content and must
        // never reach the log; only the bounded reason-code enum may pass.
        let reason =
            client::DisconnectReason::ReceivedDisconnect(russh::client::RemoteDisconnectInfo {
                reason_code: russh::Disconnect::ConnectionLost,
                message: "free-form server text that must not be logged".to_string(),
                lang_tag: "en-US".to_string(),
            });

        match classify_mid_session_disconnect(&reason) {
            Some(MidSessionDisconnect::ServerClosed(russh::Disconnect::ConnectionLost)) => {}
            other => panic!("expected ServerClosed(ConnectionLost), got {other:?}"),
        }
    }

    #[test]
    fn each_physical_cause_maps_to_a_distinct_user_facing_code() {
        let reason = PhysicalDisconnectCause::ServerClosed.disconnect_reason();
        let macsftp_core::DisconnectReason::Error(error) = reason else {
            panic!("physical causes must map to DisconnectReason::Error")
        };
        assert_eq!(error.code, ErrorCode::ServerDisconnected);
        assert_eq!(error.title, "Server closed the connection");

        let reason = PhysicalDisconnectCause::NetworkTimeout.disconnect_reason();
        let macsftp_core::DisconnectReason::Error(error) = reason else {
            panic!("physical causes must map to DisconnectReason::Error")
        };
        assert_eq!(error.code, ErrorCode::NetworkTimeout);
        assert!(error.retryable);

        let reason = PhysicalDisconnectCause::NetworkError.disconnect_reason();
        let macsftp_core::DisconnectReason::Error(error) = reason else {
            panic!("physical causes must map to DisconnectReason::Error")
        };
        assert_eq!(error.code, ErrorCode::NetworkError);
    }

    fn test_handler() -> ClientHandler {
        ClientHandler {
            host: "example.com".to_string(),
            port: 22,
            scope: RemoteEventScope::new(TabId(1), SessionId(1), 1),
            trust_request_id: TrustRequestId(1),
            known_hosts: Arc::new(Mutex::new(crate::known_hosts::KnownHostsStore::empty())),
            trust_config: Arc::new(HostTrustConfig::new(
                PathBuf::from("/tmp/known_hosts"),
                None,
            )),
            trust_registry: Arc::new(TrustRegistry::new()),
            event_tx: flume::unbounded().0,
            rejection: Arc::new(Mutex::new(None)),
            connection_lost: CancellationToken::new(),
            disconnect_cause: Arc::new(Mutex::new(None)),
        }
    }

    #[tokio::test]
    async fn server_disconnect_records_cause_before_cancelling_token() {
        // The actor reads the cause only after waking up on the cancelled
        // token, so recording must happen strictly before cancellation —
        // otherwise it races and users see an unclassified disconnect.
        let mut handler = test_handler();
        let reason =
            client::DisconnectReason::ReceivedDisconnect(russh::client::RemoteDisconnectInfo {
                reason_code: russh::Disconnect::ByApplication,
                message: "free-form text".to_string(),
                lang_tag: "en-US".to_string(),
            });

        handler.disconnected(reason).await.expect("must succeed");

        assert!(handler.connection_lost.is_cancelled());
        let recorded = handler
            .disconnect_cause
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        assert_eq!(*recorded, Some(PhysicalDisconnectCause::ServerClosed));
    }

    #[tokio::test]
    async fn keepalive_timeout_records_network_timeout_cause() {
        let mut handler = test_handler();
        let reason = client::DisconnectReason::Error(russh::Error::KeepaliveTimeout);

        let result = handler
            .disconnected(reason)
            .await
            .expect_err("transport errors must propagate");

        assert!(matches!(result, russh::Error::KeepaliveTimeout));
        let recorded = handler
            .disconnect_cause
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        assert_eq!(*recorded, Some(PhysicalDisconnectCause::NetworkTimeout));
    }

    #[tokio::test]
    async fn local_teardown_records_no_cause() {
        // The Error::Disconnect sentinel means macSFTP closed the connection
        // itself; no incident cause must be recorded for it.
        let mut handler = test_handler();
        let reason = client::DisconnectReason::Error(russh::Error::Disconnect);

        handler
            .disconnected(reason)
            .await
            .expect_err("sentinel must propagate as error");

        let recorded = handler
            .disconnect_cause
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        assert_eq!(*recorded, None);
    }
}
