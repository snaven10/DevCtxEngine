package server

import (
	"github.com/acme/demo/lib/util2"
	"github.com/acme/demo/store"
)

func Review() {
	store.Platform()
	helpers.Do()
	var a, err = NewServer()
	_ = a
	err.Error()
	NewServer := func() *store.Store { return nil }
	z := NewServer()
	z.helper()
}
