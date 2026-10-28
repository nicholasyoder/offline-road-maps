//! The car routing sections (`docs/format.md`, phase 3 step 3.3): the contraction hierarchy
//! (`RCHE`), the road segments with their geometry (`RGEO`), their names and signs
//! (`RNAM`), the grid that finds segments near a point (`RGRD`), the node copies made by
//! turn restrictions (`RCPY`) and exit numbers (`RJCT`), with `RHDR` saying how they fit.
//!
//! [`Routing`] reads them with positioned reads and budgeted caches of decoded blocks
//! (phase 1 decision 1). [`query`] searches the hierarchy between nodes (step 3.4), and
//! [`trip`] snaps points to segments and routes the legs of a trip between them, and
//! [`guide`] turns a trip into directions (step 3.5). [`matrix`] finds the times between
//! every pair of a trip's stops, and [`order`] the quickest order through them (phase 4
//! step 4.1).

mod guide;
mod matrix;
pub mod order;
mod query;
mod trip;

use std::io;
use std::ops::Range;
use std::sync::Arc;

use serde::{Deserialize, Serialize};

pub use crate::blocked::write_spanning;
use crate::blocked::{Blocked, Cache, string};
use crate::geo::{self, P};
use crate::pack::Pack;
use crate::tile::hilbert;
use crate::varint::{Reader, invalid};
use crate::zframe;
pub use guide::{Kind, Modifier, Names, Road, Side, Step};
pub use matrix::Optimized;
pub use order::{Ends, Matrix};
pub use query::Seed;
pub use trip::{FAR_SNAP_M, Leg, Piece, Snap, Stop, Trip};

/// The first minor version with routing sections; older packs have no routing.
pub const MIN_MINOR: u16 = 5;

pub const RHDR: [u8; 4] = *b"RHDR";
pub const RCHE: [u8; 4] = *b"RCHE";
pub const RGEO: [u8; 4] = *b"RGEO";
pub const RNAM: [u8; 4] = *b"RNAM";
pub const RGRD: [u8; 4] = *b"RGRD";
pub const RCPY: [u8; 4] = *b"RCPY";
pub const RJCT: [u8; 4] = *b"RJCT";

/// Flags of a hierarchy edge (`RCHE`) and of a segment's direction (`RGEO`).
pub use crate::profile::{FERRY, TOLL};
/// The flags a route can avoid.
pub const AVOID: u8 = TOLL | FERRY;
/// `RCHE`: the edge is a shortcut, through its middle node.
pub const SHORTCUT: u8 = 4;
/// `RGEO`: a road a car may use only to start or end a route on it.
pub const RESTRICTED: u8 = 4;
/// `RGEO`: a [`RESTRICTED`] direction entered from a node an open road touches; its weight
/// carries [`crate::profile::RESTRICTED_PENALTY_S`].
pub const ENTRY: u8 = 8;
/// `RGEO`: a route that reaches this direction's start at the plain node (not a copy) and
/// leaves along it turns back the way it came, and pays the U-turn penalty
/// ([`Routing::uturn_penalty_s`]; phase 4 step 4.4).
pub const UTURN: u8 = 16;
/// The `RHDR` setting that holds the U-turn penalty in seconds. Without it there is none,
/// and `RCPY`'s leaves have no U-turn bit.
pub const UTURN_SETTING: &str = "uturn_penalty_s";
/// A segment's class: an index into [`crate::street::CLASSES`], or this for a ferry.
pub const FERRY_CLASS: u8 = 19;
/// Stored points are in units of 2^6 world units, as in `SGEO`.
pub const SHIFT: u32 = 6;
/// The grid's cells are tiles of this zoom, as `SGRD`'s.
pub const GRID_ZOOM: u8 = 14;

/// Decoded blocks kept, in bytes. Step 3.4 measured North America routes: 16 MiB of
/// nodes holds a continental search (8 took short routes from 2.5 to 8.6 ms when asked
/// first); 16 MiB of segments holds a route of a few hundred km (4 took Cincinnati to
/// Cleveland from 0.7 to 14 ms), and more does not help a coast-to-coast route, whose
/// thousand blocks are decoded each time.
const NODE_CACHE: usize = 16 << 20;
const SEGMENT_CACHE: usize = 16 << 20;
const SMALL_CACHE: usize = 1 << 20;

