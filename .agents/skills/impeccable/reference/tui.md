# Terminal/TUI register

Use this register for CLIs, terminal dashboards, full-screen TUIs, command output, logs, prompts, and keyboard-driven workflows.

## Identity

Terminal design is operational design. It should feel like durable instrumentation: clear state, aligned data, predictable controls, terse copy, and zero decoration that competes with the task.

Prefer:
- Monospace-native rhythm.
- Fixed columns, aligned labels, and stable widths.
- Short status tokens: `[OK]`, `[ERR]`, `[WARN]`, `[INFO]`.
- Explicit state text in addition to color.
- Functional glyphs only when they improve scanning: arrows, rules, bullets, selection carets, box drawing, spinners.
- Compact, repeatable patterns for details, lists, tables, errors, confirmations, and empty states.

Avoid:
- Emoji-as-interface.
- Cute or apologetic copy in operational states.
- Decorative banners in routine flows.
- Color as the only state carrier.
- Wide tables that only work at one terminal width.
- Hidden shortcuts with no visible affordance.

## Layout

- Design for 80 columns first unless the product explicitly targets wider terminals.
- Verify narrow, normal, and wide widths. Typical checkpoints: 80x24, 100x30, 120x40.
- Keep columns stable. Use truncation, wrapping, or detail views intentionally instead of letting tables explode horizontally.
- Group related lines tightly; separate concepts with blank lines or rules.
- Prefer label/value blocks for details:

```text
ID        req_123
METHOD    POST
STATUS    200
TARGET    http://localhost:3000
```

- Prefer tables for lists users compare:

```text
ID        METHOD   STATUS   PATH
req_123   POST     200      /webhooks/github
```

## Color

- Treat color as semantic reinforcement, not decoration.
- Define roles: success, error, warning, info, muted, selected, focus, border, surface.
- Make output readable with color stripped. ANSI-free output should still communicate status.
- Account for light and dark terminal themes; avoid low-contrast pastel text as the only signal.
- Respect `NO_COLOR` when the codebase already supports it or when adding color controls is in scope.

## Typography

- You do not pick fonts in most terminal products; you design for a user's monospace font.
- Hierarchy comes from spacing, casing, labels, alignment, intensity, and color roles.
- Use uppercase for short section labels and status tags. Do not use all-caps sentences.
- Keep line length intentional. Long prose in terminal output should wrap cleanly and preserve indentation.

## Interaction

- Make the keyboard model visible. Show the active mode and the high-value shortcuts in a stable place.
- Selection, focus, disabled, loading, empty, and error states must be distinct without relying on color alone.
- Do not overload single keys across nearby modes unless the mode is unmistakable.
- Preserve muscle memory. If a key means "back" or "quit" in one view, keep it consistent.
- For long-running work, show progress or heartbeat state and keep cancellation discoverable.

## Copy

- Write like a machine reporting state, not a chatbot.
- Confirm what happened and what the user can do next.
- Error messages need cause and recovery:

```text
[ERR] AUTH REQUIRED

MESSAGE   Not authenticated
ACTION    Run `hooklistener login`
```

- Success messages should be specific:

```text
[OK] ENDPOINT CREATED

ID        ep_123
URL       https://example.hook.events
```

## Validation

Use evidence appropriate to terminals:
- Snapshot tests for rendered TUI states.
- Golden output tests for command output.
- Manual command runs for important flows.
- Terminal screenshots when visual layout matters.
- Width/height checks for responsive layouts.
- ANSI-stripped output review for logs, pipes, and accessibility.
- Emoji/non-ASCII scans when the product has a strict visual language. Keep useful terminal glyphs if the product allows them.

## U.S. Graphics / Industrial Direction

When the product asks for a U.S. Graphics, industrial, robust, or Berkeley Mono-like aesthetic:
- Favor stamped status tokens, fixed grids, labels, counters, timestamps, rules, and terse state language.
- Use restrained color. Let alignment, density, and typography carry the identity.
- Make the interface feel like infrastructure, not military cosplay.
- Keep it operational, not decorative; precise, not cryptic.
