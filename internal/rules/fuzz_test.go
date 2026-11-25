package rules

import (
	"encoding/json"
	"testing"
)

func FuzzRulesJSON(f *testing.F) {
	f.Add([]byte(`{"enabled":true,"mode":"block","apps":["*.docx"],"sites":["a.cl"],"ips":["10.0.0.0/8","fe80::/10"]}`), "doc.docx", "a.cl", "10.1.2.3")
	f.Fuzz(func(t *testing.T, raw []byte, doc, site, ip string) {
		var r Rules
		if json.Unmarshal(raw, &r) != nil {
			return
		}
		r.Normalize()
		app := doc
		j := &Job{Document: doc, App: &app, User: &app, OriginIP: &ip, Site: &site}
		_, _ = r.Explain(j)
		_ = r.Decide(j)
		_, _ = ExplainBoth(&r, &r, j)
		_, _ = ParseIP(ip)
		_ = HostOf(site)
	})
}
