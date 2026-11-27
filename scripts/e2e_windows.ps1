# Prueba extremo a extremo de la app completa en Windows (ver .github/workflows/e2e-windows.yml).
# Instala el MSI en silencio, lanza la app instalada con --minimized y CONTROL_IMPRESION_DIR,
# envia trabajos RAW al spooler y comprueba el registro (events.db); luego mata la app y verifica el guardian.
param([Parameter(Mandatory)][string]$Msi, [Parameter(Mandatory)][string]$Out)
$ErrorActionPreference = 'Continue'
New-Item -ItemType Directory -Force $Out | Out-Null
$Data = Join-Path $Out 'data'
New-Item -ItemType Directory -Force $Data | Out-Null
$script:fails = @()
function Check($ok, $msg) {
  if ($ok) { Write-Host "[OK]   $msg" } else { Write-Host "[FAIL] $msg"; $script:fails += $msg }
}
function Shot($name) {
  try {
    Add-Type -AssemblyName System.Windows.Forms, System.Drawing
    $b = [System.Windows.Forms.SystemInformation]::VirtualScreen
    $bmp = New-Object System.Drawing.Bitmap $b.Width, $b.Height
    $g = [System.Drawing.Graphics]::FromImage($bmp)
    $g.CopyFromScreen($b.Left, $b.Top, 0, 0, $bmp.Size)
    $bmp.Save((Join-Path $Out "$name.png")); $g.Dispose(); $bmp.Dispose()
  } catch { Write-Host "captura $name fallo: $_" }
}
function Dump($title) {
  Write-Host "--- $title"
  Get-Printer | Where-Object Name -like 'CtrlE2E*' | Format-Table Name, PrinterStatus, JobCount -AutoSize | Out-String | Write-Host
  Get-Printer | Format-Table Name, PrinterStatus, JobCount -AutoSize | Out-String | Write-Host
}

# ---- 1. MSI ----
Write-Host "== 1. Instalar MSI"
$p = Start-Process msiexec -ArgumentList "/i `"$Msi`" /qn /l*v `"$Out\msi-install.log`"" -Wait -PassThru
Check ($p.ExitCode -eq 0) "msiexec /i termino con codigo $($p.ExitCode)"
$exe = Get-ChildItem 'C:\Program Files','C:\Program Files (x86)',"$env:LOCALAPPDATA\Programs" -Recurse -Filter 'control-impresion.exe' -ErrorAction SilentlyContinue | Select-Object -First 1
Check ($null -ne $exe) "control-impresion.exe instalado en: $($exe.FullName)"
if (-not $exe) { Write-Host 'Sin binario instalado; abortando'; exit 1 }
$inst = $exe.DirectoryName
Get-ChildItem $inst -Recurse | Select-Object -First 40 FullName | Out-String | Write-Host
Check (Test-Path (Join-Path $inst 'extension')) "recurso extension/ instalado junto al binario"
$lnk = Get-ChildItem 'C:\ProgramData\Microsoft\Windows\Start Menu',"$env:APPDATA\Microsoft\Windows\Start Menu" -Recurse -Filter '*.lnk' -ErrorAction SilentlyContinue | Where-Object Name -like '*SpoolGuard*'
Write-Host "Accesos del menu Inicio: $($lnk.FullName -join ', ')"
Check ($lnk.Count -ge 1) "acceso directo en menu Inicio"
$run = @(); foreach ($k in 'HKLM:\Software\Microsoft\Windows\CurrentVersion\Run','HKCU:\Software\Microsoft\Windows\CurrentVersion\Run') { $run += (Get-ItemProperty $k -ErrorAction SilentlyContinue).PSObject.Properties | Where-Object { $_.Value -like '*control-impresion*' } }
Write-Host "Entradas de inicio automatico tras instalar (esperado: ninguna hasta activarlo en Ajustes): $($run.Count)"
Get-ItemProperty 'HKLM:\Software\Microsoft\Windows\CurrentVersion\Uninstall\*','HKLM:\Software\WOW6432Node\Microsoft\Windows\CurrentVersion\Uninstall\*' -ErrorAction SilentlyContinue | Where-Object DisplayName -like '*SpoolGuard*' | Select-Object DisplayName, DisplayVersion, Publisher, InstallLocation | Format-List | Out-String | Write-Host

