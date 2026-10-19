# ADR-0012: Agents — worktrees, the processes they start, and cleaning up after them

- **Status:** accepted
- **Date:** 2026-10-15
- **Applies to:** `mast-engine` (`agents`, the reconcile pass, `Action::StopHostRun`, the worktree actions), `mast-contract` (`Worktree`, `HostRun`, `AgentSession`), `clients/desktop-vue` (`AgentsPane.vue`, `ProjectAgents.vue`, `lib/agents.ts`), `mast-cli` (`mast agents`, `mast_agents`)

## Question

Coding agents work in parallel by giving each task its own git worktree,
and each one runs things from there: `sail up` brings up a whole second
stack, `npm run dev` a Vite on the host, a headless browser for a test.
They start them in the background and often finish without stopping
them. ADR-0011 made what runs _inside_ the app container visible, but the
rest stayed invisible: the worktrees themselves, the stacks started from
them, the host processes working in them, and the agent sessions behind
all of it. A real machine had an agent's worktree whose directory was
already gone, still listed by git, and a headless Chrome of twenty
processes an agent's browser tool had left running in the project's
checkout. What should Mast show, how does it know, and what may it
clean up?

## Decision

**Worktrees come from git's own admin directory, wherever they live.**
Each pass reads `<project>/.git/worktrees/*`: `gitdir` names the checkout
(its parent directory), `HEAD` the branch or detached commit, `locked`
whether it is pinned. This finds Claude Code's `.claude/worktrees/<name>`,
Codex's and Cursor's directories under the home folder, and anything a
person made with `git worktree add`, with no list of locations to keep up
to date (lerd does the same). A checkout whose directory is gone is
listed as _missing_ until it is pruned. The path is then compared with
known tools' conventions only to _label_ the worktree ("under
.claude/worktrees, where Claude Code makes them") — a convention is not
proof of who made it, so the UI words it as a location.

For each worktree that exists, `git status --porcelain=v2` counts the
files with uncommitted changes and `git rev-list --count` the commits the
project's own checkout does not have. Those two numbers are what decide
whether removing it loses anything: the branch survives
`git worktree remove`, uncommitted files do not, and a detached HEAD's
commits are reachable from nothing once the worktree goes.

The file watcher adds `<project>/.git` and `<project>/.git/worktrees`
(non-recursively, filtered to the `worktrees` entries), so a worktree
appears or disappears in the next pass rather than up to twenty seconds
later.

**A worktree's containers are the worktree's, not the project's.**
Containers whose compose working directory or config files lie inside a
worktree are attributed to it and shown with it. The project's own
service association and its process-inventory pick both exclude them. A
nested worktree's config files sit under the project root, so before this
a `sail up` in `.claude/worktrees/foo` could be taken for the project's
own app container by the inventory pass; and a worktree whose copied
`.env` pins the same `COMPOSE_PROJECT_NAME` is, to compose, the same
project — its `sail up` recreates the project's containers from the
worktree's checkout. That case gets an advisory on the project, because
every chip stays green while the browser serves another branch.

**Host processes come from one `/proc` walk per pass, Linux only.** The
engine reads every process it can see — its own user's — with parent,
start time, resident set, command line and working directory. Two
things are drawn from it:

- **Agent sessions:** a process whose _program_ is a known coding-agent
  CLI (Claude Code, Codex, Gemini CLI, Aider, opencode, Cursor's agent,
  Amp, …), read from the program position as ADR-0011 reads artisan,
  whose working directory is a project's checkout or one of its
  worktrees. A wrapper and the binary it launches are one session.
- **Host runs:** process trees working in a project or its worktrees
  that do something worth knowing about — a Vite, an artisan daemon, a
  dev script (`npm run dev`, `composer run dev`), a `docker compose up`
  or `exec` (`sail …`), a headless browser. Each is either still owned by
  a live agent session (it descends from one) or **orphaned**: its parent
  is init or the user's service manager (`systemd --user`), which adopts
  whatever a finished session leaves behind.

