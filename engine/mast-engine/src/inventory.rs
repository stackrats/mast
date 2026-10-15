//! What is running inside a project's app container, and who started it.
//!
//! Container-level observation sees one container; everything that matters
//! for a Laravel app runs as processes inside it, and each `docker exec` —
//! Mast's commands, `sail artisan …` from a terminal, an agent's background
//! job — leaves its own process tree there. Killing the client on the host
//! never ends that tree, so leftovers accumulate: two Horizon masters, two
//! schedulers running every task twice, a Vite whose exit deleted another's
//! `public/hot`. This module turns one `/proc` walk into those trees, with an
//! owner for each, so the card can show them and offer Stop.
//!
//! Nothing here reads `/proc/<pid>/environ`: Sail's PHP processes are
//! non-dumpable (its PHP binary carries a file capability), and their
//! environment is unreadable to every user without CAP_SYS_PTRACE. Ownership
//! comes from the run records Mast's commands leave in `/tmp/mast-runs`
//! (see `custom_command`), from Mast's own registry of the processes it
//! supervises, and from the process tree.

use std::collections::{BTreeSet, HashMap};

use mast_contract::{Advisory, AdvisoryAction, ContainerRun, ContainerRunOwner};

/// One `/proc` walk, printed as lines the parser below reads:
///
/// - `H <own pid> <clock ticks per second> <uptime seconds> <page size>`
/// - `P <pid> <ppid> <start ticks> <rss pages> <command line>`
/// - `R <run id> <pid> <start ticks> <command name>` per run record
///
/// POSIX sh with nothing beyond `tr`, `cut` and `getconf` (whose absence
/// falls back to Linux's defaults), so it runs in busybox images too. The
/// stat line is split after its last `) `, because a process's name may hold
/// spaces and parentheses; from there field N of stat is `$((N-2))`.
pub(crate) const SCAN_SCRIPT: &str = r#"printf 'H %s %s %s %s\n' "$$" "$(getconf CLK_TCK 2>/dev/null || echo 100)" "$(cut -d' ' -f1 /proc/uptime)" "$(getconf PAGESIZE 2>/dev/null || echo 4096)"
for d in /proc/[0-9]*; do
    { read -r stat < "$d/stat"; } 2>/dev/null || continue
    rest=${stat##*) }; set -- $rest
    case "$1" in Z|X) continue;; esac
    printf 'P %s %s %s %s %s\n' "${d#/proc/}" "$2" "${20}" "${22}" "$(tr '\0\n' '  ' 2>/dev/null < "$d/cmdline")"
done
for r in /tmp/mast-runs/*; do
    { read -r pid start name < "$r"; } 2>/dev/null || continue
    printf 'R %s %s %s %s\n' "${r##*/}" "$pid" "$start" "$name"
done"#;

#[derive(Debug, Clone, PartialEq)]
pub(crate) struct Proc {
    pub pid: u32,
    pub ppid: u32,
    pub start: u64,
    pub rss_pages: u64,
    pub cmdline: String,
}

#[derive(Debug, Clone, PartialEq)]
pub(crate) struct Record {
    pub run: String,
    pub pid: u32,
    pub start: u64,
    pub name: String,
}

#[derive(Debug, Clone, PartialEq)]
pub(crate) struct Scan {
    pub own_pid: u32,
    pub clk_tck: u64,
    pub uptime_secs: f64,
    pub page_size: u64,
    pub procs: Vec<Proc>,
    pub records: Vec<Record>,
}

pub(crate) fn parse(out: &str) -> Option<Scan> {
    let mut scan: Option<Scan> = None;
    let mut procs = Vec::new();
    let mut records = Vec::new();
    for line in out.lines() {
        if let Some(rest) = line.strip_prefix("H ") {
            let f: Vec<&str> = rest.split_whitespace().collect();
            if f.len() < 4 {
                continue;
            }
            scan = Some(Scan {
                own_pid: f[0].parse().ok()?,
                clk_tck: f[1].parse().ok().filter(|t| *t > 0).unwrap_or(100),
                uptime_secs: f[2].parse().ok()?,
                page_size: f[3].parse().ok().filter(|p| *p > 0).unwrap_or(4096),
                procs: Vec::new(),
                records: Vec::new(),
            });
        } else if let Some(rest) = line.strip_prefix("P ") {
            let mut f = rest.splitn(5, ' ');
            let (Some(pid), Some(ppid), Some(start), Some(rss)) =
                (f.next(), f.next(), f.next(), f.next())
            else {
                continue;
            };
            let (Ok(pid), Ok(ppid), Ok(start), Ok(rss_pages)) =
                (pid.parse(), ppid.parse(), start.parse(), rss.parse())
            else {
                continue;
            };
            let cmdline = f.next().unwrap_or("").trim().to_string();
            procs.push(Proc {
                pid,
                ppid,
                start,
                rss_pages,
                cmdline,
            });
        } else if let Some(rest) = line.strip_prefix("R ") {
            let f: Vec<&str> = rest.split_whitespace().collect();
            if f.len() < 3 {
                continue;
            }
            let (Ok(pid), Ok(start)) = (f[1].parse(), f[2].parse()) else {
                continue;
            };
            records.push(Record {
                run: f[0].to_string(),
                pid,
                start,
                name: f.get(3).copied().unwrap_or("command").to_string(),
            });
        }
    }
    let mut scan = scan?;
    scan.procs = procs;
    scan.records = records;
    Some(scan)
}

/// What a process is, in the terms the card uses, or None for the plumbing
/// around it (shells, package-manager wrappers, Horizon's own supervisors).
pub(crate) fn kind_of(cmdline: &str) -> Option<&'static str> {
    let words = program_words(cmdline);
    if let Some(verb) = artisan_verb(&words) {
        return match verb {
            "horizon" => Some("Horizon"),
            "queue:work" | "queue:listen" => Some("Queue worker"),
            "schedule:work" => Some("Scheduler"),
            "reverb:start" => Some("Reverb"),
            "pail" => Some("Pail"),
            "serve" => Some("App server"),
            v if v.starts_with("octane:start") || v == "octane:frankenphp" => Some("Octane"),
            _ => None,
        };
    }
    // The dev server itself, not the wrappers around it (`npm run dev`,
    // `vp dev`) and not a build watcher.
    let is_node = words
        .first()
        .is_some_and(|w| *w == "node" || w.ends_with("/node"));
    let entry = words.iter().position(|w| {
        w.ends_with("/vite/bin/vite.js")
            || w.ends_with("/.bin/vite")
            || w.ends_with("/vite/node/cli.js")
    });
    if is_node
        && let Some(at) = entry
        && !words[at + 1..]
            .iter()
            .any(|w| matches!(*w, "build" | "optimize"))
    {
        return Some("Vite");
    }
    None
}

