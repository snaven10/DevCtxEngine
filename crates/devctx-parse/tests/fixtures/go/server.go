package api

import (
	"fmt"
	h "net/http"

	"github.com/acme/demo/store"
)

const Version = "1"

type Handler interface {
	Serve()
}

type Server struct {
	Base
	store *store.Store
	Name  string
}

func NewServer() *Server {
	return &Server{Name: "x"}
}

func (s *Server) Handle(w h.ResponseWriter) {
	fmt.Println(s.Name)
	s.helper()
}

func (s *Server) helper() {}