# ---- 2. Impresoras virtuales ----
Write-Host "== 2. Impresoras virtuales"
wevtutil sl Microsoft-Windows-PrintService/Operational /e:true
foreach ($n in 'CtrlE2E','CtrlE2E2') {
  Add-Printer -Name $n -DriverName 'Microsoft Print to PDF' -PortName 'PORTPROMPT:' -ErrorAction Stop
}
Check ((Get-Printer -Name CtrlE2E,CtrlE2E2 -ErrorAction SilentlyContinue).Count -eq 2) "CtrlE2E y CtrlE2E2 creadas"

# ---- P/Invoke winspool ----
Add-Type -TypeDefinition @'
using System; using System.Runtime.InteropServices;
public static class Spool {
  [StructLayout(LayoutKind.Sequential, CharSet=CharSet.Unicode)]
  public struct DOC_INFO_1 { public string pDocName; public string pOutputFile; public string pDatatype; }
  [DllImport("winspool.drv", CharSet=CharSet.Unicode, SetLastError=true)] public static extern bool OpenPrinter(string n, out IntPtr h, IntPtr d);
  [DllImport("winspool.drv", CharSet=CharSet.Unicode, SetLastError=true)] public static extern int StartDocPrinter(IntPtr h, int lvl, ref DOC_INFO_1 di);
  [DllImport("winspool.drv", SetLastError=true)] public static extern bool StartPagePrinter(IntPtr h);
  [DllImport("winspool.drv", SetLastError=true)] public static extern bool WritePrinter(IntPtr h, byte[] b, int n, out int w);
  [DllImport("winspool.drv", SetLastError=true)] public static extern bool EndPagePrinter(IntPtr h);
  [DllImport("winspool.drv", SetLastError=true)] public static extern bool EndDocPrinter(IntPtr h);
  [DllImport("winspool.drv", SetLastError=true)] public static extern bool ClosePrinter(IntPtr h);
  [StructLayout(LayoutKind.Sequential, CharSet=CharSet.Unicode)] public struct PRINTER_DEFAULTS { public string pDatatype; public IntPtr pDevMode; public int DesiredAccess; }
  [DllImport("winspool.drv", CharSet=CharSet.Unicode, SetLastError=true)] public static extern bool OpenPrinter(string n, out IntPtr h, ref PRINTER_DEFAULTS d);
  [DllImport("winspool.drv", SetLastError=true)] public static extern bool SetPrinter(IntPtr h, int lvl, IntPtr p, int cmd);
  public static string Pause(string printer, int cmd) {
    IntPtr h; PRINTER_DEFAULTS d = new PRINTER_DEFAULTS(); d.DesiredAccess = 12;
    if (!OpenPrinter(printer, out h, ref d)) return "OpenPrinter err " + Marshal.GetLastWin32Error();
    bool ok = SetPrinter(h, 0, IntPtr.Zero, cmd); int e = Marshal.GetLastWin32Error(); ClosePrinter(h); return ok ? "ok" : "SetPrinter err " + e;
  }
  public static string Submit(string printer, string doc) {
    IntPtr h; if (!OpenPrinter(printer, out h, IntPtr.Zero)) return "OpenPrinter err " + Marshal.GetLastWin32Error();
    DOC_INFO_1 di = new DOC_INFO_1(); di.pDocName = doc; di.pOutputFile = null; di.pDatatype = "RAW";
    int id = StartDocPrinter(h, 1, ref di); if (id == 0) return "StartDoc err " + Marshal.GetLastWin32Error();
    StartPagePrinter(h); byte[] d = System.Text.Encoding.ASCII.GetBytes("hola\r\n"); int w; WritePrinter(h, d, d.Length, out w);
    EndPagePrinter(h); EndDocPrinter(h); ClosePrinter(h); return "ok " + id;
  }
}
'@

