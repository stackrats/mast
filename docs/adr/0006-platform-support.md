# ADR-0006: Cross-platform support architecture

Status: accepted (2026-08-25, after the v0.3.0 field-fix wave — PRs #8–#26)

Linux, macOS and Windows are all supported. This ADR records where platform
knowledge is allowed to live and the rules that two weeks of field-testing
real machines burned in. New platform behavior goes into one of the existing
seats below; scattering `cfg!` through business logic is the failure mode
this document exists to prevent.

## Where platform knowledge lives

| Concern                                                                           | Single home                                                                                                                                                                                                                                                                                    |
| --------------------------------------------------------------------------------- | ---------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------- |
| Spawning subprocesses (console suppression, docker CLI discovery, PATH fallbacks) | `mast-docker/src/command.rs` — every captured spawn funnels through `run_command`/`run_streaming`; `spawn_detached` takes a `console` flag only the terminal launch sets. The one documented exception: the per-reconcile git probe in `effects.rs`, which carries its own `CREATE_NO_WINDOW`. |
| Which runner drives a project (`sail` script vs `docker compose`)                 | the resolver in `mast-compose` — the sail wrapper is a bash script, so the Sail runner exists only on unix. Every consumer (lifecycle, diagnostics, php switch, db repair) branches on the resolved runner, never on the OS.                                                                   |
| `sail …` user-command translation on Windows                                      | `project_ops::sail_fallback_argv`                                                                                                                                                                                                                                                              |
| Desktop tool launching (opener, editors, terminals, path hygiene)                 | `mast-engine/src/integrations.rs`                                                                                                                                                                                                                                                              |
| Elevation (hosts file, CA trust)                                                  | `mast-engine/src/proxy.rs` — polkit (`pkexec`) on Linux, `osascript` administrator on macOS, UAC via a generated `.cmd` + `Start-Process -Verb RunAs` on Windows. Callers use `privileged_shell_argv` / `windows_elevated_cmd` and `elevation_note()`.                                         |
| Path spelling (`\\?\` verbatim stripping)                                         | `mast_compose::strip_verbatim` at invocation resolution; `mast-project` strips on import/scan/load (deliberate small duplication — the crates do not depend on each other); `integrations::argv_path` for argv-bound paths.                                                                    |
| Wizard birth defects (ports/URL, writable dirs, in-container PHP user on Windows) | the create flow in `project_ops` — `reconcile_bootstrap_url` (pure, tested) and `force_php_root_for_windows`.                                                                                                                                                                                  |

## Rules the field enforced

1. **Probe reality, not metadata.** `[ -w ]` lied about a bind mount; only an
   actual write told the truth. Prefer doing the thing (touch a file, connect
   the port) over reading a bit that claims it would work.
2. **Probe as the right user.** Root's success proves nothing about the user
   PHP runs as — and "the right user" is what the runtime resolves
   (`SUPERVISOR_PHP_USER`, default `sail`), not what config claims
   (`WWWUSER` after a failed `usermod`).
3. **Container-truth outranks file-truth until a recreate.** A container
   keeps the environment it was created with; editing `.env` or the compose
   file changes nothing running. Repairs that flip container env must
   recreate, and their no-op checks must ask the container.
4. **Config must reach its consumer.** Sail's compose stub forwards
   `WWWUSER` but not `SUPERVISOR_PHP_USER`; an `.env`-only write was a
   silent no-op. Trace the whole path from file to process before calling a
   knob wired.
5. **Exit codes lie across elevation boundaries.** After a UAC/polkit step,
   verify the effect (re-read the hosts file), don't trust the status.
6. **Both address families.** Docker can publish v6-only; every host-port
   probe connects `127.0.0.1` and `::1`.
7. **Fixes must target the causing project.** A port-conflict repair offered
   on the project that _noticed_ the conflict repairs nothing; find the
   holder.
8. **`cfg!(...)` runtime gates over `#[cfg]` where possible** so every
   platform's code cross-compiles from Linux (the zig cross-check recipe)
   and dead branches stay visible to clippy.

## Verification

- CI compiles **and runs the test suite** on Linux, macOS and Windows on
  every push (docker-gated e2e suites skip cleanly where the runner has no
  usable docker).
- Platform-pure logic (argv shapes, path stripping, signature parsing,
  decision tables) is unit-tested and runs everywhere.
- The repair _apply_ arms and container probes are exercised by the
  docker-gated suites and field walkthroughs; growing a dedicated
  docker-gated integration suite for repairs is the standing next
  investment.

## Not yet supported

Projects living inside WSL2 (`\\wsl$\…`): the design direction is a
`wsl.exe -d <distro>`-prefixed invocation (which also restores the real sail
runner inside the distro) plus path translation for container association —
deliberately parked until native-Windows support had proven itself.
