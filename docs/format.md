# Pack format

*Version 0.1 (draft), 2026-10-24. Written in phase 0 step 0.6 from the measurements in [phase0-plan.md](phase0-plan.md) and [size-table.md](size-table.md).*

A **pack** holds everything the app needs for one region: map tiles, addresses, and later routing, search and style data. It is built by `omp-builder` and read by `omp-app`, usually from a USB flash drive. This document is the contract between the two.

Status of each part:

| Part | Status |
|---|---|
| Container: header, section table, chunking, checksums | **Specified.** Reader and writer in `omp-core::pack`, with round-trip tests. |
| Metadata and attribution (`META`), schema (`SCHM`) | **Specified.** |
| Tiles (`TIDX`, `TDIC`, `TDAT`) | **Specified** (encoding V4 from phase 0). |
| Addresses (`ASTR`, `AIDX`, `ASUF`, `AUNI`, `ALSE`, `ARNG`) | **Specified** (encoding A3 from phase 0). |
| Routing (`RCH*`) | *Provisional*, finalized in Phase 3. |
| Search (`SFST`, `SPST`, `SENT`) | *Provisional*, finalized in Phase 2. |
| Style and labels (`STYL`) | *Provisional*, finalized in Phase 1. |

## Conventions

- All integers are **little-endian**. `u8`…`u64` and `i32` are fixed width.
- `varint` is unsigned LEB128 (7 bits per byte, low bits first). `zvarint` is a varint of the zigzag value (`(v << 1) ^ (v >> 63)`).
- `str` is a `varint` byte length followed by UTF-8 bytes, without a terminator.
- **World coordinates** are Web Mercator on a `u32` grid: x and y both run 0 to 2³² − 1 across the world, with y pointing south. A tile `(z, x, y)` covers 2^(32−z) world units on each side.
- **Degrees** in the header are `i32` in units of 10⁻⁷ degrees (about 1 cm).
- **Hashes** are XXH3-64 with seed 0.
- Sizes and offsets in this document are bytes unless stated otherwise.

## Files and chunks

A pack is a directory containing **chunk files** named `<name>.omp.000`, `<name>.omp.001`, …, three decimal digits, in order. `<name>` is free (e.g. `north-america-2026-10`). A pack that fits in one chunk still uses the `.000` suffix.

Chunks exist because FAT32, the default format of most USB drives, caps a file at 4 GiB − 1 byte. The North America pack measured in phase 0 is about 11.6 GiB, so it needs three chunks.

**Layout of each chunk file:**
- Bytes `0..4096`: the **chunk page**. In chunk 0 it holds the pack header; in every other chunk, a chunk header. Both are described below.
- Bytes `4096..`: the chunk's slice of the **virtual address space**.

**Virtual address space.** The reader presents the chunks as one address space of `chunk_count × chunk_capacity` bytes. Virtual offset `v` lives in chunk `v / chunk_capacity`, at file offset `4096 + v % chunk_capacity`. Every chunk except the last is exactly `4096 + chunk_capacity` bytes long. The default `chunk_capacity` is 2³² − 2²⁰ (4 GiB − 1 MiB), which keeps each file under the FAT32 limit; any multiple of 4096 that does that is valid, and tests use small values.

**Sections and chunk boundaries.** Each section has a flag that says which rule it follows:
- **Contiguous** (flag bit 0 clear): the section lies inside one chunk, so the reader can hand out one `&[u8]` (one `mmap`) for it. If it would cross a boundary, the writer starts it at the next chunk instead. A contiguous section can be at most `chunk_capacity` long.
- **Spanning** (flag bit 0 set): the section may cross chunk boundaries, but none of its **records** does. The section defines what a record is (one tile for `TDAT`, one zstd block for `ASTR`). The writer pads with zero bytes before a record that would cross a boundary, and the section's own index points past the padding. The reader reads one record at a time and never sees a boundary.

The tile data of a continent is several GiB, so it must be spanning. Every other section specified so far is under 1 GiB for North America and is contiguous.

**Alignment.** Every section starts at a virtual offset that is a multiple of 4096, so it can be memory-mapped. The gap before it is zero-filled.

## Pack header (chunk 0, bytes 0..4096)