# ---- 2b. Control sin la app: el spooler retiene con la cola pausada? ----
Write-Host "== 2b. Control (sin app): cola pausada con CIM y envio con el mismo P/Invoke"
Add-Printer -Name CtrlCtl -DriverName 'Microsoft Print to PDF' -PortName 'PORTPROMPT:' -ErrorAction Stop
$cp = Get-CimInstance Win32_Printer -Filter "Name='CtrlCtl'"; $pr = Invoke-CimMethod -InputObject $cp -MethodName Pause; Write-Host "Pause rc=$($pr.ReturnValue)"
Start-Sleep 5
$r = [Spool]::Submit('CtrlCtl','control.txt'); Write-Host "control -> $r"
for ($i = 0; $i -lt 8; $i++) { Write-Host ("ctl t+{0}s printer={1} jobs={2}" -f $i, (Get-Printer -Name CtrlCtl).PrinterStatus, ((@(Get-PrintJob -PrinterName CtrlCtl -ErrorAction SilentlyContinue) | ForEach-Object { "$($_.DocumentName)[$($_.JobStatus)]" }) -join ';')); Start-Sleep 1 }
Remove-Printer -Name CtrlCtl -ErrorAction SilentlyContinue
# Variante B: pausa con SetPrinter (igual que la app y el test de Rust) y envio con Out-Printer
Add-Printer -Name CtrlCtl -DriverName 'Microsoft Print to PDF' -PortName 'PORTPROMPT:' -ErrorAction Stop
Write-Host "B pause: $([Spool]::Pause('CtrlCtl', 1))"
Start-Sleep 6
$rb = [Spool]::Submit('CtrlCtl','controlB.txt'); Write-Host "B submit -> $rb"
for ($i = 0; $i -lt 5; $i++) { Write-Host ("ctlB t+{0}s printer={1} jobs={2}" -f $i, (Get-Printer -Name CtrlCtl).PrinterStatus, ((@(Get-PrintJob -PrinterName CtrlCtl -ErrorAction SilentlyContinue) | ForEach-Object { "$($_.DocumentName)[$($_.JobStatus)]" }) -join ';')); Start-Sleep -Milliseconds 400 }
Remove-Printer -Name CtrlCtl -ErrorAction SilentlyContinue
# Variante C: pausa con SetPrinter y envio RAW inmediato en el mismo proceso, sondeo fino
Add-Printer -Name CtrlCtl -DriverName 'Microsoft Print to PDF' -PortName 'PORTPROMPT:' -ErrorAction Stop
Write-Host "C pause: $([Spool]::Pause('CtrlCtl', 1))"
$r = [Spool]::Submit('CtrlCtl','controlC.txt'); Write-Host "C submit -> $r"
for ($i = 0; $i -lt 10; $i++) { Write-Host ("ctlC t+{0}ms printer={1} jobs={2}" -f ($i*50), (Get-Printer -Name CtrlCtl).PrinterStatus, ((@(Get-PrintJob -PrinterName CtrlCtl -ErrorAction SilentlyContinue) | ForEach-Object { "$($_.DocumentName)[$($_.JobStatus)]" }) -join ';')); Start-Sleep -Milliseconds 50 }
Get-PrintJob -PrinterName CtrlCtl -ErrorAction SilentlyContinue | Remove-PrintJob -ErrorAction SilentlyContinue; Remove-Printer -Name CtrlCtl -ErrorAction SilentlyContinue

# ---- 3. Lanzar la app ----
Write-Host "== 3. Lanzar app"
$cfg = '{"printers":{"CtrlE2E":{"enabled":true,"mode":"block","apps":["*.docx"],"sites":[],"ips":[]}},"global":{"enabled":true,"mode":"block","apps":["*.xlsx"],"sites":[],"ips":[]},"settings":{}}'
[IO.File]::WriteAllText((Join-Path $Data 'config.json'), $cfg)
$env:CONTROL_IMPRESION_DIR = $Data
$app = Start-Process $exe.FullName -ArgumentList '--minimized' -PassThru -RedirectStandardOutput (Join-Path $Out 'app-stdout.txt') -RedirectStandardError (Join-Path $Out 'app-stderr.txt')
# El guardián se lanza al final del arranque (tras reaplicar la retención): medido ~9,5 s en la VM, más en un runner cargado.
# Se espera hasta 60 s en vez de un plazo fijo, y se informa la latencia real.
$t0 = Get-Date
while (((Get-Date) - $t0).TotalSeconds -lt 60) {
  if ((Test-Path (Join-Path $Data 'app.pid')) -and (Get-CimInstance Win32_Process -Filter "Name='control-impresion.exe'" | Where-Object CommandLine -like '*--guardian*')) { break }
  Start-Sleep -Milliseconds 500
}
Write-Host ("Arranque completo (app.pid + guardian) tras {0:N1} s" -f ((Get-Date) - $t0).TotalSeconds)
Shot 'desktop-1-arranque'
$alive = -not (Get-Process -Id $app.Id -ErrorAction SilentlyContinue).HasExited -and (Get-Process -Id $app.Id -ErrorAction SilentlyContinue)
Check ([bool]$alive) "la app sigue viva tras 12 s (pid $($app.Id))"
if (-not $alive) { Write-Host "ExitCode: $($app.ExitCode)"; Get-Content (Join-Path $Out 'app-stderr.txt') -ErrorAction SilentlyContinue | Write-Host
  Get-WinEvent -FilterHashtable @{LogName='Application';Level=2;StartTime=(Get-Date).AddMinutes(-5)} -MaxEvents 5 -ErrorAction SilentlyContinue | Format-List TimeCreated,Message | Out-String | Write-Host
  exit 2 }
