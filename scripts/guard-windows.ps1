# SpoolGuard - proteccion previa a la impresion en Windows (print processor "spoolguardpp").
# Uso (PowerShell como administrador):
#   guard-windows.ps1 -Action install   -Owner <SID del usuario de la app> -Dll <winguard.dll> [-Queues Cola1,Cola2]
#   guard-windows.ps1 -Action uninstall [-Queues ...]
#   guard-windows.ps1 -Action status    (imprime "protegidas total")
# Cada trabajo espera, dentro del spooler y antes de enviar datos a la impresora, el veredicto de la app
# (C:\ProgramData\SpoolGuard\verdicts). Sin veredicto en 10 s se imprime: la app cerrada no debe parar la impresion.
# Solo ASCII a proposito: PowerShell 5.1 lee mal un .ps1 en UTF-8 sin BOM.
param([string]$Action = 'status', [string]$Owner = '', [string]$Dll = '', [string[]]$Queues = @())
$ErrorActionPreference = 'Stop'
$ProgressPreference = 'SilentlyContinue'
$PP = 'spoolguardpp'
$Data = Join-Path $env:ProgramData 'SpoolGuard'
$Log = Join-Path $Data 'install.log'
$RegQ = 'HKLM:\SOFTWARE\SpoolGuard\Queues'
$RegPP = 'HKLM:\SYSTEM\CurrentControlSet\Control\Print\Environments\Windows x64\Print Processors\' + $PP

function Say($m) {
    Write-Output $m
    try { if (Test-Path $Data) { Add-Content -Path $Log -Value ('{0:s} {1}' -f (Get-Date), $m) } } catch {}
}

# Colas locales que se pueden proteger: usan winprint (o ya usan el nuestro), no son el fax y NO usan un driver V4 (medido: el
# spooler no llama al print processor de las colas con driver V4, ni con datos RAW ni XPS; protegerlas seria aparentar algo falso).
function Get-Targets {
    $v4 = @(Get-PrinterDriver | Where-Object { $_.MajorVersion -ge 4 } | ForEach-Object { $_.Name })
    $all = @(Get-Printer | Where-Object { $_.Type -eq 'Local' -and $_.PortName -ne 'SHRFAX:' -and ($v4 -notcontains $_.DriverName) -and ($_.PrintProcessor -eq 'winprint' -or $_.PrintProcessor -eq $PP) })
    if ($Queues.Count) { $all = @($all | Where-Object { $Queues -contains $_.Name }) }
    return $all
}

if ($Action -eq 'status') {
    $t = @(Get-Targets)
    '{0} {1}' -f @($t | Where-Object { $_.PrintProcessor -eq $PP }).Count, $t.Count
    exit 0
}

if (-not ([Security.Principal.WindowsPrincipal][Security.Principal.WindowsIdentity]::GetCurrent()).IsInRole([Security.Principal.WindowsBuiltInRole]::Administrator)) {
    Write-Error 'Ejecuta como administrador.'; exit 5
}

Add-Type -TypeDefinition @'
using System; using System.Text; using System.Runtime.InteropServices;
public static class SgSpool {
  [DllImport("winspool.drv", EntryPoint="GetPrintProcessorDirectoryW", CharSet=CharSet.Unicode, SetLastError=true)]
  static extern bool GetDir(string server, string env, int level, StringBuilder buf, int cb, out int needed);
  [DllImport("winspool.drv", EntryPoint="AddPrintProcessorW", CharSet=CharSet.Unicode, SetLastError=true)]
  static extern bool Add(string server, string env, string path, string name);
  [DllImport("winspool.drv", EntryPoint="DeletePrintProcessorW", CharSet=CharSet.Unicode, SetLastError=true)]
  static extern bool Del(string server, string env, string name);
  public static string Dir() { int n; StringBuilder sb = new StringBuilder(520); if (!GetDir(null, null, 1, sb, 1040, out n)) throw new System.ComponentModel.Win32Exception(Marshal.GetLastWin32Error()); return sb.ToString(); }
  public static int AddPP(string file, string name) { return Add(null, null, file, name) ? 0 : Marshal.GetLastWin32Error(); }
  public static int DelPP(string name) { return Del(null, null, name) ? 0 : Marshal.GetLastWin32Error(); }
}
'@

# Detiene el Spooler, hace $during (copiar o borrar la DLL cargada) y lo vuelve a iniciar SIEMPRE (aunque $during falle).
function Restart-Spooler($during) {
    try {
        Stop-Service Spooler -Force
        for ($i = 0; $i -lt 60 -and (Get-Process spoolsv -ErrorAction SilentlyContinue); $i++) { Start-Sleep -Milliseconds 250 }
        for ($i = 0; ; $i++) {
            try { & $during; break } catch { if ($i -ge 20) { throw }; Start-Sleep -Milliseconds 500 }
        }
    } finally { Start-Service Spooler }
}

