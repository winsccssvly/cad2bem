// SPDX-License-Identifier: GPL-3.0-or-later
//! Display adapters: GPU meshes and camera, spline sampling, GUI conversion sessions and the geometry files the
//! viewer opens.
//!
//! Display data is derived from scenes and staged artifacts. Exports always use the staged files.

pub mod conversion;
pub mod fast;
pub mod mesh_file;
pub mod spline;
pub mod viewport;
