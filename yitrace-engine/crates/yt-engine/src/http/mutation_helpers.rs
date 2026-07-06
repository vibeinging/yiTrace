fn read_model_mutating_route(method: &str, base: &str) -> bool {
    match (method, base) {
        ("POST", "/v1/ingest")
        | ("POST", "/v1/traces")
        | ("POST", "/v1/annotations")
        | ("POST", "/v1/dataset-associations")
        | ("POST", "/v1/dataset-links")
        | ("POST", "/v1/golden-paths")
        | ("POST", "/v1/retention/apply")
        | ("POST", "/v1/retention-policies")
        | ("POST", "/v1/retention/policies")
        | ("POST", "/v1/retention-policies/run-due")
        | ("POST", "/v1/retention/policies/run-due")
        | ("POST", "/v1/retention/run-due")
        | ("POST", "/v1/replication/wal")
        | ("POST", "/v1/replication/apply")
        | ("POST", "/v1/replication/apply-wal") => true,
        ("PATCH", path) | ("DELETE", path) => {
            path.starts_with("/v1/annotations") || path.starts_with("/v1/golden-paths")
        }
        ("POST", path) => {
            path.starts_with("/v1/annotations/") || path.starts_with("/v1/golden-paths/")
        }
        _ => false,
    }
}
