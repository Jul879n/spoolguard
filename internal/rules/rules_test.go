package rules

import (
	"encoding/json"
	"os"
	"testing"
)

type vcase struct {
	Name     string `json:"name"`
	Global   *Rules `json:"global"`
	Specific *Rules `json:"specific"`
	Job      Job    `json:"job"`
	Decision string `json:"decision"`
	Reason   string `json:"reason"`
}

func TestVectores(t *testing.T) {
	b, err := os.ReadFile("../../core/tests/vectors/rules.json")
	if err != nil {
		t.Fatal(err)
	}
	var f struct {
		Cases []vcase `json:"cases"`
	}
	if err := json.Unmarshal(b, &f); err != nil {
		t.Fatal(err)
	}
	if len(f.Cases) < 40 {
		t.Fatalf("pocos casos: %d", len(f.Cases))
	}
	for _, c := range f.Cases {
		g, s := c.Global, c.Specific
		if g == nil {
			g = &Rules{}
		}
		if s == nil {
			s = &Rules{}
		}
		g.Normalize()
		s.Normalize()
		ok, why := ExplainBoth(g, s, &c.Job)
		d := "Deny"
		if ok {
			d = "Allow"
		}
		if d != c.Decision {
			t.Errorf("%s: decisión %s, esperada %s", c.Name, d, c.Decision)
		}
		if why != c.Reason {
			t.Errorf("%s: motivo %q, esperado %q", c.Name, why, c.Reason)
		}
	}
}
