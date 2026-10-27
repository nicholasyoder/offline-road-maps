use osmpbf::Element;

use super::*;

/// Protobuf encoding, enough for a small PBF with uncompressed blobs.
fn varint(out: &mut Vec<u8>, mut v: u64) {
    while v >= 0x80 {
        out.push(v as u8 | 0x80);
        v >>= 7;
    }
    out.push(v as u8);
}

fn int_field(out: &mut Vec<u8>, field: u64, v: u64) {
    varint(out, field << 3);
    varint(out, v);
}

fn bytes_field(out: &mut Vec<u8>, field: u64, data: &[u8]) {
    varint(out, field << 3 | 2);
    varint(out, data.len() as u64);
    out.extend_from_slice(data);
}

fn zigzag(v: i64) -> u64 {
    ((v << 1) ^ (v >> 63)) as u64
}

/// One primitive group of nodes, ways or relations.
enum Group {
    Nodes(Vec<i64>),
    Ways(Vec<i64>),
    Relations(Vec<i64>),
}

fn group(g: &Group) -> Vec<u8> {
    let mut out = Vec::new();
    match g {
        Group::Nodes(ids) => {
            for &id in ids {
                let mut n = Vec::new();
                int_field(&mut n, 1, zigzag(id));
                int_field(&mut n, 8, zigzag(400_000_000));
                int_field(&mut n, 9, zigzag(-800_000_000));
                bytes_field(&mut out, 1, &n);
            }
        }
        Group::Ways(ids) => {
            for &id in ids {
                let mut w = Vec::new();
                int_field(&mut w, 1, id as u64);
                let mut refs = Vec::new();
                varint(&mut refs, zigzag(1));
                bytes_field(&mut w, 8, &refs);
                bytes_field(&mut out, 3, &w);
            }
        }
        Group::Relations(ids) => {
            for &id in ids {
                let mut r = Vec::new();
                int_field(&mut r, 1, id as u64);
                bytes_field(&mut out, 4, &r);
            }
        }
    }
    out
}

fn blob(out: &mut Vec<u8>, kind: &str, data: &[u8]) {
    let mut b = Vec::new();
    bytes_field(&mut b, 1, data);
    int_field(&mut b, 2, data.len() as u64);
    let mut h = Vec::new();
    bytes_field(&mut h, 1, kind.as_bytes());
    int_field(&mut h, 3, b.len() as u64);
    out.extend_from_slice(&(h.len() as u32).to_be_bytes());
    out.extend_from_slice(&h);
    out.extend_from_slice(&b);
}

/// A PBF of `blocks`, each a list of groups, with a header that says it is sorted or not.
fn pbf(sorted: bool, blocks: &[Vec<Group>]) -> Vec<u8> {
    let mut out = Vec::new();
    let mut header = Vec::new();
    bytes_field(&mut header, 4, b"OsmSchema-V0.6");
    if sorted {
        bytes_field(&mut header, 5, b"Sort.Type_then_ID");
    }
    blob(&mut out, "OSMHeader", &header);
    for groups in blocks {
        let mut block = Vec::new();
        bytes_field(&mut block, 1, &[]);
        for g in groups {
            bytes_field(&mut block, 2, &group(g));
        }
        blob(&mut out, "OSMData", &block);
    }
    out
}

/// Nodes 1–9, ways 10–15 and relations 20–23 in nine blocks; block 3 holds the last nodes
/// and the first ways, block 6 the last ways and the first relations.
fn blocks() -> Vec<Vec<Group>> {
    use Group::*;
    vec![
        vec![Nodes(vec![1, 2])],
        vec![Nodes(vec![3, 4])],
        vec![Nodes(vec![5, 6])],
        vec![Nodes(vec![7, 8, 9]), Ways(vec![10])],
        vec![Ways(vec![11, 12])],
        vec![Ways(vec![13])],
        vec![Ways(vec![14, 15]), Relations(vec![20])],
        vec![Relations(vec![21, 22])],
        vec![Relations(vec![23])],
    ]
}

fn write(dir: &Path, name: &str, data: &[u8]) -> PathBuf {
    let path = dir.join(name);
    std::fs::write(&path, data).unwrap();
    path
}

