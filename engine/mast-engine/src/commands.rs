//! Ordering between a project's user-defined commands.
//!
//! `after` exists because a frontend dev server is useless before the backend
//! it proxies to is up, and auto-start otherwise fires everything at once.
//! The graph it describes is tiny — a handful of commands per project — so
//! the checks here are the plain quadratic ones, and clarity wins.

use mast_contract::ProjectCommand;

/// Reject an `after` graph that could never run: a command waiting on itself,
/// on a name that is not there, or on a cycle. Every one of those presents
/// identically at runtime — a command that never starts — so the error has to
/// name which it is.
pub(crate) fn check_order(commands: &[ProjectCommand]) -> Result<(), String> {
    let named = |name: &str| commands.iter().find(|c| c.name == name);

    for command in commands {
        let Some(after) = command.after.as_deref().filter(|a| !a.trim().is_empty()) else {
            continue;
        };
        if after == command.name {
            return Err(format!("\"{}\" cannot wait for itself", command.name));
        }
        if named(after).is_none() {
            return Err(format!(
                "\"{}\" waits for \"{after}\", which is not a command of this project",
                command.name
            ));
        }
    }

    // Walk each chain to its end. A cycle is the walk that outlives the list.
    for command in commands {
        let mut seen = vec![command.name.as_str()];
        let mut at = command;
        while let Some(after) = at.after.as_deref().filter(|a| !a.trim().is_empty()) {
            let Some(next) = named(after) else { break };
            if seen.contains(&next.name.as_str()) {
                seen.push(next.name.as_str());
                return Err(format!("commands wait on each other in a loop: {}", seen.join(" → ")));
            }
            seen.push(next.name.as_str());
            at = next;
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn cmd(name: &str, after: Option<&str>) -> ProjectCommand {
        ProjectCommand {
            name: name.into(),
            command: "true".into(),
            auto_start: true,
            cwd: None,
            after: after.map(String::from),
            ready_when: None,
        }
    }

    #[test]
    fn a_plain_chain_is_allowed() {
        let list = [cmd("api", None), cmd("web", Some("api")), cmd("worker", Some("web"))];
        assert!(check_order(&list).is_ok());
    }

    #[test]
    fn independent_commands_need_no_order() {
        assert!(check_order(&[cmd("a", None), cmd("b", None)]).is_ok());
    }

    /// Each of these presents at runtime as "the chip never turns green", so
    /// the message has to say which mistake it was.
    #[test]
    fn unsatisfiable_orders_are_named_not_merely_refused() {
        let missing = check_order(&[cmd("web", Some("api"))]).unwrap_err();
        assert!(missing.contains("not a command of this project"), "{missing}");

        let itself = check_order(&[cmd("web", Some("web"))]).unwrap_err();
        assert!(itself.contains("cannot wait for itself"), "{itself}");

        let pair = check_order(&[cmd("a", Some("b")), cmd("b", Some("a"))]).unwrap_err();
        assert!(pair.contains("loop"), "{pair}");

        let longer =
            check_order(&[cmd("a", Some("c")), cmd("b", Some("a")), cmd("c", Some("b"))])
                .unwrap_err();
        assert!(longer.contains("loop"), "{longer}");
    }

    /// An empty string is what an untouched form field sends; it means "no
    /// dependency", not a command whose name is blank.
    #[test]
    fn a_blank_after_is_no_dependency() {
        assert!(check_order(&[cmd("web", Some("   "))]).is_ok());
    }
}
