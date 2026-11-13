// SPDX-License-Identifier: GPL-3.0-or-later
//! The conversion engine: CAD preparation, orientation, meshing, spline patches, checks and writers.
//!
//! - [`units`]: the length unit a CAD file states.
//! - [`prepare`]: import, cuts, overlaps, fragment and solid-based face sides.
//! - [`lofree`]: free low-order panels and their checks (lo-free).
//! - [`winding`]: face orientation by the generalized winding number where the solids do not decide.
//! - [`panels`]: panel geometry and the HBG/GDF/STL writers.
//! - [`worker`]: the private conversion worker process.
//! - [`fanout`]: spline patches made by several worker processes at once.
//! - [`primitives`]: bodies made from their dimensions alone (cylinder, moonpool, sphere, hemisphere, barge).
//! - [`primitives_ho`]: the same bodies as exact spline patches (Fast Mesh, high order).
//! - [`patches`]: four-sided B-spline patches of prepared CAD faces.
//! - [`ho`]: high-order spline patches (ho): the patches oriented, named, checked and reported.
//! - [`structured`]: structured low-order panels (lo-str) on the spline patch grids.
//! - [`bspline`]: clamped B-spline patches, polynomial or rational: evaluation, fitting, knots.
//! - [`exact`]: exact rational patches for the common analytic pieces.
//! - [`surfaces`]: the B-spline surfaces stored in IGES and STEP files.
//! - [`trim_repair`]: repair of folded quads on trimmed cylinders.
//! - [`checks`]: geometric checks of spline patches (Jacobian, folds, corners, gaps, volumes).
//! - [`kdtree`]: a static 3-D k-d tree over points.

pub mod bspline;
pub mod checks;
pub mod exact;
pub mod fanout;
pub mod ho;
pub mod kdtree;
pub mod lofree;
pub mod panels;
pub mod patches;
pub mod prepare;
pub mod primitives;
pub mod primitives_ho;
pub mod structured;
pub mod surfaces;
pub mod trim_repair;
pub mod units;
pub mod winding;
pub mod worker;
