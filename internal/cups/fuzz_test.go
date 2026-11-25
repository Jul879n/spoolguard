package cups

import "testing"

// Fuzz de los analizadores de la salida de ipptool/lpstat y de URIs de dispositivo.
func FuzzIPPToolOutput(f *testing.F) {
	f.Add("    job-uri (uri) = ipp://localhost/jobs/12\n    job-state (enum) = pending-held\n    job-name (nameWithoutLanguage) = a.docx\n    job-k-octets (integer) = 12\n")
	f.Add("device-uri (uri) = socket://192.168.1.5:9100\nprinter-state (enum) = idle")
	f.Add(" = \n = = =\n(\n job-uri\n")
	f.Fuzz(func(t *testing.T, out string) {
		_ = parseAttrs(out)
		_ = ippAttr(out, "device-uri")
		_ = classify(out)
		_ = ConnType(out)
		_ = NetHost(out)
		_, _, _ = hostPort(out)
		_ = ValidQueue(out)
	})
}
