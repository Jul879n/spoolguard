# Registra el host nativo (usuario actual) para Chrome, Edge y Firefox.
# Uso: .\install-native-host.ps1 -HostPath C:\ruta\native_host.exe -ExtId ID_EXTENSION_CHROMIUM
param([Parameter(Mandatory)][string]$HostPath, [Parameter(Mandatory)][string]$ExtId)
$ErrorActionPreference = 'Stop'
if (-not (Test-Path $HostPath)) { throw "No existe: $HostPath" }
$HostPath = (Resolve-Path $HostPath).Path
$name = 'cl.cohasa.controlimpresion'
$aqui = Split-Path -Parent $MyInvocation.MyCommand.Path
$dest = Join-Path $env:APPDATA "$name\native-host"
New-Item -ItemType Directory -Force $dest | Out-Null

function Generar($plantilla, $salida) {
  (Get-Content -Raw $plantilla).Replace('/RUTA/ABSOLUTA/native_host', $HostPath.Replace('\', '\\')).Replace('ID_DE_LA_EXTENSION', $ExtId) |
    Set-Content -Encoding ascii $salida
}
$chrome = Join-Path $dest "$name.chrome.json"
$ff = Join-Path $dest "$name.firefox.json"
Generar (Join-Path $aqui "$name.chrome.json") $chrome
Generar (Join-Path $aqui "$name.firefox.json") $ff

# La clave de registro apunta al manifiesto (valor predeterminado).
foreach ($k in @("HKCU:\Software\Google\Chrome\NativeMessagingHosts\$name",
                 "HKCU:\Software\Microsoft\Edge\NativeMessagingHosts\$name")) {
  New-Item -Force $k | Out-Null; Set-ItemProperty $k -Name '(default)' -Value $chrome
}
$k = "HKCU:\Software\Mozilla\NativeMessagingHosts\$name"
New-Item -Force $k | Out-Null; Set-ItemProperty $k -Name '(default)' -Value $ff
Write-Host "Host registrado para Chrome, Edge y Firefox."