Only recognised kinds are listed. An editor, a file manager or a browser
window opened on the project is not a leftover, and listing every process
with the project as its working directory would make the list useless.
Nothing is attributed by environment variables, for the reason ADR-0011
gives, and a session's own children that are not one of those kinds (its
MCP servers, its shell tool calls) are not listed.

**Stopping is by PID and start time, then the whole tree.** A host stop
re-reads the tree from `/proc`, refuses if the root's start time no
longer matches, sends SIGTERM to every member, waits up to ten seconds,
and SIGKILLs what is left — the escalation Mast's own commands already
get when something they started outlives the TERM. Only a tree the
project's listing shows can be stopped this way, and never Mast itself or
an ancestor of it. An agent session itself is never offered for stopping:
it is a conversation someone may still be having in a terminal.

**Removing a worktree stops what works in it first.** `RemoveWorktree`
takes a fresh look at the process table, refuses while an agent session
works in the worktree (pulling the directory out from under a running
agent breaks it without telling it), stops every listed host run working
there — a Vite still writing its cache makes `git worktree remove` fail —
removes the containers started from it by id (named volumes are kept,
and a compose project the worktree started alone loses its networks),
and then runs `git worktree remove`. Unlisted processes, such as a shell
someone left in the directory, are not touched. A worktree with
uncommitted changes, or a detached HEAD with commits no branch keeps, is
refused unless the client passes `force`; the desktop states what will be
lost and asks for a separate tick before sending it. Missing worktrees
are cleared with `git worktree prune`, which only forgets admin entries
whose checkout is already gone.

**Clean up is a list, not a button that guesses.** The Agents view's
Clean up offers each leftover with a checkbox — orphaned host runs,
container runs started outside Mast or orphaned (ADR-0011), stacks
started from worktrees, missing worktrees — and runs exactly what stays
ticked. Removing a worktree is never part of it; that stays a per-worktree
decision with its own confirmation.

**One section, in two places.** The Agents page lists every project with
anything going on; each project's card shows its own copy of the same
section (`ProjectAgents.vue`), because the card is where something wrong
with a project is looked for. The card's copy leaves out the app
container's process trees, which the card already lists in its own row,
and its Clean up is scoped to that project.

**Agents can see this too, but not act on it.** (Since ADR-0013 the
CLI — a person's tool — can stop listed runs and clean up; MCP stays
read-only.) `mast agents` and the read-only MCP tool `mast_agents` report
the same view, so an agent can
check what it — or the one before it — left running. The MCP server's
`initialize` instructions tell agents to check it before starting a dev
server or `sail up`, and again before finishing: clients that load tool
schemas lazily show an agent only the tool names, which say nothing about
when to call them. Stopping and removing stay in the app, as ADR-0008
keeps repairs there.

## Consequences

Git reads made for display set `GIT_OPTIONAL_LOCKS=0` — the existing
branch-and-dirty read included — so a `git status` from Mast never takes
`index.lock` while an agent in the same checkout is committing.

The project summary carries three more lists (`worktrees`,
`agent_sessions`, `host_runs`), all `#[serde(default)]`, so older clients
ignore them. Memory is rounded as ADR-0011 rounds it, and sessions carry
no process count, so a busy agent does not churn the summary on every
pass.

Each worktree costs two `git` subprocesses per pass. Projects rarely
have more than a handful; if that changes, caching by HEAD and index
modification time is the obvious next step.

## Not decided here

Host processes on macOS and Windows. macOS can list processes with `ps`,
but a working directory per process needs `lsof` or `proc_pidinfo`;
Windows needs the PEB. Until then those platforms show worktrees and
their containers, and say host processes are not watched there.

Linking a host `docker compose exec` to the container tree it created —
which would let ADR-0011's "outside Mast" say "started by Claude Code in
kusina.design". The host side is now visible, and command line plus start
time is strong evidence, but it is still a match rather than a fact.
