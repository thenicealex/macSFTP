---
name: macSFTP
description: A precise, native macOS SFTP workspace where files, transfers, and trust decisions stay visible.
colors:
  accent: "#007aff"
  accent-dark: "#0a84ff"
  text-on-accent: "#ffffff"
  background: "#ffffff"
  surface: "#f5f5f7"
  elevated-surface: "#ffffff"
  background-dark: "#1e1e1e"
  surface-dark: "#262628"
  elevated-surface-dark: "#2c2c2e"
  border: "#d1d1d6"
  border-dark: "#38383a"
  border-focus: "#007aff"
  border-focus-dark: "#0a84ff"
  text: "#1d1d1f"
  text-dark: "#f5f5f7"
  text-muted: "#86868b"
  text-muted-dark: "#8e8e93"
  text-disabled: "#c7c7cc"
  text-disabled-dark: "#636366"
  element-hover: "rgba(0, 0, 0, 0.05)"
  element-hover-dark: "rgba(255, 255, 255, 0.06)"
  element-active: "rgba(0, 0, 0, 0.09)"
  element-active-dark: "rgba(255, 255, 255, 0.10)"
  element-selected: "rgba(0, 122, 255, 0.14)"
  element-selected-dark: "rgba(10, 132, 255, 0.20)"
  error: "#ff3b30"
  error-dark: "#ff453a"
  warning: "#ff9500"
  warning-dark: "#ff9f0a"
  success: "#34c759"
  success-dark: "#30d158"
  info: "#0071a4"
  info-dark: "#64d2ff"
typography:
  display:
    fontFamily: ".SystemUIFont"
    fontSize: "22px"
    fontWeight: 700
    lineHeight: 1.1
  brand:
    fontFamily: ".SystemUIFont"
    fontSize: "18px"
    fontWeight: 500
    lineHeight: 1.2
  title:
    fontFamily: ".SystemUIFont"
    fontSize: "15px"
    fontWeight: 600
    lineHeight: 1.2
  heading:
    fontFamily: ".SystemUIFont"
    fontSize: "13px"
    fontWeight: 600
    lineHeight: 1.3
  body:
    fontFamily: ".SystemUIFont"
    fontSize: "13px"
    fontWeight: 400
    lineHeight: 1.35
  body-medium:
    fontFamily: ".SystemUIFont"
    fontSize: "13px"
    fontWeight: 500
    lineHeight: 1.35
  secondary:
    fontFamily: ".SystemUIFont"
    fontSize: "12px"
    fontWeight: 400
    lineHeight: 1.35
  label:
    fontFamily: ".SystemUIFont"
    fontSize: "11px"
    fontWeight: 500
    lineHeight: 1.2
  control:
    fontFamily: ".SystemUIFont"
    fontSize: "11.375px"
    fontWeight: 400
    lineHeight: 1.25
  mono:
    fontFamily: "Menlo"
    fontSize: "11px"
    fontWeight: 400
    lineHeight: 1.25
  shortcut:
    fontFamily: "Menlo"
    fontSize: "12px"
    fontWeight: 400
    lineHeight: 1.25
rounded:
  control: "5px"
  card: "10px"
  pill: "9999px"
spacing:
  "0.5": "1.625px"
  "1": "3.25px"
  "1.5": "4.875px"
  "2": "6.5px"
  "2.5": "8.125px"
  "3": "9.75px"
  "3.5": "11.375px"
  "4": "13px"
  "5": "16.25px"
  "6": "19.5px"
  "8": "26px"
