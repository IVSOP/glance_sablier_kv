# sablier_mini_kv

A small Tokio/Axum service that remembers each container's latest reported state
in memory. Includes Tower HTTP request tracing, `tracing-subscriber` logging,
Clap configuration, `.env` loading through dotenvy, and graceful Ctrl-C/SIGTERM
shutdown.

## Run

```sh
cargo run --release -- --listen-addr 127.0.0.1:3000
```

Alternatively set `LISTEN_ADDR`, or copy `.env.example` to `.env`. CLI arguments
override environment variables; existing environment variables override `.env`.
The default address is `127.0.0.1:3000`. Use `0.0.0.0:3000` to listen on all IPv4
interfaces, including when running inside a container. Configure log filtering
with `RUST_LOG` (default `info`); `RUST_LOG=debug` also shows ignored older events.
Use `--help` for CLI options.

## Connect Sablier

Add an endpoint in `sablier.yaml`, using a hostname reachable from Sablier:

```yaml
webhooks:
  endpoints:
    - url: http://sablier-mini-kv:3000/webhooks/sablier
      events:
        - started
        - stopped
```

[Sablier's webhook documentation](https://sablierapp.dev/features/webhooks/)
currently specifies `started` and `stopped`. This receiver also accepts `paused`
for producers that supply it; Sablier does not currently emit it.

## API

`POST /webhooks/sablier` with `Content-Type: application/json`:

```sh
curl -i http://127.0.0.1:3000/webhooks/sablier \
  -H 'Content-Type: application/json' \
  -d '{"event":"started","instance":{"name":"my-nginx"},"timestamp":"2026-11-15T10:30:00Z"}'
```

Returns `204 No Content`. All three fields are required, the name must not be
blank, and the timestamp must be RFC 3339. Additional JSON fields are accepted.
Malformed JSON returns `400`, invalid fields or unsupported events return `422`,
and missing/unsupported JSON content types return `415`. Bodies are limited to
16 KiB (`413` if exceeded).

`GET /containers/{name}`:

```sh
curl -i http://127.0.0.1:3000/containers/my-nginx
```

Returns `200` with a plain-text body of `started`, `stopped`, or `paused`.
Unknown containers return `404`. Names are case sensitive; URL-encode names
when needed. Lookup responses include `Cache-Control: no-store`.

Only the latest state per name is retained. Older timestamps are acknowledged
with `204` but ignored; equal timestamps use the last received event. Repeated
deliveries are accepted. State disappears on restart, and separate processes
have independent stores. The service reports the last delivered event, rather
than polling the container runtime. Sablier sends asynchronously without retries,
so missed deliveries can leave state stale.

There is no authentication; deploy on a trusted network or behind an access
controlled reverse proxy when exposing it beyond your machine.

## Check

```sh
cargo fmt --check
cargo clippy --all-targets -- -D warnings
cargo test --all-targets
```

Dependencies were resolved to current stable crates.io releases when implemented.
`Cargo.lock` records the resolved versions for reproducible builds.
