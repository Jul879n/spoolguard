package app

import (
	"archive/zip"
	"bytes"
	"crypto/sha256"
	"encoding/hex"
	"encoding/json"
	"fmt"
	"io/fs"
	"net/http"
	"os"
	"path/filepath"
	"sort"
	"strings"
	"sync"
	"time"
)

// Actualización de la extensión en modo servidor: /ext/version y /ext/package.zip entregan la carpeta
// `extension` que trae la imagen (/app/extension) como zip determinista. El servidor nunca ejecuta nada de ella.

const (
	extVersionPerMin = 30
	extPackagePerMin = 10
)

type extPackage struct {
	Version string
	SHA256  string
	Size    int
	Data    []byte
}

type extPkgCache struct {
	mu  sync.Mutex
	sig string
	pkg *extPackage
}

// extDirs: carpeta de la extensión, por orden: CONTROL_EXT_DIR, /app/extension, junto al ejecutable, ./extension.
func extDirs() []string {
	var c []string
	if d := os.Getenv("CONTROL_EXT_DIR"); d != "" {
		c = append(c, d)
	}
	c = append(c, "/app/extension")
	if exe, err := os.Executable(); err == nil {
		c = append(c, filepath.Join(filepath.Dir(exe), "extension"))
	}
	return append(c, "extension")
}

func findExtDir(override string) string {
	c := extDirs()
	if override != "" {
		c = []string{override}
	}
	for _, d := range c {
		if st, err := os.Stat(filepath.Join(d, "manifest.json")); err == nil && !st.IsDir() {
			return d
		}
	}
	return ""
}

// listExtFiles: archivos regulares (sin enlaces ni ocultos), ordenados.
func listExtFiles(dir string) ([]string, error) {
	var files []string
	err := filepath.WalkDir(dir, func(p string, d fs.DirEntry, err error) error {
		if err != nil {
			return err
		}
		if strings.HasPrefix(d.Name(), ".") && p != dir {
			if d.IsDir() {
				return filepath.SkipDir
			}
			return nil
		}
		if d.Type().IsRegular() {
			rel, _ := filepath.Rel(dir, p)
			files = append(files, filepath.ToSlash(rel))
		}
		return nil
	})
	sort.Strings(files)
	return files, err
}

func signature(dir string, files []string) string {
	var b strings.Builder
	for _, f := range files {
		if st, err := os.Stat(filepath.Join(dir, f)); err == nil {
			fmt.Fprintf(&b, "%s|%d|%d;", f, st.Size(), st.ModTime().UnixNano())
		}
	}
	return b.String()
}

// buildExtPackage arma el zip (orden y fecha fijos: mismo contenido, mismo SHA-256).
func buildExtPackage(dir string, files []string) (*extPackage, error) {
	var buf bytes.Buffer
	zw := zip.NewWriter(&buf)
	for _, f := range files {
		b, err := os.ReadFile(filepath.Join(dir, filepath.FromSlash(f)))
		if err != nil {
			return nil, err
		}
		w, err := zw.CreateHeader(&zip.FileHeader{Name: f, Method: zip.Deflate, Modified: time.Date(2020, 1, 1, 0, 0, 0, 0, time.UTC)})
		if err != nil {
			return nil, err
		}
		if _, err := w.Write(b); err != nil {
			return nil, err
		}
	}
	if err := zw.Close(); err != nil {
		return nil, err
	}
	mb, err := os.ReadFile(filepath.Join(dir, "manifest.json"))
	if err != nil {
		return nil, err
	}
	var m struct {
		Version string `json:"version"`
	}
	if json.Unmarshal(mb, &m) != nil || m.Version == "" {
		return nil, fmt.Errorf("manifest.json sin version")
	}
	sum := sha256.Sum256(buf.Bytes())
	return &extPackage{Version: m.Version, SHA256: hex.EncodeToString(sum[:]), Size: buf.Len(), Data: buf.Bytes()}, nil
}

func (s *Server) extPackage() (*extPackage, error) {
	dir := findExtDir(s.extDir)
	if dir == "" {
		return nil, fmt.Errorf("el servidor no incluye la extensión")
	}
	files, err := listExtFiles(dir)
	if err != nil {
		return nil, err
	}
	c := &s.extCache
	c.mu.Lock()
	defer c.mu.Unlock()
	if sig := dir + "#" + signature(dir, files); c.pkg == nil || c.sig != sig {
		p, err := buildExtPackage(dir, files)
		if err != nil {
			return nil, err
		}
		c.sig, c.pkg = sig, p
	}
	return c.pkg, nil
}

func (s *Server) extVersion(w http.ResponseWriter, r *http.Request) {
	if !s.extAuth(w, r, http.MethodGet, "version", extVersionPerMin) {
		return
	}
	p, err := s.extPackage()
	if err != nil {
		writeJSON(w, http.StatusNotFound, map[string]string{"error": err.Error()})
		return
	}
	writeJSON(w, http.StatusOK, map[string]any{"version": p.Version, "sha256": p.SHA256, "size": p.Size, "url": "/ext/package.zip"})
}

func (s *Server) extPackageZip(w http.ResponseWriter, r *http.Request) {
	if !s.extAuth(w, r, http.MethodGet, "package", extPackagePerMin) {
		return
	}
	p, err := s.extPackage()
	if err != nil {
		writeJSON(w, http.StatusNotFound, map[string]string{"error": err.Error()})
		return
	}
	h := w.Header()
	h.Set("Content-Type", "application/zip")
	h.Set("Content-Disposition", `attachment; filename="spoolguard-extension-`+p.Version+`.zip"`)
	h.Set("X-Content-SHA256", p.SHA256)
	h.Set("Cache-Control", "no-store")
	h.Set("X-Content-Type-Options", "nosniff")
	_, _ = w.Write(p.Data)
}
