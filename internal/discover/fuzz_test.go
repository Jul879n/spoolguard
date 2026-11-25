package discover

import "testing"

// Una impresora hostil controla toda la respuesta IPP: el analizador no debe entrar en pánico ni crecer sin límite.
func FuzzParseIPPResponse(f *testing.F) {
	ok := encodeRequest(0x000B, "ipp://h/ipp/print", []ippAttr{{0x44, "requested-attributes", "printer-name"}})
	f.Add(ok)
	f.Add([]byte{1, 1, 0, 0, 0, 0, 0, 1, 4, 0x34, 0, 0, 0, 0, 0x37, 3})
	f.Add([]byte{1, 1, 0, 0, 0, 0, 0, 1, 4, 0x41, 0xff, 0xff, 0, 0})
	f.Fuzz(func(t *testing.T, b []byte) { _, _ = ParseResponse(b) })
}

func FuzzParseCIDRs(f *testing.F) {
	f.Add("192.168.1.0/24")
	f.Add("10.0.0.1")
	f.Add("0.0.0.0/0")
	f.Fuzz(func(t *testing.T, s string) {
		hosts, err := ParseCIDRs([]string{s}, false)
		if err == nil && len(hosts) > MaxHosts {
			t.Fatalf("más de MaxHosts: %d", len(hosts))
		}
		for _, h := range hosts {
			if !(h.IsPrivate() || h.IsLoopback() || h.IsLinkLocalUnicast()) {
				t.Fatalf("host público sin confirmación: %s", h)
			}
		}
	})
}
