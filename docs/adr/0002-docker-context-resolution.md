# ADR-0002: Docker context resolution & endpoint mapping

- **Status:** accepted
- **Date:** 2026-08-02
- **Milestone:** M0 spike (b)
- **Tested against:** Docker CLI 29.7.1, Linux. This machine doubled as a real-world fixture: it has a rootful `default` context **plus** a stale `desktop-linux` context (endpoint socket absent) **plus** an unrelated user-level `/run/user/1000/docker.sock` — exactly the ambiguity the plan's diagnostics anticipate.
- **Method:** throwaway harness; created/removed only `mast-spike-*` contexts; never ran `docker context use`; persisted `currentContext` verified untouched after the run.

## Question

How does the docker CLI decide the effective endpoint (context files, `DOCKER_CONTEXT`, `DOCKER_HOST`), how should Mast resolve it identically, and what maps onto bollard for observation?

## Findings (empirical)

1. **Precedence, proven end-to-end:** `DOCKER_HOST` > `DOCKER_CONTEXT` (named) > persisted `currentContext` (`~/.docker/config.json`, `null` ⇒ `default`) > `default`. With both `DOCKER_HOST=tcp://192.0.2.1:2375` and `DOCKER_CONTEXT=mast-spike-prec` set, `docker context inspect` reports **`Name: default`** with the `DOCKER_HOST` endpoint — the CLI synthesizes an ad-hoc default context and **ignores the named context entirely** — and a daemon command actually dials the tcp endpoint (hangs on TEST-NET, rather than failing fast on the named context's missing socket).
2. **`docker context inspect` (no args) computes the effective answer** — under `DOCKER_HOST` it reports that endpoint, not the stored one — **client-side and offline**. This is the resolver: run `docker context inspect --format json` with the exact environment of the ComposeInvocation and parse the result. Never hand-parse `config.json` (validates plan §4), never reimplement precedence.
3. **`docker context show` does not validate**: `DOCKER_CONTEXT=anything-at-all docker context show` happily echoes the nonexistent name with exit 0. Only daemon-touching commands fail (`context not found`, exit 1). Mast must validate context existence by enumeration, not by `show`.
4. **Enumeration format gotcha:** `docker context ls --format json` emits **NDJSON** (one object per line: `Name`, `DockerEndpoint`, `Current`, `Error`), not a JSON array. `docker context inspect <name> [<name>…]` returns a proper JSON array with full `Endpoints.docker.{Host,SkipTLSVerify}`, `TLSMaterial`, `Storage.{MetadataPath,TLSPath}`. Enumerate names via `ls`, then `inspect` for detail.
5. **Endpoint shapes** stored verbatim: `unix:///…`, `ssh://user@host`, `tcp://…`. TLS material, when present, lives under the context's `Storage.TLSPath` dir.
6. **Rootless/Desktop detection:** the truthful signal is per-endpoint `docker info --format '{{json .SecurityOptions}}'` containing `"name=rootless"`. **Socket location is not proof**: this machine has `/run/user/1000/docker.sock` present while the current context is rootful (`apparmor`/`seccomp`/`cgroupns`, no rootless flag). Detection must query the endpoint it's classifying.
7. **Stale Desktop context, live specimen:** `desktop-linux` exists with `DockerEndpoint: unix:///home/matt/.docker/desktop/docker.sock`, `Error: ""` in `ls` — but the socket doesn't exist. `ls` output alone looks healthy; only dialing fails. Error strings are clean and parseable:
   - dead socket: `failed to connect to the docker API at unix:///…; check if the path is correct and if the daemon is running: dial unix …: connect: no such file or directory` (exit 1)
   - missing context: `Failed to initialize: unable to resolve docker endpoint: context "…": context not found: open ~/.docker/contexts/meta/<sha>/meta.json: …` (exit 1)
8. `docker compose` daemon-touching commands follow identical context resolution and fail identically (`compose config` remains offline per ADR-0001 finding 6).

## Decision

- **Resolver:** effective context/endpoint is obtained by running `docker context inspect` with the ComposeInvocation's environment overlay (`DOCKER_HOST`/`DOCKER_CONTEXT` if the project or user set them). Result is cached on the invocation as `effective_endpoint` with provenance (`DockerHostEnv | DockerContextEnv | PersistedCurrent | Default`). Existence of a named `DOCKER_CONTEXT` is validated by enumeration before use, since the CLI won't.
- **bollard mapping for observation:**
  - `unix://` → `connect_with_unix`
  - `tcp://` / `http://` → `connect_with_http`; with TLS material (`Storage.TLSPath`, honoring `SkipTLSVerify`) → `connect_with_ssl`
  - `ssh://` → **unsupported for bollard observation in v1**: project enters a documented "observation degraded" state (poll via CLI where needed); lifecycle shell-outs are unaffected (the CLI handles ssh natively)
  - `npipe://` → Windows milestone (M8); `fd://` → unsupported → CLI fallback
- **Diagnostics confirmed with real data** (M7 check set): stale/dead context endpoint (the `desktop-linux` case), dual-socket ambiguity (rootful current + user socket present), `DOCKER_HOST`-shadows-context surprise (user sets `DOCKER_CONTEXT` but an exported `DOCKER_HOST` silently wins).

## Consequences

- `context_env` on ComposeInvocation stays minimal (`DOCKER_HOST?`, `DOCKER_CONTEXT?`, `COMPOSE_PATH_SEPARATOR?`); everything else resolves through the CLI at observation time, so Mast can never drift from docker's own precedence.
- Live Docker Desktop behavior (running daemon, `desktop-linux` healthy) was not testable on this machine; the not-running shape is captured above. Residual risk small and confined to M2's capability detection; revisit on a Desktop machine.
- Rootless _daemon_ behavior beyond detection (socket perms, port-forward differences) is deferred to the M7 diagnostics work as planned.
