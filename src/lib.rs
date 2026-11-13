// SPDX-License-Identifier: GPL-3.0-or-later
//! Library of the cad2bem executable.
//!
//! - [`cad`]: native CAD inspection through the Gmsh C API in a private worker process.
//! - [`conversion`]: shared GUI/CLI conversion requests, jobs and validated HBG/GDF/STL output.
//! - [`engine`]: the conversion engine.
//! - [`view`]: display adapters for wgpu; they never change exported geometry.

pub mod cad;
pub mod conversion;
pub mod engine;
pub mod view;