/// A command line's words, without a leading `sh -c`: for a shell wrapper,
/// what matters is the script it runs.
pub(crate) fn program_words(cmdline: &str) -> Vec<&str> {
    let words: Vec<&str> = cmdline.split_whitespace().collect();
    match words.as_slice() {
        [shell, "-c", rest @ ..] if shell.ends_with("sh") => rest.to_vec(),
        _ => words,
    }
}

/// The artisan command a process runs, when artisan is the program it runs:
/// `php [-d …] artisan <verb>`, or the artisan script itself. Not merely the
/// word anywhere — a dev-stack runner carries its panes' `php artisan …`
/// lines as arguments without being any of them.
fn artisan_verb<'a>(words: &[&'a str]) -> Option<&'a str> {
    let is_artisan = |w: &str| w == "artisan" || w.ends_with("/artisan");
    let program = words.first()?.rsplit('/').next()?;
    if is_artisan(program) {
        return words.get(1).copied();
    }
    if !program.starts_with("php") {
        return None;
    }
    let mut rest = words[1..].iter();
    while let Some(word) = rest.next() {
        if *word == "-d" || *word == "-c" {
            rest.next();
        } else if word.starts_with('-') {
            continue;
        } else {
            return is_artisan(word).then(|| rest.next().copied()).flatten();
        }
    }
    None
}

/// The process ids [`mast_contract::ProcessState`] uses for each daemon kind.
pub(crate) fn process_id_of(kind: &str) -> Option<&'static str> {
    match kind {
        "Horizon" => Some("horizon"),
        "Queue worker" => Some("queue"),
        "Scheduler" => Some("schedule"),
        "Reverb" => Some("reverb"),
        _ => None,
    }
}

/// The daemon kind a built-in process id runs, the inverse of
/// [`process_id_of`].
pub(crate) fn kind_of_process_id(id: &str) -> Option<&'static str> {
    ["Horizon", "Queue worker", "Scheduler", "Reverb"]
        .into_iter()
        .find(|kind| process_id_of(kind) == Some(id))
}

/// Mast's view of what it is running in this project, for attribution.
#[derive(Debug, Default, Clone)]
pub(crate) struct Live {
    /// Run id → command name, for commands this session started.
    pub commands: HashMap<String, String>,
    /// Built-in processes this session supervises, by kind ("Horizon"), with
    /// the title to show. They start as `sail artisan …` for terminal parity
    /// and carry no record, so they are matched by what they run.
    pub processes: HashMap<&'static str, String>,
}

/// One process tree: the process whose parent is outside the container (a
/// `docker exec`) or is init, and everything beneath it.
struct Tree<'a> {
    root: &'a Proc,
    members: Vec<&'a Proc>,
    /// The daemon kinds it runs, each once however many processes it takes.
    kinds: BTreeSet<&'static str>,
    /// Launched by the container's own startup — init's children at boot,
    /// or every child of an init that is not a supervisor.
    container_own: bool,
}

/// How long after the container's init a child may start and still count as
/// part of the container's own startup (Sail's supervised `artisan serve`
/// starts about a second and a half in).
const STARTUP_GRACE_SECS: u64 = 10;

/// Group a scan into process trees. Left out: init itself, and Mast's own
/// passes over `/proc` (this scan, a stop, or another Mast's scan caught
/// mid-flight) and processes whose command line is not readable yet — they
/// are seconds-long glimpses, not something running.
fn trees(scan: &Scan) -> Vec<Tree<'_>> {
    let by_pid: HashMap<u32, &Proc> = scan.procs.iter().map(|p| (p.pid, p)).collect();
    let init = by_pid.get(&1).copied();
    let init_is_supervisor = init.is_some_and(|p| p.cmdline.contains("supervisord"));
    // Each process's root: the ancestor whose parent is outside the
    // container (ppid 0) or is init (ppid 1).
    let root_of = |pid: u32| -> u32 {
        let mut current = pid;
        for _ in 0..256 {
            match by_pid.get(&current) {
                Some(p) if p.ppid > 1 && by_pid.contains_key(&p.ppid) => current = p.ppid,
                _ => break,
            }
        }
        current
    };
    let mut trees: Vec<Tree> = Vec::new();
    let mut index: HashMap<u32, usize> = HashMap::new();
    for p in &scan.procs {
        if p.pid == 1 {
            continue;
        }
        let Some(root) = by_pid.get(&root_of(p.pid)).copied() else {
            continue;
        };
        let at = *index.entry(root.pid).or_insert_with(|| {
            // Started by init: the container's own unless it is a leftover
            // init merely adopted. Sail's supervisor launches its one program
            // at boot with `-d variables_order=EGPCS`; another init's children
            // are the container's command itself.
            let at_boot = init.is_some_and(|init| {
                root.start.saturating_sub(init.start) <= STARTUP_GRACE_SECS * scan.clk_tck
            });
            let container_own = root.ppid == 1
                && (!init_is_supervisor
                    || at_boot
                    || root.cmdline.contains("variables_order=EGPCS"));
            trees.push(Tree {
                root,
                members: Vec::new(),
                kinds: BTreeSet::new(),
                container_own,
            });
            trees.len() - 1
        });
        trees[at].members.push(p);
        if let Some(kind) = kind_of(&p.cmdline) {
            trees[at].kinds.insert(kind);
        }
    }
    trees.retain(|tree| {
        tree.root.pid != scan.own_pid
            && !tree.root.cmdline.trim().is_empty()
            && !tree.root.cmdline.contains("/proc/[0-9]*")
    });
    trees
}

