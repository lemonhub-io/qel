//! Commit graph traversal: rev-list ordering, merge-base, reachability.

use crate::object::{Commit, ObjType, Oid};
use crate::repo::Repo;
use crate::util::{GitError, Result};
use std::collections::{HashMap, HashSet};

pub fn load_commit(repo: &Repo, oid: &Oid) -> Result<Commit> {
    let obj = repo.odb.read(oid)?;
    match obj.0 {
        ObjType::Commit => {
            let mut c = Commit::parse(&obj.1)?;
            // shallow boundary: graft the commit to have no parents
            if repo.is_shallow(oid) {
                c.parents.clear();
            }
            Ok(c)
        }
        ObjType::Tag => {
            let tag = crate::object::Tag::parse(&obj.1)?;
            load_commit(repo, &tag.object)
        }
        _ => Err(GitError::ObjectCorrupt(format!("{} is not a commit", oid))),
    }
}

/// All commits reachable from `tips` (inclusive) in git's default log
/// order: a date-ordered commit_list — pop the front (newest) entry and
/// re-insert its parents after all entries with date >= theirs.
pub fn rev_list(repo: &Repo, tips: &[Oid]) -> Result<Vec<Oid>> {
    fn load(repo: &Repo, o: &Oid) -> Result<Option<Commit>> {
        let obj = match repo.odb.read_opt(o)? {
            Some(o) => o,
            None => return Ok(None),
        };
        match obj.0 {
            ObjType::Commit => {
                let mut c = Commit::parse(&obj.1)?;
                if repo.is_shallow(o) {
                    c.parents.clear();
                }
                Ok(Some(c))
            }
            ObjType::Tag => {
                let t = crate::object::Tag::parse(&obj.1)?;
                load(repo, &t.object)
            }
            _ => Ok(None),
        }
    }
    // sorted list, newest (largest committer time) at the front
    let mut list: std::collections::VecDeque<(Oid, Commit)> = Default::default();
    let mut seen: HashSet<Oid> = HashSet::new();
    let insert_by_date =
        |list: &mut std::collections::VecDeque<(Oid, Commit)>, o: Oid, c: Commit| {
            // insert before the first entry strictly older than `c`
            let pos = list
                .iter()
                .position(|(_, e)| e.committer.time < c.committer.time)
                .unwrap_or(list.len());
            list.insert(pos, (o, c));
        };
    for t in tips {
        if seen.insert(*t) {
            if let Some(c) = load(repo, t)? {
                insert_by_date(&mut list, *t, c);
            }
        }
    }
    let mut out = Vec::new();
    while let Some((o, c)) = list.pop_front() {
        out.push(o);
        for p in &c.parents {
            if seen.insert(*p) {
                if let Some(pc) = load(repo, p)? {
                    insert_by_date(&mut list, *p, pc);
                }
            }
        }
    }
    Ok(out)
}

/// Set of all commits reachable from `tip` (for is_ancestor etc).
#[allow(dead_code)]
pub fn reachable_set(repo: &Repo, tip: &Oid) -> Result<HashSet<Oid>> {
    Ok(rev_list(repo, &[*tip])?.into_iter().collect())
}

/// Objects (commits, trees, blobs, tags) reachable from the given tips —
/// used for pack generation & fsck.
pub fn reachable_objects(repo: &Repo, tips: &[Oid]) -> Result<HashSet<Oid>> {
    let mut out: HashSet<Oid> = HashSet::new();
    let mut stack: Vec<Oid> = tips.to_vec();
    while let Some(o) = stack.pop() {
        if !out.insert(o) {
            continue;
        }
        let obj = match repo.odb.read_opt(&o)? {
            Some(o) => o,
            None => continue,
        };
        match obj.0 {
            ObjType::Commit => {
                let mut c = Commit::parse(&obj.1)?;
                if repo.is_shallow(&o) {
                    c.parents.clear();
                }
                stack.push(c.tree);
                stack.extend(c.parents.iter().copied());
            }
            ObjType::Tree => {
                for e in crate::object::parse_tree(&obj.1)? {
                    stack.push(e.oid);
                }
            }
            ObjType::Tag => {
                let t = crate::object::Tag::parse(&obj.1)?;
                stack.push(t.object);
            }
            ObjType::Blob => {}
        }
    }
    Ok(out)
}