/// `RHDR`: counts and how the hierarchy is laid out. A zstd-compressed JSON object.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct Header {
    pub nodes: u32,
    /// Hierarchy edges, up and down.
    pub edges: u32,
    pub segments: u32,
    pub labels: u32,
    /// Nodes per `RCHE` block; block `k` holds nodes `k × block_nodes` onwards.
    pub block_nodes: u32,
    /// Nodes `0..top_nodes` are the top of the hierarchy, in `top_bytes` of `RCHE` after
    /// its index: one read brings them into the cache.
    pub top_nodes: u32,
    pub top_bytes: u64,
    /// The profile and build settings the graph was made with, for display.
    #[serde(default)]
    pub settings: serde_json::Map<String, serde_json::Value>,
}

impl Header {
    pub fn encode(&self) -> io::Result<Vec<u8>> {
        zframe::to_json_section(self)
    }

    pub fn decode(b: &[u8]) -> io::Result<Self> {
        zframe::from_json_section(b)
    }
}

/// An edge of the hierarchy, from the node whose list holds it: up to a higher node, or,
/// in a down list, from a higher node into it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ChEdge {
    pub other: u32,
    /// Tenths of a second, the restricted-road penalty included.
    pub weight: u32,
    /// [`TOLL`], [`FERRY`], [`SHORTCUT`].
    pub flags: u8,
    /// A shortcut's middle node, or a base edge's segment direction (`segment << 1 |
    /// reversed`).
    pub mid: u32,
}

struct NodeRec {
    /// Its edges in the block's list: up edges `start..start + up`, then down edges to `end`.
    start: u32,
    up: u32,
    end: u32,
    small: bool,
}

/// A decoded `RCHE` block.
pub struct NodeBlock {
    nodes: Vec<NodeRec>,
    edges: Vec<ChEdge>,
}

/// A node's edges.
#[derive(Clone)]
pub struct Node {
    block: Arc<NodeBlock>,
    i: usize,
}

impl Node {
    fn rec(&self) -> &NodeRec {
        &self.block.nodes[self.i]
    }

    pub fn up(&self) -> &[ChEdge] {
        let r = self.rec();
        &self.block.edges[r.start as usize..(r.start + r.up) as usize]
    }

    pub fn down(&self) -> &[ChEdge] {
        let r = self.rec();
        &self.block.edges[(r.start + r.up) as usize..r.end as usize]
    }

    /// In a strongly connected component of fewer than 1,000 nodes (step 3.1).
    pub fn small(&self) -> bool {
        self.rec().small
    }
}

/// One way a segment is driven: from its `a` to its `b` (forward), or back.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Direction {
    /// Driving time in tenths of a second, without the restricted-road penalty.
    pub time_ds: u32,
    /// [`TOLL`], [`FERRY`], [`RESTRICTED`], [`ENTRY`].
    pub flags: u8,
    /// The node a route reaches at the end of this direction when it starts on the
    /// segment: the plain end node, or the copy of it that a turn restriction made for
    /// this approach.
    pub head: u32,
}

impl Direction {
    /// What a route minimizes, as the hierarchy's weights.
    pub fn weight(&self) -> u32 {
        self.time_ds
            + match self.flags & ENTRY {
                0 => 0,
                _ => crate::profile::RESTRICTED_PENALTY_S * 10,
            }
    }
}

/// A stretch of road between two graph nodes, with its geometry: what a base edge of the
/// hierarchy stands for, one or both ways.
#[derive(Clone, Debug, PartialEq)]
pub struct Segment {
    /// Its end nodes, plain (not copies), in the order of its points.
    pub a: u32,
    pub b: u32,
    /// [`crate::street::CLASSES`] index, or [`FERRY_CLASS`].
    pub class: u8,
    pub roundabout: bool,
    /// Its `a` is in a small component.
    pub small: bool,
    /// Its street in `SSTR`, for the name and refs.
    pub street: Option<u32>,
    /// Its `RNAM` label: the name of a way with no street (a ferry), and signs.
    pub label: Option<u32>,
    pub metres: f64,
    pub forward: Direction,
    /// `None` on a one-way road.
    pub backward: Option<Direction>,
    /// Simplified to about 2 m, from `a` to `b`; at least two points.
    pub points: Vec<P>,
}

impl Segment {
    pub fn direction(&self, reversed: bool) -> Option<&Direction> {
        match reversed {
            false => Some(&self.forward),
            true => self.backward.as_ref(),
        }
    }

