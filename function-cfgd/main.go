package main

import (
	"os"

	fn "github.com/crossplane/function-sdk-go"
	"github.com/crossplane/function-sdk-go/logging"
)

func main() {
	log, err := logging.NewLogger(false)
	if err != nil {
		os.Exit(1)
	}
	if err := fn.Serve(&Function{log: log},
		fn.Listen(fn.DefaultNetwork, fn.DefaultAddress),
		fn.MTLSCertificates(os.Getenv("TLS_SERVER_CERTS_DIR")),
	); err != nil {
		log.Info("Error serving function", "error", err)
		os.Exit(1)
	}
}
