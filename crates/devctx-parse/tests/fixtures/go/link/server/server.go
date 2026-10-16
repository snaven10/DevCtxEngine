package server

import (
	"fmt"
	h "net/http"

	"github.com/acme/demo/store"
	"github.com/pkg/errors"
	"golang.org/x/sync/errgroup"
)

type Server struct {
	store *store.Store
	Name  string
}

func NewServer() *Server {
	return &Server{Name: "x"}
}

func (s *Server) Handle(w h.ResponseWriter, g store.Getter) {
	fmt.Println(s.Name)
	s.helper()
	s.store.Get("k")
	x := NewServer()
	x.helper()
	y := store.New()
	y.Get("a")
	g.Get("b")
	_ = errors.New("e")
	_, _ = errgroup.WithContext(nil)
	f := func(s *store.Store) {
		s.Get("c")
	}
	f(nil)
	other()
	len(s.Name)
}
