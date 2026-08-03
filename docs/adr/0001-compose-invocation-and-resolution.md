# ADR-0001: ComposeInvocation & `docker compose config` resolution semantics

- **Status:** accepted
- **Date:** 2026-08-02
- **Milestone:** M0 spike (a)
- **Tested against:** Docker 29.7.1, Docker Compose v5.3.1, Linux; `laravel/sail` 1.x `bin/sail` (691-line script fetched 2026-08-02)
- **Method:** throwaway bash harness (23 isolated scenarios, fully controlled env via `env -i`) + a live sail-style fixture with unique project name `mast-spike-a-labels` (created, inspected, torn down). Spike code does not ship; every finding below is reproducible from the tables here.

## Question

What exactly determines which files, project name, env files, and profiles a `docker compose` invocation resolves to — and when Mast manages a Sail project, must lifecycle go through `vendor/bin/sail` to match the developer's terminal?

## Findings (empirical)

### 1. File selection

| Scenario                                               | Result                                                                                                  |
| ------------------------------------------------------ | ------------------------------------------------------------------------------------------------------- |
| `compose.yaml` only / `docker-compose.yml` only        | each used                                                                                               |
| both present                                           | `compose.yaml` wins; **warning on stderr** names both files                                             |
| `compose.yaml` + `compose.override.yaml`               | merged, override wins per-key, new services added                                                       |
| `compose.yaml` + `docker-compose.override.yml`         | **cross-family override IS applied** (do not assume family pairing)                                     |
| `COMPOSE_FILE=a.yml:b.yml`                             | both loaded in order, later file wins per-key                                                           |
| `COMPOSE_PATH_SEPARATOR=";"`                           | honored                                                                                                 |
| `-f b.yml` with `COMPOSE_FILE=a.yml` set               | `-f` **replaces** COMPOSE_FILE entirely (a.yml fully ignored)                                           |
| run from `sub/deeper/` with compose file two levels up | **parent-directory walk-up** finds it; project dir, name, and `.env` all come from the file's directory |

### 2. Project name

Precedence (each proven to beat the next): `-p` flag > `COMPOSE_PROJECT_NAME` (real env) > `COMPOSE_PROJECT_NAME` (from `.env`) > top-level `name:` key > basename of project directory.

Normalization of the default: lowercased, invalid chars **stripped** (not replaced) — `My_Weird.Dir-NAME` → `my_weirddir-name` (`_` and `-` survive, `.` deleted).

### 3. `.env` semantics — the chicken-and-egg

- `.env` is loaded from the **project directory** (= directory of the first compose file, or `--project-directory` if given) — **not** the cwd.
- `.env` can set compose _behavior_ variables, not just interpolation values: `COMPOSE_PROJECT_NAME` **and `COMPOSE_FILE`** in `.env` are honored. **Consequence: the resolver must read the project-dir `.env` before it can even decide which files apply.** Mast's resolution order must be: locate project dir → parse `.env` → overlay real environment (real env wins) → resolve files/name/profiles.
- `--env-file custom.env` **replaces** the default `.env` (values from `.env` are no longer seen at all); repeated `--env-file` flags: later wins. `COMPOSE_ENV_FILES=a,b` env var is the same mechanism.
- For interpolation, OS environment beats `.env` per-variable (both stay active; precedence is per-key).
- Missing required variable (`${X:?msg}`): exit 1, single-line stderr `error while interpolating services.app.image: required variable MISSING is missing a value: set MISSING please` — parseable enough to surface with provenance.

### 4. Profiles

Services with `profiles:` are excluded from `config` output, `--services`, etc. unless activated by `--profile X` or `COMPOSE_PROFILES`. `docker compose config --profiles` lists all declared profiles — useful for UI. Profile activation is therefore **part of project identity**: the same files with different profiles is a different resolved model, which is why profiles live in ComposeInvocation.

### 5. `config` output is a resolved model, not a source representation

Confirmed normalizations: ports/volumes expanded to long form, relative paths absolutized, anchors & `<<:` merge keys resolved, interpolation applied, `name:` injected, key order differs between flag combinations (`--no-interpolate` even reorders keys and adds `create_host_path`). **Never diff `config` output against source text; never write it back.** Use it only for association, validation (`--quiet`: exit 0/1), and semantic display. Useful extra flags that exist in v5.3.1: `--variables` (M5 env editor), `--environment` (provenance debugging), `--hash "*"` (per-service config hash), `--images/--services/--profiles/--networks/--volumes`.

### 6. Resolution works offline

`docker compose config` succeeds with `DOCKER_HOST` pointing at a nonexistent socket. The M2 resolver keeps working while Docker is down — degraded observation, not degraded resolution.

### 7. Container label contract (association ground truth)

Labels observed on a compose-v5.3.1-created container:

```
com.docker.compose.project              = mast-spike-a-labels
com.docker.compose.project.config_files = /abs/path/docker-compose.yml   (comma-joined, absolute)
com.docker.compose.project.working_dir  = /abs/path
com.docker.compose.service              = laravel.test
com.docker.compose.container-number    = 1
com.docker.compose.config-hash         = <matches `config --hash` mechanism>
com.docker.compose.image / .oneoff / .depends_on / .version
```

Association strategy for M2: match container→project by `project` label; cross-check `config_files`/`working_dir` against the project's ComposeInvocation to detect "same name, different directory" collisions. `config-hash` is a cheap drift detector (running containers vs current files) — feed it into reconcile.

### 8. Sail mechanics (read from the real script)

