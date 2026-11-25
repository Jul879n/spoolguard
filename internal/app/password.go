package app

import (
	"crypto/pbkdf2"
	"crypto/sha256"
	"crypto/subtle"
	"encoding/hex"
	"strconv"
	"strings"
)

// Formato de contraseña (idéntico en el escritorio Rust, ver src-tauri/src/lib.rs):
//
//	pbkdf2-sha256$<iteraciones>$<sal en hex>$<clave derivada en hex>
//
// Los hashes antiguos (SHA-256 de sal+contraseña, 64 hex y sal aparte) se siguen aceptando y se
// reescriben al formato nuevo en el siguiente inicio de sesión correcto.
const (
	pwScheme    = "pbkdf2-sha256"
	pwIter      = 600_000   // recomendación OWASP 2023 para PBKDF2-HMAC-SHA256
	pwIterMax   = 5_000_000 // tope al verificar: una config manipulada no debe colgar el proceso
	pwKeyLen    = 32
	pwSaltBytes = 16
)

// kdfHash genera el hash en formato nuevo con una sal aleatoria.
func kdfHash(pw string) string {
	salt, _ := hex.DecodeString(randHex(pwSaltBytes))
	return kdfHashWith(pw, salt, pwIterations)
}

// pwIterations es el coste con que se generan hashes nuevos (verificar usa el del propio hash). Solo las
// pruebas lo bajan, para no pagar 600 000 iteraciones por cada inicio de sesión simulado.
var pwIterations = pwIter

func kdfHashWith(pw string, salt []byte, iter int) string {
	dk, err := pbkdf2.Key(sha256.New, pw, salt, iter, pwKeyLen)
	if err != nil {
		panic(err)
	}
	return pwScheme + "$" + strconv.Itoa(iter) + "$" + hex.EncodeToString(salt) + "$" + hex.EncodeToString(dk)
}

// kdfVerify comprueba pw contra un hash de formato nuevo en tiempo constante.
func kdfVerify(pw, stored string) bool {
	p := strings.Split(stored, "$")
	if len(p) != 4 || p[0] != pwScheme {
		return false
	}
	iter, err := strconv.Atoi(p[1])
	if err != nil || iter < 1 || iter > pwIterMax {
		return false
	}
	salt, err := hex.DecodeString(p[2])
	if err != nil || len(salt) == 0 {
		return false
	}
	want, err := hex.DecodeString(p[3])
	if err != nil || len(want) != pwKeyLen {
		return false
	}
	got, err := pbkdf2.Key(sha256.New, pw, salt, iter, pwKeyLen)
	return err == nil && subtle.ConstantTimeCompare(got, want) == 1
}

func isKDF(stored string) bool { return strings.HasPrefix(stored, pwScheme+"$") }