    fn heap_bytes(&self) -> usize {
        std::mem::size_of::<Self>() + self.points.capacity() * 8
    }
}

/// A way's name and signs (`RNAM`), for ways with no street and for exits.
#[derive(Clone, Debug, Default, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct Label {
    pub name: String,
    /// `ref`, as tagged (`;`-separated).
    pub refs: String,
    pub destination: String,
    pub destination_ref: String,
    pub junction_ref: String,
}

impl Label {
    pub fn is_empty(&self) -> bool {
        self.fields().iter().all(|f| f.is_empty())
    }

    pub fn fields(&self) -> [&String; 5] {
        [
            &self.name,
            &self.refs,
            &self.destination,
            &self.destination_ref,
            &self.junction_ref,
        ]
    }

    fn heap_bytes(&self) -> usize {
        self.fields().iter().map(|f| 24 + f.capacity()).sum()
    }
}

/// What `RGRD` holds for one cell.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Cell {
    /// The segments whose first point is in the cell.
    pub home: Range<u32>,
    /// Segments from other cells that pass through it, ascending.
    pub cross: Vec<u32>,
}

impl Cell {
    pub fn segments(&self) -> impl Iterator<Item = u32> + '_ {
        self.home.clone().chain(self.cross.iter().copied())
    }
}

/// A copy of a node, made for one approach by turn restrictions or the U-turn penalty
/// (`RCPY`).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Copy {
    pub node: u32,
    /// The segment directions (`segment << 1 | reversed`) a route may leave it by.
    pub leaves: Vec<u32>,
    /// Those of `leaves` that turn back the way the copy was reached, and pay the U-turn
    /// penalty.
    pub uturns: Vec<u32>,
}

/// The grid cell of a point.
pub fn cell_xy(p: P) -> (u32, u32) {
    let s = 32 - u32::from(GRID_ZOOM);
    (p[0] >> s, p[1] >> s)
}

/// The cell's Hilbert key.
pub fn cell_key(x: u32, y: u32) -> u32 {
    hilbert(GRID_ZOOM, x, y) as u32
}

/// A route through the hierarchy: its weight, and the segment directions it drives.
#[derive(Clone, Debug, PartialEq)]
pub struct Path {
    pub weight: u32,
    /// `segment << 1 | reversed`, in order.
    pub legs: Vec<u32>,
    /// Nodes the search settled.
    pub settled: usize,
    /// The start and end [`Seed`]s it runs between, by index.
    pub source: usize,
    pub target: usize,
}

/// Bytes the routing readers have read from the pack, for the F3 overlay and checks.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Reads {
    pub nodes: u64,
    pub segments: u64,
    pub other: u64,
}

/// `RCHE`'s index: each block's offset from the section's start, and the end of the last.
/// A block runs to the next one's offset, padding at a chunk boundary included.
struct BlockIndex {
    section: u64,
    at: Vec<u32>,
}

impl BlockIndex {
    fn open(pack: &Pack, nodes: u32, block_nodes: u32) -> io::Result<Self> {
        let s = *pack
            .section(RCHE)
            .ok_or_else(|| invalid("no RCHE section"))?;
        let head = pack.read(s.offset, 8)?;
        let u32_at = |b: &[u8], i: usize| u32::from_le_bytes(b[i..i + 4].try_into().unwrap());
        let (count, n) = (u32_at(&head, 0), u32_at(&head, 4) as usize);
        if count != nodes || n != nodes.div_ceil(block_nodes.max(1)) as usize || s.len < 12 {
            return Err(invalid("RHDR does not match RCHE"));
        }
        let raw = pack.read(s.offset + 8, 4 * (n + 1))?;
        let at: Vec<u32> = (0..=n).map(|k| u32_at(&raw, 4 * k)).collect();
        let head_len = 8 + 4 * (n as u64 + 1);
        if u64::from(at[0]) < head_len
            || at.windows(2).any(|w| w[1] < w[0])
            || u64::from(at[n]) > s.len
        {
            return Err(invalid("bad RCHE block index"));
        }
        Ok(BlockIndex {
            section: s.offset,
            at,
        })
    }

    /// Block `k`'s virtual offset and length.
    fn frame(&self, k: usize) -> (u64, u64) {
        (
            self.section + u64::from(self.at[k]),
            u64::from(self.at[k + 1] - self.at[k]),
        )
    }

