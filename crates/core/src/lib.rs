//! Shared pack format types, reader/writer, tag whitelist and text normalization, and the
//! search, address, street, admin and grid readers, and the car routing profile and reader.
//!
//! The pack format is specified in `docs/format.md`.

pub mod addr;
pub mod admin;
mod blocked;
pub mod find;
pub mod fuzzy;
pub mod geo;
pub mod grid;
pub mod here;
pub mod meta;
pub mod normalize;
pub mod pack;
pub mod profile;
pub mod query;
pub mod route;
pub mod routes;
pub mod schema;
pub mod search;
pub mod street;
pub mod tile;
pub mod tiles;
pub mod varint;
pub mod whitelist;
mod zframe;

#[cfg(test)]
mod search_tests;
