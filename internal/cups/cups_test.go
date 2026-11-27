package cups

import (
	"os"
	"path/filepath"
	"testing"
)

func TestValidQueue(t *testing.T) {
	ok := []string{"Oficina", "HP_LaserJet-400", "Taller.2", "impresora_ñ"}
	bad := []string{"", "-p", "--help", "-o", "a b", "a\nb", "a\rb", "a\x00b", "a/b", "../x", "a#b", "a?b=1", "a\\b", "a\tb"}
	for _, n := range ok {
		if !ValidQueue(n) {
			t.Errorf("rechazó %q", n)
		}
	}
	for _, n := range bad {
		if ValidQueue(n) {
			t.Errorf("aceptó %q", n)
		}
	}
}

func TestMutadoresRechazanNombresMaliciosos(t *testing.T) {
	t.Setenv("PATH", t.TempDir()) // sin lpadmin/lpstat/ipptool: si se llamara a alguno, el error sería otro
	for _, n := range []string{"-x", "a b", "a\nb", "a/b"} {
		if err := SetHold(t.TempDir(), n, true); err != errQueue {
			t.Errorf("SetHold(%q) = %v", n, err)
		}
		if err := HoldNewJobs(t.TempDir(), n, true); err != errQueue {
			t.Errorf("HoldNewJobs(%q) = %v", n, err)
		}
		if _, err := HeldJobIDs(n); err != errQueue {
			t.Errorf("HeldJobIDs(%q) = %v", n, err)
		}
		if DeviceURI(t.TempDir(), n) != "" || Info(t.TempDir(), n, nil).Status != "desconocido" {
			t.Errorf("Info/DeviceURI aceptaron %q", n)
		}
	}
}

func TestMarcaDeColaControlada(t *testing.T) {
	dir := t.TempDir()
	t.Setenv("CONTROL_VERDICTS_DIR", dir)
	m := filepath.Join(dir, "controlled-Q1")
	MarkControlled("Q1", true)
	if _, err := os.Stat(m); err != nil {
		t.Fatalf("debía crear la marca: %v", err)
	}
	MarkControlled("Q1", false)
	if _, err := os.Stat(m); err == nil {
		t.Fatal("debía borrar la marca")
	}
	MarkControlled("../x", true) // nombre inválido: no escribe fuera de la carpeta
	if _, err := os.Stat(filepath.Join(filepath.Dir(dir), "controlled-../x")); err == nil {
		t.Fatal("no debe aceptar nombres inválidos")
	}
	t.Setenv("CONTROL_VERDICTS_DIR", filepath.Join(dir, "no-existe"))
	MarkControlled("Q1", true) // sin carpeta de veredictos no hace nada ni falla
}
