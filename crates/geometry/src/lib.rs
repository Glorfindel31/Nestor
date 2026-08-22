//! Pure geometry math: Clipper2 wrapper, NFP-tracing primitives, DXF import.
//! Zero I/O, zero threading — see RUST-REWRITE-PLAN.md Phase 1.

pub mod circular_nfp;
pub mod clearance;
pub mod clipper;
pub mod dxf_export;
pub mod dxf_import;
pub mod hull_polygon;
pub mod inner_nfp;
pub mod nfp;
pub mod obstacle_nfp;
pub mod pdf_export;
pub mod point;
pub mod polygon;
pub mod remnant;
pub mod simplify;
pub mod simplify_polygon;
pub mod svg_export;
pub mod svg_import;

pub use point::Point;
