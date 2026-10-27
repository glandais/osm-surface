//! A drop-in replacement for the Valhalla / Stadia Maps `trace_attributes` endpoint, fed
//! by Shortbread vector tiles (OpenStreetMap data).

pub mod api;
pub mod config;
pub mod geo;
pub mod matcher;
pub mod mvt;
pub mod network;
pub mod polyline;
pub mod tiles;
pub mod trace;
pub mod tribly;
pub mod valhalla;
