package store

type Store struct {
	items map[string]string
}

func New() *Store {
	return &Store{}
}

func (s *Store) Get(key string) string {
	return s.items[key]
}

type Getter interface {
	Get(key string) string
}
