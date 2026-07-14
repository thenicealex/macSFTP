//! Integration tests for the real `RemoteSessionActor` against a local
//! OpenSSH server (see `macsftp_test_support::SshTestServer`).
//!
//! Skipped (with a message) when no local sshd is available. CI must
//! provide a real server per plan §19.

use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use macsftp_core::{
    AppCommand, AppEvent, AuthCredential, ConflictDecision, ConnectCommand, ConnectionPoolIdentity,
    ConnectionSettings, DisconnectReason, ErrorCode, FileKind, ProfileId, RemoteEventScope,
    RemotePath, RuntimeBridgeConfig, SessionId, TabId, Timestamp, TransferDirection,
    TransferEndpoint, TransferId, TransferJob, TransferState, TrustDecision, TrustRequestId,
};
use macsftp_sftp::pool::ConnectionManager;
use macsftp_sftp::{
    EventReceiver, HostTrustConfig, KnownHostsStore, RemoteSessionActor, RemoteSessionRequest,
    RuntimeController, SessionBackend, TrustRegistry,
};
use macsftp_test_support::SshTestServer;
use tokio_util::sync::CancellationToken;

const TAB: TabId = TabId(1);
const SESSION: SessionId = SessionId(1);
const EPOCH: u64 = 1;
const TRUST_REQUEST: TrustRequestId = TrustRequestId(1);
const SECOND_TAB: TabId = TabId(2);
const SECOND_SESSION: SessionId = SessionId(2);

struct ActorFixture {
    events: flume::Receiver<AppEvent>,
    requests: flume::Sender<RemoteSessionRequest>,
    trust_registry: Arc<TrustRegistry>,
    cancel: CancellationToken,
    app_known_hosts_path: PathBuf,
}

/// Spawn the real actor against the fixture server on the current
/// tokio runtime.
fn spawn_actor(
    server: &SshTestServer,
    auth: AuthCredential,
    prefill_host_key: Option<&str>,
) -> ActorFixture {
    let (event_tx, event_rx) = flume::bounded(64);
    let trust_registry = Arc::new(TrustRegistry::new());
    let app_known_hosts_path = server.fixture_dir.join("app_known_hosts");

    if let Some(key_line) = prefill_host_key {
        let line = format!("[127.0.0.1]:{} {key_line}\n", server.port);
        std::fs::write(&app_known_hosts_path, line).expect("prefill app known_hosts");
    }

    let known_hosts = Arc::new(Mutex::new(KnownHostsStore::load(
        &app_known_hosts_path,
        None,
    )));
    let trust_config = Arc::new(HostTrustConfig::new(app_known_hosts_path.clone(), None));

    let settings = ConnectionSettings {
        host: "127.0.0.1".to_string(),
        port: server.port,
        username: server.username.clone(),
        auth,
    };

    let (requests, request_rx) = flume::bounded(16);
    let cancel = CancellationToken::new();

    // Establish the connection via the Connection Pool (mirrors the runtime
    // path), then build the actor from the already-connected SharedConnection
    // plus a fresh SFTP session. We return the fixture immediately and let the
    // test resolve the host-key trust prompt via `next_event`, which runs
    // concurrently with the connection task — so there is no deadlock.
    let cm = Arc::new(ConnectionManager::new());
    let scope = RemoteEventScope::new(TAB, SESSION, EPOCH);
    let connect_rx = cm.connect_session(
        &settings,
        &ConnectionPoolIdentity::Ephemeral(SESSION),
        &scope,
        TRUST_REQUEST,
        known_hosts,
        trust_config,
        trust_registry.clone(),
        event_tx.clone(),
        CancellationToken::new(),
    );

    let actor_cancel = cancel.clone();
    tokio::spawn(async move {
        if let Ok(Ok((shared, sftp))) = connect_rx.recv_async().await {
            let actor = RemoteSessionActor::new(
                TAB,
                SESSION,
                EPOCH,
                shared,
                sftp,
                event_tx,
                Arc::new(std::sync::atomic::AtomicU64::new(1)),
            );
            actor.run(actor_cancel, request_rx).await;
        }
    });

    ActorFixture {
        events: event_rx,
        requests,
        trust_registry,
        cancel,
        app_known_hosts_path,
    }
}

async fn next_event(fixture: &ActorFixture, label: &str) -> AppEvent {
    match tokio::time::timeout(Duration::from_secs(15), fixture.events.recv_async()).await {
        Ok(Ok(event)) => event,
        Ok(Err(_)) => panic!("{label}: event channel closed"),
        Err(_) => panic!("{label}: timed out waiting for event"),
    }
}

async fn next_runtime_event(events: &mut EventReceiver, label: &str) -> AppEvent {
    match tokio::time::timeout(Duration::from_secs(15), events.recv()).await {
        Ok(Some(event)) => event,
        Ok(None) => panic!("{label}: event channel closed"),
        Err(_) => panic!("{label}: timed out waiting for event"),
    }
}

async fn wait_for_transfer_completion(fixture: &ActorFixture, transfer_id: TransferId) {
    loop {
        match next_event(fixture, "transfer event").await {
            AppEvent::TransferRunning(snapshot) if snapshot.job.id == transfer_id => {}
            AppEvent::TransferProgress(progress) if progress.transfer_id == transfer_id => {}
            AppEvent::TransferCompleted {
                transfer_id: completed_id,
            } if completed_id == transfer_id => return,
            AppEvent::TransferFailed(failure) if failure.transfer_id == transfer_id => {
                panic!("transfer failed: {:?}", failure.error)
            }
            AppEvent::ResidualTempCreated(_) | AppEvent::ResidualTempCleared { .. } => {}
            other => panic!("unexpected transfer event: {other:?}"),
        }
    }
}

async fn wait_for_runtime_transfer_completion(events: &mut EventReceiver) -> TransferId {
    let mut transfer_id = None;
    loop {
        match next_runtime_event(events, "runtime transfer event").await {
            AppEvent::TransferPlanStarted(_) | AppEvent::TransferPlanCompleted { .. } => {}
            AppEvent::TransferPlanProgress(progress) => {
                transfer_id = progress.child_jobs.first().map(|job| job.id);
            }
            AppEvent::TransferRunning(snapshot) => {
                assert_eq!(Some(snapshot.job.id), transfer_id);
            }
            AppEvent::TransferProgress(progress) => {
                assert_eq!(Some(progress.transfer_id), transfer_id);
            }
            AppEvent::TransferCompleted {
                transfer_id: completed_id,
            } if Some(completed_id) == transfer_id => return completed_id,
            AppEvent::TransferFailed(failure) => {
                panic!("runtime transfer failed: {:?}", failure.error)
            }
            AppEvent::ResidualTempCreated(_) | AppEvent::ResidualTempCleared { .. } => {}
            other => panic!("unexpected runtime transfer event: {other:?}"),
        }
    }
}

