param(
  [string]$Repo = $env:CC_SWITCH_RELEASE_REPO,
  [string]$ReleaseTag = $env:CC_SWITCH_RELEASE_TAG,
  [string]$MetadataUrl = $env:CC_SWITCH_METADATA_URL,
  [string]$DownloadUrl = $env:CC_SWITCH_DOWNLOAD_URL,
  [string]$Sha256 = $env:CC_SWITCH_SHA256,
  [switch]$DryRun
)

Set-StrictMode -Version Latest

function Write-CCSwitch {
  param([Parameter(Mandatory = $true)][string]$Message)
  Write-Host "[CC Switch] $Message"
}

function Throw-CCSwitch {
  param([Parameter(Mandatory = $true)][string]$Message)
  throw "[CC Switch] 安装失败：$Message"
}

function Get-CCSwitchWindowsArch {
  $arch = $env:PROCESSOR_ARCHITECTURE
  if ($env:PROCESSOR_ARCHITEW6432) {
    $arch = $env:PROCESSOR_ARCHITEW6432
  }

  switch -Regex ($arch) {
    '^(AMD64|X64|X86_64)$' { return 'x86_64' }
    default { Throw-CCSwitch "当前公司版 Windows 一键安装仅支持 x64，检测到架构：$arch" }
  }
}

function Get-CCSwitchMetadataUrl {
  param(
    [Parameter(Mandatory = $true)][string]$Repo,
    [string]$ReleaseTag,
    [string]$MetadataUrl
  )

  if ($MetadataUrl) {
    return $MetadataUrl
  }
  if ($ReleaseTag) {
    return "https://github.com/$Repo/releases/download/$ReleaseTag/latest-company.json"
  }
  return "https://github.com/$Repo/releases/latest/download/latest-company.json"
}

function Get-CCSwitchLatestMetadata {
  param([Parameter(Mandatory = $true)][string]$Url)

  Write-CCSwitch "正在读取 GitHub 版本信息：$Url"
  try {
    return Invoke-RestMethod -UseBasicParsing -Uri $Url
  } catch {
    Throw-CCSwitch "无法读取 GitHub Release 版本信息：$($_.Exception.Message)"
  }
}

function Read-CCSwitchProperty {
  param(
    [Parameter(Mandatory = $true)]$Object,
    [Parameter(Mandatory = $true)][string]$Name
  )

  if ($null -eq $Object) {
    return $null
  }
  if ($Object.PSObject.Properties.Name -contains $Name) {
    return $Object.$Name
  }
  return $null
}

function Get-CCSwitchInstallerInfo {
  param(
    [Parameter(Mandatory = $true)][string]$Repo,
    [Parameter(Mandatory = $true)][string]$Arch,
    [string]$ReleaseTag,
    [string]$MetadataUrl,
    [string]$DownloadUrl,
    [string]$Sha256
  )

  if ($DownloadUrl) {
    return [pscustomobject]@{
      Url = $DownloadUrl
      Sha256 = $Sha256
      Kind = if ($DownloadUrl -match '\.zip(\?|$)') { 'portable_zip' } else { 'msi' }
      Status = 'ready'
    }
  }

  $latest = Get-CCSwitchLatestMetadata -Url (Get-CCSwitchMetadataUrl -Repo $Repo -ReleaseTag $ReleaseTag -MetadataUrl $MetadataUrl)
  $platformKey = "windows-$Arch"
  $installers = Read-CCSwitchProperty -Object $latest -Name 'installers'
  $installer = Read-CCSwitchProperty -Object $installers -Name $platformKey

  if ($null -eq $installer) {
    $platforms = Read-CCSwitchProperty -Object $latest -Name 'platforms'
    $platform = Read-CCSwitchProperty -Object $platforms -Name $platformKey
    if ($null -ne $platform -and $platform.url) {
      return [pscustomobject]@{
        Url = [string]$platform.url
        Sha256 = $Sha256
        Kind = if ([string]$platform.url -match '\.zip(\?|$)') { 'portable_zip' } else { 'msi' }
        Status = 'ready'
      }
    }
    Throw-CCSwitch "latest-company.json 中没有 $platformKey 安装包"
  }

  return [pscustomobject]@{
    Url = [string]$installer.url
    Sha256 = if ($installer.sha256) { [string]$installer.sha256 } else { $Sha256 }
    Kind = if ($installer.kind) { [string]$installer.kind } elseif ([string]$installer.url -match '\.zip(\?|$)') { 'portable_zip' } else { 'msi' }
    Status = if ($installer.status) { [string]$installer.status } else { 'ready' }
  }
}

