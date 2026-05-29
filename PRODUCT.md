# Product

## Register

product

## Users

Hooklistener CLI is for developers, platform engineers, support engineers, and technical teams debugging webhook traffic from a terminal. They use it while building integrations, reproducing provider events, forwarding real requests into local services, sharing payloads with teammates, and checking endpoint health.

Users are often in an operational mindset: they need to know what arrived, where it came from, whether forwarding worked, what failed, and what to do next. The interface must support fast scanning, keyboard-first inspection, scriptable output, and trustworthy state reporting.

## Product Purpose

Hooklistener CLI lets users debug, forward, replay, share, tunnel, and monitor webhooks without leaving the terminal. It combines interactive Ratatui views for live inspection with automation-friendly commands and `--json` output for scripts and CI.

Success means a user can move from "a webhook is failing" to a concrete diagnosis quickly: inspect the request, replay it locally, verify the response, share evidence, or expose a local callback URL with minimal friction.

## Brand Personality

Operational, robust, precise.

The product should feel like durable infrastructure: calm, capable, and direct. It should report state with confidence, use terse language, and rely on alignment, spacing, and disciplined information hierarchy instead of decorative personality.

## Anti-references

- Emoji-led CLI output, playful status icons, and cute feedback copy.
- Generic SaaS polish: soft cards, bubbly language, decorative gradients, and "friendly app" ornament.
- Pastel-heavy terminal themes that make serious operational states feel casual or low-contrast.
- Military cosplay, fake severity, or cryptic minimalism. The tone is industrial and public-infrastructure-like, not hostile.
- Wide terminal layouts that only work in one viewport.
- Hidden keyboard actions that require memorization without visible affordance.
- Color-only status communication that fails when ANSI is stripped or color perception differs.

## Design Principles

1. **The terminal is an instrument panel.** Prioritize state, counters, IDs, timestamps, routes, methods, and outcomes over decoration.
2. **One operational vocabulary.** Use the same status tokens, labels, table patterns, and recovery language across command output and TUI views.
3. **Density is a feature when it is disciplined.** Webhook debugging needs compact information, but columns, wrapping, and grouping must remain stable at practical terminal sizes.
4. **Every state needs text.** Color and glyphs may reinforce state, but the user must understand status with color removed.
5. **Human and machine output are both first-class.** Interactive views should be efficient for humans; `--json` and non-interactive output should stay clean, predictable, and script-safe.

## Accessibility & Inclusion

Target accessible terminal behavior rather than browser-only accessibility rules:

- Preserve keyboard-only operation across TUI workflows.
- Keep focus, selection, loading, success, warning, and error states distinguishable without relying on color alone.
- Maintain readable output when ANSI styling is stripped, when logs are copied, and when output is piped.
- Prefer high-contrast semantic color roles that work on common dark and light terminal themes.
- Avoid emoji as interface, because rendering varies by platform and can degrade alignment.
- Validate important TUI states at narrow, normal, and wide terminal sizes, with 80 columns as the baseline.
