# hooklistener

A fast, terminal-based CLI for browsing webhooks, forwarding events, and exposing local servers using [Hooklistener](https://hooklistener.com).

## Installation

```bash
npm install -g hooklistener
```

## Usage

```bash
# Authenticate
hooklistener login

# Browse webhooks (TUI)
hooklistener

# Forward webhooks to localhost
hooklistener listen my-endpoint

# Expose local server with a public URL
hooklistener tunnel --port 3000
```

For full documentation, visit the [GitHub repository](https://github.com/hooklistener/hooklistener-cli).

## License

MIT
