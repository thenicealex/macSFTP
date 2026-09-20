use std::pin::Pin;
use std::process::Stdio;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex, MutexGuard};
use std::task::{Context, Poll};
use std::time::Duration;

use aws_lc_rs::rand::SystemRandom;
use aws_lc_rs::signature::{RSA_PKCS1_SHA256, RSA_PKCS1_SHA512, RsaKeyPair};
use crypto_bigint::{BoxedUint, CheckedSub, NonZero};
use der::asn1::UintRef;
use macsftp_core::{
    AppEvent, AuthCredential, AuthFailure, ConnectionSettings, ErrorCode, HostKeyMismatch,
    HostKeyPrompt, KeyboardInteractivePrompt, KeyboardInteractivePromptField,
    KeyboardInteractiveRequestId, RemoteEventScope, RemoteScoped, ResolvedConnectionRoute,
    TrustDecision, TrustRequestId, UserFacingError,
};
use pkcs1::RsaPrivateKey as Pkcs1RsaPrivateKey;
use russh::Signer;
use russh::client;
use russh::client::KeyboardInteractiveAuthResponse;
use russh::keys::agent::{AgentIdentity, client::AgentClient};
use russh::keys::ssh_encoding::Encode;
use russh::keys::{PrivateKeyWithHashAlg, load_secret_key};
use ssh_key::{Algorithm, EcdsaCurve, HashAlg};
use tokio::io::{AsyncRead, AsyncWrite, ReadBuf};
use tokio::process::{Child, ChildStdin, ChildStdout, Command};
use tokio::sync::oneshot;
use tokio_util::sync::CancellationToken;
use tracing::{debug, info, warn};
use zeroize::Zeroizing;

use crate::keyboard_interactive::{KeyboardInteractiveRegistry, KeyboardInteractiveRegistryEntry};
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
    // Legacy SHA-1 `ssh-rsa` remains excluded. Direct RSA client signatures
    // also use AWS-LC and never enable russh's RustCrypto `rsa` feature.
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

#[derive(Debug)]
enum AwsLcRsaSignerError {
    Send,
    InvalidKey,
    Signing,
}

impl From<russh::SendError> for AwsLcRsaSignerError {
    fn from(_error: russh::SendError) -> Self {
        Self::Send
    }
}

struct AwsLcRsaSigner {
    key_pair: RsaKeyPair,
    public_key: ssh_key::PublicKey,
}

fn rsa_crt_exponent(
    private_exponent: &[u8],
    prime: &[u8],
    bits_precision: u32,
) -> Result<Zeroizing<Box<[u8]>>, AwsLcRsaSignerError> {
    let private_exponent = Zeroizing::new(
        BoxedUint::from_be_slice(private_exponent, bits_precision)
            .map_err(|_| AwsLcRsaSignerError::InvalidKey)?,
    );
    let prime = Zeroizing::new(
        BoxedUint::from_be_slice(prime, bits_precision)
            .map_err(|_| AwsLcRsaSignerError::InvalidKey)?,
    );
    let modulus = prime
        .checked_sub(&BoxedUint::one_with_precision(bits_precision))
        .into_option()
        .ok_or(AwsLcRsaSignerError::InvalidKey)?;
    let modulus = Zeroizing::new(
        NonZero::new(modulus)
            .into_option()
            .ok_or(AwsLcRsaSignerError::InvalidKey)?,
    );
    let exponent = Zeroizing::new(&*private_exponent % &*modulus);
    Ok(Zeroizing::new(exponent.to_be_bytes_trimmed_vartime()))
}

