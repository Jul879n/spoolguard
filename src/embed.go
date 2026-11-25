// Package src incrusta la interfaz web (la misma de escritorio) en el binario del servidor.
package src

import "embed"

//go:embed *.html *.js *.css
var FS embed.FS
