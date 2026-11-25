package app

import (
	"fmt"
	"os"
	"runtime/debug"
)

// safely ejecuta f y convierte un pánico en una línea de registro: un trabajo o una respuesta de
// impresora malformados no deben tumbar el proceso (y con él el control de todas las colas).
func safely(where string, f func()) {
	defer func() {
		if r := recover(); r != nil {
			fmt.Fprintf(os.Stderr, "control-server: pánico recuperado en %s: %v\n%s\n", where, r, debug.Stack())
		}
	}()
	f()
}
