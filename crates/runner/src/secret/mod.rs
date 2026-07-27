// Secret management module
// Handles:
// - Vault secret retrieval for pipeline variables
// - In-memory secret storage (never written to disk)
// - Secret cleanup after job completion
// - SSH credential retrieval for remote execution