1. `source ./.env` — or **`.env.$APP_ENV` instead, if `APP_ENV` is set in the caller's environment** while compose itself still reads `.env`; two different files can feed the two layers. Sourcing uses **bash semantics**, not compose's `.env` grammar — a compose-valid `.env` can break sail (future diagnostic candidate).
2. Exports with defaults: `APP_PORT=80`, `APP_SERVICE=laravel.test`, `APP_USER=sail`, `DB_PORT=3306`, **`WWWUSER=$UID`, `WWWGROUP=$(id -g)`**, `SAIL_FILES`, `SAIL_DOCKER_BINARY=docker` (podman override), share vars.
3. `SAIL_FILES` (colon-separated) → repeated `-f` flags in order; missing file is a hard error. Unset → default resolution.
4. No `-p`/`COMPOSE_PROJECT_NAME` handling — project identity is standard compose resolution.
5. **Hazard:** unless `SAIL_SKIP_CHECKS` is set, _every_ sail invocation runs `docker info`, then if `compose ps $APP_SERVICE` shows an exited app container it silently runs **`compose down`** ("Shutting down old Sail processes") before doing anything else.
6. Known verbs (`php`, `artisan`, `npm`, `shell`, …) become `compose exec …`; **unknown verbs pass through verbatim** to the compose binary with sail's env and `-f` flags — so `sail config`, `sail up`, `sail stop` are all plain compose commands run inside sail's environment.

### 9. The parity gap, proven

Same fixture, same directory:

| Invocation                                  | `WWWUSER`/`WWWGROUP` in resolved model |
| ------------------------------------------- | -------------------------------------- |
| `docker compose config`                     | `""` / `""` + stderr warnings          |
| `SAIL_SKIP_CHECKS=1 vendor/bin/sail config` | `1000` / `1000`                        |

Bare compose on a Sail project resolves (and would run) with empty build args/user mapping — the classic volume-permission failure. Terminal parity is not cosmetic.

`SAIL_SKIP_CHECKS=1 vendor/bin/sail config --format json` also works **offline** (finding 6 composes through the pass-through).

## Decision

**ComposeInvocation** (contract type, refined by these findings):

```
ComposeInvocation {
  runner:        Sail { script: PathBuf } | DockerCompose,   // per-project, detected
  project_dir:   PathBuf,          // anchor; Mast always sets cwd here (no walk-up reliance)
  files:         Vec<{ path, source: Flag | ComposeFileEnv | ComposeFileDotEnv | DefaultDiscovery | SailFiles }>,
  env_files:     Vec<{ path, source: Flag | Default | ComposeEnvFiles }>,
  project_name:  { value, source: Flag | Env | DotEnv | NameKey | DirBasename },
  profiles:      Vec<String>,
  context_env:   { DOCKER_CONTEXT?, DOCKER_HOST?, COMPOSE_PATH_SEPARATOR? },   // spike (b) refines
  extra_flags:   Vec<String>,
}
```

Every field carries **provenance** — the UI must be able to answer "why this file / this name".

**Sail-vs-compose rule:**

- **Sail project** (executable `vendor/bin/sail` present and project imported as Sail):
  - _Lifecycle_ (`up`, `stop`, `restart`, `down`): shell out to `vendor/bin/sail <verb> …` — argv array, `cwd = project_dir`, user's environment, **without** `SAIL_SKIP_CHECKS` (the auto-`down` hygiene is sail-normal terminal behavior; Mast preserves parity but surfaces "sail shut down stale containers" when the marker line appears on stderr).
  - _Read-only resolution_ (`config`, `config --quiet`, `ps`): `SAIL_SKIP_CHECKS=1 vendor/bin/sail <verb> …` — parity-by-construction (sail computes its own env), no docker-info gate, no auto-`down` side effect, works offline.
- **Plain compose project:** `docker compose` with explicit flags reconstructed from ComposeInvocation (`-f` per file, `-p`, `--profile`, `--env-file` as applicable).
- Mast never re-implements sail's env computation in Rust; where the resolved model must match sail, it runs sail. Static parsing of `SAIL_FILES`/defaults is used only to know _which files to watch/edit_, and is cross-checked at runtime against `com.docker.compose.project.config_files` labels.

**Resolver algorithm** (order forced by finding 3): project dir → parse `.env` (compose grammar) → overlay real env (wins per-key) → determine `COMPOSE_FILE`/`COMPOSE_PROJECT_NAME`/`COMPOSE_PROFILES`/`COMPOSE_ENV_FILES` → file discovery (both-family override matrix, warning surfaced when both base families exist) → name precedence chain → validate via `config --quiet` using the chosen runner.

## Consequences

- The **file watcher must watch `.env`** not just for value changes but because it can change _which compose files apply_ (`COMPOSE_FILE` in `.env`) → re-run full resolution, not just re-interpolation, on `.env` change.
- Resolved output is never diffed against source; the M5 write transaction's "targeted semantic verify" stands (whole-document equality remains unsound).
- M2 reconcile gets a free drift signal: label `config-hash` vs current `config --hash "*"`.
- New diagnostic candidates for M7: bash-vs-compose `.env` grammar divergence (sail would crash); `APP_ENV` set while `.env.$APP_ENV` exists (sail and compose reading different files); both base families present (compose warns, users miss stderr).
- Open for spike (b): context/rootless/`DOCKER_HOST` interaction with all of the above; podman (`SAIL_DOCKER_BINARY`) explicitly out of scope for now.
