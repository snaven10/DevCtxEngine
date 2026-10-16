package fx

import "example.com/fx/store"

func Leaf() int {
	return 1
}

func WithLiteral() int {
	f := func() int {
		return Leaf()
	}
	return f()
}

func RunLiteral() int {
	return WithLiteral()
}

type Server struct{}

func (s *Server) Handle() int {
	return Leaf()
}

func RunHandle(s *Server) int {
	return s.Handle()
}

func RunStore() int {
	return store.Open()
}

type Val struct{}

func (v Val) Get() int {
	return Leaf()
}

func ViaExpr() int {
	return Val.Get(Val{})
}

func RunViaExpr() int {
	return ViaExpr()
}

func ViaValue(s Server) int {
	return s.Handle()
}

func RunViaValue() int {
	return ViaValue(Server{})
}
