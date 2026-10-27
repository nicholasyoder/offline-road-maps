//! Strongly connected components (Tarjan's algorithm, without recursion), so the nodes in
//! small ones can be marked: a gated community, an island parking lot, a mapping error.
//! Snapping (step 3.4) prefers the large network, so a route never starts in a pocket it
//! cannot leave.

use serde::{Deserialize, Serialize};

use super::Csr;

#[derive(Default, Serialize, Deserialize)]
pub struct ComponentStats {
    pub count: u64,
    pub largest: u64,
    /// Components under the small size, and the nodes in them.
    pub small: u64,
    pub small_nodes: u64,
    /// Components of 1, 2–9, 10–99, 100–999 and 1000 or more nodes.
    pub by_size: [u64; 5],
}

const NONE: u32 = u32::MAX;

struct Tarjan {
    index: Vec<u32>,
    low: Vec<u32>,
    on_stack: Vec<bool>,
    stack: Vec<u32>,
    /// (node, position of its next edge)
    calls: Vec<(u32, u32)>,
    next: u32,
}

impl Tarjan {
    fn visit(&mut self, v: u32, first_edge: u32) {
        self.index[v as usize] = self.next;
        self.low[v as usize] = self.next;
        self.next += 1;
        self.stack.push(v);
        self.on_stack[v as usize] = true;
        self.calls.push((v, first_edge));
    }
}

/// Each node's component, numbered from 0, and the number of components.
pub fn components(g: &Csr) -> (Vec<u32>, u32) {
    let n = g.nodes();
    let mut t = Tarjan {
        index: vec![NONE; n],
        low: vec![0; n],
        on_stack: vec![false; n],
        stack: Vec::new(),
        calls: Vec::new(),
        next: 0,
    };
    let mut comp = vec![NONE; n];
    let mut count = 0u32;
    for root in 0..n as u32 {
        if t.index[root as usize] != NONE {
            continue;
        }
        t.visit(root, g.first[root as usize]);
        while let Some(&(v, pos)) = t.calls.last() {
            if pos < g.first[v as usize + 1] {
                t.calls.last_mut().unwrap().1 += 1;
                let w = g.edges[pos as usize].to;
                if t.index[w as usize] == NONE {
                    t.visit(w, g.first[w as usize]);
                } else if t.on_stack[w as usize] {
                    t.low[v as usize] = t.low[v as usize].min(t.index[w as usize]);
                }
                continue;
            }
            t.calls.pop();
            if let Some(&(p, _)) = t.calls.last() {
                t.low[p as usize] = t.low[p as usize].min(t.low[v as usize]);
            }
            if t.low[v as usize] == t.index[v as usize] {
                loop {
                    let w = t.stack.pop().unwrap();
                    t.on_stack[w as usize] = false;
                    comp[w as usize] = count;
                    if w == v {
                        break;
                    }
                }
                count += 1;
            }
        }
    }
    (comp, count)
}

/// Whether each node is in a component of fewer than `small` nodes, and the counts.
pub fn mark_small(g: &Csr, small: u32) -> (Vec<bool>, ComponentStats) {
    let (comp, count) = components(g);
    let mut sizes = vec![0u32; count as usize];
    for &c in &comp {
        sizes[c as usize] += 1;
    }
    let mut st = ComponentStats {
        count: u64::from(count),
        largest: sizes.iter().copied().max().unwrap_or(0).into(),
        ..ComponentStats::default()
    };
    for &s in &sizes {
        let bucket = match s {
            1 => 0,
            2..=9 => 1,
            10..=99 => 2,
            100..=999 => 3,
            _ => 4,
        };
        st.by_size[bucket] += 1;
        if s < small {
            st.small += 1;
            st.small_nodes += u64::from(s);
        }
    }
    let marks = comp.iter().map(|&c| sizes[c as usize] < small).collect();
    (marks, st)
}