/// How many trees run each daemon kind, the container's own included: a
/// Horizon its supervisor runs and another started from a terminal are two.
pub(crate) fn kind_counts(scan: &Scan) -> HashMap<&'static str, u32> {
    let mut counts = HashMap::new();
    for tree in trees(scan) {
        for kind in tree.kinds {
            *counts.entry(kind).or_insert(0) += 1;
        }
    }
    counts
}

/// The trees running in the container beyond its own startup, and who
/// started each.
pub(crate) fn runs(scan: &Scan, live: &Live, now_unix: u64) -> Vec<ContainerRun> {
    let boot_unix = now_unix as f64 - scan.uptime_secs;
    let mut out = Vec::new();
    for tree in trees(scan) {
        if tree.container_own {
            continue;
        }
        let root = tree.root;
        let owner = if root.ppid == 1 {
            ContainerRunOwner::Orphaned
        } else if let Some(record) = scan
            .records
            .iter()
            .find(|r| r.pid == root.pid && r.start == root.start)
        {
            match live.commands.get(&record.run) {
                Some(command) => ContainerRunOwner::Mast {
                    command: command.clone(),
                },
                None => ContainerRunOwner::EarlierMast {
                    command: record.name.clone(),
                },
            }
        } else {
            ContainerRunOwner::Outside
        };
        let title = match &owner {
            ContainerRunOwner::Mast { command } | ContainerRunOwner::EarlierMast { command } => {
                command.clone()
            }
            _ => short_command(&root.cmdline),
        };
        let rss: u64 = tree.members.iter().map(|p| p.rss_pages).sum();
        out.push(ContainerRun {
            pid: root.pid,
            start_ticks: root.start,
            owner,
            title,
            command_line: root.cmdline.clone(),
            started_at_unix: (boot_unix + root.start as f64 / scan.clk_tck as f64).max(0.0) as u64,
            memory_bytes: round_memory(rss * scan.page_size),
            process_count: tree.members.len() as u32,
            kinds: tree.kinds.into_iter().map(String::from).collect(),
        });
    }
    attribute_builtin_processes(&mut out, live);
    out.sort_by_key(|run| run.started_at_unix);
    out
}

/// A built-in process Mast supervises starts as `sail artisan <daemon>` and
/// has no record, so it reads as started outside Mast. Claim it back: for
/// each kind Mast is running, the newest tree that is exactly that daemon is
/// Mast's (a supervised restart replaces the old one, so the newest is the
/// live copy).
fn attribute_builtin_processes(runs: &mut [ContainerRun], live: &Live) {
    for (kind, title) in &live.processes {
        let newest = runs
            .iter_mut()
            .filter(|run| {
                run.owner == ContainerRunOwner::Outside
                    && run.kinds.len() == 1
                    && run.kinds[0] == *kind
            })
            .max_by_key(|run| run.start_ticks);
        if let Some(run) = newest {
            run.owner = ContainerRunOwner::Mast {
                command: title.clone(),
            };
            run.title = title.clone();
        }
    }
}

/// A command line short enough for a chip: the artisan verb, or the program
/// and its first argument.
pub(crate) fn short_command(cmdline: &str) -> String {
    let words = program_words(cmdline);
    if let Some(verb) = artisan_verb(&words) {
        return format!("artisan {verb}");
    }
    // Stack runners name themselves: `multiplex --title 'artisan dev · App'`.
    if let Some(title) = cmdline
        .split("--title '")
        .nth(1)
        .and_then(|rest| rest.split('\'').next())
        && !title.trim().is_empty()
    {
        return title.trim().to_string();
    }
    let program = words
        .first()
        .map(|w| w.rsplit('/').next().unwrap_or(w))
        .unwrap_or("");
    let text = std::iter::once(program)
        .chain(words.iter().skip(1).take(2).copied())
        .collect::<Vec<_>>()
        .join(" ");
    if text.chars().count() > 48 {
        text.chars().take(47).collect::<String>() + "…"
    } else {
        text
    }
}

/// Two significant figures, so a chip's number does not churn the card on
/// every scan as a process's resident set breathes.
pub(crate) fn round_memory(bytes: u64) -> u64 {
    if bytes < 100 {
        return bytes;
    }
    let digits = bytes.ilog10();
    let unit = 10u64.pow(digits - 1);
    (bytes + unit / 2) / unit * unit
}

/// Kinds that must run once: a second Horizon master works the same queues
/// beside the first, a second scheduler runs every task twice, a second
/// Reverb or Vite cannot bind and exits — Vite deleting `public/hot` as it
/// goes.
const SINGLE: &[(&str, &str)] = &[
    ("Horizon", "every job is picked up by two masters' workers"),
    ("Scheduler", "scheduled tasks run twice"),
    ("Reverb", "only one can hold the port"),
    ("Vite", "only one can serve the page's assets"),
    ("Pail", "log lines are tailed twice"),
];

/// Card advisories for daemons running more than once, each offering to stop
/// the copies Mast is not running. Kinds duplicated across the same copies —
/// two of one dev stack double its Horizon, scheduler and Pail together — are
/// one advisory, not one per kind.
pub(crate) fn duplicate_advisories(
    runs: &[ContainerRun],
    counts: &HashMap<&'static str, u32>,
) -> Vec<Advisory> {
    // The copies listed as runs, how many more the container's own startup
    // runs, and the kinds (with what doubling each costs) they share.
    type Group<'a> = (
        Vec<&'a ContainerRun>,
        u32,
        Vec<(&'static str, &'static str)>,
    );
    let mut groups: Vec<Group> = Vec::new();
    for (kind, consequence) in SINGLE {
        let total = counts.get(kind).copied().unwrap_or(0);
        if total < 2 {
            continue;
        }
        let holders: Vec<&ContainerRun> = runs
            .iter()
            .filter(|run| run.kinds.iter().any(|k| k == kind))
            .collect();
        let container_copies = total.saturating_sub(holders.len() as u32);
        let same = |group: &Group| {
            group.1 == container_copies
                && group
                    .0
                    .iter()
                    .map(|r| r.pid)
                    .eq(holders.iter().map(|r| r.pid))
        };
        match groups.iter_mut().find(|group| same(group)) {
            Some(group) => group.2.push((kind, consequence)),
            None => groups.push((holders, container_copies, vec![(kind, consequence)])),
        }
    }
    groups
        .into_iter()
        .map(|(holders, container_copies, kinds)| {
            let times = match holders.len() as u32 + container_copies {
                2 => "twice".to_string(),
                n => format!("{n} times"),
            };
            let names: Vec<&str> = kinds.iter().map(|(kind, _)| *kind).collect();
            let consequences: Vec<&str> = kinds.iter().map(|(_, c)| *c).collect();
            let (subject, verb) = match names.as_slice() {
                [one] => (one.to_string(), "is"),
                _ => (list(&names), "are each"),
            };
            let mut described: Vec<String> = holders.iter().map(|run| describe(run)).collect();
            if container_copies > 0 {
                described.insert(0, "the container's own startup".into());
            }
            Advisory {
                message: format!(
                    "{subject} {verb} running {times} in the app container, so {}. Running: {}.",
                    list(&consequences),
                    described.join("; ")
                ),
                actions: holders
                    .iter()
                    .filter(|run| !matches!(run.owner, ContainerRunOwner::Mast { .. }))
                    .map(|run| AdvisoryAction::StopContainerRun {
                        label: format!("Stop {}", describe(run)),
                        pid: run.pid,
                        start_ticks: run.start_ticks,
                    })
                    .collect(),
            }
        })
        .collect()
}

