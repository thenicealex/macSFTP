//! Docker-backed password authentication release gate.

use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Duration;

use macsftp_core::{
    AppCommand, AppEvent, AuthCredential, ConnectCommand, ConnectionPoolIdentity,
    ConnectionSettings, HostKeyDecisionCommand, ProfileId, RuntimeBridgeConfig, SessionId, TabId,
};
use macsftp_sftp::{EventReceiver, HostTrustConfig, RuntimeController, SessionBackend};

struct PasswordServer {
    host: String,
    port: u16,
    username: String,
    password: String,
}

impl PasswordServer {
    fn from_environment() -> Option<Self> {
        let required = std::env::var_os("MACSFTP_REQUIRE_PASSWORD_TEST").is_some();
        let result = (|| {
            Some(Self {
                host: std::env::var("MACSFTP_PASSWORD_TEST_HOST").ok()?,
                port: std::env::var("MACSFTP_PASSWORD_TEST_PORT")
                    .ok()?
                    .parse()
                    .ok()?,
                username: std::env::var("MACSFTP_PASSWORD_TEST_USERNAME").ok()?,
                password: std::env::var("MACSFTP_PASSWORD_TEST_PASSWORD").ok()?,
            })
        })();
        if required && result.is_none() {
            panic!("password test is required but its fixture environment is incomplete");
        }
        if result.is_none() {
            eprintln!("Password integration test skipped: Docker fixture environment is absent");
        }
        result
    }

    fn settings(&self, password: String) -> ConnectionSettings {
        ConnectionSettings {
            host: self.host.clone(),
            port: self.port,
            username: self.username.clone(),
            auth: AuthCredential::Password { password },
        }
    }
}

fn temp_known_hosts_path() -> std::path::PathBuf {
    static SEQUENCE: AtomicU64 = AtomicU64::new(0);
    std::env::temp_dir().join(format!(
        "macsftp-password-known-hosts-{}-{}",
        std::process::id(),
        SEQUENCE.fetch_add(1, Ordering::Relaxed)
    ))
}

async fn next_event(events: &mut EventReceiver, label: &str) -> AppEvent {
    match tokio::time::timeout(Duration::from_secs(10), events.recv()).await {
        Ok(Some(event)) => event,
        Ok(None) => panic!("{label}: event channel closed"),
        Err(_) => panic!("{label}: timed out waiting for event"),
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn password_authenticates_and_a_different_identity_cannot_reuse_it() {
    let Some(server) = PasswordServer::from_environment() else {
        return;
    };
    let known_hosts_path = temp_known_hosts_path();
    let mut controller = RuntimeController::start(
        RuntimeBridgeConfig::default(),
        SessionBackend::Real(HostTrustConfig::new(known_hosts_path.clone(), None)),
    );
    let client = controller.client();
    let mut events = controller
        .take_event_receiver()
        .expect("password gate owns the event receiver");

    client
        .try_send(AppCommand::ConnectTab(ConnectCommand {
            tab_id: TabId(1),
            session_id: SessionId(1),
            session_epoch: 1,
            profile_id: ProfileId(0),
            pool_identity: ConnectionPoolIdentity::Ephemeral(SessionId(1)),
            settings: server.settings(server.password.clone()),
        }))
        .expect("send valid password connection");

    assert!(matches!(
        next_event(&mut events, "first TabConnecting").await,
        AppEvent::TabConnecting { tab_id: TabId(1) }
    ));
    let prompt = match next_event(&mut events, "HostKeyUnknown").await {
        AppEvent::HostKeyUnknown(prompt) => prompt,
        other => panic!("expected HostKeyUnknown, got {other:?}"),
    };
    client
        .try_send(AppCommand::AcceptHostKey(HostKeyDecisionCommand {
            request_id: prompt.request_id,
        }))
        .expect("accept fixture host key");

    loop {
        match next_event(&mut events, "first TabConnected").await {
            AppEvent::TabConnected(scoped) if scoped.scope.tab_id == TabId(1) => break,
            _ => {}
        }
    }

    client
        .try_send(AppCommand::ConnectTab(ConnectCommand {
            tab_id: TabId(2),
            session_id: SessionId(2),
            session_epoch: 1,
            profile_id: ProfileId(0),
            pool_identity: ConnectionPoolIdentity::Ephemeral(SessionId(2)),
            settings: server.settings("definitely-wrong-password".to_string()),
        }))
        .expect("send invalid password connection");

    loop {
        match next_event(&mut events, "second AuthFailed").await {
            AppEvent::AuthFailed(scoped) if scoped.scope.tab_id == TabId(2) => break,
            AppEvent::TabConnected(scoped) if scoped.scope.tab_id == TabId(2) => {
                panic!("wrong password reused a connection from another identity")
            }
            _ => {}
        }
    }

    controller.shutdown();
    std::fs::remove_file(known_hosts_path).expect("remove password fixture known_hosts");
}
