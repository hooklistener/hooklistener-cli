---
name: Hooklistener CLI
description: Industrial terminal interface for webhook debugging, forwarding, tunneling, and monitoring.
colors:
  ink: "#D8D8D2"
  ink-muted: "#8F938A"
  black: "#10110F"
  surface: "#171916"
  surface-raised: "#20231F"
  border: "#5F675C"
  grid: "#343A33"
  accent: "#D4B15F"
  info: "#7FA7C8"
  success: "#8FAE79"
  warning: "#D4B15F"
  error: "#C76F64"
  selection: "#2A3029"
typography:
  terminal:
    fontFamily: "User terminal monospace"
    fontSize: "terminal default"
    fontWeight: "normal"
    lineHeight: 1
    letterSpacing: "normal"
  label:
    fontFamily: "User terminal monospace"
    fontWeight: "bold"
    letterSpacing: "normal"
  data:
    fontFamily: "User terminal monospace"
    fontFeature: "tabular-nums"
rounded:
  none: "0 cells"
spacing:
  cell: "1 terminal cell"
  gap: "2 spaces"
  section: "1 blank line"
  margin: "1 cell"
components:
  status-ok:
    textColor: "{colors.success}"
    typography: "{typography.label}"
  status-error:
    textColor: "{colors.error}"
    typography: "{typography.label}"
  status-warning:
    textColor: "{colors.warning}"
    typography: "{typography.label}"
  panel:
    backgroundColor: "{colors.surface}"
    textColor: "{colors.ink}"
    rounded: "{rounded.none}"
  selection-row:
    backgroundColor: "{colors.selection}"
    textColor: "{colors.ink}"
---

# Design System: Hooklistener CLI

## 1. Overview

**Creative North Star: "Public Works Control Room"**

Hooklistener CLI should feel like a durable terminal instrument for live webhook operations. The interface is not decorative software; it is a working surface for inspecting traffic, forwarding events, replaying payloads, and proving what happened. The design language should draw from U.S. Graphics, technical manuals, public infrastructure labels, lab instruments, and control room readouts.

The current codebase contains a soft pastel Ratatui palette in `src/ui.rs` and `src/syntax.rs`. Treat that as legacy implementation detail, not the desired long-term identity. The target system is industrial, restrained, monospace-native, and robust. It keeps useful terminal glyphs such as arrows, rules, selection carets, box drawing, and spinners, but rejects emoji as interface.

**Key Characteristics:**

- Grid-aligned labels, stable columns, and terse state reporting.
- Status tokens such as `[OK]`, `[ERR]`, `[WARN]`, and `[INFO]` in command output.
- Text-first state communication reinforced by restrained color.
- High-density tables for inspection, with detail views for overflow.
- Keyboard-first interaction with visible mode and shortcut affordances.

## 2. Colors

The palette is a restrained industrial terminal palette: near-black surfaces, warm off-white text, muted metal borders, and semantic colors used only for state.

### Primary

- **Industrial Ink** (`#D8D8D2`): Primary terminal text. Use for body copy, table values, detail rows, and active operational data.
- **Signal Amber** (`#D4B15F`): Primary accent and warning role. Use for active borders, highlighted labels, pending states, and attention without alarm.

### Secondary

- **Instrument Blue** (`#7FA7C8`): Informational role. Use for URLs, GET/HEAD methods, secondary links, and non-critical system information.
- **Field Green** (`#8FAE79`): Success role. Use for connected states, 2xx responses, completed forwards, and `[OK]` status.

### Tertiary

- **Fault Red** (`#C76F64`): Error role. Use for failed forwards, 5xx responses, authentication errors, destructive confirmations, and `[ERR]` status.

### Neutral

- **Terminal Black** (`#10110F`): Base background.
- **Panel Surface** (`#171916`): Main TUI surface.
- **Raised Surface** (`#20231F`): Selected rows, overlays, and action menus.
- **Grid Line** (`#343A33`): Dividers, low-emphasis table rules, and inactive borders.
- **Muted Ink** (`#8F938A`): Metadata, pagination, inactive shortcuts, timestamps, and empty-state supporting text.
- **Metal Border** (`#5F675C`): Standard panel borders and structural lines.

### Named Rules

**The State-First Rule.** Color may reinforce state, but the words must carry the state. `connected`, `pending`, `ERR`, `200`, and `[OK]` must remain meaningful when color is removed.

**The Accent Scarcity Rule.** Amber and blue are operational signals, not decoration. If more than one accent competes in the same view, reduce the weaker one to muted ink.

**The Legacy Pastel Rule.** Do not add new pastel roles. Existing pastel constants should be migrated toward the industrial palette as affected screens are touched.

## 3. Typography

**Display Font:** User terminal monospace  
**Body Font:** User terminal monospace  
**Label/Mono Font:** User terminal monospace

**Character:** The CLI does not control the user's font. The design must work in Berkeley Mono, JetBrains Mono, SF Mono, Cascadia Code, Menlo, and common system monospace terminals. Hierarchy comes from alignment, casing, weight, spacing, and semantic color rather than font variety.