components:
  button-primary:
    backgroundColor: "{colors.accent}"
    textColor: "{colors.text-on-accent}"
    rounded: "{rounded.control}"
    padding: "0 9.75px"
    height: "26px"
    typography: "{typography.control}"
  button-secondary:
    backgroundColor: "{colors.surface}"
    textColor: "{colors.text}"
    rounded: "{rounded.control}"
    padding: "0 9.75px"
    height: "26px"
    typography: "{typography.control}"
  button-danger:
    backgroundColor: "{colors.error}"
    textColor: "{colors.text-on-accent}"
    rounded: "{rounded.control}"
    padding: "0 9.75px"
    height: "26px"
    typography: "{typography.control}"
  connection-entry:
    backgroundColor: "{colors.accent}"
    textColor: "{colors.text-on-accent}"
    rounded: "{rounded.control}"
    padding: "0 9.75px"
    width: "160px"
    height: "34px"
    typography: "{typography.body}"
  icon-button:
    backgroundColor: "#00000000"
    textColor: "{colors.text}"
    rounded: "{rounded.control}"
    width: "26px"
    height: "22px"
  text-field:
    backgroundColor: "{colors.background}"
    textColor: "{colors.text}"
    rounded: "{rounded.control}"
    padding: "2px 8px"
    height: "19.5px"
    typography: "{typography.control}"
  tab-active:
    backgroundColor: "{colors.background}"
    textColor: "{colors.text}"
    rounded: "{rounded.control}"
    height: "26px"
  tab-inactive:
    backgroundColor: "#00000000"
    textColor: "{colors.text-muted}"
    rounded: "{rounded.control}"
    height: "26px"
  file-row:
    backgroundColor: "#00000000"
    textColor: "{colors.text}"
    height: "26px"
  file-row-selected:
    backgroundColor: "{colors.element-selected}"
    textColor: "{colors.text}"
    rounded: "{rounded.control}"
    height: "26px"
  modal-card:
    backgroundColor: "{colors.elevated-surface}"
    textColor: "{colors.text}"
    rounded: "{rounded.card}"
    width: "460px"
  palette-card:
    backgroundColor: "{colors.elevated-surface}"
    textColor: "{colors.text}"
    rounded: "{rounded.card}"
    width: "520px"
  transfer-row:
    backgroundColor: "#00000000"
    textColor: "{colors.text}"
    rounded: "{rounded.control}"
    height: "44px"
---

# Design System: macSFTP

## Overview

**Creative North Star: "The Instrument Bench"**

macSFTP is a precision workbench for files. The two panes are the bench surface, the chrome is the rack around it, and every readout — path, status dot, transfer progress, conflict state — must be legible at a glance. The system is built for repeated, consequential operations, so familiarity outranks novelty and exactness outranks expression. Nothing decorates the space between the user and the file.

Both appearances share one grammar: tonal surfaces, 1px hairlines, and system blue reserved for action and focus. Depth is a soft edge on floating layers, never a hierarchy device. Controls are 26px tall, file rows are 26px, labels are 11px; density is the point, not a compromise. The result should feel like a well-machined tool that happens to be software: quiet at rest, unambiguous under the pointer.

Motion is limited to state. The transfer drawer settles on a critically damped 300ms spring, overlays fade in over 150ms and dismiss instantly, progress interpolates over 200ms, and the busy glyph turns once every 900ms. Every one of these follows the system reduced-motion switch. When nothing is changing, nothing moves.

**Key Characteristics:**
- Fixed dual-pane shell: connection tabs above, local and remote file panes side by side, transfer drawer below, status bar at the edge.
- A single type family (the macOS system font) at 11–15px, with 18px and 22px reserved for the About window and Menlo reserved for machine facts.
- 26px controls and file rows; 5px radii on controls, 10px on cards.
- Hairline separation everywhere; no glass, gradients, or heavy shadows.
- System blue rationed to action, focus, folders, and selection tint.
- Four fixed-meaning status colors: error, warning, success, info.

## Colors

A near-neutral macOS palette whose only saturated voice is system blue, with four status colors that never change meaning.

### Primary
- **System Signal Blue** (`#007aff` light, `#0a84ff` dark): the fill of primary buttons, the focused text-field border and ring, the 1px underline that marks the focused pane, folder icons, the active sort arrow, the selected-row tint, and the busy glyph. It is never a page wash, heading color, or decorative fill.