const PARENT1: u8 = 1;
const PARENT2: u8 = 2;
const STALE: u8 = 4;
const RESULT: u8 = 8;

/// git merge-base: "paint down to common" algorithm.
/// Returns the best common ancestor(s) of a and b.
pub fn merge_bases(repo: &Repo, a: &Oid, b: &Oid) -> Result<Vec<Oid>> {
    if a == b {
        return Ok(vec![*a]);
    }
    let mut flags: HashMap<Oid, u8> = HashMap::new();
    let mut commits: HashMap<Oid, Commit> = HashMap::new();
    let mut get_commit = |repo: &Repo, o: &Oid| -> Result<Commit> {
        if let Some(c) = commits.get(o) {
            return Ok(c.clone());
        }
        let c = load_commit(repo, o)?;
        commits.insert(*o, c.clone());
        Ok(c)
    };
    let mut queue: std::collections::BinaryHeap<(i64, Oid)> =
        std::collections::BinaryHeap::new();
    flags.insert(*a, PARENT1);
    flags.insert(*b, PARENT2);
    queue.push((get_commit(repo, a)?.committer.time, *a));
    queue.push((get_commit(repo, b)?.committer.time, *b));
    let mut results: Vec<Oid> = Vec::new();
    let mut results_set: HashSet<Oid> = HashSet::new();

    loop {
        // stop when every queued commit is already stale
        if !queue
            .iter()
            .any(|(_, o)| flags.get(o).copied().unwrap_or(0) & STALE == 0)
        {
            break;
        }
        let (_, oid) = match queue.pop() {
            Some(x) => x,
            None => break,
        };
        let f = flags.get(&oid).copied().unwrap_or(0);
        let mut propagate = f & (PARENT1 | PARENT2 | STALE);
        if propagate == (PARENT1 | PARENT2) {
            if f & RESULT == 0 {
                flags.insert(oid, f | RESULT);
                if results_set.insert(oid) {
                    results.push(oid);
                }
            }
            propagate |= STALE;
        }
        if propagate == 0 {
            continue;
        }
        let commit = get_commit(repo, &oid)?;
        for p in &commit.parents {
            let pf = flags.entry(*p).or_insert(0);
            if *pf & propagate == propagate {
                continue;
            }
            *pf |= propagate;
            let pt = get_commit(repo, p)?.committer.time;
            queue.push((pt, *p));
        }
    }

    // Remove redundant results (a result that is an ancestor of another
    // result is not a "best" merge base)
    let mut independent: Vec<Oid> = Vec::new();
    'outer: for (i, r) in results.iter().enumerate() {
        for (j, other) in results.iter().enumerate() {
            if i == j {
                continue;
            }
            if is_ancestor_of(repo, r, other)? {
                continue 'outer;
            }
        }
        independent.push(*r);
    }
    Ok(independent)
}

/// Is `a` an ancestor of (or equal to) `b`?
pub fn is_ancestor_of(repo: &Repo, a: &Oid, b: &Oid) -> Result<bool> {
    if a == b {
        return Ok(true);
    }
    // walk b's ancestors; early exit when we pass below a's date impossible
    // to know — do bounded BFS
    let target = *a;
    let mut seen: HashSet<Oid> = HashSet::new();
    let mut queue: std::collections::VecDeque<Oid> = [*b].into_iter().collect();
    while let Some(o) = queue.pop_front() {
        if o == target {
            return Ok(true);
        }
        if !seen.insert(o) {
            continue;
        }
        let obj = match repo.odb.read_opt(&o)? {
            Some(o) => o,
            None => continue,
        };
        if obj.0 != ObjType::Commit {
            continue;
        }
        let c = Commit::parse(&obj.1)?;
        if repo.is_shallow(&o) {
            continue;
        }
        queue.extend(c.parents.iter().copied());
    }
    Ok(false)
}

/// First-parent chain from `tip` back `n` generations (~N semantics).
pub fn nth_ancestor(repo: &Repo, tip: &Oid, n: usize) -> Result<Oid> {
    let mut cur = *tip;
    for _ in 0..n {
        let c = load_commit(repo, &cur)?;
        cur = *c
            .parents
            .first()
            .ok_or_else(|| GitError::InvalidInput(format!("{} has no parent", cur)))?;
    }
    Ok(cur)
}