    fn blocks(&self) -> usize {
        self.at.len() - 1
    }

    fn heap_bytes(&self) -> usize {
        self.at.capacity() * 4
    }
}

pub struct Routing {
    pack: Arc<Pack>,
    pub header: Header,
    rche: BlockIndex,
    rgeo: Blocked,
    rnam: Blocked,
    rgrd: Blocked,
    rcpy: Blocked,
    rjct: Blocked,
    nodes: Cache<usize, NodeBlock>,
    segments: Cache<usize, Vec<Segment>>,
    labels: Cache<usize, Vec<Label>>,
    cells: Cache<usize, Vec<(u32, Cell)>>,
    copies: Cache<usize, Vec<(u32, Copy)>>,
    junctions: Cache<usize, Vec<(u32, String)>>,
    /// From `RHDR`'s [`UTURN_SETTING`].
    uturn_s: u32,
}

fn need(b: Option<Blocked>, id: [u8; 4]) -> io::Result<Blocked> {
    b.ok_or_else(|| invalid(format!("no {} section", String::from_utf8_lossy(&id))))
}

impl Routing {
    /// Opens the routing sections, or `None` if the pack has none (built before 0.5).
    pub fn open(pack: Arc<Pack>) -> io::Result<Option<Self>> {
        let Some(&hdr) = pack.section(RHDR).filter(|_| pack.minor >= MIN_MINOR) else {
            return Ok(None);
        };
        let header = Header::decode(&pack.read_section(&hdr)?)?;
        if header.block_nodes == 0 {
            return Err(invalid("RHDR has no block size"));
        }
        let rche = BlockIndex::open(&pack, header.nodes, header.block_nodes)?;
        let open = |id| -> io::Result<Blocked> { need(Blocked::open(&pack, id)?, id) };
        let r = Routing {
            rgeo: open(RGEO)?,
            rnam: open(RNAM)?,
            rgrd: open(RGRD)?,
            rcpy: open(RCPY)?,
            rjct: open(RJCT)?,
            pack,
            uturn_s: header
                .settings
                .get(UTURN_SETTING)
                .and_then(|v| v.as_u64())
                .map_or(0, |s| s as u32),
            header,
            rche,
            nodes: Cache::new(NODE_CACHE),
            segments: Cache::new(SEGMENT_CACHE),
            labels: Cache::new(SMALL_CACHE),
            cells: Cache::new(SMALL_CACHE),
            copies: Cache::new(SMALL_CACHE),
            junctions: Cache::new(SMALL_CACHE),
        };
        if r.rgeo.count != r.header.segments || r.rnam.count != r.header.labels {
            return Err(invalid("RHDR does not match RGEO and RNAM"));
        }
        Ok(Some(r))
    }

    /// The same reader with `bytes` for decoded `RCHE` blocks instead of the default.
    pub fn with_node_cache(mut self, bytes: usize) -> Self {
        self.nodes = Cache::new(bytes);
        self
    }

    /// The same reader with `bytes` for decoded `RGEO` blocks instead of the default.
    pub fn with_segment_cache(mut self, bytes: usize) -> Self {
        self.segments = Cache::new(bytes);
        self
    }

    /// Reads the top of the hierarchy once, so the system's file cache holds it: almost
    /// every long route passes through it. Returns the bytes read.
    pub fn warm_top(&self) -> io::Result<u64> {
        let blocks = (self.header.top_nodes.div_ceil(self.header.block_nodes) as usize)
            .min(self.rche.blocks());
        let cap = self.pack.capacity;
        let (mut read, mut b) = (0, 0);
        while b < blocks {
            // Consecutive blocks, up to 1 MiB in one read, never across a chunk boundary.
            let (o, l) = self.rche.frame(b);
            let (start, mut end) = (o, o + l);
            b += 1;
            while b < blocks {
                let (s, l) = self.rche.frame(b);
                let e = s + l;
                if s != end || e - start > 1 << 20 || (e - 1) / cap != start / cap {
                    break;
                }
                end = e;
                b += 1;
            }
            self.pack.read(start, (end - start) as usize)?;
            read += end - start;
        }
        Ok(read)
    }