function Stop-CCSwitch {
  $processNames = @('cc-switch', 'CC Switch')
  foreach ($name in $processNames) {
    Get-Process -Name $name -ErrorAction SilentlyContinue | ForEach-Object {
      Write-CCSwitch "正在退出已打开的 CC Switch..."
      Stop-Process -Id $_.Id -Force -ErrorAction SilentlyContinue
    }
  }
}

function Start-CCSwitch {
  $candidatePaths = @(
    (Join-Path $env:LOCALAPPDATA 'Programs\CC Switch\cc-switch.exe'),
    (Join-Path $env:LOCALAPPDATA 'Programs\CC Switch\CC Switch.exe')
  )

  foreach ($path in $candidatePaths) {
    if (Test-Path $path) {
      Write-CCSwitch "安装完成，正在启动 CC Switch..."
      Start-Process -FilePath $path | Out-Null
      return
    }
  }

  $shortcutPaths = @(
    (Join-Path $env:APPDATA 'Microsoft\Windows\Start Menu\Programs\CC Switch\CC Switch.lnk'),
    (Join-Path ([Environment]::GetFolderPath('Desktop')) 'CC Switch.lnk')
  )
  foreach ($shortcut in $shortcutPaths) {
    if (Test-Path $shortcut) {
      Write-CCSwitch "安装完成，正在启动 CC Switch..."
      Start-Process -FilePath $shortcut | Out-Null
      return
    }
  }

  Write-CCSwitch "安装完成。未找到启动路径，请从开始菜单打开 CC Switch。"
}

function New-CCSwitchShortcut {
  param(
    [Parameter(Mandatory = $true)][string]$TargetPath,
    [Parameter(Mandatory = $true)][string]$ShortcutPath
  )

  $shortcutDir = Split-Path -Parent $ShortcutPath
  New-Item -ItemType Directory -Force -Path $shortcutDir | Out-Null

  $shell = New-Object -ComObject WScript.Shell
  $shortcut = $shell.CreateShortcut($ShortcutPath)
  $shortcut.TargetPath = $TargetPath
  $shortcut.WorkingDirectory = Split-Path -Parent $TargetPath
  $shortcut.IconLocation = "$TargetPath,0"
  $shortcut.Save()
}

function Install-CCSwitchPortableZip {
  param(
    [Parameter(Mandatory = $true)][string]$ArchivePath,
    [Parameter(Mandatory = $true)][string]$TempDir
  )

  $extractDir = Join-Path $TempDir 'portable'
  $installDir = Join-Path $env:LOCALAPPDATA 'Programs\CC Switch'

  Write-CCSwitch "正在解压 Portable 安装包..."
  Expand-Archive -Path $ArchivePath -DestinationPath $extractDir -Force

  $exe = Get-ChildItem -Path $extractDir -Recurse -File -Include 'cc-switch.exe', 'CC Switch.exe' |
    Select-Object -First 1
  if ($null -eq $exe) {
    Throw-CCSwitch 'Portable 包里没有找到 CC Switch 可执行文件'
  }

  Stop-CCSwitch
  Write-CCSwitch "正在安装到当前用户目录：$installDir"
  Remove-Item -Recurse -Force $installDir -ErrorAction SilentlyContinue
  New-Item -ItemType Directory -Force -Path $installDir | Out-Null
  Copy-Item -Path (Join-Path $exe.DirectoryName '*') -Destination $installDir -Recurse -Force

  $installedExe = Join-Path $installDir $exe.Name
  New-CCSwitchShortcut `
    -TargetPath $installedExe `
    -ShortcutPath (Join-Path $env:APPDATA 'Microsoft\Windows\Start Menu\Programs\CC Switch\CC Switch.lnk')
  New-CCSwitchShortcut `
    -TargetPath $installedExe `
    -ShortcutPath (Join-Path ([Environment]::GetFolderPath('Desktop')) 'CC Switch.lnk')

  Start-CCSwitch
}

