# SPDX-License-Identifier: GPL-3.0-or-later
<#
.SYNOPSIS
Set up this PowerShell session for cargo against the static Gmsh/OCCT build, the Gmsh the executable ships with.

.DESCRIPTION
Dot-source it: it sets CAD2BEM_STATIC_CAD_PREFIX (default out/static-cad/install), CARGO_TARGET_DIR=target/static
and the Rust flags (static CRT, source paths remapped), removes CAD2BEM_GMSH_LIBRARY, and defines $StaticCargo with the
cargo flags of that build. The conversion tests use it: the static Gmsh carries the corrections of
scripts/build_static_cad.ps1, which a separately installed Gmsh SDK does not.

.EXAMPLE
. scripts/static_env.ps1
cargo clippy @StaticCargo --all-targets -- -D warnings
cargo test @StaticCargo -- --include-ignored
#>
# No $ErrorActionPreference here: dot-sourced, it would stay in the session and turn cargo's progress on
# standard error into errors.
$repo = Split-Path -Parent $PSScriptRoot
$prefix = if ($env:CAD2BEM_STATIC_CAD_PREFIX) { $env:CAD2BEM_STATIC_CAD_PREFIX } else { Join-Path $repo 'out/static-cad/install' }
$prefix = [IO.Path]::GetFullPath($prefix)
if (!(Test-Path -LiteralPath (Join-Path $prefix 'static-link.txt'))) {
    throw "No static CAD SDK at $($prefix): build it with scripts/build_static_cad.ps1 (docs/development/static-cad.md)"
}
$env:CAD2BEM_STATIC_CAD_PREFIX = $prefix
$env:CARGO_TARGET_DIR = Join-Path $repo 'target/static'
$cargoHome = if ($env:CARGO_HOME) { $env:CARGO_HOME } else { Join-Path $HOME '.cargo' }
# Static CRT; panic and debug paths relative to the crate sources, so the executable carries no path of the
# build machine. The encoded form keeps paths with spaces whole.
$env:CARGO_ENCODED_RUSTFLAGS = @('-C', 'target-feature=+crt-static',
    "--remap-path-prefix=$([IO.Path]::GetFullPath($cargoHome))=/cargo", "--remap-path-prefix=$repo=/cad2bem") -join [char]0x1f
Remove-Item Env:RUSTFLAGS -ErrorAction SilentlyContinue
Remove-Item Env:CAD2BEM_GMSH_LIBRARY -ErrorAction SilentlyContinue
$StaticCargo = @('--locked', '--release', '--target', 'x86_64-pc-windows-msvc', '--features', 'static-gmsh')
Write-Host "Static CAD build: $prefix (cargo ... @StaticCargo)"
