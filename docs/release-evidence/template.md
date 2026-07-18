# macSFTP release evidence

Version: X.Y.Z
Status: IN PROGRESS
Tester: TBD
Date: TBD
Supported architectures: TBD
Minimum macOS tested: TBD
Latest supported macOS tested: TBD

Use only local fixtures or non-sensitive test servers in screenshots and notes.

- [ ] `bash scripts/check.sh`
- [ ] Password-authentication Docker gate
- [ ] Isolated macOS Keychain gate
- [ ] `bash scripts/build_app.sh` and `plutil` validation
- [ ] App launches from the generated bundle
- [ ] Native menus, About, Settings, and version display
- [ ] Two windows preserve independent tabs and paths after quit/relaunch
- [ ] Closing windows in either order does not discard another window's state
- [ ] Password and Ed25519/ECDSA private-key connections
- [ ] Unknown host acceptance and host-key mismatch blocking
- [ ] Upload, download, conflict, cancel, and retry
- [ ] 10k entries, filtering, tab switching, and four active transfers
- [ ] 720x480, Retina, light/dark themes, keyboard navigation, basic VoiceOver
- [ ] Log inspection contains no credentials, fingerprints, or private-key paths

Notes:

- TBD
