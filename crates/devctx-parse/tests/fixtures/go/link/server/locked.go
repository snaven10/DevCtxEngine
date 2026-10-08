package server

import "sync"

type Locked struct {
	sync.Mutex
	*Base
}

func (l *Locked) Go() {
	l.Close()
}

func Closer(x interface{ Shutdown() }) {
	x.Shutdown()
}