| Offset | Type | Field |
|---:|---|---|
| 0 | `[u8; 8]` | Magic `89 4F 4D 50 0D 0A 1A 0A` (`\x89OMP\r\n\x1a\n`). Like PNG's, it catches text-mode and 7-bit transfers. |
| 8 | `u16` | Major version (currently 0) |
| 10 | `u16` | Minor version (currently 1) |
| 12 | `u32` | Header length used, in bytes (fields plus strings; the rest of the page is zero) |
| 16 | `[u8; 16]` | Pack ID: random, chosen at build time. Each chunk header repeats it. |
| 32 | `u64` | Chunk capacity |
| 40 | `u32` | Chunk count |
| 44 | `u32` | Section count |
| 48 | `u64` | Section table: virtual offset |
| 56 | `u64` | Section table: length (`32 × section count`) |
| 64 | `u64` | Section table: XXH3-64 |
| 72 | `i32 × 4` | Region bounding box: west, south, east, north, in 10⁻⁷ degrees |
| 88 | `i64` | Build time, Unix seconds (UTC) |
| 96 | `str` | Region name for display (e.g. `North America`), at most 255 bytes |
| … | `str` | Input snapshot, at most 255 bytes (e.g. `osm 2026-10-01; openaddresses 2026-10-23`); the full manifest is in `META` |
| last 8 | `u64` | XXH3-64 of bytes `0..header_length − 8` |

The reader checks the magic, the version, and the header hash on every open. These checks cost nothing because the page is read anyway.

## Chunk header (chunks 1…n, bytes 0..4096)

| Offset | Type | Field |
|---:|---|---|
| 0 | `[u8; 8]` | Magic, the same as above |
| 8 | `u16`, `u16` | Major and minor version |
| 12 | `u32` | Chunk index (1, 2, …) |
| 16 | `[u8; 16]` | Pack ID |
| 32 | `u64` | XXH3-64 of bytes `0..32` |

On open, the reader checks each chunk's index and pack ID, so chunks from two builds cannot be mixed by accident. It also checks the length of every chunk.

## Section table

`section_count` entries of 32 bytes each, sorted by offset:

| Offset | Type | Field |
|---:|---|---|
| 0 | `[u8; 4]` | Section ID: four ASCII characters, e.g. `TDAT` |
| 4 | `u32` | Flags. Bit 0: spanning (see above). Bits 1–31: reserved, written as 0. |
| 8 | `u64` | Virtual offset (a multiple of 4096) |
| 16 | `u64` | Length |
| 24 | `u64` | XXH3-64 of the section's bytes, including the padding inside a spanning section |

Each section ID appears at most once. Sections do not overlap. The table itself is placed like a contiguous section (page-aligned, inside one chunk, usually last), but it has no entry of its own: the header points to it.

## Versioning

