//! Myers diff (O(ND), linear space via middle-snake), unified diff output,
//! and three-way merge.

use crate::object::Oid;

// ============================== line diff ==============================

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Op {
    Keep,
    Delete,
    Insert,
}

/// Split into lines, each slice INCLUDING its trailing '\n' (except possibly
/// the last line). This preserves "\ No newline at end of file" semantics.
pub fn split_lines(data: &[u8]) -> Vec<&[u8]> {
    let mut out = Vec::new();
    let mut start = 0;
    for (i, &b) in data.iter().enumerate() {
        if b == b'\n' {
            out.push(&data[start..=i]);
            start = i + 1;
        }
    }
    if start < data.len() {
        out.push(&data[start..]);
    }
    out
}

/// Myers diff producing per-line ops: Keep(a) / Delete(a) / Insert(b).
/// Ops are emitted in order; Keep/Delete consume `a`, Keep/Insert consume `b`.
pub fn diff(a: &[&[u8]], b: &[&[u8]]) -> Vec<(Op, usize)> {
    let mut ops = Vec::new();
    diff_rec(a, b, 0, 0, &mut ops);
    // merge consecutive keeps? we return raw per-line ops
    ops
}

fn diff_rec(
    a: &[&[u8]],
    b: &[&[u8]],
    aoff: usize,
    boff: usize,
    ops: &mut Vec<(Op, usize)>,
) {
    // strip common prefix
    let mut pre = 0;
    while pre < a.len() && pre < b.len() && a[pre] == b[pre] {
        ops.push((Op::Keep, aoff + pre));
        pre += 1;
    }
    let a = &a[pre..];
    let b = &b[pre..];
    // strip common suffix
    let mut suf = 0;
    while suf < a.len() && suf < b.len() && a[a.len() - 1 - suf] == b[b.len() - 1 - suf] {
        suf += 1;
    }
    let a = &a[..a.len() - suf];
    let b = &b[..b.len() - suf];

    let base = ops.len();
    if a.is_empty() {
        for j in 0..b.len() {
            ops.push((Op::Insert, boff + pre + j));
        }
    } else if b.is_empty() {
        for i in 0..a.len() {
            ops.push((Op::Delete, aoff + pre + i));
        }
    } else {
        match middle_snake(a, b) {
            Some((x1, y1, x2, y2)) => {
                diff_rec(&a[..x1], &b[..y1], aoff + pre, boff + pre, ops);
                for i in x1..x2 {
                    ops.push((Op::Keep, aoff + pre + i));
                }
                diff_rec(&a[x2..], &b[y2..], aoff + pre + x2, boff + pre + y2, ops);
            }
            // too expensive: emit a valid non-minimal diff
            None => {
                for i in 0..a.len() {
                    ops.push((Op::Delete, aoff + pre + i));
                }
                for j in 0..b.len() {
                    ops.push((Op::Insert, boff + pre + j));
                }
            }
        }
    }
    let _ = base;
    // emit common suffix
    for i in 0..suf {
        ops.push((Op::Keep, aoff + pre + a.len() + i));
    }
}

/// Cap on Myers edit-script search depth (like xdiff's XDF_MAX_COST).
/// Beyond this the diff falls back to a valid non-minimal form.
const DIFF_MAX_COST: isize = 4096;

