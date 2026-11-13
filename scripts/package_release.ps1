# SPDX-License-Identifier: GPL-3.0-or-later
<#
.SYNOPSIS
Assemble a Windows release of cad2bem: the executable with every licence it needs, and its corresponding source.

.DESCRIPTION
The static executable contains cad2bem, Gmsh, OpenCASCADE and the Rust crates of its dependency graph. A
distribution of it follows the GPL-3.0, so this script writes two folders, each also as a zip, and the
executable on its own (cad2bem-VERSION-windows-x64.exe; it carries its licence files, `cad2bem licenses`):

  cad2bem-VERSION-windows-x64    cad2bem.exe, README, CHANGELOG, LICENSE, NOTICE, THIRD_PARTY_NOTICES,
                                 SOURCE.txt and licenses/ (Gmsh with the notices of its bundled code,
                                 LEMON, OpenCASCADE, each Rust crate: written by the executable)
  cad2bem-VERSION-source         the cad2bem source, the pinned Gmsh and OpenCASCADE archives (checked against
                                 scripts/native-sources.json; the build script applies its changes to Gmsh),
                                 the vendored crate sources, and SOURCE.txt

-SourceTree is the published source (default: this repository). In a git repository its committed files are
archived and uncommitted changes to them are refused, so the source matches the executable built from it;
any other folder is taken whole (-Commit then names the commit it was exported from). -Exe defaults to the
static build of that tree.

.EXAMPLE
. scripts/static_env.ps1
cargo build @StaticCargo --bin cad2bem
scripts/package_release.ps1
#>
[CmdletBinding()]
param(
    [string]$SourceTree,
    [string]$Exe,
    [string]$NativeSources,
    [string]$Out,
    [string]$Commit,
    [switch]$NoVendor
)
$ErrorActionPreference = 'Stop'
$repo = Split-Path -Parent $PSScriptRoot
if (!$SourceTree) { $SourceTree = $repo }
$SourceTree = [IO.Path]::GetFullPath($SourceTree)
if (!$Exe) { $Exe = Join-Path $SourceTree 'target/static/x86_64-pc-windows-msvc/release/cad2bem.exe' }
if (!$NativeSources) { $NativeSources = Join-Path $repo 'out/native-sources' }
if (!$Out) { $Out = Join-Path $repo 'dist' }
$Exe = [IO.Path]::GetFullPath($Exe)
$NativeSources = [IO.Path]::GetFullPath($NativeSources)
$Out = [IO.Path]::GetFullPath($Out)
Add-Type -AssemblyName System.IO.Compression
Add-Type -AssemblyName System.IO.Compression.FileSystem