- **Major** version: the reader opens a pack only if the major version equals its own. Any change that an old reader would misread bumps it: a changed field, encoding or meaning.
- **Minor** version: additions only. New sections, and new fields in space that older versions wrote as zero (the header's tail, reserved flag bits). A reader ignores section IDs it does not know, and accepts any minor version. It may show "built by a newer builder".
- Version 0.x is the development series. Until 1.0, the app and builder are released together and the major version stays 0. Breaking changes bump the minor version, and the reader requires an exact minor match.

## Checksums

Every section has an XXH3-64 checksum in the section table. The app does **not** check them on load: hashing 11 GiB from a USB 2 drive would take minutes. Instead:
- The header, the chunk headers and the section table are checked on every open. They are small.
- A **"Verify pack"** command hashes every section and reports any that fail. The app suggests running it after copying a pack, and when a decode fails.
- Each zstd frame inside a section also carries zstd's own content checksum (enabled at write time), so a corrupt tile fails to decode instead of drawing garbage.

## `META`: metadata and attribution (contiguous)

A UTF-8 JSON object, zstd-compressed (one frame). Fields:
- `region`, `bbox`, `build_time`, `builder_version`, `format_version`.
- `sources`: the `fetch` manifest. For each input: name, URL, date, SHA-256, license, attribution text. Every OpenAddresses source is listed, including those whose license is `unspecified` (decision 13).
- `attribution`: the text the app shows on its "About this map" screen, built from `sources` (e.g. "© OpenStreetMap contributors, ODbL", the Statistics Canada NAR licence, each OpenAddresses source's attribution).
- `settings`: the build options that shaped the data (whitelist version, simplification tolerances, address dedup and attach distances).

JSON keeps this section easy to extend. It is a few hundred KiB at most and is read once.

## `SCHM`: schema (contiguous)

The ID tables that the tile and address sections refer to, so the app does not depend on a compiled-in whitelist. A zstd-compressed JSON object:
- `whitelist_version`.
- `layers`: in layer-ID order. For each layer: `name`, `geometry` (`point`, `line` or `area`), `props` (in prop-key order) and `classes` (in class-ID order; each has `name` and `minzoom`).

The style (Phase 1) refers to layers and classes by name. The app resolves names to IDs through this section when it opens the pack.

## Tiles

Tiles cover zooms 0–14. The app overzooms z14 tiles for higher zooms. Each tile is cut with a buffer of 64 grid units on every side, so lines and polygon edges continue a little past the tile's edge and strokes join cleanly.

### Layer payload (encoding V2)

A tile holds one **layer payload** per non-empty layer. A payload is the bytes below, compressed as described under `TDAT`.

Coordinates are **tile-local**: a 4096 × 4096 grid per tile (12 bits), origin at the top-left corner, y down. Because of the buffer, values run from −64 to 4159.

```
payload    = nfeatures:varint  nstrings:varint  string*  feature*
string     = str
feature    = (class << 2 | geomtype):varint  nprops:varint  prop*  geometry
prop       = key:varint  value:varint     ; value = string_index << 1  or  integer << 1 | 1
geometry   = point | lines | polygon       ; geomtype 0, 1, 2
point      = dx:zvarint dy:zvarint
lines      = nparts:varint  (npoints:varint (dx:zvarint dy:zvarint)*)*
polygon    = nrings:varint  (npoints:varint (dx:zvarint dy:zvarint)*)*
```

- `class` indexes the layer's classes in `SCHM`. `key` indexes the layer's `props`.
- A prop value that is a small non-negative decimal integer without leading zeros (e.g. a `ref` of `401`) is stored as an integer. Every other value is an index into the payload's string table.
- All deltas are from one **cursor** that starts at `(0, 0)` and carries across points, parts, rings and features. Each decoded point moves the cursor.
- **Polygons:** rings are implicitly closed (the last point is not repeated). Outer rings are clockwise on screen (positive shoelace area with y down) and holes counter-clockwise, as in MVT. A ring belongs to the nearest preceding outer ring. A polygon has at least one outer ring.
- Features are already simplified for their zoom (Douglas–Peucker, with tolerances from the whitelist, see [phase0-plan.md](phase0-plan.md#water-simplification-follow-up-2026-10-24)) and filtered by `minzoom`. The app draws what it gets.

### `TDIC`: dictionaries (contiguous)

zstd dictionaries, one per **(layer, zoom band)** that had enough data to train on. Zoom bands are z0–7, z8–11, z12–13 and z14.

```
TDIC  = ndicts:u32  entry*  dict_bytes*
entry = layer:u8  band:u8  reserved:u16  offset:u32  length:u32    ; offset from the start of TDIC
```

A (layer, band) without an entry has no dictionary, and its payloads are compressed without one. North America's dictionaries are 64 KiB each and about 2 MiB in all. The app loads them all at start-up.

### `TDAT`: tile data (spanning)

The compressed layer payloads, one **record per tile**, with the tile's layers back to back in layer-ID order. Each layer payload is one zstd frame (level 19, content checksum on, no dictionary ID in the frame), compressed with its (layer, band) dictionary. Tiles are stored by zoom, then in Hilbert order within the zoom, the same order as the index. Spatially close tiles are therefore close on disk, so one viewport is usually one or two reads.

### `TIDX`: tile index (contiguous)

Sparse and Hilbert-ordered (decision 7). For North America it is about 110 MiB over all zooms (1.2% of tile bytes); dense offset arrays over each zoom's bounding box would be 3.2 GiB, because most cells are empty (95% of Quebec's z14 bounding box).

```
TIDX       = zoom_dir[15]  block*
zoom_dir   = block_count:u32  dir_offset:u32       ; offsets from the start of TIDX
dir_entry  = first_key:u64  data_offset:u64  block_offset:u32  tiles:u32   ; per block, at dir_offset
block      = (key_delta:varint  layer_mask:u16  layer_len:varint*)*        ; `tiles` tiles
```

- `key` is the tile's Hilbert index `d(z, x, y)` on the 2^z × 2^z grid, using the curve in `omp-builder`'s `compress::hilbert`. `key_delta` is the difference from the previous tile in the block. The first tile's delta is from `first_key`, so it is 0.
- `layer_mask` has bit *i* set when layer *i* is present. One `layer_len` follows per set bit, in layer order: the compressed length of that layer's frame.
- Blocks hold 256 tiles. The directory gives each block's first key, the virtual offset in `TDAT` of its first tile, and where the block starts in `TIDX`.
- **Lookup of `(z, x, y)`:** binary-search zoom `z`'s directory by `first_key` and decode one block (at most 256 entries). Sum the frame lengths of the tiles before the target to get its offset from `data_offset`. Padding at a chunk boundary is never inside a block: the writer starts a new block at the first tile after padding.

## Addresses

Addresses are stored per **street**, as in roadmap §4 and decision 9 (A3). A street is a set of connected OSM ways that share a normalized name. Its **offset** coordinate runs along the street in metres, from a fixed starting end. Street names and geometry belong to the search section, which is shared. Addresses refer to streets by **street ID**, a dense index into the search section's street table.

The phase 0 sizes for North America: 151.5M addresses, 315 MiB (2.18 bytes each), plus 50 MiB of US TIGER ranges.

### `ASTR`: street runs (spanning)

Records are **zstd blocks** (level 19, no dictionary). Each block holds the runs of consecutive streets, about 32 KiB uncompressed (the block size barely affects size: 4 KiB blocks are 3.6% larger, so Phase 2 may lower it). A street never spans two blocks. Within a block:

```
run   = street_delta:varint  count:varint  entry*        ; street_delta from the previous run's street ID
entry = (number_delta << 3 | left << 2 | osm << 1 | approx):varint  offset_delta:zvarint
```

- Entries are sorted by house number, then suffix, then offset. `number_delta` and `offset_delta` start from 0 for each street.
- Offsets are in units of **5 m** along the street.
- `left`: the address is on the left side when walking in the offset direction. `osm`: it comes from OSM, which wins in the dedup and affects the attribution shown. `approx`: its position is approximate (an NAR blockface-only point).
- Each street's first `street_delta` in a block is from the last street of the previous block, or from 0 in the first block.

### `AIDX`: block index (contiguous)

```
AIDX  = nblocks:u32  (first_street:u32  offset:u64  length:u32)*
```

The virtual offset (in `ASTR`) and compressed length of each block, and the first street ID in it. To look up a street, binary-search `first_street` and decompress that one block.

### `ASUF` and `AUNI`: suffixes and units (contiguous)

Each is zstd-compressed (one frame) and indexed by **entry ordinal**, the position of an entry in the street runs counted across all of `ASTR`:

```
ASUF  = (ordinal_delta:varint  suffix:str)*                     ; "A", "1/2", "-125", …
AUNI  = (ordinal_delta:varint  nunits:varint  unit:str*)*       ; "Apt 2", "Unit 101", …
```

Phase 2 may split these into blocks aligned with `ASTR` blocks if lookups need it. At 0.4's sizes (units 0.4–1.1 MiB per region) one frame per pack is workable, but a continent's 32M units are not.

### `ALSE`: unattached addresses (contiguous)

Addresses that matched no OSM street within 200 m (11% in North America). Stored with their own position and street name, sorted along a Morton curve, zstd-compressed:

```
ALSE  = nnames:varint  name:str*  point*
point = dx:zvarint  dy:zvarint  name:varint  (number << 2 | osm << 1 | approx):varint  suffix:str  nunits:varint  unit:str*
```

`dx`, `dy` are deltas in units of 128 world units (about 1 m at the equator and 0.7 m at 45°), from a cursor that starts at (0, 0). `name` indexes the name table.

### `ARNG`: interpolation ranges (contiguous)

TIGER ranges with no point address nearby, and OSM `addr:interpolation` ranges (decision 11). zstd-compressed:

```
range = street_delta:varint  start_delta:zvarint  length:zvarint  from:varint  to_minus_from:zvarint  (parity << 1 | left):varint
```

Sorted by street, then start. `start` and `length` are offsets in 5 m units along the street. `start_delta` resets to 0 at each new street. `parity` is 0 for all numbers, 1 for odd and 2 for even.

## Routing — *provisional*

Phase 3 builds its own contraction hierarchy. The size table reserves about 3.2 GiB for North America at 12 bytes per node and per edge in a compact layout (62.5M nodes, 150M edges, about 1.5 CH edges per edge). The expected sections:
- `RCHN`: nodes in a cache-friendly order, with coordinates.
- `RCHE`: up and down edges in CSR form, with weights and a shortcut flag.
- `RCHU`: shortcut unpacking (the middle node of each shortcut).
- `RGEO`: road geometry for drawing the route and turn-by-turn, possibly shared with the z14 roads tiles.
- Turn restrictions, and the mapping from tiles to the graph for snapping.

## Search — *provisional*

Phase 2. The phase 0 spike measured an `fst` of normalized tokens, postings lists, and an entries table (at most 253 MiB for North America):
- `SFST`: an `fst` map from token to postings offset.
- `SPST`: postings lists (delta-coded entry IDs).
- `SENT`: entries: places, streets (the street table that addresses refer to), POIs, each with a name, kind, position and admin context.

The admin hierarchy, ranking data and the street table's exact form are open.

## Style and labels — *provisional*

Phase 1. A `STYL` section holds the style the pack was built for (colors, widths and label rules per layer, class and zoom), so a pack and the app's renderer agree. The app may also ship a built-in style and ignore it. Label placement data (e.g. precomputed label points for large polygons) is open.
