//! The semantic-delta gate: structural comparison of old vs new documents
//! permitting EXACTLY the intended change. Whole-document equality is unsound
//! (anchors/merge keys/interpolation); this walk is what turns "the splice
//! landed somewhere wrong" or "the anchor rippled" into a refusal.

use saphyr::{Scalar, Yaml};

use crate::{Edit, PathSeg};

pub(crate) fn check_delta(old: &Yaml, new: &Yaml, edit: &Edit) -> Result<(), String> {
    let mut cur: Vec<PathSeg> = Vec::new();
    match edit {
        Edit::SetScalar { path, .. } => {
            walk(old, new, &Mode::ChangeAt(path), &mut cur)
        }
        Edit::InsertMapKey { path, key, .. } | Edit::InsertMapBlock { path, key, .. } => {
            walk(old, new, &Mode::InsertKey { parent: path, key }, &mut cur)
        }
        Edit::InsertSeqItem { path, .. } => {
            walk(old, new, &Mode::InsertItem { parent: path }, &mut cur)
        }
        Edit::RemoveKey { path } => {
            let (parent, last) = path.split_at(path.len().saturating_sub(1));
            let Some(PathSeg::Key(removed)) = last.first() else {
                return Err("RemoveKey path must end in a key".into());
            };
            walk(old, new, &Mode::RemoveKey { parent, key: removed }, &mut cur)
        }
        Edit::RenameKey { path, to } => {
            let (parent, last) = path.split_at(path.len().saturating_sub(1));
            let Some(PathSeg::Key(from)) = last.first() else {
                return Err("RenameKey path must end in a key".into());
            };
            walk(old, new, &Mode::RenameKey { parent, from, to }, &mut cur)
        }
        Edit::RemoveSeqItem { path } => {
            let (parent, last) = path.split_at(path.len().saturating_sub(1));
            let Some(PathSeg::Index(removed)) = last.first() else {
                return Err("RemoveSeqItem path must end in an index".into());
            };
            walk(old, new, &Mode::RemoveItem { parent, item: *removed }, &mut cur)
        }
    }
}

