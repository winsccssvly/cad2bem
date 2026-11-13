// SPDX-License-Identifier: GPL-3.0-or-later
//! Gmsh access and native CAD inspection.
//!
//! [`api`] is the typed C API table (dynamic SDK or static link), [`session`] the safe session that every
//! Gmsh user goes through. Gmsh holds process-wide state, so sessions run only in private worker processes.
//! Scenes hold CAD preview triangles or an untrimmed surface mesh with CAD face ownership. They are
//! inspection data, not BEM geometry: conversion goes through [`crate::conversion`].

pub mod api;
pub mod gmsh;
pub mod scene;
pub mod session;
pub mod worker;