/// Find the middle snake: returns (x1, y1, x2, y2) where the snake runs
/// a[x1..x2] == b[y1..y2]. None when the search exceeds DIFF_MAX_COST.
fn middle_snake(a: &[&[u8]], b: &[&[u8]]) -> Option<(usize, usize, usize, usize)> {
    let n = a.len() as isize;
    let m = b.len() as isize;
    let max = n + m;
    let delta = n - m;
    let odd = delta % 2 != 0;
    let size = (2 * max + 1) as usize;
    let off = max; // index offset for k in [-max, max]
    let mut vf = vec![0isize; size];
    // vb is indexed by diagonal k which ranges over [delta-d, delta+d];
    // |k| can reach |delta| + (max+1)/2 <= 2*max+1, so use a wider window.
    let mut vb = vec![0isize; (4 * max + 3) as usize];
    let boff = 2 * max + 1;
    // forward: vf[k] = furthest x reachable on diagonal k (k=x-y)
    // backward: vb[k] = smallest x reachable on diagonal k going back
    // from (n,m). Backward diagonals are centered on delta.
    vf[(off + 1) as usize] = 0;
    vb[(boff + delta + 1) as usize] = n + 1; // sentinel: d=0 starts at (n,m)
    let d_max = ((max + 1) / 2).min(DIFF_MAX_COST);
    for d in 0..=d_max {
        // ---- forward: diagonals -d..d ----
        let mut k = -d;
        while k <= d {
            let ki = (off + k) as usize;
            let mut x = if k == -d || (k != d && vf[ki - 1] < vf[ki + 1]) {
                vf[ki + 1]
            } else {
                vf[ki - 1] + 1
            };
            let mut y = x - k;
            let x0 = x;
            let y0 = y;
            while x < n && y < m && a[x as usize] == b[y as usize] {
                x += 1;
                y += 1;
            }
            vf[ki] = x;
            if odd
                && k >= delta - (d - 1)
                && k <= delta + (d - 1)
                && vb[(boff + k) as usize] <= x
            {
                return Some((x0 as usize, y0 as usize, x as usize, y as usize));
            }
            k += 2;
        }
        // ---- backward: diagonals delta-d..delta+d ----
        // From (x',y') on k+1 a left-move lands on k at (x'-1, y');
        // from k-1 an up-move lands on k at (x', y'-1) — same x.
        // So vb[k] = min(vb[k-1], vb[k+1] - 1), tracking the smallest x
        // reachable on each diagonal (poisoned negative values lose to
        // any real in-grid predecessor).
        let mut k = delta - d;
        while k <= delta + d {
            let ki = (boff + k) as usize;
            let mut x = if k == delta - d {
                vb[ki + 1] - 1
            } else if k == delta + d {
                vb[ki - 1]
            } else {
                vb[ki - 1].min(vb[ki + 1] - 1)
            };
            let mut y = x - k;
            let x0 = x;
            let y0 = y;
            while x > 0 && y > 0 && a[(x - 1) as usize] == b[(y - 1) as usize] {
                x -= 1;
                y -= 1;
            }
            vb[ki] = x;
            if !odd && k >= -d && k <= d && vf[(off + k) as usize] >= x {
                return Some((x as usize, y as usize, x0 as usize, y0 as usize));
            }
            k += 2;
        }
    }
    if (max + 1) / 2 <= DIFF_MAX_COST {
        // provably reachable for valid inputs
        Some((0, 0, n as usize, m as usize))
    } else {
        None
    }
}

// ============================== changed regions ==============================

/// Coalesce per-line ops into changed regions: (a_start, a_end, b_start, b_end)
/// in line indices, where a[a_start..a_end] was replaced by b[b_start..b_end].
#[derive(Debug, Clone)]
pub struct Region {
    pub a_start: usize,
    pub a_end: usize,
    pub b_start: usize,
    pub b_end: usize,
}

pub fn changed_regions(ops: &[(Op, usize)]) -> Vec<Region> {
    let mut out: Vec<Region> = Vec::new();
    let mut ai = 0usize;
    let mut bi = 0usize;
    let mut cur: Option<Region> = None;
    for (op, _) in ops {
        match op {
            Op::Keep => {
                if let Some(r) = cur.take() {
                    out.push(r);
                }
                ai += 1;
                bi += 1;
            }
            Op::Delete => {
                let r = cur.get_or_insert(Region {
                    a_start: ai,
                    a_end: ai,
                    b_start: bi,
                    b_end: bi,
                });
                ai += 1;
                r.a_end = ai;
            }
            Op::Insert => {
                let r = cur.get_or_insert(Region {
                    a_start: ai,
                    a_end: ai,
                    b_start: bi,
                    b_end: bi,
                });
                bi += 1;
                r.b_end = bi;
            }
        }
    }
    if let Some(r) = cur.take() {
        out.push(r);
    }
    out
}

// ============================== unified output ==============================

pub struct FilePatch {
    pub old_path: Option<String>,
    pub new_path: Option<String>,
    pub old_mode: Option<u32>,
    pub new_mode: Option<u32>,
    pub old_oid: Option<Oid>,
    pub new_oid: Option<Oid>,
    pub hunks: Vec<String>, // rendered hunk text
    pub binary: bool,
    pub is_new: bool,
    pub is_delete: bool,
}

pub fn is_binary(data: &[u8]) -> bool {
    data.iter().take(8000).any(|&b| b == 0)
}

