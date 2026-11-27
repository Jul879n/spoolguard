package app

import (
	"fmt"
	"os"
	"strconv"
	"strings"
)

// applyVerdictEnv aplica SPOOLGUARD_VERDICT_TIMEOUT (segundos, 1-600) y SPOOLGUARD_STRICT (0/1/true/false) sobre los
// ajustes al arrancar, y normaliza lo guardado (config.json antiguo sin el campo = 10 s). Un valor inválido se ignora y
// se devuelve un aviso: nunca se aplica algo a medias.
func applyVerdictEnv(st *Settings) string {
	var warn []string
	if st.VerdictTimeoutSecs < 1 || st.VerdictTimeoutSecs > 600 {
		st.VerdictTimeoutSecs = 10
	}
	if v := strings.TrimSpace(os.Getenv("SPOOLGUARD_VERDICT_TIMEOUT")); v != "" {
		if n, err := strconv.ParseUint(v, 10, 64); err == nil && n >= 1 && n <= 600 {
			st.VerdictTimeoutSecs = n
		} else {
			warn = append(warn, fmt.Sprintf("SPOOLGUARD_VERDICT_TIMEOUT=%q ignorada: debe ser un entero entre 1 y 600", v))
		}
	}
	if v := strings.ToLower(strings.TrimSpace(os.Getenv("SPOOLGUARD_STRICT"))); v != "" {
		switch v {
		case "1", "true":
			st.Strict = true
		case "0", "false":
			st.Strict = false
		default:
			warn = append(warn, fmt.Sprintf("SPOOLGUARD_STRICT=%q ignorada: debe ser 0, 1, true o false", v))
		}
	}
	return strings.Join(warn, "; ")
}
