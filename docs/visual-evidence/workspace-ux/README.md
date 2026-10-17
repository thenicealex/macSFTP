# Workspace UX visual evidence

The light, dark and recent-connections captures were refreshed on 2026-10-17 through `regenerate_workspace_ux_evidence` (command below), which draws them with gpui's headless Metal renderer: the real scene, text system and rasterizer off-screen, so the refresh is deterministic and needs no screen-recording permission. These images show the app's content view only, without the native window titlebar. The preview state is a temporary configuration directory, an empty `/tmp/test` local folder (the test asserts it is empty) and six generated `example.com` recent connections; no live connection was made. Images are at 2× scale, 1440×960 for a 720×480 logical window. The path bars in them show the cluster rules — navigation, breadcrumb, file actions, transfer — with the 26×22 icon hit areas and the full `/tmp/test` breadcrumb intact. The pipeline turns the reduce-motion flag on so overlay fades are captured settled, keeping the images deterministic.

Three further captures show the visual-craft pass: a classified connection failure (title above a muted message line, next to the single-line empty state), the command palette over the workspace (shared scrim, elevated card) and a two-tab window (the inactive tab without a close button, the active one with it).

The type-select capture shows the accessibility pass: with the local pane focused, typing `b` moves the selection to `beta-report.pdf` and leaves every other row listed — no filter bar appears, because filtering is now an explicit action (⌘F, the menu/palette command or the filter field).

The dark-maximized and light form-focus captures remain the original 2026-10-11 native preview captures of those layouts.

| Capture | Verified behavior |
| --- | --- |
| [Light, 720×480](light-720x480.jpg) | Compact default transfer drawer, centered first-connection card, visible footer entry |
| [Dark, 720×480](dark-720x480.jpg) | Theme colors, centered queue guidance and the outward focus ring on the Base connection button |
| [Dark, maximized](dark-wide.jpg) | Resizing retains balanced pane layout and drawer height |
| [Light focus, 720×480](light-focus-720x480.jpg) | Connection shortcut opens the form; Tab moves to Port with a visible focus ring |
| [Recent connections, 720×480](light-recents-scroll-720x480.jpg) | The shared scrollbar reaches the last of six recent connections without clipping the final row |
| [Failed connection, 720×480](light-failed-connection-720x480.jpg) | A classified error leads with its title and keeps the message on a muted second line |
| [Command palette, 720×480](light-palette-720x480.jpg) | The scrim token dims the workspace behind the elevated palette card |
| [Two tabs, 720×480](light-two-tabs-720x480.jpg) | The inactive tab hides its close button; the active tab keeps it |
| [Type-select, 720×480](light-type-select-720x480.jpg) | Typing `b` jumps the selection to the first match; the list stays complete and the filter bar stays hidden |

Refresh the headless captures with (the test writes PNGs because the renderer hands back RGBA; the committed files are JPEG):

```sh
cargo test -p macsftp-app regenerate_workspace_ux_evidence -- --ignored --nocapture
cd docs/visual-evidence/workspace-ux
for name in light-720x480 dark-720x480 light-recents-scroll-720x480 \
    light-failed-connection-720x480 light-palette-720x480 light-two-tabs-720x480 \
    light-type-select-720x480; do
    sips -s format jpeg -s formatOptions high "$name.png" --out "$name.jpg" && rm "$name.png"
done
```

Follow-up captures refreshed the light/dark workspace and recent list after centering queue guidance and reducing recent rows to 30px; the 2026-10-17 passes added the outward focus ring on the connection button, the path bar cluster rules, and the failed-connection, palette and two-tab captures. The accessibility pass added the type-select capture (its own `/tmp/macsftp-type-select` folder, which the capture creates if missing, writes into only while it is empty, and leaves empty afterwards, so it never touches an entry it did not write), refreshed the palette (it gained the sort commands), and pinned the second tab to the `/tmp/test` fixture — a fresh tab opens the home folder, so that capture used to embed this machine's home listing and drift with its timestamps. The 2026-10-18 quality polish pass updated the command palette top padding (48px) and viewport-adaptive height (ensuring the bottom shortcut legend is never clipped at 480pt), replaced the path-bar Delete icon with a trash can icon, aligned the table header 'Name' label with file row names, and gave breadcrumb tail segments shrink protection. Two consecutive regeneration runs are byte-identical. The light form-focus capture remains evidence from the first pass; the form itself is unchanged. Native keyboard traversal to the connection button and Enter opening its form were verified after excluding the pane shortcut in its context.

Manual checks also covered the drawer header collapse button, reopening through the footer, ⌘J toggling, opening an empty local folder and its centered message. Automated geometry checks cover empty content in light/dark themes at 720×480 and 1200×800 and the path bar rules with their breadcrumb in a 360 pt pane; interaction tests cover preserved drawer height and wheel scrolling through recent connections. Unit tests cover the elevation scale order, the scrim range, the two-line error state and the tab close button hover behavior.

Validation: `bash scripts/check.sh` (574 tests plus 2 ignored across the workspace, including 42 UI tests), Clippy with `-D warnings`, formatting, architecture isolation and sensitive-log checks passed. These captures document disconnected workspace behavior; live SFTP and active-transfer visuals were outside this layout change.