    /// The hierarchy edges of node `v`.
    pub fn node(&self, v: u32) -> io::Result<Node> {
        if v >= self.header.nodes {
            return Err(invalid(format!("node {v} out of range")));
        }
        let bn = self.header.block_nodes;
        let b = (v / bn) as usize;
        let block = self.nodes.get(b, || {
            let (at, len) = self.rche.frame(b);
            let raw = self.pack.read(at, len as usize)?;
            let first = b as u32 * bn;
            let n = bn.min(self.header.nodes - first);
            let blk = decode_nodes(&raw, first, n, self.header.nodes)?;
            let bytes = blk.nodes.capacity() * 16 + blk.edges.capacity() * 16;
            Ok((blk, bytes, raw.len() as u64))
        })?;
        Ok(Node {
            block,
            i: (v % bn) as usize,
        })
    }

    /// Segment `s`.
    pub fn segment(&self, s: u32) -> io::Result<Segment> {
        self.with_segment(s, Segment::clone)
    }

    /// `f` of segment `s`, without copying it out of the cache.
    pub fn with_segment<T>(&self, s: u32, f: impl FnOnce(&Segment) -> T) -> io::Result<T> {
        let (b, segs) = self.segment_block(s)?;
        segs.get((s - self.rgeo.first(b)) as usize)
            .map(f)
            .ok_or_else(|| invalid(format!("RGEO block {b} is short")))
    }

    /// The `RGEO` block holding segment `s`, and its index.
    fn segment_block(&self, s: u32) -> io::Result<(usize, Arc<Vec<Segment>>)> {
        let Some(b) = self.rgeo.find(s).filter(|_| s < self.rgeo.count) else {
            return Err(invalid(format!("segment {s} out of range")));
        };
        let segs = self.segments.get(b, || {
            let (raw, read) = self.rgeo.read(&self.pack, b)?;
            let v = decode_segments(&raw, self.header.nodes)?;
            let bytes = v.iter().map(Segment::heap_bytes).sum();
            Ok((v, bytes, read))
        })?;
        Ok((b, segs))
    }

    /// Calls `f` for every segment in order, reading each block once.
    pub fn for_each_segment(&self, mut f: impl FnMut(u32, &Segment)) -> io::Result<()> {
        for b in 0..self.rgeo.blocks() {
            let (raw, _) = self.rgeo.read(&self.pack, b)?;
            for (i, s) in decode_segments(&raw, self.header.nodes)?.iter().enumerate() {
                f(self.rgeo.first(b) + i as u32, s);
            }
        }
        Ok(())
    }

    pub fn label(&self, l: u32) -> io::Result<Label> {
        let Some(b) = self.rnam.find(l).filter(|_| l < self.rnam.count) else {
            return Err(invalid(format!("label {l} out of range")));
        };
        let labels = self.labels.get(b, || {
            let (raw, read) = self.rnam.read(&self.pack, b)?;
            let mut r = Reader::new(&raw);
            let mut v = Vec::new();
            while !r.at_end() {
                v.push(Label {
                    name: string(&mut r)?,
                    refs: string(&mut r)?,
                    destination: string(&mut r)?,
                    destination_ref: string(&mut r)?,
                    junction_ref: string(&mut r)?,
                });
            }
            let bytes = v.iter().map(Label::heap_bytes).sum();
            Ok((v, bytes, read))
        })?;
        labels
            .get((l - self.rnam.first(b)) as usize)
            .cloned()
            .ok_or_else(|| invalid(format!("RNAM block {b} is short")))
    }

    /// Cells stored in `RGRD`.
    pub fn cells(&self) -> u32 {
        self.rgrd.count
    }

    /// Cell `(x, y)` at [`GRID_ZOOM`]; empty if not stored.
    pub fn cell(&self, x: u32, y: u32) -> io::Result<Cell> {
        if x >> GRID_ZOOM != 0 || y >> GRID_ZOOM != 0 {
            return Ok(Cell::default());
        }
        let k = cell_key(x, y);
        let Some(b) = self.rgrd.find(k) else {
            return Ok(Cell::default());
        };
        let cells = self.cells.get(b, || {
            let (raw, read) = self.rgrd.read(&self.pack, b)?;
            let v = decode_cells(self.rgrd.first(b), &raw)?;
            let bytes = v.iter().map(|(_, c)| 48 + c.cross.capacity() * 4).sum();
            Ok((v, bytes, read))
        })?;
        Ok(match cells.binary_search_by_key(&k, |c| c.0) {
            Ok(i) => cells[i].1.clone(),
            Err(_) => Cell::default(),
        })
    }