### Neutral
- **Bench White** (`#ffffff`): the light window background and file-list canvas; elevated cards reuse it.
- **Cool Fog** (`#f5f5f7`): light chrome — tab bar, path bars, table header, drawer, status bar — and the plain button fill.
- **Bench Graphite** (`#1e1e1e`): the dark window background and file-list canvas.
- **Graphite Chrome** (`#262628`): dark chrome surfaces.
- **Graphite Lift** (`#2c2c2e`): dark elevated surfaces — modals, popovers, tooltips.
- **Rule Gray** (`#d1d1d6`) / **Rule Graphite** (`#38383a`): every 1px border, pane divider, table rule, and path-bar separator.
- **Ink** (`#1d1d1f`) / **Paper Ink** (`#f5f5f7`): primary text and icons.
- **Muted Gray** (`#86868b`) / **Muted Graphite** (`#8e8e93`): secondary text, table headers, timestamps, placeholders.
- **Disabled Gray** (`#c7c7cc`) / **Disabled Graphite** (`#636366`): disabled labels, icons, and the disconnected status dot.

### Status
- **Ember Red** (`#ff3b30` light, `#ff453a` dark): errors, failed transfers, and destructive button fills.
- **Amber** (`#ff9500` light, `#ff9f0a` dark): connecting, reconnecting, and host-verification states.
- **Signal Green** (`#34c759` light, `#30d158` dark): connected and successful states.
- **Deep Teal** (`#0071a4` light, `#64d2ff` dark): symlinks and informational marks.

### Named Rules

**The One Signal Rule.** System Signal Blue marks action, focus, selection, folders, and active sorting. If blue is not telling the user what they can do or where they are, it is decoration and must go.

**The Hairline Rule.** Regions are separated by a 1px border in the border token. Never use a shadow to divide panes, bars, or rows.

**The Fixed-Meaning Rule.** Error, warning, success, and info never change roles. State must also be carried by text or shape so color is never the only signal.

**The Two-Appearance Rule.** Every color token exists in both appearances. Components read tokens; they never hardcode a hex.

## Typography

**Display Font:** none — macSFTP has no display face.
**Body Font:** `.SystemUIFont` (SF Pro; CSS equivalent `-apple-system, BlinkMacSystemFont, "SF Pro Text", system-ui, sans-serif`)
**Label/Mono Font:** Menlo (`"SF Mono", monospace` fallback)

**Character:** One family carries the entire interface, so hierarchy comes from size, weight, and leading rather than pairing. Menlo appears only where the content is a machine fact: sizes, timestamps, shortcuts, and transfer detail.

GPUI 0.2 exposes no letter-spacing property, so tracking is not part of this system; hierarchy is carried by size, weight, line height, and color. Components take a semantic role (`theme.typography.*` through `text_role`), never a raw size.

### Hierarchy
Eleven roles, each with one job, drawn from seven sizes: 11, 11.375, 12, 13, 15, 18, 22. No in-between steps exist.

- **Display** (700, 22px, 1.1): the ⇄ mark in the About window, with the product name at 18px medium (Brand).
- **Title** (600, 15px, 1.2): page and surface titles — Settings, file-operation and connection modals, the palette header, the first-connection card.
- **Heading** (600, 13px, 1.3): section titles inside a surface — Settings cards, the transfer drawer header.
- **Body** (400, 13px, 1.35): file rows, tabs, palette rows, form text. Its medium twin (500, 1.35) marks the current breadcrumb segment, selected row names, switch titles, and empty-state lead lines.
- **Secondary** (400, 12px, 1.35): explanatory prose, field hints, error messages, tooltips, transfer titles, breadcrumb parents, drag previews.
- **Label** (500, 11px, 1.2): table headers, section eyebrows, status counts, transfer state labels, filter captions, field labels.
- **Control** (400, 11.375px, 1.25): button and input labels.
- **Mono** (400, 11px, 1.25): size and modified columns, remote endpoints, transfer detail.
- **Shortcut** (400, 12px, 1.25): keyboard shortcuts.

