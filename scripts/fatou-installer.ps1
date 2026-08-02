Param()

$ErrorActionPreference = 'Stop'

$repo = if ($env:FATOU_REPO) { $env:FATOU_REPO } else { 'jolars/fatou' }
$installDir = if ($env:FATOU_INSTALL_DIR) { $env:FATOU_INSTALL_DIR } else { Join-Path $env:LOCALAPPDATA 'Programs\fatou\bin' }
$tag = if ($env:FATOU_TAG) { $env:FATOU_TAG } else { $null }

$arch = [System.Runtime.InteropServices.RuntimeInformation]::OSArchitecture.ToString()
switch ($arch) {
    'X64' { $target = 'x86_64-pc-windows-msvc' }
    'Arm64' { $target = 'aarch64-pc-windows-msvc' }
    default { throw "Unsupported Windows architecture: $arch" }
}

$asset = "fatou-$target.zip"

function Resolve-DownloadUrl {
    param(
        [string]$Repository,
        [string]$AssetName,
        [string]$Tag
    )

    if ($Tag) {
        if ($Tag -match '^(v|fatou-v)') {
            $tagCandidates = @($Tag)
        } else {
            $tagCandidates = @("v$Tag", "fatou-v$Tag")
        }

        foreach ($tagCandidate in $tagCandidates) {
            $candidateUrl = "https://github.com/$Repository/releases/download/$tagCandidate/$AssetName"
            try {
                Invoke-WebRequest -Method Head -Uri $candidateUrl | Out-Null
                return $candidateUrl
            }
            catch {
                continue
            }
        }

        throw "Could not find release asset $AssetName for FATOU_TAG='$Tag' in $Repository."
    }

    $releases = Invoke-RestMethod -Uri "https://api.github.com/repos/$Repository/releases?per_page=100"
    foreach ($release in $releases) {
        foreach ($releaseAsset in $release.assets) {
            if ($releaseAsset.name -eq $AssetName) {
                return $releaseAsset.browser_download_url
            }
        }
    }
    throw "Could not find a release asset named $AssetName in $Repository."
}

$url = Resolve-DownloadUrl -Repository $repo -AssetName $asset -Tag $tag

$tmpDir = Join-Path ([System.IO.Path]::GetTempPath()) ("fatou-install-" + [System.Guid]::NewGuid().ToString('N'))
New-Item -ItemType Directory -Path $tmpDir | Out-Null

try {
    $zipPath = Join-Path $tmpDir $asset
    Write-Host "Downloading $asset..."
    Invoke-WebRequest -Uri $url -OutFile $zipPath

    Expand-Archive -Path $zipPath -DestinationPath $tmpDir -Force
    New-Item -ItemType Directory -Path $installDir -Force | Out-Null
    Copy-Item -Path (Join-Path $tmpDir 'fatou.exe') -Destination (Join-Path $installDir 'fatou.exe') -Force

    Write-Host "Installed fatou to $(Join-Path $installDir 'fatou.exe')"
    if (-not (($env:Path -split ';') -contains $installDir)) {
        Write-Host "Note: $installDir is not in PATH."
    }
}
finally {
    Remove-Item -Recurse -Force $tmpDir -ErrorAction SilentlyContinue
}
