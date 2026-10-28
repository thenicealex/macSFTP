import os
import pathlib
import shutil
import subprocess
import tempfile
import unittest


class SensitiveLogsTests(unittest.TestCase):
    def setUp(self):
        self.directory = tempfile.TemporaryDirectory(prefix='macsftp-sensitive-logs-')
        self.addCleanup(self.directory.cleanup)
        self.root = pathlib.Path(self.directory.name)
        scripts = self.root / 'scripts'
        scripts.mkdir()
        self.scanner = scripts / 'check_sensitive_logs.sh'
        shutil.copyfile(pathlib.Path(__file__).with_name('check_sensitive_logs.sh'), self.scanner)
        self.source = self.root / 'crates/sample.rs'
        self.agent_source = self.root / 'crates/vendor-russh/src/keys/agent/client.rs'
        self.agent_source.parent.mkdir(parents=True)
        self.source.write_text('fn safe() {}\n')
        self.agent_source.write_text('fn safe() {}\n')
        self.bin = self.root / 'bin'
        self.bin.mkdir()
        dirname = shutil.which('dirname')
        if dirname is None:
            raise RuntimeError('dirname is required to test the scanner')
        (self.bin / 'dirname').symlink_to(dirname)

    def run_scanner(self, **extra_env):
        env = os.environ.copy()
        env.update(PATH=str(self.bin), **extra_env)
        return subprocess.run(['/bin/bash', str(self.scanner)], env=env, capture_output=True, text=True)

    def install_rg(self):
        rg = shutil.which('rg')
        if rg is None:
            raise RuntimeError('ripgrep is required to test the scanner')
        (self.bin / 'rg').symlink_to(rg)

    def test_missing_rg_fails(self):
        result = self.run_scanner()
        self.assertEqual(result.returncode, 127, result.stderr)
        self.assertIn('rg', result.stderr)
        self.assertIn('sensitive log scan failed', result.stderr)

    def test_each_scan_error_fails(self):
        rg = self.bin / 'rg'
        rg.write_text('''#!/bin/bash
count=0
if [[ -f "$SCAN_COUNT" ]]; then read -r count < "$SCAN_COUNT"; fi
count=$((count + 1))
printf '%s\\n' "$count" > "$SCAN_COUNT"
if [[ "$count" == "$SCAN_FAIL_ON" ]]; then exit 2; fi
exit 1
''')
        rg.chmod(0o755)
        count_file = self.root / 'scan-count'
        for scan in (1, 2, 3):
            with self.subTest(scan=scan):
                count_file.unlink(missing_ok=True)
                result = self.run_scanner(SCAN_COUNT=str(count_file), SCAN_FAIL_ON=str(scan))
                self.assertEqual(result.returncode, 2, result.stderr)
                self.assertIn('sensitive log scan failed', result.stderr)

    def test_clean_inputs_pass(self):
        self.install_rg()
        result = self.run_scanner()
        self.assertEqual(result.returncode, 0, result.stderr)

    def test_actual_scan_error_fails(self):
        self.install_rg()
        self.agent_source.unlink()
        result = self.run_scanner()
        self.assertEqual(result.returncode, 2, result.stderr)
        self.assertIn('sensitive log scan failed', result.stderr)

    def test_sensitive_logs_fail(self):
        self.install_rg()
        cases = [
            (self.source, 'tracing::info!(password);\n', 'credential or fingerprint'),
            (self.source, 'tracing::info!(?event);\n', 'complete event payload'),
            (self.agent_source, 'tracing::debug!(identities: data);\n', 'identity or signing material'),
        ]
        for source, content, message in cases:
            with self.subTest(message=message):
                source.write_text(content)
                result = self.run_scanner()
                self.assertNotEqual(result.returncode, 0, result.stderr)
                self.assertIn(message, result.stderr)
                source.write_text('fn safe() {}\n')


if __name__ == '__main__':
    unittest.main()