impl AwsLcRsaSigner {
    fn new(key: &ssh_key::PrivateKey) -> Result<Self, AwsLcRsaSignerError> {
        let rsa = key
            .key_data()
            .rsa()
            .ok_or(AwsLcRsaSignerError::InvalidKey)?;
        if rsa.key_size() < 2048 {
            return Err(AwsLcRsaSignerError::InvalidKey);
        }
        let public = rsa.public();
        let private = rsa.private();
        let private_exponent = private
            .d()
            .as_positive_bytes()
            .ok_or(AwsLcRsaSignerError::InvalidKey)?;
        let prime1 = private
            .p()
            .as_positive_bytes()
            .ok_or(AwsLcRsaSignerError::InvalidKey)?;
        let prime2 = private
            .q()
            .as_positive_bytes()
            .ok_or(AwsLcRsaSignerError::InvalidKey)?;
        let exponent1 = rsa_crt_exponent(private_exponent, prime1, rsa.key_size())?;
        let exponent2 = rsa_crt_exponent(private_exponent, prime2, rsa.key_size())?;
        let modulus = public
            .n()
            .as_positive_bytes()
            .ok_or(AwsLcRsaSignerError::InvalidKey)?;
        let public_exponent = public
            .e()
            .as_positive_bytes()
            .ok_or(AwsLcRsaSignerError::InvalidKey)?;
        let coefficient = private
            .iqmp()
            .as_positive_bytes()
            .ok_or(AwsLcRsaSignerError::InvalidKey)?;
        let document = der::SecretDocument::try_from(&Pkcs1RsaPrivateKey {
            modulus: UintRef::new(modulus).map_err(|_| AwsLcRsaSignerError::InvalidKey)?,
            public_exponent: UintRef::new(public_exponent)
                .map_err(|_| AwsLcRsaSignerError::InvalidKey)?,
            private_exponent: UintRef::new(private_exponent)
                .map_err(|_| AwsLcRsaSignerError::InvalidKey)?,
            prime1: UintRef::new(prime1).map_err(|_| AwsLcRsaSignerError::InvalidKey)?,
            prime2: UintRef::new(prime2).map_err(|_| AwsLcRsaSignerError::InvalidKey)?,
            exponent1: UintRef::new(&exponent1).map_err(|_| AwsLcRsaSignerError::InvalidKey)?,
            exponent2: UintRef::new(&exponent2).map_err(|_| AwsLcRsaSignerError::InvalidKey)?,
            coefficient: UintRef::new(coefficient).map_err(|_| AwsLcRsaSignerError::InvalidKey)?,
            other_prime_infos: None,
        })
        .map_err(|_| AwsLcRsaSignerError::InvalidKey)?;
        let key_pair = RsaKeyPair::from_der(document.as_bytes())
            .map_err(|_| AwsLcRsaSignerError::InvalidKey)?;
        Ok(Self {
            key_pair,
            public_key: key.public_key().clone(),
        })
    }
}

impl Signer for AwsLcRsaSigner {
    type Error = AwsLcRsaSignerError;

    async fn auth_sign(
        &mut self,
        key: &AgentIdentity,
        hash_alg: Option<HashAlg>,
        mut to_sign: Vec<u8>,
    ) -> Result<Vec<u8>, Self::Error> {
        if key.public_key().key_data() != self.public_key.key_data() {
            return Err(AwsLcRsaSignerError::InvalidKey);
        }
        let (algorithm_name, encoding) = match hash_alg {
            Some(HashAlg::Sha512) => ("rsa-sha2-512", &RSA_PKCS1_SHA512),
            Some(HashAlg::Sha256) => ("rsa-sha2-256", &RSA_PKCS1_SHA256),
            None | Some(_) => return Err(AwsLcRsaSignerError::Signing),
        };
        let mut signature = vec![0u8; self.key_pair.public_modulus_len()];
        self.key_pair
            .sign(encoding, &SystemRandom::new(), &to_sign, &mut signature)
            .map_err(|_| AwsLcRsaSignerError::Signing)?;
        (algorithm_name.len() + signature.len() + 8)
            .encode(&mut to_sign)
            .map_err(|_| AwsLcRsaSignerError::Signing)?;
        algorithm_name
            .encode(&mut to_sign)
            .map_err(|_| AwsLcRsaSignerError::Signing)?;
        signature
            .encode(&mut to_sign)
            .map_err(|_| AwsLcRsaSignerError::Signing)?;
        Ok(to_sign)
    }
}