/// "a", "a and b", "a, b and c".
fn list(items: &[&str]) -> String {
    match items {
        [] => String::new(),
        [one] => one.to_string(),
        [rest @ .., last] => format!("{} and {last}", rest.join(", ")),
    }
}

/// What decides where a page's scripts and styles come from: `public/hot`
/// (while it exists, `@vite` points at the dev server it names) against the
/// port this project's Vite is configured for.
#[derive(Debug, Clone, PartialEq)]
pub(crate) struct ViteFacts {
    pub hot: Option<mast_laravel::vite::HotFile>,
    /// `VITE_PORT` from `.env`, or Vite's default.
    pub vite_port: u16,
    /// Whether anything answers at the hot file's address; None when there
    /// is no hot file or its host is not this machine.
    pub hot_listening: Option<bool>,
}

/// Read a project's Vite facts. Blocking: file reads and, when there is a
/// hot file, a quarter-second connect to the address it names.
pub(crate) fn read_vite_facts(dir: &std::path::Path) -> ViteFacts {
    let env = mast_compose::parse_env_file(&dir.join(".env"));
    let vite_port = env
        .get("VITE_PORT")
        .and_then(|p| p.trim().parse().ok())
        .unwrap_or(5173);
    let hot = std::fs::read_to_string(dir.join("public/hot"))
        .ok()
        .and_then(|contents| mast_laravel::vite::parse_hot_file(&contents));
    let hot_listening = hot
        .as_ref()
        .and_then(crate::diagnostics::dev_server_listening);
    ViteFacts {
        hot,
        vite_port,
        hot_listening,
    }
}

/// Card advisories for pages that are not getting their assets from this
/// project's Vite — the failure that otherwise only shows as a browser
/// serving yesterday's CSS while every chip is green.
pub(crate) fn vite_advisories(facts: &ViteFacts, runs: &[ContainerRun]) -> Vec<Advisory> {
    let vite_running = runs.iter().any(|run| run.kinds.iter().any(|k| k == "Vite"));
    // The way to put things right is restarting the dev server Mast runs:
    // starting up, Vite writes public/hot again.
    let restart: Vec<AdvisoryAction> = runs
        .iter()
        .filter(|run| run.kinds.iter().any(|k| k == "Vite"))
        .find_map(|run| match &run.owner {
            ContainerRunOwner::Mast { command } => Some(AdvisoryAction::RestartCommand {
                label: format!("Restart {command}"),
                name: command.clone(),
            }),
            _ => None,
        })
        .into_iter()
        .collect();
    let remove = || AdvisoryAction::ApplyRepair {
        label: "Remove public/hot".into(),
        repair: mast_diagnostics::REPAIR_REMOVE_HOT.into(),
        arg: None,
    };
    let Some(hot) = &facts.hot else {
        if !vite_running {
            return Vec::new();
        }
        return vec![Advisory {
            message: "Vite is running in the app container, but public/hot is missing, so \
                      pages load the last build instead of live assets. Another Vite deletes \
                      that file when it exits; restarting the dev server writes it again."
                .into(),
            actions: restart,
        }];
    };
    let stale = facts.hot_listening == Some(false);
    if hot.port != facts.vite_port {
        if stale {
            return vec![Advisory {
                message: format!(
                    "public/hot points pages at {}, but nothing is listening there, so their \
                     scripts and styles fail to load.",
                    hot.url
                ),
                actions: vec![remove()],
            }];
        }
        let mut actions = restart;
        actions.push(remove());
        return vec![Advisory {
            message: format!(
                "Pages load scripts and styles from a dev server at {}, not from this \
                 project's Vite on port {} — another dev server wrote public/hot. When it \
                 exits it deletes the file and pages fall back to the last build.",
                hot.url, facts.vite_port
            ),
            actions,
        }];
    }
    if stale && !vite_running {
        return vec![Advisory {
            message: format!(
                "public/hot points pages at {}, but no Vite is running, so their scripts \
                 and styles fail to load.",
                hot.url
            ),
            actions: vec![remove()],
        }];
    }
    Vec::new()
}

/// "dev (started by an earlier Mast session)" and the like.
pub(crate) fn describe(run: &ContainerRun) -> String {
    match &run.owner {
        ContainerRunOwner::Mast { command } => format!("{command} (Mast)"),
        ContainerRunOwner::EarlierMast { command } => {
            format!("{command} (left by an earlier Mast session)")
        }
        ContainerRunOwner::Outside => format!("{} (started outside Mast)", run.title),
        ContainerRunOwner::Orphaned => format!("{} (orphaned)", run.title),
    }
}

