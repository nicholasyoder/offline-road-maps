//! Addresses: OSM address points and interpolation ways ([`osm`]), OpenAddresses and StatCan
//! (NAR, ODA) points ([`sources`]) and TIGER ranges ([`tiger`]), merged per state and
//! province onto the OSM streets ([`merge`], whose stages are in [`pipeline`]) and stored in
//! the pack's address sections ([`store`]), with the postcodes they give ([`postcodes`]).

mod attach;
pub mod merge;
pub mod osm;
mod pipeline;
pub mod postcodes;
mod sources;
pub mod store;
pub mod streets;
pub mod tiger;

pub use pipeline::{Input, Raw, SourceOpts, Src};
