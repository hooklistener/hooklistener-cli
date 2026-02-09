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

# List orgs and select a default org for API-backed commands
hooklistener org list
hooklistener org use <organization-id>

# Forward webhooks to localhost
hooklistener listen my-endpoint

# Manage endpoints and requests
hooklistener endpoint list
hooklistener endpoint requests <endpoint-id>
hooklistener endpoint forward-request <endpoint-id> <request-id> http://localhost:3000/webhook

# Expose local server with a public URL
hooklistener tunnel --port 3000

# Use JSON output for scripts
hooklistener --json endpoint list

# Generate shell completions
hooklistener completions bash
```

For full documentation, visit the [GitHub repository](https://github.com/hooklistener/hooklistener-cli).

## License

MIT