### Named Rules

**The One-Family Rule.** No display or decorative face enters the UI. The system font carries every label, heading, and row. Menlo is reserved for numbers, timestamps, shortcuts, and machine-generated strings.

**The Seven-Size Rule.** The scale is 11 / 11.375 / 12 / 13 / 15 / 18 / 22. Functional text stays between 11 and 15px; 18 and 22 appear only in About. The window rem is 13px, so micro-spacing steps in 3.25px increments and Control is 0.875rem. Anything outside the set is a defect, not a local tweak.

**The Four-Axis Rule.** Hierarchy is size, weight, leading, and color together. A title is not merely bigger — it is 600 weight at 1.2 leading; a label is not merely smaller — it is 500 weight at 1.2. Prose takes 1.35 leading, controls 1.25. Never change size alone.

## Layout

The shell never floats. From the top: a 34px tab bar, the two panes (32px path bar, 26px table header, 26px rows), a 5px divider box whose drawn rule is 1px, the transfer drawer, and a 26px status bar. The pane divider gives each side a stored ratio; neither side may drop below 20% or 300pt, and a window too narrow for both falls back to 50/50 rather than squeezing one pane.

The path bar is grouped into four clusters — navigation, breadcrumb, file actions, transfer — separated by 1px × 14px rules. Icon hit areas are frozen at 26×22px, and the breadcrumb's tail (the current directory) is the part that survives a narrow pane.

Lists are virtualized and keep a fixed rhythm: names take the remaining width, size is an 80px right-aligned column, modified is 130px, both in Menlo at 11px. Row height never changes with content, hover, selection, or progress. The transfer drawer opens at 160px, can shrink to 40px or grow to the smaller of half the content area and 480px, and resizes through a 5px grip. Its rows are 44px.

Empty states center a 13px medium lead line, an optional 12px muted detail capped at 320px wide, and at most a short row of actions. The first-connection card is the larger sibling: a 15px semibold title over 13px muted detail, with the 160×34px Connect entry as its decision. They state the next step, never a feature tour. Overlays are centered: the command palette is 520px wide with 48px top padding and a viewport-adaptive height; task modals are 460px wide.

Settings is a fixed 176px sidebar plus a scrollable content pane. General and Editor pad the pane 16.25px and center a 640px measure, so a wider window gains margin instead of stretching switches and fields; sections stack at 16.25px. Only groups with several rows take a card (Behavior, Transfers, Editor Associations, the profile form sections); a single control — the appearance segmented control, the File Editor field, the Troubleshooting buttons — sits directly under its header, and description text always stays closer to its own section than to the one above. Saved Connections keeps its 195px profile list beside the editor, with 48px profile rows and the same card rules.

### Adaptation

The window is the only size axis; there is no phone or tablet layout to switch to, so adaptation restructures inside the same shell. The window minimum is 720×480, the default is 1200×800, and the layout is verified at 720, 1024, 1280 and 1440.

- **Fixed columns, flexible middle.** The 176px Settings sidebar, the 195px profile list, the 34px tab bar, the 32px path bars and the 26px rows never scale with the window. Remaining width goes to the file panes, the form measure, or the detail pane.
- **Stack instead of squeeze.** Below an 820px window the profile editor's detail pane can no longer give a side-by-side field ~240px, so labels move above their fields and choice buttons wrap under their own label. Above it, labels keep their 112px column.
- **Long values truncate, descriptions wrap.** File names, transfer titles, rule patterns and app names, profile names, endpoints, breadcrumbs and tab titles ellipsize at the end of their slot and keep the full value in a tooltip; the row's actions always keep their place. Prose (section descriptions, errors) wraps instead, and security-critical fingerprints wrap rather than truncate because the full text is the content.
- **Type does not scale with a system text-size setting.** Native macOS desktop interfaces keep type at logical points; the seven-size scale and the 26px control floor are fixed, and the display's backing scale handles Retina rendering.

