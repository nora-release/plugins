//go:build linux

package init

import (
	"errors"
	"reflect"
	"strings"
	"testing"
)

func TestNormalizeDNSResolvers(t *testing.T) {
	got := normalizeDNSResolvers([]string{" 1.1.1.1 ", "", "9.9.9.9", "1.1.1.1", "8.8.8.8"})
	want := []string{"1.1.1.1", "9.9.9.9", "8.8.8.8"}
	if !reflect.DeepEqual(got, want) {
		t.Fatalf("normalizeDNSResolvers() = %#v, want %#v", got, want)
	}
}

func TestRenderResolvConf(t *testing.T) {
	got, err := renderResolvConf([]string{"1.1.1.1", "", "1.1.1.1", "9.9.9.9"})
	if err != nil {
		t.Fatalf("renderResolvConf() error = %v", err)
	}
	want := "nameserver 1.1.1.1\nnameserver 9.9.9.9\n"
	if got != want {
		t.Fatalf("renderResolvConf() = %q, want %q", got, want)
	}
	if strings.Contains(got, "options ") {
		t.Fatalf("renderResolvConf() should keep resolver retry defaults, got %q", got)
	}
}

func TestRenderResolvConfRejectsEmptyResolvers(t *testing.T) {
	_, err := renderResolvConf([]string{"", "  "})
	if err == nil {
		t.Fatal("renderResolvConf() error = nil, want error")
	}
}

func TestPreflightDNSResolversWithLookup(t *testing.T) {
	calls := make([]string, 0)
	lookup := func(resolver string, domains []string) error {
		calls = append(calls, resolver+":"+strings.Join(domains, ","))
		if resolver == "8.8.8.8" {
			return errors.New("query timed out")
		}
		return nil
	}

	healthy, failures := preflightDNSResolversWithLookup(
		[]string{"1.1.1.1", "", "1.1.1.1", "9.9.9.9", "8.8.8.8"},
		[]string{"github.com"},
		lookup,
	)

	wantHealthy := []string{"1.1.1.1", "9.9.9.9"}
	if !reflect.DeepEqual(healthy, wantHealthy) {
		t.Fatalf("healthy = %#v, want %#v", healthy, wantHealthy)
	}
	if len(failures) != 1 || !strings.Contains(failures[0], "8.8.8.8: query timed out") {
		t.Fatalf("failures = %#v, want 8.8.8.8 timeout", failures)
	}
	wantCalls := []string{
		"1.1.1.1:github.com",
		"9.9.9.9:github.com",
		"8.8.8.8:github.com",
	}
	if !reflect.DeepEqual(calls, wantCalls) {
		t.Fatalf("lookup calls = %#v, want %#v", calls, wantCalls)
	}
}

func TestSameStringSlice(t *testing.T) {
	if !sameStringSlice([]string{"1.1.1.1", "9.9.9.9"}, []string{"1.1.1.1", "9.9.9.9"}) {
		t.Fatal("sameStringSlice() = false, want true")
	}
	if sameStringSlice([]string{"1.1.1.1", "9.9.9.9"}, []string{"9.9.9.9", "1.1.1.1"}) {
		t.Fatal("sameStringSlice() = true for reordered slices, want false")
	}
	if sameStringSlice([]string{"1.1.1.1"}, []string{"1.1.1.1", "9.9.9.9"}) {
		t.Fatal("sameStringSlice() = true for different lengths, want false")
	}
}