$guard = Get-CimInstance Win32_Process -Filter "Name='control-impresion.exe'" | Where-Object CommandLine -like '*--guardian*'
Check ($null -ne $guard) "proceso guardian (--guardian) en marcha: $($guard.CommandLine)"
Check (Test-Path (Join-Path $Data 'app.pid')) "app.pid escrito"
$wins = Get-Process control-impresion -ErrorAction SilentlyContinue | Where-Object { $_.MainWindowHandle -ne 0 }
Write-Host "Ventanas principales visibles con --minimized: $($wins.Count) (esperado 0)"
Get-Process msedgewebview2 -ErrorAction SilentlyContinue | Measure-Object | ForEach-Object { Write-Host "procesos WebView2: $($_.Count)" }
Dump 'tras arrancar la app'
$st = (Get-Printer -Name CtrlE2E).PrinterStatus
Check ("$st" -eq 'Paused') "CtrlE2E retenida (Paused) mientras la app corre (estado: $st)"

# ---- 4. Trabajos ----
Write-Host "== 4. Enviar trabajos"
# Diagnostico fino: un trabajo de prueba con sondeo cada 100 ms (solo imprime cambios de estado)
Write-Host "-- sonda: envio de sonda.docx a CtrlE2E"
$t0 = Get-Date; $prev = ''
$r = [Spool]::Submit('CtrlE2E','sonda.docx'); Write-Host "sonda -> $r"
while (((Get-Date) - $t0).TotalSeconds -lt 10) {
  $ps = (Get-Printer -Name CtrlE2E).PrinterStatus
  $js = (@(Get-PrintJob -PrinterName CtrlE2E -ErrorAction SilentlyContinue) | ForEach-Object { "$($_.DocumentName)[$($_.JobStatus)]" }) -join ';'
  $cur = "printer=$ps jobs=$js log=$(if (Test-Path "$Data\events.db") { (Get-Item "$Data\events.db").Length } else { -1 })B"
  if ($cur -ne $prev) { Write-Host ("{0:N1}s {1}" -f ((Get-Date) - $t0).TotalSeconds, $cur); $prev = $cur }
  Start-Sleep -Milliseconds 100
}
foreach ($pr in 'CtrlE2E','CtrlE2E2') { foreach ($d in 'informe.docx','planilla.xlsx','notas.txt') {
  $r = [Spool]::Submit($pr, $d); Write-Host "$pr $d -> $r"; Check ($r -like 'ok*') "envio $d a $pr ($r)"
} }
for ($i = 0; $i -lt 16; $i++) {
  $js = @(Get-PrintJob -PrinterName CtrlE2E -ErrorAction SilentlyContinue) + @(Get-PrintJob -PrinterName CtrlE2E2 -ErrorAction SilentlyContinue)
  $sz = if (Test-Path "$Data\events.db") { (Get-Item "$Data\events.db").Length } else { -1 }
  Write-Host ("t+{0}s app viva={1} log={2}B jobs: {3}" -f $i, [bool](Get-Process -Id $app.Id -ErrorAction SilentlyContinue), $sz, (($js | ForEach-Object { "$($_.PrinterName)/$($_.DocumentName)[$($_.JobStatus)]" }) -join '; '))
  Start-Sleep 1
}
Write-Host '--- PrintService/Operational'
Get-WinEvent -LogName 'Microsoft-Windows-PrintService/Operational' -MaxEvents 60 -ErrorAction SilentlyContinue | Sort-Object TimeCreated | ForEach-Object { "{0:HH:mm:ss.fff} {1} {2}" -f $_.TimeCreated, $_.Id, ($_.Message -replace '\s+',' ') } | Write-Host
Shot 'desktop-2-trabajos'
# El registro vive en events.db (SQLite): se lee con el Python del runner (sqlite3 de la biblioteca estandar).
$pyf = Join-Path $env:TEMP 'leer_eventos.py'
Set-Content -Path $pyf -Encoding ascii -Value @'
import sqlite3, json, sys
c = sqlite3.connect(sys.argv[1]); c.row_factory = sqlite3.Row
print(json.dumps([dict(r) for r in c.execute("select printer, document, decision, reason from events order by ts")]))
'@
$log = @()
$db = Join-Path $Data 'events.db'
if (Test-Path $db) {
  $json = python $pyf $db
  $log = @($json | ConvertFrom-Json | ForEach-Object { $_ | Add-Member -NotePropertyName allowed -NotePropertyValue ($_.decision -eq 'allow') -PassThru })
}
Write-Host "eventos:"; $log | Format-Table printer, document, decision, reason | Out-String | Write-Host
function Entry($pr, $doc) { $log | Where-Object { $_.printer -eq $pr -and $_.document -eq $doc } | Select-Object -First 1 }
$e = Entry 'CtrlE2E' 'informe.docx';  Check ($e -and -not $e.allowed) "informe.docx bloqueado en CtrlE2E ($($e.reason))"
$e = Entry 'CtrlE2E' 'planilla.xlsx'; Check ($e -and -not $e.allowed -and $e.reason -like 'Global*') "planilla.xlsx bloqueado por Global en CtrlE2E ($($e.reason))"
$e = Entry 'CtrlE2E' 'notas.txt';     Check ($e -and $e.allowed) "notas.txt permitido en CtrlE2E ($($e.reason))"
$e = Entry 'CtrlE2E2' 'planilla.xlsx'; Check ($e -and -not $e.allowed -and $e.reason -like 'Global*') "planilla.xlsx bloqueado por Global en CtrlE2E2 ($($e.reason))"
$e = Entry 'CtrlE2E2' 'informe.docx'; Check ($e -and $e.allowed) "informe.docx permitido en CtrlE2E2 ($($e.reason))"
$e = Entry 'CtrlE2E2' 'notas.txt';    Check ($e -and $e.allowed) "notas.txt permitido en CtrlE2E2 ($($e.reason))"
Dump 'tras evaluar'
foreach ($pr in 'CtrlE2E','CtrlE2E2') {
  $jobs = @(Get-PrintJob -PrinterName $pr -ErrorAction SilentlyContinue)
  Write-Host "$pr trabajos en cola: $(($jobs | ForEach-Object { "$($_.DocumentName)[$($_.JobStatus)]" }) -join ', ')"
  Check ($jobs.Count -eq 0) "ningun trabajo retenido en $pr (quedan $($jobs.Count))"
}