### Spacing

Spacing is the 13px window rem in quarter steps, so every gap is a multiple of 3.25px: 1.625, 3.25, 4.875, 6.5, 8.125, 9.75, 11.375, 13, 16.25, 19.5, 26. Layouts use gap first; margins are reserved for a single offset, and new scattered margins are a defect.

- **Inside a group** — a label and its control, icon and title, rows of one list: 3.25–6.5px. A section header keeps 1.625px between title and description.
- **Header to content:** 6.5px.
- **Between sections:** 16.25px, always larger than any gap inside either section.
- **Page padding:** 16.25px in Settings on every side; a scrollable form uses the same 16.25px as its section gap.
- **Inside cards and rows:** 13px horizontal, 11.375–13px vertical; list rows take 8.125px side padding with a 3.25px stack gap.

**The Proximity Rule.** A group is defined by its gap, not its border. Same-group content sits 3.25–6.5px apart; different groups sit at least 16.25px apart. A card is justified only when a group needs its own surface — a multi-row list, a form section, an overlay — never for a single control or a lone paragraph.

**The Scale Rule.** Every padding, gap, and margin is a multiple of 3.25px. Raw pixel values outside the scale are reserved for fixed geometry: 26px rows and controls, 44px transfer rows, 19.5px fields, 4px file-row inset, 5px pane grip, 1px hairlines.

**The Frozen-Height Rule.** Chrome, controls, and rows keep fixed heights. Hover, loading, progress, and error states change color and text, never geometry.

## Elevation & Depth

Flat, with edge-light on floating layers. The system has no glass, no translucency, and no ambient glow. Depth is conveyed first by tonal steps (canvas → chrome → elevated), then by a 1px border, and only last by a shadow that softens the edge. Every overlay dims the workspace behind it with the same 45% scrim.

### Shadow Vocabulary
- **Chip** (`0 1px 3px -1px rgba(0, 0, 0, 0.08)`): tooltips, the active tab, and other one-line affordances.
- **Popover** (`0 4px 6px -1px rgba(0, 0, 0, 0.10), 0 2px 4px -2px rgba(0, 0, 0, 0.10)`): command palette, tab switcher, drag previews.
- **Modal** (`0 8px 16px -4px rgba(0, 0, 0, 0.12), 0 2px 6px -2px rgba(0, 0, 0, 0.12)`): task modals and sheets.
- **Chrome highlight** (a 1px top rule at 8% white in dark appearance, 6% black in light): the window's top edge, the only material cue the chrome gets.

**The Edge-Light Rule.** A shadow may only soften the edge of a floating layer. It never establishes hierarchy, never lifts a row, and never divides a region.

**The One Scrim Rule.** Every overlay uses the same 45% scrim. There is no second dimming value and no blur-behind.

## Shapes

Controls, rows, tabs, and inputs use a 5px radius. Tooltips take a subtler step (3.25px, the rem-derived small radius). Task modals and the command palette use 10px. Status dots, progress bars, and the 18px circular tab-close affordance are fully round. Borders are always 1px; there are no double rules, bevels, or clipped corners. The pane divider is a single 1px rule inside a 5px grab area, and it stays a line rather than becoming a handle.

## Components

