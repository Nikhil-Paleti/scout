package server

// Handle serves one request.
func Handle(path string) error {
	if path == "" {
		return ErrEmpty
	}
	return nil
}
