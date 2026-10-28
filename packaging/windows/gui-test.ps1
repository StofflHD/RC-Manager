# GUI test of the Windows build on a GitHub Windows runner (CI only – it switches on Remote
# Desktop and changes the runner's RDP settings). Sessions in tabs: RDP via wfreerdp against
# the runner itself (127.0.0.2), SSH via PuTTY; screenshots of every step go to $Out.
#
#   packaging/windows/gui-test.ps1 -App dist\RC-Manager-x-windows-x64 -Out gui-win
param([Parameter(Mandatory)][string]$App, [Parameter(Mandatory)][string]$Out)
$ErrorActionPreference = 'Continue'
New-Item -ItemType Directory -Force $Out | Out-Null
$Out = (Resolve-Path $Out).Path
$exe = Join-Path (Resolve-Path $App).Path 'rc-manager.exe'

Add-Type -AssemblyName System.Drawing, System.Windows.Forms
Add-Type @"
using System; using System.Runtime.InteropServices;
public static class U {
  [DllImport("user32.dll")] public static extern bool SetCursorPos(int x, int y);
  [DllImport("user32.dll")] public static extern void mouse_event(uint f, uint dx, uint dy, uint d, IntPtr e);
  [DllImport("user32.dll")] public static extern bool SetWindowPos(IntPtr h, IntPtr a, int x, int y, int w, int hh, uint f);
  [DllImport("user32.dll")] public static extern bool SetForegroundWindow(IntPtr h);
  public static void Click(int x, int y) { SetCursorPos(x, y); mouse_event(2, 0, 0, 0, IntPtr.Zero); mouse_event(4, 0, 0, 0, IntPtr.Zero); }
}
"@

function Shot([string]$name) {
    $b = [System.Windows.Forms.Screen]::PrimaryScreen.Bounds
    $bmp = New-Object System.Drawing.Bitmap $b.Width, $b.Height
    $g = [System.Drawing.Graphics]::FromImage($bmp)
    $g.CopyFromScreen($b.Location, [System.Drawing.Point]::Empty, $b.Size)
    $bmp.Save((Join-Path $Out "$name.png"), [System.Drawing.Imaging.ImageFormat]::Png)
    $g.Dispose(); $bmp.Dispose()
}

function Start-App([string[]]$arguments, [string]$log) {
    $p = Start-Process -FilePath $exe -ArgumentList $arguments -PassThru `
        -RedirectStandardOutput (Join-Path $Out "$log.out.txt") -RedirectStandardError (Join-Path $Out "$log.err.txt")
    Start-Sleep -Seconds 5
    $p.Refresh()
    if ($p.MainWindowHandle -ne [IntPtr]::Zero) {
        # A fixed place and size, so dialogs appear at known positions.
        [U]::SetWindowPos($p.MainWindowHandle, [IntPtr]::Zero, 0, 0, 1150, 760, 0x0040) | Out-Null
        [U]::SetForegroundWindow($p.MainWindowHandle) | Out-Null
    }
    return $p
}

"Screen: " + [System.Windows.Forms.Screen]::PrimaryScreen.Bounds | Out-File (Join-Path $Out 'info.txt')

# Remote Desktop on the runner: TLS without NLA, so FreeRDP reaches the logon screen without
# valid credentials (the test data has none for this machine).
$ts = 'HKLM:\System\CurrentControlSet\Control\Terminal Server'
Set-ItemProperty $ts -Name fDenyTSConnections -Value 0
Set-ItemProperty "$ts\WinStations\RDP-Tcp" -Name UserAuthentication -Value 0
Set-ItemProperty "$ts\WinStations\RDP-Tcp" -Name SecurityLayer -Value 2
Enable-NetFirewallRule -DisplayGroup 'Remote Desktop' -ErrorAction SilentlyContinue
Restart-Service TermService -Force -ErrorAction SilentlyContinue
Start-Sleep -Seconds 5
Get-NetTCPConnection -LocalPort 3389 -State Listen -ErrorAction SilentlyContinue | Out-File -Append (Join-Path $Out 'info.txt')

# Test data: DC01 -> the runner (127.0.0.2:3389), Linux-Box -> SSH to 127.0.0.1:22 (refused).
$data = Join-Path $env:RUNNER_TEMP 'rc-gui-data'
Remove-Item -Recurse -Force $data -ErrorAction SilentlyContinue
python -c "import shutil,sqlite3; shutil.copy('tests/fixtures/gui-test.db3','gui-test-win.db3'); db=sqlite3.connect('gui-test-win.db3'); db.execute(""UPDATE Connections SET Target='127.0.0.2:3389' WHERE Name='DC01'""); db.commit()"
& $exe --data-dir $data --import gui-test-win.db3 | Out-File -Append (Join-Path $Out 'info.txt')
$env:RC_MANAGER_NO_UPDATE = '1'

# 1: RDP – the certificate dialog of RC Manager (asked on wfreerdp's console), then the tab.
$p = Start-App @('--data-dir', $data, '--connect', 'DC01') 'app-rdp'
Start-Sleep -Seconds 8
# DC01 has no credentials: RC Manager's logon dialog (centred; positions for 1150x760 at 0,0).
Shot '1a-rdp-logon-dialog'
[U]::Click(600, 362); Start-Sleep -Milliseconds 300
[System.Windows.Forms.SendKeys]::SendWait('rc-gui-test'); Start-Sleep -Milliseconds 300
[U]::Click(432, 453)
Start-Sleep -Seconds 10
Shot '1-rdp-certificate-dialog'
Get-Process | Where-Object { $_.ProcessName -match 'wfreerdp|putty|vncviewer|rc-manager' } | Select-Object Id, ProcessName, MainWindowTitle |
    Out-File -Append (Join-Path $Out 'info.txt')
Start-Sleep -Seconds 3
Stop-Process -Id $p.Id -Force -ErrorAction SilentlyContinue
Get-Process wfreerdp -ErrorAction SilentlyContinue | Stop-Process -Force
Start-Sleep -Seconds 2

# 2: SSH via PuTTY – its window is moved into the tab (connection refused: PuTTY shows the error).
$p = Start-App @('--data-dir', $data, '--connect', 'Linux-Box') 'app-ssh'
Start-Sleep -Seconds 10
Shot '2-ssh-putty-tab'
Get-Process | Where-Object { $_.ProcessName -match 'putty|rc-manager' } | Select-Object Id, ProcessName, MainWindowTitle |
    Out-File -Append (Join-Path $Out 'info.txt')
Stop-Process -Id $p.Id -Force -ErrorAction SilentlyContinue
Get-Process putty -ErrorAction SilentlyContinue | Stop-Process -Force
exit 0