### Buttons
- **Shape:** 5px radius, 26px height, 9.75px horizontal padding, minimum width 19.5px.
- **Primary:** System Signal Blue fill with white text. The fill does not tint on hover — AppKit prominent buttons do not — and darkens by a 15% black overlay while pressed.
- **Secondary:** Cool Fog fill, 1px Rule Gray border, Ink text. Hover steps to the shared hover fill and fades the border to the fill color; pressed steps to the active fill.
- **Danger:** Ember Red fill with white text, same press behavior as primary.
- **Connection entry:** the first-connection action is its own, larger control — 160×34px, 13px label, System Signal Blue fill, and the shortcut parked at the trailing edge. Hover and press dim the whole control to 90%/80% opacity rather than darkening the fill.
- **Focus:** a 3px ring at 50% blue, drawn outside the button's bounds; disabled buttons stay neutral and take no ring. The connection entry's ring follows an 8px outer radius.
- **Icon buttons:** 26×22px, 16px glyph at 65% opacity (25% disabled), transparent at rest, shared hover/active fills, mandatory tooltip.

### Inputs / Fields
- **Style:** 19.5px height (1.5 × 13px), 5px radius, 1px border, 2px × 8px padding, 11.375px text. In dark appearance the field background is the canvas at 30% alpha.
- **Focus:** the border turns System Signal Blue and a 3px 50% ring appears outside it.
- **Placeholder:** Muted Gray. Disabled fields drop to 40% background alpha and muted text.

### Tabs
- **Active:** Bench White fill, 1px border, chip shadow, Ink text at medium weight, close button always visible.
- **Inactive:** transparent, muted text, hover fill; the 18px circular close affordance stays hidden until the tab is hovered, and its slot never shifts the title.
- **Status:** an 8px dot — green connected, amber connecting or awaiting trust, red failed, gray disconnected — with a tooltip label.
- Tabs cap at 220px, truncate the title, and never shrink the close slot.

### File Rows
- **Shape:** 26px height, 4px inset, 5px radius.
- **Default:** folder icons in System Signal Blue, symlinks in Deep Teal, files in Muted Gray; hidden names stay muted.
- **Selected:** the accent tint fill, medium-weight name, and secondary columns promoted from muted to full contrast — selection is never color alone, because weight and contrast change too.
- **Columns:** size 80px, modified 130px, both Menlo 11px; directories leave size blank rather than printing a dash.

### Transfer Rows
- **Shape:** 44px height, 5px radius, hover fill.
- **Content:** a 16px direction icon, a 12px title, an 11px state label in its semantic color, and a 3px full-radius progress bar on a shared active-fill track. Progress text always reports the real fraction while the drawn bar interpolates.
- **Actions:** reveal, cancel, and retry appear as 26×22 icon buttons only when the state allows them.

### Palette and Modals
- **Card:** 520px (palette) or 460px (modal), 10px radius, 1px border, elevated fill, popover or modal shadow. The palette pads 9.75px; task modals pad 13px.
- **Scrim:** the shared 45% dim; overlays enter with a 150ms fade and leave instantly.
- **Palette rows:** 13px text, a 56px shortcut column in Menlo 12px, selected row on the accent tint.
- **Modal actions:** the primary action sits right, with a clear cancel path; modal titles take the 15px Title role, and section titles inside a sheet take the 13px Heading role.

## Do's and Don'ts

### Do:
- **Do** keep the 26px row and control rhythm; the interface should read as one machined grid.
- **Do** separate regions with a 1px border in the border token.
- **Do** reserve System Signal Blue for action, focus, selection, folders, and active sorting.
- **Do** pair every icon-only control with a tooltip and an accessibility label.
- **Do** carry state through text, weight, and shape as well as color.
- **Do** keep floating layers flat and let the three-step edge-light scale do the work.

### Don't:
- **Don't** use gradients, glass, translucency, or heavy shadows.
- **Don't** fill large areas with brand color or turn chrome into decoration.
- **Don't** introduce a landing page, hero, marketing illustration, or decorative empty state.
- **Don't** lift rows or cards with shadows; hierarchy comes from tone and hairlines.
- **Don't** change chrome or row heights between states.
- **Don't** use Menlo for prose or the system font for machine data.
- **Don't** animate anything that is not a state change, and never bypass reduced-motion.
