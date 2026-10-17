# Workspace UX visual evidence

The light, dark and recent-connections captures were refreshed on 2026-10-17 through `regenerate_workspace_ux_evidence` (command below), which draws them with gpui's headless Metal renderer: the real scene, text system and rasterizer off-screen, so the refresh is deterministic and needs no screen-recording permission. These images show the app's content view only, without the native window titlebar. The preview state is a temporary configuration directory, an empty `/tmp/test` local folder (the test asserts it is empty) and six generated `example.com` recent connections; no live connection was made. Images are at 2× scale, 1440×960 for a 720×480 logical window. The path bars in them show the cluster rules — navigation, breadcrumb, file actions, transfer — with the 26×22 icon hit areas and the full `/tmp/test` breadcrumb intact.

The dark-maximized and light form-focus captures remain the original 2026-10-11 native preview captures of those layouts.

| Capture | Verified behavior |
| --- | --- |
| [Light, 720×480](light-720x480.jpg) | Compact default transfer drawer, centered first-connection card, visible footer entry |
| [Dark, 720×480](dark-720x480.jpg) | Theme colors, centered queue guidance and the outward focus ring on the Base connection button |
| [Dark, maximized](dark-wide.jpg) | Resizing retains balanced pane layout and drawer height |
| [Light focus, 720×480](light-focus-720x480.jpg) | Connection shortcut opens the form; Tab moves to Port with a visible focus ring |
| [Recent connections, 720×480](light-recents-scroll-720x480.jpg) | The shared scrollbar reaches the last of six recent connections without clipping the final row |

Refresh the headless captures with (the test writes PNGs because the renderer hands back RGBA; the committed files are JPEG):

```sh
cargo test -p macsftp-app regenerate_workspace_ux_evidence -- --ignored --nocapture
cd docs/visual-evidence/workspace-ux
for name in light-720x480 dark-720x480 light-recents-scroll-720x480; do
    sips -s format jpeg -s formatOptions high "$name.png" --out "$name.jpg" && rm "$name.png"
done
```

Follow-up captures refreshed the light/dark workspace and recent list after centering queue guidance and reducing recent rows to 30px; the 2026-10-17 pass added the outward focus ring on the connection button, and a later pass the path bar cluster rules. The light form-focus capture remains evidence from the first pass; the form itself is unchanged. Native keyboard traversal to the connection button and Enter opening its form were verified after excluding the pane shortcut in its context.

Manual checks also covered the drawer header collapse button, reopening through the footer, ⌘J toggling, opening an empty local folder and its centered message. Automated geometry checks cover empty content in light/dark themes at 720×480 and 1200×800; interaction tests cover preserved drawer height and wheel scrolling through recent connections.

Validation: `bash scripts/check.sh` (538 tests plus 2 ignored across the workspace, including 37 UI tests), Clippy with `-D warnings`, formatting, architecture isolation and sensitive-log checks passed. These captures document disconnected workspace behavior; live SFTP and active-transfer visuals were outside this layout change.
