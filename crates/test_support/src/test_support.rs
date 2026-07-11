use std::net::{TcpListener, TcpStream};
use std::path::PathBuf;
use std::process::{Child, Command, Stdio};
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{Duration, Instant};

pub fn crate_name() -> &'static str {
    "macsftp-test-support"
}

/// A real OpenSSH server for SFTP integration tests.
///
/// Runs the local `/usr/sbin/sshd` as the current user on a loopback
/// port with generated host and client keys. This covers the full
/// handshake, host key verification, and public key authentication.
/// Password authentication is disabled (a non-root sshd cannot verify
/// passwords), so password tests can only assert the rejection path;
/// the full password matrix needs the Docker fixture in CI (plan §19).
///
/// `spawn()` returns `None` (with an explanatory message on stderr)
/// when sshd or ssh-keygen is unavailable — integration tests must
/// treat that as a skip, not a failure.
pub struct SshTestServer {
    child: Child,
    pub port: u16,
    pub username: String,
    /// Directory holding all generated fixture files.
    pub fixture_dir: PathBuf,
    /// Unencrypted ed25519 client key accepted by the server.
    pub client_key_path: PathBuf,
    /// Same key type, encrypted with [`Self::ENCRYPTED_KEY_PASSPHRASE`].
    /// Not in authorized_keys — only for key-loading tests.
    pub encrypted_key_path: PathBuf,
    /// The server's public host key in OpenSSH format.
    pub host_public_key: String,
}

static FIXTURE_COUNTER: AtomicU64 = AtomicU64::new(0);

impl SshTestServer {
    pub const ENCRYPTED_KEY_PASSPHRASE: &'static str = "macsftp-test-passphrase";

    pub fn spawn() -> Option<Self> {
        let sshd = PathBuf::from("/usr/sbin/sshd");
        if !sshd.exists() {
            eprintln!("SFTP integration tests skipped: /usr/sbin/sshd not available");
            return None;
        }
        let username = match std::env::var("USER") {
            Ok(user) if !user.is_empty() => user,
            _ => {
                eprintln!("SFTP integration tests skipped: USER not set");
                return None;
            }
        };

        let fixture_id = FIXTURE_COUNTER.fetch_add(1, Ordering::Relaxed);
        let fixture_dir =
            std::env::temp_dir().join(format!("macsftp-sshd-{}-{fixture_id}", std::process::id()));
        if let Err(error) = std::fs::create_dir_all(&fixture_dir) {
            eprintln!("SFTP integration tests skipped: cannot create fixture dir: {error}");
            return None;
        }

        let host_key_path = fixture_dir.join("host_ed25519");
        let client_key_path = fixture_dir.join("client_ed25519");
        let encrypted_key_path = fixture_dir.join("client_encrypted_ed25519");
        if !generate_key(&host_key_path, "")
            || !generate_key(&client_key_path, "")
            || !generate_key(&encrypted_key_path, Self::ENCRYPTED_KEY_PASSPHRASE)
        {
            eprintln!("SFTP integration tests skipped: ssh-keygen failed");
            return None;
        }

        let client_public = match std::fs::read_to_string(client_key_path.with_extension("pub")) {
            Ok(content) => content,
            Err(error) => {
                eprintln!("SFTP integration tests skipped: cannot read client key: {error}");
                return None;
            }
        };
        let host_public_key = match std::fs::read_to_string(host_key_path.with_extension("pub")) {
            Ok(content) => content.trim().to_string(),
            Err(error) => {
                eprintln!("SFTP integration tests skipped: cannot read host key: {error}");
                return None;
            }
        };

        let authorized_keys_path = fixture_dir.join("authorized_keys");
        if let Err(error) = std::fs::write(&authorized_keys_path, client_public) {
            eprintln!("SFTP integration tests skipped: cannot write authorized_keys: {error}");
            return None;
        }

        let port = match free_loopback_port() {
            Some(port) => port,
            None => {
                eprintln!("SFTP integration tests skipped: no free loopback port");
                return None;
            }
        };

        let sshd_config_path = fixture_dir.join("sshd_config");
        let sshd_config = format!(
            "Port {port}\n\
             ListenAddress 127.0.0.1\n\
             HostKey {host_key}\n\
             PidFile {pid_file}\n\
             AuthorizedKeysFile {authorized_keys}\n\
             PubkeyAuthentication yes\n\
             PasswordAuthentication no\n\
             KbdInteractiveAuthentication no\n\
             UsePAM no\n\
             StrictModes no\n\
             Subsystem sftp internal-sftp\n\
             LogLevel ERROR\n",
            host_key = host_key_path.display(),
            pid_file = fixture_dir.join("sshd.pid").display(),
            authorized_keys = authorized_keys_path.display(),
        );
        if let Err(error) = std::fs::write(&sshd_config_path, sshd_config) {
            eprintln!("SFTP integration tests skipped: cannot write sshd_config: {error}");
            return None;
        }

        let child = match Command::new(&sshd)
            .arg("-D")
            .arg("-f")
            .arg(&sshd_config_path)
            .arg("-E")
            .arg(fixture_dir.join("sshd.log"))
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
        {
            Ok(child) => child,
            Err(error) => {
                eprintln!("SFTP integration tests skipped: cannot start sshd: {error}");
                return None;
            }
        };

        let mut server = Self {
            child,
            port,
            username,
            fixture_dir,
            client_key_path,
            encrypted_key_path,
            host_public_key,
        };

        if !server.wait_until_ready(Duration::from_secs(5)) {
            let log =
                std::fs::read_to_string(server.fixture_dir.join("sshd.log")).unwrap_or_default();
            eprintln!("SFTP integration tests skipped: sshd did not become ready.\n{log}");
            return None; // Drop kills the child and cleans up.
        }

        Some(server)
    }

    fn wait_until_ready(&mut self, timeout: Duration) -> bool {
        let deadline = Instant::now() + timeout;
        while Instant::now() < deadline {
            if let Ok(Some(_status)) = self.child.try_wait() {
                return false; // sshd exited — config error
            }
            if TcpStream::connect(("127.0.0.1", self.port)).is_ok() {
                return true;
            }
            std::thread::sleep(Duration::from_millis(100));
        }
        false
    }
}

impl Drop for SshTestServer {
    fn drop(&mut self) {
        // Best-effort teardown; a leaked loopback sshd dies with the
        // test process group anyway.
        if self.child.kill().is_ok() {
            match self.child.wait() {
                Ok(_status) => {}
                Err(error) => eprintln!("WARN: sshd fixture wait failed: {error}"),
            }
        }
        if let Err(error) = std::fs::remove_dir_all(&self.fixture_dir) {
            eprintln!("WARN: sshd fixture cleanup failed: {error}");
        }
    }
}

fn generate_key(path: &std::path::Path, passphrase: &str) -> bool {
    Command::new("/usr/bin/ssh-keygen")
        .arg("-q")
        .arg("-t")
        .arg("ed25519")
        .arg("-N")
        .arg(passphrase)
        .arg("-f")
        .arg(path)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .map(|status| status.success())
        .unwrap_or(false)
}

/// Grab a free loopback port. Racy between drop and sshd bind, but
/// good enough for tests.
fn free_loopback_port() -> Option<u16> {
    let listener = TcpListener::bind(("127.0.0.1", 0)).ok()?;
    listener.local_addr().ok().map(|address| address.port())
}

#[cfg(test)]
mod tests {
    use super::crate_name;

    #[test]
    fn exposes_crate_name() {
        assert_eq!(crate_name(), "macsftp-test-support");
    }
}
