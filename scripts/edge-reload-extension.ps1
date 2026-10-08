# Reloads an unpacked extension in the running Edge, like its Reload button on
# edge://extensions, through Windows UI Automation. No DevTools port is needed and no
# keystrokes are sent, so nothing can land in another window.
#
# Needs, once: a tab open on edge://extensions (Edge does not open edge:// addresses from
# the command line). Switching an extension off and on does not reread its files; this presses
# the card's Reload button and checks the card shows the version in -Folder's manifest.
#
#   powershell -NoProfile -STA -File edge-reload-extension.ps1 [-Name "AscendCord Stereo Proof"] [-Folder <unpacked folder>]
param(
	[string]$Name = "AscendCord Stereo Proof",
	[string]$Folder
)
$ErrorActionPreference = "Stop"
Add-Type -AssemblyName UIAutomationClient, UIAutomationTypes
$A = [System.Windows.Automation.AutomationElement]
$T = [System.Windows.Automation.TreeScope]
$All = [System.Windows.Automation.Condition]::TrueCondition
$Type = { param($kind) New-Object System.Windows.Automation.PropertyCondition($A::ControlTypeProperty, $kind) }

$windows = $A::RootElement.FindAll($T::Children,
	(New-Object System.Windows.Automation.PropertyCondition($A::ClassNameProperty, "Chrome_WidgetWin_1")))
$window = $null; $extensionsTab = $null; $previous = $null
foreach ($candidate in $windows) {
	foreach ($tab in $candidate.FindAll($T::Descendants, (& $Type ([System.Windows.Automation.ControlType]::TabItem)))) {
		if ($tab.Current.Name -match "^Extensions") { $window = $candidate; $extensionsTab = $tab }
	}
	if ($window) {
		foreach ($tab in $window.FindAll($T::Descendants, (& $Type ([System.Windows.Automation.ControlType]::TabItem)))) {
			try { if ($tab.GetCurrentPattern([System.Windows.Automation.SelectionItemPattern]::Pattern).Current.IsSelected) { $previous = $tab } } catch {}
		}
		break
	}
}
if (-not $extensionsTab) { throw "Open edge://extensions in a tab once; this script reloads through that page." }
$extensionsTab.GetCurrentPattern([System.Windows.Automation.SelectionItemPattern]::Pattern).Select()

function Find-Card {
	foreach ($group in $window.FindAll($T::Descendants, (& $Type ([System.Windows.Automation.ControlType]::Group)))) {
		if ($group.Current.Name -like "$Name Version*") { return $group }
	}
	return $null
}
$card = $null
for ($i = 0; $i -lt 40 -and -not $card; $i++) { Start-Sleep -Milliseconds 250; $card = Find-Card }
if (-not $card) { throw "No '$Name' card on edge://extensions (is Developer mode on, or a search hiding it?)." }
$before = $card.Current.Name
$reload = $card.FindAll($T::Descendants, $All) | Where-Object { $_.Current.Name -eq "Reload" } | Select-Object -First 1
if (-not $reload) { throw "The card has no Reload button (Developer mode off?)." }
$reload.GetCurrentPattern([System.Windows.Automation.InvokePattern]::Pattern).Invoke()

$expected = $null
if ($Folder) { $expected = (Get-Content (Join-Path $Folder "manifest.json") -Raw | ConvertFrom-Json).version }
$after = $before
for ($i = 0; $i -lt 40; $i++) {
	Start-Sleep -Milliseconds 250
	$card = Find-Card
	if ($card) { $after = $card.Current.Name }
	if (-not $expected -or $after -like "*Version $expected *") { break }
}
# A switched-off extension is switched back on.
if ($after -like "*Extension off*") {
	$switch = $card.FindAll($T::Descendants, $All) | Where-Object { $_.Current.Name -like "Turn on*" } | Select-Object -First 1
	$switch.GetCurrentPattern([System.Windows.Automation.TogglePattern]::Pattern).Toggle()
	Start-Sleep -Milliseconds 800
	$after = (Find-Card).Current.Name
}
if ($previous -and $previous.Current.Name -notmatch "^Extensions") {
	$previous.GetCurrentPattern([System.Windows.Automation.SelectionItemPattern]::Pattern).Select()
}
"before: $before"
"after:  $after"
if ($after -notlike "*Extension on*" -or ($expected -and $after -notlike "*Version $expected *")) { exit 1 }