fn transfer_job(
    id: u64,
    direction: TransferDirection,
    source: TransferEndpoint,
    destination: TransferEndpoint,
) -> TransferJob {
    TransferJob {
        id: TransferId(id),
        direction,
        source,
        destination,
        state: TransferState::Queued,
        metadata_policy: macsftp_core::MetadataPolicy::default(),
        conflict_policy: macsftp_core::ConflictPolicy::default(),
        warnings: Vec::new(),
        created_at: Timestamp::from_secs_since_epoch(1),
    }
}

fn client_key_auth(server: &SshTestServer) -> AuthCredential {
    AuthCredential::PrivateKey {
        key_path: server.client_key_path.display().to_string(),
        passphrase: None,
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn unknown_host_key_accept_connects_and_persists_trust() {
    let Some(server) = SshTestServer::spawn() else {
        return; // skip message already printed
    };
    let fixture = spawn_actor(&server, client_key_auth(&server), None);

    // 1. The unknown host key produces a prompt with real key data.
    let event = next_event(&fixture, "HostKeyUnknown").await;
    let prompt = match event {
        AppEvent::HostKeyUnknown(prompt) => prompt,
        other => panic!("expected HostKeyUnknown, got {other:?}"),
    };
    assert_eq!(prompt.tab_id, TAB);
    assert_eq!(prompt.session_epoch, EPOCH);
    assert_eq!(prompt.algorithm, "ssh-ed25519");
    assert!(prompt.fingerprint_sha256.starts_with("SHA256:"));

    // 2. Trusting resumes the handshake through auth and SFTP setup.
    assert!(
        fixture
            .trust_registry
            .resolve(prompt.request_id, TrustDecision::TrustAndSave),
        "trust request must be resolvable"
    );
    let event = next_event(&fixture, "TabConnected").await;
    match event {
        AppEvent::TabConnected(scoped) => {
            assert_eq!(scoped.scope.tab_id, TAB);
            assert_eq!(scoped.scope.session_epoch, EPOCH);
            assert!(
                scoped.payload.remote_root.as_str().starts_with('/'),
                "remote root should be an absolute path, got {:?}",
                scoped.payload.remote_root
            );
        }
        other => panic!("expected TabConnected, got {other:?}"),
    }

    // 3. The decision was persisted to the app-owned known_hosts file.
    let store = KnownHostsStore::load(&fixture.app_known_hosts_path, None);
    assert_eq!(store.entry_count(), 1, "trusted key must be persisted");

    fixture.cancel.cancel();
}

#[tokio::test(flavor = "multi_thread")]
async fn known_host_connects_without_prompt() {
    let Some(server) = SshTestServer::spawn() else {
        return;
    };
    let fixture = spawn_actor(
        &server,
        client_key_auth(&server),
        Some(&server.host_public_key),
    );

    // No HostKeyUnknown — the first event is TabConnected.
    let event = next_event(&fixture, "TabConnected").await;
    assert!(
        matches!(event, AppEvent::TabConnected(_)),
        "known host must connect without prompt, got {event:?}"
    );

    fixture.cancel.cancel();
}

#[tokio::test(flavor = "multi_thread")]
async fn read_dir_returns_real_remote_entries() {
    let Some(server) = SshTestServer::spawn() else {
        return;
    };
    let directory = server.fixture_dir.join("browse-fixture");
    std::fs::create_dir(&directory).expect("create fixture directory");
    std::fs::write(directory.join("report.txt"), b"hello").expect("write fixture file");
    std::fs::create_dir(directory.join("nested")).expect("create fixture child directory");

    let fixture = spawn_actor(
        &server,
        client_key_auth(&server),
        Some(&server.host_public_key),
    );
    let _ = next_event(&fixture, "TabConnected").await;

    let path = RemotePath::new(directory.display().to_string());
    fixture
        .requests
        .send_async(RemoteSessionRequest::ReadDir { path: path.clone() })
        .await
        .expect("directory request should reach actor");

    let loading = next_event(&fixture, "RemoteDirLoading").await;
    assert!(matches!(
        loading,
        AppEvent::RemoteDirLoading(scoped) if scoped.payload.path == path
    ));

    let loaded = next_event(&fixture, "RemoteDirLoaded").await;
    let snapshot = match loaded {
        AppEvent::RemoteDirLoaded(scoped) => {
            assert_eq!(scoped.scope.tab_id, TAB);
            assert_eq!(scoped.scope.session_id, SESSION);
            assert_eq!(scoped.scope.session_epoch, EPOCH);
            assert_eq!(scoped.payload.path, path);
            scoped.payload
        }
        other => panic!("expected RemoteDirLoaded, got {other:?}"),
    };

    let file = snapshot
        .entries
        .iter()
        .find(|entry| entry.name == "report.txt")
        .expect("listing should include report.txt");
    assert_eq!(file.kind, FileKind::File);
    assert_eq!(file.size, Some(5));
    assert_eq!(
        file.path,
        RemotePath::new(directory.join("report.txt").display().to_string())
    );
    assert!(
        snapshot
            .entries
            .iter()
            .any(|entry| entry.name == "nested" && entry.kind == FileKind::Directory),
        "listing should preserve directory types"
    );

    fixture.cancel.cancel();
}

#[tokio::test(flavor = "multi_thread")]
async fn read_dir_handles_ten_thousand_entries() {
    let Some(server) = SshTestServer::spawn() else {
        return;
    };
    let directory = server.fixture_dir.join("ten-thousand-entries");
    std::fs::create_dir(&directory).expect("create fixture directory");
    for index in 0..10_000 {
        std::fs::File::create(directory.join(format!("entry-{index:05}")))
            .expect("create fixture entry");
    }

    let fixture = spawn_actor(
        &server,
        client_key_auth(&server),
        Some(&server.host_public_key),
    );
    let _ = next_event(&fixture, "TabConnected").await;
    let path = RemotePath::new(directory.display().to_string());
    fixture
        .requests
        .send_async(RemoteSessionRequest::ReadDir { path: path.clone() })
        .await
        .expect("directory request should reach actor");
    let _ = next_event(&fixture, "RemoteDirLoading").await;

    let loaded = next_event(&fixture, "RemoteDirLoaded").await;
    match loaded {
        AppEvent::RemoteDirLoaded(scoped) => {
            assert_eq!(scoped.payload.path, path);
            assert_eq!(scoped.payload.entries.len(), 10_000);
        }
        other => panic!("expected RemoteDirLoaded, got {other:?}"),
    }

    fixture.cancel.cancel();
}

#[tokio::test(flavor = "multi_thread")]
async fn single_file_upload_and_download_use_independent_sftp_channels() {
    let Some(server) = SshTestServer::spawn() else {
        return;
    };
    let fixture = spawn_actor(
        &server,
        client_key_auth(&server),
        Some(&server.host_public_key),
    );
    let _ = next_event(&fixture, "TabConnected").await;

    let upload_source = server.fixture_dir.join("upload-source.txt");
    let upload_destination = server.fixture_dir.join("uploaded.txt");
    std::fs::write(&upload_source, b"uploaded through sftp").expect("write upload source");
    let upload = transfer_job(
        101,
        TransferDirection::Upload,
        TransferEndpoint::Local(macsftp_core::LocalPath::new(
            upload_source.display().to_string(),
        )),
        TransferEndpoint::Remote(RemotePath::new(upload_destination.display().to_string())),
    );
    fixture
        .requests
        .send_async(RemoteSessionRequest::RunTransferJobs {
            plan_id: macsftp_core::TransferPlanId(1),
            jobs: vec![upload],
        })
        .await
        .expect("upload request should reach actor");
    wait_for_transfer_completion(&fixture, TransferId(101)).await;
    assert_eq!(
        std::fs::read(&upload_destination).expect("read uploaded file"),
        b"uploaded through sftp"
    );
    assert!(
        !upload_destination
            .parent()
            .expect("upload destination has parent")
            .join(".uploaded.txt.macsftp-part-101")
            .exists(),
        "successful upload must remove its remote temporary file"
    );

    let download_source = server.fixture_dir.join("download-source.txt");
    let download_destination = server.fixture_dir.join("downloaded.txt");
    std::fs::write(&download_source, b"downloaded through sftp").expect("write download source");
    let download = transfer_job(
        102,
        TransferDirection::Download,
        TransferEndpoint::Remote(RemotePath::new(download_source.display().to_string())),
        TransferEndpoint::Local(macsftp_core::LocalPath::new(
            download_destination.display().to_string(),
        )),
    );
    fixture
        .requests
        .send_async(RemoteSessionRequest::RunTransferJobs {
            plan_id: macsftp_core::TransferPlanId(2),
            jobs: vec![download],
        })
        .await
        .expect("download request should reach actor");
    wait_for_transfer_completion(&fixture, TransferId(102)).await;
    assert_eq!(
        std::fs::read(&download_destination).expect("read downloaded file"),
        b"downloaded through sftp"
    );
    assert!(
        !download_destination
            .parent()
            .expect("download destination has parent")
            .join(".downloaded.txt.macsftp-part-102")
            .exists(),
        "successful download must remove its local temporary file"
    );

    fixture.cancel.cancel();
}

#[tokio::test(flavor = "multi_thread")]
async fn overwrite_uses_temporary_files_without_leaving_residue() {
    let Some(server) = SshTestServer::spawn() else {
        return;
    };
    let fixture = spawn_actor(
        &server,
        client_key_auth(&server),
        Some(&server.host_public_key),
    );
    let _ = next_event(&fixture, "TabConnected").await;

    let upload_source = server.fixture_dir.join("overwrite-upload-source.txt");
    let upload_destination = server.fixture_dir.join("overwrite-upload-destination.txt");
    std::fs::write(&upload_source, b"new remote content").expect("write upload source");
    std::fs::write(&upload_destination, b"old remote content").expect("write upload destination");
    let mut upload = transfer_job(
        103,
        TransferDirection::Upload,
        TransferEndpoint::Local(macsftp_core::LocalPath::new(
            upload_source.display().to_string(),
        )),
        TransferEndpoint::Remote(RemotePath::new(upload_destination.display().to_string())),
    );
    upload.conflict_policy = macsftp_core::ConflictPolicy::OverwriteAll;
    fixture
        .requests
        .send_async(RemoteSessionRequest::RunTransferJobs {
            plan_id: macsftp_core::TransferPlanId(8),
            jobs: vec![upload],
        })
        .await
        .expect("overwrite upload should reach actor");
    wait_for_transfer_completion(&fixture, TransferId(103)).await;
    assert_eq!(
        std::fs::read(&upload_destination).expect("read overwritten remote file"),
        b"new remote content"
    );
    assert!(
        !server
            .fixture_dir
            .join(".overwrite-upload-destination.txt.macsftp-part-103")
            .exists(),
        "overwrite upload must clean temporary file"
    );

    let download_source = server.fixture_dir.join("overwrite-download-source.txt");
    let download_destination = server
        .fixture_dir
        .join("overwrite-download-destination.txt");
    std::fs::write(&download_source, b"new local content").expect("write download source");
    std::fs::write(&download_destination, b"old local content")
        .expect("write download destination");
    let mut download = transfer_job(
        104,
        TransferDirection::Download,
        TransferEndpoint::Remote(RemotePath::new(download_source.display().to_string())),
        TransferEndpoint::Local(macsftp_core::LocalPath::new(
            download_destination.display().to_string(),
        )),
    );
    download.conflict_policy = macsftp_core::ConflictPolicy::OverwriteAll;
    fixture
        .requests
        .send_async(RemoteSessionRequest::RunTransferJobs {
            plan_id: macsftp_core::TransferPlanId(8),
            jobs: vec![download],
        })
        .await
        .expect("overwrite download should reach actor");
    wait_for_transfer_completion(&fixture, TransferId(104)).await;
    assert_eq!(
        std::fs::read(&download_destination).expect("read overwritten local file"),
        b"new local content"
    );
    assert!(
        !server
            .fixture_dir
            .join(".overwrite-download-destination.txt.macsftp-part-104")
            .exists(),
        "overwrite download must clean temporary file"
    );

    fixture.cancel.cancel();
}

#[tokio::test(flavor = "multi_thread")]
async fn conflict_decision_pauses_job_and_applies_skip_to_later_plan_jobs() {
    let Some(server) = SshTestServer::spawn() else {
        return;
    };
    let fixture = spawn_actor(
        &server,
        client_key_auth(&server),
        Some(&server.host_public_key),
    );
    let _ = next_event(&fixture, "TabConnected").await;
    let source = server.fixture_dir.join("conflict-source.txt");
    let first_destination = server.fixture_dir.join("conflict-first.txt");
    let second_destination = server.fixture_dir.join("conflict-second.txt");
    std::fs::write(&source, b"new data").expect("write source");
    std::fs::write(&first_destination, b"old data").expect("write first destination");
    std::fs::write(&second_destination, b"old data").expect("write second destination");

    let first = transfer_job(
        71,
        TransferDirection::Upload,
        TransferEndpoint::Local(macsftp_core::LocalPath::new(source.display().to_string())),
        TransferEndpoint::Remote(RemotePath::new(first_destination.display().to_string())),
    );
    fixture
        .requests
        .send_async(RemoteSessionRequest::RunTransferJobs {
            plan_id: macsftp_core::TransferPlanId(7),
            jobs: vec![first],
        })
        .await
        .expect("first transfer request should reach actor");
    let conflict = next_event(&fixture, "first conflict prompt").await;
    let request_id = match conflict {
        AppEvent::TransferConflict(prompt) => {
            assert_eq!(prompt.plan_id, macsftp_core::TransferPlanId(7));
            assert_eq!(prompt.transfer_id, TransferId(71));
            assert_eq!(prompt.source_size, Some(8));
            assert!(
                prompt.source_modified_at.is_some(),
                "conflict prompt should show the source mtime"
            );
            prompt.request_id
        }
        other => panic!("expected transfer conflict, got {other:?}"),
    };
    fixture
        .requests
        .send_async(RemoteSessionRequest::ResolveTransferConflict {
            request_id,
            decision: ConflictDecision::Skip { apply_to_all: true },
        })
        .await
        .expect("conflict decision should reach actor");
    assert!(matches!(
        next_event(&fixture, "first skipped").await,
        AppEvent::TransferSkipped {
            transfer_id: TransferId(71)
        }
    ));

    let second = transfer_job(
        72,
        TransferDirection::Upload,
        TransferEndpoint::Local(macsftp_core::LocalPath::new(source.display().to_string())),
        TransferEndpoint::Remote(RemotePath::new(second_destination.display().to_string())),
    );
    fixture
        .requests
        .send_async(RemoteSessionRequest::RunTransferJobs {
            plan_id: macsftp_core::TransferPlanId(7),
            jobs: vec![second],
        })
        .await
        .expect("second transfer request should reach actor");
    assert!(matches!(
        next_event(&fixture, "second skipped by plan policy").await,
        AppEvent::TransferSkipped {
            transfer_id: TransferId(72)
        }
    ));
    assert_eq!(
        std::fs::read(&first_destination).expect("read first destination"),
        b"old data"
    );
    assert_eq!(
        std::fs::read(&second_destination).expect("read second destination"),
        b"old data"
    );

    fixture.cancel.cancel();
}

#[tokio::test(flavor = "multi_thread")]
async fn skip_all_resolves_other_conflicts_already_waiting_in_the_same_plan() {
    let Some(server) = SshTestServer::spawn() else {
        return;
    };
    let fixture = spawn_actor(
        &server,
        client_key_auth(&server),
        Some(&server.host_public_key),
    );
    let _ = next_event(&fixture, "TabConnected").await;
    let source = server.fixture_dir.join("skip-all-source.txt");
    std::fs::write(&source, b"new data").expect("write source");

    let mut jobs = Vec::new();
    for index in 0..4 {
        let destination = server.fixture_dir.join(format!("skip-all-{index}.txt"));
        std::fs::write(&destination, b"old data").expect("write destination");
        jobs.push(transfer_job(
            90 + index,
            TransferDirection::Upload,
            TransferEndpoint::Local(macsftp_core::LocalPath::new(source.display().to_string())),
            TransferEndpoint::Remote(RemotePath::new(destination.display().to_string())),
        ));
    }
    fixture
        .requests
        .send_async(RemoteSessionRequest::RunTransferJobs {
            plan_id: macsftp_core::TransferPlanId(9),
            jobs,
        })
        .await
        .expect("transfer requests should reach actor");

    let mut first_request_id = None;
    for _ in 0..4 {
        match next_event(&fixture, "conflict prompt").await {
            AppEvent::TransferConflict(prompt) => {
                first_request_id.get_or_insert(prompt.request_id);
            }
            other => panic!("expected transfer conflict, got {other:?}"),
        }
    }
    fixture
        .requests
        .send_async(RemoteSessionRequest::ResolveTransferConflict {
            request_id: first_request_id.expect("at least one conflict prompt"),
            decision: ConflictDecision::Skip { apply_to_all: true },
        })
        .await
        .expect("skip all should reach actor");

    let mut skipped = Vec::new();
    while skipped.len() < 4 {
        match next_event(&fixture, "skipped transfer").await {
            AppEvent::TransferSkipped { transfer_id } => skipped.push(transfer_id),
            other => panic!("skip all should not require another decision, got {other:?}"),
        }
    }
    skipped.sort_by_key(|transfer_id| transfer_id.0);
    assert_eq!(
        skipped,
        vec![
            TransferId(90),
            TransferId(91),
            TransferId(92),
            TransferId(93)
        ]
    );

    fixture.cancel.cancel();
}

#[cfg(unix)]
#[tokio::test(flavor = "multi_thread")]
async fn upload_preserves_requested_permissions_and_mtime() {
    use std::os::unix::fs::PermissionsExt;

    let Some(server) = SshTestServer::spawn() else {
        return;
    };
    let fixture = spawn_actor(
        &server,
        client_key_auth(&server),
        Some(&server.host_public_key),
    );
    let _ = next_event(&fixture, "TabConnected").await;
    let source = server.fixture_dir.join("metadata-source.txt");
    let destination = server.fixture_dir.join("metadata-destination.txt");
    std::fs::write(&source, b"metadata").expect("write source");
    std::fs::set_permissions(&source, std::fs::Permissions::from_mode(0o640))
        .expect("set source permissions");
    let expected_mtime = std::time::UNIX_EPOCH + Duration::from_secs(1_700_000_000);
    std::fs::File::open(&source)
        .expect("open source for timestamps")
        .set_times(std::fs::FileTimes::new().set_modified(expected_mtime))
        .expect("set source mtime");

    let job = transfer_job(
        81,
        TransferDirection::Upload,
        TransferEndpoint::Local(macsftp_core::LocalPath::new(source.display().to_string())),
        TransferEndpoint::Remote(RemotePath::new(destination.display().to_string())),
    );
    fixture
        .requests
        .send_async(RemoteSessionRequest::RunTransferJobs {
            plan_id: macsftp_core::TransferPlanId(8),
            jobs: vec![job],
        })
        .await
        .expect("metadata transfer request should reach actor");
    wait_for_transfer_completion(&fixture, TransferId(81)).await;

    let destination_metadata = std::fs::metadata(&destination).expect("read destination metadata");
    assert_eq!(destination_metadata.permissions().mode() & 0o777, 0o640);
    let destination_mtime = destination_metadata
        .modified()
        .expect("read destination mtime")
        .duration_since(std::time::UNIX_EPOCH)
        .expect("destination mtime after epoch")
        .as_secs();
    assert_eq!(destination_mtime, 1_700_000_000);

    fixture.cancel.cancel();
}

#[cfg(unix)]
#[tokio::test(flavor = "multi_thread")]
async fn single_file_symlinks_are_copied_without_dereferencing() {
    use std::os::unix::fs::symlink;

    let Some(server) = SshTestServer::spawn() else {
        return;
    };
    let fixture = spawn_actor(
        &server,
        client_key_auth(&server),
        Some(&server.host_public_key),
    );
    let _ = next_event(&fixture, "TabConnected").await;

    let local_target = server.fixture_dir.join("symlink-upload-target.txt");
    let local_link = server.fixture_dir.join("symlink-upload-source");
    let remote_link = server.fixture_dir.join("symlink-upload-destination");
    std::fs::write(&local_target, b"upload target").expect("write upload target");
    symlink("symlink-upload-target.txt", &local_link).expect("create upload source symlink");
    fixture
        .requests
        .send_async(RemoteSessionRequest::RunTransferJobs {
            plan_id: macsftp_core::TransferPlanId(10),
            jobs: vec![transfer_job(
                101,
                TransferDirection::Upload,
                TransferEndpoint::Local(macsftp_core::LocalPath::new(
                    local_link.display().to_string(),
                )),
                TransferEndpoint::Remote(RemotePath::new(remote_link.display().to_string())),
            )],
        })
        .await
        .expect("symlink upload should reach actor");
    wait_for_transfer_completion(&fixture, TransferId(101)).await;
    assert_eq!(
        std::fs::read_link(&remote_link).expect("read uploaded symlink"),
        std::path::PathBuf::from("symlink-upload-target.txt")
    );

    let remote_target = server.fixture_dir.join("symlink-download-target.txt");
    let remote_source = server.fixture_dir.join("symlink-download-source");
    let local_destination = server.fixture_dir.join("symlink-download-destination");
    std::fs::write(&remote_target, b"download target").expect("write download target");
    symlink("symlink-download-target.txt", &remote_source).expect("create remote source symlink");
    fixture
        .requests
        .send_async(RemoteSessionRequest::RunTransferJobs {
            plan_id: macsftp_core::TransferPlanId(11),
            jobs: vec![transfer_job(
                102,
                TransferDirection::Download,
                TransferEndpoint::Remote(RemotePath::new(remote_source.display().to_string())),
                TransferEndpoint::Local(macsftp_core::LocalPath::new(
                    local_destination.display().to_string(),
                )),
            )],
        })
        .await
        .expect("symlink download should reach actor");
    wait_for_transfer_completion(&fixture, TransferId(102)).await;
    assert_eq!(
        std::fs::read_link(&local_destination).expect("read downloaded symlink"),
        std::path::PathBuf::from("symlink-download-target.txt")
    );

    fixture.cancel.cancel();
}

#[tokio::test(flavor = "multi_thread")]
async fn runtime_plans_and_executes_single_file_upload_and_download() {
    let Some(server) = SshTestServer::spawn() else {
        return;
    };
    let known_hosts_path = server.fixture_dir.join("transfer_runtime_known_hosts");
    std::fs::write(
        &known_hosts_path,
        format!("[127.0.0.1]:{} {}\n", server.port, server.host_public_key),
    )
    .expect("prefill known_hosts");
    let mut controller = RuntimeController::start(
        RuntimeBridgeConfig::default(),
        SessionBackend::Real(HostTrustConfig::new(known_hosts_path, None)),
    );
    let client = controller.client();
    let mut events = controller
        .take_event_receiver()
        .expect("event receiver should be available once");

    client
        .try_send(AppCommand::ConnectTab(ConnectCommand {
            tab_id: TAB,
            session_id: SESSION,
            session_epoch: EPOCH,
            profile_id: ProfileId(1),
            pool_identity: ConnectionPoolIdentity::Ephemeral(SESSION),
            settings: ConnectionSettings {
                host: "127.0.0.1".to_string(),
                port: server.port,
                username: server.username.clone(),
                auth: client_key_auth(&server),
            },
        }))
        .expect("connect command should send");
    let _ = next_runtime_event(&mut events, "TabConnecting").await;
    let _ = next_runtime_event(&mut events, "TabConnected").await;

    let upload_source = server.fixture_dir.join("runtime-upload-source.txt");
    let upload_destination = server.fixture_dir.join("runtime-uploaded.txt");
    std::fs::write(&upload_source, b"runtime upload").expect("write upload source");
    client
        .try_send(AppCommand::StartTransfer(
            macsftp_core::StartTransferCommand {
                tab_id: TAB,
                session_epoch: EPOCH,
                profile_id: ProfileId(1),
                direction: TransferDirection::Upload,
                sources: vec![TransferEndpoint::Local(macsftp_core::LocalPath::new(
                    upload_source.display().to_string(),
                ))],
                destination: TransferEndpoint::Remote(RemotePath::new(
                    upload_destination.display().to_string(),
                )),
                metadata_policy: macsftp_core::MetadataPolicy::default(),
                conflict_policy: macsftp_core::ConflictPolicy::default(),
            },
        ))
        .expect("upload command should send");
    let _ = wait_for_runtime_transfer_completion(&mut events).await;
    assert_eq!(
        std::fs::read(&upload_destination).expect("read uploaded file"),
        b"runtime upload"
    );

    let download_source = server.fixture_dir.join("runtime-download-source.txt");
    let download_destination = server.fixture_dir.join("runtime-downloaded.txt");
    std::fs::write(&download_source, b"runtime download").expect("write download source");
    client
        .try_send(AppCommand::StartTransfer(
            macsftp_core::StartTransferCommand {
                tab_id: TAB,
                session_epoch: EPOCH,
                profile_id: ProfileId(1),
                direction: TransferDirection::Download,
                sources: vec![TransferEndpoint::Remote(RemotePath::new(
                    download_source.display().to_string(),
                ))],
                destination: TransferEndpoint::Local(macsftp_core::LocalPath::new(
                    download_destination.display().to_string(),
                )),
                metadata_policy: macsftp_core::MetadataPolicy::default(),
                conflict_policy: macsftp_core::ConflictPolicy::default(),
            },
        ))
        .expect("download command should send");
    let _ = wait_for_runtime_transfer_completion(&mut events).await;
    assert_eq!(
        std::fs::read(&download_destination).expect("read downloaded file"),
        b"runtime download"
    );

    std::thread::spawn(move || controller.shutdown())
        .join()
        .expect("runtime shutdown thread should finish");
}

#[tokio::test(flavor = "multi_thread")]
async fn runtime_executes_directory_upload_with_a_bounded_session_queue() {
    let Some(server) = SshTestServer::spawn() else {
        return;
    };
    let source_root = server.fixture_dir.join("directory-upload-source");
    let nested_source = source_root.join("nested");
    std::fs::create_dir(&source_root).expect("create upload source");
    std::fs::create_dir(&nested_source).expect("create nested upload source");
    std::fs::write(source_root.join("first.txt"), b"first").expect("write first upload source");
    std::fs::write(nested_source.join("second.txt"), b"second")
        .expect("write second upload source");
    let destination_root = server.fixture_dir.join("directory-upload-destination");
    std::fs::create_dir(&destination_root).expect("create upload destination");

    let known_hosts_path = server.fixture_dir.join("directory_upload_known_hosts");
    std::fs::write(
        &known_hosts_path,
        format!("[127.0.0.1]:{} {}\n", server.port, server.host_public_key),
    )
    .expect("prefill known_hosts");
    let mut controller = RuntimeController::start(
        RuntimeBridgeConfig::default(),
        SessionBackend::Real(HostTrustConfig::new(known_hosts_path, None)),
    );
    let client = controller.client();
    let mut events = controller
        .take_event_receiver()
        .expect("event receiver should be available once");

    client
        .try_send(AppCommand::ConnectTab(ConnectCommand {
            tab_id: TAB,
            session_id: SESSION,
            session_epoch: EPOCH,
            profile_id: ProfileId(1),
            pool_identity: ConnectionPoolIdentity::Ephemeral(SESSION),
            settings: ConnectionSettings {
                host: "127.0.0.1".to_string(),
                port: server.port,
                username: server.username.clone(),
                auth: client_key_auth(&server),
            },
        }))
        .expect("connect command should send");
    let _ = next_runtime_event(&mut events, "TabConnecting").await;
    let _ = next_runtime_event(&mut events, "TabConnected").await;

    client
        .try_send(AppCommand::StartTransfer(
            macsftp_core::StartTransferCommand {
                tab_id: TAB,
                session_epoch: EPOCH,
                profile_id: ProfileId(1),
                direction: TransferDirection::Upload,
                sources: vec![TransferEndpoint::Local(macsftp_core::LocalPath::new(
                    source_root.display().to_string(),
                ))],
                destination: TransferEndpoint::Remote(RemotePath::new(
                    destination_root.display().to_string(),
                )),
                metadata_policy: macsftp_core::MetadataPolicy::default(),
                conflict_policy: macsftp_core::ConflictPolicy::default(),
            },
        ))
        .expect("directory upload command should send");

    let mut planned_jobs = 0;
    let mut completed_jobs = 0;
    while completed_jobs < 3 {
        match next_runtime_event(&mut events, "directory upload event").await {
            AppEvent::TransferPlanStarted(_) | AppEvent::TransferPlanCompleted { .. } => {}
            AppEvent::TransferPlanProgress(progress) => {
                planned_jobs += progress.child_jobs.len();
            }
            AppEvent::TransferRunning(_) | AppEvent::TransferProgress(_) => {}
            AppEvent::TransferCompleted { .. } => completed_jobs += 1,
            AppEvent::TransferFailed(failure) => {
                panic!("directory upload failed: {:?}", failure.error)
            }
            AppEvent::ResidualTempCreated(_) | AppEvent::ResidualTempCleared { .. } => {}
            other => panic!("unexpected directory upload event: {other:?}"),
        }
    }
    assert_eq!(
        planned_jobs, 3,
        "directory planner must produce all child jobs"
    );
    assert_eq!(
        std::fs::read(destination_root.join("first.txt")).expect("read first result"),
        b"first"
    );
    assert_eq!(
        std::fs::read(destination_root.join("nested/second.txt")).expect("read nested result"),
        b"second"
    );

    std::thread::spawn(move || controller.shutdown())
        .join()
        .expect("runtime shutdown thread should finish");
}

#[tokio::test(flavor = "multi_thread")]
async fn runtime_streams_and_executes_directory_download() {
    let Some(server) = SshTestServer::spawn() else {
        return;
    };
    let source_root = server.fixture_dir.join("directory-download-source");
    let nested_source = source_root.join("nested");
    std::fs::create_dir(&source_root).expect("create download source");
    std::fs::create_dir(&nested_source).expect("create nested download source");
    std::fs::write(source_root.join("first.txt"), b"first").expect("write first download source");
    std::fs::write(nested_source.join("second.txt"), b"second")
        .expect("write second download source");
    let destination_root = server.fixture_dir.join("directory-download-destination");

    let known_hosts_path = server.fixture_dir.join("directory_download_known_hosts");
    std::fs::write(
        &known_hosts_path,
        format!("[127.0.0.1]:{} {}\n", server.port, server.host_public_key),
    )
    .expect("prefill known_hosts");
    let mut controller = RuntimeController::start(
        RuntimeBridgeConfig::default(),
        SessionBackend::Real(HostTrustConfig::new(known_hosts_path, None)),
    );
    let client = controller.client();
    let mut events = controller
        .take_event_receiver()
        .expect("event receiver should be available once");

    client
        .try_send(AppCommand::ConnectTab(ConnectCommand {
            tab_id: TAB,
            session_id: SESSION,
            session_epoch: EPOCH,
            profile_id: ProfileId(1),
            pool_identity: ConnectionPoolIdentity::Ephemeral(SESSION),
            settings: ConnectionSettings {
                host: "127.0.0.1".to_string(),
                port: server.port,
                username: server.username.clone(),
                auth: client_key_auth(&server),
            },
        }))
        .expect("connect command should send");
    let _ = next_runtime_event(&mut events, "TabConnecting").await;
    let _ = next_runtime_event(&mut events, "TabConnected").await;

    client
        .try_send(AppCommand::StartTransfer(
            macsftp_core::StartTransferCommand {
                tab_id: TAB,
                session_epoch: EPOCH,
                profile_id: ProfileId(1),
                direction: TransferDirection::Download,
                sources: vec![TransferEndpoint::Remote(RemotePath::new(
                    source_root.display().to_string(),
                ))],
                destination: TransferEndpoint::Local(macsftp_core::LocalPath::new(
                    destination_root.display().to_string(),
                )),
                metadata_policy: macsftp_core::MetadataPolicy::default(),
                conflict_policy: macsftp_core::ConflictPolicy::default(),
            },
        ))
        .expect("directory download command should send");

    let mut planned_jobs = 0;
    let mut completed_jobs = 0;
    while completed_jobs < 4 {
        match next_runtime_event(&mut events, "directory download event").await {
            AppEvent::TransferPlanStarted(_) | AppEvent::TransferPlanCompleted { .. } => {}
            AppEvent::TransferPlanProgress(progress) => {
                planned_jobs += progress.child_jobs.len();
            }
            AppEvent::TransferRunning(_) | AppEvent::TransferProgress(_) => {}
            AppEvent::TransferCompleted { .. } => completed_jobs += 1,
            AppEvent::TransferFailed(failure) => {
                panic!("directory download failed: {:?}", failure.error)
            }
            AppEvent::ResidualTempCreated(_) | AppEvent::ResidualTempCleared { .. } => {}
            other => panic!("unexpected directory download event: {other:?}"),
        }
    }
    assert_eq!(
        planned_jobs, 4,
        "directory planner must emit root, nested directory, and files"
    );
    assert_eq!(
        std::fs::read(destination_root.join("first.txt")).expect("read first result"),
        b"first"
    );
    assert_eq!(
        std::fs::read(destination_root.join("nested/second.txt")).expect("read nested result"),
        b"second"
    );

    std::thread::spawn(move || controller.shutdown())
        .join()
        .expect("runtime shutdown thread should finish");
}

#[tokio::test(flavor = "multi_thread")]
async fn missing_directory_reports_not_found_error() {
    let Some(server) = SshTestServer::spawn() else {
        return;
    };
    let fixture = spawn_actor(
        &server,
        client_key_auth(&server),
        Some(&server.host_public_key),
    );
    let _ = next_event(&fixture, "TabConnected").await;

    let missing_path = RemotePath::new(
        server
            .fixture_dir
            .join("directory-that-does-not-exist")
            .display()
            .to_string(),
    );
    fixture
        .requests
        .send_async(RemoteSessionRequest::ReadDir {
            path: missing_path.clone(),
        })
        .await
        .expect("directory request should reach actor");
    let _ = next_event(&fixture, "RemoteDirLoading").await;

    let failure = next_event(&fixture, "RemoteOperationFailed").await;
    match failure {
        AppEvent::RemoteOperationFailed(scoped) => {
            assert_eq!(scoped.scope.tab_id, TAB);
            assert_eq!(scoped.payload.path, Some(missing_path));
            assert_eq!(scoped.payload.error.code, ErrorCode::NotFound);
            assert!(scoped.payload.error.retryable);
        }
        other => panic!("expected RemoteOperationFailed, got {other:?}"),
    }

    fixture.cancel.cancel();
}

#[tokio::test(flavor = "multi_thread")]
async fn runtime_routes_read_dir_to_real_actor() {
    let Some(server) = SshTestServer::spawn() else {
        return;
    };
    let directory = server.fixture_dir.join("runtime-browse-fixture");
    std::fs::create_dir(&directory).expect("create fixture directory");
    std::fs::write(directory.join("routed.txt"), b"runtime").expect("write fixture file");

    let known_hosts_path = server.fixture_dir.join("runtime_app_known_hosts");
    std::fs::write(
        &known_hosts_path,
        format!("[127.0.0.1]:{} {}\n", server.port, server.host_public_key),
    )
    .expect("prefill known_hosts");
    let mut controller = RuntimeController::start(
        RuntimeBridgeConfig::default(),
        SessionBackend::Real(HostTrustConfig::new(known_hosts_path, None)),
    );
    let client = controller.client();
    let mut events = controller
        .take_event_receiver()
        .expect("event receiver should be available once");

    client
        .try_send(AppCommand::ConnectTab(ConnectCommand {
            tab_id: TAB,
            session_id: SESSION,
            session_epoch: EPOCH,
            profile_id: ProfileId(1),
            pool_identity: ConnectionPoolIdentity::Ephemeral(SESSION),
            settings: ConnectionSettings {
                host: "127.0.0.1".to_string(),
                port: server.port,
                username: server.username.clone(),
                auth: client_key_auth(&server),
            },
        }))
        .expect("connect command should send");
    assert!(matches!(
        next_runtime_event(&mut events, "TabConnecting").await,
        AppEvent::TabConnecting { tab_id: TAB }
    ));
    assert!(matches!(
        next_runtime_event(&mut events, "TabConnected").await,
        AppEvent::TabConnected(_)
    ));

    let path = RemotePath::new(directory.display().to_string());
    client
        .try_send(AppCommand::ReadRemoteDir {
            tab_id: TAB,
            path: path.clone(),
        })
        .expect("read directory command should send");
    assert!(matches!(
        next_runtime_event(&mut events, "RemoteDirLoading").await,
        AppEvent::RemoteDirLoading(scoped) if scoped.payload.path == path
    ));
    let loaded = next_runtime_event(&mut events, "RemoteDirLoaded").await;
    assert!(matches!(
        loaded,
        AppEvent::RemoteDirLoaded(scoped)
            if scoped.payload.path == path
                && scoped.payload.entries.iter().any(|entry| entry.name == "routed.txt")
    ));

    std::thread::spawn(move || controller.shutdown())
        .join()
        .expect("runtime shutdown thread should finish");
}

#[tokio::test(flavor = "multi_thread")]
async fn tabs_browse_independently_after_another_tab_disconnects() {
    let Some(server) = SshTestServer::spawn() else {
        return;
    };
    let first_directory = server.fixture_dir.join("first-tab-directory");
    let second_directory = server.fixture_dir.join("second-tab-directory");
    std::fs::create_dir(&first_directory).expect("create first fixture directory");
    std::fs::create_dir(&second_directory).expect("create second fixture directory");
    std::fs::write(first_directory.join("first.txt"), b"first").expect("write first fixture");
    std::fs::write(second_directory.join("second.txt"), b"second").expect("write second fixture");

    let known_hosts_path = server.fixture_dir.join("multi_tab_app_known_hosts");
    std::fs::write(
        &known_hosts_path,
        format!("[127.0.0.1]:{} {}\n", server.port, server.host_public_key),
    )
    .expect("prefill known_hosts");
    let mut controller = RuntimeController::start(
        RuntimeBridgeConfig::default(),
        SessionBackend::Real(HostTrustConfig::new(known_hosts_path, None)),
    );
    let client = controller.client();
    let mut events = controller
        .take_event_receiver()
        .expect("event receiver should be available once");

    for (tab_id, session_id) in [(TAB, SESSION), (SECOND_TAB, SECOND_SESSION)] {
        client
            .try_send(AppCommand::ConnectTab(ConnectCommand {
                tab_id,
                session_id,
                session_epoch: EPOCH,
                profile_id: ProfileId(tab_id.0),
                pool_identity: ConnectionPoolIdentity::Ephemeral(session_id),
                settings: ConnectionSettings {
                    host: "127.0.0.1".to_string(),
                    port: server.port,
                    username: server.username.clone(),
                    auth: client_key_auth(&server),
                },
            }))
            .expect("connect command should send");
        assert!(matches!(
            next_runtime_event(&mut events, "TabConnecting").await,
            AppEvent::TabConnecting { tab_id: event_tab_id } if event_tab_id == tab_id
        ));
        assert!(matches!(
            next_runtime_event(&mut events, "TabConnected").await,
            AppEvent::TabConnected(scoped)
                if scoped.scope.tab_id == tab_id && scoped.scope.session_id == session_id
        ));
    }

    let first_path = RemotePath::new(first_directory.display().to_string());
    client
        .try_send(AppCommand::ReadRemoteDir {
            tab_id: TAB,
            path: first_path.clone(),
        })
        .expect("first tab request should send");
    assert!(matches!(
        next_runtime_event(&mut events, "first tab loading").await,
        AppEvent::RemoteDirLoading(scoped)
            if scoped.scope.tab_id == TAB && scoped.payload.path == first_path
    ));
    assert!(matches!(
        next_runtime_event(&mut events, "first tab listing").await,
        AppEvent::RemoteDirLoaded(scoped)
            if scoped.scope.tab_id == TAB
                && scoped.payload.entries.iter().any(|entry| entry.name == "first.txt")
    ));

    let second_path = RemotePath::new(second_directory.display().to_string());
    client
        .try_send(AppCommand::ReadRemoteDir {
            tab_id: SECOND_TAB,
            path: second_path.clone(),
        })
        .expect("second tab request should send");
    assert!(matches!(
        next_runtime_event(&mut events, "second tab loading").await,
        AppEvent::RemoteDirLoading(scoped)
            if scoped.scope.tab_id == SECOND_TAB && scoped.payload.path == second_path
    ));
    assert!(matches!(
        next_runtime_event(&mut events, "second tab listing").await,
        AppEvent::RemoteDirLoaded(scoped)
            if scoped.scope.tab_id == SECOND_TAB
                && scoped.payload.entries.iter().any(|entry| entry.name == "second.txt")
    ));

    client
        .try_send(AppCommand::DisconnectTab { tab_id: TAB })
        .expect("first tab disconnect should send");
    client
        .try_send(AppCommand::ReadRemoteDir {
            tab_id: SECOND_TAB,
            path: second_path.clone(),
        })
        .expect("second tab refresh should send");
    assert!(matches!(
        next_runtime_event(&mut events, "second tab refresh loading").await,
        AppEvent::RemoteDirLoading(scoped)
            if scoped.scope.tab_id == SECOND_TAB && scoped.payload.path == second_path
    ));
    assert!(matches!(
        next_runtime_event(&mut events, "second tab refresh listing").await,
        AppEvent::RemoteDirLoaded(scoped)
            if scoped.scope.tab_id == SECOND_TAB
                && scoped.payload.entries.iter().any(|entry| entry.name == "second.txt")
    ));

    std::thread::spawn(move || controller.shutdown())
        .join()
        .expect("runtime shutdown thread should finish");
}

#[tokio::test(flavor = "multi_thread")]
async fn host_key_mismatch_blocks_connection() {
    let Some(server) = SshTestServer::spawn() else {
        return;
    };
    // Store a DIFFERENT valid key for this host: reuse the client
    // public key as the "expected" host key.
    let wrong_key = std::fs::read_to_string(server.client_key_path.with_extension("pub"))
        .expect("read client public key");
    let fixture = spawn_actor(&server, client_key_auth(&server), Some(wrong_key.trim()));

    let event = next_event(&fixture, "HostKeyMismatch").await;
    match event {
        AppEvent::HostKeyMismatch(mismatch) => {
            assert_eq!(mismatch.tab_id, TAB);
            assert!(mismatch.expected_fingerprint_sha256.is_some());
            assert!(
                mismatch.actual_fingerprint_sha256.starts_with("SHA256:"),
                "actual fingerprint must be present"
            );
            assert_ne!(
                mismatch.expected_fingerprint_sha256.as_deref(),
                Some(mismatch.actual_fingerprint_sha256.as_str()),
            );
        }
        other => panic!("expected HostKeyMismatch, got {other:?}"),
    }

    // The connection is blocked — no further event may arrive. The
    // actor exiting (channel disconnect) is fine; another event is not.
    let follow_up =
        tokio::time::timeout(Duration::from_millis(500), fixture.events.recv_async()).await;
    match follow_up {
        Err(_timeout) => {}
        Ok(Err(_channel_closed)) => {}
        Ok(Ok(event)) => panic!("mismatch must block without further events, got {event:?}"),
    }

    fixture.cancel.cancel();
}

#[tokio::test(flavor = "multi_thread")]
async fn rejecting_unknown_host_key_disconnects() {
    let Some(server) = SshTestServer::spawn() else {
        return;
    };
    let fixture = spawn_actor(&server, client_key_auth(&server), None);

    let event = next_event(&fixture, "HostKeyUnknown").await;
    let prompt = match event {
        AppEvent::HostKeyUnknown(prompt) => prompt,
        other => panic!("expected HostKeyUnknown, got {other:?}"),
    };
    fixture
        .trust_registry
        .resolve(prompt.request_id, TrustDecision::Reject);

    let event = next_event(&fixture, "TabDisconnected").await;
    match event {
        AppEvent::TabDisconnected(scoped) => {
            assert_eq!(scoped.payload.reason, DisconnectReason::UserRequested);
        }
        other => panic!("expected TabDisconnected, got {other:?}"),
    }

    // Nothing was persisted.
    let store = KnownHostsStore::load(&fixture.app_known_hosts_path, None);
    assert_eq!(store.entry_count(), 0);

    fixture.cancel.cancel();
}

#[tokio::test(flavor = "multi_thread")]
async fn password_auth_rejection_maps_to_auth_failed() {
    let Some(server) = SshTestServer::spawn() else {
        return;
    };
    // The fixture server disables password auth entirely, so this
    // exercises the AuthResult::Failure mapping.
    let fixture = spawn_actor(
        &server,
        AuthCredential::Password {
            password: "not-the-password".to_string(),
        },
        Some(&server.host_public_key),
    );

    let event = next_event(&fixture, "AuthFailed").await;
    match event {
        AppEvent::AuthFailed(scoped) => {
            assert_eq!(scoped.scope.tab_id, TAB);
            assert_eq!(scoped.payload.reason.code, ErrorCode::AuthFailed);
        }
        other => panic!("expected AuthFailed, got {other:?}"),
    }

    fixture.cancel.cancel();
}

#[tokio::test(flavor = "multi_thread")]
async fn encrypted_key_with_wrong_passphrase_fails_cleanly() {
    let Some(server) = SshTestServer::spawn() else {
        return;
    };
    let fixture = spawn_actor(
        &server,
        AuthCredential::PrivateKey {
            key_path: server.encrypted_key_path.display().to_string(),
            passphrase: Some("wrong-passphrase".to_string()),
        },
        Some(&server.host_public_key),
    );

    let event = next_event(&fixture, "AuthFailed").await;
    match event {
        AppEvent::AuthFailed(scoped) => {
            assert_eq!(scoped.payload.reason.code, ErrorCode::AuthFailed);
            // The key path must not leak into the user-facing message.
            let key_file_name = server
                .encrypted_key_path
                .file_name()
                .and_then(|name| name.to_str())
                .expect("key file name");
            assert!(
                !scoped.payload.reason.message.contains(key_file_name),
                "user-facing message must not contain the key path"
            );
        }
        other => panic!("expected AuthFailed, got {other:?}"),
    }

    fixture.cancel.cancel();
}

#[tokio::test(flavor = "multi_thread")]
async fn encrypted_key_without_passphrase_reports_encrypted() {
    let Some(server) = SshTestServer::spawn() else {
        return;
    };
    let fixture = spawn_actor(
        &server,
        AuthCredential::PrivateKey {
            key_path: server.encrypted_key_path.display().to_string(),
            passphrase: None,
        },
        Some(&server.host_public_key),
    );

    let event = next_event(&fixture, "AuthFailed").await;
    match event {
        AppEvent::AuthFailed(scoped) => {
            assert_eq!(scoped.payload.reason.code, ErrorCode::AuthFailed);
        }
        other => panic!("expected AuthFailed, got {other:?}"),
    }

    fixture.cancel.cancel();
}