enum Mode<'a> {
    ChangeAt(&'a [PathSeg]),
    InsertKey { parent: &'a [PathSeg], key: &'a str },
    InsertItem { parent: &'a [PathSeg] },
    RemoveKey { parent: &'a [PathSeg], key: &'a str },
    RemoveItem { parent: &'a [PathSeg], item: usize },
    RenameKey { parent: &'a [PathSeg], from: &'a str, to: &'a str },
}

fn seg_of(key: &Yaml) -> PathSeg {
    match key {
        Yaml::Value(Scalar::String(s)) => PathSeg::Key(s.to_string()),
        other => PathSeg::Key(format!("{other:?}")),
    }
}

fn at(cur: &[PathSeg], target: &[PathSeg]) -> bool {
    cur.len() == target.len() && cur.iter().zip(target).all(|(a, b)| a == b)
}

fn here(cur: &[PathSeg]) -> String {
    if cur.is_empty() {
        "<root>".into()
    } else {
        cur.iter()
            .map(|seg| match seg {
                PathSeg::Key(k) => k.clone(),
                PathSeg::Index(i) => format!("[{i}]"),
            })
            .collect::<Vec<_>>()
            .join(".")
    }
}

fn walk(a: &Yaml, b: &Yaml, mode: &Mode, cur: &mut Vec<PathSeg>) -> Result<(), String> {
    if let Mode::ChangeAt(path) = mode
        && at(cur, path)
    {
        return Ok(()); // the one place allowed to differ
    }

    match (a, b) {
        (Yaml::Mapping(ma), Yaml::Mapping(mb)) => {
            match mode {
                Mode::InsertKey { parent, key } if at(cur, parent) => {
                    if mb.len() != ma.len() + 1 {
                        return Err(format!(
                            "expected exactly one inserted key at {}",
                            here(cur)
                        ));
                    }
                    let mut ib = mb.iter();
                    for (ka, va) in ma.iter() {
                        let (kb, vb) = ib.next().expect("length checked");
                        if ka != kb {
                            return Err(format!("key order changed at {}", here(cur)));
                        }
                        descend(va, vb, mode, cur, seg_of(ka))?;
                    }
                    let (kb, _) = ib.next().expect("length checked");
                    if seg_of(kb) != PathSeg::Key(key.to_string()) {
                        return Err(format!(
                            "inserted key mismatch at {}: expected {key}",
                            here(cur)
                        ));
                    }
                    Ok(())
                }
                Mode::RenameKey { parent, from, to } if at(cur, parent) => {
                    if ma.len() != mb.len() {
                        return Err(format!("rename changed key count at {}", here(cur)));
                    }
                    let mut renamed_seen = false;
                    for ((ka, va), (kb, vb)) in ma.iter().zip(mb.iter()) {
                        if ka == kb {
                            descend(va, vb, mode, cur, seg_of(ka))?;
                            continue;
                        }
                        // The one position allowed to differ: from -> to, with
                        // the value carried across untouched.
                        if renamed_seen
                            || seg_of(ka) != PathSeg::Key(from.to_string())
                            || seg_of(kb) != PathSeg::Key(to.to_string())
                        {
                            return Err(format!("unexpected key change at {}", here(cur)));
                        }
                        if va != vb {
                            return Err(format!("renamed value changed at {}", here(cur)));
                        }
                        renamed_seen = true;
                    }
                    if !renamed_seen && from != to {
                        return Err(format!("{from} was not renamed at {}", here(cur)));
                    }
                    Ok(())
                }
                Mode::RemoveKey { parent, key } if at(cur, parent) => {
                    if ma.len() != mb.len() + 1 {
                        return Err(format!(
                            "expected exactly one removed key at {}",
                            here(cur)
                        ));
                    }
                    let mut ib = mb.iter();
                    let mut removed_seen = false;
                    for (ka, va) in ma.iter() {
                        if !removed_seen && seg_of(ka) == PathSeg::Key(key.to_string()) {
                            removed_seen = true;
                            continue;
                        }
                        let Some((kb, vb)) = ib.next() else {
                            return Err(format!("unexpected shrinkage at {}", here(cur)));
                        };
                        if ka != kb {
                            return Err(format!("key order changed at {}", here(cur)));
                        }
                        descend(va, vb, mode, cur, seg_of(ka))?;
                    }
                    if !removed_seen {
                        return Err(format!("removed key {key} not found at {}", here(cur)));
                    }
                    Ok(())
                }
                _ => {
                    if ma.len() != mb.len() {
                        return Err(format!("mapping size changed at {}", here(cur)));
                    }
                    for ((ka, va), (kb, vb)) in ma.iter().zip(mb.iter()) {
                        if ka != kb {
                            return Err(format!("keys differ at {}", here(cur)));
                        }
                        descend(va, vb, mode, cur, seg_of(ka))?;
                    }
                    Ok(())
                }
            }
        }
        (Yaml::Sequence(sa), Yaml::Sequence(sb)) => {
            match mode {
                Mode::InsertItem { parent } if at(cur, parent) => {
                    if sb.len() != sa.len() + 1 {
                        return Err(format!(
                            "expected exactly one appended item at {}",
                            here(cur)
                        ));
                    }
                    for (i, (va, vb)) in sa.iter().zip(sb.iter()).enumerate() {
                        descend(va, vb, mode, cur, PathSeg::Index(i))?;
                    }
                    Ok(())
                }
                Mode::RemoveItem { parent, item } if at(cur, parent) => {
                    if sa.len() != sb.len() + 1 {
                        return Err(format!(
                            "expected exactly one removed item at {}",
                            here(cur)
                        ));
                    }
                    if *item >= sa.len() {
                        return Err(format!("removed index out of range at {}", here(cur)));
                    }
                    let mut ib = sb.iter();
                    for (i, va) in sa.iter().enumerate() {
                        if i == *item {
                            continue;
                        }
                        let vb = ib.next().expect("length checked");
                        descend(va, vb, mode, cur, PathSeg::Index(i))?;
                    }
                    Ok(())
                }
                _ => {
                    if sa.len() != sb.len() {
                        return Err(format!("sequence length changed at {}", here(cur)));
                    }
                    for (i, (va, vb)) in sa.iter().zip(sb.iter()).enumerate() {
                        descend(va, vb, mode, cur, PathSeg::Index(i))?;
                    }
                    Ok(())
                }
            }
        }
        _ => {
            if a == b {
                Ok(())
            } else {
                Err(format!("unexpected change at {}", here(cur)))
            }
        }
    }
}

fn descend(
    a: &Yaml,
    b: &Yaml,
    mode: &Mode,
    cur: &mut Vec<PathSeg>,
    seg: PathSeg,
) -> Result<(), String> {
    cur.push(seg);
    let result = walk(a, b, mode, cur);
    cur.pop();
    result
}
