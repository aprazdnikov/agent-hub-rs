# Windows-only integration smoke test. Uses an isolated HKCU key and shortcuts.
# Usage: pwsh -File scripts/test-windows-installer.ps1 path\to\agent-hub.exe 1.2.3
param(
    [Parameter(Mandatory=$true, Position=0)][string]$Binary,
    [Parameter(Mandatory=$true, Position=1)][string]$Version
)
$ErrorActionPreference = 'Stop'
Set-StrictMode -Version Latest
$binaryPath = (Resolve-Path -LiteralPath $Binary).Path
$repo = Split-Path -Parent $PSScriptRoot
$compiler = (Get-Command makensis -ErrorAction Stop).Source
$id = [guid]::NewGuid().ToString('N')
$root = Join-Path ([System.IO.Path]::GetTempPath()) "agent-hub-nsis-smoke-$id"
$install = Join-Path $root 'install directory'
$shortcuts = Join-Path $root 'shortcuts'
$installer = Join-Path $root 'agent-hub-installer.exe'
$key = "Software\agent-hub-nsis-smoke-$id"
$regPath = "HKCU:\$key"
$lockPath = Join-Path $root 'installer.lock'
$heldLock = $null
function Wait-Bounded([System.Diagnostics.Process]$Process) {
    if (-not $Process.WaitForExit(30000)) {
        & taskkill.exe /PID $Process.Id /T /F | Out-Null
        throw "Process $($Process.Id) timed out and was terminated"
    }
    $Process.Refresh()
    return $Process.ExitCode
}
function Run-Installer([string]$Path, [string]$Destination, [bool]$Uninstall = $false) {
    if ($Uninstall) { $argsLine = "/S _?=$Destination" } else { $argsLine = "/S /D=$Destination" }
    $process = Start-Process -FilePath $Path -ArgumentList $argsLine -PassThru
    return (Wait-Bounded $process)
}
function Require([bool]$Condition, [string]$Message) {
    if (-not $Condition) { throw $Message }
}
function Run-NormalUninstall([string]$Path) {
    # NSIS launches a temporary copy and the first process exits immediately.
    $process = Start-Process -FilePath $Path -ArgumentList '/S' -PassThru
    $null = Wait-Bounded $process
    $deadline = [DateTime]::UtcNow.AddSeconds(30)
    do {
        $probe = $null
        try { $probe = [IO.File]::Open($lockPath, [IO.FileMode]::Open, [IO.FileAccess]::ReadWrite, [IO.FileShare]::None) }
        catch [IO.IOException] { }
        if ($null -ne $probe) {
            $probe.Dispose()
            if (-not (Test-Path -LiteralPath $regPath) -and -not (Test-Path -LiteralPath $Path)) { return }
        }
        Start-Sleep -Milliseconds 100
    } while ([DateTime]::UtcNow -lt $deadline)
    throw 'Normal uninstallation failed or timed out'
}
try {
    New-Item -ItemType Directory -Path $root, $install | Out-Null
    & $compiler '-V2' "-DINPUT_EXE=$binaryPath" "-DLICENSE_FILE=$repo\LICENSE" "-DOUTPUT_EXE=$installer" "-DVERSION=$Version" "-DTEST_UNINSTALL_KEY=$key" "-DTEST_SHORTCUT_DIR=$shortcuts" "-DTEST_LOCK_FILE=$lockPath" "$repo\packaging\windows\agent-hub.nsi"
    if ($LASTEXITCODE -ne 0) { throw "makensis failed: $LASTEXITCODE" }
    $exe = Join-Path $install 'agent-hub.exe'
    $marker = Join-Path $install 'agent-hub.install-kind'
    $uninstaller = Join-Path $install 'uninstall.exe'
    foreach ($name in @('agent-hub.exe.staged', 'LICENSE.staged', 'uninstall.exe.staged', 'agent-hub.install-kind.staged')) {
        $staged = Join-Path $install $name
        [IO.File]::WriteAllText($staged, 'do not touch')
        Require ((Run-Installer $installer $install) -ne 0) "Installer accepted $name"
        Require (([IO.File]::ReadAllText($staged)) -eq 'do not touch') "Installer changed $name"
        Require (-not (Test-Path -LiteralPath $regPath)) 'Rejected install created registry key'
        Remove-Item -LiteralPath $staged
    }

    # This user file resembles the old journal but must never be owned or removed.
    $foreignPending = Join-Path $install 'agent-hub.uninstall-pending'
    [IO.File]::WriteAllText($foreignPending, 'pending')
    Require ((Run-Installer $installer $install) -eq 0) 'Fresh installation with foreign pending file failed'
    Require (([IO.File]::ReadAllText($foreignPending)) -ceq 'pending') 'Fresh install changed a user file'
    Require ((Get-FileHash -LiteralPath $exe).Hash -eq (Get-FileHash -LiteralPath $binaryPath).Hash) 'Installed executable differs'
    Require ((Get-FileHash -LiteralPath (Join-Path $install 'LICENSE')).Hash -eq (Get-FileHash -LiteralPath "$repo\LICENSE").Hash) 'Installed license differs'
    Require (([IO.File]::ReadAllText($marker)) -ceq 'nsis') 'Install marker differs'
    Require (Test-Path -LiteralPath $uninstaller) 'Missing uninstaller'
    Require (Test-Path -LiteralPath $regPath) 'Missing uninstall registry key'
    Require (Test-Path -LiteralPath (Join-Path $shortcuts 'agent-hub.lnk')) 'Missing application shortcut'
    Require (Test-Path -LiteralPath (Join-Path $shortcuts 'Uninstall agent-hub.lnk')) 'Missing uninstall shortcut'
    $data = Join-Path $install 'user-data-sentinel.txt'
    [IO.File]::WriteAllText($data, 'keep me')

    $heldLock = [IO.File]::Open($lockPath, [IO.FileMode]::OpenOrCreate, [IO.FileAccess]::ReadWrite, [IO.FileShare]::None)
    try {
        Require ((Run-Installer $installer $install) -ne 0) 'Concurrent installer did not fail'
        Require ((Run-Installer $uninstaller $install $true) -ne 0) 'Concurrent uninstaller did not fail'
    } finally { $heldLock.Dispose(); $heldLock = $null }
    Require (Test-Path -LiteralPath $exe) 'Concurrent operation removed executable'
    Require (([IO.File]::ReadAllText($marker)) -ceq 'nsis') 'Concurrent operation changed marker'

    $heldFile = [IO.File]::Open($exe, [IO.FileMode]::Open, [IO.FileAccess]::Read, [IO.FileShare]::None)
    try { Require ((Run-Installer $installer $install) -ne 0) 'Upgrade accepted locked executable' }
    finally { $heldFile.Dispose() }
    Require ((Get-FileHash -LiteralPath $exe).Hash -eq (Get-FileHash -LiteralPath $binaryPath).Hash) 'Failed upgrade changed executable'
    Require (([IO.File]::ReadAllText($marker)) -ceq 'nsis') 'Failed upgrade changed marker'
    # An exited process releases its exclusive handle without explicit disposal.
    $escapedLock = $lockPath.Replace("'", "''")
    $childCommand = '$f = [IO.File]::Open(''' + $escapedLock + ''', [IO.FileMode]::OpenOrCreate, [IO.FileAccess]::ReadWrite, [IO.FileShare]::None)'
    $encoded = [Convert]::ToBase64String([Text.Encoding]::Unicode.GetBytes($childCommand))
    $engine = (Get-Process -Id $PID).Path
    $child = Start-Process -FilePath $engine -ArgumentList "-NoProfile -NonInteractive -EncodedCommand $encoded" -PassThru
    Require ((Wait-Bounded $child) -eq 0) 'Could not create exited lock owner fixture'
    # Simulate a fully published upgrade with a locked old backup.
    $backup = Join-Path $install 'LICENSE.previous'
    [IO.File]::WriteAllText($backup, 'old backup')
    Require ((Run-Installer $installer $install) -ne 0) 'Unjournaled backup was accepted'
    Require (([IO.File]::ReadAllText($backup)) -ceq 'old backup') 'Unjournaled backup changed'
    $foreignCommitted = Join-Path $install 'agent-hub.upgrade-committed'
    [IO.File]::WriteAllText($foreignCommitted, 'committed')
    Require ((Run-Installer $installer $install) -ne 0) 'User file was accepted as a commit flag'
    Require (([IO.File]::ReadAllText($backup)) -ceq 'old backup') 'User backup changed without a registry flag'
    Require (([IO.File]::ReadAllText($foreignCommitted)) -ceq 'committed') 'User commit file changed'
    New-ItemProperty -LiteralPath $regPath -Name UpgradeCommitted -Value 'committed' -PropertyType String | Out-Null
    $heldFile = [IO.File]::Open($backup, [IO.FileMode]::Open, [IO.FileAccess]::Read, [IO.FileShare]::None)
    try { Require ((Run-Installer $installer $install) -ne 0) 'Locked backup was reported as success' }
    finally { $heldFile.Dispose() }
    Require (Test-Path -LiteralPath $backup) 'Locked backup disappeared'
    Require ((Run-Installer $installer $install) -eq 0) 'Upgrade failed'
    Require (([IO.File]::ReadAllText($data)) -eq 'keep me') 'Upgrade changed user data'
    Require (([IO.File]::ReadAllText($marker)) -ceq 'nsis') 'Upgrade changed marker'
    Require (-not (Test-Path -LiteralPath $backup)) 'Retry did not remove backup'
    Require (([IO.File]::ReadAllText($foreignCommitted)) -ceq 'committed') 'Backup retry changed the user commit file'
    # Every locked-file check starts with a complete, independent installation.
    foreach ($target in @($exe, (Join-Path $install 'LICENSE'), (Join-Path $shortcuts 'agent-hub.lnk'), (Join-Path $shortcuts 'Uninstall agent-hub.lnk'), $marker)) {
        $name = [IO.Path]::GetFileName($target)
        Require (Test-Path -LiteralPath $target) "Missing fixture $name"
        $heldFile = [IO.File]::Open($target, [IO.FileMode]::Open, [IO.FileAccess]::Read, [IO.FileShare]::None)
        try { Require ((Run-Installer $uninstaller $install $true) -ne 0) "Uninstall accepted locked $name" }
        finally { $heldFile.Dispose() }
        Require (Test-Path -LiteralPath $regPath) "Locked $name removed retry registration"
        Require (Test-Path -LiteralPath $uninstaller) "Locked $name removed retry executable"
        if ($target -eq $exe) {
            Require (Test-Path -LiteralPath $exe) 'Locked executable disappeared'
            Require (([IO.File]::ReadAllText($marker)) -ceq 'nsis') 'Locked executable lost its marker'
        }
        Run-NormalUninstall $uninstaller
        Require (([IO.File]::ReadAllText($data)) -ceq 'keep me') 'Uninstall changed user data'
        Require ((Run-Installer $installer $install) -eq 0) "Reinstallation after locked $name failed"
    }

    # Simulate termination after deleting the registered executable but before
    # deleting its key. The installer must reconstruct a usable retry entry.
    Remove-Item -LiteralPath $exe, (Join-Path $install 'LICENSE'), $marker, (Join-Path $shortcuts 'agent-hub.lnk'), (Join-Path $shortcuts 'Uninstall agent-hub.lnk')
    New-ItemProperty -LiteralPath $regPath -Name UninstallPending -Value 'pending' -PropertyType String | Out-Null
    Remove-Item -LiteralPath $uninstaller
    Set-ItemProperty -LiteralPath $regPath -Name InstallLocation -Value (Join-Path $root 'another installation')
    Require ((Run-Installer $installer $install) -ne 0) 'Recovery accepted another installation registry key'
    Require (-not (Test-Path -LiteralPath $uninstaller)) 'Recovery wrote into an unrelated installation'
    Require ((Get-ItemProperty -LiteralPath $regPath -Name UninstallPending).UninstallPending -ceq 'pending') 'Rejected recovery changed the flag'
    Set-ItemProperty -LiteralPath $regPath -Name InstallLocation -Value $install
    # An interrupted or foreign recovery staging file is never overwritten or removed.
    $recoveryStaged = Join-Path $install 'uninstall-recovery.exe.staged'
    [IO.File]::WriteAllText($recoveryStaged, 'partial or user data')
    Require ((Run-Installer $installer $install) -ne 0) 'Recovery accepted an ambiguous staging file'
    Require (([IO.File]::ReadAllText($recoveryStaged)) -ceq 'partial or user data') 'Recovery changed ambiguous staging data'
    Require (-not (Test-Path -LiteralPath $uninstaller)) 'Recovery published an ambiguous staged executable'
    Remove-Item -LiteralPath $recoveryStaged
    Require ((Run-Installer $installer $install) -ne 0) 'Interrupted uninstall was reported as an installation'
    Require (Test-Path -LiteralPath $regPath) 'Recovery removed the retry registration'
    Require (Test-Path -LiteralPath $uninstaller) 'Recovery did not restore the retry executable'
    Require (-not ((Get-Item -LiteralPath $regPath).Property -contains 'UninstallPending')) 'Recovery left the registry flag'
    Run-NormalUninstall $uninstaller
    Require (([IO.File]::ReadAllText($data)) -ceq 'keep me') 'Recovery changed user data'
    Require (([IO.File]::ReadAllText($foreignPending)) -ceq 'pending') 'Uninstall changed the user pending file'

    # A crash after the key is deleted needs no journal cleanup: all registry
    # recovery state disappears with the key and user files remain untouched.
    Require ((Run-Installer $installer $install) -eq 0) 'Fresh installation after interrupted cleanup failed'
    Require (([IO.File]::ReadAllText($foreignPending)) -ceq 'pending') 'Reinstallation changed the user pending file'
    Run-NormalUninstall $uninstaller
    Require (-not (Test-Path -LiteralPath $exe)) 'Executable remains after uninstall'
    Require (-not (Test-Path -LiteralPath $marker)) 'Marker remains after uninstall'
    Require (-not (Test-Path -LiteralPath $regPath)) 'Registry key remains after uninstall'
    Require (-not (Test-Path -LiteralPath (Join-Path $shortcuts 'agent-hub.lnk'))) 'Shortcut remains after uninstall'
    Require (([IO.File]::ReadAllText($data)) -ceq 'keep me') 'Uninstall changed user data'
    Require (([IO.File]::ReadAllText($foreignPending)) -ceq 'pending') 'Uninstall changed the user pending file'
    Require (([IO.File]::ReadAllText($foreignCommitted)) -ceq 'committed') 'Uninstall changed the user commit file'
    Write-Output 'Windows installer smoke test passed'
} finally {
    if ($null -ne $heldLock) { $heldLock.Dispose() }
    if (Test-Path -LiteralPath $regPath) { Remove-Item -LiteralPath $regPath -Recurse -Force }
    if (Test-Path -LiteralPath $root) { Remove-Item -LiteralPath $root -Recurse -Force }
}
