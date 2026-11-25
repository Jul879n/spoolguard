package app

import (
	"encoding/hex"
	"strings"
	"testing"
)

// Vector compartido con src-tauri/src/lib.rs (test pw_vector_compartido): deben dar el mismo hash.
const vectorSalt = "000102030405060708090a0b0c0d0e0f"
const vectorHash = "pbkdf2-sha256$1000$000102030405060708090a0b0c0d0e0f$0bf74a30e91f54e3f085113e2db57b84e1ecaa89d2065a9d77015dee173f4aef"

func TestKDFVectorCompartido(t *testing.T) {
	salt, _ := hex.DecodeString(vectorSalt)
	got := kdfHashWith("contraseña-ñandú", salt, 1000)
	if got != vectorHash {
		t.Fatalf("hash %s != %s", got, vectorHash)
	}
	if !kdfVerify("contraseña-ñandú", vectorHash) || kdfVerify("otra", vectorHash) {
		t.Fatal("verificación incorrecta")
	}
}

func TestKDFRechazaMalformados(t *testing.T) {
	for _, h := range []string{"", "pbkdf2-sha256$", "pbkdf2-sha256$0$00$00", "pbkdf2-sha256$99999999$00$" + strings.Repeat("0", 64),
		"pbkdf2-sha256$1000$zz$" + strings.Repeat("0", 64), "pbkdf2-sha256$1000$00$00", strings.Repeat("a", 64)} {
		if kdfVerify("x", h) {
			t.Fatalf("aceptó %q", h)
		}
	}
}

func TestMigracionHashAntiguo(t *testing.T) {
	s := newEnv(t).s
	salt := "abcd"
	s.cfg.Settings.PasswordSalt, s.cfg.Settings.PasswordHash = salt, hashPW(salt, "vieja-clave-1")
	if s.checkPassword("mala") {
		t.Fatal("aceptó clave incorrecta")
	}
	if isKDF(s.cfg.Settings.PasswordHash) {
		t.Fatal("migró con clave incorrecta")
	}
	if !s.checkPassword("vieja-clave-1") {
		t.Fatal("rechazó la clave antigua válida")
	}
	if !isKDF(s.cfg.Settings.PasswordHash) || s.cfg.Settings.PasswordSalt != "" {
		t.Fatal("no migró al formato nuevo")
	}
	if !s.checkPassword("vieja-clave-1") || s.checkPassword("mala") {
		t.Fatal("tras migrar debe seguir funcionando")
	}
}

func init() { pwIterations = 1000 } // las pruebas no necesitan el coste de producción

func TestCostePorDefectoEsElRecomendado(t *testing.T) {
	if pwIter < 600_000 {
		t.Fatalf("el coste de producción bajó de 600 000: %d", pwIter)
	}
	salt, _ := hex.DecodeString(vectorSalt)
	h := kdfHashWith("x", salt, pwIter)
	if !strings.Contains(h, "$600000$") || !kdfVerify("x", h) {
		t.Fatalf("hash de producción inválido: %s", h)
	}
}