    /// The segments in the cells within `radius_m` of `p` (by their squares), without
    /// repeats.
    pub fn segments_near(&self, p: P, radius_m: f64) -> io::Result<Vec<u32>> {
        let r = (radius_m / geo::metres_per_unit(p))
            .ceil()
            .min(f64::from(u32::MAX)) as u32;
        let s = 32 - u32::from(GRID_ZOOM);
        let lo = |v: u32| v.saturating_sub(r) >> s;
        let hi = |v: u32| v.saturating_add(r) >> s;
        let mut out = Vec::new();
        for y in lo(p[1])..=hi(p[1]) {
            for x in lo(p[0])..=hi(p[0]) {
                out.extend(self.cell(x, y)?.segments());
            }
        }
        out.sort_unstable();
        out.dedup();
        Ok(out)
    }

    /// Calls `f` for every stored cell in Hilbert order, reading each block once.
    pub fn for_each_cell(&self, mut f: impl FnMut(u32, &Cell)) -> io::Result<()> {
        for b in 0..self.rgrd.blocks() {
            let (raw, _) = self.rgrd.read(&self.pack, b)?;
            for (k, c) in decode_cells(self.rgrd.first(b), &raw)? {
                f(k, &c);
            }
        }
        Ok(())
    }

    /// Seconds a U-turn at a junction adds to a route's weight (0: none).
    pub fn uturn_penalty_s(&self) -> u32 {
        self.uturn_s
    }

    /// Whether a hierarchy weight is `directions`, the weights of its segment directions,
    /// plus some U-turn penalties: the hierarchy's edges carry them, the directions do not.
    pub fn adds_up(&self, directions: u32, weight: u32) -> bool {
        match (weight.checked_sub(directions), self.uturn_s * 10) {
            (Some(0), _) => true,
            (Some(extra), u) => u > 0 && extra % u == 0,
            (None, _) => false,
        }
    }

    /// The copies of plain node `v`.
    pub fn copies(&self, v: u32) -> io::Result<Vec<Copy>> {
        let Some(b) = self.rcpy.find(v) else {
            return Ok(Vec::new());
        };
        let list = self.copies.get(b, || {
            let (raw, read) = self.rcpy.read(&self.pack, b)?;
            let v = decode_copies(
                self.rcpy.first(b),
                &raw,
                self.header.nodes,
                self.uturn_s > 0,
            )?;
            let bytes = v
                .iter()
                .map(|(_, c)| 64 + (c.leaves.capacity() + c.uturns.capacity()) * 4)
                .sum();
            Ok((v, bytes, read))
        })?;
        let start = list.partition_point(|(p, _)| *p < v);
        Ok(list[start..]
            .iter()
            .take_while(|(p, _)| *p == v)
            .map(|(_, c)| c.clone())
            .collect())
    }

    /// Calls `f` with each plain node and copy, in order.
    pub fn for_each_copy(&self, mut f: impl FnMut(u32, &Copy)) -> io::Result<()> {
        for b in 0..self.rcpy.blocks() {
            let (raw, _) = self.rcpy.read(&self.pack, b)?;
            for (p, c) in decode_copies(
                self.rcpy.first(b),
                &raw,
                self.header.nodes,
                self.uturn_s > 0,
            )? {
                f(p, &c);
            }
        }
        Ok(())
    }

    /// The exit number at node `v` (`highway=motorway_junction` + `ref`).
    pub fn junction(&self, v: u32) -> io::Result<Option<String>> {
        let Some(b) = self.rjct.find(v) else {
            return Ok(None);
        };
        let list = self.junctions.get(b, || {
            let (raw, read) = self.rjct.read(&self.pack, b)?;
            let v = decode_junctions(self.rjct.first(b), &raw)?;
            let bytes = v.iter().map(|(_, s)| 32 + s.capacity()).sum();
            Ok((v, bytes, read))
        })?;
        Ok(list
            .binary_search_by_key(&v, |j| j.0)
            .ok()
            .map(|i| list[i].1.clone()))
    }

    /// Junction entries stored.
    pub fn junctions(&self) -> u32 {
        self.rjct.count
    }

