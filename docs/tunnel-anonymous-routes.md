# Anonymous tunnel routes

Start a public direct-response tunnel without signing in:

```text
hooklistener anon tunnel --port 3000
hooklistener anon tunnel --port 3000 --name stable-demo --ttl 1200
```

The CLI resolves and pins the target with the same loopback-first policy as an
authenticated tunnel. Anonymous routes last 15 minutes by default and accept a
TTL from 60 to 1,800 seconds. The service allows three active routes per source
IP, five creations per hour, 60 requests per minute per route, and request
bodies up to 1 MiB. Normal framing, response, header, address-pinning, and
deadline limits still apply.

The creation receipt prints two long-lived credentials once:

- The route token obtains a new short-lived, single-use relay ticket whenever
  the CLI reconnects. It does not authorize claim.
- The claim token moves the name into an authenticated organization. It does
  not authorize a relay connection.

Do not put either token in command logs, issue reports, URLs, or source control.
Machine mode emits the creation receipt followed by the normal tunnel NDJSON
event stream:

```text
hooklistener --json anon tunnel --port 3000 --ttl 900
```

## Claim a stable name

Save the route ID and claim token, sign in, then run:

```text
hooklistener anon claim <route-id> --token <claim-token> --org <organization-id>
```

Claim is atomic. It discards all pre-claim captures and creates the same name as
an authenticated static reservation. The receipt always reports zero captures
transferred. A collision rolls the operation back, so the old anonymous route
does not partially change ownership. Invalid IDs and claim tokens are
non-enumerable.

After claim, start the name through the authenticated path:

```text
hooklistener tunnel start --slug stable-demo --port 3000
```

## Detach and recover

To hand an authenticated stable session to another CLI without closing its
route, run:

```text
hooklistener tunnel detach <session-id> --reason "switching machines"
```

Detach fences the old connection and preserves the route and canonical session.
The next authorized activation reacquires it with a newer fence. Use `tunnel
stop` when the session and ephemeral route should become terminal instead.
