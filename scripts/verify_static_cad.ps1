# SPDX-License-Identifier: GPL-3.0-or-later
<# Compile and prove the static CAD backend. This does not certify a Rust-only BEM pipeline. #>
[CmdletBinding()]
param(
    [Parameter(Mandatory=$true)][string]$VisualStudioPath,
    [string]$Prefix,
    [string]$Out
)
$ErrorActionPreference = 'Stop'
$repo = Split-Path -Parent $PSScriptRoot
if (!$Prefix) { $Prefix = Join-Path $repo 'out/static-cad/install' }
if (!$Out) { $Out = Join-Path $repo 'out/static-cad-proof' }
$Prefix = [IO.Path]::GetFullPath($Prefix)
$Out = [IO.Path]::GetFullPath($Out)
if (!$Prefix.StartsWith(([IO.Path]::GetFullPath($repo).TrimEnd([IO.Path]::DirectorySeparatorChar) + '\'),[StringComparison]::OrdinalIgnoreCase)) {
    throw 'This proof may hide only a generated SDK inside the repository workspace'
}
if (Test-Path -LiteralPath $Out) { throw 'Choose a fresh proof output directory' }
if (!(Test-Path -LiteralPath (Join-Path $Prefix 'static-link.txt'))) { throw 'Build the static CAD SDK first' }
New-Item -ItemType Directory -Path $Out | Out-Null
& (Join-Path $VisualStudioPath 'Common7/Tools/Launch-VsDevShell.ps1') -Arch amd64 -HostArch amd64 -SkipAutomaticLocation -NoLogo | Out-Null
$env:CAD2BEM_STATIC_CAD_PREFIX = $Prefix
$env:CARGO_TARGET_DIR = Join-Path $repo 'target/static'
$cargoHome = if ($env:CARGO_HOME) { $env:CARGO_HOME } else { Join-Path $HOME '.cargo' }
# Static CRT; panic and debug paths relative to the crate sources, so the executable carries no path of the
# build machine. The encoded form keeps paths with spaces whole.
$env:CARGO_ENCODED_RUSTFLAGS = @('-C', 'target-feature=+crt-static',
    "--remap-path-prefix=$([IO.Path]::GetFullPath($cargoHome))=/cargo", "--remap-path-prefix=$repo=/cad2bem") -join [char]0x1f
Remove-Item Env:RUSTFLAGS -ErrorAction SilentlyContinue
$cargo = (Get-Command cargo).Source
$dumpbin = (Get-Command dumpbin).Source
function Checked([string]$program,[string[]]$arguments,[string]$name,[string]$cwd) {
    $quoted = @($arguments | ForEach-Object { '"' + $_.Replace('"','\"') + '"' })
    $process = Start-Process -FilePath $program -ArgumentList $quoted -WorkingDirectory $cwd -WindowStyle Hidden -PassThru `
        -RedirectStandardOutput (Join-Path $Out "$name.stdout.log") -RedirectStandardError (Join-Path $Out "$name.stderr.log")
    $null = $process.Handle
    if (!$process.WaitForExit(180000)) { Stop-Process -Id $process.Id -Force; throw "$name timed out" }
    if ($process.ExitCode -ne 0) {
        Get-Content -LiteralPath (Join-Path $Out "$name.stderr.log") -Tail 24
        throw "$name failed ($($process.ExitCode))"
    }
}
Checked $cargo @('build','--locked','--release','--target','x86_64-pc-windows-msvc','-p','cad2bem','--features','static-gmsh','--bin','cad2bem','--example','static_cad_probe') 'rust-build' $repo
$release = Join-Path $env:CARGO_TARGET_DIR 'x86_64-pc-windows-msvc/release'
$binaries = @((Join-Path $release 'cad2bem.exe'),(Join-Path $release 'examples/static_cad_probe.exe'))
$allowed = @('kernel32','ntdll','user32','gdi32','advapi32','shell32','ole32','oleaut32','comdlg32','comctl32',
    'msvcrt','ws2_32','wsock32','winmm','version','psapi','shlwapi','bcrypt','bcryptprimitives','combase','imm32',
    'dwmapi','uxtheme','opengl32','glu32','windowscodecs','dbghelp','setupapi','propsys','d3d12','dxgi',
    'd3dcompiler_47','powrprof','crypt32','secur32','rpcrt4','shcore','dwrite')
$imports = @{}
foreach ($binary in $binaries) {
    $text = (& $dumpbin /DEPENDENTS $binary | Out-String)
    if ($LASTEXITCODE -ne 0) { throw 'Cannot inspect PE imports' }
    $dlls = @([regex]::Matches($text,'(?im)^\s+([\w.-]+\.dll)\s*$') | ForEach-Object { $_.Groups[1].Value })
    if (!$dlls.Count) { throw 'No DLL imports found: audit did not run' }
    foreach ($dll in $dlls) {
        $name = [IO.Path]::GetFileNameWithoutExtension($dll).ToLowerInvariant()
        if ($name -notmatch '^(api-ms-win-|ext-ms-win-)' -and $name -notin $allowed) { throw "Non-system import $dll in $binary" }
    }
    $imports[[IO.Path]::GetFileName($binary)] = $dlls
}
# Only compiled EXEs are copied: no SDK/runtime/resource files. Hide the SDK install while executing.
$tempBase = [IO.Path]::GetFullPath([IO.Path]::GetTempPath())
$private = Join-Path $tempBase ('cad2bem-static-' + [Guid]::NewGuid().ToString('N'))
$parked = $Prefix + '.proof-hidden'
if (Test-Path -LiteralPath $parked) { throw 'A previous SDK proof needs recovery before continuing' }
if ([IO.Path]::GetDirectoryName($parked) -ne [IO.Path]::GetDirectoryName($Prefix)) { throw 'Invalid SDK parking target' }
New-Item -ItemType Directory -Path $private | Out-Null
Copy-Item -LiteralPath $binaries[0] -Destination (Join-Path $private 'cad2bem.exe')
Copy-Item -LiteralPath $binaries[1] -Destination (Join-Path $private 'static_cad_probe.exe')
$saved = @{}
$keys = @('PATH','CAD2BEM_PYTHON','CAD2BEM_GMSH_LIBRARY','CONDA_PREFIX','VIRTUAL_ENV','PYTHONHOME','PYTHONPATH','CASROOT','OpenCASCADE_DIR')
$keys += @(Get-ChildItem Env: | Where-Object { $_.Name.StartsWith('CSF_') } | ForEach-Object { $_.Name })
try {
    foreach ($key in $keys) { $saved[$key] = [Environment]::GetEnvironmentVariable($key,'Process'); [Environment]::SetEnvironmentVariable($key,$null,'Process') }
    $env:PATH = "$env:SystemRoot\System32;$env:SystemRoot"
    # Both absolute paths have been checked to be siblings of the explicitly selected SDK directory.
    Move-Item -LiteralPath $Prefix -Destination $parked
    Checked (Join-Path $private 'static_cad_probe.exe') @((Join-Path $Out 'geometry')) 'probe' $private
    $probe = Get-Content -LiteralPath (Join-Path $Out 'geometry/static-probe.json') -Raw | ConvertFrom-Json
    foreach ($extension in @('igs','step')) {
        $request = @{ schema=1;source=(Join-Path $Out "geometry/box.$extension");library=$null;mode='preview';size_m=$null }
        $requestFile = Join-Path $Out "$extension-request.json"
        $responseFile = Join-Path $Out "$extension-response.json"
        [IO.File]::WriteAllText($requestFile,($request | ConvertTo-Json),(New-Object Text.UTF8Encoding $false))
        Checked (Join-Path $private 'cad2bem.exe') @('--worker',$requestFile,$responseFile) "worker-$extension" $private
        $response = Get-Content -LiteralPath $responseFile -Raw | ConvertFrom-Json
        if ($response.status -ne 'completed' -or $response.scene.faces.Count -ne 6) { throw "Static $extension worker failed" }
    }
    $result = @{schema=1;imports=$imports;probe=$probe;private_dlls=$false;sdk_hidden=$true;scope='static Gmsh and OpenCASCADE in the executable'}
    [IO.File]::WriteAllText((Join-Path $Out 'verification.json'),($result | ConvertTo-Json -Depth 15),(New-Object Text.UTF8Encoding $false))
} finally {
    if ((Test-Path -LiteralPath $parked) -and !(Test-Path -LiteralPath $Prefix)) { Move-Item -LiteralPath $parked -Destination $Prefix }
    foreach ($key in $saved.Keys) { [Environment]::SetEnvironmentVariable($key,$saved[$key],'Process') }
    $resolved = [IO.Path]::GetFullPath($private)
    if ([IO.Path]::GetDirectoryName($resolved) -eq $tempBase.TrimEnd([IO.Path]::DirectorySeparatorChar) -and [IO.Path]::GetFileName($resolved).StartsWith('cad2bem-static-')) {
        Remove-Item -LiteralPath $resolved -Recurse -Force
    } else { throw 'Refusing cleanup outside the owned temporary directory' }
}
Write-Host "Static CAD verified: $Out"