### Hierarchy

- **Display** (bold, terminal default size, line-height 1): Reserved for rare headers such as login title or top-level TUI identity. Avoid oversized banners.
- **Headline** (bold, terminal default size, line-height 1): Panel titles, table headings, modal titles, and major section labels.
- **Title** (bold label/value key): Detail labels such as `ID`, `METHOD`, `STATUS`, `TARGET`, `HEADERS`, and `BODY`.
- **Body** (regular, terminal default size): Request data, URLs, paths, messages, and detail values. Wrap long values with stable indentation.
- **Label** (bold, short uppercase or Title Case): Status tokens, table headers, and action-menu key labels. Uppercase is for short labels only, never full sentences.

### Named Rules

**The Monospace Contract.** Every column should align in a normal terminal font. Do not rely on emoji width, proportional glyphs, or fragile visual tricks.

**The Label Block Rule.** Detail views should prefer fixed-width label/value blocks:

```text
ID        req_123
METHOD    POST
STATUS    200
TARGET    http://localhost:3000
```

## 4. Elevation

Terminal elevation is structural, not shadow-based. Depth is conveyed through borders, spacing, selected-row backgrounds, modal overlays, and z-ordering. There are no shadows in the TUI. A surface is "raised" when it overlays another surface, uses the raised surface color, and has a clear border.

### Named Rules

**The No Shadow Rule.** Do not simulate drop shadows in terminal output. Use box drawing, spacing, and tonal contrast.

**The Overlay Discipline Rule.** Action menus and dialogs must have clear bounds, a visible title, a stable close action, and no ambiguous overlap with primary data.

## 5. Components

### Status Tokens

- **Shape:** Bracketed ASCII tokens in command output: `[OK]`, `[ERR]`, `[WARN]`, `[INFO]`.
- **Color:** Optional semantic color; the token text must remain meaningful without it.
- **Usage:** Start confirmations, errors, warnings, and informational notices. Do not mix token styles in the same command family.

### TUI Panels

- **Shape:** Square-corner box drawing, no rounded corners.
- **Border:** Standard borders use Metal Border or Grid Line. Active or primary panels may use Signal Amber or Instrument Blue sparingly.
- **Padding:** One terminal cell where space allows.
- **Titles:** Short noun phrases: `HTTP Tunnel`, `Live Requests`, `Actions`, `Headers`, `Body`.

### Tables

- **Style:** Dense, aligned, and scan-first.
- **Headers:** Short labels: `ID`, `Age`, `Method`, `Path`, `Status`, `Duration`, `Size`, `From`.
- **Rows:** Use stable widths for IDs, methods, statuses, duration, and size. Allow paths and URLs to truncate or wrap intentionally.
- **Selection:** Use a selection caret and raised surface. Selection must not rely on color alone.

### Action Menus

- **Style:** Overlay panel with visible title and fixed key/action rows.
- **Key Labels:** Keep keys left-aligned and actions plain: `D Details`, `P Pin request`, `R Replay`.
- **Close:** Always show `Esc Close`.
- **Behavior:** Do not hide primary actions behind undocumented shortcuts.

### Detail Views

- **Style:** Label/value blocks first, then structured sections for `HEADERS`, `QUERY`, `BODY`, and `RESPONSE`.
- **Long Values:** Preserve indentation when wrapping long headers, URLs, signatures, and JSON bodies.
- **Body Rendering:** Pretty-print JSON when valid; keep raw text readable when not JSON.

### Empty States

- **Style:** Quiet and operational. Explain the state, not the feature.
- **Example:** `Waiting for webhooks...` is acceptable. Avoid cute illustrations or promotional copy inside operational views.
- **Logo:** ASCII logo animation is allowed only in idle/empty states, never where it competes with active request data.

### Command Output

- **Style:** Script-safe by default, with human-readable confirmations for non-JSON output.
- **Tables:** Use `comfy_table` only when comparison matters. Use label/value blocks for single resources.
- **JSON:** `--json` output is machine contract and should not inherit visual styling.

## 6. Do's and Don'ts

### Do

- Do design for 80 columns first, then improve wider layouts.
- Do keep glyphs that improve terminal scanning: `→`, `●`, `⟳`, `✗`, `✓`, `▸`, box drawing, and spinners.
- Do pair every colored state with visible text.
- Do keep command output terse and predictable.
- Do use consistent labels across CLI and TUI surfaces.
- Do validate major TUI states with snapshots and representative terminal sizes.
- Do preserve `--json` as clean machine output.

### Don't

- Don't use emoji as interface, status, method labels, or decorative feedback.
- Don't add pastel colors to new surfaces.
- Don't use marketing language in operational states.
- Don't turn routine command output into banners.
- Don't make color the only difference between success, warning, and error.
- Don't let tables require a single ideal terminal width.
- Don't over-apply the industrial aesthetic until the CLI becomes cryptic or hostile.