    /// Bytes read from the pack so far.
    pub fn reads(&self) -> Reads {
        use std::sync::atomic::Ordering::Relaxed;
        Reads {
            nodes: self.nodes.bytes_read.load(Relaxed),
            segments: self.segments.bytes_read.load(Relaxed),
            other: [
                &self.labels.bytes_read,
                &self.cells.bytes_read,
                &self.copies.bytes_read,
                &self.junctions.bytes_read,
            ]
            .iter()
            .map(|b| b.load(Relaxed))
            .sum(),
        }
    }

    /// RAM held: the block indexes and the caches.
    pub fn heap_bytes(&self) -> usize {
        self.rche.heap_bytes()
            + [&self.rgeo, &self.rnam, &self.rgrd, &self.rcpy, &self.rjct]
                .iter()
                .map(|b| b.heap_bytes())
                .sum::<usize>()
            + self.nodes.held()
            + self.segments.held()
            + self.labels.held()
            + self.cells.held()
            + self.copies.held()
            + self.junctions.held()
    }
}

fn node_at(first: u32, delta: i32, nodes: u32) -> io::Result<u32> {
    let v = i64::from(first) + i64::from(delta);
    if (0..i64::from(nodes)).contains(&v) {
        Ok(v as u32)
    } else {
        Err(invalid("node out of range"))
    }
}

/// An `RCHE` block of `n` nodes from `first`.
pub fn decode_nodes(raw: &[u8], first: u32, n: u32, nodes: u32) -> io::Result<NodeBlock> {
    let mut r = Reader::new(raw);
    let mut recs = Vec::with_capacity(n as usize);
    let mut edges = Vec::new();
    // Base edges' segment directions are deltas from the block's previous one.
    let mut seg = 0u32;
    for k in 0..n {
        let v = first + k;
        let head = r.varint()?;
        let (up, down) = (head >> 1, r.varint()?);
        if up + down > raw.len() as u64 {
            return Err(invalid("RCHE edge count larger than the data"));
        }
        let start = edges.len() as u32;
        for count in [up, down] {
            let mut prev = v;
            for _ in 0..count {
                let other = node_at(prev, r.zvarint()?, nodes)?;
                let wf = r.varint()?;
                let flags = (wf & 7) as u8;
                let mid = match flags & SHORTCUT {
                    0 => {
                        seg = seg.wrapping_add(r.zvarint()? as u32);
                        seg
                    }
                    _ => node_at(v, r.zvarint()?, nodes)?,
                };
                edges.push(ChEdge {
                    other,
                    weight: u32::try_from(wf >> 3).map_err(|_| invalid("RCHE weight"))?,
                    flags,
                    mid,
                });
                prev = other;
            }
        }
        recs.push(NodeRec {
            start,
            up: up as u32,
            end: edges.len() as u32,
            small: head & 1 != 0,
        });
    }
    // Padding before a chunk boundary follows the last block of a chunk.
    if r.b[r.i..].iter().any(|&b| b != 0) {
        return Err(invalid("RCHE block has trailing bytes"));
    }
    Ok(NodeBlock { nodes: recs, edges })
}

fn direction(r: &mut Reader, end: u32, nodes: u32) -> io::Result<Direction> {
    let time_ds = u32::try_from(r.varint()?).map_err(|_| invalid("RGEO time"))?;
    let flags = u8::try_from(r.varint()?).map_err(|_| invalid("RGEO flags"))?;
    let head = match r.varint()? {
        0 => end,
        h if h <= u64::from(nodes) => h as u32 - 1,
        _ => return Err(invalid("RGEO head out of range")),
    };
    Ok(Direction {
        time_ds,
        flags,
        head,
    })
}

fn option(v: u64) -> io::Result<Option<u32>> {
    match v {
        0 => Ok(None),
        v => u32::try_from(v - 1)
            .map(Some)
            .map_err(|_| invalid("RGEO index out of range")),
    }
}

