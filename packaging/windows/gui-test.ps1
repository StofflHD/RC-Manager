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
  [DllImport("user32.dll")] static extern IntPtr FindWindowEx(IntPtr p, IntPtr after, string cls, string title);
  [DllImport("user32.dll")] static extern IntPtr GetParent(IntPtr h);
  [DllImport("user32.dll")] static extern IntPtr GetAncestor(IntPtr h, uint f);
  [DllImport("user32.dll")] static extern IntPtr GetWindowLongPtr(IntPtr h, int i);
  [DllImport("user32.dll")] static extern bool EnumChildWindows(IntPtr p, EnumProc f, IntPtr l);
  [DllImport("user32.dll")] static extern bool EnumWindows(EnumProc f, IntPtr l);
  [DllImport("user32.dll", CharSet = CharSet.Unicode)] static extern int GetClassName(IntPtr h, System.Text.StringBuilder s, int n);
  delegate bool EnumProc(IntPtr h, IntPtr l);
  static string Cls(IntPtr h) { var s = new System.Text.StringBuilder(256); GetClassName(h, s, 256); return s.ToString(); }
  // Style and parent of every window of a class (top-level and children), for the test log.
  public static string Describe(string cls) {
    var o = new System.Text.StringBuilder();
    EnumProc each = (h, l) => {
      if (Cls(h) == cls) o.AppendLine(String.Format("{0} {1:X}: parent {2:X} root {3:X} style {4:X8} ex {5:X8}", cls, (long)h,
          (long)GetParent(h), (long)GetAncestor(h, 2), (long)GetWindowLongPtr(h, -16), (long)GetWindowLongPtr(h, -20)));
      EnumChildWindows(h, (c, l2) => { if (Cls(c) == cls) o.AppendLine(String.Format("{0} {1:X} (child): parent {2:X} style {3:X8} ex {4:X8}",
          cls, (long)c, (long)GetParent(c), (long)GetWindowLongPtr(c, -16), (long)GetWindowLongPtr(c, -20))); return true; }, IntPtr.Zero);
      return true;
    };
    EnumWindows(each, IntPtr.Zero);
    return o.ToString();
  }
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
"PuTTY windows:" | Out-File -Append (Join-Path $Out 'info.txt')
[U]::Describe('PuTTY') | Out-File -Append (Join-Path $Out 'info.txt')
Get-Process | Where-Object { $_.ProcessName -match 'putty|rc-manager' } | Select-Object Id, ProcessName, MainWindowTitle |
    Out-File -Append (Join-Path $Out 'info.txt')
# Closing RC Manager normally ends the sessions in its tabs (PuTTY must not keep running).
$p.CloseMainWindow() | Out-Null
$p.WaitForExit(15000) | Out-Null
Start-Sleep -Seconds 2
$left = @(Get-Process putty -ErrorAction SilentlyContinue)
"After closing RC Manager: exited=$($p.HasExited), PuTTY processes left: $($left.Count)" | Out-File -Append (Join-Path $Out 'info.txt')
Stop-Process -Id $p.Id -Force -ErrorAction SilentlyContinue
$left | Stop-Process -Force -ErrorAction SilentlyContinue
exit 0