# Native programs write progress to standard error; only their exit code counts.
function Native([string]$program, [string[]]$arguments, [string]$cwd) {
    Push-Location -LiteralPath $cwd
    try {
        $ErrorActionPreference = 'Continue'
        $output = & $program @arguments 2>$null
        if ($LASTEXITCODE -ne 0) { throw "$program $($arguments -join ' ') failed ($LASTEXITCODE)" }
        $output
    } finally { Pop-Location }
}
function Fresh([string]$path) {
    if (Test-Path -LiteralPath $path) { throw "$path exists: remove it or choose another -Out" }
    New-Item -ItemType Directory -Path $path | Out-Null
}
function Copy-Into([string]$file, [string]$folder, [string]$name) {
    if (!(Test-Path -LiteralPath $folder)) { New-Item -ItemType Directory -Path $folder | Out-Null }
    if (!$name) { $name = Split-Path -Leaf $file }
    Copy-Item -LiteralPath $file -Destination (Join-Path $folder $name)
}
function Write-Text([string]$path, [string[]]$lines) {
    [IO.File]::WriteAllText($path, (($lines -join "`r`n") + "`r`n"), (New-Object Text.UTF8Encoding $false))
}
# A zip of the folder, the folder's name the top entry. Entry names use '/', as the zip format requires: the
# CreateFromDirectory of Windows PowerShell's .NET writes '\', which unzip on other systems keeps in file names.
function Zip([string]$folder, [string]$archive) {
    if (!$archive) { $archive = "$folder.zip" }
    $parent = Split-Path -Parent ([IO.Path]::GetFullPath($folder))
    $stream = [IO.File]::Open($archive, [IO.FileMode]::CreateNew)
    $zip = New-Object IO.Compression.ZipArchive($stream, [IO.Compression.ZipArchiveMode]::Create)
    try {
        foreach ($file in (Get-ChildItem -LiteralPath $folder -Recurse -File -Force | Sort-Object FullName)) {
            $entry = $file.FullName.Substring($parent.Length + 1).Replace('\', '/')
            [IO.Compression.ZipFileExtensions]::CreateEntryFromFile($zip, $file.FullName, $entry,
                [IO.Compression.CompressionLevel]::Optimal) | Out-Null
        }
    } finally {
        $zip.Dispose()
        $stream.Dispose()
    }
}

# The version, the source and the executable
$manifest = Get-Content -Raw -LiteralPath (Join-Path $SourceTree 'Cargo.toml')
if ($manifest -notmatch '(?m)^version = "([^"]+)"') { throw 'No package version in Cargo.toml' }
$version = $Matches[1]
if (!(Test-Path -LiteralPath $Exe)) { throw "No executable at $($Exe): build it first (docs/development/static-cad.md)" }
$reported = (Native $Exe @('version') $SourceTree | Select-Object -First 1)
if ($reported -notmatch "^cad2bem $([regex]::Escape($version)) ") { throw "The executable reports '$reported', the source is $version" }
# No path of the build machine in the executable (scripts/static_env.ps1 and the static build trim them)
$bytes = [IO.File]::ReadAllBytes($Exe)
$texts = @([Text.Encoding]::GetEncoding(28591).GetString($bytes), [Text.Encoding]::Unicode.GetString($bytes))
foreach ($path in @($env:USERPROFILE, $repo, $SourceTree) | Where-Object { $_ }) {
    foreach ($text in $texts) {
        if ($text.IndexOf($path, [StringComparison]::OrdinalIgnoreCase) -ge 0) {
            throw "The executable contains the path $($path): build it after . scripts/static_env.ps1 on an SDK from the current scripts/build_static_cad.ps1"
        }
    }
}
$git = Test-Path -LiteralPath (Join-Path $SourceTree '.git')
$commit = if ($Commit) { $Commit } else { 'none (not a git repository)' }
if ($git) {
    $dirty = Native 'git' @('status', '--porcelain', '--untracked-files=no') $SourceTree
    if ($dirty) { throw "Uncommitted changes in $($SourceTree): commit them, rebuild, then package" }
    $commit = (Native 'git' @('rev-parse', 'HEAD') $SourceTree | Select-Object -First 1)
}
$pins = Get-Content -Raw -LiteralPath (Join-Path $SourceTree 'scripts/native-sources.json') | ConvertFrom-Json
foreach ($pin in @($pins.gmsh, $pins.occt, $pins.lemon)) {
    $archive = Join-Path $NativeSources $pin.filename
    if (!(Test-Path -LiteralPath $archive)) { throw "No $($pin.filename) in $($NativeSources): run scripts/build_static_cad.ps1" }
    $hash = (Get-FileHash -Algorithm SHA256 -LiteralPath $archive).Hash.ToLowerInvariant()
    if ($hash -ne $pin.sha256) { throw "$($pin.filename) does not match its pinned SHA-256" }
}
$gmshSource = Join-Path $NativeSources $pins.gmsh.directory
$occtSource = Join-Path $NativeSources $pins.occt.directory
$lemonSource = Join-Path $NativeSources $pins.lemon.directory
foreach ($folder in @($gmshSource, $occtSource, $lemonSource)) {
    if (!(Test-Path -LiteralPath $folder)) { throw "No unpacked $($folder): run scripts/build_static_cad.ps1" }
}

$binary = Join-Path $Out "cad2bem-$version-windows-x64"
$source = Join-Path $Out "cad2bem-$version-source"
foreach ($path in @($binary, $source, "$binary.zip", "$source.zip", "$binary.exe")) {
    if (Test-Path -LiteralPath $path) { throw "$path exists: remove it or choose another -Out" }
}
Fresh $binary
Fresh $source

# The executable and its documents
Copy-Item -LiteralPath $Exe -Destination (Join-Path $binary 'cad2bem.exe')
foreach ($name in @('README.md', 'CHANGELOG.md', 'CONTRIBUTING.md', 'LICENSE', 'NOTICE', 'THIRD_PARTY_NOTICES.md')) {
    Copy-Into (Join-Path $SourceTree $name) $binary
}
# The documentation the README links to, and the example models to try
foreach ($folder in @('docs', 'examples')) {
    Copy-Item -LiteralPath (Join-Path $SourceTree $folder) -Destination (Join-Path $binary $folder) -Recurse
}
$licenses = Join-Path $binary 'licenses'

# The licence files: the executable writes those it carries (build/licenses.rs collects them when it is built:
# cad2bem's notices, Gmsh with the licence files of the bundled code it compiles, LEMON, OpenCASCADE and every
# linked Rust crate), so the folder and `cad2bem licenses` are the same files.
Native $Exe @('licenses', '--write', $licenses) $SourceTree | Out-Null
foreach ($part in @('LICENSE', 'gmsh/LICENSE.txt', 'gmsh/contrib/gmm/LGPL-3.0.txt', 'lemon/LICENSE',
        'opencascade/LICENSE_LGPL_21.txt', 'opencascade/OCCT_LGPL_EXCEPTION.txt', 'crates/README.md')) {
    if (!(Test-Path -LiteralPath (Join-Path $licenses $part))) { throw "The executable carries no $($part): build it with the static-gmsh feature" }
}
$rows = @(Get-ChildItem -LiteralPath (Join-Path $licenses 'crates') -Directory)

# The corresponding source
$name = "cad2bem-$version"
$archive = Join-Path $source "$name-src.zip"
if ($git) {
    Native 'git' @('archive', '--format=zip', "--prefix=$name/", '-o', $archive, 'HEAD') $SourceTree | Out-Null
} else {
    $staging = Join-Path $source $name
    Copy-Item -LiteralPath $SourceTree -Destination $staging -Recurse
    Zip $staging $archive
    Remove-Item -LiteralPath $staging -Recurse -Force
}
foreach ($pin in @($pins.gmsh, $pins.occt, $pins.lemon)) { Copy-Into (Join-Path $NativeSources $pin.filename) $source }
if (!$NoVendor) {
    $vendor = Join-Path $source 'vendor'
    Native 'cargo' @('vendor', '--locked', '--versioned-dirs', $vendor) $SourceTree | Out-Null
    Zip $vendor (Join-Path $source "$name-vendor.zip")
    Remove-Item -LiteralPath $vendor -Recurse -Force
}

# Where the source is, in both folders
$sums = foreach ($file in (Get-ChildItem -LiteralPath $source -File | Sort-Object Name)) {
    "    $((Get-FileHash -Algorithm SHA256 -LiteralPath $file.FullName).Hash.ToLowerInvariant())  $($file.Name)"
}
$vendored = if ($NoVendor) { 'The crates come from crates.io as pinned in Cargo.lock.' } else {
    "$name-vendor.zip holds the sources of the Rust crates pinned in Cargo.lock (cargo vendor)." }
$text = @(
    "cad2bem $version for Windows x64",
    '',
    'cad2bem.exe links cad2bem, Gmsh, OpenCASCADE, LEMON and Rust crates statically. It is free software under the GNU',
    'General Public License version 3 or later (LICENSE); the licences of the components are in licenses/ and',
    'THIRD_PARTY_NOTICES.md, and cad2bem.exe carries the same files: cad2bem licenses prints them, About in the',
    'GUI shows them.',
    '',
    "Corresponding source: the folder (or zip) cad2bem-$version-source published with this release.",
    "  $name-src.zip              cad2bem at commit $commit",
    "  $($pins.gmsh.filename)  Gmsh $($pins.gmsh.version) ($($pins.gmsh.url))",
    "  $($pins.occt.filename)      OpenCASCADE $($pins.occt.version) ($($pins.occt.url))",
    "  $($pins.lemon.filename)  LEMON $($pins.lemon.version) ($($pins.lemon.url))",
    "  $vendored",
    '',
    'scripts/build_static_cad.ps1 in the cad2bem source builds Gmsh and OpenCASCADE from these archives and applies',
    'the changes to Gmsh (each marked cad2bem: in the patched file; Gmsh is built without contrib/blossom, its',
    'recombination using LEMON''s matching); docs/development/static-cad.md describes the',
    'build of cad2bem.exe from them.',
    '',
    'SHA-256 of the source files:'
) + $sums
Write-Text (Join-Path $binary 'SOURCE.txt') $text
Write-Text (Join-Path $source 'SOURCE.txt') $text

Zip $binary
Zip $source
# The executable on its own, for those who want only it: it carries its licence files.
$single = "$binary.exe"
Copy-Item -LiteralPath $Exe -Destination $single
Write-Host "Release $version ($($rows.Count) crates): $binary(.zip), $source(.zip), $single"
