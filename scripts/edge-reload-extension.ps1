# Reloads an unpacked extension in the running Edge through its edge://extensions page,
# using Windows UI Automation (no DevTools port needed). Usage:
#   powershell -NoProfile -STA -File edge-reload-extension.ps1 [-Id <extension id>] [-Name <card name>]
param(
	[string]$Id = "jbchdifpgimmmmlnimidbfockpbigfni",
	[string]$Name = "AscendCord Stereo Proof"
)
$ErrorActionPreference = "Stop"
Add-Type -AssemblyName UIAutomationClient, UIAutomationTypes, System.Windows.Forms
Add-Type @"
using System; using System.Runtime.InteropServices;
public static class Win {
	[DllImport("user32.dll")] public static extern bool SetForegroundWindow(IntPtr h);
	[DllImport("user32.dll")] public static extern bool ShowWindow(IntPtr h, int cmd);
	[DllImport("user32.dll")] public static extern IntPtr GetForegroundWindow();
}
"@
$A = [System.Windows.Automation.AutomationElement]
$T = [System.Windows.Automation.TreeScope]
$C = [System.Windows.Automation.ControlType]

$edge = Get-Process msedge -ErrorAction SilentlyContinue | Where-Object { $_.MainWindowHandle -ne 0 } | Select-Object -First 1
if (-not $edge) { throw "Edge has no open window." }
$hwnd = $edge.MainWindowHandle
$previous = [Win]::GetForegroundWindow()
[Win]::ShowWindow($hwnd, 9) | Out-Null
[Win]::SetForegroundWindow($hwnd) | Out-Null
Start-Sleep -Milliseconds 400
[System.Windows.Forms.SendKeys]::SendWait("^t")
Start-Sleep -Milliseconds 500
[System.Windows.Forms.SendKeys]::SendWait("edge://extensions/?id=$Id{ENTER}")

$window = $A::FromHandle($hwnd)
# The details page's reload icon has no accessible name; turning the extension off and on
# unregisters its service worker and registers it again from disk, which is the same reload.
function Find-Switch {
	foreach ($element in $window.FindAll($T::Descendants, [System.Windows.Automation.Condition]::TrueCondition)) {
		if ($element.Current.Name -match "^Extension (on|off)$") { return $element }
	}
	return $null
}
$switch = $null
for ($i = 0; $i -lt 40 -and -not $switch; $i++) { Start-Sleep -Milliseconds 250; $switch = Find-Switch }
$result = "extension switch not found"
if ($switch) {
	$state = { (Find-Switch).Current.Name }
	$press = {
		$target = Find-Switch
		try { $target.GetCurrentPattern([System.Windows.Automation.TogglePattern]::Pattern).Toggle() }
		catch { $target.GetCurrentPattern([System.Windows.Automation.InvokePattern]::Pattern).Invoke() }
	}
	$before = & $state
	if ($before -eq "Extension on") { & $press; Start-Sleep -Milliseconds 1200 }
	$middle = & $state
	& $press
	Start-Sleep -Milliseconds 1500
	$result = "switch: $before -> $middle -> " + (& $state)
}
[System.Windows.Forms.SendKeys]::SendWait("^w")
if ($previous -ne [IntPtr]::Zero) { [Win]::SetForegroundWindow($previous) | Out-Null }
$result