/// An `RGEO` block.
pub fn decode_segments(raw: &[u8], nodes: u32) -> io::Result<Vec<Segment>> {
    let mut r = Reader::new(raw);
    let mut out = Vec::new();
    let (mut a, mut cur) = (0u32, [0i64; 2]);
    while !r.at_end() {
        a = node_at(a, r.zvarint()?, nodes)?;
        let b = node_at(a, r.zvarint()?, nodes)?;
        let bits = r.varint()?;
        let street = option(r.varint()?)?;
        // Label 0 is the empty one, so the field is the label itself.
        let label = match r.varint()? {
            0 => None,
            l => Some(u32::try_from(l).map_err(|_| invalid("RGEO label"))?),
        };
        let metres = r.varint()? as f64 / 10.0;
        let forward = direction(&mut r, b, nodes)?;
        let backward = match bits & 1 {
            0 => Some(direction(&mut r, a, nodes)?),
            _ => None,
        };
        let n = r.count()?;
        if n < 2 || n > raw.len() {
            return Err(invalid("bad RGEO point count"));
        }
        let mut points = Vec::with_capacity(n);
        for _ in 0..n {
            cur[0] += i64::from(r.zvarint()?);
            cur[1] += i64::from(r.zvarint()?);
            let q = |c: i64| -> io::Result<u32> {
                u32::try_from(c << SHIFT).map_err(|_| invalid("RGEO point out of range"))
            };
            points.push([q(cur[0])?, q(cur[1])?]);
        }
        out.push(Segment {
            a,
            b,
            class: u8::try_from(bits >> 3).map_err(|_| invalid("RGEO class"))?,
            roundabout: bits & 4 != 0,
            small: bits & 2 != 0,
            street,
            label,
            metres,
            forward,
            backward,
            points,
        });
    }
    Ok(out)
}

/// An `RGRD` block.
pub fn decode_cells(first: u32, raw: &[u8]) -> io::Result<Vec<(u32, Cell)>> {
    let mut r = Reader::new(raw);
    let mut home = u32::try_from(r.varint()?).map_err(|_| invalid("RGRD segment"))?;
    let mut out = Vec::new();
    let (mut k, mut cross_first) = (first, 0u32);
    while !r.at_end() {
        k = k
            .checked_add(r.varint()? as u32)
            .ok_or_else(|| invalid("RGRD key out of range"))?;
        let nhome = u32::try_from(r.varint()?).map_err(|_| invalid("RGRD count"))?;
        let ncross = r.count()?;
        if ncross > raw.len() {
            return Err(invalid("RGRD count larger than the data"));
        }
        let mut cross = Vec::with_capacity(ncross);
        if ncross > 0 {
            cross_first = cross_first.wrapping_add(r.zvarint()? as u32);
            let mut s = cross_first;
            cross.push(s);
            for _ in 1..ncross {
                s = s.wrapping_add(r.varint()? as u32);
                cross.push(s);
            }
        }
        let end = home
            .checked_add(nhome)
            .ok_or_else(|| invalid("RGRD segment out of range"))?;
        out.push((
            k,
            Cell {
                home: home..end,
                cross,
            },
        ));
        home = end;
    }
    Ok(out)
}

/// An `RCPY` block: plain nodes and their copies. With `uturn`, each leaf has the U-turn bit
/// below it.
pub fn decode_copies(
    first: u32,
    raw: &[u8],
    nodes: u32,
    uturn: bool,
) -> io::Result<Vec<(u32, Copy)>> {
    let mut r = Reader::new(raw);
    let mut out = Vec::new();
    let mut plain = first;
    while !r.at_end() {
        plain = plain
            .checked_add(r.varint()? as u32)
            .filter(|&p| p < nodes)
            .ok_or_else(|| invalid("RCPY node out of range"))?;
        let node = node_at(plain, r.zvarint()?, nodes)?;
        let n = r.count()?;
        if n > raw.len() {
            return Err(invalid("RCPY count larger than the data"));
        }
        let mut leaves = Vec::with_capacity(n);
        let mut uturns = Vec::new();
        let mut s = 0u32;
        for i in 0..n {
            let d = r.varint()? as u32;
            s = if i == 0 { d } else { s.wrapping_add(d) };
            match uturn {
                true => {
                    leaves.push(s >> 1);
                    if s & 1 != 0 {
                        uturns.push(s >> 1);
                    }
                }
                false => leaves.push(s),
            }
        }
        out.push((
            plain,
            Copy {
                node,
                leaves,
                uturns,
            },
        ));
    }
    Ok(out)
}

/// An `RJCT` block.
pub fn decode_junctions(first: u32, raw: &[u8]) -> io::Result<Vec<(u32, String)>> {
    let mut r = Reader::new(raw);
    let mut out = Vec::new();
    let mut v = first;
    while !r.at_end() {
        v = v
            .checked_add(r.varint()? as u32)
            .ok_or_else(|| invalid("RJCT node out of range"))?;
        out.push((v, string(&mut r)?));
    }
    Ok(out)
}