/// Render hunks between two blobs into unified-diff body text.
pub fn render_hunks(a_data: &[u8], b_data: &[u8], context: usize) -> Vec<String> {
    if is_binary(a_data) || is_binary(b_data) {
        return Vec::new();
    }
    let a_lines = split_lines(a_data);
    let b_lines = split_lines(b_data);
    let ops = diff(&a_lines, &b_lines);
    let regions = changed_regions(&ops);
    if regions.is_empty() {
        return Vec::new();
    }
    // group regions into hunks: merge if gap <= 2*context
    let mut hunks: Vec<(usize, usize)> = Vec::new(); // (region_start, region_end)
    let mut i = 0;
    while i < regions.len() {
        let start = i;
        let mut end = i;
        while end + 1 < regions.len()
            && regions[end + 1].a_start - regions[end].a_end <= 2 * context
        {
            end += 1;
        }
        hunks.push((start, end));
        i = end + 1;
    }
    let mut out = Vec::new();
    for (rs, re) in hunks {
        let a_lo = regions[rs].a_start.saturating_sub(context);
        let a_hi = (regions[re].a_end + context).min(a_lines.len());
        let b_lo = regions[rs].b_start.saturating_sub(context);
        let b_hi = (regions[re].b_end + context).min(b_lines.len());
        let a_count = a_hi - a_lo;
        let b_count = b_hi - b_lo;
        let a_disp = if a_count == 0 { a_lo } else { a_lo + 1 };
        let b_disp = if b_count == 0 { b_lo } else { b_lo + 1 };
        let mut h = format!(
            "@@ -{} +{} @@\n",
            if a_count == 1 {
                format!("{}", a_disp)
            } else {
                format!("{},{}", a_disp, a_count)
            },
            if b_count == 1 {
                format!("{}", b_disp)
            } else {
                format!("{},{}", b_disp, b_count)
            }
        );
        // walk regions rs..=re emitting context/del/ins lines
        let mut ai = a_lo;
        let mut bj = b_lo;
        for ri in rs..=re {
            let r = &regions[ri];
            while ai < r.a_start {
                write_line(&mut h, ' ', a_lines[ai]);
                ai += 1;
                bj += 1;
            }
            while ai < r.a_end {
                write_line(&mut h, '-', a_lines[ai]);
                ai += 1;
            }
            while bj < r.b_end {
                write_line(&mut h, '+', b_lines[bj]);
                bj += 1;
            }
        }
        while ai < a_hi {
            write_line(&mut h, ' ', a_lines[ai]);
            ai += 1;
        }
        out.push(h);
    }
    out
}

fn write_line(h: &mut String, prefix: char, line: &[u8]) {
    h.push(prefix);
    let s = String::from_utf8_lossy(line);
    h.push_str(&s);
    if !line.ends_with(b"\n") {
        h.push('\n');
        h.push_str("\\ No newline at end of file\n");
    }
}

/// Render a complete patch for one file pair (headers + hunks).
pub fn render_patch(p: &FilePatch) -> String {
    let mut out = String::new();
    let a = p.old_path.as_deref().unwrap_or("/dev/null");
    let b = p.new_path.as_deref().unwrap_or("/dev/null");
    out.push_str(&format!("diff --git a/{} b/{}\n", a, b));
    if p.is_new {
        if let Some(m) = p.new_mode {
            out.push_str(&format!("new file mode {:o}\n", m));
        }
    } else if p.is_delete {
        if let Some(m) = p.old_mode {
            out.push_str(&format!("deleted file mode {:o}\n", m));
        }
    } else if p.old_mode != p.new_mode {
        if let (Some(om), Some(nm)) = (p.old_mode, p.new_mode) {
            out.push_str(&format!("old mode {:o}\nnew mode {:o}\n", om, nm));
        }
    }
    // git emits "index <a7>..<b7>" always when both sides have an oid, and
    // with a zero side for adds/deletes (when the blob exists).
    let zero = Oid::ZERO;
    let oo = p.old_oid.unwrap_or(zero);
    let no = p.new_oid.unwrap_or(zero);
    if (p.is_new && p.new_oid.is_some())
        || (p.is_delete && p.old_oid.is_some())
        || (p.old_oid.is_some() && p.new_oid.is_some())
    {
        out.push_str(&format!("index {}..{}", oo.short(7), no.short(7)));
        if !p.is_new && !p.is_delete && p.old_mode == p.new_mode {
            if let Some(m) = p.new_mode {
                out.push_str(&format!(" {:o}", m));
            }
        }
        out.push('\n');
    }
    if p.binary {
        out.push_str(&format!("Binary files a/{} and b/{} differ\n", a, b));
        return out;
    }
    if p.hunks.is_empty() {
        return out;
    }
    let minus = if p.is_new {
        "/dev/null".to_string()
    } else {
        format!("a/{}", a)
    };
    let plus = if p.is_delete {
        "/dev/null".to_string()
    } else {
        format!("b/{}", b)
    };
    out.push_str(&format!("--- {}\n+++ {}\n", minus, plus));
    for h in &p.hunks {
        out.push_str(h);
    }
    out
}