async fn authenticate_rsa_private_key(
    handle: &mut client::Handle<ClientHandler>,
    settings: &ConnectionSettings,
    key: &ssh_key::PrivateKey,
) -> Result<client::AuthResult, ConnectFailure> {
    let server_support = handle.best_supported_rsa_hash().await.map_err(|error| {
        ConnectFailure::Connection(connection_error(&settings.host, settings.port, &error))
    })?;
    let hash = match server_support {
        Some(Some(hash @ (HashAlg::Sha256 | HashAlg::Sha512))) => hash,
        None => HashAlg::Sha512,
        Some(None) => {
            return Err(ConnectFailure::AuthFailed(AuthFailure {
                reason: UserFacingError::new(
                    ErrorCode::AuthFailed,
                    "Server does not support RSA-SHA2",
                    "Enable rsa-sha2-256 or rsa-sha2-512 on the server. SHA-1 ssh-rsa is not supported.",
                ),
            }));
        }
        Some(Some(_)) => {
            return Err(ConnectFailure::AuthFailed(AuthFailure {
                reason: UserFacingError::new(
                    ErrorCode::AuthFailed,
                    "Unsupported RSA hash algorithm",
                    "The server selected an RSA signature algorithm macSFTP does not support.",
                ),
            }));
        }
    };
    let mut signer = AwsLcRsaSigner::new(key).map_err(|_| {
        ConnectFailure::AuthFailed(AuthFailure {
            reason: UserFacingError::new(
                ErrorCode::AuthFailed,
                "Could not use RSA private key",
                "RSA client keys must be valid and at least 2048 bits.",
            ),
        })
    })?;
    handle
        .authenticate_publickey_with(
            settings.username.clone(),
            key.public_key().clone(),
            Some(hash),
            &mut signer,
        )
        .await
        .map_err(|_| {
            ConnectFailure::AuthFailed(AuthFailure {
                reason: UserFacingError::new(
                    ErrorCode::AuthFailed,
                    "RSA private-key authentication failed",
                    "AWS-LC could not sign the SSH authentication request.",
                ),
            })
        })
}

async fn emit_auth_failure(
    scope: &RemoteEventScope,
    event_tx: &flume::Sender<AppEvent>,
    reason: UserFacingError,
) -> ConnectFailure {
    let failure = AuthFailure {
        reason: reason.clone(),
    };
    if let Err(send_error) = event_tx
        .send_async(AppEvent::AuthFailed(RemoteScoped::new(
            scope.clone(),
            failure.clone(),
        )))
        .await
    {
        warn!(error = %send_error, "authentication failure event dropped");
    }
    ConnectFailure::AuthFailed(failure)
}

