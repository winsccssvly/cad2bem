# SPDX-License-Identifier: GPL-3.0-or-later
<#
.SYNOPSIS
Render the application icons from resources/logo.svg with ImageMagick (`magick`, with its librsvg renderer).

.DESCRIPTION
Writes resources/logo-256.png (the window icon, embedded by the executable) and resources/cad2bem.ico (the
executable's icon on Windows: 16, 24, 32, 48, 64, 128 and 256 pixels). Run it again after changing the logo
and commit the results; the build does not need ImageMagick.
#>
$ErrorActionPreference = 'Stop'
$resources = Join-Path (Split-Path -Parent $PSScriptRoot) 'resources'
$svg = Join-Path $resources 'logo.svg'
$work = Join-Path ([IO.Path]::GetTempPath()) ("cad2bem-icons-" + [Guid]::NewGuid())
New-Item -ItemType Directory $work | Out-Null
try {
    $sizes = 16, 24, 32, 48, 64, 128, 256
    $pngs = foreach ($size in $sizes) {
        $png = Join-Path $work "logo-$size.png"
        & magick -background none -density 768 $svg -resize "${size}x${size}" "PNG32:$png"
        if ($LASTEXITCODE) { throw "magick failed for $size px" }
        $png
    }
    Copy-Item (Join-Path $work 'logo-256.png') (Join-Path $resources 'logo-256.png') -Force
    & magick @pngs (Join-Path $resources 'cad2bem.ico')
    if ($LASTEXITCODE) { throw 'magick failed for the icon' }
} finally {
    Remove-Item -Recurse -Force $work
}
"Wrote resources/logo-256.png and resources/cad2bem.ico"