// ============================== three-way merge ==============================

pub enum MergeResult {
    Clean(Vec<Vec<u8>>),
    Conflicted(Vec<Vec<u8>>),
}

impl MergeResult {
    pub fn data(&self) -> Vec<u8> {
        match self {
            MergeResult::Clean(v) | MergeResult::Conflicted(v) => {
                v.concat()
            }
        }
    }
    pub fn is_clean(&self) -> bool {
        matches!(self, MergeResult::Clean(_))
    }
}

/// Three-way merge of line contents. `label_ours`/`label_theirs` appear in
/// conflict markers.
pub fn merge3(
    base: &[u8],
    ours: &[u8],
    theirs: &[u8],
    label_ours: &str,
    label_theirs: &str,
) -> MergeResult {
    let base_lines = split_lines(base);
    let our_lines = split_lines(ours);
    let their_lines = split_lines(theirs);
    let our_regions = changed_regions(&diff(&base_lines, &our_lines));
    let their_regions = changed_regions(&diff(&base_lines, &their_lines));

    let mut out: Vec<Vec<u8>> = Vec::new();
    let mut conflicted = false;
    let mut bi = 0usize; // base line cursor
    let mut ai = 0usize;
    let mut ti = 0usize;

    // region application within a hunk; Region.a_* = base coords, b_* = src
    let apply = |regions: &[Region],
                 lo: usize,
                 hi: usize,
                 src: &[&[u8]],
                 base: &[&[u8]]|
     -> Vec<Vec<u8>> {
        let mut v = Vec::new();
        let mut cur = lo;
        for r in regions {
            if r.a_start > cur {
                for l in &base[cur..r.a_start] {
                    v.push(l.to_vec());
                }
            }
            for l in &src[r.b_start..r.b_end] {
                v.push(l.to_vec());
            }
            cur = r.a_end;
        }
        if hi > cur {
            for l in &base[cur..hi] {
                v.push(l.to_vec());
            }
        }
        v
    };

    loop {
        let next_our = our_regions.get(ai);
        let next_their = their_regions.get(ti);
        if next_our.is_none() && next_their.is_none() {
            for l in &base_lines[bi..] {
                out.push(l.to_vec());
            }
            break;
        }
        // find earliest hunk start in base coordinates
        let hunk_start = match (next_our, next_their) {
            (Some(o), Some(t)) => o.a_start.min(t.a_start),
            (Some(o), None) => o.a_start,
            (None, Some(t)) => t.a_start,
            (None, None) => unreachable!(),
        };
        for l in &base_lines[bi..hunk_start] {
            out.push(l.to_vec());
        }
        // extend hunk while regions overlap in base space
        let mut hunk_end = hunk_start;
        let mut a_in: Vec<Region> = Vec::new();
        let mut t_in: Vec<Region> = Vec::new();
        loop {
            let mut grew = false;
            while let Some(o) = our_regions.get(ai) {
                if o.a_start <= hunk_end {
                    hunk_end = hunk_end.max(o.a_end);
                    a_in.push(o.clone());
                    ai += 1;
                    grew = true;
                } else {
                    break;
                }
            }
            while let Some(t) = their_regions.get(ti) {
                if t.a_start <= hunk_end {
                    hunk_end = hunk_end.max(t.a_end);
                    t_in.push(t.clone());
                    ti += 1;
                    grew = true;
                } else {
                    break;
                }
            }
            if !grew {
                break;
            }
        }
        let ours_text = apply(&a_in, hunk_start, hunk_end, &our_lines, &base_lines);
        let theirs_text = apply(&t_in, hunk_start, hunk_end, &their_lines, &base_lines);
        let a_changed = !a_in.is_empty();
        let t_changed = !t_in.is_empty();
        if !a_changed {
            out.extend(theirs_text);
        } else if !t_changed || ours_text == theirs_text {
            out.extend(ours_text);
        } else {
            conflicted = true;
            out.push(format!("<<<<<<< {}\n", label_ours).into_bytes());
            out.extend(ours_text);
            out.push(b"=======\n".to_vec());
            out.extend(theirs_text);
            out.push(format!(">>>>>>> {}\n", label_theirs).into_bytes());
        }
        bi = hunk_end;
    }
    if conflicted {
        MergeResult::Conflicted(out)
    } else {
        MergeResult::Clean(out)
    }
}