async fn authenticate_keyboard_interactive(
    handle: &mut client::Handle<ClientHandler>,
    settings: &ConnectionSettings,
    scope: &RemoteEventScope,
    event_tx: &flume::Sender<AppEvent>,
    registry: &KeyboardInteractiveRegistry,
    next_request_id: &AtomicU64,
) -> Result<client::AuthResult, ConnectFailure> {
    let mut result = handle
        .authenticate_keyboard_interactive_start(settings.username.clone(), None::<String>)
        .await
        .map_err(|error| {
            ConnectFailure::Connection(connection_error(&settings.host, settings.port, &error))
        })?;

    loop {
        result = match result {
            KeyboardInteractiveAuthResponse::Success => return Ok(client::AuthResult::Success),
            KeyboardInteractiveAuthResponse::Failure {
                remaining_methods,
                partial_success,
            } => {
                return Ok(client::AuthResult::Failure {
                    remaining_methods,
                    partial_success,
                });
            }
            KeyboardInteractiveAuthResponse::InfoRequest {
                name,
                instructions,
                prompts,
            } => {
                let request_id =
                    KeyboardInteractiveRequestId(next_request_id.fetch_add(1, Ordering::Relaxed));
                let prompt_count = prompts.len();
                let (responder, response_rx) = oneshot::channel();
                registry.register(
                    request_id,
                    KeyboardInteractiveRegistryEntry {
                        tab_id: scope.tab_id,
                        session_epoch: scope.session_epoch,
                        responder,
                    },
                );
                let event = AppEvent::KeyboardInteractivePrompt(KeyboardInteractivePrompt {
                    request_id,
                    scope: scope.clone(),
                    name,
                    instruction: instructions,
                    prompts: prompts
                        .into_iter()
                        .map(|prompt| KeyboardInteractivePromptField {
                            prompt: prompt.prompt,
                            echo: prompt.echo,
                        })
                        .collect(),
                });
                if event_tx.send_async(event).await.is_err() {
                    registry.cancel(request_id);
                    let reason = UserFacingError::new(
                        ErrorCode::ChannelClosed,
                        "Authentication prompt unavailable",
                        "The application could not display the server's authentication prompt.",
                    );
                    return Err(emit_auth_failure(scope, event_tx, reason).await);
                }
                let response = tokio::time::timeout(Duration::from_secs(120), response_rx).await;
                let mut response = match response {
                    Ok(Ok(Some(response))) => response,
                    Ok(Ok(None)) | Ok(Err(_)) => {
                        let reason = UserFacingError::new(
                            ErrorCode::Cancelled,
                            "Authentication cancelled",
                            "The keyboard-interactive prompt was cancelled.",
                        );
                        return Err(emit_auth_failure(scope, event_tx, reason).await);
                    }
                    Err(_) => {
                        registry.cancel(request_id);
                        let reason = UserFacingError::new(
                            ErrorCode::AuthFailed,
                            "Authentication prompt timed out",
                            "The server's keyboard-interactive prompt was not answered in time.",
                        );
                        return Err(emit_auth_failure(scope, event_tx, reason).await);
                    }
                };
                if response.responses.len() != prompt_count {
                    let reason = UserFacingError::new(
                        ErrorCode::AuthFailed,
                        "Invalid authentication response",
                        "The number of responses did not match the server's prompts.",
                    );
                    return Err(emit_auth_failure(scope, event_tx, reason).await);
                }
                handle
                    .authenticate_keyboard_interactive_respond(response.take_responses())
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
    }
}

async fn authenticate_ssh_agent(
    handle: &mut client::Handle<ClientHandler>,
    settings: &ConnectionSettings,
    socket_path: Option<&str>,
) -> Result<client::AuthResult, ConnectFailure> {
    let agent_result = match socket_path {
        Some(path) => AgentClient::connect_uds(path).await,
        None => AgentClient::connect_env().await,
    };
    let mut agent = agent_result.map_err(|_| {
        ConnectFailure::AuthFailed(AuthFailure {
            reason: UserFacingError::new(
                ErrorCode::AuthFailed,
                "SSH agent unavailable",
                "Start an SSH agent and make SSH_AUTH_SOCK available to macSFTP.",
            ),
        })
    })?;
    let identities = agent.request_identities().await.map_err(|_| {
        ConnectFailure::AuthFailed(AuthFailure {
            reason: UserFacingError::new(
                ErrorCode::AuthFailed,
                "Could not read SSH agent identities",
                "The SSH agent did not return its available identities.",
            ),
        })
    })?;
    if identities.is_empty() {
        return Err(ConnectFailure::AuthFailed(AuthFailure {
            reason: UserFacingError::new(
                ErrorCode::AuthFailed,
                "SSH agent has no identities",
                "Add a key to the SSH agent and try again.",
            ),
        }));
    }

    let rsa_hash_support = handle.best_supported_rsa_hash().await.map_err(|error| {
        ConnectFailure::Connection(connection_error(&settings.host, settings.port, &error))
    })?;
    let mut last_failure = None;
    for identity in identities {
        let public_key = identity.public_key();
        let hash = if matches!(public_key.algorithm(), Algorithm::Rsa { .. }) {
            match rsa_hash_support {
                Some(Some(hash)) => Some(hash),
                None => Some(HashAlg::Sha512),
                Some(None) => continue,
            }
        } else {
            None
        };
        let result = match identity.clone() {
            AgentIdentity::PublicKey { key, .. } => {
                handle
                    .authenticate_publickey_with(settings.username.clone(), key, hash, &mut agent)
                    .await
            }
            AgentIdentity::Certificate { certificate, .. } => {
                handle
                    .authenticate_certificate_with(
                        settings.username.clone(),
                        certificate,
                        hash,
                        &mut agent,
                    )
                    .await
            }
        }
        .map_err(|_| {
            ConnectFailure::AuthFailed(AuthFailure {
                reason: UserFacingError::new(
                    ErrorCode::AuthFailed,
                    "SSH agent signing failed",
                    "The SSH agent could not sign the authentication request.",
                ),
            })
        })?;
        if result.success() {
            return Ok(result);
        }
        last_failure = Some(result);
    }

    match last_failure {
        Some(failure) => Ok(failure),
        None => Err(ConnectFailure::AuthFailed(AuthFailure {
            reason: UserFacingError::new(
                ErrorCode::AuthFailed,
                "No compatible SSH agent identity",
                "The server did not accept any compatible identity from the SSH agent.",
            ),
        })),
    }
}

async fn authenticate(
    handle: &mut client::Handle<ClientHandler>,
    settings: &ConnectionSettings,
    scope: &RemoteEventScope,
    event_tx: &flume::Sender<AppEvent>,
    keyboard_interactive_registry: &KeyboardInteractiveRegistry,
    next_keyboard_interactive_id: &AtomicU64,
) -> Result<(), ConnectFailure> {
    let method = match &settings.auth {
        AuthCredential::Password { .. } => "password",
        AuthCredential::PrivateKey { .. } => "private_key",
        AuthCredential::KeyboardInteractive => "keyboard_interactive",
        AuthCredential::SshAgent { .. } => "ssh_agent",
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
            if matches!(key.algorithm(), Algorithm::Rsa { .. }) {
                match authenticate_rsa_private_key(handle, settings, &key).await {
                    Ok(result) => result,
                    Err(ConnectFailure::AuthFailed(failure)) => {
                        return Err(emit_auth_failure(scope, event_tx, failure.reason).await);
                    }
                    Err(other) => return Err(other),
                }
            } else {
                handle
                    .authenticate_publickey(
                        settings.username.clone(),
                        PrivateKeyWithHashAlg::new(Arc::new(key), None),
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
        }
        AuthCredential::KeyboardInteractive => {
            authenticate_keyboard_interactive(
                handle,
                settings,
                scope,
                event_tx,
                keyboard_interactive_registry,
                next_keyboard_interactive_id,
            )
            .await?
        }
        AuthCredential::SshAgent { socket_path } => {
            match authenticate_ssh_agent(handle, settings, socket_path.as_deref()).await {
                Ok(result) => result,
                Err(ConnectFailure::AuthFailed(failure)) => {
                    return Err(emit_auth_failure(scope, event_tx, failure.reason).await);
                }
                Err(other) => return Err(other),
            }
        }
    };

    let auth_result = match &auth_result {
        client::AuthResult::Failure {
            remaining_methods,
            partial_success: true,
        } if !matches!(settings.auth, AuthCredential::KeyboardInteractive)
            && remaining_methods.contains(&russh::MethodKind::KeyboardInteractive) =>
        {
            authenticate_keyboard_interactive(
                handle,
                settings,
                scope,
                event_tx,
                keyboard_interactive_registry,
                next_keyboard_interactive_id,
            )
            .await?
        }
        _ => auth_result,
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

struct ProxyCommandStream {
    stdout: ChildStdout,
    stdin: ChildStdin,
    _child: Child,
}

impl AsyncRead for ProxyCommandStream {
    fn poll_read(
        mut self: Pin<&mut Self>,
        context: &mut Context<'_>,
        buffer: &mut ReadBuf<'_>,
    ) -> Poll<std::io::Result<()>> {
        Pin::new(&mut self.stdout).poll_read(context, buffer)
    }
}

impl AsyncWrite for ProxyCommandStream {
    fn poll_write(
        mut self: Pin<&mut Self>,
        context: &mut Context<'_>,
        buffer: &[u8],
    ) -> Poll<std::io::Result<usize>> {
        Pin::new(&mut self.stdin).poll_write(context, buffer)
    }

    fn poll_flush(
        mut self: Pin<&mut Self>,
        context: &mut Context<'_>,
    ) -> Poll<std::io::Result<()>> {
        Pin::new(&mut self.stdin).poll_flush(context)
    }

    fn poll_shutdown(
        mut self: Pin<&mut Self>,
        context: &mut Context<'_>,
    ) -> Poll<std::io::Result<()>> {
        Pin::new(&mut self.stdin).poll_shutdown(context)
    }
}

struct JumpHostStream {
    stream: russh::ChannelStream<client::Msg>,
    _jump_handle: client::Handle<ClientHandler>,
}

impl AsyncRead for JumpHostStream {
    fn poll_read(
        mut self: Pin<&mut Self>,
        context: &mut Context<'_>,
        buffer: &mut ReadBuf<'_>,
    ) -> Poll<std::io::Result<()>> {
        Pin::new(&mut self.stream).poll_read(context, buffer)
    }
}

impl AsyncWrite for JumpHostStream {
    fn poll_write(
        mut self: Pin<&mut Self>,
        context: &mut Context<'_>,
        buffer: &[u8],
    ) -> Poll<std::io::Result<usize>> {
        Pin::new(&mut self.stream).poll_write(context, buffer)
    }

    fn poll_flush(
        mut self: Pin<&mut Self>,
        context: &mut Context<'_>,
    ) -> Poll<std::io::Result<()>> {
        Pin::new(&mut self.stream).poll_flush(context)
    }

    fn poll_shutdown(
        mut self: Pin<&mut Self>,
        context: &mut Context<'_>,
    ) -> Poll<std::io::Result<()>> {
        Pin::new(&mut self.stream).poll_shutdown(context)
    }
}

fn shell_quote(value: &str) -> String {
    format!("'{}'", value.replace('\'', "'\\''"))
}

fn expand_proxy_command(command: &str, settings: &ConnectionSettings) -> String {
    let mut expanded = String::with_capacity(command.len());
    let mut characters = command.chars();
    while let Some(character) = characters.next() {
        if character != '%' {
            expanded.push(character);
            continue;
        }
        match characters.next() {
            Some('%') => expanded.push('%'),
            Some('h') => expanded.push_str(&shell_quote(&settings.host)),
            Some('p') => expanded.push_str(&settings.port.to_string()),
            Some('r') => expanded.push_str(&shell_quote(&settings.username)),
            Some(other) => {
                expanded.push('%');
                expanded.push(other);
            }
            None => expanded.push('%'),
        }
    }
    expanded
}

fn spawn_proxy_command(
    command: &str,
    settings: &ConnectionSettings,
) -> Result<ProxyCommandStream, ConnectFailure> {
    let expanded = expand_proxy_command(command, settings);
    let mut child = Command::new("/bin/sh")
        .arg("-c")
        .arg(expanded)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .kill_on_drop(true)
        .spawn()
        .map_err(|_| {
            ConnectFailure::Connection(
                UserFacingError::new(
                    ErrorCode::ChannelClosed,
                    "Could not start ProxyCommand",
                    "The configured proxy command could not be started.",
                )
                .with_retryable(true),
            )
        })?;
    let stdin = child.stdin.take().ok_or_else(|| {
        ConnectFailure::Connection(UserFacingError::new(
            ErrorCode::ChannelClosed,
            "Could not start ProxyCommand",
            "The proxy command did not provide a writable input stream.",
        ))
    })?;
    let stdout = child.stdout.take().ok_or_else(|| {
        ConnectFailure::Connection(UserFacingError::new(
            ErrorCode::ChannelClosed,
            "Could not start ProxyCommand",
            "The proxy command did not provide a readable output stream.",
        ))
    })?;
    Ok(ProxyCommandStream {
        stdout,
        stdin,
        _child: child,
    })
}

/// Trust registry, known_hosts, and event_tx are co-required for host-key flow.
#[allow(clippy::too_many_arguments)]
async fn establish_on_stream<R>(
    stream: R,
    settings: &ConnectionSettings,
    scope: &RemoteEventScope,
    trust_request_id: TrustRequestId,
    known_hosts: Arc<Mutex<KnownHostsStore>>,
    trust_config: Arc<HostTrustConfig>,
    trust_registry: Arc<TrustRegistry>,
    keyboard_interactive_registry: Arc<KeyboardInteractiveRegistry>,
    next_keyboard_interactive_id: Arc<AtomicU64>,
    event_tx: flume::Sender<AppEvent>,
    connection_lost: CancellationToken,
    disconnect_cause: Arc<Mutex<Option<PhysicalDisconnectCause>>>,
) -> Result<client::Handle<ClientHandler>, ConnectFailure>
where
    R: AsyncRead + AsyncWrite + Unpin + Send + 'static,
{
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

    let mut handle = match client::connect_stream(config, stream, handler).await {
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

    authenticate(
        &mut handle,
        settings,
        scope,
        &event_tx,
        &keyboard_interactive_registry,
        &next_keyboard_interactive_id,
    )
    .await?;

    Ok(handle)
}

async fn connect_tcp_stream(
    settings: &ConnectionSettings,
) -> Result<tokio::net::TcpStream, ConnectFailure> {
    tokio::net::TcpStream::connect((settings.host.as_str(), settings.port))
        .await
        .map_err(|error| {
            ConnectFailure::Connection(connection_error(
                &settings.host,
                settings.port,
                &russh::Error::IO(error),
            ))
        })
}

/// Trust registry, known_hosts, and event_tx are co-required for host-key flow.
#[allow(clippy::too_many_arguments)]
pub async fn establish_physical_connection(
    settings: &ConnectionSettings,
    scope: &RemoteEventScope,
    trust_request_id: TrustRequestId,
    jump_trust_request_id: TrustRequestId,
    known_hosts: Arc<Mutex<KnownHostsStore>>,
    trust_config: Arc<HostTrustConfig>,
    trust_registry: Arc<TrustRegistry>,
    keyboard_interactive_registry: Arc<KeyboardInteractiveRegistry>,
    next_keyboard_interactive_id: Arc<AtomicU64>,
    event_tx: flume::Sender<AppEvent>,
    connection_lost: CancellationToken,
    disconnect_cause: Arc<Mutex<Option<PhysicalDisconnectCause>>>,
) -> Result<client::Handle<ClientHandler>, ConnectFailure> {
    match &settings.route {
        ResolvedConnectionRoute::Direct => {
            let stream = connect_tcp_stream(settings).await?;
            establish_on_stream(
                stream,
                settings,
                scope,
                trust_request_id,
                known_hosts,
                trust_config,
                trust_registry,
                keyboard_interactive_registry,
                next_keyboard_interactive_id,
                event_tx,
                connection_lost,
                disconnect_cause,
            )
            .await
        }
        ResolvedConnectionRoute::ProxyCommand { command } => {
            let stream = spawn_proxy_command(command, settings)?;
            establish_on_stream(
                stream,
                settings,
                scope,
                trust_request_id,
                known_hosts,
                trust_config,
                trust_registry,
                keyboard_interactive_registry,
                next_keyboard_interactive_id,
                event_tx,
                connection_lost,
                disconnect_cause,
            )
            .await
        }
        ResolvedConnectionRoute::JumpHost {
            settings: jump_settings,
        } => {
            if !matches!(&jump_settings.route, ResolvedConnectionRoute::Direct) {
                return Err(ConnectFailure::Connection(UserFacingError::new(
                    ErrorCode::ChannelClosed,
                    "Invalid jump-host route",
                    "Jump-host profiles must connect directly.",
                )));
            }
            let jump_stream = connect_tcp_stream(jump_settings).await?;
            let jump_handle = establish_on_stream(
                jump_stream,
                jump_settings,
                scope,
                jump_trust_request_id,
                known_hosts.clone(),
                trust_config.clone(),
                trust_registry.clone(),
                keyboard_interactive_registry.clone(),
                next_keyboard_interactive_id.clone(),
                event_tx.clone(),
                connection_lost.clone(),
                disconnect_cause.clone(),
            )
            .await?;
            let channel = jump_handle
                .channel_open_direct_tcpip(
                    settings.host.clone(),
                    u32::from(settings.port),
                    "127.0.0.1",
                    0,
                )
                .await
                .map_err(|_| {
                    ConnectFailure::Connection(
                        UserFacingError::new(
                            ErrorCode::ChannelClosed,
                            "Jump host could not reach target",
                            "The jump host rejected the TCP forwarding request.",
                        )
                        .with_retryable(true),
                    )
                })?;
            let stream = JumpHostStream {
                stream: channel.into_stream(),
                _jump_handle: jump_handle,
            };
            establish_on_stream(
                stream,
                settings,
                scope,
                trust_request_id,
                known_hosts,
                trust_config,
                trust_registry,
                keyboard_interactive_registry,
                next_keyboard_interactive_id,
                event_tx,
                connection_lost,
                disconnect_cause,
            )
            .await
        }
    }
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
        client_config, connection_error, expand_proxy_command, host_key_mismatch_event,
        legacy_rsa_host_key_bits, private_key_file_name, sftp_connection_error,
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
    fn proxy_command_expansion_quotes_target_values() {
        let settings = macsftp_core::ConnectionSettings {
            host: "host'; touch /tmp/never".into(),
            port: 2200,
            username: "user name".into(),
            auth: macsftp_core::AuthCredential::KeyboardInteractive,
            route: macsftp_core::ResolvedConnectionRoute::Direct,
        };
        assert_eq!(
            expand_proxy_command("proxy --host %h --port %p --user %r %%", &settings),
            "proxy --host 'host'\\''; touch /tmp/never' --port 2200 --user 'user name' %"
        );
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
