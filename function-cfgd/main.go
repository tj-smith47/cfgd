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

	// The DeploymentRuntimeConfig may pass --insecure to skip mTLS.
	insecure := false
	for _, arg := range os.Args[1:] {
		if arg == "--insecure" {
			insecure = true
		}
	}

	opts := []fn.ServeOption{
		fn.Listen(fn.DefaultNetwork, fn.DefaultAddress),
	}
	if insecure {
		opts = append(opts, fn.Insecure(true))
	} else {
		certDir := os.Getenv("TLS_SERVER_CERTS_DIR")
		if certDir == "" {
			certDir = "/tls/server" // Crossplane's standard mount path
		}
		opts = append(opts, fn.MTLSCertificates(certDir))
	}

	if err := fn.Serve(&Function{log: log}, opts...); err != nil {
		log.Info("Error serving function", "error", err)
		os.Exit(1)
	}
}