# ---- 5. Cierre forzoso y guardian ----
Write-Host "== 5. Cierre forzoso"
Stop-Process -Id $app.Id -Force
Start-Sleep 1
# Trabajo nuevo con la app muerta: debe quedar en cola y ser liberado por el guardian
$r = [Spool]::Submit('CtrlE2E','tras-kill.txt'); Write-Host "envio tras kill: $r"
Start-Sleep 15
Shot 'desktop-3-tras-kill'
Dump 'tras cierre forzoso'
foreach ($pr in 'CtrlE2E','CtrlE2E2') {
  $s = (Get-Printer -Name $pr).PrinterStatus
  Check ("$s" -ne 'Paused') "$pr no esta pausada tras el cierre forzoso (estado: $s)"
}
$left = @(Get-PrintJob -PrinterName CtrlE2E -ErrorAction SilentlyContinue)
Write-Host "CtrlE2E trabajos tras kill: $(($left | ForEach-Object { "$($_.DocumentName)[$($_.JobStatus)]" }) -join ', ')"
$allp = @(Get-Printer | Where-Object { "$($_.PrinterStatus)" -eq 'Paused' })
Check ($allp.Count -eq 0) "ninguna impresora del sistema pausada ($(($allp.Name) -join ', '))"
$guard2 = Get-CimInstance Win32_Process -Filter "Name='control-impresion.exe'" | Where-Object CommandLine -like '*--guardian*'
Check ($null -eq $guard2) "el guardian termino tras liberar las colas"
Write-Host "managed.json: $(Get-Content (Join-Path $Data 'managed.json') -ErrorAction SilentlyContinue)"

# ---- 6. Limpieza ----
Get-Process control-impresion -ErrorAction SilentlyContinue | Stop-Process -Force
foreach ($n in 'CtrlE2E','CtrlE2E2') { Get-PrintJob -PrinterName $n -ErrorAction SilentlyContinue | Remove-PrintJob -ErrorAction SilentlyContinue; Remove-Printer -Name $n -ErrorAction SilentlyContinue }
Start-Process msiexec -ArgumentList "/x `"$Msi`" /qn" -Wait
Write-Host "== Resumen"
if ($script:fails.Count) { $script:fails | ForEach-Object { Write-Host "FALLO: $_" }; exit 1 }
Write-Host 'Todo correcto'
