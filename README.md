# glance_sablier_kv

Stores the latest container state received from Sablier webhooks and serves it
through a small HTTP API. It also runs on-demand health checks for Glance.

## Docker Compose

Use the included [compose.yaml](compose.yaml), or add its `glance-sablier-kv`
service to your existing Compose project:

```sh
docker compose up -d --pull always
```

The API is available at `http://127.0.0.1:3000`. Images are published to
`ghcr.io/ivsop/glance_sablier_kv` for Linux amd64 and arm64.

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
    - url: http://glance-sablier-kv:3000/webhooks/sablier
      events:
        - started
        - stopped
```

See [Sablier's webhook documentation](https://sablierapp.dev/features/webhooks/).

## Glance health checks

`POST /checks` accepts the sites to check in a JSON body. A Glance `custom-api`
widget can send the whole list in one request and use its `cache` setting to
control how often it asks for fresh results.

```yaml
- type: custom-api
  title: Services
  cache: 5m
  url: http://glance-sablier-kv:3000/checks
  method: POST
  body:
    sites:
      - title: Jellyfin
        url: https://jellyfin.example.com
        check-url: http://jellyfin:8096/health
  template: |
    {{ range .JSON.Array "sites" }}
      <p>{{ .String "title" }} · {{ .String "status" }}</p>
    {{ end }}
```

The checker sends a GET to each `check-url`, falling back to `url`. It follows
redirects and treats HTTP 200 and any configured `alt-status-codes` as healthy.
Sites can also specify `basic-auth`, `allow-insecure`, and `timeout` (default
`3s`). The batch has 4.5 seconds to finish so Glance can receive the response
within its five-second HTTP timeout.

The response includes a `sites` array in request order. Each entry contains
`title`, `url`, any supplied `icon`, `response-time-ms`, and a `status` of
`ok`, `http-error`, `timed-out`, or `unreachable`. HTTP responses also include
`code`, and unreachable sites include an `error` message.

## API

`GET /health` returns `200 OK` with the plain-text body `ok`, including before
any webhooks have arrived. Use it for uptime monitoring.

`GET /containers/{name}` returns the latest state as plain text: `started`,
`stopped`, or `paused`. Unknown containers return `404`.

```sh
curl http://localhost:3000/containers/my-nginx
```

From another container on the same network, use `glance-sablier-kv:3000` as the host.
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