/// SIGTERM a process tree in the container, root first identified by PID and
/// start time, then wait up to ten seconds for all of it to exit. Run as
/// root: signalling needs only CAP_KILL, which Docker grants, and the tree is
/// read from `/proc/<pid>/stat`, which anyone can read.
pub(crate) fn stop_tree_script(pid: u32, start_ticks: u64) -> String {
    format!(
        r#"root={pid}; start={start_ticks}; tracked=''; stopped=''; remaining=10
if {{ read -r stat < "/proc/$root/stat"; }} 2>/dev/null; then
    rest=${{stat##*) }}; set -- $rest
    [ "${{20}}" = "$start" ] && tracked=$root
fi
[ -n "$tracked" ] || exit 0
while :; do
    table=''
    for path in /proc/[0-9]*/stat; do
        pid=${{path#/proc/}}; pid=${{pid%/stat}}
        [ "$pid" = "$$" ] && continue
        {{ read -r stat < "$path"; }} 2>/dev/null || continue
        rest=${{stat##*) }}; set -- $rest
        case "$1" in Z|X) continue;; esac
        table="$table$pid $2
"
    done
    grown=1
    while [ -n "$grown" ]; do
        grown=''
        while read -r pid ppid; do
            [ -n "$pid" ] || continue
            case " $tracked " in *" $pid "*) continue;; esac
            case " $tracked " in *" $ppid "*) tracked="$tracked $pid"; grown=1;; esac
        done <<EOF
$table
EOF
    done
    alive=''
    for pid in $tracked; do
        case "
$table" in *"
$pid "*) ;; *) continue;; esac
        alive="$alive $pid"
        case " $stopped " in *" $pid "*) ;; *) kill -TERM "$pid" 2>/dev/null; stopped="$stopped $pid";; esac
    done
    tracked=$alive
    [ -z "$alive" ] && exit 0
    [ "$remaining" -gt 0 ] || break
    remaining=$((remaining - 1)); sleep 1
done
echo "processes did not stop within 10 seconds:$alive" >&2
exit 1"#
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The shape of the real container this was built against: Sail's
    /// supervisor and its `artisan serve`, a leftover `artisan dev` stack
    /// from an earlier Mast session, the current session's copy, a Horizon
    /// started from a terminal, and the scan itself.
    fn sample() -> Scan {
        let out = "\
H 900 100 50000.00 4096
P 1 0 1000 5000 /usr/bin/python3 /usr/bin/supervisord -c /etc/supervisor/conf.d/supervisord.conf
P 26 1 1159 20000 /usr/bin/php -d variables_order=EGPCS /var/www/html/artisan serve --host=0.0.0.0 --port=80
P 42 0 2000 400 sh -c pnpm dlx @laravel/multiplex --title 'artisan dev'
P 121 42 2010 30000 node /home/sail/.cache/pnpm/dlx/x/node_modules/.bin/multiplex
P 151 121 2020 300 sh -c php artisan horizon
P 152 151 2021 20000 php artisan horizon
P 443 152 2030 20000 /usr/bin/php8.5 artisan horizon:supervisor x:supervisor-1 redis
P 155 121 2020 300 sh -c php artisan schedule:work
P 156 155 2021 15000 php artisan schedule:work
P 234 121 2040 30000 node /var/www/html/node_modules/.bin/../vite-plus/bin/vp dev
P 344 234 2050 500000 /usr/bin/node /var/www/html/node_modules/@voidzero-dev/vite-plus-core/dist/vite/node/cli.js dev
P 343 121 2050 90000 /usr/bin/node /var/www/html/node_modules/@voidzero-dev/vite-plus-core/dist/vite/node/cli.js build -c x.ts --watch
P 500 0 9000 400 sh -c pnpm dlx @laravel/multiplex --title 'artisan dev'
P 501 500 9010 20000 php artisan horizon
P 502 500 9010 15000 php artisan schedule:work
P 700 0 12000 20000 php artisan horizon
P 900 0 15000 100 sh -c printf
P 901 900 15001 100 tr \\0\\n
R 46075-12-111 42 2000 dev
R 46075-15-222 500 9000 dev
R 1-1-1 31337 77 gone
";
        parse(out).unwrap()
    }

    fn live_dev() -> Live {
        Live {
            commands: HashMap::from([("46075-15-222".to_string(), "dev".to_string())]),
            processes: HashMap::new(),
        }
    }

    #[test]
    fn parses_header_processes_and_records() {
        let scan = sample();
        assert_eq!(scan.own_pid, 900);
        assert_eq!(scan.clk_tck, 100);
        assert_eq!(scan.procs.len(), 18);
        assert_eq!(scan.records.len(), 3);
        let p = scan.procs.iter().find(|p| p.pid == 152).unwrap();
        assert_eq!((p.ppid, p.start, p.rss_pages), (151, 2021, 20000));
        assert_eq!(p.cmdline, "php artisan horizon");
    }

    #[test]
    fn a_scan_without_its_header_is_no_scan() {
        assert!(parse("P 1 0 1 1 init\n").is_none());
        assert!(parse("").is_none());
    }

    #[test]
    fn groups_trees_and_says_who_started_each() {
        let runs = runs(&sample(), &live_dev(), 1_000_000);
        let owners: Vec<(u32, &ContainerRunOwner)> =
            runs.iter().map(|r| (r.pid, &r.owner)).collect();
        assert_eq!(
            owners,
            [
                (
                    42,
                    &ContainerRunOwner::EarlierMast {
                        command: "dev".into()
                    }
                ),
                (
                    500,
                    &ContainerRunOwner::Mast {
                        command: "dev".into()
                    }
                ),
                (700, &ContainerRunOwner::Outside),
            ],
            "init, Sail's own serve and the scan are the container, not runs in it"
        );
        let leftover = &runs[0];
        assert_eq!(leftover.title, "dev");
        assert_eq!(leftover.process_count, 10);
        assert_eq!(leftover.kinds, ["Horizon", "Scheduler", "Vite"]);
        // 0.5M + 0.09M + … pages of 4 KiB, to two significant figures.
        assert_eq!(leftover.memory_bytes, 2_900_000_000);
        // Boot was 50 000 s before "now"; the root started 20 s after boot.
        assert_eq!(leftover.started_at_unix, 1_000_000 - 50_000 + 20);
        assert_eq!(runs[2].title, "artisan horizon");
    }

    #[test]
    fn a_child_init_adopted_is_orphaned_unless_it_came_up_with_the_container() {
        let mut scan = sample();
        // Its starter exited long after boot; init adopted it.
        scan.procs.push(Proc {
            pid: 800,
            ppid: 1,
            start: 30_000,
            rss_pages: 100,
            cmdline: "php artisan queue:work".into(),
        });
        let runs = runs(&scan, &live_dev(), 1_000_000);
        let orphan = runs.iter().find(|r| r.pid == 800).unwrap();
        assert_eq!(orphan.owner, ContainerRunOwner::Orphaned);
        assert_eq!(orphan.kinds, ["Queue worker"]);
        // Without a supervisor as init, init's children are the container's
        // own command.
        scan.procs[0].cmdline = "php-fpm: master process".into();
        assert!(
            super::runs(&scan, &live_dev(), 1_000_000)
                .iter()
                .all(|r| r.pid != 800)
        );
    }

    #[test]
    fn a_record_whose_pid_was_reused_claims_nothing() {
        let mut scan = sample();
        scan.records[0].start += 1;
        let runs = runs(&scan, &live_dev(), 1_000_000);
        assert_eq!(runs[0].owner, ContainerRunOwner::Outside);
    }

    #[test]
    fn mast_supervised_daemons_are_claimed_by_kind_newest_first() {
        let mut scan = sample();
        scan.procs.push(Proc {
            pid: 710,
            ppid: 0,
            start: 13_000,
            rss_pages: 100,
            cmdline: "php artisan horizon".into(),
        });
        let live = Live {
            commands: HashMap::new(),
            processes: HashMap::from([("Horizon", "Horizon".to_string())]),
        };
        let runs = runs(&scan, &live, 1_000_000);
        let owner = |pid| &runs.iter().find(|r| r.pid == pid).unwrap().owner;
        assert_eq!(
            owner(710),
            &ContainerRunOwner::Mast {
                command: "Horizon".into()
            }
        );
        assert_eq!(owner(700), &ContainerRunOwner::Outside);
        // A stack that contains Horizon among other things is not the daemon.
        assert_eq!(
            owner(500),
            &ContainerRunOwner::EarlierMast {
                command: "dev".into()
            }
        );
    }

    #[test]
    fn kinds_are_the_daemons_not_their_wrappers() {
        assert_eq!(kind_of("php artisan horizon"), Some("Horizon"));
        assert_eq!(
            kind_of("/usr/bin/php8.5 artisan horizon:supervisor x"),
            None
        );
        assert_eq!(kind_of("/usr/bin/php8.5 artisan horizon:work redis"), None);
        assert_eq!(
            kind_of("php artisan queue:listen --tries=1"),
            Some("Queue worker")
        );
        assert_eq!(
            kind_of("/usr/bin/php8.5 artisan queue:work --once"),
            Some("Queue worker")
        );
        assert_eq!(
            kind_of("php /var/www/html/artisan reverb:start --debug"),
            Some("Reverb")
        );
        assert_eq!(kind_of("sh -c php artisan horizon"), Some("Horizon"));
        assert_eq!(
            kind_of("node /var/www/html/node_modules/.bin/vite"),
            Some("Vite")
        );
        assert_eq!(
            kind_of("node /app/node_modules/vite/bin/vite.js --host"),
            Some("Vite")
        );
        assert_eq!(
            kind_of("node /app/node_modules/vite/bin/vite.js build"),
            None
        );
        assert_eq!(
            kind_of("node /app/node_modules/.bin/../vite-plus/bin/vp dev"),
            None
        );
        assert_eq!(kind_of("node /usr/bin/pnpm run dev"), None);
        assert_eq!(kind_of("node /app/node_modules/vitest/vitest.mjs"), None);
        assert_eq!(
            kind_of(
                "/usr/bin/php -d variables_order=EGPCS /var/www/html/artisan octane:start --server=frankenphp"
            ),
            Some("Octane")
        );
    }

    #[test]
    fn the_wrapper_shell_counts_as_its_daemon_but_one_tree_counts_once() {
        // `sh -c php artisan horizon` and the php under it are one Horizon:
        // kinds are per tree, so the wrapper cannot double it.
        let runs = runs(&sample(), &live_dev(), 1_000_000);
        assert_eq!(runs[0].kinds.iter().filter(|k| *k == "Horizon").count(), 1);
    }

    #[test]
    fn duplicates_name_every_copy_and_offer_to_stop_the_ones_mast_is_not_running() {
        let runs = runs(&sample(), &live_dev(), 1_000_000);
        let advisories = duplicate_advisories(&runs, &kind_counts(&sample()));
        let horizon = advisories
            .iter()
            .find(|a| a.message.starts_with("Horizon"))
            .unwrap();
        assert_eq!(
            horizon.message,
            "Horizon is running 3 times in the app container, so every job is picked up by \
             two masters' workers. Running: dev (left by an earlier Mast session); dev (Mast); \
             artisan horizon (started outside Mast)."
        );
        let stops: Vec<u32> = horizon
            .actions
            .iter()
            .map(|a| match a {
                AdvisoryAction::StopContainerRun { pid, .. } => *pid,
                other => panic!("unexpected {other:?}"),
            })
            .collect();
        assert_eq!(
            stops,
            [42, 700],
            "never offers to stop what Mast is running"
        );
        let scheduler = advisories
            .iter()
            .find(|a| a.message.starts_with("Scheduler"))
            .unwrap();
        assert!(scheduler.message.contains("running twice"));
        assert!(
            advisories.iter().all(|a| !a.message.starts_with("Vite")),
            "one Vite is fine"
        );
    }

    fn vite_run(owner: ContainerRunOwner) -> ContainerRun {
        ContainerRun {
            pid: 10,
            start_ticks: 1,
            owner,
            title: "dev".into(),
            command_line: "npm run dev".into(),
            started_at_unix: 0,
            memory_bytes: 0,
            process_count: 3,
            kinds: vec!["Vite".into()],
        }
    }

    fn facts(hot: Option<&str>, listening: Option<bool>) -> ViteFacts {
        ViteFacts {
            hot: hot.and_then(mast_laravel::vite::parse_hot_file),
            vite_port: 5180,
            hot_listening: listening,
        }
    }

    fn action_labels(advisories: &[Advisory]) -> Vec<String> {
        advisories
            .iter()
            .flat_map(|a| &a.actions)
            .map(|action| match action {
                AdvisoryAction::StopContainerRun { label, .. }
                | AdvisoryAction::RestartCommand { label, .. }
                | AdvisoryAction::ApplyRepair { label, .. } => label.clone(),
            })
            .collect()
    }

    #[test]
    fn vite_running_without_a_hot_file_means_pages_get_the_old_build() {
        let mast = [vite_run(ContainerRunOwner::Mast {
            command: "dev".into(),
        })];
        let advisories = vite_advisories(&facts(None, None), &mast);
        assert!(advisories[0].message.contains("pages load the last build"));
        assert_eq!(action_labels(&advisories), ["Restart dev"]);
        // Only Mast's own runs are offered for restart.
        let leftover = [vite_run(ContainerRunOwner::EarlierMast {
            command: "dev".into(),
        })];
        let advisories = vite_advisories(&facts(None, None), &leftover);
        assert_eq!(advisories.len(), 1);
        assert!(action_labels(&advisories).is_empty());
        // No Vite, no hot file: built assets, as intended.
        assert!(vite_advisories(&facts(None, None), &[]).is_empty());
    }

    #[test]
    fn a_hot_file_from_another_dev_server_is_named() {
        let mast = [vite_run(ContainerRunOwner::Mast {
            command: "dev".into(),
        })];
        let advisories = vite_advisories(&facts(Some("http://127.0.0.1:5288"), Some(true)), &mast);
        assert!(advisories[0].message.contains("http://127.0.0.1:5288"));
        assert!(advisories[0].message.contains("port 5180"));
        assert_eq!(
            action_labels(&advisories),
            ["Restart dev", "Remove public/hot"]
        );
    }

    #[test]
    fn a_hot_file_nothing_answers_is_stale() {
        let advisories = vite_advisories(&facts(Some("http://localhost:5180"), Some(false)), &[]);
        assert!(advisories[0].message.contains("no Vite is running"));
        assert_eq!(action_labels(&advisories), ["Remove public/hot"]);
        let advisories = vite_advisories(&facts(Some("http://localhost:5288"), Some(false)), &[]);
        assert!(advisories[0].message.contains("nothing is listening"));
    }

    #[test]
    fn the_projects_own_vite_and_its_hot_file_is_quiet() {
        let mast = [vite_run(ContainerRunOwner::Mast {
            command: "dev".into(),
        })];
        assert!(
            vite_advisories(&facts(Some("http://localhost:5180"), Some(true)), &mast).is_empty()
        );
        // A remote dev server is not ours to judge.
        assert!(
            vite_advisories(&facts(Some("https://vite.myapp.test:5180"), None), &[]).is_empty()
        );
    }

    #[test]
    fn process_ids_and_kinds_map_both_ways() {
        for id in ["horizon", "queue", "schedule", "reverb"] {
            assert_eq!(kind_of_process_id(id).and_then(process_id_of), Some(id));
        }
        assert_eq!(kind_of_process_id("vite"), None);
    }

    #[test]
    fn a_daemon_the_container_runs_itself_counts_but_is_never_offered_for_stopping() {
        let mut scan = sample();
        // A supervisor program started at boot, as some images run Horizon.
        scan.procs.push(Proc {
            pid: 30,
            ppid: 1,
            start: 1200,
            rss_pages: 100,
            cmdline: "php artisan horizon".into(),
        });
        let counts = kind_counts(&scan);
        assert_eq!(counts["Horizon"], 4, "three runs and the container's own");
        let runs = runs(&scan, &live_dev(), 1_000_000);
        assert!(
            runs.iter().all(|r| r.pid != 30),
            "the container's own is not a run"
        );
        let advisories = duplicate_advisories(&runs, &counts);
        let horizon = advisories
            .iter()
            .find(|a| a.message.starts_with("Horizon"))
            .unwrap();
        assert!(horizon.message.contains("running 4 times"));
        assert!(
            horizon
                .message
                .contains("Running: the container's own startup; dev (left by")
        );
        assert_eq!(
            horizon.actions.len(),
            2,
            "only the two runs Mast is not running"
        );
    }

    #[test]
    fn glimpses_of_mast_itself_and_half_started_processes_are_not_runs() {
        let mut scan = sample();
        for (pid, cmdline) in [
            (
                950,
                "sh -c for p in /proc/[0-9]*/cmdline; do tr '\\0' ' ' < $p; done",
            ),
            (960, ""),
        ] {
            scan.procs.push(Proc {
                pid,
                ppid: 0,
                start: 15_500,
                rss_pages: 10,
                cmdline: cmdline.into(),
            });
        }
        let runs = runs(&scan, &live_dev(), 1_000_000);
        assert!(
            runs.iter().all(|r| r.pid != 950 && r.pid != 960),
            "{runs:#?}"
        );
    }

    #[test]
    fn a_runner_naming_a_daemon_in_its_arguments_is_not_that_daemon_running() {
        // The stack runner outlives the pane whose Horizon died; its argument
        // list still says `php artisan horizon`.
        let out = "\
H 900 100 50000.00 4096
P 1 0 1000 5000 /usr/bin/python3 /usr/bin/supervisord -c /etc/supervisor/conf.d/supervisord.conf
P 17 0 2000 400 sh -c pnpm dlx @laravel/multiplex --title 'artisan dev' 'horizon@#93c5fd,php artisan horizon' 'reverb@#c4b5fd,php artisan reverb:start'
P 54 17 2010 30000 node /usr/bin/pnpm dlx @laravel/multiplex --title artisan dev horizon@#93c5fd,php artisan horizon
P 70 54 2020 300 sh -c php artisan reverb:start
P 71 70 2021 20000 php artisan reverb:start
";
        let counts = kind_counts(&parse(out).unwrap());
        assert_eq!(counts.get("Horizon"), None);
        assert_eq!(counts.get("Reverb"), Some(&1));
    }

    #[test]
    fn kinds_doubled_by_the_same_copies_are_one_advisory() {
        let stack = |pid, owner| ContainerRun {
            pid,
            start_ticks: pid as u64,
            owner,
            title: "dev".into(),
            command_line: "php artisan dev".into(),
            started_at_unix: 0,
            memory_bytes: 0,
            process_count: 20,
            kinds: vec!["Horizon".into(), "Pail".into(), "Scheduler".into()],
        };
        let runs = [
            stack(42, ContainerRunOwner::Outside),
            stack(
                500,
                ContainerRunOwner::Mast {
                    command: "dev".into(),
                },
            ),
        ];
        let counts = HashMap::from([("Horizon", 2), ("Pail", 2), ("Scheduler", 2)]);
        let advisories = duplicate_advisories(&runs, &counts);
        assert_eq!(advisories.len(), 1);
        assert_eq!(
            advisories[0].message,
            "Horizon, Scheduler and Pail are each running twice in the app container, so every \
             job is picked up by two masters' workers, scheduled tasks run twice and log lines \
             are tailed twice. Running: dev (started outside Mast); dev (Mast)."
        );
        assert_eq!(advisories[0].actions.len(), 1);
    }

    #[test]
    fn artisan_counts_only_as_the_program() {
        let multiplex = "sh -c pnpm dlx @laravel/multiplex --title 'artisan dev · Kusina' \
                         'horizon@#93c5fd,php artisan horizon' 'vite@#c4b5fd,pnpm run dev'";
        assert_eq!(kind_of(multiplex), None);
        assert_eq!(short_command(multiplex), "artisan dev · Kusina");
        assert_eq!(
            kind_of("/usr/bin/php -d variables_order=EGPCS /var/www/html/artisan serve --port=80"),
            Some("App server")
        );
        assert_eq!(
            kind_of("/var/www/html/artisan queue:work"),
            Some("Queue worker")
        );
        assert_eq!(kind_of("php -r echo artisan horizon"), None);
        assert_eq!(kind_of("php vendor/bin/phpunit --filter artisan"), None);
    }

    #[test]
    fn short_commands_fit_a_chip() {
        assert_eq!(short_command("php artisan horizon"), "artisan horizon");
        assert_eq!(
            short_command("/usr/bin/php8.5 /var/www/html/artisan queue:work --once"),
            "artisan queue:work"
        );
        assert_eq!(
            short_command("sh -c pnpm dlx @laravel/multiplex --title x"),
            "pnpm dlx @laravel/multiplex"
        );
        assert_eq!(
            short_command("/usr/bin/node /x/server.js --port 3000"),
            "node /x/server.js --port"
        );
        assert!(short_command(&format!("node {}", "a".repeat(80))).ends_with('…'));
    }

    #[test]
    fn memory_rounds_to_two_figures() {
        assert_eq!(round_memory(2_871_234_567), 2_900_000_000);
        assert_eq!(round_memory(87_400_000), 87_000_000);
        assert_eq!(round_memory(45), 45);
    }
}

#[cfg(test)]
mod docker_tests {
    use super::*;
    use crate::test_docker::{alpine, docker, docker_usable};

    async fn scan_of(container: &str) -> Scan {
        let out = docker(&["exec", container, "sh", "-c", SCAN_SCRIPT]).await;
        assert!(out.success(), "{}", out.stderr);
        parse(&out.stdout).expect("a scan with its header")
    }

    async fn alive(container: &str, needle: &str) -> bool {
        let ps = docker(&["exec", container, "ps", "-o", "args"]).await;
        ps.stdout.lines().any(|line| line.contains(needle))
    }

    // The scan and the tree stop are shell run inside someone else's image;
    // this runs both in busybox, the most minimal of them. One exec starts a
    // shell with two children, another a lone process with a run record —
    // the ownership a Mast command leaves — and the container's own init must
    // not be listed.
    #[tokio::test]
    async fn the_scan_and_tree_stop_work_in_a_real_container() {
        if !docker_usable().await {
            eprintln!("skipping: no usable docker daemon");
            return;
        }
        let (name, _cleanup) = alpine().await;
        for exec in [
            [
                "exec",
                "-d",
                name.as_str(),
                "sh",
                "-c",
                "sleep 300 & sleep 301 & wait",
            ],
            ["exec", "-d", name.as_str(), "sh", "-c", "exec sleep 302"],
        ] {
            let out = docker(&exec).await;
            assert!(out.success(), "{}", out.stderr);
        }
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(10);
        while !(alive(&name, "sleep 301").await && alive(&name, "sleep 302").await) {
            assert!(
                std::time::Instant::now() < deadline,
                "the execs did not start"
            );
            tokio::time::sleep(std::time::Duration::from_millis(100)).await;
        }
        // Record the lone process the way the run wrapper does.
        let recorded = docker(&[
            "exec",
            &name,
            "sh",
            "-c",
            "for p in /proc/[0-9]*; do [ \"$(tr '\\0' ' ' < $p/cmdline)\" = 'sleep 302 ' ] && \
             mkdir -p /tmp/mast-runs && echo \"${p#/proc/} $(cut -d' ' -f22 $p/stat) dev\" \
             > /tmp/mast-runs/1-2-3; done; cat /tmp/mast-runs/1-2-3",
        ])
        .await;
        assert!(
            recorded.success() && !recorded.stdout.is_empty(),
            "{}",
            recorded.stderr
        );

        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_secs();
        let runs = runs(&scan_of(&name).await, &Live::default(), now);
        assert_eq!(runs.len(), 2, "{runs:#?}");
        let tree = runs
            .iter()
            .find(|r| r.process_count == 3)
            .expect("the shell and its two sleeps");
        assert_eq!(tree.owner, ContainerRunOwner::Outside);
        let lone = runs.iter().find(|r| r.process_count == 1).unwrap();
        assert_eq!(
            lone.owner,
            ContainerRunOwner::EarlierMast {
                command: "dev".into()
            }
        );
        assert!(
            now.abs_diff(tree.started_at_unix) < 120,
            "started just now: {tree:?}"
        );

        // A start time that no longer matches names a different process.
        let script = stop_tree_script(tree.pid, tree.start_ticks + 1);
        let out = docker(&["exec", &name, "sh", "-c", &script]).await;
        assert!(out.success(), "{}", out.stderr);
        assert!(
            alive(&name, "sleep 300").await,
            "a mismatched start time must stop nothing"
        );

        let script = stop_tree_script(tree.pid, tree.start_ticks);
        let out = docker(&["exec", &name, "sh", "-c", &script]).await;
        assert!(out.success(), "{}", out.stderr);
        assert!(!alive(&name, "sleep 300").await && !alive(&name, "sleep 301").await);
        assert!(
            alive(&name, "sleep 302").await,
            "another tree must not be touched"
        );
    }
}
