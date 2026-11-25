// control-server: modo servidor de SpoolGuard (CUPS + panel web). Solo biblioteca estándar.
package main

import (
	"context"
	"errors"
	"flag"
	"log"
	"net/http"
	"os"
	"os/signal"
	"syscall"
	"time"

	"controlimpresion/internal/app"
	"controlimpresion/src"
)

func env(k, def string) string {
	if v := os.Getenv(k); v != "" {
		return v
	}
	return def
}

func main() {
	listen := flag.String("listen", env("CONTROL_LISTEN", "0.0.0.0:8080"), "dirección HTTP (CONTROL_LISTEN)")
	dir := flag.String("data", env("CONTROL_IMPRESION_DIR", "data"), "directorio de datos (CONTROL_IMPRESION_DIR)")
	flag.Parse()

	s, err := app.New(*dir)
	if err != nil {
		log.Fatal(err)
	}
	s.Reconcile()

	ctx, stop := signal.NotifyContext(context.Background(), os.Interrupt, syscall.SIGTERM)
	defer stop()
	done := make(chan struct{})
	go func() { s.Worker(ctx); close(done) }()
	go s.DirectWatch(ctx) // apagada por defecto: solo actúa si el administrador la activa

	srv := &http.Server{
		Addr:              *listen,
		Handler:           s.Handler(src.FS),
		ReadHeaderTimeout: 10 * time.Second,
		ReadTimeout:       30 * time.Second,
		WriteTimeout:      60 * time.Second,
		IdleTimeout:       2 * time.Minute,
		MaxHeaderBytes:    32 << 10,
	}
	go func() {
		<-ctx.Done()
		sc, c := context.WithTimeout(context.Background(), 5*time.Second)
		defer c()
		_ = srv.Shutdown(sc)
	}()
	log.Printf("control-server escuchando en %s (datos: %s)", *listen, *dir)
	if err := srv.ListenAndServe(); err != nil && !errors.Is(err, http.ErrServerClosed) {
		log.Fatal(err)
	}
	<-done
	// Por defecto las colas siguen retenidas (los trabajos esperan al servidor, no se imprimen sin control).
	// CONTROL_RELEASE_ON_EXIT=1 las libera como la app de escritorio.
	if os.Getenv("CONTROL_RELEASE_ON_EXIT") == "1" {
		s.ReleaseAll()
	}
}
