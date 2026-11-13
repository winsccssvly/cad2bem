# SPDX-License-Identifier: GPL-3.0-or-later
<# Build a pinned headless Gmsh/OCCT static SDK with a matching MSVC static CRT. #>
[CmdletBinding()]
param(
    [Parameter(Mandatory=$true)][string]$VisualStudioPath,
    [ValidateSet('Occt','Gmsh','All')][string]$Stage = 'All',
    [int]$Jobs = 8,
    [string]$BuildRoot,
    [string]$SourceRoot,
    # The perfect matching of Gmsh's quadrilateral recombination: LEMON (Boost licence; the default, for a release),
    # or Gmsh's Blossom IV, whose licence allows it only within Gmsh and so not in a GPL executable that links Gmsh.
    [ValidateSet('Lemon','Blossom')][string]$Matching = 'Lemon'
)
$ErrorActionPreference = 'Stop'
$repo = Split-Path -Parent $PSScriptRoot
if (!$BuildRoot) { $BuildRoot = Join-Path $repo 'out/static-cad' }
if (!$SourceRoot) { $SourceRoot = Join-Path $repo 'out/native-sources' }
$BuildRoot = [IO.Path]::GetFullPath($BuildRoot)
$SourceRoot = [IO.Path]::GetFullPath($SourceRoot)
New-Item -ItemType Directory -Force -Path $BuildRoot,$SourceRoot | Out-Null
$lock = Get-Content -LiteralPath (Join-Path $repo 'scripts/native-sources.json') -Raw | ConvertFrom-Json
$devShell = Join-Path $VisualStudioPath 'Common7/Tools/Launch-VsDevShell.ps1'
& $devShell -Arch amd64 -HostArch amd64 -SkipAutomaticLocation -NoLogo | Out-Null
$env:VSLANG = '1033'
# __FILE__ (assertions, exception messages) relative to the source and build folders: the executable then
# carries no path of the build machine. cl reads CL on every compile; a change needs a clean build.
$env:CL = "/d1trimfile:`"$SourceRoot`" /d1trimfile:`"$BuildRoot`""
$cmake = Join-Path $VisualStudioPath 'Common7/IDE/CommonExtensions/Microsoft/CMake/CMake/bin/cmake.exe'
$ninja = Join-Path $VisualStudioPath 'Common7/IDE/CommonExtensions/Microsoft/CMake/Ninja/ninja.exe'
function Source($item) {
    $archive = Join-Path $SourceRoot $item.filename
    if (!(Test-Path -LiteralPath $archive)) { Invoke-WebRequest -Uri $item.url -OutFile $archive -UseBasicParsing }
    if ((Get-FileHash -LiteralPath $archive -Algorithm SHA256).Hash.ToLowerInvariant() -ne $item.sha256) { throw "Source hash mismatch: $($item.filename)" }
    $directory = Join-Path $SourceRoot $item.directory
    if (!(Test-Path -LiteralPath (Join-Path $directory 'CMakeLists.txt'))) {
        & tar -xf $archive -C $SourceRoot
        if ($LASTEXITCODE -ne 0) { throw "Cannot extract $archive" }
    }
    return $directory
}
function Run-CMake([string]$name, [string[]]$arguments) {
    $log = Join-Path $BuildRoot "$name.log"
    Write-Host "$name (log: $log)"
    $errorLog = Join-Path $BuildRoot "$name.stderr.log"
    # Separate native streams avoid Windows PowerShell turning compiler stderr into huge ErrorRecords.
    $quoted = @($arguments | ForEach-Object { '"' + $_.Replace('"','\"') + '"' })
    $process = Start-Process -FilePath $cmake -ArgumentList $quoted -WindowStyle Hidden -PassThru `
        -WorkingDirectory $BuildRoot -RedirectStandardOutput $log -RedirectStandardError $errorLog
    $null = $process.Handle
    $process.WaitForExit()
    if ($process.ExitCode -ne 0) {
        foreach ($file in @($log,$errorLog)) {
            Get-Content -LiteralPath $file -Tail 20 | ForEach-Object { if ($_.Length -gt 500) { $_.Substring(0,500) } else { $_ } }
        }
        throw "$name failed ($($process.ExitCode))"
    }
}
$prefix = Join-Path $BuildRoot 'install'
$common = @('-G','Ninja',"-DCMAKE_MAKE_PROGRAM=$ninja",'-DCMAKE_BUILD_TYPE=Release',
    '-DCMAKE_POLICY_VERSION_MINIMUM=3.5','-DCMAKE_POLICY_DEFAULT_CMP0091=NEW',
    '-DCMAKE_CL_SHOWINCLUDES_PREFIX=Note: including file:',
    '-DCMAKE_MSVC_RUNTIME_LIBRARY=MultiThreaded',"-DCMAKE_INSTALL_PREFIX=$prefix")
if ($Stage -eq 'Occt' -or $Stage -eq 'All') {
    $src = Source $lock.occt
    $build = Join-Path $BuildRoot 'occt'
    $options = @('-DBUILD_LIBRARY_TYPE=Static','-DBUILD_CPP_STANDARD=C++14',
        '-DBUILD_MODULE_FoundationClasses=OFF','-DBUILD_MODULE_ModelingData=OFF','-DBUILD_MODULE_ModelingAlgorithms=OFF',
        '-DBUILD_MODULE_Visualization=OFF','-DBUILD_MODULE_ApplicationFramework=OFF','-DBUILD_MODULE_DataExchange=OFF',
        '-DBUILD_MODULE_Draw=OFF','-DBUILD_MODULE_DETools=OFF',
        '-DBUILD_ADDITIONAL_TOOLKITS=TKDESTEP;TKDEIGES;TKBinXCAF;TKXmlXCAF;TKOffset;TKFeat;TKFillet;TKBool;TKMesh;TKHLR;TKBO;TKPrim;TKShHealing;TKTopAlgo;TKGeomAlgo',
        '-DBUILD_DOC_Overview=OFF','-DUSE_TK=OFF','-DUSE_FREETYPE=OFF','-DUSE_TBB=OFF','-DUSE_OPENGL=OFF','-DUSE_GLES2=OFF',
        '-DUSE_FREEIMAGE=OFF','-DUSE_FFMPEG=OFF','-DUSE_VTK=OFF','-DUSE_RAPIDJSON=OFF',
        '-DINSTALL_DIR_LIB=lib','-DINSTALL_DIR_INCLUDE=include/opencascade','-DINSTALL_DIR_RESOURCE=share/opencascade/resources')
    Run-CMake 'occt-configure' (@('-S',$src,'-B',$build) + $common + $options)
    Run-CMake 'occt-build' @('--build',$build,'--parallel',"$Jobs")
    Run-CMake 'occt-install' @('--install',$build)
}
if ($Stage -eq 'Gmsh' -or $Stage -eq 'All') {
    if (!(Test-Path -LiteralPath (Join-Path $prefix 'lib/TKernel.lib')) -or !(Test-Path -LiteralPath (Join-Path $prefix 'include/opencascade/Standard_Version.hxx'))) {
        throw 'OCCT installation must finish before configuring Gmsh'
    }
    # A Gmsh installed earlier must not be compiled against: its GmshConfig.h in the prefix comes before the new
    # one on the include path (a build without Blossom once compiled with the old HAVE_BLOSSOM).
    foreach ($old in @('include/GmshConfig.h','include/gmshc.h','lib/gmsh.lib','static-link.txt')) {
        Remove-Item -LiteralPath (Join-Path $prefix $old) -Force -ErrorAction SilentlyContinue
    }
    $src = Source $lock.gmsh
    if ($Matching -eq 'Lemon') {
        # LEMON's headers (header only; config.h is what its CMake would write without LP solvers), and the matching
        # that Gmsh's recombination calls in place of Blossom IV's perfect_match.
        $lemon = Source $lock.lemon
        [IO.File]::WriteAllText((Join-Path $lemon 'lemon/config.h'), "#define LEMON_VERSION `"$($lock.lemon.version)`"`n#define LEMON_HAVE_LONG_LONG 1`n")
        $env:CL = "$env:CL /I`"$lemon`""
        Copy-Item -LiteralPath (Join-Path $PSScriptRoot 'gmsh/cad2bemLemonMatching.h') -Destination (Join-Path $src 'src/mesh/cad2bemLemonMatching.h') -Force
        $optimize = Join-Path $src 'src/mesh/meshGFaceOptimize.cpp'
        $text = [IO.File]::ReadAllText($optimize)
        if (!$text.Contains('cad2bemPerfectMatch')) {
            $edits = @(
                @("                             int partialprice, double *totalzeit);`n#endif`n",
                  "                             int partialprice, double *totalzeit);`n#endif`n#if !defined(HAVE_BLOSSOM)`n#include `"cad2bemLemonMatching.h`" // cad2bem: LEMON's matching in place of Blossom IV`n#endif`n"),
                @("  if(blossom) {`n#if defined(HAVE_BLOSSOM)`n",
                  "  if(blossom) {`n#if defined(HAVE_BLOSSOM) || defined(CAD2BEM_LEMON_MATCHING_H) // cad2bem: or LEMON's matching`n"),
                @("      if(perfect_match(ncount, nullptr, ecount, &elist, &elen, nullptr,`n                       MATCHFILE, 0, 0, 0, 0, &matzeit)) {`n",
                  "#if defined(HAVE_BLOSSOM)`n      if(perfect_match(ncount, nullptr, ecount, &elist, &elen, nullptr,`n                       MATCHFILE, 0, 0, 0, 0, &matzeit)) {`n#else`n      if(cad2bemPerfectMatch(ncount, ecount, &elist, &elen)) { // cad2bem`n#endif`n")
            )
            foreach ($edit in $edits) {
                if (([regex]::Matches($text, [regex]::Escape($edit[0]))).Count -ne 1) { throw "Pinned Gmsh matching patch context changed: $($edit[0])" }
                $text = $text.Replace($edit[0], $edit[1])
            }
            [IO.File]::WriteAllText($optimize, $text)
        }
        # The default recombination stays Blossom's (algorithm 1), now LEMON's matching; without Blossom Gmsh would
        # fall back to the simple one for every mesh that does not set it.
        $defaults = Join-Path $src 'src/common/DefaultOptions.h'
        $text = [IO.File]::ReadAllText($defaults)
        $original = "#if defined(HAVE_BLOSSOM)`n  { F|O, `"RecombinationAlgorithm`""
        $replacement = "#if defined(HAVE_BLOSSOM) || 1 // cad2bem: Blossom IV or LEMON's matching`n  { F|O, `"RecombinationAlgorithm`""
        if ($text.Contains($original)) { [IO.File]::WriteAllText($defaults, $text.Replace($original, $replacement)) }
        elseif (!$text.Contains($replacement)) { throw 'Pinned Gmsh recombination default patch context changed' }
    }
    # Gmsh 4.15.2 declares OCC_STATIC but still forces dllimport with MSVC. Patch only the pinned text.
    $cmakeFile = Join-Path $src 'CMakeLists.txt'
    $text = [IO.File]::ReadAllText($cmakeFile)
    $original = 'if(WIN32 AND MSVC)' + "`n" + '      add_definitions(-D "Standard_EXPORT=__declspec(dllimport)")' + "`n" + '    endif()'
    $replacement = 'if(WIN32 AND MSVC AND NOT ENABLE_OCC_STATIC)' + "`n" + '      add_definitions(-D "Standard_EXPORT=__declspec(dllimport)")' + "`n" + '    elseif(ENABLE_OCC_STATIC)' + "`n" + '      add_definitions(-DOCCT_STATIC_BUILD)' + "`n" + '    endif()'
    if ($text.Contains($original)) { [IO.File]::WriteAllText($cmakeFile, $text.Replace($original,$replacement)) }
    elseif (!$text.Contains($replacement)) { throw 'Pinned Gmsh static OCC patch context changed' }
    # Gmsh 4.15.2: the frontal quad mesher (algorithm 8) changes two globals of the Delaunay mesher and resets
    # them only at its normal end; on its early error return every later frontal mesh of the process kept
    # them. Reset them there too, so that a face's mesh does not depend on a failed one before it.
    $delaunay = Join-Path $src 'src/mesh/meshGFaceDelaunayInsertion.cpp'
    $text = [IO.File]::ReadAllText($delaunay)
    $head = '    MTri3::radiusNorm = -1;' + "`n" + '  }' + "`n`n" + '  if(!buildMeshGenerationDataStructures(gf, AllTris, DATA)) {' + "`n" + '    Msg::Error("Invalid meshing data structure");' + "`n"
    $original = $head + '    return;'
    $replacement = $head + '    MTri3::radiusNorm = 2; // cad2bem: restore the globals set above' + "`n" + '    LIMIT_ = 0.5 * std::sqrt(2.0) * 1;' + "`n" + '    return;'
    if ($text.Contains($original)) { [IO.File]::WriteAllText($delaunay, $text.Replace($original,$replacement)) }
    elseif (!$text.Contains($replacement)) { throw 'Pinned Gmsh frontal quad patch context changed' }
    # Gmsh 4.15.2 quasi-structured quads: a mesh patch lists its vertices in pointer order (sorted addresses),
    # and the smoothing moves them one after the other in that order. Addresses change from run to run, so
    # the mesh did too. Order them by vertex number instead.
    $patchFile = Join-Path $src 'contrib/QuadMeshingTools/qmtMeshUtils.cpp'
    $text = [IO.File]::ReadAllText($patchFile)
    $original = '      sort_unique(patch.bdrVertices[0]);' + "`n"
    $replacement = '      sort_unique(patch.bdrVertices[0]);' + "`n" + '      std::sort(patch.bdrVertices[0].begin(), patch.bdrVertices[0].end(), [](const MVertex *a, const MVertex *b) { return a->getNum() < b->getNum(); }); // cad2bem: number order, not address order' + "`n"
    $original2 = "  patch.elements = elements;`n"
    $replacement2 = '  /* cad2bem: number order, not address order (addresses change from run to run) */' + "`n" + '  std::sort(patch.intVertices.begin(), patch.intVertices.end(), [](const MVertex *a, const MVertex *b) { return a->getNum() < b->getNum(); });' + "`n" + '  std::sort(patch.embVertices.begin(), patch.embVertices.end(), [](const MVertex *a, const MVertex *b) { return a->getNum() < b->getNum(); });' + "`n" + $original2
    if ($text.Contains($replacement) -and $text.Contains($replacement2)) { } # already applied
    elseif ($text.Contains($original) -and ([regex]::Matches($text, [regex]::Escape($original2))).Count -eq 1) {
        [IO.File]::WriteAllText($patchFile, $text.Replace($original,$replacement).Replace($original2,$replacement2))
    }
    else { throw 'Pinned Gmsh quad patch order context changed' }
    # Gmsh 4.15.2 quasi-structured refinement deleted each old vertex as soon as its replacement was made, but kept
    # the old address as a key to update the elements afterwards: a later vertex given the freed address was then
    # replaced too. Whether an address came back depended on the earlier heap use, so a face's quads did as well.
    # Delete the old vertices only after the elements are updated.
    $quasi = Join-Path $src 'src/mesh/meshQuadQuasiStructured.cpp'
    $text = [IO.File]::ReadAllText($quasi)
    if (!$text.Contains('cad2bemDeleted')) {
        $start = $text.IndexOf('int RefineMeshWithBackgroundMeshProjectionSimple(GModel *gm)')
        $end = $text.IndexOf('int RefineMeshWithBackgroundMeshProjection(GModel *gm)', $start + 1)
        if ($start -lt 0 -or $end -lt 0) { throw 'Pinned Gmsh refine patch context changed' }
        $body = $text.Substring($start, $end - $start)
        $declaration = '  unordered_map<MVertex *, MVertex *> old2new;' + "`n"
        $deletes = ([regex]::Matches($body, [regex]::Escape("        delete v;`n"))).Count
        $tail = '  if(PARANO_VALIDITY) {'
        if (([regex]::Matches($body, [regex]::Escape($declaration))).Count -ne 1 -or $deletes -ne 2 -or ([regex]::Matches($body, [regex]::Escape($tail))).Count -ne 1) {
            throw 'Pinned Gmsh refine patch context changed'
        }
        $body = $body.Replace($declaration, $declaration + '  std::vector<MVertex *> cad2bemDeleted; // cad2bem: delete after the elements are updated' + "`n")
        $body = $body.Replace("        delete v;`n", "        cad2bemDeleted.push_back(v);`n")
        $body = $body.Replace($tail, '  for(MVertex *v : cad2bemDeleted) delete v;' + "`n`n" + $tail)
        [IO.File]::WriteAllText($quasi, $text.Substring(0, $start) + $body + $text.Substring($end))
    }
    # The same refinement makes each quad's centre as a sum over its corners, whose order (where the element's
    # vertex list starts) came out differently after other faces had been meshed: the centre then differed in the
    # last bit, and later threshold decisions turned that into another quad layout. Start every element at its
    # vertex of smallest number (the cycle, so the orientation, is kept) before refining.
    $text = [IO.File]::ReadAllText($quasi)
    if (!$text.Contains('cad2bemCanonical')) {
        $start = $text.IndexOf('int RefineMeshWithBackgroundMeshProjectionSimple(GModel *gm)')
        $end = $text.IndexOf('int RefineMeshWithBackgroundMeshProjection(GModel *gm)', $start + 1)
        if ($start -lt 0 -or $end -lt 0) { throw 'Pinned Gmsh refine order patch context changed' }
        $body = $text.Substring($start, $end - $start)
        $anchor = '  bool linear = true;' + "`n" + '  RefineMesh(gm, linear, true, false);' + "`n"
        if (([regex]::Matches($body, [regex]::Escape($anchor))).Count -ne 1) { throw 'Pinned Gmsh refine order patch context changed' }
        $canonical = '  bool linear = true;' + "`n" +
            '  /* cad2bem: each element starts at its vertex of smallest number (cad2bemCanonical) */' + "`n" +
            '  for(GFace *gf : gm->getFaces()) {' + "`n" +
            '    auto canonical = [](MElement *e) {' + "`n" +
            '      std::size_t n = e->getNumPrimaryVertices(), k0 = 0;' + "`n" +
            '      if(n > 4) return;' + "`n" +
            '      for(std::size_t k = 1; k < n; k++)' + "`n" +
            '        if(e->getVertex(k)->getNum() < e->getVertex(k0)->getNum()) k0 = k;' + "`n" +
            '      if(!k0) return;' + "`n" +
            '      MVertex *vs[4];' + "`n" +
            '      for(std::size_t k = 0; k < n; k++) vs[k] = e->getVertex((k + k0) % n);' + "`n" +
            '      for(std::size_t k = 0; k < n; k++) e->setVertex(k, vs[k]);' + "`n" +
            '    };' + "`n" +
            '    for(MTriangle *t : gf->triangles) canonical(t);' + "`n" +
            '    for(MQuadrangle *q : gf->quadrangles) canonical(q);' + "`n" +
            '  }' + "`n" +
            '  RefineMesh(gm, linear, true, false);' + "`n"
        $body = $body.Replace($anchor, $canonical)
        [IO.File]::WriteAllText($quasi, $text.Substring(0, $start) + $body + $text.Substring($end))
    }
    # The quad meshing tools sort, intersect and subtract vertex and element pointers in many places with
    # cppUtils.h: order mesh entities there by number too (other values as before).
    $utils = Join-Path $src 'contrib/QuadMeshingTools/cppUtils.h'
    $text = [IO.File]::ReadAllText($utils)
    if (!$text.Contains('cad2bem_less')) {
        $less = '  /* cad2bem: mesh entities (vertices, elements) in number order, not address order, which changes from' + "`n" +
            '   * run to run; other values as before. */' + "`n" +
            '  template<class T> inline auto cad2bem_less(T *const &a, T *const &b, int) -> decltype(a->getNum() < b->getNum()) { return a->getNum() < b->getNum(); }' + "`n" +
            '  template<class T> inline bool cad2bem_less(const T &a, const T &b, long) { return a < b; }' + "`n" +
            '  struct Cad2bemLess { template<class T> bool operator()(const T &a, const T &b) const { return cad2bem_less(a, b, 0); } };' + "`n`n"
        $edits = @(
            # (A comma binds before +: each concatenated element is in parentheses.)
            @(('  template<class T>' + "`n" + '  void sort_unique(std::vector<T>& vec) {' + "`n" + '      std::sort( vec.begin(), vec.end() );'),
              ($less + '  template<class T>' + "`n" + '  void sort_unique(std::vector<T>& vec) {' + "`n" + '      std::sort( vec.begin(), vec.end(), Cad2bemLess() );')),
            @('[&in](const T2& a, const T2&b){ return (in[a] < in[b]); }', '[&in](const T2& a, const T2&b){ return Cad2bemLess()(in[a], in[b]); }'),
            @('      std::sort(uniques.begin(), uniques.end());', '      std::sort(uniques.begin(), uniques.end(), Cad2bemLess());'),
            @('set_intersection(s1.begin(),s1.end(),s2.begin(),s2.end(), std::back_inserter(s3));', 'set_intersection(s1.begin(),s1.end(),s2.begin(),s2.end(), std::back_inserter(s3), Cad2bemLess());'),
            @('set_union(s1.begin(),s1.end(),s2.begin(),s2.end(), std::back_inserter(s3));', 'set_union(s1.begin(),s1.end(),s2.begin(),s2.end(), std::back_inserter(s3), Cad2bemLess());'),
            @('set_difference(s1.begin(),s1.end(),s2.begin(),s2.end(), std::inserter(s3,s3.begin()));', 'set_difference(s1.begin(),s1.end(),s2.begin(),s2.end(), std::inserter(s3,s3.begin()), Cad2bemLess());')
        )
        foreach ($edit in $edits) {
            if (([regex]::Matches($text, [regex]::Escape($edit[0]))).Count -ne 1) { throw "Pinned Gmsh cppUtils patch context changed: $($edit[0])" }
            $text = $text.Replace($edit[0], $edit[1])
        }
        [IO.File]::WriteAllText($utils, $text)
    }
    $build = Join-Path $BuildRoot 'gmsh'
    $options = @('-DENABLE_BUILD_LIB=ON','-DENABLE_BUILD_SHARED=OFF','-DENABLE_BUILD_DYNAMIC=OFF',
        '-DENABLE_MSVC_STATIC_RUNTIME=ON','-DENABLE_OCC=ON','-DENABLE_OCC_STATIC=ON','-DENABLE_OCC_CAF=OFF',
        '-DGMSH_RELEASE=ON','-DGMSH_PACKAGER=cad2bem-static',
        "-DOCC_INC=$prefix/include/opencascade","-DCMAKE_PREFIX_PATH=$prefix",'-DENABLE_MESH=ON',
        "-DENABLE_BLOSSOM=$(if ($Matching -eq 'Blossom') { 'ON' } else { 'OFF' })",'-DENABLE_QUADMESHINGTOOLS=ON','-DENABLE_FLTK=OFF','-DENABLE_GRAPHICS=OFF',
        '-DENABLE_OPENMP=OFF','-DENABLE_BLAS_LAPACK=OFF','-DENABLE_PETSC=OFF','-DENABLE_SLEPC=OFF',
        '-DENABLE_WRAP_PYTHON=OFF','-DENABLE_WRAP_JAVA=OFF','-DENABLE_NUMPY=OFF','-DENABLE_TESTS=OFF',
        '-DENABLE_ZIPPER=OFF','-DENABLE_CAIRO=OFF','-DENABLE_MED=OFF','-DENABLE_CGNS=OFF',
        '-DENABLE_HXT=OFF','-DENABLE_METIS=OFF','-DENABLE_NETGEN=OFF',
        '-DENABLE_OS_SPECIFIC_INSTALL=OFF','-DENABLE_PACKAGE_STRIP=OFF')
    Run-CMake 'gmsh-configure' (@('-S',$src,'-B',$build) + $common + $options)
    $features = [IO.File]::ReadAllText((Join-Path $build 'src/common/GmshConfig.h'))
    $required = @('HAVE_OCC','HAVE_MESH','HAVE_QUADMESHINGTOOLS') + @(if ($Matching -eq 'Blossom') { 'HAVE_BLOSSOM' })
    foreach ($feature in $required) {
        if ($features -notmatch "(?m)^#define\s+$feature\b") { throw "Missing required CAD/mesh feature $feature" }
    }
    if ($Matching -ne 'Blossom' -and $features -match '(?m)^#define\s+HAVE_BLOSSOM\b') { throw 'Blossom is still configured' }
    Run-CMake 'gmsh-build' @('--build',$build,'--target','lib','--parallel',"$Jobs")
    # Install only the static SDK. Gmsh's normal install also expects its standalone executable.
    New-Item -ItemType Directory -Force -Path (Join-Path $prefix 'include') | Out-Null
    Copy-Item -LiteralPath (Join-Path $build 'gmsh.lib') -Destination (Join-Path $prefix 'lib/gmsh.lib')
    Copy-Item -LiteralPath (Join-Path $src 'api/gmshc.h') -Destination (Join-Path $prefix 'include/gmshc.h')
    Copy-Item -LiteralPath (Join-Path $build 'src/common/GmshConfig.h') -Destination (Join-Path $prefix 'include/GmshConfig.h')
    $cache = Get-Content -LiteralPath (Join-Path $build 'CMakeCache.txt')
    $external = ($cache | Where-Object { $_.StartsWith('GMSH_EXTERNAL_LIBRARIES:STRING=') }) -replace '^GMSH_EXTERNAL_LIBRARIES:STRING=',''
    $records = @()
    foreach ($entry in ($external -split ';')) {
        if (!$entry) { continue }
        if ($entry.EndsWith('.lib')) {
            $resolved = [IO.Path]::GetFullPath($entry)
            if (!(Test-Path -LiteralPath $resolved)) { throw "Missing link input $entry" }
            if (!(Split-Path -Parent $resolved).Equals((Join-Path $prefix 'lib'),[StringComparison]::OrdinalIgnoreCase)) {
                throw "Unexpected external static library location: $entry"
            }
            $records += 'static:' + [IO.Path]::GetFileNameWithoutExtension($entry)
        } elseif ($entry -match '^[A-Za-z0-9_]+$') {
            $records += 'system:' + $entry
        } else { throw "Unsupported Gmsh link input: $entry" }
    }
    # Static OCCT readers have dependencies beyond Gmsh's direct OCC list (e.g. format providers).
    foreach ($library in (Get-ChildItem -LiteralPath (Join-Path $prefix 'lib') -Filter 'TK*.lib' -File)) {
        $records += 'static:' + $library.BaseName
    }
    [IO.File]::WriteAllLines((Join-Path $prefix 'static-link.txt'),@($records | Select-Object -Unique),(New-Object Text.UTF8Encoding $false))
}
Write-Host "Static CAD prefix: $prefix"
