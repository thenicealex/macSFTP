# Product

<!-- impeccable:product-schema 1 -->

## Platform

macos

## Users

Prioritize developers and system administrators who manage files on remote servers from a Mac. They need to connect, navigate local and remote directories, transfer files, and work on remote files while maintaining control over potentially consequential operations.

## Product Purpose

macSFTP is a focused SFTP client for macOS. It brings local files, remote files, connection tabs, and transfers into one workspace so users can carry out everyday file management with clear feedback and informed decisions.

Successful work balances safe and reliable operations, efficient workflows, and a familiar native Mac experience. No quantitative success metric has been established.

## Positioning

A keyboard-friendly, dual-pane Mac workspace for working with files over SFTP. It combines visible transfer state with explicit decisions about server trust, file conflicts, and uploading edited files.

## Operating Context

The primary surface is a native desktop application built with Rust and GPUI. It requires macOS 13 or newer; arm64 is the tested architecture. The repository also contains a separate product website.

Impeccable does not currently recognize `macos` in its platform selector and falls back to `web`. Future design work must use the actual desktop platform and the project's macOS guidance rather than assume a web or mobile interface.

Users can work in multiple windows and connection tabs, use saved profiles or temporary connections, and manage files on either side of the workspace. Directory uploads and downloads report progress in a transfer drawer shared across windows.

Remote editing uses an external editor and a temporary local copy. Saving that copy does not upload it: the user explicitly starts the upload. The app checks current remote metadata before uploading and offers a conflict decision when it detects a change. Closing the tab or window ends the edit session and removes its temporary directory.

## Capabilities and Constraints

- SFTP is the supported file-transfer protocol. Other protocols, a built-in terminal, and directory sync are outside the current scope.
- Authentication supports passwords, keyboard-interactive prompts, SSH agent, and private keys. Connections can use saved-profile jump hosts or an explicitly configured ProxyCommand.
- Passwords and private-key passphrases are stored in macOS Keychain, with only short-lived copies in memory. Unknown host keys require an explicit decision; a changed host key blocks the connection.
- File and directory transfers support progress, conflict decisions, cancellation, and retry. Applying a conflict choice to all is scoped to the current transfer plan. Symlinks are copied as links, and metadata preservation failures are surfaced as warnings when the data transfer succeeds.
- Transfers already queued are independent of a browsing tab, but transfers do not continue after the app quits or resume after relaunch.
- Remote-edit conflict checks use file size and whole-second modification time; they can miss same-size changes made within the same second.
- The project is pre-release. Local app bundles are unsigned; the README describes public releases as source only. Developer ID signing, notarization, and automatic updates are outside the current scope.
- The app opens into an operable file workspace. Its interface should remain responsive with large directory listings and communicate loading, progress, errors, and recovery actions clearly.

## Brand Commitments

The product name is macSFTP. Its existing app icon assets are `packaging/macos/AppIcon.svg` and `packaging/macos/AppIcon.png`.

Product copy is concise, specific, and action-oriented. Security and destructive actions explain their consequences; recoverable errors offer a next step. The app may use one consistent language per build; the repository maintains README versions in English, Simplified Chinese, Japanese, and Korean.

## Evidence on Hand

The README and `docs/architecture.md` document the shipped workflows and product constraints. Screenshots in `docs/screenshots/` show sample local files and a disconnected server pane; they are not evidence of a live server session. No customer testimonials, adoption figures, performance benchmarks, or formal accessibility certification were established for this record.

## Product Principles

1. Balance safety and reliability, efficient workflows, and native Mac behavior when making product tradeoffs.
2. Make trust, conflicts, progress, and recovery visible so users can make informed decisions.
3. Keep remote file changes under explicit user control, especially when uploading edits or replacing existing files.
4. Make frequent file operations quick and keyboard-accessible without sacrificing clear state or recoverability.
5. Keep the product focused on the SFTP workspace and its documented scope.

## Accessibility & Inclusion

Support keyboard navigation of file lists, tabs, panes, dialogs, and Settings. Icon-only controls require a tooltip or label; dialogs need a logical focus order and focus restoration. Loading, error, and success states must not rely only on color or animation. Respect the system reduced-motion setting and maintain readable text contrast. No specific formal accessibility standard or certification has been confirmed.