# Carpeta de datos y de veredictos. Solo SYSTEM y Administradores escriben en la de datos; en la de veredictos escribe
# ademas el usuario que ejecuta la app (y nadie mas). Un usuario estandar no puede falsificar un "allow".
function Set-Folders {
    if ($Owner -notmatch '^S-1-[0-9-]+$') { throw 'Falta el SID del usuario de la app (-Owner).' }
    foreach ($d in @($Data, (Join-Path $Data 'verdicts'))) {
        if (Test-Path $d) {
            if ((Get-Item $d -Force).Attributes -band [IO.FileAttributes]::ReparsePoint) { throw "$d es un enlace: se rechaza." }
        } else { New-Item -ItemType Directory -Path $d | Out-Null }
        & icacls.exe $d /setowner 'BUILTIN\Administrators' | Out-Null
    }
    $sd = @{ $Data = "D:PAI(A;OICI;FA;;;SY)(A;OICI;FA;;;BA)(A;OICI;0x1200a9;;;$Owner)";
             (Join-Path $Data 'verdicts') = "D:PAI(A;OICI;FA;;;SY)(A;OICI;FA;;;BA)(A;OICI;0x1301bf;;;$Owner)" }
    foreach ($d in $sd.Keys) {
        $acl = New-Object Security.AccessControl.DirectorySecurity
        $acl.SetSecurityDescriptorSddlForm($sd[$d])
        Set-Acl -Path $d -AclObject $acl
    }
}

try {
    if ($Action -eq 'install') {
        if (-not $Dll -or -not (Test-Path $Dll)) { throw "No se encuentra la DLL del procesador ($Dll)." }
        Set-Folders
        $dst = Join-Path ([SgSpool]::Dir()) ($PP + '.dll')
        $new = (Get-FileHash $Dll -Algorithm SHA256).Hash
        $old = if (Test-Path $dst) { (Get-FileHash $dst -Algorithm SHA256).Hash } else { '' }
        if ($old -ne $new) {
            if ($old) {
                Say 'Actualizando la DLL: se reinicia el servicio Spooler (los trabajos en cola se conservan).'
                Restart-Spooler { Copy-Item -Path $Dll -Destination $dst -Force }
            } else { Copy-Item -Path $Dll -Destination $dst -Force }
        }
        if (-not (Test-Path $RegPP)) {
            $e = [SgSpool]::AddPP($PP + '.dll', $PP)
            if ($e) { throw "AddPrintProcessor fallo (error $e)." }
            Say 'Print processor registrado.'
        }
        if (-not (Test-Path $RegQ)) { New-Item -Path $RegQ -Force | Out-Null }
        foreach ($p in @(Get-Targets)) {
            if ($p.PrintProcessor -eq $PP) { Say "$($p.Name): ya protegida"; continue }
            try {
                New-ItemProperty -Path $RegQ -Name $p.Name -Value $p.PrintProcessor -PropertyType String -Force | Out-Null
                Set-Printer -Name $p.Name -PrintProcessor $PP
                Say "$($p.Name): protegida"
            } catch { Say "$($p.Name): no se pudo ($($_.Exception.Message))" }
        }
    }
    elseif ($Action -eq 'uninstall') {
        $mine = @(Get-Printer | Where-Object { $_.Type -eq 'Local' -and $_.PrintProcessor -eq $PP })
        if ($Queues.Count) { $mine = @($mine | Where-Object { $Queues -contains $_.Name }) }
        foreach ($p in $mine) {
            $orig = (Get-ItemProperty -Path $RegQ -Name $p.Name -ErrorAction SilentlyContinue).($p.Name)
            if (-not $orig) { $orig = 'winprint' }
            Set-Printer -Name $p.Name -PrintProcessor $orig
            Remove-ItemProperty -Path $RegQ -Name $p.Name -ErrorAction SilentlyContinue
            Say "$($p.Name): restaurada ($orig)"
        }
        # Solo se retira el procesador si ninguna cola lo usa (tambien las que no son locales ni winprint).
        $left = @(Get-Printer | Where-Object { $_.PrintProcessor -eq $PP })
        if ($left.Count) { Say "Quedan $($left.Count) colas con el procesador: no se retira."; }
        else {
            if (Test-Path $RegPP) { $e = [SgSpool]::DelPP($PP); if ($e) { throw "DeletePrintProcessor fallo (error $e)." }; Say 'Print processor retirado.' }
            $dst = Join-Path ([SgSpool]::Dir()) ($PP + '.dll')
            if (Test-Path $dst) { Restart-Spooler { Remove-Item -Path $dst -Force -ErrorAction SilentlyContinue } }
            Remove-Item -Path (Join-Path $Data 'verdicts') -Recurse -Force -ErrorAction SilentlyContinue
            if (Test-Path $RegQ) { Remove-Item -Path $RegQ -Force -ErrorAction SilentlyContinue }
        }
    }
    else { throw "Accion desconocida: $Action" }
    exit 0
} catch {
    Say ('ERROR: ' + $_.Exception.Message)
    exit 1
}
