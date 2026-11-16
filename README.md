# sablier_mini_kv

Stores the latest container state received from Sablier webhooks and serves it
through a small HTTP API. Data stays in memory and is lost on restart.

## Docker Compose

Use the included [compose.yaml](compose.yaml), or add its `sablier-mini-kv`
service to your existing Compose project:

```sh
docker compose up -d --pull always
```

The API is available at `http://127.0.0.1:3000`. Images are published to
`ghcr.io/ivsop/sablier_mini_kv` for Linux amd64 and arm64.

The included Compose file accepts these settings through the environment or `.env`:

| Setting | Default | Purpose |
| --- | --- | --- |
| `HTTP_PORT` | `3000` | Port exposed on the host's loopback interface |
| `IMAGE_TAG` | `latest` | Image version to deploy |
| `RUST_LOG` | `info` | Log level |

To build locally instead of pulling an image:

```sh
docker compose -f compose.yaml -f compose.build.yaml up -d --build
```

## Connect Sablier

Put both services on the same Docker network and add this to `sablier.yaml`:

```yaml
webhooks:
  endpoints:
    - url: http://sablier-mini-kv:3000/webhooks/sablier
      events:
        - started
        - stopped
```

See [Sablier's webhook documentation](https://sablierapp.dev/features/webhooks/).

## API

`GET /health` returns `200 OK` with the plain-text body `ok`, including before
any webhooks have arrived. Use it for uptime monitoring.

`GET /containers/{name}` returns the latest state as plain text: `started`,
`stopped`, or `paused`. Unknown containers return `404`.

```sh
curl http://localhost:3000/containers/my-nginx
```

From another container on the same network, use `sablier-mini-kv:3000` as the host.
Names are case sensitive and must be URL-encoded when needed.

`POST /webhooks/sablier` accepts JSON and returns `204`:

```json
{
  "event": "started",
  "instance": { "name": "my-nginx" },
  "timestamp": "2026-11-15T10:30:00Z"
}
```

All fields are required. Older timestamps are ignored. Sablier emits `started`
and `stopped`; the receiver also accepts `paused`.

State reflects received events and may be stale if a webhook is missed. There is
no authentication; keep the service on a trusted network or behind a proxy with
access controls.

## Run from source

```sh
cargo run --release -- --listen-addr 127.0.0.1:3000
```

The address can also be set through `LISTEN_ADDR` in the environment or `.env`.
