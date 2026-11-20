# glance_sablier_kv

A companion service for Glance that stores Sablier container events and runs
health checks on demand.

## Try it with Glance

The [Compose file](compose.yaml) runs Glance and the helper on the same network.
To try them from this checkout, build the helper locally.

```sh
docker compose -f compose.yaml -f compose.build.yaml up -d --build
```

Open [Glance on port 8080](http://localhost:8080) to see both services' health
checks. Set `GLANCE_PORT` if port 8080 is already in use. To use the published
helper image, run `docker compose up -d` with the base file.

For an existing Glance deployment, copy the widget from the
[example configuration](examples/glance/glance.yml) and point its site entries
at your services. Use `check-url` when the probe and the link use different
addresses. Glance's `cache` setting controls how often it requests fresh results.

## Connect Sablier

Connect Sablier to the same Docker network and add this to `sablier.yaml` to
track starts and stops.

```yaml
webhooks:
  endpoints:
    - url: http://glance-sablier-kv:3000/webhooks/sablier
      events: [started, stopped]
```

Read the latest event with `GET /containers/{name}`. The service itself responds
to `GET /health`.