function Install-CCSwitchMsi {
  param([Parameter(Mandatory = $true)][string]$InstallerPath)

  Stop-CCSwitch
  Unblock-File -Path $InstallerPath -ErrorAction SilentlyContinue

  Write-CCSwitch "正在安装到当前用户目录..."
  $arguments = @('/i', "`"$InstallerPath`"", '/qn', '/norestart', 'AUTOLAUNCHAPP=1')
  $process = Start-Process -FilePath 'msiexec.exe' -ArgumentList $arguments -Wait -PassThru
  if ($process.ExitCode -ne 0) {
    Throw-CCSwitch "msiexec 返回错误码 $($process.ExitCode)"
  }

  Start-CCSwitch
}

function Install-CCSwitch {
  param(
    [string]$Repo = $env:CC_SWITCH_RELEASE_REPO,
    [string]$ReleaseTag = $env:CC_SWITCH_RELEASE_TAG,
    [string]$MetadataUrl = $env:CC_SWITCH_METADATA_URL,
    [string]$DownloadUrl = $env:CC_SWITCH_DOWNLOAD_URL,
    [string]$Sha256 = $env:CC_SWITCH_SHA256,
    [switch]$DryRun
  )

  $ErrorActionPreference = 'Stop'

  if (-not $Repo) {
    $Repo = 'qinghua362330/cc-switch-company'
  }

  $arch = Get-CCSwitchWindowsArch
  $installerInfo = Get-CCSwitchInstallerInfo `
    -Repo $Repo `
    -Arch $arch `
    -ReleaseTag $ReleaseTag `
    -MetadataUrl $MetadataUrl `
    -DownloadUrl $DownloadUrl `
    -Sha256 $Sha256

  Write-CCSwitch "检测到 Windows 架构：$arch"
  Write-CCSwitch "下载地址：$($installerInfo.Url)"
  if ($installerInfo.Status -and $installerInfo.Status -ne 'ready') {
    Throw-CCSwitch "公司版 Windows 安装包尚未发布，当前状态：$($installerInfo.Status)。请先完成 GitHub Release。"
  }
  if ($installerInfo.Sha256) {
    Write-CCSwitch "SHA256：$($installerInfo.Sha256)"
  } else {
    Write-CCSwitch "未提供 SHA256，跳过哈希校验。"
  }

  if ($DryRun) {
    return
  }

  $tempDir = Join-Path $env:TEMP ("cc-switch-install-" + [Guid]::NewGuid().ToString('N'))
  New-Item -ItemType Directory -Force -Path $tempDir | Out-Null
  $downloadName = [IO.Path]::GetFileName(([Uri]$installerInfo.Url).AbsolutePath)
  if (-not $downloadName) {
    $downloadName = if ($installerInfo.Kind -eq 'portable_zip') {
      'CC-Switch-Windows-Portable.zip'
    } else {
      'CC-Switch-Windows.msi'
    }
  }
  $installerPath = Join-Path $tempDir $downloadName

  try {
    Write-CCSwitch "正在下载安装包..."
    try {
      Invoke-WebRequest -UseBasicParsing -Uri $installerInfo.Url -OutFile $installerPath
    } catch {
      Throw-CCSwitch "下载安装包失败，请确认 GitHub Release 安装包已发布：$($installerInfo.Url)"
    }

    if ($installerInfo.Sha256) {
      $actualHash = (Get-FileHash -Algorithm SHA256 -Path $installerPath).Hash.ToLowerInvariant()
      if ($actualHash -ne $installerInfo.Sha256.ToLowerInvariant()) {
        Throw-CCSwitch '校验失败，下载文件可能不完整'
      }
    }

    switch ($installerInfo.Kind) {
      'portable_zip' {
        Install-CCSwitchPortableZip -ArchivePath $installerPath -TempDir $tempDir
      }
      'msi' {
        Install-CCSwitchMsi -InstallerPath $installerPath
      }
      default {
        Throw-CCSwitch "不支持的 Windows 安装包类型：$($installerInfo.Kind)"
      }
    }

    Write-CCSwitch "好了，可以开始使用 CC Switch。"
  } finally {
    Remove-Item -Recurse -Force $tempDir -ErrorAction SilentlyContinue
  }
}

if ($MyInvocation.InvocationName -ne '.') {
  Install-CCSwitch `
    -Repo $Repo `
    -ReleaseTag $ReleaseTag `
    -MetadataUrl $MetadataUrl `
    -DownloadUrl $DownloadUrl `
    -Sha256 $Sha256 `
    -DryRun:$DryRun
}