/// The sorted IDs of the elements of `kinds` a pass over them sees, and the number of data
/// blocks it reads.
fn ids(pbf: &Pbf, kinds: Kinds) -> (Vec<i64>, usize) {
    let blocks = pbf
        .pass("test", kinds, Vec::new, |block, out| {
            out.push(0);
            for el in block.elements() {
                let (k, id) = match el {
                    Element::Node(n) => (Kinds::NODES, n.id()),
                    Element::DenseNode(n) => (Kinds::NODES, n.id()),
                    Element::Way(w) => (Kinds::WAYS, w.id()),
                    Element::Relation(r) => (Kinds::RELATIONS, r.id()),
                };
                if k.meets(kinds) {
                    out.push(id);
                }
            }
            Ok(())
        })
        .unwrap();
    let mut ids: Vec<i64> = blocks.into_iter().flatten().collect();
    // A 0 for each block read.
    let n = ids.iter().filter(|&&id| id == 0).count();
    ids.retain(|&id| id != 0);
    ids.sort_unstable();
    (ids, n)
}

#[test]
fn sorted_index_matches_a_full_pass() {
    let dir = tempfile::tempdir().unwrap();
    let sorted = Pbf::open(&write(dir.path(), "s.pbf", &pbf(true, &blocks()))).unwrap();
    let unsorted = Pbf::open(&write(dir.path(), "u.pbf", &pbf(false, &blocks()))).unwrap();
    assert!(unsorted.index.get().is_none());
    // The first pass reads every block and indexes them.
    assert_eq!(ids(&unsorted, Kinds::WAYS), ((10..=15).collect(), 9));
    let (n, w, r) = (Kinds::NODES, Kinds::WAYS, Kinds::RELATIONS);
    let want = [n, n, n, n | w, w, w, w | r, r, r];
    for pbf in [&sorted, &unsorted] {
        let kinds: Vec<Kinds> = pbf.index.get().unwrap().iter().map(|e| e.1).collect();
        assert_eq!(kinds, want);
    }
    // The same blobs: the sorted file's header is longer by its sort feature.
    let relative = |pbf: &Pbf| -> Vec<u64> {
        let index = pbf.index.get().unwrap();
        index.iter().map(|e| e.0 - index[0].0).collect()
    };
    assert_eq!(relative(&sorted), relative(&unsorted));
}

#[test]
fn passes_read_only_the_blocks_of_their_kinds() {
    let dir = tempfile::tempdir().unwrap();
    let pbf = Pbf::open(&write(dir.path(), "s.pbf", &pbf(true, &blocks()))).unwrap();
    assert_eq!(ids(&pbf, Kinds::NODES), ((1..=9).collect(), 4));
    assert_eq!(ids(&pbf, Kinds::WAYS), ((10..=15).collect(), 4));
    assert_eq!(ids(&pbf, Kinds::RELATIONS), ((20..=23).collect(), 3));
    let all: Vec<i64> = (1..=15).chain(20..=23).collect();
    assert_eq!(
        ids(&pbf, Kinds::NODES | Kinds::WAYS | Kinds::RELATIONS),
        (all, 9)
    );
}

#[test]
fn files_with_only_nodes_or_no_relations_are_indexed() {
    use Group::*;
    let dir = tempfile::tempdir().unwrap();
    let nodes = [vec![Nodes(vec![1])], vec![Nodes(vec![2])]];
    let pbf = Pbf::open(&write(dir.path(), "n.pbf", &pbf(true, &nodes))).unwrap();
    assert_eq!(ids(&pbf, Kinds::NODES), (vec![1, 2], 2));
    assert_eq!(ids(&pbf, Kinds::WAYS), (vec![], 0));
    let ways = [
        vec![Nodes(vec![1])],
        vec![Ways(vec![2])],
        vec![Ways(vec![3])],
    ];
    let pbf2 = Pbf::open(&write(dir.path(), "w.pbf", &self::pbf(true, &ways))).unwrap();
    assert_eq!(ids(&pbf2, Kinds::WAYS), (vec![2, 3], 2));
    assert_eq!(ids(&pbf2, Kinds::RELATIONS), (vec![], 0));
}
